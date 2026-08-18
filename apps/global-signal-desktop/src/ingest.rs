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
    GeoTemporalEvent, IngestFailure, SignalSource, SourceError, SourceFilters, SourceId, TimeWindow,
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
/// that reuse an id are deliberately not re-applied; see
/// docs/ENGINEERING_NOTES.md).
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

/// Feature-gated Telegram handle — same credential-gated stub pattern as
/// [`acled`] (a missing session means missing setup, not a hard error).
#[cfg(feature = "telegram-live")]
mod telegram {
    pub use source_telegram::TelegramSource;
    pub const BUILT: bool = true;
    pub fn make() -> Result<Option<TelegramSource>, core_types::SourceError> {
        TelegramSource::from_env()
    }
}
#[cfg(not(feature = "telegram-live"))]
mod telegram {
    use core_types::{
        GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
        SourceId, TimeWindow,
    };

    pub struct TelegramSource;
    pub const BUILT: bool = false;
    pub fn make() -> Result<Option<TelegramSource>, SourceError> {
        Ok(None)
    }
    impl SignalSource for TelegramSource {
        fn id(&self) -> SourceId {
            SourceId::Telegram
        }
        async fn fetch(
            &self,
            _: TimeWindow,
            _: &SourceFilters,
        ) -> Result<Vec<RawRecord>, SourceError> {
            unreachable!("built without the telegram-live feature")
        }
        fn normalize(&self, _: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
            unreachable!("built without the telegram-live feature")
        }
    }
}

/// Bluesky is a *stream*, not a feed: the socket task counts continuously and
/// each "poll" only drains what it counted. Draining on the accumulator's own
/// flush cadence keeps at most one partial window pending at a time.
const BLUESKY_POLL_SECS: u64 = 5 * 60;

/// Telegram is poll-based (unlike Bluesky): each cycle sweeps a small
/// curated channel allowlist over MTProto. Kept well clear of flood limits —
/// eight channels every 15 minutes, same cadence as IODA.
const TELEGRAM_POLL_SECS: u64 = 15 * 60;

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

/// Feature-gated Bluesky handle — same stub pattern, with one difference:
/// `make()` also starts the long-lived socket task, because a stream that is
/// never started would drain empty forever with no visible error.
#[cfg(feature = "bluesky-live")]
mod bluesky {
    pub use source_bluesky::BlueskySource;
    pub fn make() -> Result<Option<BlueskySource>, core_types::SourceError> {
        let src = BlueskySource::from_env()?;
        // Detached on purpose: it reconnects on its own and lives as long as
        // the worker's runtime.
        src.spawn_stream();
        Ok(Some(src))
    }
}
#[cfg(not(feature = "bluesky-live"))]
mod bluesky {
    use core_types::{
        GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
        SourceId, TimeWindow,
    };

    pub struct BlueskySource;
    pub fn make() -> Result<Option<BlueskySource>, SourceError> {
        Ok(None)
    }
    impl SignalSource for BlueskySource {
        fn id(&self) -> SourceId {
            SourceId::Bluesky
        }
        async fn fetch(
            &self,
            _: TimeWindow,
            _: &SourceFilters,
        ) -> Result<Vec<RawRecord>, SourceError> {
            unreachable!("built without the bluesky-live feature")
        }
        fn normalize(&self, _: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
            unreachable!("built without the bluesky-live feature")
        }
    }
}

/// Live-source status surfaced in the UI — one per source, keyed by `source`.
#[derive(Debug, Clone)]
pub struct SourceStatus {
    /// Which live source this line describes. The Settings screen joins on
    /// this rather than on `name`: `core_types::attribution` is keyed by
    /// `SourceId`, and matching display strings across two crates is exactly
    /// the drift this field exists to prevent.
    pub source: SourceId,
    /// Display label for this source ("GDELT", "ACLED").
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
    /// The user has this source switched on in Settings. Distinct from
    /// `online` (global live-updates pause), from being compiled in, and from
    /// being credentialed — a source can be enabled and still never fetch
    /// because one of those three is false.
    pub enabled: bool,
}

