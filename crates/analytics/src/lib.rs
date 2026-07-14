//! Aggregation (M1) and transparent scoring / baselines / spike detection (M2).
//!
//! Everything here is a pure function over domain types — no I/O, no state.
//! `aggregate_buckets` is also the reference implementation that the storage
//! crate's SQL `GROUP BY` is integration-tested against.

use std::collections::BTreeMap;

use core_types::{GeoTemporalEvent, RegionBucket, bucket_start_epoch};

/// Combined-signal weights (docs/SCORING.md). Named constants, not magic
/// numbers; the M2 scoring functions will consume these.
pub mod weights {
    pub const ATTENTION: f64 = 0.40;
    pub const UNREST: f64 = 0.45;
    pub const SPIKE: f64 = 0.15;
}

/// Aggregate events into (H3 res-3 cell × 6-hour bucket) counts.
///
/// Deterministic output order (cell, then bucket start). Attention
/// observations and discrete events are counted separately — see
/// docs/DATA_MODEL.md "Counting semantics".
pub fn aggregate_buckets(events: &[GeoTemporalEvent]) -> Vec<RegionBucket> {
    let mut map: BTreeMap<(u64, i64), RegionBucket> = BTreeMap::new();
    for ev in events {
        let key = (ev.h3_cell, bucket_start_epoch(ev.ts_utc.timestamp()));
        let bucket = map.entry(key).or_insert(RegionBucket {
            h3_cell: key.0,
            bucket_start: key.1,
            event_count: 0,
            attention_count: 0,
            article_count: 0,
            source_count: 0,
        });
        if ev.kind.is_attention() {
            bucket.attention_count += 1;
        } else {
            bucket.event_count += 1;
        }
        bucket.article_count += u64::from(ev.article_count);
        bucket.source_count += u64::from(ev.distinct_source_count);
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use core_types::{BUCKET_SECS, EventKind, LocationPrecision, SourceId, event_id};

    fn ev(kind: EventKind, cell: u64, hour: u32, articles: u32, sources: u32) -> GeoTemporalEvent {
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, hour, 15, 0).unwrap();
        GeoTemporalEvent {
            id: event_id(SourceId::Fixtures, &format!("{kind:?}-{cell}-{hour}")),
            source: SourceId::Fixtures,
            source_event_id: "x".into(),
            kind,
            themes: vec![],
            ts_utc: ts,
            ingested_at: ts,
            lat: 0.0,
            lon: 0.0,
            location_precision: LocationPrecision::City,
            location_confidence: 0.9,
            country_iso: "UNK".into(),
            admin1: None,
            h3_cell: cell,
            article_count: articles,
            distinct_source_count: sources,
            severity: None,
            headline: None,
            outlet_domains: vec![],
            urls: vec![],
        }
    }

    #[test]
    fn aggregates_by_cell_and_six_hour_bucket() {
        // Hand-computed: hours 1 and 5 share bucket 00–06; hour 7 is 06–12.
        let events = vec![
            ev(EventKind::NewsAttention, 10, 1, 4, 2),
            ev(EventKind::Protest, 10, 5, 3, 1),
            ev(EventKind::NewsAttention, 10, 7, 5, 3),
            ev(EventKind::Conflict, 20, 1, 1, 1),
        ];
        let buckets = aggregate_buckets(&events);
        assert_eq!(buckets.len(), 3);

        let day = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        // Deterministic order: (cell 10, bucket 0), (cell 10, bucket 1), (cell 20, bucket 0).
        assert_eq!((buckets[0].h3_cell, buckets[0].bucket_start), (10, day));
        assert_eq!(buckets[0].attention_count, 1);
        assert_eq!(buckets[0].event_count, 1);
        assert_eq!(buckets[0].article_count, 4 + 3);
        assert_eq!(buckets[0].source_count, 2 + 1);

        assert_eq!(
            (buckets[1].h3_cell, buckets[1].bucket_start),
            (10, day + BUCKET_SECS)
        );
        assert_eq!(buckets[1].attention_count, 1);
        assert_eq!(buckets[1].event_count, 0);

        assert_eq!(buckets[2].h3_cell, 20);
        assert_eq!(buckets[2].event_count, 1);
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(aggregate_buckets(&[]).is_empty());
    }

    #[test]
    fn combined_weights_sum_to_one() {
        assert!((weights::ATTENTION + weights::UNREST + weights::SPIKE - 1.0).abs() < 1e-12);
    }
}
