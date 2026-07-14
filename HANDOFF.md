# Session handoff — Live Earth Signals

Last session: 2026-07-13 → 2026-07-14. **M0 + M1 complete and verified.**
Next session starts **Milestone 2** (scoring depth). Read this file, then
[CLAUDE.md](CLAUDE.md), then skim [docs/PLAN.md](docs/PLAN.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — git initialized, **no remote yet** (CI workflow is committed but dormant until pushed to GitHub) |
| Commits | 5 clean PR-sized commits (`git log --oneline`), working tree clean |
| Tests | 40 pass (`cargo test --workspace`), clippy `-D warnings` clean, fmt clean |
| App | Launches and works: 11,043 fixture events ingested, map/heatmap/markers/timeline/inspector all verified live (screenshot + scripted click on Nairobi filled the inspector correctly) |
| Brief | `../prompt_1.md` (original project prompt, outside the repo) |
| Plan | [docs/PLAN.md](docs/PLAN.md) — the user-approved plan, vendored from `~/.claude/plans/prompt-1-md-recursive-rossum.md` |

## Milestone 1 acceptance — all met

Offline `cargo run -p global-signal-desktop`: loads fixtures → normalizes
into `GeoTemporalEvent` → DuckDB → dark world map with H3 heatmap +
precision-aware markers + 6h-bucket time slider with looping replay + region
inspector (attention vs. event data always separated) + ingest-log surfacing
of the 2 planted malformed records. Headless E2E test
([apps/global-signal-desktop/tests/pipeline.rs](apps/global-signal-desktop/tests/pipeline.rs))
covers the whole data path and checks the SQL aggregation against
`analytics::aggregate_buckets`. Perf smoke test (10k-point mesh build) lives
in `crates/renderer/src/markers.rs`.

## Milestone 2 — next up (scoring depth)

Per the approved plan §8/§11/§12. Suggested PR-sized order:

1. `analytics::scoring` — attention/unrest component functions with named
   weights (`analytics::weights` already exists) + **hand-computed golden
   tests**. Formulas are locked in [docs/SCORING.md](docs/SCORING.md).
2. `core-types` — add score-component fields to `RegionBucket`
   (attention/unrest/spike/combined, each stored separately); storage
   migration `0002_*.sql` adds the columns (migration runner already
   handles versioning).
3. `storage` — populate `baselines` (table exists, empty): trailing 28-day
   **median** per (h3_cell, 6h time-of-day bucket) via DuckDB window
   functions, recomputed after ingest.
4. `analytics` — spike = clamped log-ratio vs. baseline + cold-start rule
   (`MIN_BASELINE_DAYS`, below it ⇒ neutral spike + low-confidence flag).
   Synthetic-series tests: flat ⇒ neutral, injected burst ⇒ high, cold
   start ⇒ neutral + flagged. Fixtures already span 35 days with scripted
   spikes (Paris d20–23, Nairobi d10–12, Jakarta d28–29, Lagos d31–32) so
   baselines work without regenerating.
5. `combined_signal` (0.40/0.45/0.15) computed and stored per bucket; UI
   must show the components separately, never only the combined number.
6. UI — score components in the inspector (per-component bars), confidence
   badges (cold-start spike, coarse-precision share of a cell's records).
7. UI — topic/theme filters (theme vocabulary from data; extend
   `query_points`/`query_buckets` filtering).
8. UI — source-diversity display (distinct outlets is already in the
   inspector; consider adding it as a heatmap metric option).
9. `storage` — Parquet session export via `COPY TO ... (FORMAT PARQUET,
   PARTITION_BY ...)` with **date partitioning designed for M4 reuse**
   (worker publishes the same layout later) + roundtrip test re-reading via
   DuckDB.
10. criterion benches (scoring + aggregation at 100k events) and the one
    known visual polish item: heatmap res-3 cells are tiny at world zoom —
    roll up to H3 parent res 1/2 when zoomed out (parents derived via h3o,
    never stored).
11. Docs: SCORING.md status flip, DATA_MODEL.md score fields, README
    roadmap check-off.

M2 acceptance (plan §12): every component individually visible in the
inspector; golden tests match hand-computed values; spike cold-start shows a
low-confidence badge; export produces date-partitioned Parquet re-readable
by DuckDB.

**Milestone gating still applies:** no live APIs until M2 is done and
approved; GDELT is M3. Offline fixture mode stays a permanent supported path.

## Landmines and quirks (learned the hard way)

- **egui 0.35 redesigned its API**: `eframe::App::update(ctx)` is now
  `fn ui(&mut self, ui: &mut egui::Ui, frame)`; `TopBottomPanel`/`SidePanel`
  are a unified `egui::Panel::top/bottom/right(id).show(ui, …)` (sides use
  `.default_size`, not `.default_width`); `CentralPanel::default_margins()`;
  `raw_scroll_delta` → `smooth_scroll_delta()`; `egui::Window` still takes
  `&Context`. Check the registry source before assuming pre-0.35 APIs.
- **eframe 0.35 rides wgpu 29** — wgpu 30 is out; do not bump independently.
- **geojson 1.0** uses struct variants (`GeometryValue::Polygon
  { coordinates }`) and a `Position` newtype (`.as_slice()`).
- **h3o accepts out-of-range lat/lon** (normalizes onto the sphere) —
  `geo_utils::cell_for_latlon` range-validates first; keep it that way so
  garbage fails into `ingest_log`.
- **duckdb crate versioning** is `1.MMmmpp.x` (pinned `1.10504.0` = DuckDB
  1.5.4). The connection is `!Sync` — all access goes through the storage
  actor thread. u64 ids/cells are stored as BIGINT via bit-cast.
- **DuckDB is single-writer per file across processes** — firm M4 rule:
  Parquet is the handoff surface, never a shared `.duckdb`.
- App data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`
  (`signals.duckdb` + `settings.sqlite`). Delete to reset. `LES_DATA_DIR`
  and `LES_FIXTURES_DIR` override. `ingest_log` grows by 2 rows per app
  start (the planted malformed fixtures re-log; inserts dedupe, log doesn't
  — accepted for M1, could dedupe in M2 if it annoys).
- First cold build compiles the DuckDB C++ amalgamation (minutes). It's
  cached in `target/` now; don't clean it casually.
- GUI verification on this machine (Windows 11, 2560×1600, DPI-scaled): see
  `.claude/skills/run/SKILL.md` — `FindWindow` with a null class fails from
  PowerShell; use `(Get-Process -Id $pid).MainWindowHandle` and call
  `SetProcessDPIAware()` before any capture/cursor Win32 calls.

## Quality gates (run after every step; CI runs the same)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
