//! Ingest worker: a long-lived thread with a current-thread tokio runtime
//! that (1) loads the offline fixtures once at startup and (2) — when online
//! mode is enabled — polls the live sources on their own cadences (GDELT
//! every feed interval; ACLED, when built with `acled-live` and credentialed,
//! twice a day), normalizes, and streams incremental batches back to the UI.
//!
//! Fixture mode is the permanent offline base and always loads first; going
//! online only *adds* live data on top. The UI thread owns storage, so the
//! worker never touches the database: it hands `(events, failures)` back over a
//! channel and the app ingests them (storage dedups by event id, so re-fetching
//! overlapping windows never double-counts). Live failures degrade gracefully —
//! the last-known data stays on screen and the worker reports a degraded status
//! and backs off (docs/PLAN.md §12 M3 acceptance).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use core_types::{
    GeoTemporalEvent, IngestFailure, SignalSource, SourceError, SourceFilters, TimeWindow,
};
use source_fixtures::FixtureSource;
use source_gdelt::{GdeltSource, sched};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::time::{Instant, sleep_until};

/// How far back each online DOC poll looks. Overlapping successive windows
/// guarantees no gaps at the 15-minute boundary; storage dedup absorbs the
/// overlap.
const DOC_LOOKBACK_MINS: i64 = 60;

/// ACLED publishes weekly (plus corrections), so its loop polls twice a day —
/// nowhere near the GDELT cadence — and each poll looks back far enough to
/// absorb late additions. Dedup-by-id makes the overlap idempotent (revisions
/// that reuse an id are deliberately not re-applied; see HANDOFF.md).
const ACLED_POLL_SECS: u64 = 12 * 60 * 60;
const ACLED_LOOKBACK_DAYS: i64 = 14;

/// NOAA active alerts are a *now* snapshot of a feed that changes on the
/// minutes scale; poll politely every 10 minutes.
const NOAA_POLL_SECS: u64 = 10 * 60;

/// Feature-gated ACLED handle. The stub keeps the ingest loop cfg-free: with
/// the feature off `make()` is always `None`, so the ACLED select arm is dead
/// code that still typechecks, and `source-acled` is not compiled at all.
#[cfg(feature = "acled-live")]
mod acled {
    pub use source_acled::AcledSource;
    /// Built with the live path; a missing source means missing credentials.
    pub const BUILT: bool = true;
    pub fn make() -> Result<Option<AcledSource>, core_types::SourceError> {
        AcledSource::from_env()
    }
}
#[cfg(not(feature = "acled-live"))]
mod acled {
    use core_types::{
        GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
        SourceId, TimeWindow,
    };

    pub struct AcledSource;
    pub const BUILT: bool = false;
    pub fn make() -> Result<Option<AcledSource>, SourceError> {
        Ok(None)
    }
    impl SignalSource for AcledSource {
        fn id(&self) -> SourceId {
            SourceId::Acled
        }
        async fn fetch(
            &self,
            _: TimeWindow,
            _: &SourceFilters,
        ) -> Result<Vec<RawRecord>, SourceError> {
            unreachable!("built without the acled-live feature")
        }
        fn normalize(&self, _: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
            unreachable!("built without the acled-live feature")
        }
    }
}

/// Feature-gated NOAA handle — same stub pattern as [`acled`]. Keyless, so
/// `make()` with the feature on is effectively always `Some`.
#[cfg(feature = "noaa-live")]
mod noaa {
    pub use source_noaa::NoaaSource;
    pub fn make() -> Result<Option<NoaaSource>, core_types::SourceError> {
        NoaaSource::from_env().map(Some)
    }
}
#[cfg(not(feature = "noaa-live"))]
mod noaa {
    use core_types::{
        GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
        SourceId, TimeWindow,
    };

    pub struct NoaaSource;
    pub fn make() -> Result<Option<NoaaSource>, SourceError> {
        Ok(None)
    }
    impl SignalSource for NoaaSource {
        fn id(&self) -> SourceId {
            SourceId::Noaa
        }
        async fn fetch(
            &self,
            _: TimeWindow,
            _: &SourceFilters,
        ) -> Result<Vec<RawRecord>, SourceError> {
            unreachable!("built without the noaa-live feature")
        }
        fn normalize(&self, _: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
            unreachable!("built without the noaa-live feature")
        }
    }
}

