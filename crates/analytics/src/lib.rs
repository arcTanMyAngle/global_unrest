//! Aggregation (M1) and transparent scoring / baselines / spike detection (M2).
//!
//! Everything here is a pure function over domain types — no I/O, no state.
//! `aggregate_buckets` is also the reference implementation that the storage
//! crate's SQL `GROUP BY` is integration-tested against.

pub mod baseline;
pub mod scoring;

use std::collections::BTreeMap;

use core_types::{GeoTemporalEvent, RegionBucket, bucket_start_epoch};

/// Every scoring constant, named (docs/SCORING.md). Nothing in the score
/// functions is a magic number.
pub mod weights {
    /// combined = 0.40·attention + 0.45·unrest + 0.15·spike.
    pub const ATTENTION: f64 = 0.40;
    pub const UNREST: f64 = 0.45;
    pub const SPIKE: f64 = 0.15;

    /// Recency decay half-life (attention and unrest recency terms).
    pub const RECENCY_HALF_LIFE_SECS: f64 = 86_400.0; // 24 h

    /// Attention volume saturates at this many articles per bucket.
    pub const ATTENTION_ARTICLE_SATURATION: f64 = 100.0;
    /// Source-diversity weight saturates at this many distinct outlets.
    pub const DIVERSITY_OUTLET_SATURATION: f64 = 8.0;
    /// Theme weight: buckets touching a high-signal theme vs. the rest.
    pub const THEME_WEIGHT_HIGH: f64 = 1.0;
    pub const THEME_WEIGHT_BASE: f64 = 0.6;
    /// Unrest-relevant themes (compared against lowercased source themes).
    pub const HIGH_SIGNAL_THEMES: &[&str] = &[
        "protest",
        "conflict",
        "riot",
        "unrest",
        "violence",
        "elections",
        "security",
        "displacement",
        "air_defense",
        "strike",
        "coup",
    ];

    /// Unrest term weights — must sum to 1 so unrest stays in [0, 1].
    pub const UNREST_EVENT_COUNT: f64 = 0.30;
    pub const UNREST_EVENT_TYPE: f64 = 0.25;
    pub const UNREST_RECENCY: f64 = 0.10;
    pub const UNREST_SEVERITY: f64 = 0.20;
    pub const UNREST_PRECISION: f64 = 0.15;
    /// Unrest count term saturates at this many events per bucket.
    pub const EVENT_COUNT_SATURATION: f64 = 10.0;

    /// Per-kind weights for the unrest event-type term.
    pub const KIND_CONFLICT: f64 = 1.0;
    pub const KIND_PROTEST: f64 = 0.7;
    pub const KIND_DISRUPTION: f64 = 0.5;
    pub const KIND_OTHER: f64 = 0.3;

    /// Spike log-ratio smoothing (half a record) and clamp span: ±3 doublings
    /// (⅛×–8× baseline) map onto [0, 1] with 0.5 neutral.
    pub const SPIKE_EPSILON: f64 = 0.5;
    pub const SPIKE_LOG2_SPAN: f64 = 3.0;
    pub const SPIKE_NEUTRAL: f64 = 0.5;

    /// Baseline = trailing median over this many days…
    pub const BASELINE_WINDOW_DAYS: u32 = 28;
    /// …and below this much history a bucket is cold-start: neutral spike,
    /// low-confidence badge in the UI.
    pub const MIN_BASELINE_DAYS: u32 = 7;
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

    #[test]
    fn unrest_term_weights_sum_to_one() {
        let sum = weights::UNREST_EVENT_COUNT
            + weights::UNREST_EVENT_TYPE
            + weights::UNREST_RECENCY
            + weights::UNREST_SEVERITY
            + weights::UNREST_PRECISION;
        assert!((sum - 1.0).abs() < 1e-12);
    }
}
