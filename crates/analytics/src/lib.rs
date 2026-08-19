//! Aggregation (M1) and transparent scoring / baselines / spike detection (M2).
//!
//! Everything here is a pure function over domain types — no I/O, no state.
//! `aggregate_buckets` is also the reference implementation that the storage
//! crate's SQL `GROUP BY` is integration-tested against.

pub mod baseline;
pub mod scoring;

use std::collections::{BTreeMap, HashSet};

use core_types::{
    BUCKET_SECS, EventKind, FamilyBucket, GeoTemporalEvent, RegionBucket, SignalFamily,
    bucket_start_epoch,
};

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

    /// A cell's spike halo (docs/VISUALIZATION.md V1 item 2) lights up once
    /// `spike_score` clears this — comfortably above `SPIKE_NEUTRAL`.
    pub const SPIKE_HALO_THRESHOLD: f64 = 0.8;
    /// Cap on cells rendered with a halo at once, so a broad spike doesn't
    /// paint hundreds of rings.
    pub const SPIKE_HALO_MAX_CELLS: usize = 40;

    /// Rows in the top-movers panel (docs/VISUALIZATION.md V2 item 6). A
    /// ranked list is only useful while it stays scannable.
    pub const TOP_MOVERS_LIMIT: usize = 12;
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
    /// The observation axis. Every scoring decision below asks the family,
    /// never the kind — see docs/SIGNAL_MODEL.md.
    pub family: SignalFamily,
    pub kind: EventKind,
    /// Volume in this family's own unit. Never summed across families.
    pub volume_count: u32,
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
            family: ev.family,
            kind: ev.kind,
            volume_count: ev.volume_count,
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

/// One row for the `family_baselines` table: a family's own trailing median,
/// per (cell, time-of-day bucket, family).
///
/// Long-form deliberately: a sixth family must not cost a schema migration,
/// and per-family deficits (silence detection) are exactly this shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FamilyBaselineRow {
    pub h3_cell: u64,
    pub tod_bucket: u8,
    pub family: SignalFamily,
    pub baseline: f64,
    pub sample_days: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ScoredBuckets {
    /// Sorted by (cell, bucket start); counts plus all score components.
    pub buckets: Vec<RegionBucket>,
    /// Current baselines (as of the newest data day) for every seen cell ×
    /// the four time-of-day slots. Built from the families that feed the
    /// generic spike only, so chatter volume can never move it.
    pub baselines: Vec<BaselineRow>,
    /// Per-family record and volume counts, sorted by (cell, bucket, family).
    pub family_buckets: Vec<FamilyBucket>,
    /// Per-family trailing medians as of the newest data day.
    pub family_baselines: Vec<FamilyBaselineRow>,
}

