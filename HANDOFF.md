# Session handoff — Live Earth Signals

Last session: 2026-07-16. **M0 + M1 + M2 + M3 + M4 complete.** M4 verified
natively (worker→Parquet→api); the `docker compose up` path is written but
unverified on this dev machine (no docker CLI installed). Next session starts
**Milestone 5** (ACLED behind a cargo feature + authorized key; optional
NOAA/AIS/CelesTrak layers). Read this file, then [CLAUDE.md](CLAUDE.md), then
skim [docs/PLAN.md](docs/PLAN.md) and [docs/API.md](docs/API.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — git initialized, **no remote yet** (CI workflow committed but dormant until pushed) |
| Commits | Clean PR-sized commits through M4 (`git log --oneline`), working tree clean |
| Tests | `cargo test --workspace` green; clippy `-D warnings` clean; fmt clean |
| App | Desktop verified 2026-07-14 (offline + graceful degradation). M4 services verified 2026-07-16 (below). |
| Brief | `../prompt_1.md` (original project prompt, outside the repo) |
| Plan | [docs/PLAN.md](docs/PLAN.md) — user-approved plan; [docs/API.md](docs/API.md) — M4 API contract |

## Milestone 4 — done (services). What shipped

Committed as PR-sized steps (see `git log`):

1. **`storage::publish_snapshot`** — versioned Parquet publish: exports the
   session to `{root}/v<millis>/` (reusing the M2 `export_parquet`
   hive-partitioned layout), writes `manifest.json`, atomically repoints
   `{root}/LATEST` (write-temp-then-rename). `keep_last` prunes older version
   dirs. Unit-tested (versioning, LATEST flip, prune, re-read).
2. **`docs/API.md`** — the read-API contract, written before the api/worker
   split (endpoints, query params, JSON shapes, error envelope, snapshot
   layout). Deliberately narrower than the desktop's storage queries —
   `RegionDetail` and `ingest_log` stay desktop-only.
3. **`services/workers`** — real ingest worker binary. Owns its **own**
   DuckDB, ingests the fixture base once, then runs the same `source-gdelt`
   rate-limit/backoff/cadence loop the desktop uses when online, and calls
   `publish_snapshot` after every cycle. `LES_ONLINE=0` = fixtures-only
   (publishes once, exits the live loop).
4. **`services/api`** — real axum read API binary. `/health` `/meta`
   `/buckets` `/events` read only the worker's snapshots via the `LATEST`
   pointer; a fresh in-memory DuckDB `read_parquet` per request (on
   `spawn_blocking`), never a `.duckdb` file. `503` until the first snapshot;
   `400` on bad params.
5. **Docker** — multi-stage Dockerfiles (build context = repo root; `cmake`
   for bundled DuckDB) + `docker-compose.yml`. The `publish` volume is the
   whole cross-process contract: rw in the worker, **ro** in the api; the
   worker's DuckDB volume is never mounted into the api. API healthcheck
   curls `/health`.

**M4 acceptance (plan §12) — met (native), Docker unverified**: worker
publishes versioned Parquet; api serves it with no shared-writer DuckDB; the
desktop can consume the same snapshots (same `RegionBucket` JSON shape). The
`docker compose up` orchestration is written but could not be run here —
**no docker CLI on this machine** — so it needs one verification run on a box
with Docker/WSL2 before calling it fully closed.

### How M4 was verified (native, no Docker)

```sh
# 1. Worker publishes a snapshot (fixtures only, no network needed):
LES_ONLINE=0 LES_WORKER_DATA_DIR=<scratch>/data LES_PUBLISH_DIR=<scratch>/publish \
  LES_FIXTURES_DIR=./fixtures cargo run -p workers
#   → ingests 11043 fixtures, writes publish/v<millis>/{manifest.json,events/,
#     region_buckets/,baselines.parquet} and publish/LATEST

# 2. API serves it:
LES_PUBLISH_DIR=<scratch>/publish LES_API_BIND=127.0.0.1:8080 cargo run -p api
curl localhost:8080/health   # {"status":"ok","snapshot":{...events:11043...}}
curl localhost:8080/meta     # time_extent + 22 themes
curl "localhost:8080/buckets?start=<s>&end=<e>"          # 2839 RegionBucket rows
curl "localhost:8080/events?start=<s>&end=<e>&kinds=protest"  # kind-filtered points
```

Confirmed: theme/kind filters, `400` (end≤start, bad kind), `503` (empty
publish dir before first snapshot).

## Milestone 5 — next up (ACLED + optional layers)

Per the approved plan §5/§11. Suggested PR-sized order:

1. **ACLED behind a `live` cargo feature** on `source-acled` (compiles out by
   default; `ACLED_API_KEY` via env var only — never committed). The crate is
   a stub today (`crates/source-acled`); `RawRecord::AcledJson` already exists
   in core-types and the `SignalSource` trait is the interface. Authorized
   access only — respect ToS and rate limits (same discipline as GDELT M3).
2. Wire ACLED into the desktop ingest loop and the worker behind the same
   feature flag; keep fixtures the permanent offline base.
3. **Optional layers** (NOAA / AIS / CelesTrak) as separate feature-gated
   sources if time allows — each with a fixture/offline path first.
4. Revisit the **deferred walkers slippy-tile basemap** (see below) if an
   online-tile design is wanted; still needs the Web-Mercator-vs-
   equirectangular projection decision.

M5 acceptance (plan §12): ACLED compiles out by default; with a key, ingests
within ToS.

## Deferred (optional stretch — not part of §12 acceptance)

- **walkers 0.56 slippy-tile online basemap** (plan §11 step 7). Needs live
  OSM tiles (unverifiable offline), an OSM tile-policy decision (SAFETY doc
  has a placeholder row), and a projection strategy — walkers renders
  Web-Mercator tiles while our renderer is a cached equirectangular layer
  library. Pick this up as its own PR when online and when the projection
  strategy is decided.

## Landmines and quirks (learned the hard way)

- **egui 0.35 API**: `App::ui(&mut self, ui, frame)`; unified
  `egui::Panel::top/bottom/right(id)`; `CentralPanel::default_margins()`;
  `egui::Window` still takes `&Context`; **menu close is `ui.close()`**.
  eframe 0.35 rides **wgpu 29** — do not bump wgpu independently.
- **duckdb crate** `1.10504.0` = DuckDB 1.5.4. Connection is `!Sync` — all
  access via one thread. The **api** honors this by opening a throwaway
  in-memory connection **inside `spawn_blocking`** per request (no shared
  connection, no cache to invalidate — snapshots are immutable). u64 via i64
  bit-cast. DuckDB **cannot ALTER TABLE ADD non-null columns** — migrations
  recreate derived tables.
- **Single-writer rule (M4)**: the worker owns its `.duckdb`; the api reads
  **only** Parquet snapshots. Never mount the worker's DB into the api, and
  never point `LES_PUBLISH_DIR` at a `.duckdb` file. `LATEST` is flipped by
  write-temp-then-rename so the api never sees a half-written pointer.
- **M4 deps**: `axum` 0.8 (services/api only). Docker builds need `cmake` in
  the builder stage (bundled DuckDB C++ compile — minutes; first cold build
  also compiles the reqwest/rustls + arrow tree).
- **M3 deps**: reqwest 0.12 **rustls-tls, `default-features=false`**; `zip` 6
  needs **`deflate-flate2` + a direct `flate2` dep**; `governor` 0.10; tokio
  needs `net` for the worker IO driver (`enable_all`).
- **GDELT DOC has no per-article coordinates** — normalize to source-country
  precision, never invent a point. Country tables in `source-gdelt::country`
  (FIPS≠ISO traps: AU/AS, CH/SZ, CI). Events keeps only CAMEO roots 14–20.
- **Retention assumes forward-moving ingest** — the online loop only sends
  recent windows. Don't build a caller that re-sends events already past the
  cap (churn: re-insert then re-prune).
- Desktop app data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`.
  The **worker** uses a *separate* namespace (`…-worker`) so the two never
  collide. `ingest_log` grows 2 rows per app start (planted malformed
  fixtures re-log).
- First cold build compiles DuckDB C++ (minutes) plus reqwest/rustls/arrow;
  cached in `target/` — don't `cargo clean` casually.
- **GUI verification on this machine**: see `.claude/skills/run/SKILL.md`.
  Focus-stealing prevention blocks `SetForegroundWindow`; **if another app
  keeps taking foreground, the user is at the machine — stop sending input.**

## Quality gates (run after every step; CI runs the same)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
