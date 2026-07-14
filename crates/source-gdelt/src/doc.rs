//! GDELT DOC 2.0 API (`artlist`, JSON) — the media-attention path.
//!
//! The DOC 2.0 API is a keyless JSON REST endpoint. `artlist` mode returns a
//! flat list of matching articles; each carries `url`, `title`, `seendate`,
//! `domain`, `language`, and `sourcecountry` — but **no per-article
//! coordinates**. We therefore emit each article as a
//! [`EventKind::NewsAttention`] observation geocoded to its **source country**
//! at [`LocationPrecision::Country`] (see [`crate::country`]). That is honest
//! about the feed's granularity and matches the precision rendering contract:
//! country-level attention shades regions, never fake point hotspots.
//!
//! Themes: `artlist` has none, but a DOC query is usually *for* a theme
//! (`theme:PROTEST`). The fetcher stamps the query's themes onto each article
//! under the private `les_query_themes` key so normalization can record that
//! provenance without inventing per-article topics.
//!
//! Everything here is pure and offline-testable: [`DocQuery::to_url`] builds
//! the request, [`articles`] parses a response body, and [`normalize`] maps
//! one article. The network round-trip lives in [`crate::GdeltSource`].

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, NormalizeError, SourceError,
    SourceId, TimeWindow, event_id,
};
use serde_json::{Value, json};
use url::Url;

use crate::country;

/// Private key under which the fetcher stamps a query's themes onto each
/// parsed article (so [`normalize`] stays a pure function of its input).
pub(crate) const QUERY_THEMES_KEY: &str = "les_query_themes";

/// GDELT caps `artlist` at 250 records per request.
pub const MAX_RECORDS: u32 = 250;

/// Confidence assigned to country-precision attention (matches the fixture
/// source's `country` mapping so the two paths score comparably).
const COUNTRY_CONFIDENCE: f32 = 0.4;

/// A DOC 2.0 `artlist` request: a GDELT query expression over a time window.
#[derive(Debug, Clone, PartialEq)]
pub struct DocQuery {
    /// GDELT query expression, e.g. `theme:PROTEST` or `flood sourcelang:eng`.
    pub query: String,
    /// Half-open UTC window; mapped to `startdatetime`/`enddatetime`.
    pub window: TimeWindow,
    /// Clamped to `1..=MAX_RECORDS`.
    pub max_records: u32,
    /// Theme tags stamped onto every result article (query provenance).
    pub themes: Vec<String>,
}

impl DocQuery {
    /// Build the request URL against a DOC endpoint base (the real endpoint is
    /// `https://api.gdeltproject.org/api/v2/doc/doc`; tests pass their own).
    pub fn to_url(&self, endpoint: &str) -> Result<Url, SourceError> {
        if self.query.trim().is_empty() {
            return Err(SourceError::Other("empty GDELT query".into()));
        }
        let mut url = Url::parse(endpoint)
            .map_err(|e| SourceError::Other(format!("bad DOC endpoint `{endpoint}`: {e}")))?;
        let max = self.max_records.clamp(1, MAX_RECORDS);
        url.query_pairs_mut()
            .append_pair("query", self.query.trim())
            .append_pair("mode", "artlist")
            .append_pair("format", "json")
            .append_pair("maxrecords", &max.to_string())
            .append_pair("sort", "datedesc")
            .append_pair("startdatetime", &fmt_datetime(self.window.start))
            .append_pair("enddatetime", &fmt_datetime(self.window.end));
        Ok(url)
    }
}

/// GDELT `startdatetime`/`enddatetime` format: 14-digit `YYYYMMDDHHMMSS` UTC.
fn fmt_datetime(ts: DateTime<Utc>) -> String {
    ts.format("%Y%m%d%H%M%S").to_string()
}

/// Parse a DOC `artlist` JSON body into its article objects. A body with no
/// `articles` key (GDELT's shape for "no matches") yields an empty list, not
/// an error.
pub fn articles(body: &str) -> Result<Vec<Value>, SourceError> {
    let doc: Value = serde_json::from_str(body)
        .map_err(|e| SourceError::Other(format!("DOC response was not JSON: {e}")))?;
    match doc.get("articles") {
        Some(Value::Array(items)) => Ok(items.clone()),
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(other) => Err(SourceError::Other(format!(
            "DOC `articles` was {}, expected array",
            kind_of(other)
        ))),
    }
}

