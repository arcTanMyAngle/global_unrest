//! Retention profiling harness (M8): how far back can the desktop hold data
//! before something degrades, and which resource gives out first.
//!
//! These are **timing measurements, not correctness gates** — CI has no
//! stable performance floor — so both tests are `#[ignore]`d and run by hand,
//! following the `chatter::observe_cost` convention:
//!
//! ```sh
//! cargo test -p global-signal-desktop --release --test retention_profile \
//!     -- --ignored --nocapture
//! ```
//!
//! Two axes, because one is not enough to answer the question honestly:
//!
//! * `profile_fixture_scale` walks the committed generator's day axis
//!   through the **real** pipeline — FixtureSource, normalize, DuckDB,
//!   queries. Faithful in shape, but the generator's 23 spots emit ~315
//!   events/day, ~300x below the ~100k/day online volume the docs cite, and
//!   pin cell cardinality at 23.
//! * `profile_online_scale` synthesizes at online rate over a realistic
//!   res-3 cell grid. Less faithful per record, but it is the only axis that
//!   reaches the volume the retention question is actually about.
//!
//! Point `profile_fixture_scale` at bigger generated spans with:
//!
//! ```sh
//! cargo run --release -p source-fixtures --bin generate-fixtures -- \
//!     --days 350 --out <dir>/events_350d.json
//! LES_PROFILE_FIXTURES=<dir> cargo test ... -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, LocationPrecision, LocationRole, SignalFamily, SignalSource,
    SourceFilters, SourceId, TimeWindow, event_id,
};
use source_fixtures::FixtureSource;
use storage::StorageHandle;

/// SplitMix64 — same generator family as the fixture generator and the
/// criterion bench, so synthetic volume is reproducible run to run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    (out, t.elapsed())
}

/// One printed report line, so every table in the docs can be traced back to
/// a real run.
struct Row {
    label: String,
    events: usize,
    buckets: usize,
    cells: usize,
    first_ingest: Duration,
    reingest: Duration,
    incremental: Duration,
    empty_tick: Duration,
    q_buckets: Duration,
    q_buckets_theme: Duration,
    q_points: Duration,
    q_histogram: Duration,
    q_vocab: Duration,
    q_detail: Duration,
    db_bytes: u64,
}

impl Row {
    fn header() -> String {
        format!(
            "{:>8} {:>9} {:>8} {:>6} | {:>9} {:>9} {:>9} {:>9} | {:>8} {:>10} {:>8} {:>8} {:>8} {:>7} | {:>8}",
            "case",
            "events",
            "buckets",
            "cells",
            "ingest",
            "re-ing",
            "incr",
            "empty",
            "bkts",
            "bkts+theme",
            "points",
            "hist",
            "vocab",
            "detail",
            "db MiB",
        )
    }

    fn line(&self) -> String {
        format!(
            "{:>8} {:>9} {:>8} {:>6} | {:>9.0} {:>9.0} {:>9.0} {:>9.0} | {:>8.1} {:>10.1} {:>8.1} {:>8.1} {:>8.1} {:>7.1} | {:>8.1}",
            self.label,
            self.events,
            self.buckets,
            self.cells,
            ms(self.first_ingest),
            ms(self.reingest),
            ms(self.incremental),
            ms(self.empty_tick),
            ms(self.q_buckets),
            ms(self.q_buckets_theme),
            ms(self.q_points),
            ms(self.q_histogram),
            ms(self.q_vocab),
            ms(self.q_detail),
            self.db_bytes as f64 / (1024.0 * 1024.0),
        )
    }
}

