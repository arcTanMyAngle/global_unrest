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
    API_BASE, API_KEY_ENV, DayDigest, DigestError, DigestFacts, MODEL, api_url, parse_response,
    request_body,
};

/// Overrides [`API_BASE`]. Used by the mock-server tests; there is no reason
/// to set it in normal operation. It is a *base* — the model id and method are
/// appended by [`api_url`], so the mock exercises the same path the real API
/// sees.
pub const ENDPOINT_ENV: &str = "LES_GEMINI_ENDPOINT";

pub struct GeminiDigester {
    http: reqwest::Client,
    url: String,
    api_key: String,
}

impl GeminiDigester {
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
        let base = std::env::var(ENDPOINT_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| API_BASE.to_owned());
        Ok(Some(Self::new(key, base)?))
    }

    pub fn new(api_key: String, base: String) -> Result<Self, DigestError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(Duration::from_secs(10))
            // Generous: thinking on a large-context model can take a while,
            // and this runs once per UTC day, not per frame.
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| DigestError::Http(e.to_string()))?;
        Ok(Self {
            http,
            url: api_url(&base),
            api_key,
        })
    }

    /// Point at a local server. Tests only. Takes the API *base*, same as
    /// [`Self::new`].
    pub fn with_endpoint(mut self, base: impl AsRef<str>) -> Self {
        self.url = api_url(base.as_ref());
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
            .post(&self.url)
            .header("content-type", "application/json")
            // API key in a header, never the `?key=` query parameter this API
            // also accepts: query strings are the part of a URL that ends up
            // in logs, proxies, and error messages.
            .header("x-goog-api-key", &self.api_key)
            .body(body)
            .send()
            .await
            .map_err(|e| DigestError::Http(e.to_string()))?;

        let status = resp.status();
        if status.as_u16() == 429 {
            let header_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            // This API usually omits `Retry-After` and puts the delay in a
            // `RetryInfo` detail instead, as a duration string ("41s").
            let text = resp.text().await.unwrap_or_default();
            let retry_after_secs = header_secs.or_else(|| {
                serde_json::from_str::<Value>(&text)
                    .ok()
                    .as_ref()
                    .and_then(error_details)
                    .and_then(|details| {
                        details
                            .iter()
                            .filter_map(|d| d.get("retryDelay").and_then(Value::as_str))
                            .find_map(parse_retry_delay)
                    })
            });
            return Err(DigestError::RateLimited { retry_after_secs });
        }
        if !status.is_success() {
            // The response body can echo request content; surface only the
            // API's own `error.message`, and never the key we just sent.
            let text = resp.text().await.unwrap_or_default();
            let value = serde_json::from_str::<Value>(&text).ok();
            let detail = value
                .as_ref()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "no error message returned".to_owned());
            // A bad key on this API is an ordinary 400 `INVALID_ARGUMENT`, not
            // a 401/403 — status alone cannot tell it apart from a malformed
            // request, so the credential hint has to come from the structured
            // `reason`. 401/403 stay mapped as well; they are what a disabled
            // or unauthorized project returns.
            let bad_credentials = matches!(status.as_u16(), 401 | 403)
                || value
                    .as_ref()
                    .and_then(error_details)
                    .is_some_and(|details| {
                        details.iter().any(|d| {
                            matches!(
                                d.get("reason").and_then(Value::as_str),
                                Some("API_KEY_INVALID" | "API_KEY_SERVICE_BLOCKED")
                            )
                        })
                    });
            return Err(if bad_credentials {
                DigestError::Api(format!(
                    "{status}: {detail} — check the {API_KEY_ENV} environment variable"
                ))
            } else {
                DigestError::Api(format!("{status}: {detail}"))
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

/// `error.details[]` — the google.rpc status details array, where this API
/// puts the machine-readable half of a failure.
fn error_details(body: &Value) -> Option<&Vec<Value>> {
    body.get("error")
        .and_then(|e| e.get("details"))
        .and_then(Value::as_array)
}

/// Whole seconds out of a protobuf duration string (`"41s"`, `"7.5s"`).
///
/// Rounds down and refuses anything unexpected: this only feeds a "try again
/// later" hint, so a wrong number is worse than no number.
fn parse_retry_delay(raw: &str) -> Option<u64> {
    let secs = raw.trim().strip_suffix('s')?;
    secs.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_parses_the_protobuf_duration_shape() {
        assert_eq!(parse_retry_delay("41s"), Some(41));
        assert_eq!(parse_retry_delay(" 7.5s "), Some(7));
        assert_eq!(parse_retry_delay("41"), None);
        assert_eq!(parse_retry_delay("soon"), None);
        assert_eq!(parse_retry_delay("-1s"), None);
    }
}
