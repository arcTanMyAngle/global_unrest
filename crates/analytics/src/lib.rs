//! Aggregation (M1) and transparent scoring / baselines / spike detection (M2).
//!
//! Everything here is a pure function over domain types — no I/O, no state.
//! `aggregate_buckets` is also the reference implementation that the storage
//! crate's SQL `GROUP BY` is integration-tested against.

pub mod baseline;
pub mod scoring;

use std::collections::{BTreeMap, HashSet};

use core_types::{BUCKET_SECS, EventKind, GeoTemporalEvent, RegionBucket, bucket_start_epoch};

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

/// Aggregate events into fully scored (H3 res-3 cell × 6-hour bucket) rows.
/// This is [`score_buckets`] over a `GeoTemporalEvent` slice — the reference
/// implementation the storage crate persists and is tested against.
pub fn aggregate_buckets(events: &[GeoTemporalEvent]) -> Vec<RegionBucket> {
    let view: Vec<ScoreEvent> = events.iter().map(ScoreEvent::from).collect();
    score_buckets(&view).buckets
}

/// The slice of an event that bucket scoring consumes. Storage reconstructs
/// these from the `events` table; in-memory callers convert from
/// [`GeoTemporalEvent`].
#[derive(Debug, Clone)]
pub struct ScoreEvent {
    pub h3_cell: u64,
    pub ts_epoch_s: i64,
    pub kind: EventKind,
    pub article_count: u32,
    pub distinct_source_count: u32,
    pub location_confidence: f32,
    pub severity: Option<f32>,
    /// City/Exact precision (the precision rendering contract predicate).
    pub renders_as_point: bool,
    pub themes: Vec<String>,
    pub outlet_domains: Vec<String>,
}

impl From<&GeoTemporalEvent> for ScoreEvent {
    fn from(ev: &GeoTemporalEvent) -> Self {
        Self {
            h3_cell: ev.h3_cell,
            ts_epoch_s: ev.ts_utc.timestamp(),
            kind: ev.kind,
            article_count: ev.article_count,
            distinct_source_count: ev.distinct_source_count,
            location_confidence: ev.location_confidence,
            severity: ev.severity,
            renders_as_point: ev.location_precision.renders_as_point(),
            themes: ev.themes.clone(),
            outlet_domains: ev.outlet_domains.clone(),
        }
    }
}

/// One row for the `baselines` table: the trailing median as of the newest
/// data day, per (cell, time-of-day bucket).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaselineRow {
    pub h3_cell: u64,
    pub tod_bucket: u8,
    pub baseline: f64,
    pub sample_days: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ScoredBuckets {
    /// Sorted by (cell, bucket start); counts plus all score components.
    pub buckets: Vec<RegionBucket>,
    /// Current baselines (as of the newest data day) for every seen cell ×
    /// the four time-of-day slots.
    pub baselines: Vec<BaselineRow>,
}

