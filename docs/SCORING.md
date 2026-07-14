# Scoring

Transparent, non-ML scoring. Every component is a pure function in
`crates/analytics` (`scoring.rs`), stored separately on `RegionBucket`, and
shown separately in the UI. The combined number is never presented without
its parts. Every constant is named in `analytics::weights` — the values
below are documentation of that module, not a second source of truth.

## Components

```
attention_score =
    log(article_count + 1)            (normalized: /ln(1+100), clamped)
  * recency_weight                    (2^(−mean_age / 24 h))
  * source_diversity_weight           (ln(1+outlets)/ln(1+8), clamped)
  * theme_weight                      (1.0 high-signal theme, else 0.6)
  * location_confidence               (mean, 0–1)

unrest_score =                        (term weights sum to 1)
    0.30 · event_count_weight         (ln(1+n)/ln(1+10), clamped)
  + 0.25 · event_type_weight          (max kind weight: conflict 1.0,
                                       protest 0.7, disruption 0.5, other 0.3)
  + 0.10 · recency_weight
  + 0.20 · severity_weight            (mean severity; missing = 0)
  + 0.15 · location_precision_weight  (share of city/exact records)

spike_score =
    0.5 + clamp(log2((current + ε) / (baseline + ε)), ±3) / 6
    with ε = 0.5 — i.e. [0, 1] with 0.5 neutral; ⅛× and 8× baseline saturate

combined_signal =
    0.40 * attention_score
  + 0.45 * unrest_score
  + 0.15 * spike_score
```

Every component is normalized to **[0, 1]**, so `combined_signal` is also in
[0, 1] and the UI renders each as a plain bar. `attention_score` consumes
only `NewsAttention` observations; `unrest_score` only discrete event
records (see DATA_MODEL.md "Counting semantics" — mixing double-counts).

## When scores are computed (M2 design, as implemented)

Scores are computed **per (H3 res-3 cell × 6-hour bucket)** at ingest time
(`analytics::score_buckets`, persisted by the storage rebuild) and each
bucket is scored **as of its own end**:

- recency ages are measured against the bucket's end (so intra-bucket decay
  only), and
- the spike baseline is the trailing median **as of that bucket's day** —
  replaying the timeline shows what a live view would have shown then, and
  early days are honestly cold-start.

For display, the inspector composes the window's stored bucket scores with
`analytics::compose_window`: each bucket is weighted by the recency of its
end relative to the **window end** (the replay's "now"), empty bucket slots
count as zero signal for attention/unrest (silence is data), and spike
averages over non-empty buckets only (no records ⇒ no ratio).

## Baseline & spike

- Bucketing: **6-hour** time-of-day buckets (00–06, 06–12, 12–18, 18–24 UTC).
  Hour-level buckets over a 28-day window yield only 28 samples — too noisy.
- Baseline statistic: trailing 28-day **median** of the cell's record count
  per (cell, time-of-day slot). News counts are heavy-tailed; means chase
  outliers. Days inside the store's coverage with no records count as **0**
  — a quiet day is an observation, not a gap.
- Spike: clamped log-ratio (above), so a 0→small change doesn't explode.
- Cold start: fewer than `MIN_BASELINE_DAYS = 7` days of history behind a
  bucket ⇒ spike is forced neutral (0.5), the bucket carries
  `spike_cold_start`, and the UI badges the region **low confidence**.
- The `baselines` table persists the *current* medians (trailing window
  ending on the newest data day) per (cell, tod) with their `sample_days`,
  for M3 live use and inspector context.

## Theme filtering interaction

With a theme filter active, heatmap buckets are recomputed over only the
matching events — including baselines — so a theme's spike reads "unusual
for this theme here", not against the all-signal baseline. The inspector's
score bars always describe the whole cell (scores are stored quantities).

## Validation

Every component has **hand-computed golden tests** (the expected value is
worked out by hand in the test comment — `scoring.rs`, plus a full-bucket
golden and a window-composition golden in `lib.rs`). Baseline/spike
additionally have synthetic series tests: flat series ⇒ spike exactly
neutral; injected burst ⇒ 0.9438… (golden); cold-start store ⇒ neutral +
flagged. The E2E pipeline test asserts the stored buckets equal the
analytics reference bitwise and that the fixtures' scripted Paris spike
(days 20–23) registers > 0.8.

## Milestone status

- **M1**: raw counts only — done.
- **M2 (current)**: full components, baselines, spike detection, cold-start
  badges, window composition — **implemented as described above**.
