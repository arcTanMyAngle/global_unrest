//! Bluesky public post search, restricted to posts that actually carry video.
//!
//! Keyless: `app.bsky.feed.searchPosts` on the public AppView needs no session
//! and returns only public posts. This is the same network the ingest path
//! reads, asked a different question — the ingest path counts words as they
//! stream past and keeps nothing, while this returns a short, transient,
//! place-scoped list of links a person asked to see.
//!
//! **The hit URL is never an extracted stream.** A native Bluesky video lives
//! behind an HLS playlist that Chromium/WebView2 cannot decode anyway, so the
//! hit is the post's own page, and that page opens in the OS browser —
//! Bluesky publishes no embeddable *player*, only a post card whose play
//! button links back to bsky.app (see [`core_types::embed_for`]). A post that
//! is instead a *link card* to a video host already names that host's own
//! page, and that page is the hit — it plays inline, and sending someone to
//! the post widget would just show them the card they would have to click
//! again.
//!
//! Pure: [`request_url`] builds the call and [`hits`] parses a response. The
//! network round trip is in `crate::live`.

use chrono::{DateTime, Utc};
use core_types::SourceError;
use serde_json::Value;
use url::Url;

use crate::{MediaHit, Provider, search_terms, short_title};

/// Bluesky's public (unauthenticated) AppView.
///
/// **`api.bsky.app`, not `public.api.bsky.app`.** The documented public host
/// answers most keyless XRPC methods but returns a bot-block HTML `403` for
/// `searchPosts` in particular — live-verified 2026-08-13: `getProfile` on
/// `public.api.bsky.app` is `200` while `searchPosts` on the same host is
/// `403`, and `api.bsky.app` serves `searchPosts` unauthenticated. Neither
/// host needs a session, so this is a routing quirk, not an auth requirement;
/// `examples/media_live_probe.rs` is what re-checks it.
pub const SEARCH_ENDPOINT: &str = "https://api.bsky.app/xrpc/app.bsky.feed.searchPosts";

/// `searchPosts` caps `limit` at 100.
pub const MAX_LIMIT: usize = 100;

/// Moderation labels that disqualify a result.
///
/// Not our editorial judgement — these are the values Bluesky's own labelers
/// attach, returned on the post and on its author. Filtering on them is
/// necessary rather than tidy: adult-content accounts tag posts with long
/// hashtag lists including country names, so a live search for `colombia`
/// returned 21 labelled posts out of 50 (verified 2026-08-13) and they
/// crowded the genuine footage out of the panel entirely.
pub const BLOCKED_LABELS: &[&str] = &[
    "porn",
    "sexual",
    "nudity",
    "graphic-media",
    "sexual-figurative",
];

/// Build the search query string for a place (+ optional topic).
///
/// Bluesky's search has its own operators (`from:`, `domain:`, quoted
/// phrases). [`crate::search_terms`] has already stripped user text to bare
/// words, so nothing here can carry one in; terms are simply ANDed by the
/// service.
pub fn query_expression(place: &str, topic: &str) -> Option<String> {
    let place = search_terms(place);
    if place.is_empty() {
        return None;
    }
    let topic = search_terms(topic);
    if topic.is_empty() {
        Some(place)
    } else {
        Some(format!("{place} {topic}"))
    }
}

/// Build the full `searchPosts` request URL for a window.
pub fn request_url(
    endpoint: &str,
    place: &str,
    topic: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    limit: usize,
) -> Result<Url, SourceError> {
    let query = query_expression(place, topic)
        .ok_or_else(|| SourceError::Other("media search needs a place".into()))?;
    let mut url = Url::parse(endpoint)
        .map_err(|e| SourceError::Other(format!("bad Bluesky endpoint `{endpoint}`: {e}")))?;
    url.query_pairs_mut()
        .append_pair("q", &query)
        .append_pair("sort", "latest")
        .append_pair("limit", &limit.clamp(1, MAX_LIMIT).to_string())
        .append_pair("since", &start.to_rfc3339())
        .append_pair("until", &end.to_rfc3339());
    Ok(url)
}