/// Position of a family in `SignalFamily::ALL`, for the fixed-size per-bucket
/// tally. Exhaustive on purpose: a new family fails to compile here.
const fn family_index(family: SignalFamily) -> usize {
    match family {
        SignalFamily::MediaAttention => 0,
        SignalFamily::RecordedEvent => 1,
        SignalFamily::OfficialAlert => 2,
        SignalFamily::Chatter => 3,
        SignalFamily::Measurement => 4,
    }
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
        /// Records in the families that feed the generic spike. Kept as its
        /// own counter rather than `event_count + attention_count`, so a new
        /// family joins the spike only by saying so in the matrix.
        generic_count: u32,
        article_count: u64,
        source_count: u64,
        outlets: HashSet<&'e str>,
        // Attention observations only (counting semantics, DATA_MODEL.md).
        att_articles: u64,
        att_outlets: HashSet<&'e str>,
        att_conf_sum: f64,
        att_age_sum: f64,
        att_theme_w_max: f64,
        // Unrest-bearing records only.
        evt_kind_w_max: f64,
        evt_sev_sum: f64,
        evt_point_count: u32,
        evt_age_sum: f64,
        /// Per-family (record_count, volume_count), indexed by
        /// `SignalFamily::ALL`.
        family: [(u32, u64); SignalFamily::ALL.len()],
    }

    let mut map: BTreeMap<(u64, i64), Accum<'_>> = BTreeMap::new();
    for ev in events {
        let bucket_start = bucket_start_epoch(ev.ts_epoch_s);
        let a = map.entry((ev.h3_cell, bucket_start)).or_default();
        let age = (bucket_start + BUCKET_SECS - ev.ts_epoch_s) as f64;

        let slot = &mut a.family[family_index(ev.family)];
        slot.0 += 1;
        slot.1 += u64::from(ev.volume_count);

        if ev.family.enters_generic_spike() {
            a.generic_count += 1;
        }
        // `article_count`, `source_count` and `distinct_outlets` are
        // attention-only by construction: chatter posts are not articles and
        // a chatter rollup names no outlet.
        if ev.family.enters_attention() {
            a.attention_count += 1;
            a.article_count += u64::from(ev.volume_count);
            a.source_count += u64::from(ev.distinct_source_count);
            for d in &ev.outlet_domains {
                a.outlets.insert(d.as_str());
            }
            a.att_articles += u64::from(ev.volume_count);
            for d in &ev.outlet_domains {
                a.att_outlets.insert(d.as_str());
            }
            a.att_conf_sum += f64::from(ev.location_confidence);
            a.att_age_sum += age;
            a.att_theme_w_max = a
                .att_theme_w_max
                .max(scoring::theme_weight(ev.themes.iter().map(String::as_str)));
        }
        if ev.family.enters_unrest() {
            a.event_count += 1;
            a.evt_kind_w_max = a.evt_kind_w_max.max(scoring::kind_weight(ev.kind));
            a.evt_sev_sum += f64::from(ev.severity.unwrap_or(0.0));
            a.evt_point_count += u32::from(ev.renders_as_point);
            a.evt_age_sum += age;
        }
    }

    let index = baseline::BaselineIndex::from_bucket_counts(
        map.iter()
            .map(|(&(cell, start), a)| (cell, start, a.generic_count)),
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
            scoring::spike_score(f64::from(a.generic_count), base)
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

    let mut family_buckets = Vec::new();
    for (&(cell, bucket_start), a) in &map {
        for (i, family) in SignalFamily::ALL.into_iter().enumerate() {
            let (record_count, volume_count) = a.family[i];
            if record_count == 0 {
                continue;
            }
            family_buckets.push(FamilyBucket {
                h3_cell: cell,
                bucket_start,
                family,
                record_count,
                volume_count,
            });
        }
    }

    let mut baselines = Vec::new();
    let mut family_baselines = Vec::new();
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
        // Each family gets its own trailing median from its own counts. This
        // is what lets a family go quiet observably (docs/SIGNAL_MODEL.md)
        // without its volume ever entering the generic spike.
        for (i, family) in SignalFamily::ALL.into_iter().enumerate() {
            let fam_index = baseline::BaselineIndex::from_bucket_counts(
                map.iter()
                    .filter(|(_, a)| a.family[i].0 > 0)
                    .map(|(&(cell, start), a)| (cell, start, a.family[i].0)),
            );
            for cell in fam_index.cells() {
                for tod in 0..4u8 {
                    let (b, n) = fam_index.current(cell, tod, last_day);
                    family_baselines.push(FamilyBaselineRow {
                        h3_cell: cell,
                        tod_bucket: tod,
                        family,
                        baseline: b,
                        sample_days: n,
                    });
                }
            }
        }
    }
    ScoredBuckets {
        buckets,
        baselines,
        family_buckets,
        family_baselines,
    }
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

/// Cells worth a spike halo (docs/VISUALIZATION.md V1 item 2): for each
/// cell, the max `spike_score` among its **non-cold-start** buckets in
/// `buckets` (cold-start buckets have no baseline, so no anomaly claim —
/// consistent with the low-confidence badge shown elsewhere for them),
/// filtered to `>= threshold` and capped to the `max_cells` strongest.
/// Threshold/cap are parameters (not baked in) so tests don't depend on the
/// production constants in [`weights`].
pub fn spike_halo_cells(
    buckets: &[RegionBucket],
    threshold: f64,
    max_cells: usize,
) -> Vec<(u64, f32)> {
    let mut best: BTreeMap<u64, f32> = BTreeMap::new();
    for b in buckets {
        if b.spike_cold_start {
            continue;
        }
        let slot = best.entry(b.h3_cell).or_insert(f32::MIN);
        *slot = slot.max(b.spike_score);
    }
    let mut cells: Vec<(u64, f32)> = best
        .into_iter()
        .filter(|&(_, score)| f64::from(score) >= threshold)
        .collect();
    cells.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    cells.truncate(max_cells);
    cells
}

