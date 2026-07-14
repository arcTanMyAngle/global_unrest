//! Golden test for the GDELT DOC 2.0 path against a committed **synthetic**
//! `artlist` response (`tests/data/doc_artlist_sample.json`). This is the
//! offline acceptance harness for M3 step 1 — it never touches the network.
//!
//! The fixture mirrors the real DOC 2.0 JSON shape (all synthetic; `.example`
//! domains, `[synthetic]` titles). One record carries an unmapped
//! `sourcecountry` to prove normalization fails **per record** rather than
//! aborting the batch.

use core_types::{EventKind, LocationPrecision, NormalizeError, RawRecord, SourceId, event_id};
use source_gdelt::doc;

fn sample_body() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/doc_artlist_sample.json");
    std::fs::read_to_string(path).expect("committed DOC sample fixture")
}

#[test]
fn doc_artlist_normalizes_with_per_record_failures() {
    let articles = doc::articles(&sample_body()).unwrap();
    assert_eq!(articles.len(), 5, "fixture has five articles");

    // Stamp a query theme onto every article, as the fetcher does, then
    // normalize each, partitioning successes from failures.
    let mut events = Vec::new();
    let mut failures = Vec::new();
    for mut article in articles {
        doc::stamp_themes(&mut article, &["PROTEST".into()]);
        match doc::normalize(&article) {
            Ok(e) => events.push(e),
            Err(e) => failures.push(e),
        }
    }

    // Four valid countries normalize; the "Atlantis" record fails, logged not
    // dropped.
    assert_eq!(events.len(), 4);
    assert_eq!(failures.len(), 1);
    assert!(matches!(
        failures[0],
        NormalizeError::InvalidValue {
            field: "sourcecountry",
            ..
        }
    ));

    // Every event is a country-precision attention observation from GDELT.
    for e in &events {
        assert_eq!(e.source, SourceId::Gdelt);
        assert_eq!(e.kind, EventKind::NewsAttention);
        assert_eq!(e.location_precision, LocationPrecision::Country);
        assert!(!e.location_precision.renders_as_point());
        assert_eq!(e.themes, vec!["protest"]);
        assert_eq!(e.article_count, 1);
        assert_eq!(e.urls.len(), 1);
    }

    // Spot-check the first record end to end.
    let paris = &events[0];
    assert_eq!(paris.country_iso, "FRA");
    assert_eq!(paris.source_event_id, "https://globalwire.example/a/1001");
    assert_eq!(paris.id, event_id(SourceId::Gdelt, &paris.source_event_id));
    assert_eq!(paris.outlet_domains, vec!["globalwire.example"]);

    // ISO codes across the batch (order preserved).
    let isos: Vec<&str> = events.iter().map(|e| e.country_iso.as_str()).collect();
    assert_eq!(isos, ["FRA", "KEN", "IDN", "USA"]);
}

#[test]
fn reingest_is_idempotent_by_url() {
    // The same article seen twice must produce the same stable id (dedup key).
    let articles = doc::articles(&sample_body()).unwrap();
    let a = doc::normalize(&articles[0]).unwrap();
    let b = doc::normalize(&articles[0]).unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(a.source_event_id, b.source_event_id);
}

#[test]
fn rejects_non_doc_record() {
    // The trait dispatch must refuse foreign payloads.
    use core_types::SignalSource;
    let src = source_gdelt::GdeltSource::new().unwrap();
    let err = src
        .normalize(&RawRecord::GdeltEventCsv("0\t1\t2".into()))
        .unwrap_err();
    assert!(matches!(err, NormalizeError::InvalidValue { .. }));
}
