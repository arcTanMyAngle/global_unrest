//! Golden test for the GDELT Events 2.0 path against a committed **synthetic**
//! export (`tests/data/events_sample.export.CSV`, 61 tab-separated columns).
//! Offline acceptance for M3 step 2 — never touches the network.
//!
//! The fixture exercises every branch: kept unrest kinds (protest / conflict /
//! disruption), a skipped cooperation row, a skipped ungeocoded row, a
//! country-precision event, and a malformed (bad-coordinate) row that must be
//! reported rather than dropped. It is also zipped in memory to prove the
//! `.CSV.zip` unpack path round-trips.

use core_types::{EventKind, LocationPrecision, NormalizeError, SourceId, event_id};
use source_gdelt::events;

fn sample_csv() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/events_sample.export.CSV");
    std::fs::read_to_string(path).expect("committed Events sample fixture")
}

#[test]
fn events_export_normalizes_with_skips_and_failures() {
    let csv = sample_csv();
    assert_eq!(events::rows(&csv).count(), 7, "fixture has seven rows");

    let mut events_out = Vec::new();
    let mut skipped = 0usize;
    let mut failures = Vec::new();
    for row in events::rows(&csv) {
        match events::normalize(row) {
            Ok(evs) if evs.is_empty() => skipped += 1,
            Ok(mut evs) => events_out.append(&mut evs),
            Err(e) => failures.push(e),
        }
    }

    // 4 kept (protest, conflict, disruption, country conflict);
    // 2 skipped (cooperation, ungeocoded); 1 failed (bad coordinates).
    assert_eq!(events_out.len(), 4);
    assert_eq!(skipped, 2);
    assert_eq!(failures.len(), 1);
    assert!(matches!(
        failures[0],
        NormalizeError::InvalidCoordinates { .. }
    ));

    // Every kept record is a discrete GDELT event (never NewsAttention).
    for e in &events_out {
        assert_eq!(e.source, SourceId::Gdelt);
        assert!(e.kind.is_discrete_event());
        assert!(e.themes.is_empty());
        assert!(e.severity.is_some());
    }

    let kinds: Vec<EventKind> = events_out.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::Protest,
            EventKind::Conflict,
            EventKind::Disruption,
            EventKind::Conflict,
        ]
    );

    // Spot-check the Paris protest end to end.
    let paris = &events_out[0];
    assert_eq!(paris.source_event_id, "1000001");
    assert_eq!(paris.id, event_id(SourceId::Gdelt, "1000001"));
    assert_eq!(paris.country_iso, "FRA");
    assert_eq!(paris.location_precision, LocationPrecision::City);
    assert_eq!(paris.outlet_domains, vec!["globalwire.example"]);

    // The country-precision Russia row never renders as a point.
    let russia = &events_out[3];
    assert_eq!(russia.country_iso, "RUS");
    assert_eq!(russia.location_precision, LocationPrecision::Country);
    assert!(!russia.location_precision.renders_as_point());
    assert_eq!(russia.admin1, None);
}

#[test]
fn csv_zip_unpack_roundtrips() {
    let csv = sample_csv();
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("20260620081500.export.CSV", opts).unwrap();
        std::io::Write::write_all(&mut w, csv.as_bytes()).unwrap();
        w.finish().unwrap();
    }
    let back = events::unzip_csv(&buf).unwrap();
    assert_eq!(back, csv);
    assert_eq!(events::rows(&back).count(), 7);
}