/// A cell's strongest bucket in the viewed window (docs/VISUALIZATION.md V2
/// item 6), carrying enough to show *why* it ranked: the spike score plus the
/// raw record count and baseline that score was computed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mover {
    pub h3_cell: u64,
    /// Peak `spike_score` among the cell's non-cold-start buckets.
    pub spike: f32,
    /// Start of the bucket that peak came from.
    pub bucket_start: i64,
    /// Records in that bucket — discrete events *and* attention observations,
    /// matching the denominator `baseline` is a median of.
    pub records: u32,
    /// Trailing 28-day median records/bucket behind that same bucket.
    pub baseline: f32,
}

impl Mover {
    /// Records above (or below) the cell's own trailing baseline — the spike
    /// score in the units it was derived from, so the panel can show the
    /// evidence next to the score rather than only the score.
    pub fn delta(&self) -> f32 {
        self.records as f32 - self.baseline
    }
}

/// The strongest-spiking cells in `buckets`, ranked, for the top-movers panel.
///
/// Cold-start buckets are skipped for the same reason [`spike_halo_cells`]
/// skips them: no baseline behind a bucket means no anomaly to claim. A cell
/// whose every bucket is cold-start therefore never appears — being absent is
/// correct, and better than ranking it on a neutral score.
///
/// Sorted by spike descending, ties broken by cell id so the panel never
/// reorders between frames on equal scores.
pub fn top_movers(buckets: &[RegionBucket], limit: usize) -> Vec<Mover> {
    let mut best: BTreeMap<u64, Mover> = BTreeMap::new();
    for b in buckets {
        if b.spike_cold_start {
            continue;
        }
        let candidate = Mover {
            h3_cell: b.h3_cell,
            spike: b.spike_score,
            bucket_start: b.bucket_start,
            records: b.event_count + b.attention_count,
            baseline: b.baseline,
        };
        best.entry(b.h3_cell)
            .and_modify(|m| {
                if candidate.spike > m.spike {
                    *m = candidate;
                }
            })
            .or_insert(candidate);
    }
    let mut movers: Vec<Mover> = best.into_values().collect();
    movers.sort_by(|a, b| {
        b.spike
            .total_cmp(&a.spike)
            .then_with(|| a.h3_cell.cmp(&b.h3_cell))
    });
    movers.truncate(limit);
    movers
}

/// Dense per-bucket record counts for one cell across `window`, oldest first —
/// the tiny series behind a top-movers row.
///
/// Built from the caller's already-loaded buckets, so the panel still issues
/// no query of its own; the cost is one scan per ranked row, paid only on
/// rebuild. Absent buckets are real zeros — the cell was silent, and silence
/// is data (the same convention [`compose_window`] uses).
pub fn cell_series(buckets: &[RegionBucket], cell: u64, window: (i64, i64)) -> Vec<u32> {
    if window.1 <= window.0 {
        return Vec::new();
    }
    let start = bucket_start_epoch(window.0);
    let slots = ((window.1 - start).div_euclid(BUCKET_SECS) + 1).max(0) as usize;
    let mut series = vec![0u32; slots];
    for b in buckets.iter().filter(|b| b.h3_cell == cell) {
        let idx = (b.bucket_start - start).div_euclid(BUCKET_SECS);
        if idx >= 0 && (idx as usize) < slots {
            series[idx as usize] += b.event_count + b.attention_count;
        }
    }
    series
}

/// One display cell's attention/unrest components, folded from its buckets
/// in the viewed window (docs/VISUALIZATION.md V2 item 5).
///
/// The display cell is not necessarily the stored res-3 cell — the heatmap
/// rolls up to coarser H3 parents at world zoom — so the caller supplies the
/// key and folds each bucket in with [`CellComponents::absorb`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellComponents {
    pub h3_cell: u64,
    /// Peak `attention_score` among the cell's buckets in the window.
    pub attention: f32,
    /// Peak `unrest_score` among the cell's buckets in the window.
    pub unrest: f32,
    /// At least one attention observation landed in this cell in the window.
    pub has_attention: bool,
    /// At least one discrete event record landed in this cell in the window.
    pub has_events: bool,
}

