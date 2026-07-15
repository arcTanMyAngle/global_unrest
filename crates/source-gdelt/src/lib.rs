//! GDELT source adapter (M3). Live, keyless ingestion of global media
//! attention and events.
//!
//! Two independent code paths, per the GDELT reality (docs/PLAN.md §5):
//! - [`doc`] — the DOC 2.0 `artlist` **JSON API**: media-attention
//!   observations, geocoded to the source country ([`GdeltSource::fetch`]).
//! - [`events`] — the 15-minute Events **CSV-zip dumps**: discrete CAMEO
//!   events with coordinates ([`GdeltSource::fetch_events`]).
//!
//! GDELT is free to use **with attribution** (see README and the About panel).
//! Rate-limiting/backoff and the fetch scheduler arrive with the ingest loop;
//! this adapter is the fetch + normalize surface those will drive. Parsing and
//! normalization are pure and fully offline-testable; only the `fetch*` methods
//! touch the network.

pub mod country;
pub mod doc;
pub mod events;
pub mod sched;

use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};

pub use doc::DocQuery;

/// The public DOC 2.0 endpoint (keyless).
pub const DOC_ENDPOINT: &str = "https://api.gdeltproject.org/api/v2/doc/doc";

/// Pointer to the current 15-minute Events dump (`<size> <md5> <url>` lines).
pub const EVENTS_LASTUPDATE_URL: &str = "http://data.gdeltproject.org/gdeltv2/lastupdate.txt";

/// A broad civic-attention default query. Callers can override it; theme
/// filters passed to [`SignalSource::fetch`] refine it further.
pub const DEFAULT_QUERY: &str =
    "(protest OR unrest OR flood OR earthquake OR wildfire OR election OR strike)";

/// Live GDELT adapter over the DOC 2.0 JSON API.
///
/// Holds a configured [`reqwest::Client`] and the DOC query to run. `fetch`
/// maps the time window onto `startdatetime`/`enddatetime`; `normalize`
/// dispatches on [`RawRecord`]. The set of sources is closed, so the app uses
/// this concrete type (no trait objects).
pub struct GdeltSource {
    http: reqwest::Client,
    doc_endpoint: String,
    events_lastupdate_url: String,
    query: String,
    themes: Vec<String>,
    max_records: u32,
}

impl GdeltSource {
    /// Build an adapter with the default civic query against the live DOC
    /// endpoint. Fails only if the HTTP client cannot be constructed.
    pub fn new() -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("live-earth-signals/", env!("CARGO_PKG_VERSION")))
            // Bound stalls so a dead network degrades promptly instead of hanging.
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| SourceError::Other(format!("building http client: {e}")))?;
        Ok(Self {
            http,
            doc_endpoint: DOC_ENDPOINT.to_owned(),
            events_lastupdate_url: EVENTS_LASTUPDATE_URL.to_owned(),
            query: DEFAULT_QUERY.to_owned(),
            themes: Vec::new(),
            max_records: doc::MAX_RECORDS,
        })
    }

    /// Override the GDELT query expression and the theme tags stamped onto
    /// results (query provenance recorded on each event).
    pub fn with_query(mut self, query: impl Into<String>, themes: Vec<String>) -> Self {
        self.query = query.into();
        self.themes = themes;
        self
    }

    /// Override the DOC endpoint base (tests point this at a local server).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.doc_endpoint = endpoint.into();
        self
    }

    /// Override the Events `lastupdate.txt` URL (tests point this locally).
    pub fn with_events_url(mut self, url: impl Into<String>) -> Self {
        self.events_lastupdate_url = url.into();
        self
    }

    /// Cap the records requested per DOC call (clamped to `1..=MAX_RECORDS`).
    pub fn with_max_records(mut self, max: u32) -> Self {
        self.max_records = max;
        self
    }

    /// Fetch the current 15-minute Events dump: read `lastupdate.txt`, pull the
    /// `export.CSV.zip`, unzip it, and hand back one [`RawRecord::GdeltEventCsv`]
    /// per row. This is the discrete-event path, independent of DOC. Backfill of
    /// older dumps (by timestamped URL) is a scheduler concern; this always
    /// fetches the latest published file.
    pub async fn fetch_events(&self) -> Result<Vec<RawRecord>, SourceError> {
        let lastupdate = self.get(&self.events_lastupdate_url).await?;
        let txt = lastupdate
            .text()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let refs = events::parse_lastupdate(&txt)?;
        let url = events::export_url(&refs)
            .ok_or_else(|| SourceError::Other("lastupdate.txt has no export dump".into()))?;

        let bytes = self
            .get(url)
            .await?
            .bytes()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let csv = events::unzip_csv(&bytes)?;

        let out: Vec<RawRecord> = events::rows(&csv)
            .map(|r| RawRecord::GdeltEventCsv(r.to_owned()))
            .collect();
        tracing::info!(records = out.len(), "gdelt events fetched");
        Ok(out)
    }

    /// GET a URL, mapping a 429 to [`SourceError::RateLimited`] (with any
    /// `Retry-After`) and other non-2xx to [`SourceError::Http`], so the
    /// scheduler can back off. Both fetch paths share this.
    async fn get(&self, url: &str) -> Result<reqwest::Response, SourceError> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            return Err(SourceError::RateLimited { retry_after_secs });
        }
        resp.error_for_status()
            .map_err(|e| SourceError::Http(e.to_string()))
    }

    /// The DOC query this fetch will issue for `window`, incorporating any
    /// theme filter. Exposed so the scheduler/tests can inspect the request
    /// without performing it.
    pub fn doc_query(&self, window: TimeWindow, filters: &SourceFilters) -> DocQuery {
        // A theme filter refines the query and becomes the results' tags;
        // otherwise the configured query and tags stand.
        let (query, themes) = match filters.themes.as_deref() {
            Some(themes) if !themes.is_empty() => (theme_query(themes), themes.to_vec()),
            _ => (self.query.clone(), self.themes.clone()),
        };
        DocQuery {
            query,
            window,
            max_records: self.max_records,
            themes,
        }
    }
}

