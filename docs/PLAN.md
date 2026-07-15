# Approved plan (vendored)

> User-approved 2026-07-13 (original at
> `~/.claude/plans/prompt-1-md-recursive-rossum.md`; brief at
> `../prompt_1.md` outside the repo). Vendored so future sessions don't
> depend on machine-local paths. **Status: M0 + M1 + M2 + M3 complete
> 2026-07-14** (§12-M1/M2/M3 acceptance verified; the walkers slippy-tile
> stretch in §11 step 7 is deferred — see HANDOFF.md). Next: M4 (services).
> Version pins in §3 were correct as of 2026-07; re-verify before bumping.

## Context

The brief asks for a desktop-first, Rust-based geospatial dashboard that
visualizes global news attention and unrest/event signals from public or
authorized sources (GDELT-style, ACLED-style if authorized), with
transparent non-ML scoring, a time slider, and a region inspector.
Milestone 1 must run 100% offline from fixture data; live APIs come only
after M1 works. The 8 planning roles requested (architecture, data,
geo-rendering, analytics, storage, safety, devops, test) were consolidated
into one design pass plus a Plan-agent review whose corrections (crate
versions, DuckDB concurrency, egui perf architecture, GDELT API reality)
are baked in below.

## 1. Executive summary

Cargo workspace `live-earth-signals/`. Milestone 1 is a fully offline
pipeline: synthetic fixtures → normalize into `GeoTemporalEvent` → DuckDB →
eframe (egui 0.35 / wgpu 29) desktop app with a dark 2D equirectangular
world map (bundled Natural Earth GeoJSON), H3-cell heatmap, precision-aware
event markers, time slider, and region inspector. Rendering uses **cached
epaint meshes** (never per-frame path tessellation). Storage uses a
**dedicated actor thread owning the DuckDB connection** (it is `!Sync`),
with rusqlite for settings. Scoring is transparent and component-visible;
full scoring depth (baselines, spike, confidence badges, Parquet export)
lands in M2, GDELT live in M3, Dockerized services in M4 (handoff via
immutable Parquet partitions — DuckDB is single-writer per file), ACLED +
optional layers in M5.

## 2. Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) — the diagram and threading model
there are the implemented version of this plan section.

Data flow: `RawRecord` (per-source enum) → fallible `normalize()` →
`GeoTemporalEvent` (successes) + `ingest_log` rows (failures) → DuckDB
`events` → aggregation → `region_buckets` (H3 res-3 × 6h bucket) →
renderer layers + inspector.

## 3. Repository tree & pins

Tree as implemented (see repo). Key pins (workspace root `Cargo.toml`):
eframe/egui **0.35** (wgpu 29 — do NOT bump wgpu until eframe does),
duckdb **1.10504.0** (= bundled DuckDB 1.5.4, `1.MMmmpp.x` scheme),
rusqlite 0.40 bundled, h3o 0.10, geojson **1.0** (breaking API vs 0.24),
geo 0.33, earcutr 0.5, governor 0.10 (M3), chrono, tokio, serde,
`directories`, `tracing`. Rust 1.96, edition 2024.

## 4. Data model

Implemented in `crates/core-types`; documented in
[DATA_MODEL.md](DATA_MODEL.md). Key decisions: deterministic FNV-1a id of
(source, source_event_id) for idempotent re-ingest; `NewsAttention` records
are attention *observations* (never mixed with discrete events in
counting); **precision rendering contract** (Country/Admin1 shade regions,
only City/Exact render as points); `RegionBucket` physical key is H3 res-3
only (country rollups are queries); baselines use 6-hour time-of-day
buckets with a trailing 28-day median.

## 5. Source adapter interfaces

`SignalSource` trait in core-types (`async fn` in trait; enum dispatch, not
`Box<dyn>`); `RawRecord` is a self-contained enum. Normalization fallible
per record → `ingest_log`. GDELT reality for M3: DOC 2.0 API is keyless
JSON REST; **Events/Mentions/GKG are 15-minute CSV-zip dumps** — two code
paths in source-gdelt. ACLED behind a cargo feature + `ACLED_API_KEY`,
disabled by default.

## 6. Rendering approach

eframe (wgpu default backend; glow is the documented driver fallback).
Renderer crate is an **egui layer library**: cached `GeoMesh` in lon/lat
(earcut for country fills, centroid fans for H3 cells, batched diamond
quads for markers), affine transform to screen only on viewport change,
world-copy offsets across ±180°. `egui_wgpu::CallbackTrait` is the escape
hatch; nothing should need it before M3. Bevy rejected; `walkers 0.56`
(tracks egui 0.35) is the M3+ online slippy-tile path.

## 7. Storage plan

DuckDB (bundled) for analytics — SQL window functions fit baselines/spike;
`COPY TO Parquet` native. **Single-writer rule**: desktop owns its DB
through M3; at M4 the worker owns the ingest DB and publishes immutable
date-partitioned Parquet as the handoff. Storage actor thread owns the
connection; UI polls `Reply` handles. Versioned `.sql` migrations against
`schema_version`. Appender for bulk inserts; rusqlite for settings.