/// Live-source status surfaced in the UI — one per source, keyed by `name`.
#[derive(Debug, Clone)]
pub struct SourceStatus {
    /// Which live source this line describes ("GDELT", "ACLED").
    pub name: &'static str,
    pub online: bool,
    pub last_attempt_epoch_s: Option<i64>,
    pub last_success_epoch_s: Option<i64>,
    pub next_attempt_epoch_s: Option<i64>,
    /// Human-readable summary of the last cycle (counts, or the error).
    pub detail: String,
    /// The last attempt failed; the UI shows cached data with a degraded badge.
    pub degraded: bool,
}

impl SourceStatus {
    fn offline(name: &'static str) -> Self {
        Self {
            name,
            online: false,
            last_attempt_epoch_s: None,
            last_success_epoch_s: None,
            next_attempt_epoch_s: None,
            detail: "offline — fixture data only".into(),
            degraded: false,
        }
    }
}

pub enum IngestMsg {
    /// One normalized batch to ingest (`origin` names the source for the UI).
    Loaded {
        events: Vec<GeoTemporalEvent>,
        failures: Vec<IngestFailure>,
        origin: &'static str,
    },
    /// Updated live-source status.
    Status(SourceStatus),
    /// Fatal: the offline fixture base could not be loaded.
    Failed(String),
}

/// Commands from the UI to the worker.
enum Ctl {
    SetOnline(bool),
    FetchNow,
}

/// UI-side handle to the worker. Dropping it stops the worker.
pub struct IngestHandle {
    ctl: tokio_mpsc::UnboundedSender<Ctl>,
}

impl IngestHandle {
    pub fn set_online(&self, on: bool) {
        let _ = self.ctl.send(Ctl::SetOnline(on));
    }

    pub fn fetch_now(&self) {
        let _ = self.ctl.send(Ctl::FetchNow);
    }
}

/// Locate the fixtures directory: `LES_FIXTURES_DIR` env override, then
/// `fixtures/` in the working directory or any ancestor (covers `cargo run`
/// from the workspace root and from crate directories).
pub fn find_fixtures_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("LES_FIXTURES_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|a| a.join("fixtures"))
        .find(|p| p.is_dir())
}

/// Spawn the ingest worker. Results arrive on the returned channel; `wake`
/// (a repaint request) fires after every message so the UI polls promptly.
/// The returned handle controls online mode and stops the worker when dropped.
pub fn spawn(
    fixtures_dir: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> (mpsc::Receiver<IngestMsg>, IngestHandle) {
    let (tx_res, rx_res) = mpsc::channel();
    let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("ingest".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx_res.send(IngestMsg::Failed(format!("tokio runtime: {e}")));
                    wake();
                    return;
                }
            };
            runtime.block_on(worker(fixtures_dir, tx_res, rx_ctl, wake));
        })
        .expect("spawn ingest thread");
    (rx_res, IngestHandle { ctl: tx_ctl })
}