/// The M2 scoring pipeline (docs/SCORING.md): aggregate per bucket, then
/// score each bucket **as of its own end** — recency ages are measured
/// against the bucket end, and the spike baseline is the trailing median as
/// of the bucket's day, so replaying history shows what a live view would
/// have shown at that moment. Buckets with under `MIN_BASELINE_DAYS` of
/// prior coverage get a neutral spike and the cold-start flag.
pub fn score_buckets(events: &[ScoreEvent]) -> ScoredBuckets {
    #[derive(Default)]
    struct Accum<'e> {
        event_count: u32,
        attention_count: u32,
        article_count: u64,
        source_count: u64,
        outlets: HashSet<&'e str>,
        // Attention observations only (counting semantics, DATA_MODEL.md).
        att_articles: u64,
        att_outlets: HashSet<&'e str>,
        att_conf_sum: f64,
        att_age_sum: f64,
        att_theme_w_max: f64,
        // Discrete event records only.
        evt_kind_w_max: f64,
        evt_sev_sum: f64,
        evt_point_count: u32,
        evt_age_sum: f64,
    }

    let mut map: BTreeMap<(u64, i64), Accum<'_>> = BTreeMap::new();
    for ev in events {
        let bucket_start = bucket_start_epoch(ev.ts_epoch_s);
        let a = map.entry((ev.h3_cell, bucket_start)).or_default();
        let age = (bucket_start + BUCKET_SECS - ev.ts_epoch_s) as f64;
        a.article_count += u64::from(ev.article_count);
        a.source_count += u64::from(ev.distinct_source_count);
        for d in &ev.outlet_domains {
            a.outlets.insert(d.as_str());
        }
        if ev.kind.is_attention() {
            a.attention_count += 1;
            a.att_articles += u64::from(ev.article_count);
            for d in &ev.outlet_domains {
                a.att_outlets.insert(d.as_str());
            }
            a.att_conf_sum += f64::from(ev.location_confidence);
            a.att_age_sum += age;
            a.att_theme_w_max = a
                .att_theme_w_max
                .max(scoring::theme_weight(ev.themes.iter().map(String::as_str)));
        } else {
            a.event_count += 1;
            a.evt_kind_w_max = a.evt_kind_w_max.max(scoring::kind_weight(ev.kind));
            a.evt_sev_sum += f64::from(ev.severity.unwrap_or(0.0));
            a.evt_point_count += u32::from(ev.renders_as_point);
            a.evt_age_sum += age;
        }
    }

    let index = baseline::BaselineIndex::from_bucket_counts(
        map.iter()
            .map(|(&(cell, start), a)| (cell, start, a.event_count + a.attention_count)),
    );
    let last_day = map.keys().map(|&(_, start)| baseline::day_of(start)).max();

    let mut buckets = Vec::with_capacity(map.len());
    for (&(cell, bucket_start), a) in &map {
        let attention = if a.attention_count > 0 {
            scoring::attention_score(
                a.att_articles,
                a.att_age_sum / f64::from(a.attention_count),
                a.att_outlets.len() as u64,
                a.att_theme_w_max,
                a.att_conf_sum / f64::from(a.attention_count),
            )
        } else {
            0.0
        };
        let unrest = if a.event_count > 0 {
            scoring::unrest_score(
                u64::from(a.event_count),
                a.evt_kind_w_max,
                a.evt_age_sum / f64::from(a.event_count),
                a.evt_sev_sum / f64::from(a.event_count),
                f64::from(a.evt_point_count) / f64::from(a.event_count),
            )
        } else {
            0.0
        };
        let (base, sample_days) = index.trailing(
            cell,
            baseline::tod_bucket(bucket_start),
            baseline::day_of(bucket_start),
        );
        let cold = sample_days < weights::MIN_BASELINE_DAYS;
        let spike = if cold {
            weights::SPIKE_NEUTRAL
        } else {
            scoring::spike_score(f64::from(a.event_count + a.attention_count), base)
        };
        buckets.push(RegionBucket {
            h3_cell: cell,
            bucket_start,
            event_count: a.event_count,
            attention_count: a.attention_count,
            article_count: a.article_count,
            source_count: a.source_count,
            distinct_outlets: a.outlets.len() as u32,
            attention_score: attention as f32,
            unrest_score: unrest as f32,
            spike_score: spike as f32,
            combined_score: scoring::combined_signal(attention, unrest, spike) as f32,
            baseline: base as f32,
            spike_cold_start: cold,
        });
    }

    let mut baselines = Vec::new();
    if let Some(last_day) = last_day {
        for cell in index.cells() {
            for tod in 0..4u8 {
                let (b, n) = index.current(cell, tod, last_day);
                baselines.push(BaselineRow {
                    h3_cell: cell,
                    tod_bucket: tod,
                    baseline: b,
                    sample_days: n,
                });
            }
        }
    }
    ScoredBuckets { buckets, baselines }
}

