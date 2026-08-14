//! On-demand media lookup over the same public channels the ingest path
//! counts — the pure half.
//!
//! **This module is the one exception to this crate's aggregate-only rule**,
//! and it is a deliberate, user-directed one. Everything else here hands
//! message text to [`chatter::ChatterAccumulator::observe`] and drops it in
//! the same call; nothing else returns a caption, a post URL, or a channel
//! attribution to a caller. This module returns all three, because a person
//! asked to *see* the footage published about one named place.
//!
//! The relaxation is bounded in the same way `crates/media-search` bounds it:
//!
//! - **Nothing runs on a timer.** A search happens only from an explicit
//!   click, for one place and one bounded window.
//! - **Nothing is stored.** Results are handed to the UI and dropped when the
//!   next search replaces them. No hit ever reaches the database, a log line,
//!   or the chatter rollup.
//! - **The ingest path is untouched.** [`crate::ChannelSweep`] and
//!   `chatter`'s `(place, topic, window) -> count` boundary have not moved.
//! - **No per-person attribution.** A hit is attributed to the *channel*, and
//!   this module never reads a message's sender: Telegram channel posts can
//!   carry a signing author, and that is a named individual we have no reason
//!   to surface.
//!
//! Read docs/SAFETY_AND_PRIVACY.md's "On-demand media lookup" section before
//! widening any of that.
//!
//! Pure by design: URL construction, the video test, and hit construction all
//! live here so they are unit-testable without a Telegram session. Only the
//! MTProto sweep is in `live.rs`.

use chrono::{DateTime, Utc};
use media_search::{MediaHit, Provider, search_terms, short_title};

/// How many hits one channel may contribute to a single search.
///
/// Small on purpose. The allowlist is swept in full for every search, so this
/// is multiplied by the channel count; a low cap keeps one loud channel from
/// filling the panel and keeps the whole sweep to a handful of calls.
pub const PER_CHANNEL_LIMIT: usize = 5;

/// File extensions treated as video when a document arrives without a MIME
/// type. Deliberately the same set `core_types::embed_for` will actually play,
/// so a hit is never offered for something the widget can only offer as a
/// download.
const VIDEO_EXTENSIONS: &[&str] = &[".mp4", ".webm", ".mov", ".m4v"];