async fn worker(
    fixtures_dir: PathBuf,
    tx: mpsc::Sender<IngestMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
) {
    // 1. Offline base: load fixtures once. A failure here is fatal (no data).
    match load_fixtures(&fixtures_dir).await {
        Ok((events, failures)) => {
            let _ = tx.send(IngestMsg::Loaded {
                events,
                failures,
                origin: "fixtures",
            });
            wake();
        }
        Err(msg) => {
            let _ = tx.send(IngestMsg::Failed(msg));
            wake();
            return;
        }
    }

    // 2. Live GDELT loop, driven by control messages and the feed cadence.
    // Endpoint env overrides let tests/mocks point the loop at a local server
    // (and reproduce the network-down path deterministically).
    let gdelt = GdeltSource::new().ok().map(|mut g| {
        if let Ok(doc) = std::env::var("LES_GDELT_DOC_ENDPOINT") {
            g = g.with_endpoint(doc);
        }
        if let Ok(events) = std::env::var("LES_GDELT_EVENTS_URL") {
            g = g.with_events_url(events);
        }
        g
    });
    let limiter = sched::request_limiter();
    let mut backoff = sched::Backoff::default();
    let mut online = false;
    let mut next_at = Instant::now();
    let mut status = SourceStatus::offline("GDELT");

    // ACLED (feature-gated): its own source, limiter, backoff, and much
    // slower cadence. `None` = feature off or no credentials.
    let acled_src = match acled::make() {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(error = %e, "acled source init failed; continuing without it");
            None
        }
    };
    let acled_limiter = sched::request_limiter();
    // First retry after a minute, capped at an hour — tuned to a twice-daily
    // poll, not GDELT's 15-minute feed.
    let mut acled_backoff = sched::Backoff::new(
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(3600),
    );
    let mut acled_next = Instant::now();
    let mut acled_status = SourceStatus::offline("ACLED");
    if acled::BUILT && acled_src.is_none() {
        // Built for ACLED but not credentialed: say why the line stays off.
        acled_status.detail = "off — set ACLED_EMAIL / ACLED_PASSWORD".into();
        let _ = tx.send(IngestMsg::Status(acled_status.clone()));
        wake();
    }

    // NOAA (feature-gated, keyless): a fast *now*-snapshot feed.
    let noaa_src = match noaa::make() {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(error = %e, "noaa source init failed; continuing without it");
            None
        }
    };
    let noaa_limiter = sched::request_limiter();
    let mut noaa_backoff = sched::Backoff::default();
    let mut noaa_next = Instant::now();
    let mut noaa_status = SourceStatus::offline("NOAA");

    loop {
        tokio::select! {
            ctl = rx_ctl.recv() => match ctl {
                None => break, // handle dropped → shut down
                Some(Ctl::SetOnline(on)) => {
                    online = on;
                    status.online = on;
                    acled_status.online = on && acled_src.is_some();
                    noaa_status.online = on && noaa_src.is_some();
                    if on {
                        status.detail = "online — fetching…".into();
                        next_at = Instant::now(); // fetch promptly
                        if acled_src.is_some() {
                            acled_status.detail = "online — fetching…".into();
                            acled_next = Instant::now();
                        }
                        if noaa_src.is_some() {
                            noaa_status.detail = "online — fetching…".into();
                            noaa_next = Instant::now();
                        }
                    } else {
                        backoff.reset();
                        status.degraded = false;
                        status.detail = "offline — fixture data only".into();
                        status.next_attempt_epoch_s = None;
                        for (b, s) in [
                            (&mut acled_backoff, &mut acled_status),
                            (&mut noaa_backoff, &mut noaa_status),
                        ] {
                            b.reset();
                            s.degraded = false;
                            s.detail = "offline — fixture data only".into();
                            s.next_attempt_epoch_s = None;
                        }
                    }
                    let _ = tx.send(IngestMsg::Status(status.clone()));
                    if acled::BUILT {
                        let _ = tx.send(IngestMsg::Status(acled_status.clone()));
                    }
                    if noaa_src.is_some() {
                        let _ = tx.send(IngestMsg::Status(noaa_status.clone()));
                    }
                    wake();
                }
                Some(Ctl::FetchNow) => {
                    if online {
                        next_at = Instant::now();
                        if acled_src.is_some() {
                            acled_next = Instant::now();
                        }
                        if noaa_src.is_some() {
                            noaa_next = Instant::now();
                        }
                    }
                }
            },
            _ = sleep_until(next_at), if online && gdelt.is_some() => {
                let gdelt = gdelt.as_ref().unwrap();
                let delay = fetch_cycle(gdelt, &limiter, &mut backoff, &mut status, &tx, &wake).await;
                next_at = Instant::now() + delay;
            }
            _ = sleep_until(acled_next), if online && acled_src.is_some() => {
                let acled_src = acled_src.as_ref().unwrap();
                let window_days = ChronoDuration::days(ACLED_LOOKBACK_DAYS);
                let delay = live_cycle(acled_src, "acled", window_days, ACLED_POLL_SECS,
                    &acled_limiter, &mut acled_backoff, &mut acled_status, &tx, &wake).await;
                acled_next = Instant::now() + delay;
            }
            _ = sleep_until(noaa_next), if online && noaa_src.is_some() => {
                let noaa_src = noaa_src.as_ref().unwrap();
                // The alerts feed is a now-snapshot; the window is nominal.
                let window_span = ChronoDuration::hours(1);
                let delay = live_cycle(noaa_src, "noaa", window_span, NOAA_POLL_SECS,
                    &noaa_limiter, &mut noaa_backoff, &mut noaa_status, &tx, &wake).await;
                noaa_next = Instant::now() + delay;
            }
        }
    }
}