impl SourceStatus {
    fn offline(source: SourceId, name: &'static str) -> Self {
        Self {
            source,
            name,
            online: false,
            last_attempt_epoch_s: None,
            last_success_epoch_s: None,
            next_attempt_epoch_s: None,
            detail: "live updates paused — cached real data only".into(),
            degraded: false,
            partial: false,
            enabled: true,
        }
    }
}

/// Nominal poll interval for a source, in seconds — what the Settings screen
/// shows as its cadence. `None` for a source the desktop never schedules
/// (fixtures are not a desktop runtime source at all). These are the
/// *nominal* intervals: a failing source is on `sched::Backoff`'s longer
/// retry delay instead, which is why the Settings screen shows the live
/// "next fetch" beside this rather than in place of it.
pub fn cadence_secs(source: SourceId) -> Option<u64> {
    match source {
        SourceId::Gdelt => Some(sched::FEED_INTERVAL_SECS as u64),
        SourceId::Acled => Some(ACLED_POLL_SECS),
        SourceId::Noaa => Some(NOAA_POLL_SECS),
        SourceId::Ioda => Some(IODA_POLL_SECS),
        SourceId::Bluesky => Some(BLUESKY_POLL_SECS),
        SourceId::Telegram => Some(TELEGRAM_POLL_SECS),
        SourceId::Fixtures => None,
    }
}

/// Per-source on/off switches owned by the ingest worker. The UI holds the
/// user's intent and replays it over [`Ctl`]; the worker holds this copy and
/// gates its `select!` arms on it. Nothing else moves — the worker still owns
/// every source, limiter, and backoff exactly as before.
#[derive(Debug, Clone, Copy)]
struct Enabled {
    gdelt: bool,
    acled: bool,
    noaa: bool,
    ioda: bool,
    bluesky: bool,
    telegram: bool,
}

impl Default for Enabled {
    /// Every source on: a build that shipped a source enables it unless the
    /// user has said otherwise.
    fn default() -> Self {
        Self {
            gdelt: true,
            acled: true,
            noaa: true,
            ioda: true,
            bluesky: true,
            telegram: true,
        }
    }
}

