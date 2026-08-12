//! Ingest worker: a long-lived thread with a current-thread tokio runtime
//! that polls live sources on their own cadences (GDELT every feed interval;
//! ACLED, when built with `acled-live` and credentialed, twice a day; NOAA
//! and IODA, both keyless, every several minutes), normalizes, and streams
//! incremental batches back to the UI.
//!
//! The desktop runtime never loads synthetic fixtures. The UI thread owns
//! storage, so the worker never touches the database: it hands `(events,
//! failures)` back over a channel and the app ingests them. Live failures
//! degrade gracefully: last-known real data stays visible while the worker
//! reports status and backs off.

use std::sync::mpsc;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use core_types::{
    GeoTemporalEvent, IngestFailure, SignalSource, SourceError, SourceFilters, TimeWindow,
};
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

/// IODA detects outages in near-real-time; poll on the same cadence as
/// GDELT's feed. Each poll looks back further than the poll interval so a
/// short gap (startup, a missed cycle) doesn't lose an outage that started
/// and ended between polls — IODA's own server-side `extendWindow` (14 days
/// by default) additionally surfaces alerts that started earlier and are
/// still ongoing. Dedup-by-id absorbs the overlap, as everywhere else.
const IODA_POLL_SECS: u64 = 15 * 60;
const IODA_LOOKBACK_HOURS: i64 = 6;

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

/// Feature-gated IODA handle — same stub pattern as [`noaa`]; keyless, so
/// with the feature on `make()` is effectively always `Some`.
#[cfg(feature = "ioda-live")]
mod ioda {
    pub use source_ioda::IodaSource;
    pub fn make() -> Result<Option<IodaSource>, core_types::SourceError> {
        IodaSource::from_env().map(Some)
    }
}
#[cfg(not(feature = "ioda-live"))]
mod ioda {
    use core_types::{
        GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
        SourceId, TimeWindow,
    };

    pub struct IodaSource;
    pub fn make() -> Result<Option<IodaSource>, SourceError> {
        Ok(None)
    }
    impl SignalSource for IodaSource {
        fn id(&self) -> SourceId {
            SourceId::Ioda
        }
        async fn fetch(
            &self,
            _: TimeWindow,
            _: &SourceFilters,
        ) -> Result<Vec<RawRecord>, SourceError> {
            unreachable!("built without the ioda-live feature")
        }
        fn normalize(&self, _: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
            unreachable!("built without the ioda-live feature")
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
    /// One part of a multi-feed source failed while another succeeded.
    pub partial: bool,
}

impl SourceStatus {
    fn offline(name: &'static str) -> Self {
        Self {
            name,
            online: false,
            last_attempt_epoch_s: None,
            last_success_epoch_s: None,
            next_attempt_epoch_s: None,
            detail: "live updates paused — cached real data only".into(),
            degraded: false,
            partial: false,
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
    /// Fatal worker initialization failure.
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

/// Spawn the ingest worker. Results arrive on the returned channel; `wake`
/// (a repaint request) fires after every message so the UI polls promptly.
/// The returned handle controls online mode and stops the worker when dropped.
pub fn spawn(wake: impl Fn() + Send + 'static) -> (mpsc::Receiver<IngestMsg>, IngestHandle) {
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
            runtime.block_on(worker(tx_res, rx_ctl, wake));
        })
        .expect("spawn ingest thread");
    (rx_res, IngestHandle { ctl: tx_ctl })
}

async fn worker(
    tx: mpsc::Sender<IngestMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
) {
    // Live GDELT loop, driven by control messages and the feed cadence.
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
    // Fixed-window override for date-restricted ACLED tiers (some accounts
    // may only read events older than N months, so a rolling recent window
    // would always be empty). Dedup keeps the repeat polls idempotent.
    let acled_window = fixed_window_env("LES_ACLED_WINDOW");
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

    // IODA (feature-gated, keyless): near-real-time internet-outage events.
    let ioda_src = match ioda::make() {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(error = %e, "ioda source init failed; continuing without it");
            None
        }
    };
    let ioda_limiter = sched::request_limiter();
    let mut ioda_backoff = sched::Backoff::default();
    let mut ioda_next = Instant::now();
    let mut ioda_status = SourceStatus::offline("IODA");

