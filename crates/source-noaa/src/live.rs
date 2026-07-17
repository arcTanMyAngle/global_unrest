//! The network path (feature `live`): one GET of the active-alerts feed.
//!
//! api.weather.gov policy: keyless, but send a descriptive `User-Agent` with
//! contact context and don't hammer — the callers poll on a fixed multi-minute
//! cadence through the shared limiter/backoff scheduler, and 429s map to
//! [`SourceError::RateLimited`].

use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use serde_json::Value;

use crate::ALERTS_URL;

/// Live NOAA/NWS adapter over the active-alerts GeoJSON endpoint.
pub struct NoaaSource {
    http: reqwest::Client,
    alerts_url: String,
}

impl NoaaSource {
    /// Build against the production endpoint; `LES_NOAA_ENDPOINT` overrides it
    /// (tests point this at a local server). Fails only if the HTTP client
    /// cannot be constructed.
    pub fn from_env() -> Result<Self, SourceError> {
        let mut src = Self::new()?;
        if let Ok(u) = std::env::var("LES_NOAA_ENDPOINT") {
            src.alerts_url = u;
        }
        Ok(src)
    }

    pub fn new() -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            // api.weather.gov asks for an identifying UA with contact context.
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| SourceError::Other(format!("building http client: {e}")))?;
        Ok(Self {
            http,
            alerts_url: ALERTS_URL.to_owned(),
        })
    }

    /// Override the alerts endpoint (tests point this at a local server).
    pub fn with_endpoint(mut self, url: impl Into<String>) -> Self {
        self.alerts_url = url.into();
        self
    }
}

impl SignalSource for NoaaSource {
    fn id(&self) -> SourceId {
        SourceId::Noaa
    }

    /// Fetch the current actual alerts. The feed is a *now* snapshot — the
    /// window is not mapped onto it; retention and dedup-by-id handle overlap
    /// across polls.
    async fn fetch(
        &self,
        _window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        let resp = self
            .http
            .get(&self.alerts_url)
            .query(&[("status", "actual"), ("message_type", "alert,update")])
            .header(reqwest::header::ACCEPT, "application/geo+json")
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
            return Err(SourceError::Http(format!("noaa alerts returned {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| SourceError::Other(format!("noaa response was not JSON: {e}")))?;
        let features = match body.get("features") {
            Some(Value::Array(items)) => items.clone(),
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                return Err(SourceError::Other(
                    "noaa `features` was not an array".into(),
                ));
            }
        };
        tracing::info!(records = features.len(), "noaa alerts fetched");
        Ok(features.into_iter().map(RawRecord::NoaaAlertJson).collect())
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::NoaaAlertJson(v) => crate::normalize_alert(v),
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("noaa source received a foreign record: {other:?}"),
            }),
        }
    }
}
