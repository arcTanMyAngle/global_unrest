//! The network half (feature `live`): one GET per provider, per user action.
//!
//! **There is no cadence here.** Unlike every `source-*` crate, nothing polls:
//! a request happens only when a person presses the search button for a named
//! place. That is the whole privacy and bandwidth argument of this crate — see
//! the module docs in `lib.rs`.
//!
//! Telegram is not a leg of this type. Its search needs the authenticated
//! MTProto session that `source-telegram` already owns, so that leg lives
//! there and returns the same [`MediaHit`] type; making this crate depend on
//! `source-telegram` would cycle the graph.

use core_types::SourceError;

use crate::{MediaHit, MediaQuery, bluesky, gdelt, merge};

/// On-demand media lookup across the keyless public APIs.
pub struct MediaSearch {
    http: reqwest::Client,
    gdelt_endpoint: String,
    bluesky_endpoint: String,
}

impl MediaSearch {
    pub fn new() -> Result<Self, SourceError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| SourceError::Other(format!("building http client: {e}")))?;
        Ok(Self {
            http,
            gdelt_endpoint: gdelt::DOC_ENDPOINT.to_owned(),
            bluesky_endpoint: bluesky::SEARCH_ENDPOINT.to_owned(),
        })
    }

    /// Point the GDELT leg at another host (tests use a local server).
    pub fn with_gdelt_endpoint(mut self, url: impl Into<String>) -> Self {
        self.gdelt_endpoint = url.into();
        self
    }

    /// Point the Bluesky leg at another host (tests use a local server).
    pub fn with_bluesky_endpoint(mut self, url: impl Into<String>) -> Self {
        self.bluesky_endpoint = url.into();
        self
    }

    /// News video for the query's place and window.
    pub async fn gdelt(&self, query: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
        let url = gdelt::request_url(
            &self.gdelt_endpoint,
            &query.place,
            &query.topic,
            query.start,
            query.end,
            query.limit,
        )?;
        gdelt::hits(&self.get(url.as_str()).await?)
    }

    /// Public Bluesky posts carrying video, for the query's place and window.
    pub async fn bluesky(&self, query: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
        let url = bluesky::request_url(
            &self.bluesky_endpoint,
            &query.place,
            &query.topic,
            query.start,
            query.end,
            query.limit,
        )?;
        bluesky::hits(&self.get(url.as_str()).await?)
    }

    /// Run both keyless legs and merge them.
    ///
    /// A failing provider does not fail the search: its error is returned
    /// alongside whatever the others found, so one rate-limited API cannot
    /// make the panel look empty. GDELT is asked first so a clip reached
    /// through both a news article and a post is attributed to the article.
    pub async fn search(&self, query: &MediaQuery) -> (Vec<MediaHit>, Vec<String>) {
        if !query.is_valid() {
            return (
                Vec::new(),
                vec!["a media search needs a place and a time window".to_string()],
            );
        }
        let mut hits = Vec::new();
        let mut problems = Vec::new();
        for (label, result) in [
            ("news", self.gdelt(query).await),
            ("bluesky", self.bluesky(query).await),
        ] {
            match result {
                Ok(found) => hits.extend(found),
                Err(e) => problems.push(format!("{label}: {e}")),
            }
        }
        (merge(hits), problems)
    }

    /// GET a URL as text.
    ///
    /// `reqwest` is pinned workspace-wide without its `json` feature, so
    /// `Response::json()` does not exist here — bodies are read as text and
    /// handed to each provider's own parser, which is also what lets those
    /// parsers name a plain-text rejection instead of reporting a JSON error.
    async fn get(&self, url: &str) -> Result<String, SourceError> {
        // One retry, and only for a failure to connect at all. GDELT throttles
        // by dropping SYNs rather than answering 429 (live-verified
        // 2026-08-13: `curl` got a 429 body while a request seconds later timed
        // out during connect), so a single search can lose to a burst that has
        // already passed. A retry after an HTTP response would be a second
        // billable/rate-limited call for no new information, hence the
        // narrow condition.
        let mut response = self.http.get(url).send().await;
        if response.as_ref().is_err_and(reqwest::Error::is_connect) {
            tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            response = self.http.get(url).send().await;
        }
        let response = response.map_err(|e| SourceError::Http(describe(&e)))?;
        let status = response.status();
        if status.as_u16() == 429 {
            let retry_after_secs = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            return Err(SourceError::RateLimited { retry_after_secs });
        }
        let body = response
            .text()
            .await
            .map_err(|e| SourceError::Http(format!("reading body: {e}")))?;
        if !status.is_success() {
            // Bodies here are API error payloads, not article text; truncated
            // so a stray HTML page cannot flood the UI or a log line.
            let excerpt: String = body.chars().take(200).collect();
            return Err(SourceError::Http(format!("HTTP {status}: {excerpt}")));
        }
        Ok(body)
    }
}

/// How long to wait before the single connect retry in [`MediaSearch::get`].
const CONNECT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// A short, human sentence for a transport failure.
///
/// This string is shown in the panel, so it must not be `reqwest`'s own
/// `Display`: that renders as "error sending request for url (…)" with the
/// whole query — several hundred characters of percent-encoded domain filter —
/// inlined, and it reads identically for a DNS failure, a TLS rejection, a
/// timeout, and a server dropping the connection. The classifier answers the
/// question a reader actually has, and the innermost cause keeps the detail.
fn describe(error: &reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "the provider did not answer in time"
    } else if error.is_connect() {
        "could not reach the provider (it may be rate-limiting this address)"
    } else if error.is_decode() {
        "the provider's response could not be read"
    } else {
        "the request failed"
    };
    // The innermost cause is the specific one; the outer layers just repeat
    // "error sending request" with the URL attached.
    let mut deepest: Option<String> = None;
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        deepest = Some(cause.to_string());
        source = cause.source();
    }
    match deepest {
        Some(cause) => format!("{kind} ({cause})"),
        None => kind.to_string(),
    }
}
