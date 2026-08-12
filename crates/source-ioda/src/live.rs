//! The network path (feature `live`): one GET of `/outages/events` per poll.
//!
//! IODA states no rate limit, but the callers poll on a fixed multi-minute
//! cadence through the shared limiter/backoff scheduler regardless, and 429s
//! (if IODA ever sends one) map to [`SourceError::RateLimited`].

use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use geo_utils::CountryIndex;
use serde_json::Value;

use crate::{EVENTS_URL, NE_COUNTRIES};

/// Live IODA adapter over the `/outages/events` JSON endpoint.
pub struct IodaSource {
    http: reqwest::Client,
    events_url: String,
    countries: CountryIndex,
}

impl IodaSource {
    /// Build against the production endpoint; `LES_IODA_ENDPOINT` overrides
    /// it (tests point this at a local server). Fails only if the HTTP
    /// client cannot be constructed or the bundled country data is corrupt.
    pub fn from_env() -> Result<Self, SourceError> {
        let mut src = Self::new()?;
        if let Ok(u) = std::env::var("LES_IODA_ENDPOINT") {
            src.events_url = u;
        }
        Ok(src)
    }

    pub fn new() -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| SourceError::Other(format!("building http client: {e}")))?;
        let countries = CountryIndex::from_geojson_str(NE_COUNTRIES)
            .map_err(|e| SourceError::Other(format!("parsing bundled country data: {e}")))?;
        Ok(Self {
            http,
            events_url: EVENTS_URL.to_owned(),
            countries,
        })
    }

    /// Override the events endpoint (tests point this at a local server).
    pub fn with_endpoint(mut self, url: impl Into<String>) -> Self {
        self.events_url = url.into();
        self
    }
}

impl SignalSource for IodaSource {
    fn id(&self) -> SourceId {
        SourceId::Ioda
    }

    async fn fetch(
        &self,
        window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        let from = window.start.timestamp().to_string();
        let until = window.end.timestamp().to_string();
        let resp = self
            .http
            .get(&self.events_url)
            .query(&[
                ("entityType", "country"),
                ("from", from.as_str()),
                ("until", until.as_str()),
                ("format", "codf"),
            ])
            .send()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok());
            return Err(SourceError::RateLimited { retry_after_secs });
        }
        if !status.is_success() {
            return Err(SourceError::Http(format!("ioda events returned {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| SourceError::Other(format!("ioda response was not JSON: {e}")))?;
        if let Some(err) = body.get("error").and_then(Value::as_str) {
            return Err(SourceError::Other(format!("ioda error: {err}")));
        }
        let data = match body.get("data") {
            Some(Value::Array(items)) => items.clone(),
            None | Some(Value::Null) => Vec::new(),
            Some(_) => return Err(SourceError::Other("ioda `data` was not an array".into())),
        };
        tracing::info!(records = data.len(), "ioda outage events fetched");
        Ok(data.into_iter().map(RawRecord::IodaEventJson).collect())
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::IodaEventJson(v) => crate::normalize_event(v, &self.countries),
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("ioda source received a foreign record: {other:?}"),
            }),
        }
    }
}
