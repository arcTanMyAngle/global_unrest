# CLAUDE.md — Live Earth Signals

Desktop-first Rust geospatial dashboard visualizing global news-attention
and unrest/event signals. Civic-data research/visualization only. Currently
between **M1 (done, verified)** and **M2 (scoring depth)** — see
[HANDOFF.md](HANDOFF.md) for status and the next task list, and
[docs/PLAN.md](docs/PLAN.md) for the approved plan.

## Commands

```sh
cargo run -p global-signal-desktop                     # run the app (offline, fixtures)
cargo test --workspace                                 # all tests, headless, no GPU
cargo fmt --all --check                                # gate 1
cargo clippy --workspace --all-targets -- -D warnings  # gate 2
cargo run -p source-fixtures --bin generate-fixtures   # regenerate fixtures (deterministic; commit result)
cargo test -p global-signal-desktop --test pipeline    # E2E acceptance test
```

Run all three gates after every change. First cold build compiles bundled
DuckDB C++ (several minutes) — never `cargo clean` casually.

## Hard project rules (from the brief; non-negotiable)

- Public/authorized data sources only; no scraping restricted sources, no
  bypassing paywalls/auth/rate limits. Live APIs land only in their
  milestone (GDELT M3, ACLED M5 feature-gated + key via env var).
- No person-level identification/tracking/targeting features. Aggregate
  signals only (H3 cells, countries).
- Store headline/URL/outlet-domain **metadata only**, never article bodies.
- "Media attention" and "event data" are computed and displayed
  **separately**; score components are always shown, never only the
  combined number. Media attention ≠ ground truth.
- One milestone at a time; offline fixture mode is a permanent supported
  path (it is the regression harness).
- API keys in env vars only; `.gitignore` covers `.env` and databases.

## Architecture in 30 seconds

Cargo workspace, edition 2024, all dep versions pinned in the **root**
`Cargo.toml` (members use `dep.workspace = true`).

- `crates/core-types` — domain types, `SignalSource` trait, shared constants
  (`H3_RESOLUTION = 3`, `BUCKET_SECS = 6h`, FNV-1a event ids). No I/O.
- `crates/geo-utils` — equirectangular viewport (affine in lon/lat), H3
  assignment (range-validates before h3o), antimeridian-normalized
  boundaries, country point-in-polygon. egui-free.
- `crates/source-fixtures` — fixture reader + deterministic generator
  (SplitMix64, fixed anchor 2026-07-01). Normalization is fallible **per
  record**; failures go to `ingest_log`, never dropped.
- `crates/analytics` — pure functions; `aggregate_buckets` is the reference
  the storage SQL is tested against. M2 scoring goes here.
- `crates/storage` — DuckDB behind a dedicated **actor thread** (the
  connection is `!Sync`); versioned `.sql` migrations in `migrations/`;
  `Reply<T>` handles polled by the UI per frame; rusqlite settings store.
  DuckDB is **single-writer per file** — M4 hands off via Parquet.
- `crates/renderer` — egui **layer library**, not a wgpu engine: geometry
  tessellated once in lon/lat (`GeoMesh`), screen meshes rebuilt only on
  viewport change (affine mul-add per vertex), world-copy offsets for ±180°.
  Never add per-frame path tessellation.
- `apps/global-signal-desktop` — eframe 0.35 shell; state machine in
  `app.rs`, map widget in `map_view.rs`, panels in `panels.rs`, ingest
  worker in `ingest.rs`. UI thread never blocks on storage.
- `services/*`, `source-gdelt`, `source-acled` — stubs until M3–M5.

Precision rendering contract: only City/Exact records render as point
markers; Country/Admin1 shade regions (enforced in the storage query).

## Version gotchas (verified against installed sources)

- egui 0.35: `App::ui(&mut self, ui, frame)` root-Ui trait; unified
  `egui::Panel::top/bottom/right`; `smooth_scroll_delta()`; `Frame::NONE`;
  `rect_stroke` needs `StrokeKind`. eframe 0.35 = wgpu 29 (do not bump wgpu).
- geojson 1.0: struct variants + `Position` newtype.
- duckdb `1.10504.0` = DuckDB 1.5.4 (`1.MMmmpp.x` scheme).
- When an API surprises you, read the crate source in
  `~/.cargo/registry/src/index.crates.io-*/<crate>/` before guessing.

## Conventions

- Small PR-sized commits; commit after each step once gates pass.
- Tests colocated in each crate; hand-computed golden tests for anything
  the brief calls "transparent" (scoring).
- Comments state constraints the code can't show (threading, contracts,
  version locks) — not narration.
- All synthetic content stays obviously synthetic: `[synthetic]` headline
  prefix, `.example` outlet domains. Never imitate real publications.