impl CellComponents {
    pub fn new(h3_cell: u64) -> Self {
        Self {
            h3_cell,
            attention: 0.0,
            unrest: 0.0,
            has_attention: false,
            has_events: false,
        }
    }

    /// Fold one of this cell's buckets into the aggregate.
    ///
    /// Peak, not mean: both components are already normalized to [0, 1] per
    /// bucket, and averaging over a window's slots would conflate "sustained"
    /// with "strong" — the divergence question is about how hard each channel
    /// registered here, which is what the max answers. Same per-cell reduction
    /// [`spike_halo_cells`] already uses.
    pub fn absorb(&mut self, b: &RegionBucket) {
        self.attention = self.attention.max(b.attention_score);
        self.unrest = self.unrest.max(b.unrest_score);
        self.has_attention |= b.attention_count > 0;
        self.has_events |= b.event_count > 0;
    }

    /// Both channels produced records here, so their ranks describe the same
    /// cell and can be differenced. A cell with zero records on one side is
    /// **not** "maximum divergence": the absence may be this project's own
    /// coverage gap (GDELT DOC is source-country geocoded, ACLED is
    /// event-only), and claiming a direction from it would over-read the data
    /// — docs/SAFETY_AND_PRIVACY.md § "Known biases".
    pub fn comparable(&self) -> bool {
        self.has_attention && self.has_events
    }
}

/// Fractional ranks of `values` mapped onto [0, 1], ties sharing their
/// average rank so the result never depends on input order.
///
/// A single value has no distribution to rank against, so it maps to the
/// midpoint rather than to an arbitrary end.
fn normalized_ranks(values: &[f32]) -> Vec<f32> {
    let n = values.len();
    if n <= 1 {
        return vec![0.5; n];
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut ranks = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        // Span of the tie group starting at `i`.
        let mut j = i + 1;
        while j < n && values[order[j]] == values[order[i]] {
            j += 1;
        }
        let avg = (i + j - 1) as f32 / 2.0;
        for &idx in &order[i..j] {
            ranks[idx] = avg / (n - 1) as f32;
        }
        i = j;
    }
    ranks
}

