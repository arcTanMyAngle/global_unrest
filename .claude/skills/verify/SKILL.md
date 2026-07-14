---
name: verify
description: Verify a Live Earth Signals change end-to-end — quality gates, the headless E2E pipeline test, and (for UI-visible changes) a live app run. Use before committing any nontrivial change in this repo.
---

# Verifying changes

Run from the workspace root, in this order:

## 1. Quality gates (always — CI runs exactly these)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 2. Headless E2E (data-path changes: core-types, geo-utils, sources, analytics, storage)

```sh
cargo test -p global-signal-desktop --test pipeline
```

This drives real fixtures → fetch → normalize → DuckDB → queries and
asserts: >10k events, exactly 2 planted normalization failures reach
`ingest_log`, extent ≈ 35 days, **SQL bucket aggregation equals
`analytics::aggregate_buckets` row-for-row**, no coarse-precision rows
returned as points, region detail is populated, and re-ingest fully
deduplicates. If you change bucket semantics, fixtures, or the events
schema, this test is the contract — update it deliberately, never loosen it
to pass.

## 3. Live app (UI-visible changes: renderer, app crates)

Use the `run` skill: launch, screenshot, and click-verify the affected flow
(don't stop at "it compiles" — egui code can compile and render nothing).
Minimum sweep for map changes: world view renders; pan/zoom stays smooth
while dragging (cached meshes — watch for accidental per-frame
tessellation); markers/heatmap toggle correctly; a click fills the
inspector; the time slider ▶ replays.

## Repo-specific invariants worth spot-checking in review

- No per-frame tessellation added to the renderer (everything heavy goes
  through `GeoMesh` + `MeshCache`).
- Storage access only via the actor (`Reply` polling); nothing blocks the
  UI thread; no second DuckDB connection to the same file.
- Attention vs. event data never merged in counts, scores, or UI.
- Precision contract intact: Country/Admin1 records must not become points.
- New fixtures/synthetic content stays obviously synthetic (`[synthetic]`,
  `.example` domains).
