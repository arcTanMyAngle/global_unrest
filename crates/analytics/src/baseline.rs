//! Trailing-median baselines per (H3 cell, 6-hour time-of-day bucket).
//!
//! The baseline for a bucket is the **median** of that cell's record counts
//! in the same time-of-day slot over the previous `BASELINE_WINDOW_DAYS`
//! days (docs/SCORING.md: medians because news counts are heavy-tailed).
//! Days inside the store's coverage with no records count as 0 — a quiet day
//! is an observation, not a gap. Days before the store's first data day are
//! excluded; when fewer than `MIN_BASELINE_DAYS` remain the caller treats
//! the bucket as cold-start (neutral spike + low-confidence flag).

use std::collections::BTreeMap;

use core_types::BUCKET_SECS;

use crate::weights;

pub const SECS_PER_DAY: i64 = 86_400;

/// UTC day index (days since epoch; floors correctly for pre-1970 times).
pub fn day_of(epoch_s: i64) -> i64 {
    epoch_s.div_euclid(SECS_PER_DAY)
}

/// Time-of-day bucket 0..=3 (00–06, 06–12, 12–18, 18–24 UTC).
pub fn tod_bucket(bucket_start: i64) -> u8 {
    (bucket_start.rem_euclid(SECS_PER_DAY) / BUCKET_SECS) as u8
}

/// Median of an unsorted list; even lengths average the middle two. Empty → 0.
fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("counts are finite"));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Per-(cell, day, tod) record counts, queryable for trailing medians.
pub struct BaselineIndex {
    counts: BTreeMap<(u64, i64, u8), u32>,
    /// First UTC day with any data, store-wide. Zero-days at one cell are
    /// real observations only while the store was collecting at all.
    first_day: Option<i64>,
}

impl BaselineIndex {
    /// Build from per-bucket totals: (h3_cell, bucket_start, record count).
    pub fn from_bucket_counts(buckets: impl IntoIterator<Item = (u64, i64, u32)>) -> Self {
        let mut counts: BTreeMap<(u64, i64, u8), u32> = BTreeMap::new();
        let mut first_day = None;
        for (cell, bucket_start, n) in buckets {
            let day = day_of(bucket_start);
            first_day = Some(match first_day {
                None => day,
                Some(d) if day < d => day,
                Some(d) => d,
            });
            *counts
                .entry((cell, day, tod_bucket(bucket_start)))
                .or_insert(0) += n;
        }
        Self { counts, first_day }
    }

    /// Trailing median for (cell, tod) over the window days strictly before
    /// `day`, clipped to the store's first data day. Returns
    /// `(median, sample_days)`; `sample_days` is how many days the median
    /// actually saw — the cold-start test compares it to `MIN_BASELINE_DAYS`.
    pub fn trailing(&self, cell: u64, tod: u8, day: i64) -> (f64, u32) {
        let Some(first_day) = self.first_day else {
            return (0.0, 0);
        };
        let lo = first_day.max(day - i64::from(weights::BASELINE_WINDOW_DAYS));
        if lo >= day {
            return (0.0, 0);
        }
        let samples: Vec<f64> = (lo..day)
            .map(|d| f64::from(*self.counts.get(&(cell, d, tod)).unwrap_or(&0)))
            .collect();
        let n = samples.len() as u32;
        (median(samples), n)
    }

    /// Baseline "as of the end of data": the trailing window ending on
    /// `last_day` inclusive. This is what gets persisted to the `baselines`
    /// table for live (M3) use and inspector display.
    pub fn current(&self, cell: u64, tod: u8, last_day: i64) -> (f64, u32) {
        self.trailing(cell, tod, last_day + 1)
    }

