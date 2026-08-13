//! Regenerates `services/api`'s committed integration-test fixture
//! snapshot (M7 item 5) under `services/api/tests/fixtures/` — deterministic
//! content (only the snapshot's timestamp-derived version changes between
//! runs), committed like `source-fixtures`'s own `generate-fixtures`.
//! Re-run this after any change to the events/region_buckets Parquet
//! schema that the committed snapshot would otherwise go stale against.
//!
//! Hand-crafts `GeoTemporalEvent`s directly rather than going through
//! `FixtureSource`: `FixtureSource::normalize_*` always tags rows
//! `SourceId::Fixtures` regardless of which real source's shape a record
//! mimics (by design — fixtures never claim to *be* a live source), so it
//! can't produce a genuine `source = 'acled'` row. The api's `/events`
//! ACLED-exclusion filter needs one to be exercised meaningfully.
//!
//! `cargo run -p workers --example gen_api_test_fixture`

use chrono::{TimeZone, Utc};
use core_types::{EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, SourceId, event_id};
use storage::StorageHandle;

fn h3(lat: f64, lon: f64) -> u64 {
    geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).expect("known-good test coordinates")
}

fn event(
    source: SourceId,
    source_event_id: &str,
    kind: EventKind,
    ts: chrono::DateTime<Utc>,
    lat: f64,
    lon: f64,
    precision: LocationPrecision,
    country_iso: &str,
    headline: &str,
) -> GeoTemporalEvent {
    GeoTemporalEvent {
        id: event_id(source, source_event_id),
        source,
        source_event_id: source_event_id.to_string(),
        kind,
        themes: vec!["synthetic".to_string()],
        ts_utc: ts,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision: precision,
        location_confidence: 0.8,
        country_iso: country_iso.to_string(),
        admin1: None,
        h3_cell: h3(lat, lon),
        article_count: 1,
        distinct_source_count: 1,
        severity: None,
        headline: Some(format!("[synthetic] {headline}")),
        outlet_domains: vec!["les-api-test.example".to_string()],
        urls: vec![],
    }
}

fn main() -> anyhow::Result<()> {
    let d = |y, m, day, h| Utc.with_ymd_and_hms(y, m, day, h, 0, 0).unwrap();

    let events = vec![
        // ACLED, city precision — excluded from /events by default; visible
        // with LES_API_ALLOW_ACLED=1. Not redistributable data in reality,
        // but this is synthetic test fixture content, not real ACLED data.
        event(
            SourceId::Acled,
            "acled-city-1",
            EventKind::Protest,
            d(2026, 6, 20, 10),
            48.8566,
            2.3522,
            LocationPrecision::City,
            "FRA",
            "Synthetic protest, Paris",
        ),
        event(
            SourceId::Acled,
            "acled-city-2",
            EventKind::Conflict,
            d(2026, 6, 20, 14),
            -1.2921,
            36.8219,
            LocationPrecision::City,
            "KEN",
            "Synthetic clash, Nairobi",
        ),
        // ACLED, admin1 precision — never a point row regardless of the
        // ACLED filter (precision-rendering contract), but still folds into
        // /buckets' aggregate counts.
        event(
            SourceId::Acled,
            "acled-admin1-1",
            EventKind::Disruption,
            d(2026, 6, 21, 8),
            -6.2088,
            106.8456,
            LocationPrecision::Admin1,
            "IDN",
            "Synthetic disruption, Jakarta area",
        ),
        // GDELT, city precision — always visible in /events. Three of them,
        // spread across three days, gives /events pagination something real
        // to page over.
        event(
            SourceId::Gdelt,
            "gdelt-city-1",
            EventKind::NewsAttention,
            d(2026, 6, 20, 9),
            48.8566,
            2.3522,
            LocationPrecision::City,
            "FRA",
            "Synthetic coverage, Paris",
        ),
        event(
            SourceId::Gdelt,
            "gdelt-city-2",
            EventKind::NewsAttention,
            d(2026, 6, 21, 9),
            -1.2921,
            36.8219,
            LocationPrecision::City,
            "KEN",
            "Synthetic coverage, Nairobi",
        ),
        event(
            SourceId::Gdelt,
            "gdelt-city-3",
            EventKind::NewsAttention,
            d(2026, 6, 22, 9),
            -6.2088,
            106.8456,
            LocationPrecision::City,
            "IDN",
            "Synthetic coverage, Jakarta",
        ),
        // GDELT, country precision — excluded from /events by the existing
        // precision filter (unrelated to the ACLED filter).
        event(
            SourceId::Gdelt,
            "gdelt-country-1",
            EventKind::NewsAttention,
            d(2026, 6, 21, 12),
            61.5240,
            105.3188,
            LocationPrecision::Country,
            "RUS",
            "Synthetic country-level coverage",
        ),
    ];

    let store = StorageHandle::open(None, Box::new(|| {}))
        .map_err(|e| anyhow::anyhow!("open in-memory store: {e}"))?;
    let report = store
        .ingest(events, Vec::new())
        .wait()
        .map_err(|e| anyhow::anyhow!("ingest: {e}"))?;
    println!("ingested: {report:?}");

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let publish_root = workspace_root.join("services/api/tests/fixtures");
    if publish_root.exists() {
        std::fs::remove_dir_all(&publish_root)?;
    }
    let snap = store
        .publish_snapshot(publish_root.clone(), None)
        .wait()
        .map_err(|e| anyhow::anyhow!("publish: {e}"))?;
    println!("published {} to {}", snap.version, publish_root.display());
    Ok(())
}
