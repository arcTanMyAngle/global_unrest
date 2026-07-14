//! Headless end-to-end test of the M1 offline pipeline:
//! fixtures → fetch → normalize → DuckDB → queries.
//!
//! This is the acceptance test for Milestone 1's data path and runs without
//! a GPU or window. It uses the real committed fixtures.

use chrono::{TimeZone, Utc};
use core_types::{LocationPrecision, SignalSource, SourceFilters, TimeWindow};
use source_fixtures::FixtureSource;
use storage::StorageHandle;

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures directory must exist (committed to the repo)")
}

#[test]
fn full_offline_pipeline() {
    let source = FixtureSource::from_dir(&fixtures_dir()).unwrap();
    assert!(
        source.files().len() >= 3,
        "expected sample + generated fixture files, found {:?}",
        source.files()
    );

    // --- fetch (async trait; drive with a tiny runtime) ---
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
    assert!(raws.len() > 10_000, "got {} raw records", raws.len());

    // --- normalize: failures are partitioned, never dropped ---
    let (events, failures) = storage::partition_normalized(&source, &raws);
    assert!(events.len() > 10_000, "got {} events", events.len());
    assert_eq!(
        failures.len(),
        2,
        "the generator plants exactly 2 malformed records"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.reason.contains("coordinates out of range")),
        "bad-coords record must fail with a coordinate error"
    );
    assert!(
        failures.iter().any(|f| f.reason.contains("shape")),
        "shapeless record must fail with a shape error"
    );

    // --- store in DuckDB (temp file, like the real app) ---
    let dir = tempfile::tempdir().unwrap();
    let store = StorageHandle::open(Some(dir.path().join("e2e.duckdb")), Box::new(|| {})).unwrap();
    let report = store
        .ingest(events.clone(), failures)
        .wait()
        .expect("ingest");
    assert_eq!(report.inserted, events.len());
    assert_eq!(report.duplicates, 0);
    assert_eq!(report.failures, 2);

    let (log_total, log_rows) = store.ingest_log(10).wait().unwrap();
    assert_eq!(log_total, 2);
    assert!(!log_rows.is_empty());

    // --- extent covers the full generated span (35 days) ---
    let (min_ts, max_ts) = store.time_extent().wait().unwrap().expect("extent");
    let span_days = (max_ts - min_ts) as f64 / 86_400.0;
    assert!(
        (34.0..=36.0).contains(&span_days),
        "fixture span was {span_days:.1} days"
    );

    // --- stored buckets must equal the analytics reference, scores included
    // (this exercises the events → DB → ScoreEvent read-back roundtrip) ---
    let buckets = store.query_buckets((min_ts, max_ts), None).wait().unwrap();
    let reference = analytics::aggregate_buckets(&events);
    assert_eq!(buckets.len(), reference.len(), "bucket count mismatch");
    for (stored, rust) in buckets.iter().zip(&reference) {
        assert_eq!(stored, rust, "stored bucket diverged from reference");
    }
    let total_counted: u64 = buckets
        .iter()
        .map(|b| u64::from(b.event_count) + u64::from(b.attention_count))
        .sum();
    assert_eq!(total_counted as usize, events.len());

    // --- M2 scoring: every component stored per bucket, all in [0, 1] ---
    for b in &buckets {
        for v in [
            b.attention_score,
            b.unrest_score,
            b.spike_score,
            b.combined_score,
        ] {
            assert!((0.0..=1.0).contains(&v), "score out of range: {v}");
        }
        assert!(b.baseline >= 0.0);
    }

    // Cold-start rule: exactly the buckets with under MIN_BASELINE_DAYS of
    // history behind them are flagged, and their spike is forced neutral.
    let start_day = min_ts.div_euclid(86_400);
    let rel_day = |b: &core_types::RegionBucket| b.bucket_start.div_euclid(86_400) - start_day;
    assert!(buckets.iter().any(|b| b.spike_cold_start));
    assert!(buckets.iter().any(|b| !b.spike_cold_start));
    for b in &buckets {
        assert_eq!(b.spike_cold_start, rel_day(b) < 7, "day {}", rel_day(b));
        if b.spike_cold_start {
            assert_eq!(b.spike_score, 0.5);
        }
    }

    // The scripted Paris spike (fixture days 20–23, ~6× attention) must
    // register clearly against its warm 28-day baseline.
    let paris = geo_utils::cell_for_latlon(48.8566, 2.3522, 3).unwrap();
    let paris_spike_max = buckets
        .iter()
        .filter(|b| b.h3_cell == paris && (20..=23).contains(&rel_day(b)))
        .map(|b| b.spike_score)
        .fold(0.0f32, f32::max);
    assert!(
        paris_spike_max > 0.8,
        "scripted spike too weak: {paris_spike_max}"
    );

    // Baselines are persisted with a full trailing window for Paris.
    let paris_base = store.baselines(paris).wait().unwrap();
    assert_eq!(paris_base.len(), 4);
    assert!(paris_base.iter().all(|r| r.sample_days == 28));
    assert!(paris_base.iter().any(|r| r.baseline > 0.0));

    // --- precision rendering contract: no coarse rows come back as points ---
    let points = store
        .query_points((min_ts, max_ts), None, None, 0.0)
        .wait()
        .unwrap();
    assert!(!points.is_empty());
    assert!(
        points.iter().all(|p| matches!(
            p.precision,
            LocationPrecision::City | LocationPrecision::Exact
        )),
        "country/admin1 records must never render as points"
    );
    let coarse_events = events
        .iter()
        .filter(|e| !e.location_precision.renders_as_point())
        .count();
    assert!(
        coarse_events > 0,
        "fixtures must include centroid-precision records"
    );
    assert!(points.len() < events.len());

    // --- region inspector query on the busiest cell ---
    let busiest = buckets
        .iter()
        .max_by_key(|b| b.attention_count + b.event_count)
        .unwrap();
    let detail = store
        .region_detail(busiest.h3_cell, (min_ts, max_ts))
        .wait()
        .unwrap();
    let total: u32 = detail.counts_by_kind.iter().map(|(_, c)| *c).sum();
    assert!(total > 0);
    assert!(!detail.top_themes.is_empty());
    assert!(detail.distinct_outlets > 0);
    assert!(detail.mean_confidence > 0.0);

    // --- idempotent re-ingest (restart simulation) ---
    let report2 = store.ingest(events, vec![]).wait().unwrap();
    assert_eq!(report2.inserted, 0);
    assert!(report2.duplicates > 10_000);
}
