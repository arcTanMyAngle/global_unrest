# Visualization roadmap — original, detailed, honest

Direction (user, 2026-07-16): do **not** copy the source providers'
visualizations (ACLED's dashboard etc.) — build something *more* original
and detailed that "truly illustrates a clear and detailed picture." This
doc is the design plan for that. Execute in batches (V1 → V3); each item
lists implementation notes against the renderer architecture and its
acceptance criteria.

## Principles (non-negotiable, from the brief + SAFETY doc)

1. **Honest before pretty.** The precision rendering contract (only
   city/exact records become points; coarser records shade regions) and the
   attention-vs-events separation survive every new view. No visualization
   may imply more certainty than the data carries.
2. **Original.** We visualize *our* computed signals (components, baselines,
   spikes, diversity) — views no provider dashboard has. Never imitate a
   provider's charts, layouts, or branding.
3. **Overview → zoom → detail on demand.** The map answers "where/when is
   something unusual"; the inspector answers "what exactly, from whom, how
   confident."
4. **Performance contract intact.** Cached `GeoMesh`es rebuilt only on
   data/viewport change; ≥ 30 fps @ 10k events (perf smoke test). Any
   animation uses a bounded number of cheap epaint overlay primitives
   (circles/lines/text) — **never** per-frame tessellation of layer meshes.

## Current state (shipped M1–M5)

Dark equirectangular world map (cached meshes, world-copy at ±180°); H3
res-3 heatmap with three metrics (attention / events / diversity, log
scale, parent-cell rollup at world zoom); precision-aware kind-colored
diamond markers; 6-h-bucket time slider with playback; region inspector
(counts by kind, score components as separate bars, confidence badges, top
themes, headline metadata); theme/kind/confidence filters; per-source live
status lines with attribution.

## V1 — Timeline & anomaly reading (next session)

The highest-leverage batch: makes *time* and *anomaly* readable at a glance.

1. **Timeline histogram strip.** Replace the bare slider track with a
   stacked per-bucket mini-histogram (discrete events stacked by kind in
   the marker palette; attention observations as a thin line overlay, never
   mixed into the stack). The playhead rides on top; dragging on the strip
   scrubs; the window length renders as a translucent brush.
   - *Impl:* one storage aggregate query `(bucket_start, kind) → count`
     over the full extent, cached in the app and refreshed on ingest report;
     drawn with epaint rects in the timeline panel (no meshes). ~140 buckets
     × 5 kinds — trivial.
   - *Accept:* scrubbing stays 60 fps; counts match the inspector totals
     for the same window; attention never stacked with events.
2. **Spike halos.** Cells whose `spike_score` clears a named threshold
   (constant in `analytics::weights`) get a slowly pulsing ring at the cell
   centroid — the "what is anomalous *right now*" layer. Cold-start cells
   are excluded (no baseline → no anomaly claim; consistent with badges).
   - *Impl:* the bucket query already returns spike scores; draw epaint
     circle strokes (radius eased by `ctx.request_repaint_after` ticks,
     alpha ∝ score), capped at the top-N (e.g. 40) to bound cost. Toggle in
     the top bar; legend entry.
   - *Accept:* halos appear only on above-threshold, warm-baseline cells;
     perf smoke unchanged.
3. **Severity-weighted markers.** Marker size interpolates with `severity`
   (where present); unknown severity keeps the base size. Hover tooltip
   gains kind, severity, precision, source, and timestamp.
   - *Impl:* size is per-instance in the existing batched quad build (a
     rebuild-on-window-change path, already cached); tooltip via
     `ui.interact` hit-testing on the marker layer's screen positions.
   - *Accept:* a 25-fatality ACLED battle is visibly larger than a
     0-fatality protest; tooltips never block panning.
4. **Recency fade during playback.** While playing, event alpha decays with
   age inside the window (newest ≈ opaque, oldest ≈ 35%), so motion reads
   as motion instead of popping.
   - *Impl:* per-vertex color already set at window rebuild; playback steps
     the window per bucket, so the fade costs nothing extra per frame.
   - *Accept:* pausing shows the same data at full detail; screenshots
     without playback are unaffected.

## V2 — Signature analytical views (original to this project)

5. **Attention ↔ unrest divergence layer.** A diverging heat metric:
   cells where *media attention outruns event data* (covered but quiet) vs
   where *events outrun attention* (unrest under-covered). Built from the
   two components we deliberately keep separate — a coverage-gap map no
   provider ships, and an honest visualization of coverage bias itself.
   - *Impl:* fourth `HeatMetric` variant computed from existing
     `attention_score`/`unrest_score` per bucket (normalized ranks, not raw
     magnitudes); diverging palette with a neutral midpoint; cells missing
     either component render neutral. Legend explains the reading and its
     bias caveat (SAFETY doc cross-link).
   - *Accept:* golden test on the divergence function; legend text reviewed
     against SAFETY_AND_PRIVACY.md's coverage-bias section.
6. **"Top movers" panel.** A ranked sidebar of the strongest spike regions
   in the current window (region label, spike badge, tiny 28-day sparkline,
   Δ vs baseline); clicking flies the viewport to the cell and selects it.
   - *Impl:* sort the already-loaded buckets; sparkline from the region
     history query (below); viewport fly-to = animated lerp of the existing
     viewport struct (bounded frames, then settles — no continuous repaint).
7. **Region history sparkline + event ledger (inspector).** The inspector
   gains (a) a 28-day sparkline of the selected region's 6-h counts with
   the baseline median drawn as a band — the spike component becomes
   *visible* instead of just a number; (b) an "event ledger": the window's
   newest ~50 events for that cell (kind glyph, severity, headline label,
   source id, precision badge, timestamp; URL links where present).
   - *Impl:* two storage queries keyed by `h3_cell` (history aggregate;
     recent-events page) behind the existing `Reply<T>` pattern —
     UI never blocks. RegionDetail stays desktop-only (docs/API.md
     deliberately excludes it).
   - *Accept:* ledger paginates (no unbounded scroll); ACLED rows show the
     structural label only (never `notes`); attention rows never appear in
     the *event* ledger (separation).

## V3 — Layer identity & orientation

8. **Per-source visual identity + real legend.** NOAA alerts render as
   translucent severity-tinted cell overlays with a distinct outline style
   (weather ≠ unrest at a glance); ACLED/GDELT-Events markers keep the kind
   palette; GDELT attention stays heatmap-only. A proper legend panel
   (collapsible) documents every encoding: kind colors, severity sizing,
   halo meaning, divergence palette, precision rules.
9. **Basemap & orientation polish (offline-first).** Subtle graticule,
   country-border hierarchy (selected/hover emphasis), region labels at low
   zoom (cached egui text galleys), focus dimming outside a selected cell.
   No online tiles here — the walkers slippy-tile basemap remains the M8
   stretch (Web-Mercator projection decision + OSM tile policy first).
10. **"How to read this map" overlay.** First-run (and `?`-key) overlay
    explaining the precision contract, attention/event separation, badges,
    and biases in plain language — the SAFETY doc's honesty, surfaced in
    the UI where users actually look.

## Sequencing & guardrails

- Order: V1 (one session) → V2 (one–two sessions) → V3 (one session);
  M6 repo-hygiene items (docs/ROADMAP.md) can interleave freely.
- Every item lands as its own PR-sized commit with the three gates plus the
  perf smoke test; anything animated gets a frame-cost note in the commit.
- Screenshots of each shipped view go into the README (portfolio value) via
  the `.claude/skills/run` recipe.
- New constants (halo threshold, fade floor, ledger page size) are named in
  one place (`analytics::weights` or a new `ui::style` module), never
  inline magic numbers.