/// Window-level scores for one cell, composed from stored bucket scores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowScores {
    pub attention: f32,
    pub unrest: f32,
    pub spike: f32,
    pub combined: f32,
    /// Any bucket in the window was cold-start.
    pub spike_cold_start: bool,
}

/// Compose per-bucket scores into scores for a viewed window `[start, end)`,
/// treating the window end as "now": each bucket is weighted by the recency
/// of its end. Empty bucket slots count as zero signal for attention/unrest
/// (silence is data) but are excluded from spike, which has no meaning
/// without records. `buckets` must all belong to one cell and lie inside the
/// window; returns `None` when there are none (no data to display).
pub fn compose_window(buckets: &[RegionBucket], window: (i64, i64)) -> Option<WindowScores> {
    if buckets.is_empty() || window.1 <= window.0 {
        return None;
    }
    let mut slot_w_total = 0.0;
    let mut slot = bucket_start_epoch(window.0);
    while slot < window.1 {
        let age = (window.1 - (slot + BUCKET_SECS)).max(0) as f64;
        slot_w_total += scoring::recency_weight(age);
        slot += BUCKET_SECS;
    }

    let (mut att_num, mut unr_num, mut spike_num, mut spike_den) = (0.0, 0.0, 0.0, 0.0);
    let mut cold = false;
    for b in buckets {
        let age = (window.1 - (b.bucket_start + BUCKET_SECS)).max(0) as f64;
        let w = scoring::recency_weight(age);
        att_num += w * f64::from(b.attention_score);
        unr_num += w * f64::from(b.unrest_score);
        spike_num += w * f64::from(b.spike_score);
        spike_den += w;
        cold |= b.spike_cold_start;
    }
    let attention = att_num / slot_w_total;
    let unrest = unr_num / slot_w_total;
    let spike = if spike_den > 0.0 {
        spike_num / spike_den
    } else {
        weights::SPIKE_NEUTRAL
    };
    Some(WindowScores {
        attention: attention as f32,
        unrest: unrest as f32,
        spike: spike as f32,
        combined: scoring::combined_signal(attention, unrest, spike) as f32,
        spike_cold_start: cold,
    })
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

    // ---- score_buckets pipeline ------------------------------------------

    /// f32 storage costs ~1e-7 of precision; goldens compare against f64.
    const F32_EPS: f32 = 1e-6;

    /// Compare an f32 score against a hand-computed f64 golden value.
    fn near(got: f32, want: f64) -> bool {
        (f64::from(got) - want).abs() < 1e-6
    }

    fn score_ev(kind: EventKind, cell: u64, ts: i64) -> ScoreEvent {
        ScoreEvent {
            h3_cell: cell,
            ts_epoch_s: ts,
            kind,
            article_count: 1,
            distinct_source_count: 1,
            location_confidence: 0.9,
            severity: None,
            renders_as_point: true,
            themes: vec![],
            outlet_domains: vec![],
        }
    }

    #[test]
    fn golden_scored_bucket() {
        // One bucket [0, 21600) with the exact inputs of the component
        // goldens in scoring.rs (see those tests for the arithmetic):
        //   attention = 0.319220766785   (19 articles, mean age 3 h,
        //                                 3 outlets, high theme, conf 0.85)
        //   unrest    = 0.655139300111   (3 events, conflict max, mean age
        //                                 3 h, mean sev 0.2, 2/3 points)
        //   spike     = 0.5 + cold flag  (first day ⇒ no history)
        //   combined  = 0.40·att + 0.45·unr + 0.15·0.5 = 0.497500991764
        let mk_att = |ts: i64, articles: u32, outlets: &[&str], theme: &str| ScoreEvent {
            article_count: articles,
            location_confidence: 0.85,
            themes: vec![theme.into()],
            outlet_domains: outlets.iter().map(|s| s.to_string()).collect(),
            ..score_ev(EventKind::NewsAttention, 5, ts)
        };
        let mk_evt = |ts: i64, kind: EventKind, sev: Option<f32>, point: bool| ScoreEvent {
            article_count: 0,
            severity: sev,
            renders_as_point: point,
            ..score_ev(kind, 5, ts)
        };
        let events = vec![
            // ages vs bucket end 21600: 2 h and 4 h → mean 3 h
            mk_att(14_400, 12, &["a.example", "b.example"], "flood"),
            mk_att(7_200, 7, &["b.example", "c.example"], "protest"),
            // ages 2 h, 3 h, 4 h → mean 3 h
            mk_evt(14_400, EventKind::Protest, Some(0.2), true),
            mk_evt(10_800, EventKind::Protest, None, true),
            mk_evt(7_200, EventKind::Conflict, Some(0.4), false),
        ];
        let scored = score_buckets(&events);
        assert_eq!(scored.buckets.len(), 1);
        let b = &scored.buckets[0];
        assert_eq!((b.attention_count, b.event_count), (2, 3));
        assert_eq!(b.article_count, 19);
        assert_eq!(b.distinct_outlets, 3);
        assert!(near(b.attention_score, 0.319_220_766_785));
        assert!(near(b.unrest_score, 0.655_139_300_111));
        assert!(b.spike_cold_start, "first-day bucket must be cold-start");
        assert_eq!(b.spike_score, 0.5, "cold start forces a neutral spike");
        assert!(near(b.combined_score, 0.497_500_991_764));
    }

    /// Flat synthetic series: one attention record per bucket for 35 days.
    fn flat_series(cell: u64, days: i64) -> Vec<ScoreEvent> {
        let mut out = Vec::new();
        for day in 0..days {
            for tod in 0..4i64 {
                let ts = day * 86_400 + tod * BUCKET_SECS + 3_600;
                out.push(score_ev(EventKind::NewsAttention, cell, ts));
            }
        }
        out
    }

    #[test]
    fn flat_series_spikes_neutral_after_warmup() {
        let scored = score_buckets(&flat_series(42, 35));
        assert_eq!(scored.buckets.len(), 35 * 4);
        for b in &scored.buckets {
            let day = b.bucket_start / 86_400;
            if day < i64::from(weights::MIN_BASELINE_DAYS) {
                assert!(b.spike_cold_start, "day {day} should be cold");
                assert_eq!(b.spike_score, 0.5);
            } else {
                assert!(!b.spike_cold_start, "day {day} should be warm");
                // current 1 vs median 1 → exactly neutral.
                assert!((b.spike_score - 0.5).abs() < F32_EPS, "day {day}");
                assert!((b.baseline - 1.0).abs() < F32_EPS);
            }
        }
    }

    #[test]
    fn injected_burst_spikes_high_then_baseline_absorbs_it() {
        let cell = 42;
        let mut events = flat_series(cell, 35);
        // Burst: 8 extra records in day 30, tod 2 → 9 total in that bucket.
        let burst_ts = 30 * 86_400 + 2 * BUCKET_SECS + 3_600;
        for _ in 0..8 {
            events.push(score_ev(EventKind::NewsAttention, cell, burst_ts));
        }
        let scored = score_buckets(&events);
        let get = |day: i64, tod: i64| {
            let start = day * 86_400 + tod * BUCKET_SECS;
            scored
                .buckets
                .iter()
                .find(|b| b.bucket_start == start)
                .unwrap()
        };
        // Hand-computed: 9 vs baseline 1 → 0.5 + log2(9.5/1.5)/6 = 0.943827502120.
        let burst = get(30, 2);
        assert!(!burst.spike_cold_start);
        assert!(near(burst.spike_score, 0.943_827_502_120));
        // The same slot next day: the median over 28 days ignores one
        // outlier day → baseline still 1, spike neutral.
        let next = get(31, 2);
        assert!((next.baseline - 1.0).abs() < F32_EPS);
        assert!((next.spike_score - 0.5).abs() < F32_EPS);
        // Adjacent time-of-day slot on the burst day is untouched.
        assert!((get(30, 1).spike_score - 0.5).abs() < F32_EPS);
    }

    #[test]
    fn cold_start_store_is_all_neutral_and_flagged() {
        let scored = score_buckets(&flat_series(7, 3));
        assert!(!scored.buckets.is_empty());
        for b in &scored.buckets {
            assert!(b.spike_cold_start);
            assert_eq!(b.spike_score, 0.5);
        }
        // The persisted current baselines also expose the thin history.
        assert!(!scored.baselines.is_empty());
        assert!(
            scored
                .baselines
                .iter()
                .all(|r| r.sample_days < weights::MIN_BASELINE_DAYS)
        );
    }

    #[test]
    fn baselines_cover_every_cell_and_tod() {
        let mut events = flat_series(1, 30);
        events.extend(flat_series(2, 30));
        let scored = score_buckets(&events);
        assert_eq!(scored.baselines.len(), 2 * 4);
        // Flat series: every slot's trailing 28-day median is exactly 1.
        for r in &scored.baselines {
            assert!((r.baseline - 1.0).abs() < 1e-9);
            assert_eq!(r.sample_days, weights::BASELINE_WINDOW_DAYS);
        }
    }

    // ---- compose_window ---------------------------------------------------

    #[test]
    fn golden_compose_window() {
        // Two adjacent buckets, window = both slots. Weights vs window end:
        //   w0 = 2^(−21600/86400) = 0.840896415254 (older), w1 = 1.
        //   attention = (w0·0.4 + 1·0.8)/(w0+1) = 0.617285446745
        //   unrest    = (w0·0.2 + 1·0.0)/(w0+1) = 0.091357276627
        //   spike     = (w0·0.6 + 1·0.7)/(w0+1) = 0.654321361686
        //   combined  = 0.40·a + 0.45·u + 0.15·s = 0.386173157433
        let mut b0 = RegionBucket::empty(9, 0);
        b0.attention_score = 0.4;
        b0.unrest_score = 0.2;
        b0.spike_score = 0.6;
        let mut b1 = RegionBucket::empty(9, BUCKET_SECS);
        b1.attention_score = 0.8;
        b1.unrest_score = 0.0;
        b1.spike_score = 0.7;
        b1.spike_cold_start = true;

        let w = compose_window(&[b0, b1], (0, 2 * BUCKET_SECS)).unwrap();
        assert!(near(w.attention, 0.617_285_446_745));
        assert!(near(w.unrest, 0.091_357_276_627));
        assert!(near(w.spike, 0.654_321_361_686));
        assert!(near(w.combined, 0.386_173_157_433));
        assert!(w.spike_cold_start, "any cold bucket taints the window");
    }

    #[test]
    fn compose_window_of_one_bucket_is_identity() {
        let mut b = RegionBucket::empty(9, 0);
        b.attention_score = 0.37;
        b.unrest_score = 0.21;
        b.spike_score = 0.66;
        let w = compose_window(&[b], (0, BUCKET_SECS)).unwrap();
        assert!((w.attention - 0.37).abs() < F32_EPS);
        assert!((w.unrest - 0.21).abs() < F32_EPS);
        assert!((w.spike - 0.66).abs() < F32_EPS);
        assert!(!w.spike_cold_start);
    }

    #[test]
    fn compose_window_dilutes_attention_with_empty_slots_but_not_spike() {
        // One active bucket in a 4-slot window: attention shrinks (silence
        // is data) while spike keeps its bucket value (no records, no ratio).
        let mut b = RegionBucket::empty(9, 3 * BUCKET_SECS);
        b.attention_score = 0.8;
        b.spike_score = 0.9;
        let w = compose_window(&[b], (0, 4 * BUCKET_SECS)).unwrap();
        assert!(w.attention < 0.3, "{}", w.attention);
        assert!((w.spike - 0.9).abs() < F32_EPS);
    }

    #[test]
    fn compose_window_empty_is_none() {
        assert!(compose_window(&[], (0, BUCKET_SECS)).is_none());
    }
}
