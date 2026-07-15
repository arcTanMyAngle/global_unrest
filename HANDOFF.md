# Session handoff — Live Earth Signals

Last session: 2026-07-14. **M0 + M1 + M2 + M3 complete and verified.**
Next session starts **Milestone 4** (services: axum API + ingest worker in
Docker Compose, Parquet handoff). Read this file, then
[CLAUDE.md](CLAUDE.md), then skim [docs/PLAN.md](docs/PLAN.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — git initialized, **no remote yet** (CI workflow committed but dormant until pushed) |
| Commits | Clean PR-sized commits through M3 (`git log --oneline`), working tree clean |
| Tests | 114 pass (`cargo test --workspace`), clippy `-D warnings` clean, fmt clean; criterion benches in `crates/analytics/benches` |
| App | Verified 2026-07-14: offline boot loads 11043 fixture events; online mode against a dead endpoint degrades gracefully (cached data stays, "degraded — showing cached data" + backoff) |
| Brief | `../prompt_1.md` (original project prompt, outside the repo) |
| Plan | [docs/PLAN.md](docs/PLAN.md) — user-approved plan |

## Milestone 3 — done (live GDELT). What shipped

Committed as PR-sized steps (see `git log`):

1. **`source-gdelt::doc`** — DOC 2.0 `artlist` JSON client: URL builder,
   response parsing, normalize → `NewsAttention` at **country precision**
   (DOC gives only the *source country*; honest + matches the precision
   contract). Golden tests on a committed synthetic `artlist` fixture.
2. **`source-gdelt::events`** — 15-minute Events CSV-zip dumps:
   `lastupdate.txt` → unzip (pure-Rust DEFLATE) → 61-column rows → discrete
   CAMEO events. Keeps only unrest signals (protest/material-conflict), skips
   cooperative/weak-verbal rows; malformed rows → `ingest_log`. Golden test +
   `.CSV.zip` roundtrip.
3. **`source-gdelt::sched`** — `governor` rate limiter + exponential backoff
   (honors `Retry-After`) + feed-cadence/backfill helpers. Deterministic
   tests (fake clock).
4. **App online loop** — `ingest.rs` became a long-lived worker: fixtures
   (offline base) + a `select!` loop that polls GDELT on cadence and streams
   incremental batches. `GDELT live` toggle, `↻` fetch-now. Dedup verified
   (headless acceptance test: re-fetch inserts 0, buckets unchanged).
5. **Retention** — storage prunes events past *N* days on ingest before
   rescoring (`IngestReport.pruned`, `set_retention`); ≥ 28 days keeps
   baselines warm. UI menu + `LES_RETENTION_DAYS`.
6. **Source status + graceful degradation** — inspector "Live source" panel
   (state, last/next fetch, attribution); network kill → degraded + cached
   data + backoff. Verified live on this offline machine.

**M3 acceptance (plan §12) — all met**: live ingest within rate limits;
network kill ⇒ graceful cached degradation with a status indicator; dedup
verified on re-fetch.

### Deferred (optional stretch — not part of §12 acceptance)

- **walkers 0.56 slippy-tile online basemap** (plan §11 step 7). Deferred:
  it needs live OSM tiles (unverifiable on an offline machine), the OSM tile
  policy must be nailed down first (SAFETY doc has a placeholder row), and
  walkers renders **Web-Mercator** tiles while our renderer is a cached
  **equirectangular** layer library — mixing the two projections in one
  viewport is a real design task, not a drop-in. Pick this up as its own PR
  when online and when a projection strategy is decided (either reproject our
  layers to Web Mercator, or keep tiles as a separate optional view).

## How M3 works (read before touching source-gdelt / the ingest loop)

`source-gdelt` has **two independent, keyless paths** and pure, offline-tested
parsing/normalization; only `GdeltSource::fetch` (DOC) and `fetch_events`
(dumps) touch the network:

- DOC 2.0 artlist JSON → `NewsAttention`, country-precision (source country
  via `country::resolve`; unknown countries fail per record). Themes come from
  the query. `source_event_id` = article URL (dedup key).
- Events 2.0 CSV dumps → discrete events; CAMEO root 14 → Protest, 15–16 →
  Disruption, 17–20 → Conflict, else skipped (not stored, not failed).
  Coordinates + geo-type → precision; Goldstein → severity; FIPS → ISO-A3.
  `source_event_id` = GLOBALEVENTID.

The app worker (`ingest.rs`) rate-limits, fetches DOC (last 60 min,
overlapping) + the latest Events dump each cycle, normalizes, and hands
`(events, failures)` to the UI, which ingests them through the storage actor.
**Storage dedups by event id**, so overlapping re-fetches never double-count.
On both fetches failing the worker sets a degraded `SourceStatus` and backs
off; the last-known data stays on screen. Retention pruning runs in
`do_ingest` before `rebuild_buckets`, so buckets/baselines reflect exactly the
retained events. Full scoring model unchanged (docs/SCORING.md).

## Milestone 4 — next up (services)

Per the approved plan §7/§10/§11. Hard rule: **DuckDB is single-writer per
file across processes** — the desktop and services must never share a
`.duckdb` read-write. The handoff surface is the immutable date-partitioned
Parquet the M2 export already produces (`StorageHandle::export_parquet`, same
layout the worker will publish). Suggested PR-sized order:

1. Define the API contract first (types shared via a small crate or an
   OpenAPI-ish doc) before splitting api/worker.
2. `services/workers`: ingest worker owns its DuckDB, publishes immutable
   Parquet partitions (reuse the export layout). Fixtures + GDELT paths reused
   from the crates.
3. `services/api`: axum read API over the published Parquet (DuckDB
   `read_parquet`), no shared writer.
4. Docker Compose (worker + api; WSL2 on Windows). Keep the desktop a native
   binary; it can optionally consume the API.
5. Docs: ARCHITECTURE cross-process section is already written for this.

M4 acceptance (plan §12): `docker compose up` serves the API; the desktop
consumes it; no shared-writer DuckDB anywhere.

## Landmines and quirks (learned the hard way)

- **egui 0.35 API**: `App::ui(&mut self, ui, frame)`; unified
  `egui::Panel::top/bottom/right(id)`; `CentralPanel::default_margins()`;
  `egui::Window` still takes `&Context`; **menu close is `ui.close()`**.
  eframe 0.35 rides **wgpu 29** — do not bump wgpu independently.
- **duckdb crate** `1.10504.0` = DuckDB 1.5.4. Connection is `!Sync` — all
  access via the storage actor thread. u64 via i64 bit-cast. DuckDB **cannot
  ALTER TABLE ADD non-null columns** — migrations recreate derived tables.
- **M3 deps**: reqwest 0.12 **rustls-tls, `default-features=false`** (no
  OpenSSL/native-tls, no http2) keeps CI clean on both OSes; `zip` 6 needs
  **`deflate-flate2` plus a direct `flate2` dep** or it fails to pick a DEFLATE
  backend; `governor` 0.10 (`FakeRelativeClock` for tests); tokio needs `net`
  for the worker's IO driver (`enable_all`).
- **GDELT DOC has no per-article coordinates** — normalize to source-country
  precision, never invent a point. Country tables live in
  `source-gdelt::country` (name→ISO3+centroid and FIPS→ISO3; FIPS≠ISO traps:
  AU/AS, CH/SZ, CI). Unknown countries fail per record (logged), never guessed.
- **Retention assumes forward-moving ingest** — the online loop only sends
  recent windows. Re-sending events already past the cap re-inserts then
  re-prunes them (churn); don't build a caller that does that.
- App data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`. Env
  overrides: `LES_DATA_DIR`, `LES_FIXTURES_DIR`, `LES_ONLINE`,
  `LES_RETENTION_DAYS`, `LES_GDELT_DOC_ENDPOINT` / `LES_GDELT_EVENTS_URL`.
  `ingest_log` grows 2 rows per app start (planted malformed fixtures re-log).
- First cold build compiles DuckDB C++ (minutes) plus the reqwest/rustls tree;
  cached in `target/` — don't `cargo clean` casually.
- **GUI verification on this machine**: see `.claude/skills/run/SKILL.md`.
  The eframe window opens small/offset and `SetForegroundWindow` is blocked by
  focus-stealing prevention; **if another app keeps taking foreground, the
  user is at the machine — stop sending input.** For M3 the graceful-
  degradation path was verified headlessly via `LES_ONLINE=1` +
  `LES_GDELT_*` pointed at a dead port (no clicks needed).

## Quality gates (run after every step; CI runs the same)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
