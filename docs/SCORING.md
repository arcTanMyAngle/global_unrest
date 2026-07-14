# Scoring

Transparent, non-ML scoring. Every component is a pure function in
`crates/analytics`, stored separately on `RegionBucket`, and shown separately
in the UI. The combined number is never presented without its parts.

## Components

```
attention_score =
    log(article_count + 1)
  * recency_weight
  * source_diversity_weight
  * theme_weight
  * location_confidence

unrest_score =
    event_count_weight
  + event_type_weight
  + recency_weight
  + severity_weight
  + location_precision_weight

spike_score =
    current_window_score / baseline_for_same_region_and_time_bucket

combined_signal =
    0.40 * attention_score
  + 0.45 * unrest_score
  + 0.15 * spike_score
```

Weights are named constants in `analytics::weights`, not magic numbers.

## Milestone status

- **M1 (current)**: raw counts only — `RegionBucket` carries event/attention
  counts per (H3 res 3 cell × 6-hour bucket). No scores yet; the inspector
  shows counts.
- **M2**: full components above, plus baselines and spike detection.

## Baseline & spike design (M2, decided now)

- Bucketing: **6-hour** time-of-day buckets (00–06, 06–12, 12–18, 18–24 UTC).
  Hour-level buckets over a 28-day window yield only 28 samples — too noisy.
- Baseline statistic: trailing 28-day **median** per (region, time-of-day
  bucket). News counts are heavy-tailed; means chase outliers.
- Spike: `clamp(log((current + ε) / (baseline + ε)))` — a clamped log-ratio,
  not a raw quotient, so a 0→small change doesn't explode.
- Cold start: fewer than `MIN_BASELINE_DAYS` of history ⇒ spike is neutral
  and the bucket is badged **low confidence** in the UI.

## Attention vs. unrest separation

`attention_score` consumes only `NewsAttention` observations;
`unrest_score` consumes only discrete event records
(`Protest`/`Conflict`/`Disruption`). See DATA_MODEL.md "Counting semantics" —
mixing the two double-counts coverage.

## Validation

Every component gets **hand-computed golden tests**: a small fixed input with
the expected value worked out by hand in the test comment. Transparency is
verified, not just implemented. Baseline/spike additionally get synthetic
series tests (flat series ⇒ spike ≈ neutral; injected burst ⇒ spike high;
cold start ⇒ neutral + low confidence).