    /// All cells that ever appear in the index, deduplicated and sorted.
    pub fn cells(&self) -> Vec<u64> {
        let mut cells: Vec<u64> = self.counts.keys().map(|&(c, _, _)| c).collect();
        cells.dedup(); // BTreeMap keys iterate sorted by cell first
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn day_and_tod_math() {
        // 2026-06-01 13:00 UTC = epoch 1780318800; day 20605, tod 2 (12–18).
        let ts = 1_780_318_800i64;
        assert_eq!(day_of(ts), 20_605);
        assert_eq!(tod_bucket((ts / BUCKET_SECS) * BUCKET_SECS), 2);
        // Pre-1970 floors downward, not toward zero.
        assert_eq!(day_of(-1), -1);
        assert_eq!(tod_bucket(-BUCKET_SECS), 3);
    }

    #[test]
    fn median_odd_even_empty() {
        assert!((median(vec![5.0, 0.0, 1.0, 2.0, 0.0]) - 1.0).abs() < EPS);
        assert!((median(vec![10.0, 1.0, 3.0, 2.0]) - 2.5).abs() < EPS);
        assert!(median(vec![]).abs() < EPS);
    }

    /// Build an index where cell 7's tod-1 slot has counts `per_day[i]` on
    /// day `start_day + i`.
    fn index_with(per_day: &[u32], start_day: i64) -> BaselineIndex {
        BaselineIndex::from_bucket_counts(per_day.iter().enumerate().map(|(i, &n)| {
            (
                7u64,
                (start_day + i as i64) * SECS_PER_DAY + BUCKET_SECS, // tod 1
                n,
            )
        }))
    }

    #[test]
    fn trailing_median_counts_quiet_days_as_zero() {
        // Days 100..105 with counts [4, 0, 2, 0, 8] (the zeros still create
        // index entries here; absence must behave identically — see below).
        let idx = index_with(&[4, 0, 2, 0, 8], 100);
        // Trailing at day 105 sees all 5 days: median(4,0,2,0,8) = 2.
        let (m, n) = idx.trailing(7, 1, 105);
        assert_eq!(n, 5);
        assert!((m - 2.0).abs() < EPS);

        // Same data but days 101 and 103 simply absent from the index:
        // they are inside coverage, so they still count as zeros.
        let idx = BaselineIndex::from_bucket_counts([
            (7u64, 100 * SECS_PER_DAY + BUCKET_SECS, 4u32),
            (7u64, 102 * SECS_PER_DAY + BUCKET_SECS, 2),
            (7u64, 104 * SECS_PER_DAY + BUCKET_SECS, 8),
        ]);
        let (m, n) = idx.trailing(7, 1, 105);
        assert_eq!(n, 5);
        assert!((m - 2.0).abs() < EPS, "{m}");
    }

    #[test]
    fn trailing_excludes_current_day_and_respects_window() {
        // 40 days of constant 3, then day of interest.
        let idx = index_with(&[3; 40], 0);
        // Trailing at day 40: only the last 28 days (12..40), median 3.
        let (m, n) = idx.trailing(7, 1, 40);
        assert_eq!(n, weights::BASELINE_WINDOW_DAYS);
        assert!((m - 3.0).abs() < EPS);
        // The bucket's own day is never part of its baseline.
        let (m, _) = idx.trailing(7, 1, 39);
        assert!((m - 3.0).abs() < EPS);
    }

    #[test]
    fn cold_start_has_few_samples() {
        let idx = index_with(&[5, 5, 5], 200);
        // Day 202 has only 2 days of history.
        let (_, n) = idx.trailing(7, 1, 202);
        assert_eq!(n, 2);
        assert!(n < weights::MIN_BASELINE_DAYS);
        // Day 200 (the first data day) has none.
        let (m, n) = idx.trailing(7, 1, 200);
        assert_eq!((m, n), (0.0, 0));
    }

    #[test]
    fn unknown_cell_medians_to_zero_inside_coverage() {
        let idx = index_with(&[9; 10], 50);
        // A cell never seen still gets a (0-valued) baseline with full
        // sample days — silence at that cell is data.
        let (m, n) = idx.trailing(999, 1, 60);
        assert_eq!(n, 10);
        assert!(m.abs() < EPS);
    }

    #[test]
    fn current_includes_last_day() {
        let idx = index_with(&[1, 1, 1, 7], 10);
        // current at last_day 13 covers days 10..=13: median(1,1,1,7) = 1.
        let (m, n) = idx.current(7, 1, 13);
        assert_eq!(n, 4);
        assert!((m - 1.0).abs() < EPS);
    }

    #[test]
    fn cells_lists_each_cell_once() {
        let idx =
            BaselineIndex::from_bucket_counts([(5u64, 0i64, 1u32), (5, BUCKET_SECS, 2), (9, 0, 1)]);
        assert_eq!(idx.cells(), vec![5, 9]);
    }

    #[test]
    fn empty_index_is_all_cold() {
        let idx = BaselineIndex::from_bucket_counts([]);
        assert_eq!(idx.trailing(1, 0, 100), (0.0, 0));
        assert!(idx.cells().is_empty());
    }
}
