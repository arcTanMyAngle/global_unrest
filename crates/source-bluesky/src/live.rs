//! The network path (feature `live`): one long-lived WebSocket, counted in
//! memory, drained on the caller's poll cadence.
//!
//! The shared `sched` limiter/backoff is built for poll-based HTTP sources
//! and does not apply to a socket, so reconnection has its own small
//! exponential backoff here.

use std::sync::{Arc, Mutex};

use chatter::{ChatterAccumulator, DEFAULT_WINDOW_SECS};
use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use crate::{JETSTREAM_ENDPOINTS, MessageOutcome, observe_message, subscribe_url};

/// Reconnect backoff bounds for a dropped socket.
const RECONNECT_MIN_SECS: u64 = 2;
const RECONNECT_MAX_SECS: u64 = 300;

/// How often the stream task logs aggregate progress. Counts only — the log
/// never names a post, an author, or a place-level count.
const STATS_EVERY_MSGS: u64 = 50_000;

/// Live Bluesky Jetstream adapter.
///
/// Cloneable handle onto a shared accumulator: the stream task and the
/// polling caller hold the same `Arc`.
pub struct BlueskySource {
    endpoint: Option<String>,
    accumulator: Arc<Mutex<ChatterAccumulator>>,
}

impl BlueskySource {
    /// Build over the bundled gazetteers. `LES_BLUESKY_ENDPOINT` pins a
    /// single endpoint (tests point this at a local server); unset means the
    /// public [`JETSTREAM_ENDPOINTS`] are rotated.
    pub fn from_env() -> Result<Self, SourceError> {
        let window_secs = std::env::var("LES_BLUESKY_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_WINDOW_SECS);
        let accumulator = ChatterAccumulator::from_bundled(window_secs)
            .map_err(|e| SourceError::Other(format!("building chatter matcher: {e}")))?;
        Ok(Self {
            endpoint: std::env::var("LES_BLUESKY_ENDPOINT").ok(),
            accumulator: Arc::new(Mutex::new(accumulator)),
        })
    }

    /// Endpoints this source will try, in order.
    fn endpoints(&self) -> Vec<String> {
        match &self.endpoint {
            Some(pinned) => vec![pinned.clone()],
            None => JETSTREAM_ENDPOINTS
                .iter()
                .map(|e| (*e).to_owned())
                .collect(),
        }
    }

    /// Spawn the long-lived stream task.
    ///
    /// Call once, before the first [`SignalSource::fetch`] — without it the
    /// accumulator stays empty and every drain returns nothing. The task
    /// reconnects on its own and only ends when the returned handle is
    /// dropped or aborted.
    pub fn spawn_stream(&self) -> tokio::task::JoinHandle<()> {
        let endpoints = self.endpoints();
        let acc = Arc::clone(&self.accumulator);
        tokio::spawn(async move { stream_forever(endpoints, acc).await })
    }

    /// Posts scanned and posts matched since the stream started.
    ///
    /// The denominator matters: "9 matched" means nothing without the
    /// thousands of posts scanned to find them, so the UI shows both.
    pub fn stats(&self) -> (u64, u64) {
        let guard = Self::lock(&self.accumulator);
        (guard.scanned(), guard.matched())
    }

    /// Lock the accumulator, surviving a poisoned mutex.
    ///
    /// A panic in the stream task must not silently stop ingestion; the
    /// counters are plain integers, so the recovered state is still usable.
    fn lock(acc: &Mutex<ChatterAccumulator>) -> std::sync::MutexGuard<'_, ChatterAccumulator> {
        acc.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Connect, read until the socket dies, back off, reconnect — forever.
///
/// **No cursor on reconnect.** Jetstream can replay from a `time_us` cursor,
/// but replayed posts would be counted a second time and inflate the very
/// aggregates this source publishes. A gap while disconnected undercounts
/// instead, which is the honest direction to fail in.
async fn stream_forever(endpoints: Vec<String>, acc: Arc<Mutex<ChatterAccumulator>>) {
    let mut attempt: u32 = 0;
    let mut next_endpoint = 0usize;
    loop {
        let endpoint = &endpoints[next_endpoint % endpoints.len()];
        next_endpoint = next_endpoint.wrapping_add(1);
        match run_once(endpoint, &acc).await {
            Ok(messages) => {
                tracing::info!(endpoint, messages, "bluesky stream closed; reconnecting");
                attempt = 0;
            }
            Err(e) => {
                tracing::warn!(endpoint, error = %e, attempt, "bluesky stream failed");
                attempt = attempt.saturating_add(1);
            }
        }
        let delay = RECONNECT_MIN_SECS
            .saturating_mul(1u64 << attempt.min(7))
            .min(RECONNECT_MAX_SECS);
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
    }
}

/// Install the rustls crypto provider this process will use for the socket.
///
/// `tokio-tungstenite`'s rustls feature pulls rustls without selecting a
/// provider, and rustls 0.23 panics on the first TLS handshake if it cannot
/// infer one. `ring` is chosen to match the provider `reqwest` already puts
/// in this workspace's tree — one provider per process. Installing is
/// idempotent here: an `Err` means something already installed one, which is
/// exactly the desired end state.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// One connection's lifetime. Returns the number of messages read.
async fn run_once(endpoint: &str, acc: &Mutex<ChatterAccumulator>) -> Result<u64, SourceError> {
    install_crypto_provider();
    let url = subscribe_url(endpoint);
    // The stream is not split: `WebSocketStream` answers protocol pings
    // itself while it is being polled, and a split read half would leave
    // pongs unflushed on a connection that only ever reads.
    let (mut socket, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| SourceError::Http(format!("jetstream connect: {e}")))?;
    tracing::info!(endpoint, "bluesky jetstream connected");

    let mut messages: u64 = 0;
    let mut malformed: u64 = 0;
    while let Some(frame) = socket.next().await {
        let frame = frame.map_err(|e| SourceError::Http(format!("jetstream read: {e}")))?;
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            // Binary frames appear only with zstd compression, which this
            // subscription does not request; ping/pong are handled upstream.
            _ => continue,
        };
        messages += 1;
        // The guard is held only across the parse+count, never across an
        // await, so the polling side is never blocked for long.
        let outcome = {
            let mut guard = BlueskySource::lock(acc);
            observe_message(text.as_str(), &mut guard)
        };
        if outcome == MessageOutcome::Malformed {
            malformed += 1;
        }
        if messages.is_multiple_of(STATS_EVERY_MSGS) {
            let guard = BlueskySource::lock(acc);
            tracing::info!(
                messages,
                malformed,
                scanned = guard.scanned(),
                matched = guard.matched(),
                pending = guard.pending(),
                "bluesky stream progress"
            );
        }
    }
    Ok(messages)
}

impl SignalSource for BlueskySource {
    fn id(&self) -> SourceId {
        SourceId::Bluesky
    }

    /// Drain whatever the stream task has counted since the last call.
    ///
    /// Touches no network: the socket is already running. `window` is
    /// ignored because a stream has no addressable past — the accumulator
    /// holds exactly what arrived since the previous drain.
    async fn fetch(
        &self,
        _window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        // Completed windows only: the window still being counted stays
        // pending, so a drain can never publish a half-counted window whose
        // remainder would then be lost to dedup-by-id.
        let rollups = {
            let mut guard = Self::lock(&self.accumulator);
            guard.drain_completed(chrono::Utc::now())
        };
        tracing::info!(rollups = rollups.len(), "bluesky chatter rollups drained");
        Ok(rollups.into_iter().map(RawRecord::ChatterRollup).collect())
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::ChatterRollup(rollup) => {
                chatter::normalize_rollup(rollup, SourceId::Bluesky)
            }
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("bluesky source received a foreign record: {other:?}"),
            }),
        }
    }
}