/// Build the string handed to Telegram's server-side message search.
///
/// Telegram's search is fuzzy and has no operator syntax to escape, but the
/// text still comes from a person, so it goes through
/// [`media_search::search_terms`] like every other provider's — one
/// sanitiser, one place, no per-provider exceptions to get wrong.
///
/// `None` means the place did not survive sanitising, which is the caller's
/// signal not to sweep at all.
pub fn query_text(place: &str, topic: &str) -> Option<String> {
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

/// The public post page for a message, or `None` if it could not be a real
/// one.
///
/// `core_types::embed_for` maps this onto Telegram's own `?embed=1` widget,
/// which is what plays the video — the underlying file is never fetched or
/// resolved here.
///
/// Both guards matter: a non-positive id and a channel name carrying `/`,
/// `@`, or whitespace would each produce a URL that points somewhere other
/// than the intended post. [`crate::ALLOWED_CHANNELS`] already holds to the
/// name rule (a test pins it), so this is the belt to that braces.
pub fn post_url(channel: &str, message_id: i32) -> Option<String> {
    if message_id <= 0 {
        return None;
    }
    if channel.is_empty()
        || channel.contains(['@', '/'])
        || channel.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(format!("https://t.me/{channel}/{message_id}"))
}

/// Does this attachment look like video the embed widget can play?
///
/// The server-side filter already restricts the search to video messages, so
/// this is a second check rather than the only one — it exists because
/// "video" on Telegram also covers documents whose MIME type says otherwise.
/// A missing MIME type falls back to the file name; a MIME type that is
/// present and not `video/*` is believed.
pub fn is_video_attachment(mime_type: Option<&str>, file_name: Option<&str>) -> bool {
    if let Some(mime) = mime_type {
        return mime.trim().to_ascii_lowercase().starts_with("video/");
    }
    let Some(name) = file_name else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    VIDEO_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

/// Turn one matched message into a hit.
///
/// `caption` is the message's own text, trimmed to a single display line by
/// [`media_search::short_title`] — a label for the link, never a reproduction
/// of the post. A caption-less clip still gets a readable row.
pub fn hit(channel: &str, message_id: i32, caption: &str, date: DateTime<Utc>) -> Option<MediaHit> {
    let url = post_url(channel, message_id)?;
    let title = if caption.trim().is_empty() {
        format!("video posted by @{channel}")
    } else {
        short_title(caption)
    };
    Some(MediaHit {
        url,
        title,
        provider: Provider::Telegram,
        ts_utc: date,
        origin: format!("@{channel}"),
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::ALLOWED_CHANNELS;

    const MESSAGE_ID: i32 = 12345;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 13, 9, 30, 0).unwrap()
    }

    #[test]
    fn user_text_is_sanitised_the_same_way_every_other_provider_sanitises_it() {
        assert_eq!(
            query_text("Colombia", "earthquake"),
            Some("colombia earthquake".to_string())
        );
        assert_eq!(
            query_text("  Port-au-Prince ", ""),
            Some("port au prince".into())
        );
        assert_eq!(query_text("***", "earthquake"), None);
    }

    /// The whole reason the hit URL is the post page: Telegram's own widget
    /// plays it, so nothing has to resolve the underlying file.
    #[test]
    fn the_hit_url_is_the_post_page_that_embed_for_can_play() {
        let hit = hit("liveuamap", MESSAGE_ID, "flooding in Bogota", ts()).unwrap();
        assert_eq!(hit.url, "https://t.me/liveuamap/12345");
        assert!(core_types::embed_for(&hit.url).is_some());
        assert_eq!(hit.origin, "@liveuamap");
        assert_eq!(hit.title, "flooding in Bogota");
        assert_eq!(hit.provider, Provider::Telegram);
        assert_eq!(hit.ts_utc, ts());
    }

    #[test]
    fn a_caption_less_clip_still_gets_a_readable_label() {
        let hit = hit("DVBTV", MESSAGE_ID, " \n\t ", ts()).unwrap();
        assert_eq!(hit.title, "video posted by @DVBTV");
    }

    #[test]
    fn nothing_that_could_not_be_a_real_post_becomes_a_link() {
        assert_eq!(post_url("liveuamap", 0), None);
        assert_eq!(post_url("liveuamap", -1), None);
        // A name with a slash would point the URL at another channel entirely.
        assert_eq!(post_url("liveuamap/evil", MESSAGE_ID), None);
        assert_eq!(post_url("@liveuamap", MESSAGE_ID), None);
        assert_eq!(post_url("live uamap", MESSAGE_ID), None);
        assert_eq!(post_url("", MESSAGE_ID), None);
    }

    /// Every allowlisted channel must actually survive URL construction —
    /// otherwise a search would silently skip it.
    #[test]
    fn every_allowlisted_channel_produces_a_playable_post_url() {
        for channel in ALLOWED_CHANNELS {
            let url = post_url(channel, MESSAGE_ID)
                .unwrap_or_else(|| panic!("{channel} produced no post URL"));
            assert!(
                core_types::embed_for(&url).is_some(),
                "{channel} produced an unplayable URL: {url}"
            );
        }
    }

    #[test]
    fn a_present_mime_type_is_believed_and_a_missing_one_falls_back_to_the_name() {
        assert!(is_video_attachment(Some("video/mp4"), None));
        assert!(is_video_attachment(Some(" VIDEO/QuickTime "), None));
        assert!(!is_video_attachment(
            Some("application/pdf"),
            Some("clip.mp4")
        ));
        assert!(!is_video_attachment(Some("image/jpeg"), None));
        assert!(is_video_attachment(None, Some("Bogota-CAPTURE.MP4")));
        assert!(!is_video_attachment(None, Some("report.pdf")));
        assert!(!is_video_attachment(None, None));
    }
}
