//! The only networked code in this crate (feature `live`).
//!
//! Everything about *what* is sent lives in `lib.rs` and is offline-testable;
//! this module is the transport plus error mapping. It follows the same shape
//! as the other keyed live adapters in the workspace (`source-acled`): a
//! struct holding a `reqwest::Client` and an overridable endpoint,
//! `from_env()` for the real thing, `with_endpoint()` for the mock-server
//! tests.

use std::time::Duration;

use serde_json::Value;

use crate::{
    ANTHROPIC_VERSION, API_KEY_ENV, API_URL, DayDigest, DigestError, DigestFacts, MODEL,
    parse_response, request_body,
};

/// Overrides [`API_URL`]. Used by the mock-server tests; there is no reason to
/// set it in normal operation.
pub const ENDPOINT_ENV: &str = "LES_ANTHROPIC_ENDPOINT";

pub struct AnthropicDigester {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl AnthropicDigester {
    /// Reads the API key from the environment. Returns `Ok(None)` when the key
    /// is absent so the caller can treat the whole feature as simply
    /// unconfigured — the same contract the other credential-gated sources
    /// use, rather than failing startup.
    pub fn from_env() -> Result<Option<Self>, DigestError> {
        let Ok(key) = std::env::var(API_KEY_ENV) else {
            return Ok(None);
        };
        let key = key.trim().to_owned();
        if key.is_empty() {
            return Ok(None);
        }
        let endpoint = std::env::var(ENDPOINT_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| API_URL.to_owned());
        Ok(Some(Self::new(key, endpoint)?))
    }

    pub fn new(api_key: String, endpoint: String) -> Result<Self, DigestError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(Duration::from_secs(10))
            // Generous: adaptive thinking on a 1M-context model can take a
            // while, and this runs once per UTC day, not per frame.
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| DigestError::Http(e.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            api_key,
        })
    }

    /// Point at a local server. Tests only.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Generate one day's digest. `now_epoch_s` is passed in rather than read
    /// from the clock so the stored `generated_at` matches whatever the caller
    /// records elsewhere in the same cycle.
    pub async fn generate(
        &self,
        facts: &DigestFacts,
        now_epoch_s: i64,
    ) -> Result<DayDigest, DigestError> {
        if facts.is_empty() {
            return Err(DigestError::NoData(facts.day_utc.0));
        }
        let body = serde_json::to_vec(&request_body(facts))
            .map_err(|e| DigestError::Parse(e.to_string()))?;
        // Serialized by hand rather than with `RequestBuilder::json`: the
        // workspace pins reqwest with `default-features = false` and only the
        // rustls TLS feature, so the `json` helper is not compiled in.
        let resp = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            // API key in a header, never a query string: query strings are
            // the part of a URL that ends up in logs and error messages.
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .body(body)
            .send()
            .await
            .map_err(|e| DigestError::Http(e.to_string()))?;

        let status = resp.status();
        if status.as_u16() == 429 {
            let retry_after_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            return Err(DigestError::RateLimited { retry_after_secs });
        }
        if !status.is_success() {
            // The response body can echo request content; surface only the
            // API's own `error.message`, and never the key we just sent.
            let text = resp.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "no error message returned".to_owned());
            return Err(match status.as_u16() {
                401 | 403 => DigestError::Api(format!(
                    "{status}: {detail} — check the {API_KEY_ENV} environment variable"
                )),
                _ => DigestError::Api(format!("{status}: {detail}")),
            });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| DigestError::Http(e.to_string()))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| DigestError::Parse(e.to_string()))?;
        let sections = parse_response(&value)?;
        tracing::info!(
            day = %facts.day_utc,
            attention_records = facts.attention.records,
            event_records = facts.events.records,
            "generated daily digest"
        );
        Ok(DayDigest {
            day_utc: facts.day_utc,
            model: MODEL.to_owned(),
            generated_at_epoch_s: now_epoch_s,
            media_attention: sections.media_attention,
            event_data: sections.event_data,
            attention_records: facts.attention.records,
            event_records: facts.events.records,
        })
    }
}