/// Ingest `events` into a fresh on-disk store and time every path the desktop
/// drives. `incremental_batch` is re-ingested on top of the loaded table —
/// that is the cost the running app pays on every cadence tick, and the
/// number that decides the retention ceiling.
fn measure(
    label: &str,
    events: Vec<GeoTemporalEvent>,
    incremental_batch: Vec<GeoTemporalEvent>,
) -> Row {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profile.duckdb");
    let store = StorageHandle::open(Some(db.clone()), Box::new(|| {})).unwrap();

    let n = events.len();
    let (report, first_ingest) = time(|| store.ingest(events.clone(), vec![]).wait().unwrap());
    assert_eq!(
        report.inserted, n,
        "{label}: unexpected dedup on first load"
    );

    // Restart simulation: every row is a duplicate, so this isolates the
    // dedup + rescore cost from the append cost.
    let (_, reingest) = time(|| store.ingest(events, vec![]).wait().unwrap());

    // One cadence tick's worth of genuinely new rows on a loaded table.
    let (_, incremental) = time(|| store.ingest(incremental_batch, vec![]).wait().unwrap());

    // A tick that brings nothing new. Whatever this costs is fixed overhead
    // paid on every cadence tick regardless of batch size: the full-table
    // dedup scan plus the full rescore in `rebuild_buckets`.
    let (_, empty_tick) = time(|| store.ingest(vec![], vec![]).wait().unwrap());

    let extent = store.time_extent().wait().unwrap().expect("extent");
    let (buckets, q_buckets) = time(|| store.query_buckets(extent, None).wait().unwrap());
    let (_, q_buckets_theme) = time(|| {
        store
            .query_buckets(extent, Some(vec!["PROTEST".to_string()]))
            .wait()
            .unwrap()
    });
    let (points, q_points) = time(|| {
        store
            .query_points(extent, None, None, 0.0, false)
            .wait()
            .unwrap()
    });
    let (_, q_histogram) = time(|| store.timeline_histogram().wait().unwrap());
    let (_, q_vocab) = time(|| store.theme_vocab().wait().unwrap());
    let cell = buckets
        .iter()
        .max_by_key(|b| b.attention_count + b.event_count)
        .expect("at least one bucket")
        .h3_cell;
    let (_, q_detail) = time(|| store.region_detail(cell, extent).wait().unwrap());

    let cells: std::collections::HashSet<u64> = buckets.iter().map(|b| b.h3_cell).collect();
    let _ = points;

    // Close the actor (Drop joins it) before sizing the file: DuckDB keeps
    // pages in a write-ahead log until the connection closes, so measuring a
    // live database reports near-zero and is simply wrong.
    drop(store);
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0)
        + std::fs::metadata(db.with_extension("duckdb.wal"))
            .map(|m| m.len())
            .unwrap_or(0);

    Row {
        label: label.to_string(),
        events: n,
        buckets: buckets.len(),
        cells: cells.len(),
        first_ingest,
        reingest,
        incremental,
        empty_tick,
        q_buckets,
        q_buckets_theme,
        q_points,
        q_histogram,
        q_vocab,
        q_detail,
        db_bytes,
    }
}

fn fixtures_dir() -> std::path::PathBuf {
    match std::env::var("LES_PROFILE_FIXTURES") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .canonicalize()
            .expect("fixtures directory must exist"),
    }
}

/// Load one generated fixture file through the real fetch/normalize path.
fn load_fixture_file(path: &std::path::Path) -> Vec<GeoTemporalEvent> {
    let source = FixtureSource::from_files(vec![path.to_path_buf()]);
    let window = TimeWindow::new(
        Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let raws = runtime
        .block_on(source.fetch(window, &SourceFilters::default()))
        .unwrap();
    let (events, _failures) = storage::partition_normalized(&source, &raws);
    events
}

/// Clone `n` events forward in time with fresh ids, so they insert rather than
/// dedup — a stand-in for one cadence tick of genuinely new records.
fn shift_batch(events: &[GeoTemporalEvent], n: usize) -> Vec<GeoTemporalEvent> {
    let max_ts = events.iter().map(|e| e.ts_utc).max().unwrap();
    events
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, e)| {
            let mut e = e.clone();
            e.source_event_id = format!("tick-{i}");
            e.id = event_id(e.source, &e.source_event_id);
            e.ts_utc = max_ts + chrono::Duration::seconds(60 + i as i64);
            e
        })
        .collect()
}

#[test]
#[ignore = "timing measurement, not a correctness gate"]
fn profile_fixture_scale() {
    let dir = fixtures_dir();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for d in [dir.clone(), dir.join("generated")] {
        if !d.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            let is_events = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("events_") && n.ends_with(".json"));
            if is_events {
                files.push(p);
            }
        }
    }
    files.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    assert!(
        !files.is_empty(),
        "no events_*.json under {}",
        dir.display()
    );

    println!("\n=== fixture-generator day axis (real pipeline) ===");
    println!("source: {}", dir.display());
    println!("{}", Row::header());
    for path in &files {
        let events = load_fixture_file(path);
        let batch = shift_batch(&events, 200);
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .replace("events_", "");
        println!("{}", measure(&label, events, batch).line());
    }
}

