//! GDELT DOC 2.0, restricted to video-hosting domains.
//!
//! The ingest path (`source-gdelt`) asks DOC for *articles*, which is why the
//! database holds 4,663 GDELT rows and not one of them links to a video: news
//! outlets link to their own story pages. Asking the same endpoint the other
//! question — "which articles in this window link to a video host, about this
//! place" — is a different query against the same public API, and it is the
//! one that actually returns footage.
//!
//! Pure: [`query_expression`] and [`request_url`] build the call,
//! [`hits`] parses a response. The network round trip is in `crate::live`.

use chrono::{DateTime, Utc};
use core_types::SourceError;
use url::Url;

use crate::{MediaHit, Provider, search_terms, short_title};

/// The live DOC endpoint (same one `source-gdelt` uses).
pub const DOC_ENDPOINT: &str = "https://api.gdeltproject.org/api/v2/doc/doc";

/// Video hosts worth asking GDELT about.
///
/// Kept in step with `core_types::is_video_url`'s host list — asking for a
/// domain whose URLs the app would not then classify as video produces hits
/// that vanish at the filtering step. `twitch.tv` is included even though it
/// has no usable embed: a live-stream link is still worth surfacing, it just
/// opens in the browser instead of inline.
pub const VIDEO_DOMAINS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "vimeo.com",
    "dailymotion.com",
    "rumble.com",
    "streamable.com",
    "tiktok.com",
    "twitch.tv",
];

/// GDELT caps `artlist` at 250 records.
pub const MAX_RECORDS: usize = 250;

/// Build the DOC query expression for a place (+ optional topic).
///
/// **Every term is a bare word and the OR group holds no quoted phrase.** DOC
/// documents phrase syntax, but a quoted phrase used as an alternative inside
/// a parenthesized OR group is rejected with HTTP 200 and a sentence of prose
/// — the failure mode that once emptied this project's whole media-attention
/// panel. [`crate::search_terms`] strips the user's text to bare words for
/// exactly this reason; the domain group below is built from constants.
pub fn query_expression(place: &str, topic: &str) -> Option<String> {
    let place = search_terms(place);
    if place.is_empty() {
        return None;
    }
    let domains = VIDEO_DOMAINS
        .iter()
        .map(|d| format!("domain:{d}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let topic = search_terms(topic);
    let mut expr = place;
    if !topic.is_empty() {
        expr.push(' ');
        expr.push_str(&topic);
    }
    Some(format!("{expr} ({domains})"))
}

/// Build the full `artlist` request URL for a window.
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
        .map_err(|e| SourceError::Other(format!("bad DOC endpoint `{endpoint}`: {e}")))?;
    url.query_pairs_mut()
        .append_pair("query", &query)
        .append_pair("mode", "artlist")
        .append_pair("format", "json")
        .append_pair("maxrecords", &limit.clamp(1, MAX_RECORDS).to_string())
        .append_pair("sort", "datedesc")
        .append_pair("startdatetime", &start.format("%Y%m%d%H%M%S").to_string())
        .append_pair("enddatetime", &end.format("%Y%m%d%H%M%S").to_string());
    Ok(url)
}

/// Parse a DOC `artlist` body into video hits.
///
/// Articles whose URL does not classify as video are dropped rather than
/// trusted: `domain:` matches the *article's* domain, and GDELT occasionally
/// indexes a channel or profile page on a video host.
pub fn hits(body: &str) -> Result<Vec<MediaHit>, SourceError> {
    let articles = source_gdelt_articles(body)?;
    let mut out = Vec::new();
    for article in articles {
        let Some(url) = article.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        if !core_types::is_video_url(url) {
            continue;
        }
        let Some(ts_utc) = article
            .get("seendate")
            .and_then(|v| v.as_str())
            .and_then(parse_seendate)
        else {
            continue;
        };
        out.push(MediaHit {
            url: url.to_string(),
            title: short_title(article.get("title").and_then(|v| v.as_str()).unwrap_or(url)),
            provider: Provider::Gdelt,
            ts_utc,
            origin: article
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(out)
}

/// Pull the `articles` array, naming DOC's plain-text query rejection.
///
/// A rejected query is not a non-2xx status — DOC answers `200` with one
/// sentence of prose, which would otherwise surface as an opaque JSON parse
/// error and read as "nothing is happening in this place".
fn source_gdelt_articles(body: &str) -> Result<Vec<serde_json::Value>, SourceError> {
    let doc: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        let trimmed = body.trim();
        if !trimmed.is_empty() && trimmed.len() <= 400 && !trimmed.starts_with(['{', '[', '<']) {
            SourceError::Other(format!(
                "GDELT rejected the media query: {}",
                trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
            ))
        } else {
            SourceError::Other(format!("DOC response was not JSON: {e}"))
        }
    })?;
    match doc.get("articles") {
        Some(serde_json::Value::Array(items)) => Ok(items.clone()),
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(_) => Err(SourceError::Other(
            "DOC `articles` was not an array".to_string(),
        )),
    }
}

/// GDELT `seendate` is `YYYYMMDDTHHMMSSZ`, with RFC 3339 as a fallback.
fn parse_seendate(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ") {
        return Some(naive.and_utc());
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_or_group_holds_only_bare_domain_terms() {
        let expr = query_expression("Colombia", "earthquake").unwrap();
        assert!(expr.starts_with("colombia earthquake ("), "{expr}");
        assert!(expr.contains("domain:youtube.com OR domain:youtu.be"), "{expr}");
        // The rejection-causing shape: no quoted phrase anywhere.
        assert!(!expr.contains('"'), "{expr}");
        // And user text cannot inject its own parentheses.
        let injected = query_expression("Colombia) OR (theme:TERROR", "").unwrap();
        assert_eq!(injected.matches('(').count(), 1);
        assert_eq!(injected.matches(')').count(), 1);
    }

    #[test]
    fn a_place_is_required() {
        assert_eq!(query_expression("  ", "earthquake"), None);
    }

    #[test]
    fn request_url_carries_the_window_and_a_clamped_limit() {
        use chrono::TimeZone;
        let url = request_url(
            "https://example.test/doc",
            "Colombia",
            "",
            Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 13, 6, 30, 0).unwrap(),
            10_000,
        )
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["mode"], "artlist");
        assert_eq!(pairs["format"], "json");
        assert_eq!(pairs["maxrecords"], MAX_RECORDS.to_string());
        assert_eq!(pairs["startdatetime"], "20260810000000");
        assert_eq!(pairs["enddatetime"], "20260813063000");
    }

    #[test]
    fn only_video_urls_survive_parsing() {
        let body = r#"{"articles":[
            {"url":"https://www.youtube.com/watch?v=abc","title":"Quake footage","seendate":"20260810T120000Z","domain":"youtube.com"},
            {"url":"https://news.example.org/story","title":"Article","seendate":"20260810T120000Z","domain":"news.example.org"},
            {"url":"https://www.youtube.com/watch?v=def","seendate":"nonsense","domain":"youtube.com"}
        ]}"#;
        let hits = hits(body).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(hits[0].title, "Quake footage");
        assert_eq!(hits[0].provider, Provider::Gdelt);
    }

    #[test]
    fn a_plain_text_rejection_is_named_rather_than_read_as_no_results() {
        let err = hits("One or more of your parenthetical clauses had an error in it.\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("GDELT rejected the media query"), "{err}");
        // Missing/empty is genuinely "no results", not an error.
        assert!(hits("{}").unwrap().is_empty());
    }
}
