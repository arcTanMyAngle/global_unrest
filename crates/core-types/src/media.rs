//! Video-URL classification, shared by the marker "has video" filter
//! ([`crate::EventKind`]-adjacent query filtering lives in `storage`) and the
//! desktop's region-inspector source-link list. Conservative: only direct
//! video/playlist file extensions and hosts whose primary product is video
//! count — an ordinary news-article URL is not proof of embedded footage.

/// Does `raw` point at a video (a direct video file, or a page on a known
/// video-hosting site)? Subdomain-aware (`clips.youtube.com` matches
/// `youtube.com`) but not fooled by a domain merely containing the name as a
/// substring (`youtube.com.attacker.example` does not match).
pub fn is_video_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let path = parsed.path().to_ascii_lowercase();
    const VIDEO_EXTENSIONS: &[&str] = &[".mp4", ".webm", ".mov", ".m4v", ".m3u8"];
    if VIDEO_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        return true;
    }
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    const VIDEO_HOSTS: &[&str] = &[
        "youtube.com",
        "youtu.be",
        "vimeo.com",
        "twitch.tv",
        "tiktok.com",
        "dailymotion.com",
        "streamable.com",
        "rumble.com",
    ];
    VIDEO_HOSTS.iter().any(|domain| host_matches(&host, domain))
}

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// How the desktop's embedded webview should play a given URL.
///
/// Pure data — this crate has no webview and no I/O. The desktop turns a
/// [`Embed::Page`] into `load_url` and a [`Embed::File`] into a one-element
/// HTML page; see `apps/global-signal-desktop/src/video.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Embed {
    /// A provider's own sanctioned player page (YouTube `/embed/`, Vimeo's
    /// `player.` host, Telegram's `?embed=1` widget, …). Loaded as-is.
    ///
    /// Using the provider's published embed endpoint is the whole reason this
    /// is legitimate: the alternative — resolving a watch page down to its
    /// underlying stream — is scraping, which this project does not do.
    Page(String),
    /// A direct media file the webview can decode itself (`.mp4`, `.webm`, …).
    File(String),
}

/// Map an arbitrary media URL onto something the embedded player can show.
///
/// `None` means "no sanctioned embed exists for this host" — the caller must
/// fall back to handing the original URL to the OS browser rather than
/// guessing at an embed URL. Twitch is the notable `None`: its embed requires
/// a `parent=` matching the hosting page's real domain, which an embedded
/// webview does not have.
pub fn embed_for(raw: &str) -> Option<Embed> {
    let parsed = url::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    // Leading empty segment is stripped by `path_segments`, so `/a/b` is
    // ["a", "b"]; a trailing slash yields a trailing "".
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|seg| !seg.is_empty()).collect())
        .unwrap_or_default();
    let query = |key: &str| {
        parsed
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };

    // Checked before the per-host arms: a URL ending in a media extension is
    // a direct file whatever its host, and some hosts serve both (Bluesky's
    // `video.bsky.app` HLS playlists sit under the same registrable domain as
    // its post pages, which have their own embed widget below).
    let path = parsed.path().to_ascii_lowercase();
    const PLAYABLE_EXTENSIONS: &[&str] = &[".mp4", ".webm", ".m4v", ".mov", ".m3u8"];
    if PLAYABLE_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        return Some(Embed::File(raw.to_string()));
    }

    if host_matches(&host, "youtube.com") {
        let id = match segments.as_slice() {
            ["watch"] => query("v"),
            ["embed", id, ..] | ["shorts", id, ..] | ["live", id, ..] | ["v", id, ..] => {
                Some((*id).to_string())
            }
            _ => None,
        }?;
        return Some(Embed::Page(youtube_embed(&id, query("t").as_deref())));
    }
    if host_matches(&host, "youtu.be") {
        let id = segments.first()?;
        return Some(Embed::Page(youtube_embed(id, query("t").as_deref())));
    }
    if host_matches(&host, "vimeo.com") {
        // player.vimeo.com/video/<id> is already the embed form.
        if let ["video", id, ..] = segments.as_slice() {
            return Some(Embed::Page(format!(
                "https://player.vimeo.com/video/{id}?autoplay=1"
            )));
        }
        // vimeo.com/<numeric id> — only numeric, so /channels, /ondemand and
        // other section pages fall through to the browser instead of
        // producing a player URL for something that is not a video.
        let id = segments
            .first()
            .filter(|s| s.chars().all(|c| c.is_ascii_digit()))?;
        return Some(Embed::Page(format!(
            "https://player.vimeo.com/video/{id}?autoplay=1"
        )));
    }
    if host_matches(&host, "dailymotion.com") {
        if let ["video", id, ..] | ["embed", "video", id, ..] = segments.as_slice() {
            return Some(Embed::Page(format!(
                "https://www.dailymotion.com/embed/video/{id}?autoplay=1"
            )));
        }
        return None;
    }
    if host_matches(&host, "dai.ly") {
        let id = segments.first()?;
        return Some(Embed::Page(format!(
            "https://www.dailymotion.com/embed/video/{id}?autoplay=1"
        )));
    }
    if host_matches(&host, "streamable.com") {
        let id = segments.first()?;
        // Already an embed path.
        if *id == "e" {
            return Some(Embed::Page(raw.to_string()));
        }
        return Some(Embed::Page(format!("https://streamable.com/e/{id}")));
    }
    if host_matches(&host, "tiktok.com") {
        // /@handle/video/<id> is the canonical post URL.
        let id = segments
            .iter()
            .position(|seg| *seg == "video")
            .and_then(|i| segments.get(i + 1))?;
        return Some(Embed::Page(format!("https://www.tiktok.com/embed/v2/{id}")));
    }
    if host_matches(&host, "rumble.com") {
        // Rumble watch-page slugs do not map to an embed id without an API
        // lookup, so only an already-embed URL is playable inline.
        if segments.first() == Some(&"embed") {
            return Some(Embed::Page(raw.to_string()));
        }
        return None;
    }
    if host_matches(&host, "t.me") {
        // Telegram's public single-post widget. `/s/<channel>` is the
        // channel *preview*, not a post, so it is skipped.
        if let [channel, post, ..] = segments.as_slice()
            && *channel != "s"
            && post.chars().all(|c| c.is_ascii_digit())
        {
            return Some(Embed::Page(format!(
                "https://t.me/{channel}/{post}?embed=1&mute=0"
            )));
        }
        return None;
    }
    if host_matches(&host, "bsky.app") {
        // https://bsky.app/profile/<actor>/post/<rkey>
        if let ["profile", actor, "post", rkey, ..] = segments.as_slice() {
            return Some(Embed::Page(format!(
                "https://embed.bsky.app/embed/{actor}/app.bsky.feed.post/{rkey}"
            )));
        }
        return None;
    }

    None
}