    loop {
        tokio::select! {
            ctl = rx_ctl.recv() => match ctl {
                None => break, // handle dropped → shut down
                Some(Ctl::SetOnline(on)) => {
                    online = on;
                    status.online = on;
                    acled_status.online = on && acled_src.is_some();
                    noaa_status.online = on && noaa_src.is_some();
                    ioda_status.online = on && ioda_src.is_some();
                    if on {
                        status.detail = "online — fetching…".into();
                        status.partial = false;
                        next_at = Instant::now(); // fetch promptly
                        if acled_src.is_some() {
                            acled_status.detail = "online — fetching…".into();
                            acled_status.partial = false;
                            acled_next = Instant::now();
                        }
                        if noaa_src.is_some() {
                            noaa_status.detail = "online — fetching…".into();
                            noaa_status.partial = false;
                            noaa_next = Instant::now();
                        }
                        if ioda_src.is_some() {
                            ioda_status.detail = "online — fetching…".into();
                            ioda_status.partial = false;
                            ioda_next = Instant::now();
                        }
                    } else {
                        backoff.reset();
                        status.degraded = false;
                        status.partial = false;
                        status.detail = "live updates paused — cached real data only".into();
                        status.next_attempt_epoch_s = None;
                        for (b, s) in [
                            (&mut acled_backoff, &mut acled_status),
                            (&mut noaa_backoff, &mut noaa_status),
                            (&mut ioda_backoff, &mut ioda_status),
                        ] {
                            b.reset();
                            s.degraded = false;
                            s.partial = false;
                            s.detail = "live updates paused — cached real data only".into();
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
                    if ioda_src.is_some() {
                        let _ = tx.send(IngestMsg::Status(ioda_status.clone()));
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
                        if ioda_src.is_some() {
                            ioda_next = Instant::now();
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
                let window = acled_window.unwrap_or_else(|| {
                    let now = Utc::now();
                    TimeWindow::new(now - ChronoDuration::days(ACLED_LOOKBACK_DAYS), now)
                });
                let delay = live_cycle(acled_src, "acled", window, ACLED_POLL_SECS,
                    &acled_limiter, &mut acled_backoff, &mut acled_status, &tx, &wake).await;
                acled_next = Instant::now() + delay;
            }
            _ = sleep_until(noaa_next), if online && noaa_src.is_some() => {
                let noaa_src = noaa_src.as_ref().unwrap();
                // The alerts feed is a now-snapshot; the window is nominal.
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::hours(1), now);
                let delay = live_cycle(noaa_src, "noaa", window, NOAA_POLL_SECS,
                    &noaa_limiter, &mut noaa_backoff, &mut noaa_status, &tx, &wake).await;
                noaa_next = Instant::now() + delay;
            }
            _ = sleep_until(ioda_next), if online && ioda_src.is_some() => {
                let ioda_src = ioda_src.as_ref().unwrap();
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::hours(IODA_LOOKBACK_HOURS), now);
                let delay = live_cycle(ioda_src, "ioda", window, IODA_POLL_SECS,
                    &ioda_limiter, &mut ioda_backoff, &mut ioda_status, &tx, &wake).await;
                ioda_next = Instant::now() + delay;
            }
        }
    }
}

/// Parse a fixed `YYYY-MM-DD|YYYY-MM-DD` window from an env var (both dates
/// inclusive → half-open window ending at the day after the second date).
/// Invalid values are ignored with a warning rather than killing the loop.
fn fixed_window_env(var: &str) -> Option<TimeWindow> {
    let raw = std::env::var(var).ok()?;
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
    let window = raw.split_once('|').and_then(|(a, b)| {
        let (start, end) = (parse(a)?, parse(b)?);
        let start = Utc.from_utc_datetime(&start.and_hms_opt(0, 0, 0)?);
        let end = Utc.from_utc_datetime(&(end + ChronoDuration::days(1)).and_hms_opt(0, 0, 0)?);
        (start < end).then(|| TimeWindow::new(start, end))
    });
    match &window {
        Some(w) => tracing::info!(%var, start = %w.start, end = %w.end, "fixed window override"),
        None => {
            tracing::warn!(%var, raw, "ignoring unparseable window (want YYYY-MM-DD|YYYY-MM-DD)")
        }
    }
    window
}

/// One poll of a simple single-feed live source (ACLED, NOAA): fetch
/// `window`, emit the normalized batch and an updated status, and return the
/// wait before the next attempt (`poll_secs` on success, backoff on
/// failure). GDELT keeps its own bespoke two-feed cycle ([`fetch_cycle`]).
#[allow(clippy::too_many_arguments)] // internal plumbing, mirrors fetch_cycle
async fn live_cycle<S: SignalSource>(
    src: &S,
    origin: &'static str,
    window: TimeWindow,
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

    let delay = match src.fetch(window, &SourceFilters::default()).await {
        Ok(raws) => {
            let (events, failures) = storage::partition_normalized(src, &raws);
            backoff.reset();
            status.degraded = false;
            status.partial = false;
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
            status.partial = false;
            status.detail = format!(
                "degraded — showing cached real data · {}",
                compact_error(&e)
            );
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
        status.partial = false;
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
        status.partial = !partial.is_empty();
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
        parts.push(format!("DOC: {}", compact_error(e)));
    }
    if let Some(e) = events_err {
        parts.push(format!("Events: {}", compact_error(e)));
    }
    parts.join("; ")
}

fn compact_error(error: &SourceError) -> String {
    let text = error.to_string();
    let without_url = text
        .split_once(" for url (")
        .map_or(text.as_str(), |(summary, _)| summary);
    const MAX_CHARS: usize = 180;
    if without_url.chars().count() <= MAX_CHARS {
        without_url.to_owned()
    } else {
        format!(
            "{}…",
            without_url.chars().take(MAX_CHARS).collect::<String>()
        )
    }
}

/// Cheap sub-second jitter in [0, 1) from the wall clock (no `rand` dep needed
/// for politeness jitter).
fn jitter01() -> f64 {
    f64::from(Utc::now().timestamp_subsec_nanos()) / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_error_drops_request_urls() {
        let error = SourceError::Http(
            "error sending request for url (https://example.invalid/private?long=query)".into(),
        );
        let summary = compact_error(&error);
        assert_eq!(summary, "http error: error sending request");
        assert!(!summary.contains("https://"));
    }

    #[test]
    fn compact_error_bounds_unstructured_messages() {
        let summary = compact_error(&SourceError::Other("x".repeat(300)));
        assert_eq!(summary.chars().count(), 181);
        assert!(summary.ends_with('…'));
    }
}
