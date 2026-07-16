//! The network path (feature `live`): OAuth password/refresh grants and the
//! paged windowed `read` endpoint.
//!
//! Politeness contract (same discipline as `source-gdelt`): the caller drives
//! requests through a rate limiter + backoff scheduler; this module maps 429s
//! to [`SourceError::RateLimited`] (honoring `Retry-After`) so that scheduler
//! can behave. Credentials and tokens are held in memory only and never
//! logged.

use std::sync::Mutex;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use serde_json::Value;
use url::Url;

use crate::{MAX_PAGES, PAGE_LIMIT, READ_URL, TOKEN_URL};

/// Refresh the access token this long before its stated expiry, so a token
/// never expires mid-page-loop.
const EXPIRY_SLACK_SECS: i64 = 300;

/// A bearer token plus what's needed to renew it.
struct Token {
    access: String,
    refresh: Option<String>,
    expires_at: DateTime<Utc>,
}

/// Live ACLED adapter: OAuth-authenticated, windowed, paged reads.
///
/// Construct via [`AcledSource::from_env`] (binaries) or [`AcledSource::new`]
/// (tests). Endpoints are overridable so integration tests run against a
/// local mock server, like `GdeltSource::with_endpoint`.
pub struct AcledSource {
    http: reqwest::Client,
    token_url: String,
    read_url: String,
    email: String,
    password: String,
    page_limit: u32,
    max_pages: u32,
    // Interior mutability: `SignalSource::fetch` takes `&self`. The ingest
    // loops are single-task, so a plain Mutex (never held across .await) is
    // enough.
    token: Mutex<Option<Token>>,
}

impl AcledSource {
    /// Build from `ACLED_EMAIL` / `ACLED_PASSWORD`; `Ok(None)` when they are
    /// unset (the caller reports ACLED as disabled). `LES_ACLED_TOKEN_URL` /
    /// `LES_ACLED_ENDPOINT` override the production endpoints for tests.
    pub fn from_env() -> Result<Option<Self>, SourceError> {
        let (Ok(email), Ok(password)) = (
            std::env::var("ACLED_EMAIL"),
            std::env::var("ACLED_PASSWORD"),
        ) else {
            return Ok(None);
        };
        if email.trim().is_empty() || password.is_empty() {
            return Ok(None);
        }
        let mut src = Self::new(email, password)?;
        if let Ok(u) = std::env::var("LES_ACLED_TOKEN_URL") {
            src = src.with_token_url(u);
        }
        if let Ok(u) = std::env::var("LES_ACLED_ENDPOINT") {
            src = src.with_read_url(u);
        }
        Ok(Some(src))
    }

