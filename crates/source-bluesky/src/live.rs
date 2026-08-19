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
use tokio::sync::watch;
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
/// Handle onto a shared accumulator: the stream task and the polling caller
/// hold the same `Arc`.
///
/// # Switching it off means switching it off
///
/// The socket used to be started once and detached, so "off" only stopped
/// the draining, not the counting - the firehose kept arriving, the
/// accumulator kept growing, and the only thing between a switched-off
/// source and stored data was a caller remembering to throw each drain
/// away. [`start_stream`](Self::start_stream) and
/// [`stop_stream`](Self::stop_stream) make the socket itself the switch:
/// stopping closes the connection, discards what was counted but never
/// drained, and returns only once the task is actually gone.
pub struct BlueskySource {
    endpoint: Option<String>,
    accumulator: Arc<Mutex<ChatterAccumulator>>,
    /// The running socket task, or `None` when the source is stopped.
    /// Interior mutability because starting and stopping happen through the
    /// same `&self` every other source method takes.
    stream: Mutex<Option<StreamTask>>,
}

/// A running socket task and the switch that stops it.
struct StreamTask {
    handle: tokio::task::JoinHandle<()>,
    /// Watched by both the read loop and the reconnect sleep, so a stop is
    /// noticed at either - a source switched off during a five-minute
    /// backoff must not keep the task alive until that sleep expires.
    stop: watch::Sender<bool>,
}

impl BlueskySource {
    /// Build over the bundled gazetteers with an explicit window. Endpoint
    /// defaults to rotating the public [`JETSTREAM_ENDPOINTS`]; see
    /// [`Self::with_endpoint`] to pin one (tests point this at a local
    /// server).
    pub fn new(window_secs: i64) -> Result<Self, SourceError> {
        let accumulator = ChatterAccumulator::from_bundled(window_secs)
            .map_err(|e| SourceError::Other(format!("building chatter matcher: {e}")))?;
        Ok(Self {
            endpoint: None,
            accumulator: Arc::new(Mutex::new(accumulator)),
            stream: Mutex::new(None),
        })
    }

    /// Pin a single endpoint instead of rotating [`JETSTREAM_ENDPOINTS`].
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Build from `LES_BLUESKY_WINDOW_SECS`/`LES_BLUESKY_ENDPOINT`.
    pub fn from_env() -> Result<Self, SourceError> {
        let window_secs = std::env::var("LES_BLUESKY_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_WINDOW_SECS);
        let mut source = Self::new(window_secs)?;
        if let Ok(endpoint) = std::env::var("LES_BLUESKY_ENDPOINT") {
            source.endpoint = Some(endpoint);
        }
        Ok(source)
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

    /// Start the long-lived stream task, unless one is already running.
    ///
    /// Call before the first [`SignalSource::fetch`] - without it the
    /// accumulator stays empty and every drain returns nothing. The task
    /// reconnects on its own until [`stop_stream`](Self::stop_stream) is
    /// called or the source is dropped.
    ///
    /// Returns whether this call is the one that started it. Idempotent on
    /// purpose: the UI can re-assert "on" without opening a second socket
    /// counting the same firehose into the same accumulator, which would
    /// double every number this source publishes.
    pub fn start_stream(&self) -> bool {
        let mut slot = Self::lock_stream(&self.stream);
        if slot.as_ref().is_some_and(|t| !t.handle.is_finished()) {
            return false;
        }
        let (stop, stop_rx) = watch::channel(false);
        let endpoints = self.endpoints();
        let acc = Arc::clone(&self.accumulator);
        let handle = tokio::spawn(async move { stream_forever(endpoints, acc, stop_rx).await });
        *slot = Some(StreamTask { handle, stop });
        true
    }

    /// Stop the stream task, close its socket, and discard what it counted
    /// but never published.
    ///
    /// Returns whether a task was running. Awaiting the join handle is the
    /// point rather than an afterthought: when this returns the task has
    /// been dropped, so the socket is closed and the server has seen the
    /// connection go - a caller can rely on "stopped" meaning stopped.
    ///
    /// The pending discard is a correctness requirement, not tidiness. A
    /// half-counted window left in the accumulator would be published by the
    /// first drain after the source came back on, presenting posts counted
    /// while it was off as part of a later window.
    pub async fn stop_stream(&self) -> bool {
        let Some(task) = Self::lock_stream(&self.stream).take() else {
            return false;
        };
        let _ = task.stop.send(true);
        let _ = task.handle.await;
        let dropped = Self::lock(&self.accumulator).drain_all().len();
        tracing::info!(dropped, "bluesky stream stopped; pending windows discarded");
        true
    }

    /// Whether a stream task is running right now.
    pub fn is_streaming(&self) -> bool {
        Self::lock_stream(&self.stream)
            .as_ref()
            .is_some_and(|t| !t.handle.is_finished())
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

    /// Lock the task slot, surviving a poisoned mutex for the same reason.
    fn lock_stream(
        slot: &Mutex<Option<StreamTask>>,
    ) -> std::sync::MutexGuard<'_, Option<StreamTask>> {
        slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for BlueskySource {
    /// Dropping the source stops the socket too. `Drop` cannot await, so the
    /// task is aborted rather than joined; the signal is still sent first so
    /// a task sitting between await points can end on its own terms.
    fn drop(&mut self) {
        let slot = self
            .stream
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(task) = slot.take() {
            let _ = task.stop.send(true);
            task.handle.abort();
        }
    }
}

/// Why a connection ended.
enum Closed {
    /// The socket ended by itself, after this many messages; reconnect.
    Ended(u64),
    /// [`BlueskySource::stop_stream`] asked for it; do not reconnect.
    Stopped,
}

/// Connect, read until the socket dies, back off, reconnect — forever.
///
/// **No cursor on reconnect.** Jetstream can replay from a `time_us` cursor,
/// but replayed posts would be counted a second time and inflate the very
/// aggregates this source publishes. A gap while disconnected undercounts
/// instead, which is the honest direction to fail in.
async fn stream_forever(
    endpoints: Vec<String>,
    acc: Arc<Mutex<ChatterAccumulator>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut attempt: u32 = 0;
    let mut next_endpoint = 0usize;
    while !*stop.borrow() {
        let endpoint = &endpoints[next_endpoint % endpoints.len()];
        next_endpoint = next_endpoint.wrapping_add(1);
        match run_once(endpoint, &acc, &mut stop).await {
            Ok(Closed::Stopped) => return,
            Ok(Closed::Ended(messages)) => {
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
        // The wait is cancellable. At the far end of the backoff this is a
        // five-minute sleep, and a source switched off in Settings must not
        // stay alive that long after the fact.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
            _ = stop.changed() => return,
        }
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

/// One connection's lifetime.
async fn run_once(
    endpoint: &str,
    acc: &Mutex<ChatterAccumulator>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Closed, SourceError> {
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
    loop {
        // Both arms are cancel-safe: `changed()` records nothing until it
        // resolves and `next()` only polls the socket, so losing the race
        // costs nothing - no frame is consumed and then dropped.
        let frame = tokio::select! {
            biased;
            // Returning here drops `socket`, which closes the connection.
            // No close frame is sent: writing one needs `futures_util`'s
            // sink half, which this crate does not otherwise compile, and a
            // read-only subscription dropping its socket is a case the
            // server already has to handle - it is what a lost network does.
            _ = stop.changed() => return Ok(Closed::Stopped),
            frame = socket.next() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
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
    Ok(Closed::Ended(messages))
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