/// Res-3 cells spread over the populated latitudes, so bucket cardinality is
/// realistic rather than the generator's 23 fixed spots. Res 3 is coarse
/// (~12,400 km² per cell, ~41k cells worldwide), so a few thousand is the
/// honest ceiling for "cells with news in them".
fn land_cells(target: usize) -> Vec<(f64, f64, u64)> {
    let mut out: Vec<(f64, f64, u64)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut lat = -55.0f64;
    while lat < 70.0 && out.len() < target {
        let mut lon = -180.0f64;
        while lon < 180.0 && out.len() < target {
            if let Ok(cell) = geo_utils::cell_for_latlon(lat, lon, core_types::H3_RESOLUTION)
                && seen.insert(cell)
            {
                out.push((lat, lon, cell));
            }
            lon += 1.5;
        }
        lat += 1.5;
    }
    out
}

/// Online-rate synthesis: `per_day` events over `days`, spread across `cells`
/// distinct res-3 cells with a fixture-like 3:1 attention/event mix.
fn synth_events(
    days: i64,
    per_day: usize,
    cells: &[(f64, f64, u64)],
    seed: u64,
    tag: &str,
) -> Vec<GeoTemporalEvent> {
    let outlets = [
        "globalwire.example",
        "daily-ledger.example",
        "worldpost.example",
        "signal-times.example",
        "cityherald.example",
    ];
    let themes = ["PROTEST", "FLOOD", "TRANSPORT", "ELECTIONS", "LABOR"];
    let anchor = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap() - chrono::Duration::days(days);
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(days as usize * per_day);
    for day in 0..days {
        for i in 0..per_day {
            let r = rng.next_u64();
            let attention = !r.is_multiple_of(4);
            // Zipf-ish concentration: most volume in a minority of cells,
            // like real coverage, rather than uniform across the grid.
            let idx = {
                let a = rng.below(cells.len() as u64) as usize;
                let b = rng.below(cells.len() as u64) as usize;
                a.min(b)
            };
            let (lat, lon, cell) = cells[idx];
            let ts = anchor
                + chrono::Duration::days(day)
                + chrono::Duration::seconds(rng.below(86_400) as i64);
            let sid = format!("{tag}-{day}-{i}");
            let n_outlets = 1 + (r % 3) as usize;
            out.push(GeoTemporalEvent {
                id: event_id(SourceId::Fixtures, &sid),
                source: SourceId::Fixtures,
                source_event_id: sid,
                family: if attention {
                    SignalFamily::MediaAttention
                } else {
                    SignalFamily::RecordedEvent
                },
                kind: if attention {
                    EventKind::NewsAttention
                } else {
                    EventKind::Protest
                },
                location_role: if attention {
                    LocationRole::MentionedPlace
                } else {
                    LocationRole::EventSite
                },
                themes: vec![themes[(r % themes.len() as u64) as usize].to_string()],
                ts_utc: ts,
                ingested_at: ts,
                lat,
                lon,
                location_precision: if r.is_multiple_of(5) {
                    LocationPrecision::Country
                } else {
                    LocationPrecision::City
                },
                location_confidence: 0.85,
                country_iso: "ZZZ".into(),
                admin1: None,
                h3_cell: cell,
                volume_count: (r % 40) as u32 + 1,
                distinct_source_count: (r % 4) as u32 + 1,
                severity: (!attention).then_some(((r % 10) as f32) * 0.1),
                headline: Some("[synthetic] profiling record".into()),
                outlet_domains: outlets
                    .iter()
                    .take(n_outlets)
                    .map(|s| (*s).to_string())
                    .collect(),
                urls: vec![],
            });
        }
    }
    out
}

#[test]
#[ignore = "timing measurement, not a correctness gate"]
fn profile_online_scale() {
    let cells = land_cells(1500);
    println!(
        "\n=== online-rate axis (synthesized, {} res-3 cells) ===",
        cells.len()
    );
    println!("{}", Row::header());
    // ~100k events/day is the online GDELT volume the docs cite. Days are the
    // retention axis; 30d is the shortest retention the UI offers.
    let per_day = std::env::var("LES_PROFILE_PER_DAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000usize);
    let days: Vec<i64> = std::env::var("LES_PROFILE_DAYS")
        .ok()
        .map(|s| s.split(',').filter_map(|d| d.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 4, 10]);
    for days in days {
        let events = synth_events(days, per_day, &cells, 42, "syn");
        // A 15-minute GDELT tick at online rate.
        let batch = synth_events(1, (per_day / 96).max(1), &cells, 7, "tick");
        println!("{}", measure(&format!("{days}d"), events, batch).line());
    }
}
