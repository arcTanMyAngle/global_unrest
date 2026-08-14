//! On-demand, place-scoped media lookup.
//!
//! **This crate is deliberately not a `SignalSource`.** It ingests nothing,
//! stores nothing, and never writes to the database. It answers one question,
//! only when a person asks it: *"show me footage published about this place in
//! this time window."* The results live in the UI for as long as that panel is
//! open and are gone when it closes.
//!
//! # Why on-demand rather than stored
//!
//! The map's job is unchanged: aggregate counts, "where the most action
//! happened", nothing per-person. What this adds is a *research* action the
//! user initiates for one place at a time. Fetching only what was asked for is
//! both the privacy-minimising choice and the bandwidth-minimising one — the
//! alternative, bulk-collecting every post URL for every place on the chance
//! someone looks, is the thing worth avoiding.
//!
//! This is a deliberate, user-directed relaxation of
//! docs/SAFETY_AND_PRIVACY.md's hard rule 6, which forbade a post URL from
//! Bluesky/Telegram existing anywhere in the process. The rule stands for the
//! *ingest* path — `crates/chatter`'s `(place, topic, window) -> count`
//! boundary has not moved, and `source-bluesky`/`source-telegram` still cannot
//! see a URL. What changed is that a person may now pull a named place's
//! public posts into a transient result list. Read that document's
//! "On-demand media lookup" section before widening this further; in
//! particular, nothing here may be persisted or aggregated across queries.
//!
//! # What is retrieved
//!
//! Only what the platform's own public API returns for a public post, and only
//! items that actually carry video. No account is followed, no history is
//! walked beyond the requested window, and no result is attributed to a person
//! beyond the handle/channel the platform prints on the post itself — which is
//! what makes the link openable at all.

use chrono::{DateTime, Utc};

pub mod bluesky;
pub mod gdelt;
#[cfg(feature = "live")]
mod live;

#[cfg(feature = "live")]
pub use live::MediaSearch;

/// Which public API a hit came from. Displayed next to every result, because
/// the three have very different reliability: a GDELT hit is a news outlet's
/// own link, a Bluesky/Telegram hit is an unverified public post.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    /// GDELT DOC 2.0, restricted to video-hosting domains.
    Gdelt,
    /// Bluesky's public `app.bsky.feed.searchPosts` (keyless).
    Bluesky,
    /// A public Telegram channel from `source-telegram`'s allowlist.
    Telegram,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Gdelt => "news",
            Provider::Bluesky => "bluesky",
            Provider::Telegram => "telegram",
        }
    }

    /// Is this hit a public social post rather than a published article?
    ///
    /// The UI labels these differently: an unverified post from an unknown
    /// account is not the same claim as a wire story, and merging the two
    /// into one undifferentiated list would be the same mistake as merging
    /// media attention with event data.
    pub fn is_social(self) -> bool {
        matches!(self, Provider::Bluesky | Provider::Telegram)
    }
}

/// One retrieved media item.
///
/// Everything here is what a public page already shows to anyone who opens
/// it. There is no field for anything the platform does not print on the post,
/// and no field that survives the panel closing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaHit {
    /// The link to open or embed. For social posts this is the *post* URL, not
    /// an extracted stream — the post's own embed widget does the playing.
    pub url: String,
    /// Headline, caption, or a short label. Truncated; never a full body.
    pub title: String,
    pub provider: Provider,
    pub ts_utc: DateTime<Utc>,
    /// Outlet domain or channel handle — where it was published.
    pub origin: String,
}

/// Captions/headlines are shown as a one-line label, not reproduced in full.
pub const MAX_TITLE_CHARS: usize = 160;

/// Trim a caption/headline to a single display line.
pub fn short_title(raw: &str) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_TITLE_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(MAX_TITLE_CHARS).collect();
    out.push('…');
    out
}