/// One poll of a simple single-feed live source (ACLED, NOAA): fetch the
/// lookback window, emit the normalized batch and an updated status, and
/// return the wait before the next attempt (`poll_secs` on success, backoff
/// on failure). GDELT keeps its own bespoke two-feed cycle ([`fetch_cycle`]).
#[allow(clippy::too_many_arguments)] // internal plumbing, mirrors fetch_cycle
async fn live_cycle<S: SignalSource>(
    src: &S,
    origin: &'static str,
    lookback: ChronoDuration,
    poll_secs: u64,
    limiter: &sched::Limiter,
    backoff: &mut sched::Backoff,
    status: &mut SourceStatus,
    tx: &mpsc::Sender<IngestMsg>,
    wake: &impl Fn(),
) -> std::time::Duration {
    limiter.until_ready().await;

    let now = Utc::now();
    status.last_attempt_epoch_s = Some(now.timestamp());
    let window = TimeWindow::new(now - lookback, now);

    let delay = match src.fetch(window, &SourceFilters::default()).await {
        Ok(raws) => {
            let (events, failures) = storage::partition_normalized(src, &raws);
            backoff.reset();
            status.degraded = false;
            status.last_success_epoch_s = Some(now.timestamp());
            status.detail = format!("online · {} records this cycle", events.len());
            tracing::info!(records = events.len(), origin, "live cycle ok");
            if !events.is_empty() || !failures.is_empty() {
                let _ = tx.send(IngestMsg::Loaded {
                    events,
                    failures,
                    origin,
                });
            }
            std::time::Duration::from_secs(poll_secs)
        }
        Err(e) => {
            let d = backoff.after_error(&e, jitter01());
            status.degraded = true;
            status.detail = format!("degraded — showing cached data · {e}");
            tracing::warn!(
                retry_in_s = d.as_secs(),
                attempt = backoff.attempt(),
                origin,
                error = %e,
                "live fetch failed; degraded, showing cached data"
            );
            d
        }
    };

    status.next_attempt_epoch_s =
        Some((Utc::now() + ChronoDuration::from_std(delay).unwrap_or_default()).timestamp());
    let _ = tx.send(IngestMsg::Status(status.clone()));
    wake();
    delay
}

