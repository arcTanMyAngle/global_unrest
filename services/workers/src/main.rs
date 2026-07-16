//! M4 ingest worker: owns its own DuckDB — never shared with the desktop app
//! or `services/api` (DuckDB is single-writer-per-file across processes,
//! docs/ARCHITECTURE.md) — and republishes the session as a versioned
//! Parquet snapshot after every ingest. That snapshot is the *only* surface
//! `services/api` reads (docs/API.md).
//!
//! Ingestion mirrors the desktop's online loop (same `source-gdelt`
//! rate-limit/backoff/cadence policy, same dedup-by-event-id semantics) but
//! is its own binary: the desktop and the worker never touch the same
//! `.duckdb` file, so there is deliberately some duplication of the cycle
//! orchestration here rather than a shared dependency on the desktop crate.

use std::path::PathBuf;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use core_types::{SignalSource, SourceFilters, TimeWindow};
use source_fixtures::FixtureSource;
use source_gdelt::{GdeltSource, sched};
use storage::StorageHandle;
use tokio::time::{Instant, sleep_until};

/// How far back each online DOC poll looks; overlapping windows plus
/// storage's dedup-by-id absorb the 15-minute feed boundary.
const DOC_LOOKBACK_MINS: i64 = 60;
/// Versioned snapshots kept under the publish root unless overridden.
const DEFAULT_KEEP_LAST_SNAPSHOTS: usize = 3;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let data_dir = env_path("LES_WORKER_DATA_DIR").unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;
    let publish_root = env_path("LES_PUBLISH_DIR").unwrap_or_else(|| data_dir.join("publish"));
    let fixtures_dir = env_path("LES_FIXTURES_DIR")
        .or_else(find_fixtures_dir)
        .ok_or_else(|| anyhow::anyhow!("no fixtures dir found; set LES_FIXTURES_DIR"))?;
    let retention_days: Option<u32> = std::env::var("LES_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|d| *d > 0);
    let keep_last: usize = std::env::var("LES_PUBLISH_KEEP_LAST")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_KEEP_LAST_SNAPSHOTS);
    // The worker's whole job is live ingestion, so it defaults to online;
    // LES_ONLINE=0 pins it to fixtures-only (e.g. offline dev/CI smoke runs).
    let online = std::env::var("LES_ONLINE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(true);

    tracing::info!(
        data_dir = %data_dir.display(),
        publish_root = %publish_root.display(),
        fixtures_dir = %fixtures_dir.display(),
        online,
        keep_last,
        "worker starting"
    );

    let store = StorageHandle::open(Some(data_dir.join("worker.duckdb")), Box::new(|| {}))?;
    store.set_retention(retention_days);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(store, fixtures_dir, publish_root, keep_last, online))
}

async fn run(
    store: StorageHandle,
    fixtures_dir: PathBuf,
    publish_root: PathBuf,
    keep_last: usize,
    online: bool,
) -> anyhow::Result<()> {
    // 1. Offline base: load and ingest fixtures once. Fatal if missing —
    // there would be nothing to ever publish.
    let (events, failures) = load_fixtures(&fixtures_dir).await?;
    let n = events.len();
    let report = store.ingest(events, failures).wait()?;
    tracing::info!(loaded = n, ?report, "fixtures ingested");
    publish(&store, &publish_root, keep_last)?;

    let gdelt = if online {
        match GdeltSource::new() {
            Ok(mut g) => {
                if let Ok(doc) = std::env::var("LES_GDELT_DOC_ENDPOINT") {
                    g = g.with_endpoint(doc);
                }
                if let Ok(events_url) = std::env::var("LES_GDELT_EVENTS_URL") {
                    g = g.with_events_url(events_url);
                }
                Some(g)
            }
            Err(e) => {
                tracing::warn!(error = %e, "gdelt source init failed; staying fixtures-only");
                None
            }
        }
    } else {
        tracing::info!("LES_ONLINE=0 — fixtures only, no live loop");
        None
    };

    let Some(gdelt) = gdelt else {
        return Ok(());
    };

    let limiter = sched::request_limiter();
    let mut backoff = sched::Backoff::default();
    let mut next_at = Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown signal received");
                break;
            }
            _ = sleep_until(next_at) => {
                let delay = fetch_cycle(&gdelt, &limiter, &mut backoff, &store, &publish_root, keep_last).await;
                next_at = Instant::now() + delay;
            }
        }
    }
    Ok(())
}