/// Stamp a query's theme tags onto an article so provenance survives into
/// [`normalize`]. Overwrites any pre-existing key (namespaced, so it won't
/// collide with real GDELT fields). The fetcher calls this on every article;
/// exposed so tests and schedulers can reproduce the same stamping offline.
pub fn stamp_themes(article: &mut Value, themes: &[String]) {
    if let Some(obj) = article.as_object_mut() {
        obj.insert(QUERY_THEMES_KEY.into(), json!(themes));
    }
}

/// Normalize one DOC `artlist` article → a `NewsAttention` observation.
///
/// Fails per record (never panics, never drops): a missing url/seendate or an
/// unrecognized `sourcecountry` returns an error the caller logs to
/// `ingest_log`.
pub fn normalize(v: &Value) -> Result<GeoTemporalEvent, NormalizeError> {
    let url = req_str(v, "url")?.to_owned();
    let ts_utc = parse_seendate(req_str(v, "seendate")?)?;
    let country_name = req_str(v, "sourcecountry")?;
    let country = country::resolve(country_name).ok_or_else(|| NormalizeError::InvalidValue {
        field: "sourcecountry",
        detail: format!("unrecognized country `{country_name}`"),
    })?;
    let h3_cell =
        geo_utils::cell_for_latlon(country.lat, country.lon, H3_RESOLUTION).map_err(|e| {
            NormalizeError::InvalidValue {
                field: "sourcecountry",
                detail: format!("h3 assignment for `{country_name}` failed: {e}"),
            }
        })?;

    let themes = v
        .get(QUERY_THEMES_KEY)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let domains = v
        .get("domain")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(|d| vec![d.to_owned()])
        .unwrap_or_default();

    Ok(GeoTemporalEvent {
        id: event_id(SourceId::Gdelt, &url),
        source: SourceId::Gdelt,
        source_event_id: url.clone(),
        kind: EventKind::NewsAttention,
        themes,
        ts_utc,
        ingested_at: Utc::now(),
        lat: country.lat,
        lon: country.lon,
        location_precision: LocationPrecision::Country,
        location_confidence: COUNTRY_CONFIDENCE,
        country_iso: country.iso_a3.to_owned(),
        admin1: None,
        h3_cell,
        // One article per record; domain is one distinct outlet.
        article_count: 1,
        distinct_source_count: 1,
        severity: None,
        headline: v
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        outlet_domains: domains,
        urls: vec![url],
    })
}

/// GDELT `seendate` is `YYYYMMDDTHHMMSSZ`; accept RFC 3339 as a fallback.
fn parse_seendate(s: &str) -> Result<DateTime<Utc>, NormalizeError> {
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ") {
        return Ok(Utc.from_utc_datetime(&naive));
    }
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| NormalizeError::InvalidValue {
            field: "seendate",
            detail: format!("`{s}`: {e}"),
        })
}