/// Run one live fetch (DOC attention + Events dump), emit any normalized batch
/// and an updated status, and return how long to wait before the next attempt.
async fn fetch_cycle(
    gdelt: &GdeltSource,
    limiter: &sched::Limiter,
    backoff: &mut sched::Backoff,
    status: &mut SourceStatus,
    tx: &mpsc::Sender<IngestMsg>,
    wake: &impl Fn(),
) -> std::time::Duration {
    limiter.until_ready().await;

    let now = Utc::now();
    status.last_attempt_epoch_s = Some(now.timestamp());
    let window = TimeWindow::new(now - ChronoDuration::minutes(DOC_LOOKBACK_MINS), now);
    let filters = SourceFilters::default();

    let mut events = Vec::new();
    let mut failures = Vec::new();
    let mut doc_err = None;
    let mut events_err = None;

    match gdelt.fetch(window, &filters).await {
        Ok(raws) => {
            let (e, f) = storage::partition_normalized(gdelt, &raws);
            events.extend(e);
            failures.extend(f);
        }
        Err(e) => doc_err = Some(e),
    }
    match gdelt.fetch_events().await {
        Ok(raws) => {
            let (e, f) = storage::partition_normalized(gdelt, &raws);
            events.extend(e);
            failures.extend(f);
        }
        Err(e) => events_err = Some(e),
    }

    let both_failed = doc_err.is_some() && events_err.is_some();
    let delay = if both_failed {
        // Prefer a server Retry-After (from a 429) for the backoff base.
        let err = pick_backoff_error(&doc_err, &events_err);
        let d = backoff.after_error(err, jitter01());
        status.degraded = true;
        status.detail = format!(
            "degraded — showing cached data · {}",
            errors_summary(&doc_err, &events_err)
        );
        tracing::warn!(
            retry_in_s = d.as_secs(),
            attempt = backoff.attempt(),
            detail = %status.detail,
            "gdelt fetch failed; degraded, showing cached data"
        );
        d
    } else {
        backoff.reset();
        status.degraded = false;
        status.last_success_epoch_s = Some(now.timestamp());
        let partial = errors_summary(&doc_err, &events_err);
        status.detail = if partial.is_empty() {
            format!("online · {} new records this cycle", events.len())
        } else {
            format!("online · {} records · partial: {partial}", events.len())
        };
        tracing::info!(records = events.len(), detail = %status.detail, "gdelt cycle ok");
        let secs = sched::until_next_slot(
            now.timestamp(),
            sched::FEED_INTERVAL_SECS,
            sched::FEED_LAG_SECS,
        );
        std::time::Duration::from_secs(secs.max(1) as u64)
    };

    if !events.is_empty() || !failures.is_empty() {
        let _ = tx.send(IngestMsg::Loaded {
            events,
            failures,
            origin: "gdelt",
        });
    }
    status.next_attempt_epoch_s =
        Some((Utc::now() + ChronoDuration::from_std(delay).unwrap_or_default()).timestamp());
    let _ = tx.send(IngestMsg::Status(status.clone()));
    wake();
    delay
}

/// Choose which error drives backoff: a `RateLimited` (so its `Retry-After` is
/// honored) wins, otherwise the DOC error.
fn pick_backoff_error<'a>(
    doc_err: &'a Option<SourceError>,
    events_err: &'a Option<SourceError>,
) -> &'a SourceError {
    for e in [doc_err, events_err].into_iter().flatten() {
        if matches!(e, SourceError::RateLimited { .. }) {
            return e;
        }
    }
    doc_err
        .as_ref()
        .or(events_err.as_ref())
        .expect("a failure exists")
}

fn errors_summary(doc_err: &Option<SourceError>, events_err: &Option<SourceError>) -> String {
    let mut parts = Vec::new();
    if let Some(e) = doc_err {
        parts.push(format!("doc: {e}"));
    }
    if let Some(e) = events_err {
        parts.push(format!("events: {e}"));
    }
    parts.join("; ")
}

/// Cheap sub-second jitter in [0, 1) from the wall clock (no `rand` dep needed
/// for politeness jitter).
fn jitter01() -> f64 {
    f64::from(Utc::now().timestamp_subsec_nanos()) / 1e9
}

/// Load and normalize all committed fixtures (the offline base). The fixture
/// span is fixed and synthetic, so fetch everything.
async fn load_fixtures(dir: &Path) -> Result<(Vec<GeoTemporalEvent>, Vec<IngestFailure>), String> {
    let source =
        FixtureSource::from_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    if source.files().is_empty() {
        return Err(format!(
            "no fixture files in {} — run `cargo run -p source-fixtures --bin generate-fixtures`",
            dir.display()
        ));
    }
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap(),
    );
    let raws = source
        .fetch(window, &SourceFilters::default())
        .await
        .map_err(|e| format!("fixture fetch: {e}"))?;
    let (events, failures) = storage::partition_normalized(&source, &raws);
    tracing::info!(
        events = events.len(),
        failures = failures.len(),
        "fixtures normalized"
    );
    Ok((events, failures))
}