/// `t` may arrive as `90` or `90s`; YouTube's embed endpoint wants bare
/// seconds in `start`, so a trailing unit is dropped rather than passed on.
fn youtube_embed(id: &str, start: Option<&str>) -> String {
    let mut out = format!("https://www.youtube-nocookie.com/embed/{id}?autoplay=1&rel=0");
    if let Some(start) = start {
        let digits: String = start.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            out.push_str(&format!("&start={digits}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_urls_are_classified_conservatively() {
        assert!(is_video_url("https://www.youtube.com/watch?v=report"));
        assert!(is_video_url(
            "https://media.example.org/capture.MP4?token=redacted"
        ));
        assert!(!is_video_url("https://news.example.org/article-with-video"));
        assert!(!is_video_url(
            "https://youtube.com.attacker.example/watch?v=false"
        ));
        assert!(!is_video_url("file:///private/capture.mp4"));
    }

    fn page(raw: &str) -> String {
        match embed_for(raw) {
            Some(Embed::Page(url)) => url,
            other => panic!("expected a player page for {raw}, got {other:?}"),
        }
    }

    #[test]
    fn youtube_watch_shorts_and_short_links_all_reach_the_embed_endpoint() {
        let expected = "https://www.youtube-nocookie.com/embed/abc123?autoplay=1&rel=0";
        assert_eq!(page("https://www.youtube.com/watch?v=abc123"), expected);
        assert_eq!(page("https://youtu.be/abc123"), expected);
        assert_eq!(page("https://www.youtube.com/shorts/abc123"), expected);
        assert_eq!(page("https://m.youtube.com/live/abc123"), expected);
        assert_eq!(
            page("https://www.youtube.com/watch?v=abc123&t=90s"),
            format!("{expected}&start=90")
        );
    }

    #[test]
    fn provider_player_hosts_are_used_rather_than_stream_extraction() {
        assert_eq!(
            page("https://vimeo.com/123456789"),
            "https://player.vimeo.com/video/123456789?autoplay=1"
        );
        assert_eq!(
            page("https://www.dailymotion.com/video/x8abcd"),
            "https://www.dailymotion.com/embed/video/x8abcd?autoplay=1"
        );
        assert_eq!(
            page("https://streamable.com/abcd12"),
            "https://streamable.com/e/abcd12"
        );
        assert_eq!(
            page("https://www.tiktok.com/@handle/video/7300000000000000000"),
            "https://www.tiktok.com/embed/v2/7300000000000000000"
        );
    }

    #[test]
    fn chatter_post_urls_use_each_platforms_published_widget() {
        assert_eq!(
            page("https://t.me/liveuamap/12345"),
            "https://t.me/liveuamap/12345?embed=1&mute=0"
        );
        assert_eq!(
            page("https://bsky.app/profile/did:plc:xyz/post/3kabc"),
            "https://embed.bsky.app/embed/did:plc:xyz/app.bsky.feed.post/3kabc"
        );
    }

    #[test]
    fn direct_media_files_are_played_by_the_webview_itself() {
        assert_eq!(
            embed_for("https://media.example.org/clip.mp4"),
            Some(Embed::File("https://media.example.org/clip.mp4".into()))
        );
        assert_eq!(
            embed_for("https://video.bsky.app/watch/did/cid/playlist.m3u8"),
            Some(Embed::File(
                "https://video.bsky.app/watch/did/cid/playlist.m3u8".into()
            ))
        );
    }

    #[test]
    fn hosts_without_a_usable_embed_fall_back_to_the_browser() {
        // Twitch's embed requires a `parent=` matching the hosting page's
        // real domain; an embedded webview has none.
        assert_eq!(embed_for("https://www.twitch.tv/somechannel"), None);
        // A Rumble watch slug needs an API lookup to become an embed id.
        assert_eq!(embed_for("https://rumble.com/v1abcd-some-title.html"), None);
        // Channel previews and profile pages are not posts.
        assert_eq!(embed_for("https://t.me/s/liveuamap"), None);
        assert_eq!(embed_for("https://bsky.app/profile/did:plc:xyz"), None);
        // Ordinary news articles stay ordinary news articles.
        assert_eq!(embed_for("https://news.example.org/story"), None);
        // Lookalike domains must not be treated as the real host.
        assert_eq!(
            embed_for("https://youtube.com.attacker.example/watch?v=x"),
            None
        );
    }
}