    /// Build an adapter against the production endpoints. Fails only if the
    /// HTTP client cannot be constructed.
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("live-earth-signals/", env!("CARGO_PKG_VERSION")))
            // Bound stalls so a dead network degrades promptly instead of hanging.
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| SourceError::Other(format!("building http client: {e}")))?;
        Ok(Self {
            http,
            token_url: TOKEN_URL.to_owned(),
            read_url: READ_URL.to_owned(),
            email: email.into(),
            password: password.into(),
            page_limit: PAGE_LIMIT,
            max_pages: MAX_PAGES,
            token: Mutex::new(None),
        })
    }

    /// Override the OAuth token endpoint (tests point this at a local server).
    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    /// Override the read endpoint (tests point this at a local server).
    pub fn with_read_url(mut self, url: impl Into<String>) -> Self {
        self.read_url = url.into();
        self
    }

    /// Rows per page (clamped to `1..=PAGE_LIMIT`).
    pub fn with_page_limit(mut self, limit: u32) -> Self {
        self.page_limit = limit.clamp(1, PAGE_LIMIT);
        self
    }

    /// Cap pages fetched per window.
    pub fn with_max_pages(mut self, pages: u32) -> Self {
        self.max_pages = pages.max(1);
        self
    }

    /// The read URL for one page of `window` — pure, exposed for tests.
    /// ACLED filters on `event_date` (dates, inclusive) via
    /// `event_date=<start>|<end>` + `event_date_where=BETWEEN`.
    pub fn read_url_for(&self, window: TimeWindow, page: u32) -> Result<Url, SourceError> {
        let mut url = Url::parse(&self.read_url).map_err(|e| {
            SourceError::Other(format!("bad ACLED endpoint `{}`: {e}", self.read_url))
        })?;
        // The window is half-open; event_date is date-granular and BETWEEN is
        // inclusive, so clamp the end date to the last date the window covers.
        let end = (window.end - ChronoDuration::seconds(1)).max(window.start);
        url.query_pairs_mut()
            .append_pair(
                "event_date",
                &format!(
                    "{}|{}",
                    window.start.format("%Y-%m-%d"),
                    end.format("%Y-%m-%d")
                ),
            )
            .append_pair("event_date_where", "BETWEEN")
            .append_pair("limit", &self.page_limit.to_string())
            .append_pair("page", &page.to_string());
        Ok(url)
    }

    /// A valid bearer token: cached if fresh, renewed via the refresh grant
    /// when possible, else a full password grant.
    async fn bearer(&self) -> Result<String, SourceError> {
        let (cached, refresh) = {
            let guard = self.token.lock().expect("token lock");
            match guard.as_ref() {
                Some(t)
                    if t.expires_at - Utc::now() > ChronoDuration::seconds(EXPIRY_SLACK_SECS) =>
                {
                    (Some(t.access.clone()), None)
                }
                Some(t) => (None, t.refresh.clone()),
                None => (None, None),
            }
        };
        if let Some(access) = cached {
            return Ok(access);
        }

        if let Some(refresh) = refresh {
            let form = [
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("client_id", "acled"),
            ];
            match self.request_token(&form).await {
                Ok(access) => return Ok(access),
                Err(e) => {
                    tracing::debug!(error = %e, "acled token refresh failed; re-authenticating")
                }
            }
        }

        let form = [
            ("username", self.email.as_str()),
            ("password", self.password.as_str()),
            ("grant_type", "password"),
            ("client_id", "acled"),
            ("scope", "authenticated"),
        ];
        self.request_token(&form).await
    }

    /// POST a grant to the token endpoint; cache and return the access token.
    /// Error text never includes the form (credentials).
    async fn request_token(&self, form: &[(&str, &str)]) -> Result<String, SourceError> {
        let resp = self
            .http
            .post(&self.token_url)
            .form(form)
            .send()
            .await
            .map_err(|e| SourceError::Http(format!("acled token request: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(SourceError::RateLimited {
                retry_after_secs: retry_after(&resp),
            });
        }
        if !status.is_success() {
            return Err(SourceError::Http(format!(
                "acled token endpoint returned {status} — check ACLED_EMAIL/ACLED_PASSWORD"
            )));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| SourceError::Http(format!("acled token response: {e}")))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| SourceError::Other(format!("acled token response was not JSON: {e}")))?;
        let access = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| SourceError::Other("acled token response missing access_token".into()))?
            .to_owned();
        let expires_in = body.get("expires_in").and_then(Value::as_i64).unwrap_or(0);
        let refresh = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned);
        tracing::info!(expires_in, "acled token acquired");
        *self.token.lock().expect("token lock") = Some(Token {
            access: access.clone(),
            refresh,
            expires_at: Utc::now() + ChronoDuration::seconds(expires_in.max(0)),
        });
        Ok(access)
    }

    /// Drop the cached token (called after a 401 so the next call re-auths).
    fn invalidate_token(&self) {
        *self.token.lock().expect("token lock") = None;
    }

    /// GET one page. `Ok(None)` signals a 401 (stale token) — the caller
    /// re-authenticates once and retries.
    async fn get_page(&self, url: &Url, bearer: &str) -> Result<Option<Vec<Value>>, SourceError> {
        let resp = self
            .http
            .get(url.clone())
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(|e| SourceError::Http(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Ok(None);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(SourceError::RateLimited {
                retry_after_secs: retry_after(&resp),
            });
        }
        if !status.is_success() {
            return Err(SourceError::Http(format!("acled read returned {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| SourceError::Http(format!("acled read response: {e}")))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| SourceError::Other(format!("acled read response was not JSON: {e}")))?;
        // Response envelope: {"success": bool, "count": n, "data": [...]}.
        if let Some(false) = body.get("success").and_then(Value::as_bool) {
            let detail = body
                .get("error")
                .map(Value::to_string)
                .unwrap_or_else(|| "unspecified".into());
            return Err(SourceError::Other(format!("acled read failed: {detail}")));
        }
        match body.get("data") {
            Some(Value::Array(items)) => Ok(Some(items.clone())),
            None | Some(Value::Null) => Ok(Some(Vec::new())),
            Some(other) => Err(SourceError::Other(format!(
                "acled `data` was not an array (got {})",
                match other {
                    Value::Object(_) => "object",
                    Value::String(_) => "string",
                    Value::Number(_) => "number",
                    Value::Bool(_) => "bool",
                    _ => "other",
                }
            ))),
        }
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

impl SignalSource for AcledSource {
    fn id(&self) -> SourceId {
        SourceId::Acled
    }

    async fn fetch(
        &self,
        window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        let mut bearer = self.bearer().await?;
        let mut out = Vec::new();
        let mut reauthed = false;
        let mut page = 1;
        while page <= self.max_pages {
            let url = self.read_url_for(window, page)?;
            match self.get_page(&url, &bearer).await? {
                Some(items) => {
                    let full_page = items.len() as u32 >= self.page_limit;
                    out.extend(items.into_iter().map(RawRecord::AcledJson));
                    if !full_page {
                        break;
                    }
                    page += 1;
                }
                // 401: the token went stale server-side; re-auth once.
                None if !reauthed => {
                    self.invalidate_token();
                    bearer = self.bearer().await?;
                    reauthed = true;
                }
                None => {
                    return Err(SourceError::Http(
                        "acled read kept returning 401 after re-authentication".into(),
                    ));
                }
            }
        }
        tracing::info!(
            records = out.len(),
            pages = page.min(self.max_pages),
            "acled fetched"
        );
        Ok(out)
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::AcledJson(v) => crate::normalize_event(v).map(|e| vec![e]),
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("acled source received a foreign record: {other:?}"),
            }),
        }
    }
}