/// Run one live fetch (DOC attention + Events dump); ingest and republish
/// only when it produced something new. Returns how long to wait before the
/// next attempt.
async fn fetch_cycle(
    gdelt: &GdeltSource,
    limiter: &sched::Limiter,
    backoff: &mut sched::Backoff,
    store: &StorageHandle,
    publish_root: &std::path::Path,
    keep_last: usize,
) -> std::time::Duration {
    limiter.until_ready().await;

    let now = Utc::now();
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
        let err = pick_backoff_error(&doc_err, &events_err);
        let d = backoff.after_error(err, jitter01());
        tracing::warn!(
            retry_in_s = d.as_secs(),
            attempt = backoff.attempt(),
            doc_err = ?doc_err,
            events_err = ?events_err,
            "gdelt fetch failed; backing off"
        );
        d
    } else {
        backoff.reset();
        let secs = sched::until_next_slot(
            now.timestamp(),
            sched::FEED_INTERVAL_SECS,
            sched::FEED_LAG_SECS,
        );
        std::time::Duration::from_secs(secs.max(1) as u64)
    };

    if !events.is_empty() || !failures.is_empty() {
        let loaded = events.len();
        match store.ingest(events, failures).wait() {
            Ok(report) => {
                tracing::info!(loaded, ?report, "gdelt cycle ingested");
                if let Err(e) = publish(store, publish_root, keep_last) {
                    tracing::error!(error = %e, "snapshot publish failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "gdelt cycle ingest failed"),
        }
    }
    delay
}

fn publish(store: &StorageHandle, root: &std::path::Path, keep_last: usize) -> anyhow::Result<()> {
    let keep = if keep_last == 0 {
        None
    } else {
        Some(keep_last)
    };
    let report = store
        .publish_snapshot(root.to_path_buf(), keep)
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!(version = %report.version, events = report.events, "snapshot published");
    Ok(())
}

fn pick_backoff_error<'a>(
    doc_err: &'a Option<core_types::SourceError>,
    events_err: &'a Option<core_types::SourceError>,
) -> &'a core_types::SourceError {
    for e in [doc_err, events_err].into_iter().flatten() {
        if matches!(e, core_types::SourceError::RateLimited { .. }) {
            return e;
        }
    }
    doc_err
        .as_ref()
        .or(events_err.as_ref())
        .expect("a failure exists")
}

fn jitter01() -> f64 {
    f64::from(Utc::now().timestamp_subsec_nanos()) / 1e9
}

async fn load_fixtures(
    dir: &std::path::Path,
) -> anyhow::Result<(
    Vec<core_types::GeoTemporalEvent>,
    Vec<core_types::IngestFailure>,
)> {
    let source = FixtureSource::from_dir(dir)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", dir.display()))?;
    if source.files().is_empty() {
        anyhow::bail!(
            "no fixture files in {} — run `cargo run -p source-fixtures --bin generate-fixtures`",
            dir.display()
        );
    }
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap(),
    );
    let raws = source
        .fetch(window, &SourceFilters::default())
        .await
        .map_err(|e| anyhow::anyhow!("fixture fetch: {e}"))?;
    Ok(storage::partition_normalized(&source, &raws))
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var).ok().map(PathBuf::from)
}

/// Mirrors the desktop's fixtures lookup: `LES_FIXTURES_DIR` env override,
/// then `fixtures/` in the working directory or any ancestor.
fn find_fixtures_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|a| a.join("fixtures"))
        .find(|p| p.is_dir())
}

/// Separate app-data namespace from the desktop (`live-earth-signals`) so the
/// two processes never resolve to the same directory by accident.
fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("org", "LiveEarthSignals", "live-earth-signals-worker")
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("data"))
}
