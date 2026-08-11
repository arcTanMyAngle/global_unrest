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
}