/// GDELT theme-filter query: `(theme:PROTEST OR theme:FLOOD)`, upper-cased as
/// GDELT expects for GKG theme tokens.
fn theme_query(themes: &[String]) -> String {
    let terms: Vec<String> = themes
        .iter()
        .map(|t| format!("theme:{}", t.trim().to_ascii_uppercase()))
        .collect();
    format!("({})", terms.join(" OR "))
}

impl SignalSource for GdeltSource {
    fn id(&self) -> SourceId {
        SourceId::Gdelt
    }

    async fn fetch(
        &self,
        window: TimeWindow,
        filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        let query = self.doc_query(window, filters);
        let url = query.to_url(&self.doc_endpoint)?;
        tracing::info!(%url, "gdelt doc fetch");

        let body = self
            .get(url.as_str())
            .await?
            .text()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;

        let mut out = Vec::new();
        for mut article in doc::articles(&body)? {
            doc::stamp_themes(&mut article, &query.themes);
            out.push(RawRecord::GdeltDocJson(article));
        }
        tracing::info!(records = out.len(), "gdelt doc fetched");
        Ok(out)
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::GdeltDocJson(v) => doc::normalize(v).map(|e| vec![e]),
            RawRecord::GdeltEventCsv(row) => events::normalize(row),
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("gdelt source received a foreign record: {other:?}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> GdeltSource {
        GdeltSource::new().unwrap()
    }

    #[test]
    fn theme_filter_builds_gdelt_theme_query() {
        let src = source();
        let window = TimeWindow::new(chrono::Utc::now(), chrono::Utc::now());
        let filters = SourceFilters {
            themes: Some(vec!["protest".into(), "flood".into()]),
            ..Default::default()
        };
        let q = src.doc_query(window, &filters);
        assert_eq!(q.query, "(theme:PROTEST OR theme:FLOOD)");
        assert_eq!(q.themes, vec!["protest", "flood"]);
    }

    #[test]
    fn no_theme_filter_uses_default_query() {
        let src = source();
        let window = TimeWindow::new(chrono::Utc::now(), chrono::Utc::now());
        let q = src.doc_query(window, &SourceFilters::default());
        assert_eq!(q.query, DEFAULT_QUERY);
        assert!(q.themes.is_empty());
    }

    #[test]
    fn normalize_rejects_foreign_records() {
        let src = source();
        let err = src
            .normalize(&RawRecord::FixtureJson(json!({"shape": "gdelt_doc"})))
            .unwrap_err();
        assert!(matches!(
            err,
            NormalizeError::InvalidValue {
                field: "record",
                ..
            }
        ));
    }

    #[test]
    fn normalize_dispatches_doc_records() {
        let src = source();
        let mut article = json!({
            "url": "https://worldpost.example/a/1",
            "title": "[synthetic] Election commission announcement draws crowds",
            "seendate": "20260620T144000Z",
            "domain": "worldpost.example",
            "sourcecountry": "Kenya"
        });
        doc::stamp_themes(&mut article, &["elections".into()]);
        let evs = src.normalize(&RawRecord::GdeltDocJson(article)).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].country_iso, "KEN");
        assert_eq!(evs[0].themes, vec!["elections"]);
    }
}
