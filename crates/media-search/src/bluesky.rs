//! Bluesky public post search, restricted to posts that actually carry video.
//!
//! Keyless: `app.bsky.feed.searchPosts` on the public AppView needs no session
//! and returns only public posts. This is the same network the ingest path
//! reads, asked a different question — the ingest path counts words as they
//! stream past and keeps nothing, while this returns a short, transient,
//! place-scoped list of links a person asked to see.
//!
//! **The hit URL is always the post's own page**, never an extracted stream.
//! A post's video lives behind an HLS playlist that Chromium/WebView2 cannot
//! decode natively anyway; `embed.bsky.app` is Bluesky's own published player
//! and is what [`core_types::embed_for`] maps a post URL onto.
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
/// `examples/live_probe.rs` is what re-checks it.
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
        if !carries_video(post.get("embed")) {
            continue;
        }
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
        let Some(rkey) = post.get("uri").and_then(|v| v.as_str()).and_then(post_rkey) else {
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
        let title = if text.trim().is_empty() {
            format!("video posted by @{handle}")
        } else {
            short_title(text)
        };
        out.push(MediaHit {
            url: format!("https://bsky.app/profile/{handle}/post/{rkey}"),
            title,
            provider: Provider::Bluesky,
            ts_utc,
            origin: format!("@{handle}"),
        });
    }
    Ok(out)
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

/// Does this embed view contain something playable?
fn carries_video(embed: Option<&Value>) -> bool {
    let Some(embed) = embed else { return false };
    match embed.get("$type").and_then(|v| v.as_str()) {
        // A native Bluesky video: `playlist` is the HLS manifest, which we
        // never open ourselves — its presence is only the "yes, video" signal.
        Some("app.bsky.embed.video#view") => embed.get("playlist").is_some(),
        // A link card. Only counts if the link is one we'd classify as video
        // anyway, so the result list cannot fill with ordinary article cards.
        Some("app.bsky.embed.external#view") => embed
            .get("external")
            .and_then(|e| e.get("uri"))
            .and_then(|v| v.as_str())
            .is_some_and(core_types::is_video_url),
        // A quote post with media attached: the media half is a nested view of
        // one of the shapes above.
        Some("app.bsky.embed.recordWithMedia#view") => carries_video(embed.get("media")),
        _ => false,
    }
}

/// `at://<did>/app.bsky.feed.post/<rkey>` -> `<rkey>`.
///
/// Rejects anything that is not a post record, so a like or repost URI cannot
/// become a broken `bsky.app/profile/.../post/...` link.
fn post_rkey(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("at://")?;
    let (_did, tail) = rest.split_once('/')?;
    let rkey = tail.strip_prefix("app.bsky.feed.post/")?;
    if rkey.is_empty() || rkey.contains('/') {
        None
    } else {
        Some(rkey)
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
                "https://bsky.app/profile/reporter.example/post/rk1",
                "https://bsky.app/profile/quoter.example/post/rk4",
            ]
        );
        assert_eq!(hits[0].title, "Quake damage in Bogota");
        assert_eq!(hits[0].origin, "@reporter.example");
        // A caption-less post still gets a readable label.
        assert_eq!(hits[1].title, "video posted by @quoter.example");
    }

    #[test]
    fn the_hit_url_is_the_post_page_that_embed_for_can_play() {
        let body = r#"{"posts":[
            {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
             "author":{"handle":"reporter.example"},
             "record":{"text":"clip"},
             "indexedAt":"2026-08-10T12:00:00Z",
             "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/watch/d/c/playlist.m3u8"}}
        ]}"#;
        let hits = hits(body).unwrap();
        // The whole point of returning the post URL: it maps to Bluesky's own
        // published player. The HLS playlist would not — WebView2 cannot
        // decode HLS natively.
        assert!(core_types::embed_for(&hits[0].url).is_some());
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
            post_rkey("at://did:plc:aaa/app.bsky.feed.post/rk1"),
            Some("rk1")
        );
        assert_eq!(post_rkey("at://did:plc:aaa/app.bsky.feed.like/rk1"), None);
        assert_eq!(post_rkey("https://bsky.app/profile/x/post/rk1"), None);
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
