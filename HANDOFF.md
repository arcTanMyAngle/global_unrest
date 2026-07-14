# Session handoff — Live Earth Signals

Last session: 2026-07-14. **M0 + M1 + M2 complete and verified.**
Next session starts **Milestone 3** (GDELT live). Read this file, then
[CLAUDE.md](CLAUDE.md), then skim [docs/PLAN.md](docs/PLAN.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — git initialized, **no remote yet** (CI workflow committed but dormant until pushed) |
| Commits | Clean PR-sized commits through M2 (`git log --oneline`), working tree clean |
| Tests | 72 pass (`cargo test --workspace`), clippy `-D warnings` clean, fmt clean; criterion benches in `crates/analytics/benches` |
| App | Verified live 2026-07-14: score bars + cold-start badge + baseline hint in inspector, heatmap rollup at world zoom, Parquet export written to disk and confirmed |
| Brief | `../prompt_1.md` (original project prompt, outside the repo) |
| Plan | [docs/PLAN.md](docs/PLAN.md) — user-approved plan |

## Milestone 2 acceptance — all met

- Every score component individually visible in the inspector (four bars:
  attention / unrest / spike / combined with its formula), never only the
  combined number.
- Golden tests match hand-computed values (`analytics/src/scoring.rs`,
  `baseline.rs`, full-bucket + compose-window goldens in `lib.rs`).
- Spike cold start (< 7 days history) forces a neutral spike and shows an
  amber low-confidence badge (verified live on the first fixture day);
  second badge for majority-coarse geocoding.
- Synthetic-series tests: flat ⇒ neutral, burst ⇒ 0.9438 golden, cold ⇒
  flagged. E2E asserts the scripted Paris spike (days 20–23) > 0.8 and the
  exact cold-start day boundary.
- Export produces hive `date=YYYY-MM-DD` Parquet re-read by a fresh DuckDB
  (roundtrip test + verified live at
  `%LOCALAPPDATA%\...\data\exports\session-<stamp>\`).
- Extras shipped: theme filters (vocabulary from data; themed buckets
  re-scored against the theme's own baseline), source-diversity heat
  metric, heatmap H3 parent rollup at world zoom, criterion benches
  (score_buckets 100k ≈ 55 ms).

## How M2 scoring works (read before touching analytics/storage)

One reference implementation: `analytics::score_buckets` aggregates events
and scores each bucket **as of its own end**; spike baselines are trailing
28-day medians per (cell, time-of-day slot) **as of that bucket's day**
(replay is honest; early days cold-start). Storage's `rebuild_buckets`
reads back all events and persists what analytics computes — there is
deliberately **no SQL scoring twin**. The inspector's window scores come
from `analytics::compose_window` (recency-weighted vs. window end, empty
slots dilute attention/unrest but not spike). Full spec: docs/SCORING.md.

At M3 volumes, note: every ingest re-reads the whole events table and
rescores (~55 ms per 100k events, measured). Fine for 15-minute batches at
~100k/day; revisit (incremental scoring) only if retention grows past a few
million rows.

## Milestone 3 — next up (GDELT live)

Per the approved plan §5/§11/§12. Hard rules: respect GDELT rate limits and
attribution; offline fixture mode remains a permanent supported path; no
API keys needed (GDELT is keyless). Suggested PR-sized order:

1. `source-gdelt`: DOC 2.0 API client (keyless JSON REST) — query builder,
   response → `RawRecord::GdeltDocJson`, `normalize()` to `NewsAttention`
   events (theme lowercase mapping, geo precision mapping like fixtures).
   Golden tests on committed **synthetic** response fixtures.
2. `source-gdelt`: Events 15-minute CSV-zip dump path (separate code path —
   `lastupdate.txt` → zip fetch → CSV rows → `RawRecord::GdeltEventCsv`),
   normalization to discrete events; malformed rows → `ingest_log`.
3. Rate limiting + politeness: `governor` (pinned 0.10) + backoff; fetch
   scheduler in the ingest worker (tokio) with 15-min cadence + manual
   backfill window.
4. Incremental ingest loop in the app: online mode toggle; dedup relies on
   the existing deterministic event ids — verify on re-fetch (acceptance).
5. Retention: cap events table (~100k/day ⇒ prune beyond N days), settings
   for retention length; keep baselines meaningful across pruning.
6. UI: source status indicator (last fetch, next fetch, error/degraded
   state); network kill ⇒ graceful cached degradation (acceptance).
7. Optional stretch: `walkers 0.56` slippy-tile layer (online basemap mode;
   OSM tile policy first — see SAFETY_AND_PRIVACY.md).
8. Docs: ARCHITECTURE/DATA_MODEL updates, GDELT attribution in README.

M3 acceptance (plan §12): live ingest within rate limits; network kill ⇒
graceful cached degradation with status indicator; dedup verified on
re-fetch.

## Landmines and quirks (learned the hard way)

- **egui 0.35 API**: `App::ui(&mut self, ui, frame)`; unified
  `egui::Panel::top/bottom/right(id)`; sides use `.default_size`;
  `CentralPanel::default_margins()`; `smooth_scroll_delta()`;
  `egui::Window` still takes `&Context`. eframe 0.35 rides **wgpu 29** — do
  not bump wgpu independently.
- **duckdb crate** `1.10504.0` = DuckDB 1.5.4 (`1.MMmmpp.x`). Connection is
  `!Sync` — all access via the storage actor thread. u64 via i64 bit-cast.
  DuckDB **cannot ALTER TABLE ADD non-null columns** — migration 0002
  recreates derived tables instead (they rebuild after every ingest).
- **Parquet COPY**: use `make_timestamp(µs)` for partition dates (timezone-
  setting independent); target dir must be fresh (timestamped session dirs).
- **Filters struct is no longer `Copy`** (themes Vec) — clone when saving.
  `#[serde(default)]` on new filter fields keeps old saved settings loading.
- **h3o**: accepts out-of-range lat/lon (normalizes) — `geo_utils`
  range-validates first; parents via `geo_utils::cell_parent` (display
  only, never stored).
- App data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`
  (`signals.duckdb`, `settings.sqlite`, `exports/`). `LES_DATA_DIR` /
  `LES_FIXTURES_DIR` override. `ingest_log` grows 2 rows per app start
  (planted malformed fixtures re-log; inserts dedupe, the log doesn't).
- First cold build compiles DuckDB C++ (minutes); it's cached in `target/`
  — don't `cargo clean` casually.
- **GUI verification on this machine**: see `.claude/skills/run/SKILL.md`.
  New lessons from M2 verification: the eframe window may open small and
  offset — force `ShowWindow(h, 3)` (SW_SHOWMAXIMIZED) before computing
  click coordinates; `SetForegroundWindow` is blocked by focus-stealing
  prevention — minimize+re-maximize with an Alt `keybd_event` wrapped
  around it, and verify with `GetForegroundWindow` before every synthetic
  click; **if another app keeps taking foreground, the user is at the
  machine — stop clicking.** Each PowerShell tool call is a fresh process:
  re-run `Add-Type` every time.

## Quality gates (run after every step; CI runs the same)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