impl Enabled {
    fn slot(&mut self, source: SourceId) -> Option<&mut bool> {
        match source {
            SourceId::Gdelt => Some(&mut self.gdelt),
            SourceId::Acled => Some(&mut self.acled),
            SourceId::Noaa => Some(&mut self.noaa),
            SourceId::Ioda => Some(&mut self.ioda),
            SourceId::Bluesky => Some(&mut self.bluesky),
            SourceId::Telegram => Some(&mut self.telegram),
            // Not a desktop runtime source; nothing to switch.
            SourceId::Fixtures => None,
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
    /// Turn one source's scheduled polling on or off (Settings screen).
    SetSourceEnabled(SourceId, bool),
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

    /// Switch one source's scheduled polling on or off. Takes effect on the
    /// worker's next loop turn; an in-flight fetch is allowed to finish
    /// rather than being cancelled mid-request.
    pub fn set_source_enabled(&self, source: SourceId, on: bool) {
        let _ = self.ctl.send(Ctl::SetSourceEnabled(source, on));
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
    // Per-source switches from the Settings screen. The UI replays its saved
    // state at startup, so `default()` (all on) only stands until then.
    let mut enabled = Enabled::default();
    let mut next_at = Instant::now();
    let mut status = SourceStatus::offline(SourceId::Gdelt, "GDELT");

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
    let mut acled_status = SourceStatus::offline(SourceId::Acled, "ACLED");
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
    let mut noaa_status = SourceStatus::offline(SourceId::Noaa, "NOAA");

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
    let mut ioda_status = SourceStatus::offline(SourceId::Ioda, "IODA");

    // Bluesky (feature-gated, keyless): aggregate chatter volume. `make()`
    // already started the socket; these cycles only drain what it counted, so
    // the first drain waits a full flush window rather than firing instantly
    // on an empty accumulator.
    let bluesky_src = match bluesky::make() {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(error = %e, "bluesky source init failed; continuing without it");
            None
        }
    };
    let bluesky_limiter = sched::request_limiter();
    let mut bluesky_backoff = sched::Backoff::default();
    let mut bluesky_next = Instant::now() + std::time::Duration::from_secs(BLUESKY_POLL_SECS);
    let mut bluesky_status = SourceStatus::offline(SourceId::Bluesky, "Bluesky");

    // Telegram (feature-gated, credential-gated): aggregate chatter volume
    // over a curated public-channel allowlist.
    let telegram_src = match telegram::make() {
        Ok(src) => src,
        Err(e) => {
            tracing::warn!(error = %e, "telegram source init failed; continuing without it");
            None
        }
    };
    let telegram_limiter = sched::request_limiter();
    let mut telegram_backoff = sched::Backoff::default();
    let mut telegram_next = Instant::now();
    let mut telegram_status = SourceStatus::offline(SourceId::Telegram, "Telegram");
    if telegram::BUILT && telegram_src.is_none() {
        telegram_status.detail =
            "off — set TELEGRAM_API_ID / TELEGRAM_API_HASH and run login_setup".into();
        let _ = tx.send(IngestMsg::Status(telegram_status.clone()));
        wake();
    }

    loop {
        tokio::select! {
            ctl = rx_ctl.recv() => match ctl {
                None => break, // handle dropped → shut down
                Some(Ctl::SetOnline(on)) => {
                    online = on;
                    // A source switched off in Settings stays off when live
                    // updates resume — the global toggle does not override
                    // the per-source one.
                    status.online = on && enabled.gdelt;
                    acled_status.online = on && enabled.acled && acled_src.is_some();
                    noaa_status.online = on && enabled.noaa && noaa_src.is_some();
                    ioda_status.online = on && enabled.ioda && ioda_src.is_some();
                    bluesky_status.online = on && enabled.bluesky && bluesky_src.is_some();
                    telegram_status.online = on && enabled.telegram && telegram_src.is_some();
                    if on {
                        if enabled.gdelt {
                            status.detail = "online — fetching…".into();
                            status.partial = false;
                            next_at = Instant::now(); // fetch promptly
                        }
                        if enabled.acled && acled_src.is_some() {
                            acled_status.detail = "online — fetching…".into();
                            acled_status.partial = false;
                            acled_next = Instant::now();
                        }
                        if enabled.noaa && noaa_src.is_some() {
                            noaa_status.detail = "online — fetching…".into();
                            noaa_status.partial = false;
                            noaa_next = Instant::now();
                        }
                        if enabled.ioda && ioda_src.is_some() {
                            ioda_status.detail = "online — fetching…".into();
                            ioda_status.partial = false;
                            ioda_next = Instant::now();
                        }
                        if enabled.bluesky && bluesky_src.is_some() {
                            // Counting resumes immediately, but a drain now
                            // would publish a stub window; wait one cadence.
                            bluesky_status.detail = "online — counting…".into();
                            bluesky_status.partial = false;
                            bluesky_next = Instant::now()
                                + std::time::Duration::from_secs(BLUESKY_POLL_SECS);
                        }
                        if enabled.telegram && telegram_src.is_some() {
                            telegram_status.detail = "online — fetching…".into();
                            telegram_status.partial = false;
                            telegram_next = Instant::now();
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
                            (&mut bluesky_backoff, &mut bluesky_status),
                            (&mut telegram_backoff, &mut telegram_status),
                        ] {
                            b.reset();
                            s.degraded = false;
                            s.partial = false;
                            s.detail = "live updates paused — cached real data only".into();
                            s.next_attempt_epoch_s = None;
                        }
                    }
                    if on {
                        // Sources switched off in Settings say so rather than
                        // inheriting a stale "paused" line once live updates
                        // are back on.
                        for (is_on, s) in [
                            (enabled.gdelt, &mut status),
                            (enabled.acled, &mut acled_status),
                            (enabled.noaa, &mut noaa_status),
                            (enabled.ioda, &mut ioda_status),
                            (enabled.bluesky, &mut bluesky_status),
                            (enabled.telegram, &mut telegram_status),
                        ] {
                            if !is_on {
                                s.detail = "off — switched off in Settings".into();
                                s.next_attempt_epoch_s = None;
                            }
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
                    if bluesky_src.is_some() {
                        let _ = tx.send(IngestMsg::Status(bluesky_status.clone()));
                    }
                    if telegram_src.is_some() {
                        let _ = tx.send(IngestMsg::Status(telegram_status.clone()));
                    }
                    wake();
                }
                Some(Ctl::FetchNow) => {
                    if online {
                        if enabled.gdelt {
                            next_at = Instant::now();
                        }
                        if enabled.acled && acled_src.is_some() {
                            acled_next = Instant::now();
                        }
                        if enabled.noaa && noaa_src.is_some() {
                            noaa_next = Instant::now();
                        }
                        if enabled.ioda && ioda_src.is_some() {
                            ioda_next = Instant::now();
                        }
                        if enabled.bluesky && bluesky_src.is_some() {
                            // Safe to drain early: only completed windows are
                            // published, so nothing is half-counted.
                            bluesky_next = Instant::now();
                        }
                        if enabled.telegram && telegram_src.is_some() {
                            telegram_next = Instant::now();
                        }
                    }
                }
                // The desktop never schedules fixtures, so there is nothing to
                // switch — drop it here rather than teaching the loop below
                // about a source it does not run.
                Some(Ctl::SetSourceEnabled(SourceId::Fixtures, _)) => {}
                Some(Ctl::SetSourceEnabled(source, on)) => {
                    let changed = enabled.slot(source).is_some_and(|slot| {
                        let changed = *slot != on;
                        *slot = on;
                        changed
                    });
                    if changed {
                        let (st, next, available) = match source {
                            SourceId::Gdelt => (&mut status, &mut next_at, gdelt.is_some()),
                            SourceId::Acled => {
                                (&mut acled_status, &mut acled_next, acled_src.is_some())
                            }
                            SourceId::Noaa => {
                                (&mut noaa_status, &mut noaa_next, noaa_src.is_some())
                            }
                            SourceId::Ioda => {
                                (&mut ioda_status, &mut ioda_next, ioda_src.is_some())
                            }
                            SourceId::Bluesky => {
                                (&mut bluesky_status, &mut bluesky_next, bluesky_src.is_some())
                            }
                            SourceId::Telegram => (
                                &mut telegram_status,
                                &mut telegram_next,
                                telegram_src.is_some(),
                            ),
                            SourceId::Fixtures => unreachable!("filtered by the arm above"),
                        };
                        st.enabled = on;
                        st.online = on && online && available;
                        st.degraded = false;
                        st.partial = false;
                        if !on {
                            st.detail = "off — switched off in Settings".into();
                            st.next_attempt_epoch_s = None;
                        } else if st.online {
                            st.detail = "online — fetching…".into();
                            // Bluesky counts continuously; a drain now would
                            // publish a stub window, so it waits one cadence
                            // exactly as the resume-from-paused path does.
                            *next = if source == SourceId::Bluesky {
                                Instant::now() + std::time::Duration::from_secs(BLUESKY_POLL_SECS)
                            } else {
                                Instant::now()
                            };
                        } else {
                            st.detail = "on — waiting for live updates".into();
                            st.next_attempt_epoch_s = None;
                        }
                        let _ = tx.send(IngestMsg::Status(st.clone()));
                        wake();
                    }
                }
            },
            _ = sleep_until(next_at), if online && enabled.gdelt && gdelt.is_some() => {
                let gdelt = gdelt.as_ref().unwrap();
                let delay = fetch_cycle(gdelt, &limiter, &mut backoff, &mut status, &tx, &wake).await;
                next_at = Instant::now() + delay;
            }
            _ = sleep_until(acled_next), if online && enabled.acled && acled_src.is_some() => {
                let acled_src = acled_src.as_ref().unwrap();
                let window = acled_window.unwrap_or_else(|| {
                    let now = Utc::now();
                    TimeWindow::new(now - ChronoDuration::days(ACLED_LOOKBACK_DAYS), now)
                });
                let delay = live_cycle(acled_src, "acled", window, ACLED_POLL_SECS,
                    &acled_limiter, &mut acled_backoff, &mut acled_status, &tx, &wake).await;
                acled_next = Instant::now() + delay;
            }
            _ = sleep_until(noaa_next), if online && enabled.noaa && noaa_src.is_some() => {
                let noaa_src = noaa_src.as_ref().unwrap();
                // The alerts feed is a now-snapshot; the window is nominal.
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::hours(1), now);
                let delay = live_cycle(noaa_src, "noaa", window, NOAA_POLL_SECS,
                    &noaa_limiter, &mut noaa_backoff, &mut noaa_status, &tx, &wake).await;
                noaa_next = Instant::now() + delay;
            }
            _ = sleep_until(ioda_next), if online && enabled.ioda && ioda_src.is_some() => {
                let ioda_src = ioda_src.as_ref().unwrap();
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::hours(IODA_LOOKBACK_HOURS), now);
                let delay = live_cycle(ioda_src, "ioda", window, IODA_POLL_SECS,
                    &ioda_limiter, &mut ioda_backoff, &mut ioda_status, &tx, &wake).await;
                ioda_next = Instant::now() + delay;
            }
            _ = sleep_until(bluesky_next), if online && bluesky_src.is_some() => {
                let bluesky_src = bluesky_src.as_ref().unwrap();
                // Nominal window: draining a stream has no addressable past,
                // the accumulator simply holds everything since the last drain.
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::seconds(BLUESKY_POLL_SECS as i64), now);
                if enabled.bluesky {
                    let delay = live_cycle(bluesky_src, "bluesky", window, BLUESKY_POLL_SECS,
                        &bluesky_limiter, &mut bluesky_backoff, &mut bluesky_status, &tx, &wake).await;
                    bluesky_next = Instant::now() + delay;
                } else {
                    // Bluesky is the one source whose arm still runs while it
                    // is switched off. The firehose socket is opened once by
                    // `make()` and has no teardown path, so the accumulator
                    // keeps counting either way; draining and dropping it on
                    // cadence is what keeps that accumulator bounded and
                    // guarantees nothing counted while off is ever stored.
                    // No network request is made here — the drain is local.
                    let _ = bluesky_src.fetch(window, &SourceFilters::default()).await;
                    bluesky_next =
                        Instant::now() + std::time::Duration::from_secs(BLUESKY_POLL_SECS);
                }
            }
            _ = sleep_until(telegram_next), if online && enabled.telegram && telegram_src.is_some() => {
                let telegram_src = telegram_src.as_ref().unwrap();
                // Nominal window: each channel's own high-water mark bounds
                // the sweep to new messages, so the query window is unused.
                let now = Utc::now();
                let window = TimeWindow::new(now - ChronoDuration::seconds(TELEGRAM_POLL_SECS as i64), now);
                let delay = live_cycle(telegram_src, "telegram", window, TELEGRAM_POLL_SECS,
                    &telegram_limiter, &mut telegram_backoff, &mut telegram_status, &tx, &wake).await;
                telegram_next = Instant::now() + delay;
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
        Err(e) => {
            failures.push(fetch_failure(
                "doc",
                &e,
                gdelt.doc_query(window, &filters).query,
            ));
            doc_err = Some(e);
        }
    }
    match gdelt.fetch_events().await {
        Ok(raws) => {
            let (e, f) = storage::partition_normalized(gdelt, &raws);
            events.extend(e);
            failures.extend(f);
        }
        Err(e) => {
            failures.push(fetch_failure(
                "events",
                &e,
                source_gdelt::EVENTS_LASTUPDATE_URL.to_owned(),
            ));
            events_err = Some(e);
        }
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

/// Record a whole-fetch failure as an [`IngestFailure`] so it reaches
/// `ingest_log`.
///
/// A failure of *one* GDELT half is not `degraded` — the cycle still succeeds
/// on the other half — so without this its only trace is a `partial:` fragment
/// inside a `SourceStatus.detail` that the next cycle overwrites. A malformed
/// DOC query emptied the entire media-attention half of the dashboard for a
/// whole session that way, while the app reported itself online. There is no
/// `RawRecord` to attribute, so the excerpt carries the request instead (the
/// DOC query expression, or the Events pointer URL) — the thing you need to
/// know to reproduce it. Volume is bounded by the scheduler's backoff.
fn fetch_failure(half: &str, err: &SourceError, request: String) -> IngestFailure {
    IngestFailure {
        source: SourceId::Gdelt,
        reason: format!("{half} fetch failed: {err}"),
        raw_excerpt: request,
        occurred_at: Utc::now(),
    }
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
