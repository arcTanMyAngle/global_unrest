//! M3 acceptance: re-fetching overlapping GDELT windows must not double-count.
//!
//! The online loop polls overlapping DOC windows and the latest Events dump, so
//! the same records recur across cycles. Ingest dedups by the deterministic
//! event id (DOC: article URL; Events: GLOBALEVENTID), so a re-fetch inserts
//! nothing new and the region-bucket aggregation is unchanged. This test drives
//! the real GdeltSource normalization over the committed synthetic fixtures and
//! the real storage actor — no network.

use core_types::{RawRecord, SignalSource};
use source_gdelt::{GdeltSource, doc, events};
use storage::StorageHandle;

fn gdelt_fixture_events() -> Vec<core_types::GeoTemporalEvent> {
    let src = GdeltSource::new().unwrap();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/source-gdelt/tests/data");

    // DOC attention records (skip the deliberately-unmapped country).
    let doc_body = std::fs::read_to_string(base.join("doc_artlist_sample.json")).unwrap();
    let mut raws: Vec<RawRecord> = doc::articles(&doc_body)
        .unwrap()
        .into_iter()
        .map(|mut a| {
            doc::stamp_themes(&mut a, &["protest".into()]);
            RawRecord::GdeltDocJson(a)
        })
        .collect();

    // Events rows.
    let csv = std::fs::read_to_string(base.join("events_sample.export.CSV")).unwrap();
    raws.extend(events::rows(&csv).map(|r| RawRecord::GdeltEventCsv(r.to_owned())));

    // Normalize via the real adapter, keeping only the successes.
    raws.iter()
        .filter_map(|r| src.normalize(r).ok())
        .flatten()
        .collect()
}

#[test]
fn refetch_deduplicates_and_leaves_buckets_unchanged() {
    let events = gdelt_fixture_events();
    assert!(events.len() >= 5, "fixtures should yield several events");

    let store = StorageHandle::open(None, Box::new(|| {})).unwrap();

    // First ingest: everything is new.
    let first = store.ingest(events.clone(), vec![]).wait().unwrap();
    assert_eq!(first.inserted, events.len());
    assert_eq!(first.duplicates, 0);

    let (min, max) = store.time_extent().wait().unwrap().unwrap();
    let buckets_before = store.query_buckets((min, max), None).wait().unwrap();
    let total_before: u64 = buckets_before
        .iter()
        .map(|b| u64::from(b.event_count) + u64::from(b.attention_count))
        .sum();

    // Re-fetch the same window (simulated): identical ids ⇒ all duplicates.
    let second = store.ingest(events.clone(), vec![]).wait().unwrap();
    assert_eq!(second.inserted, 0, "re-fetch must insert nothing new");
    assert_eq!(second.duplicates, events.len());

    // Aggregation is byte-for-byte unchanged after the re-fetch.
    let buckets_after = store.query_buckets((min, max), None).wait().unwrap();
    let total_after: u64 = buckets_after
        .iter()
        .map(|b| u64::from(b.event_count) + u64::from(b.attention_count))
        .sum();
    assert_eq!(buckets_before, buckets_after, "buckets changed on re-fetch");
    assert_eq!(total_before, total_after);
    assert_eq!(total_after as usize, events.len());
}
