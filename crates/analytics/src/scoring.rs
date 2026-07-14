//! Transparent score component functions (docs/SCORING.md).
//!
//! Every function is pure and every constant is named in [`crate::weights`].
//! All components are normalized to [0, 1] so the combined signal
//! (0.40·attention + 0.45·unrest + 0.15·spike) is also in [0, 1] and the UI
//! can render each component as a plain bar. Spike is centered: 0.5 = "at
//! baseline", 1.0 = ≥8× baseline, 0.0 = ≤⅛× baseline.

use core_types::EventKind;

use crate::weights;

/// Exponential decay by age: 2^(−age / half-life). Age is clamped at 0 so
/// records timestamped after the reference moment weigh 1.0, never more.
pub fn recency_weight(age_secs: f64) -> f64 {
    (-age_secs.max(0.0) / weights::RECENCY_HALF_LIFE_SECS).exp2()
}

/// ln(1+v) / ln(1+saturation), clamped to [0, 1]. The workhorse for count
/// inputs: heavy-tailed counts grow a score logarithmically until
/// `saturation`, after which more volume adds nothing.
pub fn saturating_log(value: f64, saturation: f64) -> f64 {
    (value.max(0.0).ln_1p() / saturation.ln_1p()).clamp(0.0, 1.0)
}

/// Source-diversity weight from the number of distinct outlet domains.
pub fn source_diversity_weight(distinct_outlets: u64) -> f64 {
    saturating_log(
        distinct_outlets as f64,
        weights::DIVERSITY_OUTLET_SATURATION,
    )
}

/// Theme weight: the max over the bucket's themes — any high-signal theme
/// (docs/SCORING.md list) lifts the weight to `THEME_WEIGHT_HIGH`; otherwise
/// (including no themes at all) it stays at `THEME_WEIGHT_BASE`.
pub fn theme_weight<'a>(themes: impl IntoIterator<Item = &'a str>) -> f64 {
    for theme in themes {
        if weights::HIGH_SIGNAL_THEMES.contains(&theme) {
            return weights::THEME_WEIGHT_HIGH;
        }
    }
    weights::THEME_WEIGHT_BASE
}

/// Per-kind weight for the unrest event-type term. `NewsAttention` never
/// enters unrest scoring (counting semantics, docs/DATA_MODEL.md).
pub fn kind_weight(kind: EventKind) -> f64 {
    match kind {
        EventKind::Conflict => weights::KIND_CONFLICT,
        EventKind::Protest => weights::KIND_PROTEST,
        EventKind::Disruption => weights::KIND_DISRUPTION,
        EventKind::Other => weights::KIND_OTHER,
        EventKind::NewsAttention => 0.0,
    }
}

/// attention = volume × recency × source-diversity × theme × confidence.
///
/// Inputs come from **attention observations only**:
/// - `articles`: summed article counts,
/// - `mean_age_secs`: mean record age relative to the scoring moment,
/// - `distinct_outlets`: distinct outlet domains,
/// - `theme_w`: from [`theme_weight`] over attention themes,
/// - `mean_confidence`: mean location confidence (0–1).
pub fn attention_score(
    articles: u64,
    mean_age_secs: f64,
    distinct_outlets: u64,
    theme_w: f64,
    mean_confidence: f64,
) -> f64 {
    if articles == 0 {
        return 0.0;
    }
    saturating_log(articles as f64, weights::ATTENTION_ARTICLE_SATURATION)
        * recency_weight(mean_age_secs)
        * source_diversity_weight(distinct_outlets)
        * theme_w
        * mean_confidence.clamp(0.0, 1.0)
}

/// unrest = count-term + type-term + recency-term + severity-term +
/// precision-term. Each term is a [0, 1] quantity times its named weight;
/// the five weights sum to 1, so unrest is in [0, 1].
///
/// Inputs come from **discrete event records only**:
/// - `n_events`: record count,
/// - `max_kind_weight`: max of [`kind_weight`] over the records,
/// - `mean_age_secs`: mean record age relative to the scoring moment,
/// - `mean_severity`: mean severity (missing severities count as 0),
/// - `point_precision_share`: fraction geocoded to city/exact precision.
pub fn unrest_score(
    n_events: u64,
    max_kind_weight: f64,
    mean_age_secs: f64,
    mean_severity: f64,
    point_precision_share: f64,
) -> f64 {
    if n_events == 0 {
        return 0.0;
    }
    weights::UNREST_EVENT_COUNT * saturating_log(n_events as f64, weights::EVENT_COUNT_SATURATION)
        + weights::UNREST_EVENT_TYPE * max_kind_weight.clamp(0.0, 1.0)
        + weights::UNREST_RECENCY * recency_weight(mean_age_secs)
        + weights::UNREST_SEVERITY * mean_severity.clamp(0.0, 1.0)
        + weights::UNREST_PRECISION * point_precision_share.clamp(0.0, 1.0)
}

/// spike = clamped log-ratio of current activity vs. baseline, remapped to
/// [0, 1] with 0.5 neutral: 0.5 + log2((cur+ε)/(base+ε)) / (2·span).
/// The ε keeps a 0→small change from exploding (docs/SCORING.md).
pub fn spike_score(current: f64, baseline: f64) -> f64 {
    let ratio =
        (current.max(0.0) + weights::SPIKE_EPSILON) / (baseline.max(0.0) + weights::SPIKE_EPSILON);
    let log2 = ratio
        .log2()
        .clamp(-weights::SPIKE_LOG2_SPAN, weights::SPIKE_LOG2_SPAN);
    0.5 + log2 / (2.0 * weights::SPIKE_LOG2_SPAN)
}