## 8. Scoring plan (M2; aggregation-only shipped in M1)

Formulas exactly as the brief (see [SCORING.md](SCORING.md)); components
stored separately on `RegionBucket` and shown separately in the UI:

- attention = log(articles+1) × recency_w × source_diversity_w × theme_w × location_confidence
- unrest = event_count_w + event_type_w + recency_w + severity_w + location_precision_w
- spike: trailing 28-day **median** per (region, 6h time-of-day bucket);
  **clamped log-ratio**; cold-start (< MIN_BASELINE_DAYS) ⇒ neutral +
  low-confidence badge
- combined = 0.40·attention + 0.45·unrest + 0.15·spike (named constants in
  `analytics::weights`)
- Every component gets hand-computed golden tests.

## 9. Safety & privacy plan

Implemented at M0 in [SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md):
aggregate-only, metadata-not-bodies, licensing table (GDELT attribution,
ACLED authorized-only, Natural Earth PD, OSM tile policy before walkers),
coverage/geocoding bias documentation, retention, misuse review checklist.

## 10. Docker / dev setup

Native Windows (MSVC) dev; CI on windows-latest + ubuntu-latest (fmt,
clippy -D warnings, test; rust-cache). Fully headless tests. Docker only at
M4 (compose: worker + api; WSL2 on Windows). Offline fixture mode is a
permanent supported path. See [DEVELOPMENT.md](DEVELOPMENT.md).

## 11. Milestone roadmap

- **M0 — Scaffold** ✅ workspace, CI, docs, license, NE data, 35-day fixture
  generator (spikes + centroid records + 2 malformed), tracing, config dirs.
- **M1 — Offline pipeline** ✅ fixtures → DuckDB → map + heatmap +
  precision-aware markers + time slider + inspector + pan/zoom +
  empty/error states + headless E2E + perf smoke.
- **M2 — Scoring depth** ✅ components, baselines, spike, confidence
  badges, topic filters, source-diversity, Parquet export (M4-compatible
  partitioning), golden tests, criterion benches.
- **M3 — GDELT live** ✅ DOC JSON + Events CSV-zip ingestion,
  rate-limit/backoff scheduling, dedup, retention (~100k events/day),
  online-mode toggle + status + graceful degradation. Walkers tile layer
  deferred (stretch; see HANDOFF).
- **M4 — Services** ⬅ next: API contract before the split; axum api + worker
  in Docker Compose; Parquet handoff.
- **M5 — ACLED + optional layers**: feature-gated ACLED (authorized key
  only), optional NOAA/AIS/CelesTrak.

## 12. Acceptance criteria

- **M0** ✅ gates green on both OSes; generator emits 35 days schema-valid;
  five docs real.
- **M1** ✅ offline run renders everything; precision rule respected; slider
  replays 35 days; inspector works; E2E covers
  fixtures→normalize→store→query; ≥30fps @ 10k events; malformed records
  land in ingest_log and surface in UI.
- **M2**: each score component individually visible; golden tests match
  hand-computed values; spike cold-start shows low-confidence badge; export
  produces date-partitioned Parquet re-readable by DuckDB.
- **M3** ✅ live ingest within rate limits; network kill ⇒ graceful cached
  degradation with status indicator; dedup verified on re-fetch.
- **M4**: `docker compose up` serves API; desktop consumes it; no
  shared-writer DuckDB anywhere.
- **M5**: ACLED compiles out by default; with key, ingests within ToS.

## 13. Risks & mitigations

1. duckdb bundled MSVC build (slow) → workspace-level dep, CI cache, crate
   boundary. 2. eframe/wgpu lockstep churn → pins + dedicated upgrade PRs.
3. egui perf cliff → cached meshes + perf smoke test. 4. DuckDB
cross-process locks → single-writer + Parquet handoff. 5. Fixture/baseline
mismatch → 35-day generator (done). 6. GDELT centroid geocoding →
precision contract (baked into M1). 7. No CI GPU → headless pipeline
tests. 8. Coverage bias misread → component separation + badges +
disclaimers.

## 14. M0+M1 implementation steps — all 16 complete

Workspace scaffold → CI → docs → fixtures/generator → core-types →
source-fixtures → geo-utils → storage → analytics v0 → desktop shell →
basemap → markers → heatmap → time slider → inspector → E2E wiring +
verification. See `git log` for the actual commit slicing.

## Verification protocol

After each step: `cargo fmt --all --check && cargo clippy --workspace
--all-targets -- -D warnings && cargo test --workspace`. End-to-end: run
the app offline and walk the §12 checklist; the GUI verification recipe for
this machine is in `.claude/skills/run/SKILL.md`.

## Defaults adopted (approved)

MIT OR Apache-2.0 dual license; fully synthetic GDELT-schema-faithful
fixtures; wgpu default with glow as fallback only; repo root
`live-earth-signals/` inside `whats_overhead/`.