/// What the user asked for: a place, an optional extra term, and a window.
///
/// A query is always place-scoped — there is no "everything, everywhere"
/// form, by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaQuery {
    /// Free text naming a place ("Colombia", "Port-au-Prince"). Sanitised
    /// before it reaches any provider; see [`search_terms`].
    pub place: String,
    /// Optional narrowing term ("earthquake", "protest"). Same sanitising.
    pub topic: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Per-provider cap. Kept small: this is a look, not a harvest.
    pub limit: usize,
}

impl MediaQuery {
    pub fn is_valid(&self) -> bool {
        !search_terms(&self.place).is_empty() && self.end > self.start
    }
}

/// Reduce free-typed input to bare search words.
///
/// Every provider here has its own query syntax with its own operators, and
/// GDELT in particular answers a malformed expression with **HTTP 200 and a
/// sentence of prose** (see `source_gdelt::DEFAULT_QUERY`). Rather than escape
/// per-provider, user text is stripped to letters, digits, and single spaces
/// before it is ever interpolated, so it cannot carry an operator into any of
/// them.
pub fn search_terms(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Merge per-provider results into one newest-first list, dropping duplicates.
///
/// The same clip routinely reaches us twice — a news article and a Bluesky
/// post both linking one YouTube video — so identity is the URL, not the
/// provider. The first occurrence wins, which is why callers pass the
/// providers in the order they'd rather attribute to.
///
/// The sort is by timestamp *only*, deliberately: `sort_by` is stable, so ties
/// keep the caller's order and that attribution preference survives. Adding a
/// URL tiebreak would silently hand ties to whichever copy sorts lower.
pub fn merge(mut hits: Vec<MediaHit>) -> Vec<MediaHit> {
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.ts_utc));
    let mut seen = std::collections::HashSet::new();
    hits.retain(|hit| seen.insert(hit.url.to_lowercase()));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn hit(url: &str, provider: Provider, secs: i64) -> MediaHit {
        MediaHit {
            url: url.to_string(),
            title: "t".into(),
            provider,
            ts_utc: ts(secs),
            origin: "o".into(),
        }
    }

    #[test]
    fn user_text_cannot_carry_query_operators_into_a_provider() {
        // Parentheses and quotes are exactly what makes GDELT answer 200 with
        // a plain-text rejection instead of results.
        assert_eq!(
            search_terms("(Colombia OR \"Bogotá\") domain:evil.example"),
            "colombia or bogotá domain evil example"
        );
        assert_eq!(search_terms("  Port-au-Prince "), "port au prince");
        assert_eq!(search_terms("!!!"), "");
        // Bounded, so one pasted paragraph cannot become a 400-term query.
        assert_eq!(search_terms("a b c d e f g h"), "a b c d e f");
    }

    #[test]
    fn a_query_without_a_usable_place_is_not_valid() {
        let base = MediaQuery {
            place: "Colombia".into(),
            topic: String::new(),
            start: ts(0),
            end: ts(3600),
            limit: 20,
        };
        assert!(base.is_valid());
        assert!(
            !MediaQuery {
                place: "***".into(),
                ..base.clone()
            }
            .is_valid()
        );
        assert!(
            !MediaQuery {
                end: ts(0),
                ..base.clone()
            }
            .is_valid()
        );
    }

    #[test]
    fn merge_is_newest_first_and_deduplicates_across_providers() {
        let merged = merge(vec![
            hit("https://youtu.be/a", Provider::Bluesky, 100),
            hit("https://youtu.be/b", Provider::Gdelt, 300),
            // Same clip, reached through a news article as well.
            hit("https://youtu.be/A", Provider::Gdelt, 100),
            hit("https://t.me/chan/7", Provider::Telegram, 200),
        ]);
        let urls: Vec<&str> = merged.iter().map(|h| h.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://youtu.be/b",
                "https://t.me/chan/7",
                "https://youtu.be/a"
            ]
        );
    }

    #[test]
    fn titles_are_one_line_and_bounded() {
        assert_eq!(short_title("  two\n lines "), "two lines");
        let long = "x".repeat(MAX_TITLE_CHARS + 50);
        let short = short_title(&long);
        assert_eq!(short.chars().count(), MAX_TITLE_CHARS + 1);
        assert!(short.ends_with('…'));
    }
}