/// combined = 0.40·attention + 0.45·unrest + 0.15·spike. Displayed only
/// alongside its components, never alone (hard project rule).
pub fn combined_signal(attention: f64, unrest: f64, spike: f64) -> f64 {
    weights::ATTENTION * attention + weights::UNREST * unrest + weights::SPIKE * spike
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn golden_attention_score() {
        // Hand-computed:
        //   volume    = ln(19+1)/ln(100+1)          = 0.649112469029
        //   recency   = 2^(−10800/86400)            = 0.917004043205  (3 h old, 24 h half-life)
        //   diversity = ln(3+1)/ln(8+1)             = 0.630929753571
        //   theme     = 1.0                          (high-signal theme)
        //   confidence= 0.85
        //   product   = 0.319220766785
        let got = attention_score(19, 10_800.0, 3, 1.0, 0.85);
        assert!((got - 0.319220766785).abs() < EPS, "{got}");
    }

    #[test]
    fn golden_unrest_score() {
        // Hand-computed, 3 events (2 protest + 1 conflict), mean age 3 h,
        // severities (0.2, 0.0, 0.4) → mean 0.2, precision 2 of 3 as points:
        //   count    = 0.30 · ln(4)/ln(11)          = 0.30 · 0.578129652636 = 0.173438895791
        //   type     = 0.25 · 1.0 (conflict)        = 0.25
        //   recency  = 0.10 · 2^(−10800/86400)      = 0.10 · 0.917004043205 = 0.091700404321
        //   severity = 0.20 · 0.2                   = 0.04
        //   precision= 0.15 · 2/3                   = 0.10
        //   sum      = 0.655139300111
        let got = unrest_score(3, 1.0, 10_800.0, 0.2, 2.0 / 3.0);
        assert!((got - 0.655139300111).abs() < EPS, "{got}");
    }

    #[test]
    fn golden_spike_score() {
        // 12 records vs baseline 2: (12.5/2.5)=5, log2(5)=2.321928…,
        // 0.5 + 2.321928/6 = 0.886988015815.
        let got = spike_score(12.0, 2.0);
        assert!((got - 0.886988015815).abs() < EPS, "{got}");

        // Quiet period, 1 vs baseline 3: (1.5/3.5), log2 = −1.222392…,
        // 0.5 − 1.222392/6 = 0.296267929777.
        let got = spike_score(1.0, 3.0);
        assert!((got - 0.296267929777).abs() < EPS, "{got}");

        // Flat series is exactly neutral.
        assert!((spike_score(3.0, 3.0) - 0.5).abs() < EPS);

        // 4 vs baseline 0: (4.5/0.5)=9, log2(9)=3.1699… clamps to +3 → 1.0.
        assert!((spike_score(4.0, 0.0) - 1.0).abs() < EPS);

        // 0 vs baseline 4: (0.5/4.5)=1/9 clamps to −3 → 0.0.
        assert!((spike_score(0.0, 4.0)).abs() < EPS);
    }

    #[test]
    fn golden_combined_signal() {
        // 0.40·0.5 + 0.45·0.4 + 0.15·0.6 = 0.2 + 0.18 + 0.09 = 0.47.
        let got = combined_signal(0.5, 0.4, 0.6);
        assert!((got - 0.47).abs() < EPS, "{got}");
    }

    #[test]
    fn recency_halves_per_half_life_and_clamps_future() {
        assert!((recency_weight(0.0) - 1.0).abs() < EPS);
        assert!((recency_weight(86_400.0) - 0.5).abs() < EPS);
        assert!((recency_weight(2.0 * 86_400.0) - 0.25).abs() < EPS);
        // Future-dated records never weigh more than 1.
        assert!((recency_weight(-3600.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn saturating_log_bounds() {
        assert!(saturating_log(0.0, 100.0).abs() < EPS);
        assert!((saturating_log(100.0, 100.0) - 1.0).abs() < EPS);
        assert!((saturating_log(1e9, 100.0) - 1.0).abs() < EPS);
        // Single outlet: ln(2)/ln(9) = 0.315464876786.
        assert!((source_diversity_weight(1) - 0.315464876786).abs() < EPS);
    }

    #[test]
    fn theme_weight_lifts_on_high_signal_themes_only() {
        assert!((theme_weight(["flood", "transport"]) - 0.6).abs() < EPS);
        assert!((theme_weight(["flood", "protest"]) - 1.0).abs() < EPS);
        assert!((theme_weight([]) - 0.6).abs() < EPS);
    }

    #[test]
    fn kind_weights_rank_conflict_highest() {
        assert!(kind_weight(EventKind::Conflict) > kind_weight(EventKind::Protest));
        assert!(kind_weight(EventKind::Protest) > kind_weight(EventKind::Disruption));
        assert!(kind_weight(EventKind::Disruption) > kind_weight(EventKind::Other));
        assert_eq!(kind_weight(EventKind::NewsAttention), 0.0);
    }

    #[test]
    fn empty_buckets_score_zero() {
        assert_eq!(attention_score(0, 0.0, 0, 1.0, 1.0), 0.0);
        assert_eq!(unrest_score(0, 1.0, 0.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn components_stay_in_unit_range() {
        // Extreme inputs must not escape [0, 1].
        let a = attention_score(u64::MAX, 0.0, u64::MAX, 1.0, 1.0);
        let u = unrest_score(u64::MAX, 1.0, 0.0, 1.0, 1.0);
        let s = spike_score(f64::MAX, 0.0);
        for v in [a, u, s, combined_signal(a, u, s)] {
            assert!((0.0..=1.0).contains(&v), "{v}");
        }
    }
}