/// Parse a `searchPosts` body, keeping only posts that carry video.
///
/// Most results in any window are text; a post with no video is dropped here
/// rather than shown as an unplayable row. Three embed shapes qualify: a
/// native Bluesky video, an external link to a video host, and the media half
/// of a quote-post-with-media.
pub fn hits(body: &str) -> Result<Vec<MediaHit>, SourceError> {
    let doc: Value = serde_json::from_str(body)
        .map_err(|e| SourceError::Other(format!("Bluesky response was not JSON: {e}")))?;
    // The AppView reports failures as a JSON object, not a status code alone.
    if let Some(message) = doc.get("error").and_then(|v| v.as_str()) {
        let detail = doc
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(message);
        return Err(SourceError::Other(format!(
            "Bluesky rejected the media query: {detail}"
        )));
    }
    let posts = match doc.get("posts") {
        Some(Value::Array(items)) => items.as_slice(),
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(_) => {
            return Err(SourceError::Other(
                "Bluesky `posts` was not an array".to_string(),
            ));
        }
    };
    let mut out = Vec::new();
    for post in posts {
        let Some(playable) = playable(post.get("embed")) else {
            continue;
        };
        if is_labelled(post.get("labels"))
            || is_labelled(post.get("author").and_then(|a| a.get("labels")))
        {
            continue;
        }
        let Some(handle) = post
            .get("author")
            .and_then(|a| a.get("handle"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let Some((did, rkey)) = post.get("uri").and_then(|v| v.as_str()).and_then(post_ref) else {
            continue;
        };
        let Some(ts_utc) = post
            .get("indexedAt")
            .and_then(|v| v.as_str())
            .and_then(parse_ts)
        else {
            continue;
        };
        let text = post
            .get("record")
            .and_then(|r| r.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        out.push(MediaHit {
            url: match playable {
                // Keyed by DID, not by handle: `bsky.app` accepts either, but
                // a DID is the stable identifier and is what the AppView
                // returns, so a later handle change cannot break the link.
                Playable::Native => format!("https://bsky.app/profile/{did}/post/{rkey}"),
                Playable::External { uri, .. } => uri.to_string(),
            },
            title: hit_title(text, playable, handle),
            provider: Provider::Bluesky,
            ts_utc,
            origin: format!("@{handle}"),
        });
    }
    Ok(out)
}

/// Label for one hit: the post's own words when it has any, otherwise the link
/// card's headline, otherwise who posted it.
///
/// A post whose entire text is the link it shares is treated as having no
/// words of its own — "youtube.com/watch?v=OjHO…" tells a reader nothing that
/// the card's title would not tell them better.
fn hit_title(text: &str, playable: Playable<'_>, handle: &str) -> String {
    let text = text.trim();
    if !text.is_empty() && !is_bare_link(text) {
        return short_title(text);
    }
    if let Playable::External {
        title: Some(card), ..
    } = playable
        && !card.trim().is_empty()
    {
        return short_title(card);
    }
    format!("video posted by @{handle}")
}

fn is_bare_link(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    words.next().is_none() && (first.starts_with("http://") || first.starts_with("https://"))
}

/// Does this label list carry one of [`BLOCKED_LABELS`]?
fn is_labelled(labels: Option<&Value>) -> bool {
    let Some(Value::Array(labels)) = labels else {
        return false;
    };
    labels.iter().any(|label| {
        label
            .get("val")
            .and_then(|v| v.as_str())
            .is_some_and(|val| BLOCKED_LABELS.contains(&val))
    })
}

/// What a post's embed gives the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Playable<'a> {
    /// Bluesky's own video — play the post page through its widget.
    Native,
    /// A link card pointing at a video host: `uri` is that host's own page,
    /// and `title` is the card's headline if it published one.
    External {
        uri: &'a str,
        title: Option<&'a str>,
    },
}

/// Does this embed view contain something playable, and if so, what?
fn playable(embed: Option<&Value>) -> Option<Playable<'_>> {
    let embed = embed?;
    match embed.get("$type").and_then(|v| v.as_str()) {
        // A native Bluesky video: `playlist` is the HLS manifest, which we
        // never open ourselves — its presence is only the "yes, video" signal.
        Some("app.bsky.embed.video#view") => embed.get("playlist").map(|_| Playable::Native),
        // A link card. Only counts if the link is one we'd classify as video
        // anyway, so the result list cannot fill with ordinary article cards.
        Some("app.bsky.embed.external#view") => {
            let external = embed.get("external")?;
            let uri = external.get("uri").and_then(|v| v.as_str())?;
            if !core_types::is_video_url(uri) {
                return None;
            }
            Some(Playable::External {
                uri,
                title: external.get("title").and_then(|v| v.as_str()),
            })
        }
        // A quote post with media attached: the media half is a nested view of
        // one of the shapes above.
        Some("app.bsky.embed.recordWithMedia#view") => playable(embed.get("media")),
        _ => None,
    }
}

/// `at://<did>/app.bsky.feed.post/<rkey>` -> `(<did>, <rkey>)`.
///
/// Rejects anything that is not a post record, so a like or repost URI cannot
/// become a broken `bsky.app/profile/.../post/...` link, and requires the
/// authority to be a real DID — the AppView always returns one there, and a
/// handle in that position would build an embed URL Bluesky's player refuses.
fn post_ref(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("at://")?;
    let (did, tail) = rest.split_once('/')?;
    if !did.starts_with("did:") {
        return None;
    }
    let rkey = tail.strip_prefix("app.bsky.feed.post/")?;
    if rkey.is_empty() || rkey.contains('/') {
        None
    } else {
        Some((did, rkey))
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn request_url_carries_the_window_and_a_clamped_limit() {
        let url = request_url(
            "https://example.test/xrpc/app.bsky.feed.searchPosts",
            "Colombia",
            "earthquake",
            Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 13, 6, 30, 0).unwrap(),
            5_000,
        )
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["q"], "colombia earthquake");
        assert_eq!(pairs["sort"], "latest");
        assert_eq!(pairs["limit"], MAX_LIMIT.to_string());
        assert_eq!(pairs["since"], "2026-08-10T00:00:00+00:00");
        assert_eq!(pairs["until"], "2026-08-13T06:30:00+00:00");
        assert_eq!(query_expression("  ", "earthquake"), None);
    }

    #[test]
    fn only_posts_that_carry_video_become_hits() {
        let body = r#"{"posts":[
            {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
             "author":{"handle":"reporter.example"},
             "record":{"text":"Quake damage in Bogota"},
             "indexedAt":"2026-08-10T12:00:00Z",
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/watch/did/cid/playlist.m3u8"}},
            {"uri":"at://did:plc:bbb/app.bsky.feed.post/rk2",
             "author":{"handle":"chatter.example"},
             "record":{"text":"just text"},
             "indexedAt":"2026-08-10T12:00:00Z"},
            {"uri":"at://did:plc:ccc/app.bsky.feed.post/rk3",
             "author":{"handle":"linker.example"},
             "record":{"text":"footage"},
             "indexedAt":"2026-08-10T11:00:00Z",
             "embed":{"$type":"app.bsky.embed.external#view","external":{"uri":"https://news.example.org/story"}}},
            {"uri":"at://did:plc:ddd/app.bsky.feed.post/rk4",
             "author":{"handle":"quoter.example"},
             "record":{"text":""},
             "indexedAt":"2026-08-10T10:00:00Z",
             "embed":{"$type":"app.bsky.embed.recordWithMedia#view",
                      "media":{"$type":"app.bsky.embed.external#view",
                               "external":{"uri":"https://youtu.be/abc"}}}}
        ]}"#;
        let hits = hits(body).unwrap();
        let urls: Vec<&str> = hits.iter().map(|h| h.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                // Native video: the post page, keyed by DID so Bluesky's own
                // player will accept it.
                "https://bsky.app/profile/did:plc:aaa/post/rk1",
                // Link card: the video host's page the post itself named.
                // Sending someone to the post widget would only show them the
                // card again.
                "https://youtu.be/abc",
            ]
        );
        assert_eq!(hits[0].title, "Quake damage in Bogota");
        assert_eq!(hits[0].origin, "@reporter.example");
        // A caption-less post still gets a readable label.
        assert_eq!(hits[1].title, "video posted by @quoter.example");
    }

    #[test]
    fn a_link_card_hit_is_labelled_by_the_cards_own_headline() {
        let body = r#"{"posts":[
            {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
             "author":{"handle":"linker.example"},
             "record":{"text":"https://youtu.be/abc"},
             "indexedAt":"2026-08-10T12:00:00Z",
             "embed":{"$type":"app.bsky.embed.external#view",
                      "external":{"uri":"https://youtu.be/abc","title":"Flooding in Cali"}}}
        ]}"#;
        let hits = hits(body).unwrap();
        // The post's whole text is the bare link, which tells a reader nothing
        // the card's headline does not tell them better.
        assert_eq!(hits[0].title, "Flooding in Cali");
        assert_eq!(hits[0].url, "https://youtu.be/abc");
    }

    #[test]
    fn a_native_video_hit_is_the_post_page_the_browser_can_play() {
        let body = r#"{"posts":[
            {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
             "author":{"handle":"reporter.example"},
             "record":{"text":"clip"},
             "indexedAt":"2026-08-10T12:00:00Z",
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/watch/d/c/playlist.m3u8"}}
        ]}"#;
        let hits = hits(body).unwrap();
        // The post page, never the HLS playlist: WebView2 cannot decode HLS,
        // and resolving a post to its stream would be scraping. Bluesky has no
        // embeddable player, so this URL is expected to have no in-app embed —
        // the desktop's "open in browser" path is the one that plays it, and
        // bsky.app does play it there.
        assert_eq!(hits[0].url, "https://bsky.app/profile/did:plc:aaa/post/rk1");
        assert_eq!(core_types::embed_for(&hits[0].url), None);
    }

    #[test]
    fn moderation_labelled_posts_are_dropped_however_well_they_match() {
        // Adult-content accounts hashtag country names, so these outrank real
        // footage on a `sort=latest` search unless they are filtered out.
        // `r##` because the hashtags in the fixture would close an `r#` literal.
        let body = r##"{"posts":[
            {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
             "author":{"handle":"spam.example"},
             "record":{"text":"#colombia #video"},
             "indexedAt":"2026-08-12T12:00:00Z",
             "labels":[{"val":"porn"}],
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/w/d/c/p.m3u8"}},
            {"uri":"at://did:plc:bbb/app.bsky.feed.post/rk2",
             "author":{"handle":"labelled-author.example","labels":[{"val":"sexual"}]},
             "record":{"text":"colombia"},
             "indexedAt":"2026-08-12T11:00:00Z",
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/w/d/c/p.m3u8"}},
            {"uri":"at://did:plc:ccc/app.bsky.feed.post/rk3",
             "author":{"handle":"reporter.example","labels":[]},
             "record":{"text":"Quake damage"},
             "indexedAt":"2026-08-12T10:00:00Z",
             "labels":[{"val":"!no-unauthenticated"}],
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/w/d/c/p.m3u8"}}
        ]}"##;
        let hits = hits(body).unwrap();
        // The third survives: `!no-unauthenticated` is a visibility label, not
        // a content one, and blocking every label would empty the panel.
        assert_eq!(hits.len(), 1, "{hits:#?}");
        assert_eq!(hits[0].origin, "@reporter.example");
    }

    #[test]
    fn a_non_post_uri_is_not_turned_into_a_broken_link() {
        assert_eq!(
            post_ref("at://did:plc:aaa/app.bsky.feed.post/rk1"),
            Some(("did:plc:aaa", "rk1"))
        );
        assert_eq!(post_ref("at://did:plc:aaa/app.bsky.feed.like/rk1"), None);
        assert_eq!(post_ref("https://bsky.app/profile/x/post/rk1"), None);
        // A handle where the DID belongs would build an embed URL Bluesky's
        // player rejects outright, so it is not a post reference at all.
        assert_eq!(
            post_ref("at://reporter.example/app.bsky.feed.post/rk1"),
            None
        );
    }

    #[test]
    fn an_error_payload_is_named_rather_than_read_as_no_results() {
        let err = hits(r#"{"error":"InvalidRequest","message":"Error: q is required"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Bluesky rejected the media query"), "{err}");
        assert!(hits("{}").unwrap().is_empty());
    }
}
