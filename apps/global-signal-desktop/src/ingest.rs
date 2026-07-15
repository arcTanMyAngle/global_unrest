//! Ingest worker: a long-lived thread with a current-thread tokio runtime
//! that (1) loads the offline fixtures once at startup and (2) — when online
//! mode is enabled — polls GDELT on the feed cadence, normalizes, and streams
//! incremental batches back to the UI.
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

/// Live-source status surfaced in the UI.
#[derive(Debug, Clone)]
pub struct SourceStatus {
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
    fn offline() -> Self {
        Self {
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
    let gdelt = GdeltSource::new().ok();
    let limiter = sched::request_limiter();
    let mut backoff = sched::Backoff::default();
    let mut online = false;
    let mut next_at = Instant::now();
    let mut status = SourceStatus::offline();

    loop {
        tokio::select! {
            ctl = rx_ctl.recv() => match ctl {
                None => break, // handle dropped → shut down
                Some(Ctl::SetOnline(on)) => {
                    online = on;
                    status.online = on;
                    if on {
                        status.detail = "online — fetching…".into();
                        next_at = Instant::now(); // fetch promptly
                    } else {
                        backoff.reset();
                        status.degraded = false;
                        status.detail = "offline — fixture data only".into();
                        status.next_attempt_epoch_s = None;
                    }
                    let _ = tx.send(IngestMsg::Status(status.clone()));
                    wake();
                }
                Some(Ctl::FetchNow) => {
                    if online {
                        next_at = Instant::now();
                    }
                }
            },
            _ = sleep_until(next_at), if online && gdelt.is_some() => {
                let gdelt = gdelt.as_ref().unwrap();
                let delay = fetch_cycle(gdelt, &limiter, &mut backoff, &mut status, &tx, &wake).await;
                next_at = Instant::now() + delay;
            }
        }
    }
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