/// Attention ↔ unrest divergence per display cell (docs/VISUALIZATION.md V2
/// item 5), as `(cell, divergence)` sorted by cell id.
///
/// `Some(d)`, `d` ∈ [-1, 1]: **positive** = media attention outruns event
/// data here (covered but quiet), **negative** = events outrun attention
/// (under-covered). `None` = no comparison to make, because one channel has
/// no records in this cell at all.
///
/// Ranks, not raw magnitudes: `attention_score` and `unrest_score` are built
/// from different components on different scales (article volume and outlet
/// diversity vs. event count, type, severity and precision), so their
/// difference is meaningless as a number. Their positions within the same
/// window's distribution are comparable. Ranks are taken over the comparable
/// cells only — an incomparable cell must not shift the distribution it was
/// excluded from.
pub fn divergence_ranks(cells: &[CellComponents]) -> Vec<(u64, Option<f32>)> {
    let comparable: Vec<&CellComponents> = cells.iter().filter(|c| c.comparable()).collect();
    let att: Vec<f32> = comparable.iter().map(|c| c.attention).collect();
    let unr: Vec<f32> = comparable.iter().map(|c| c.unrest).collect();
    let att_ranks = normalized_ranks(&att);
    let unr_ranks = normalized_ranks(&unr);

    let scored: BTreeMap<u64, f32> = comparable
        .iter()
        .enumerate()
        .map(|(i, c)| (c.h3_cell, att_ranks[i] - unr_ranks[i]))
        .collect();

    let mut out: Vec<(u64, Option<f32>)> = cells
        .iter()
        .map(|c| (c.h3_cell, scored.get(&c.h3_cell).copied()))
        .collect();
    out.sort_by_key(|&(cell, _)| cell);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use core_types::{BUCKET_SECS, EventKind, LocationPrecision, LocationRole, SourceId, event_id};

    fn ev(kind: EventKind, cell: u64, hour: u32, articles: u32, sources: u32) -> GeoTemporalEvent {
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, hour, 15, 0).unwrap();
        GeoTemporalEvent {
            id: event_id(SourceId::Fixtures, &format!("{kind:?}-{cell}-{hour}")),
            source: SourceId::Fixtures,
            source_event_id: "x".into(),
            family: kind.family(),
            kind,
            location_role: LocationRole::EventSite,
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
            volume_count: articles,
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
        // Attention-only by construction: the protest record's volume is
        // measured in records, not articles, so it is not added in here.
        assert_eq!(buckets[0].article_count, 4);
        assert_eq!(buckets[0].source_count, 2);

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
            family: kind.family(),
            kind,
            volume_count: 1,
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
            volume_count: articles,
            location_confidence: 0.85,
            themes: vec![theme.into()],
            outlet_domains: outlets.iter().map(|s| s.to_string()).collect(),
            ..score_ev(EventKind::NewsAttention, 5, ts)
        };
        let mk_evt = |ts: i64, kind: EventKind, sev: Option<f32>, point: bool| ScoreEvent {
            volume_count: 0,
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

    // ---- spike_halo_cells ---------------------------------------------

    fn bucket(cell: u64, spike: f32, cold: bool) -> RegionBucket {
        let mut b = RegionBucket::empty(cell, 0);
        b.spike_score = spike;
        b.spike_cold_start = cold;
        b
    }

    #[test]
    fn spike_halo_cells_filters_by_threshold() {
        let buckets = vec![bucket(1, 0.9, false), bucket(2, 0.6, false)];
        let halos = spike_halo_cells(&buckets, 0.8, 40);
        assert_eq!(halos, vec![(1, 0.9)]);
    }

    #[test]
    fn spike_halo_cells_excludes_cold_start_even_above_threshold() {
        let buckets = vec![bucket(1, 0.95, true)];
        assert!(spike_halo_cells(&buckets, 0.8, 40).is_empty());
    }

    #[test]
    fn spike_halo_cells_takes_max_score_per_cell() {
        // Same cell, two buckets in the window: one cold (ignored), the
        // warm one's score wins even though it's listed first.
        let buckets = vec![
            bucket(1, 0.85, false),
            bucket(1, 0.99, true),
            bucket(1, 0.90, false),
        ];
        assert_eq!(spike_halo_cells(&buckets, 0.8, 40), vec![(1, 0.90)]);
    }

    #[test]
    fn spike_halo_cells_sorts_descending_and_caps_at_max_cells() {
        let buckets = vec![
            bucket(1, 0.81, false),
            bucket(2, 0.95, false),
            bucket(3, 0.88, false),
        ];
        assert_eq!(
            spike_halo_cells(&buckets, 0.8, 2),
            vec![(2, 0.95), (3, 0.88)]
        );
    }

    // ---- top_movers ---------------------------------------------------

    fn mover_bucket(
        cell: u64,
        start: i64,
        spike: f32,
        records: u32,
        baseline: f32,
    ) -> RegionBucket {
        let mut b = RegionBucket::empty(cell, start);
        b.spike_score = spike;
        b.event_count = records;
        b.baseline = baseline;
        b
    }

    #[test]
    fn top_movers_ranks_by_spike_and_caps() {
        let buckets = vec![
            mover_bucket(1, 0, 0.55, 3, 2.0),
            mover_bucket(2, 0, 0.95, 9, 1.0),
            mover_bucket(3, 0, 0.72, 5, 3.0),
        ];
        let movers = top_movers(&buckets, 2);
        assert_eq!(
            movers.iter().map(|m| m.h3_cell).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!((movers[0].delta() - 8.0).abs() < F32_EPS);
    }

    #[test]
    fn top_movers_keeps_the_peak_bucket_evidence_not_just_the_score() {
        // Two buckets for one cell: the panel must report the counts from the
        // bucket that actually produced the winning spike.
        let buckets = vec![
            mover_bucket(1, 0, 0.60, 40, 39.0),
            mover_bucket(1, BUCKET_SECS, 0.91, 7, 1.0),
        ];
        let movers = top_movers(&buckets, 10);
        assert_eq!(movers.len(), 1);
        assert_eq!(movers[0].bucket_start, BUCKET_SECS);
        assert_eq!(movers[0].records, 7);
        assert!((movers[0].delta() - 6.0).abs() < F32_EPS);
    }

    #[test]
    fn top_movers_counts_attention_and_events_together() {
        let mut b = mover_bucket(1, 0, 0.9, 2, 1.5);
        b.attention_count = 5;
        assert_eq!(top_movers(&[b], 10)[0].records, 7);
    }

    #[test]
    fn top_movers_excludes_cold_start_cells_entirely() {
        let mut cold = mover_bucket(1, 0, 0.99, 20, 0.0);
        cold.spike_cold_start = true;
        let warm = mover_bucket(2, 0, 0.50, 2, 1.0);
        let movers = top_movers(&[cold, warm], 10);
        assert_eq!(
            movers.iter().map(|m| m.h3_cell).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn top_movers_breaks_ties_by_cell_id_for_a_stable_panel() {
        let buckets = vec![
            mover_bucket(9, 0, 0.8, 1, 1.0),
            mover_bucket(3, 0, 0.8, 1, 1.0),
            mover_bucket(6, 0, 0.8, 1, 1.0),
        ];
        assert_eq!(
            top_movers(&buckets, 10)
                .iter()
                .map(|m| m.h3_cell)
                .collect::<Vec<_>>(),
            vec![3, 6, 9]
        );
    }

    #[test]
    fn cell_series_is_dense_and_scoped_to_one_cell() {
        let buckets = vec![
            mover_bucket(1, 0, 0.5, 3, 1.0),
            // slot 1 deliberately absent for cell 1 -> a real zero, not a gap
            mover_bucket(1, 2 * BUCKET_SECS, 0.5, 5, 1.0),
            mover_bucket(2, BUCKET_SECS, 0.5, 99, 1.0), // other cell
        ];
        let series = cell_series(&buckets, 1, (0, 3 * BUCKET_SECS));
        assert_eq!(series, vec![3, 0, 5, 0]);
        assert!(
            cell_series(&buckets, 7, (0, 3 * BUCKET_SECS))
                .iter()
                .all(|&v| v == 0)
        );
        assert!(cell_series(&buckets, 1, (0, 0)).is_empty());
    }

    #[test]
    fn cell_series_counts_attention_and_events_and_ignores_out_of_window() {
        let mut inside = mover_bucket(1, BUCKET_SECS, 0.5, 2, 1.0);
        inside.attention_count = 4;
        let outside = mover_bucket(1, -5 * BUCKET_SECS, 0.5, 50, 1.0);
        let series = cell_series(&[inside, outside], 1, (0, 2 * BUCKET_SECS));
        assert_eq!(series, vec![0, 6, 0]);
    }

    // ---- divergence ---------------------------------------------------

    /// Cell with both components present (so it is comparable) at the given
    /// scores.
    fn comp(cell: u64, attention: f32, unrest: f32) -> CellComponents {
        CellComponents {
            h3_cell: cell,
            attention,
            unrest,
            has_attention: true,
            has_events: true,
        }
    }

    fn assert_divergence(got: &[(u64, Option<f32>)], want: &[(u64, Option<f32>)]) {
        assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g.0, w.0, "cell order: {got:?} vs {want:?}");
            match (g.1, w.1) {
                (Some(a), Some(b)) => assert!(
                    (a - b).abs() < F32_EPS,
                    "cell {}: {a} != {b} ({got:?})",
                    g.0
                ),
                (None, None) => {}
                _ => panic!("cell {}: {:?} != {:?}", g.0, g.1, w.1),
            }
        }
    }

    /// Golden case, hand-computed. Four comparable cells with attention
    /// descending and unrest ascending, so the two rank orders are exact
    /// mirrors; with n = 4 the normalized ranks are 0, 1/3, 2/3, 1.
    ///
    ///   cell  att   unrest | att rank  unrest rank  divergence
    ///     10  0.90   0.10  |    1        0            +1
    ///     20  0.70   0.30  |    2/3      1/3          +1/3
    ///     30  0.50   0.50  |    1/3      2/3          -1/3
    ///     40  0.30   0.70  |    0        1            -1
    ///     50  0.95   0.00  | attention-only -> no comparison
    #[test]
    fn divergence_ranks_golden() {
        let mut cells = vec![
            comp(10, 0.90, 0.10),
            comp(20, 0.70, 0.30),
            comp(30, 0.50, 0.50),
            comp(40, 0.30, 0.70),
        ];
        cells.push(CellComponents {
            h3_cell: 50,
            attention: 0.95,
            unrest: 0.0,
            has_attention: true,
            has_events: false,
        });
        assert_divergence(
            &divergence_ranks(&cells),
            &[
                (10, Some(1.0)),
                (20, Some(1.0 / 3.0)),
                (30, Some(-1.0 / 3.0)),
                (40, Some(-1.0)),
                (50, None),
            ],
        );
    }

    #[test]
    fn divergence_ties_share_the_average_rank() {
        // Attention identical across all three: that component carries no
        // ordering information, so every cell sits at the midpoint of it and
        // the divergence is driven entirely by unrest.
        let cells = vec![comp(1, 0.5, 0.2), comp(2, 0.5, 0.4), comp(3, 0.5, 0.6)];
        assert_divergence(
            &divergence_ranks(&cells),
            &[(1, Some(0.5)), (2, Some(0.0)), (3, Some(-0.5))],
        );
    }

    #[test]
    fn divergence_is_independent_of_input_order() {
        let cells = vec![
            comp(10, 0.90, 0.10),
            comp(20, 0.70, 0.30),
            comp(30, 0.5, 0.5),
        ];
        let mut shuffled = vec![cells[2], cells[0], cells[1]];
        shuffled.swap(0, 2);
        assert_divergence(&divergence_ranks(&cells), &divergence_ranks(&shuffled));
    }

    #[test]
    fn incomparable_cells_do_not_shift_the_distribution() {
        let base = vec![comp(1, 0.9, 0.1), comp(2, 0.1, 0.9)];
        let mut with_gaps = base.clone();
        // An events-only and an attention-only cell, both at extremes: if
        // either entered the ranking it would move cells 1 and 2 off ±1.
        with_gaps.push(CellComponents {
            h3_cell: 3,
            attention: 0.0,
            unrest: 1.0,
            has_attention: false,
            has_events: true,
        });
        with_gaps.push(CellComponents {
            h3_cell: 4,
            attention: 1.0,
            unrest: 0.0,
            has_attention: true,
            has_events: false,
        });
        assert_divergence(
            &divergence_ranks(&with_gaps),
            &[(1, Some(1.0)), (2, Some(-1.0)), (3, None), (4, None)],
        );
    }

    #[test]
    fn single_comparable_cell_is_neutral_not_extreme() {
        // One cell has no distribution to rank against; neutral is the only
        // honest answer, and it must not read as "maximally covered".
        assert_divergence(&divergence_ranks(&[comp(7, 0.99, 0.01)]), &[(7, Some(0.0))]);
        assert!(divergence_ranks(&[]).is_empty());
    }

    #[test]
    fn cell_components_absorb_takes_peaks_and_ors_presence() {
        let mut c = CellComponents::new(42);
        assert!(!c.comparable());

        let mut b1 = RegionBucket::empty(42, 0);
        b1.attention_score = 0.4;
        b1.unrest_score = 0.7;
        b1.attention_count = 2;
        c.absorb(&b1);
        assert!(!c.comparable(), "attention alone is not comparable");

        let mut b2 = RegionBucket::empty(42, BUCKET_SECS);
        b2.attention_score = 0.9;
        b2.unrest_score = 0.2;
        b2.event_count = 1;
        c.absorb(&b2);

        assert!((c.attention - 0.9).abs() < F32_EPS);
        assert!((c.unrest - 0.7).abs() < F32_EPS);
        assert!(c.comparable());
    }

    // ---- docs/SIGNAL_MODEL.md, enforced ----------------------------------

    /// The A0 claim in its strongest form: take a scored bucket, add chatter
    /// to it, and *nothing* generic may move. Asserted directly rather than
    /// inferred from a zero weight, because the old failure was a count term
    /// that no weight could reach.
    #[test]
    fn chatter_contributes_nothing_generic() {
        let base = vec![
            score_ev(EventKind::NewsAttention, 5, 1_000),
            score_ev(EventKind::Conflict, 5, 2_000),
        ];
        let mut with_chatter = base.clone();
        for ts in [1_100, 1_200, 1_300, 1_400] {
            with_chatter.push(ScoreEvent {
                volume_count: 500,
                ..score_ev(EventKind::Chatter, 5, ts)
            });
        }

        let a = score_buckets(&base);
        let b = score_buckets(&with_chatter);
        assert_eq!(a.buckets.len(), 1);
        assert_eq!(b.buckets.len(), 1);
        let (a, b) = (&a.buckets[0], &b.buckets[0]);

        assert_eq!(a.event_count, b.event_count, "unrest count");
        assert_eq!(a.attention_count, b.attention_count, "attention count");
        assert_eq!(a.article_count, b.article_count, "article total");
        assert_eq!(a.source_count, b.source_count, "attention source count");
        assert_eq!(a.distinct_outlets, b.distinct_outlets, "outlet diversity");
        assert_eq!(a.unrest_score, b.unrest_score);
        assert_eq!(a.attention_score, b.attention_score);
        assert_eq!(a.spike_score, b.spike_score);
        assert_eq!(a.baseline, b.baseline);
        assert_eq!(a.combined_score, b.combined_score);
    }

    /// An official alert is an authority announcing a hazard, not an
    /// occurrence of unrest. This is the M9 behaviour change: NOAA used to
    /// normalize to `Disruption` and land in the unrest branch.
    #[test]
    fn official_alerts_do_not_score_as_unrest() {
        let quiet = score_buckets(&[score_ev(EventKind::NewsAttention, 5, 1_000)]);
        let alerted = score_buckets(&[
            score_ev(EventKind::NewsAttention, 5, 1_000),
            score_ev(EventKind::Alert, 5, 1_500),
            score_ev(EventKind::Alert, 5, 2_500),
        ]);
        assert_eq!(alerted.buckets[0].event_count, 0);
        assert_eq!(alerted.buckets[0].unrest_score, 0.0);
        assert_eq!(
            quiet.buckets[0].combined_score, alerted.buckets[0].combined_score,
            "alerts must not move the headline score"
        );
        // …but they are still counted, in their own family.
        let alerts = alerted
            .family_buckets
            .iter()
            .find(|f| f.family == SignalFamily::OfficialAlert)
            .expect("alerts are recorded in their own family bucket");
        assert_eq!(alerts.record_count, 2);
    }

    #[test]
    fn family_buckets_count_each_family_in_its_own_unit() {
        let events = vec![
            ScoreEvent {
                volume_count: 9,
                ..score_ev(EventKind::NewsAttention, 5, 1_000)
            },
            ScoreEvent {
                volume_count: 300,
                ..score_ev(EventKind::Chatter, 5, 1_100)
            },
            ScoreEvent {
                volume_count: 1,
                ..score_ev(EventKind::Protest, 5, 1_200)
            },
        ];
        let scored = score_buckets(&events);
        let by_family: BTreeMap<SignalFamily, (u32, u64)> = scored
            .family_buckets
            .iter()
            .map(|f| (f.family, (f.record_count, f.volume_count)))
            .collect();

        assert_eq!(by_family[&SignalFamily::MediaAttention], (1, 9));
        assert_eq!(by_family[&SignalFamily::Chatter], (1, 300));
        assert_eq!(by_family[&SignalFamily::RecordedEvent], (1, 1));
        assert!(!by_family.contains_key(&SignalFamily::OfficialAlert));
        // The 300 posts appear nowhere near the 9 articles.
        assert_eq!(scored.buckets[0].article_count, 9);
    }

    /// Silence detection needs a family to be able to go quiet against its
    /// own history, which the single combined baseline could never express.
    #[test]
    fn each_family_gets_its_own_baseline() {
        let day = 86_400;
        let mut events = Vec::new();
        for d in 0..10 {
            events.push(score_ev(EventKind::NewsAttention, 5, d * day + 100));
            for n in 0..4 {
                events.push(score_ev(EventKind::Chatter, 5, d * day + 200 + n));
            }
        }
        let scored = score_buckets(&events);
        let att = scored
            .family_baselines
            .iter()
            .find(|b| b.family == SignalFamily::MediaAttention && b.tod_bucket == 0)
            .expect("attention baseline");
        let chat = scored
            .family_baselines
            .iter()
            .find(|b| b.family == SignalFamily::Chatter && b.tod_bucket == 0)
            .expect("chatter baseline");
        assert_eq!(att.baseline, 1.0);
        assert_eq!(chat.baseline, 4.0);
        assert_eq!(att.sample_days, chat.sample_days);

        // The generic baseline saw the attention record only.
        let generic = scored
            .baselines
            .iter()
            .find(|b| b.tod_bucket == 0)
            .expect("generic baseline");
        assert_eq!(generic.baseline, 1.0);
    }
}