fn req_str<'v>(v: &'v Value, field: &'static str) -> Result<&'v str, NormalizeError> {
    v.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(NormalizeError::MissingField(field))
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn window() -> TimeWindow {
        TimeWindow::new(
            Utc.with_ymd_and_hms(2026, 6, 20, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 6, 21, 0, 0, 0).unwrap(),
        )
    }

    #[test]
    fn builds_canonical_doc_url() {
        let q = DocQuery {
            query: "theme:PROTEST".into(),
            window: window(),
            max_records: 75,
            themes: vec!["protest".into()],
        };
        let url = q
            .to_url("https://api.gdeltproject.org/api/v2/doc/doc")
            .unwrap();
        assert_eq!(url.path(), "/api/v2/doc/doc");
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["query"], "theme:PROTEST");
        assert_eq!(pairs["mode"], "artlist");
        assert_eq!(pairs["format"], "json");
        assert_eq!(pairs["maxrecords"], "75");
        assert_eq!(pairs["sort"], "datedesc");
        assert_eq!(pairs["startdatetime"], "20260620000000");
        assert_eq!(pairs["enddatetime"], "20260621000000");
    }

    #[test]
    fn url_clamps_maxrecords_and_encodes_query() {
        let q = DocQuery {
            query: "flood OR \"heavy rain\"".into(),
            window: window(),
            max_records: 10_000,
            themes: vec![],
        };
        let url = q.to_url("https://example.test/doc").unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["maxrecords"], MAX_RECORDS.to_string());
        // Round-trips through decoding despite quotes/spaces.
        assert_eq!(pairs["query"], "flood OR \"heavy rain\"");
    }

    #[test]
    fn empty_query_is_rejected() {
        let q = DocQuery {
            query: "   ".into(),
            window: window(),
            max_records: 50,
            themes: vec![],
        };
        assert!(q.to_url("https://example.test/doc").is_err());
    }

    #[test]
    fn articles_handles_missing_and_present() {
        assert_eq!(articles("{}").unwrap().len(), 0);
        assert_eq!(articles(r#"{"articles":null}"#).unwrap().len(), 0);
        assert_eq!(
            articles(r#"{"articles":[{"url":"a"},{"url":"b"}]}"#)
                .unwrap()
                .len(),
            2
        );
        assert!(articles("not json").is_err());
        assert!(articles(r#"{"articles":42}"#).is_err());
    }

    fn sample_article() -> Value {
        json!({
            "url": "https://globalwire.example/a/1001",
            "title": "[synthetic] Transit workers rally in central Paris",
            "seendate": "20260620T081500Z",
            "domain": "globalwire.example",
            "language": "French",
            "sourcecountry": "France"
        })
    }

    #[test]
    fn normalizes_article_to_country_attention() {
        let mut a = sample_article();
        stamp_themes(&mut a, &["PROTEST".into(), "Labor".into()]);
        let e = normalize(&a).unwrap();

        assert_eq!(e.source, SourceId::Gdelt);
        assert_eq!(
            e.id,
            event_id(SourceId::Gdelt, "https://globalwire.example/a/1001")
        );
        assert_eq!(e.kind, EventKind::NewsAttention);
        assert_eq!(e.location_precision, LocationPrecision::Country);
        assert!((e.location_confidence - COUNTRY_CONFIDENCE).abs() < 1e-6);
        assert_eq!(e.country_iso, "FRA");
        assert_eq!(e.admin1, None);
        // Query themes are recorded, lowercased.
        assert_eq!(e.themes, vec!["protest", "labor"]);
        assert_eq!(
            e.ts_utc,
            Utc.with_ymd_and_hms(2026, 6, 20, 8, 15, 0).unwrap()
        );
        assert_eq!(e.article_count, 1);
        assert_eq!(e.distinct_source_count, 1);
        assert_eq!(e.outlet_domains, vec!["globalwire.example"]);
        assert_eq!(e.urls, vec!["https://globalwire.example/a/1001"]);
        assert_eq!(e.severity, None);
        // Centroid-derived cell is a valid res-3 index.
        assert_eq!(
            e.h3_cell,
            geo_utils::cell_for_latlon(e.lat, e.lon, H3_RESOLUTION).unwrap()
        );
    }

    #[test]
    fn no_query_themes_yields_empty_themes() {
        let e = normalize(&sample_article()).unwrap();
        assert!(e.themes.is_empty());
    }

    #[test]
    fn parses_rfc3339_seendate_fallback() {
        let mut a = sample_article();
        a["seendate"] = json!("2026-06-20T08:15:00Z");
        let e = normalize(&a).unwrap();
        assert_eq!(
            e.ts_utc,
            Utc.with_ymd_and_hms(2026, 6, 20, 8, 15, 0).unwrap()
        );
    }

    #[test]
    fn missing_fields_fail_per_record() {
        // Missing url.
        let mut a = sample_article();
        a.as_object_mut().unwrap().remove("url");
        assert!(matches!(
            normalize(&a).unwrap_err(),
            NormalizeError::MissingField("url")
        ));

        // Missing seendate.
        let mut a = sample_article();
        a.as_object_mut().unwrap().remove("seendate");
        assert!(matches!(
            normalize(&a).unwrap_err(),
            NormalizeError::MissingField("seendate")
        ));

        // Unknown source country.
        let mut a = sample_article();
        a["sourcecountry"] = json!("Atlantis");
        assert!(matches!(
            normalize(&a).unwrap_err(),
            NormalizeError::InvalidValue {
                field: "sourcecountry",
                ..
            }
        ));

        // Unparseable seendate.
        let mut a = sample_article();
        a["seendate"] = json!("nope");
        assert!(matches!(
            normalize(&a).unwrap_err(),
            NormalizeError::InvalidValue {
                field: "seendate",
                ..
            }
        ));
    }
}
