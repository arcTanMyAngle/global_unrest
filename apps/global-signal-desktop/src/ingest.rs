//! Fixture ingest pipeline: fetch + normalize on a worker thread (with a
//! small current-thread tokio runtime for the async `SignalSource::fetch`),
//! then hand (events, failures) back to the UI, which owns the storage
//! actor. Fixture mode is the permanent offline regression path.

use std::path::PathBuf;
use std::sync::mpsc;

use chrono::{TimeZone, Utc};
use core_types::{GeoTemporalEvent, IngestFailure, SignalSource, SourceFilters, TimeWindow};
use source_fixtures::FixtureSource;

pub enum IngestMsg {
    Loaded {
        events: Vec<GeoTemporalEvent>,
        failures: Vec<IngestFailure>,
    },
    Failed(String),
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

/// Spawn the ingest worker; results arrive on the returned channel and the
/// UI is woken via `wake` (a repaint request).
pub fn spawn(fixtures_dir: PathBuf, wake: impl Fn() + Send + 'static) -> mpsc::Receiver<IngestMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fixture-ingest".into())
        .spawn(move || {
            let msg = run(&fixtures_dir);
            let _ = tx.send(msg);
            wake();
        })
        .expect("spawn ingest thread");
    rx
}

fn run(fixtures_dir: &std::path::Path) -> IngestMsg {
    let source = match FixtureSource::from_dir(fixtures_dir) {
        Ok(s) => s,
        Err(e) => return IngestMsg::Failed(format!("reading {}: {e}", fixtures_dir.display())),
    };
    if source.files().is_empty() {
        return IngestMsg::Failed(format!(
            "no fixture files in {} — run `cargo run -p source-fixtures --bin generate-fixtures`",
            fixtures_dir.display()
        ));
    }

    // Fixtures span a fixed synthetic period; fetch everything.
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap(),
    );

    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(rt) => rt,
        Err(e) => return IngestMsg::Failed(format!("tokio runtime: {e}")),
    };
    let raws = match runtime.block_on(source.fetch(window, &SourceFilters::default())) {
        Ok(r) => r,
        Err(e) => return IngestMsg::Failed(format!("fixture fetch: {e}")),
    };

    let (events, failures) = storage::partition_normalized(&source, &raws);
    tracing::info!(
        events = events.len(),
        failures = failures.len(),
        "fixtures normalized"
    );
    IngestMsg::Loaded { events, failures }
}
