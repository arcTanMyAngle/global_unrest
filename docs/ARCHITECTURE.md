# Architecture

Live Earth Signals is a desktop-first Rust workspace that visualizes global
news attention and unrest/event signals from public or authorized sources.
Milestone 1 runs 100% offline from committed fixtures; live APIs arrive only
after that pipeline is proven.

## Runtime architecture (desktop, M1–M3)

```
┌─────────────────────────── Desktop app (eframe) ───────────────────────────┐
│  UI thread (egui)                                                          │
│   ├── MapView ── renderer crate: Basemap / Heatmap / Marker layers         │
│   │              (projection + cached epaint::Mesh, invalidated only on    │
│   │               viewport or time-window change)                          │
│   ├── TimelinePanel (time slider, play/replay)                             │
│   ├── InspectorPanel (region scores, components, headlines)                │
│   └── FilterPanel (event kinds, confidence)                                │
│         ▲ response channel (query results → repaint notification)          │
│         ▼ command channel (queries, ingest commands)                       │
│  Storage actor thread ── owns duckdb::Connection (it is !Sync)             │
│         │  migrations, appender inserts, bucket queries, Parquet export    │
│  Ingest thread (small tokio runtime) ── SignalSource adapters              │
│         fixtures (M1) │ GDELT DOC JSON + CSV-zip dumps (M3) │ ACLED (M5)   │
└─────────────────────────────────────────────────────────────────────────────┘
```

Data flow: `RawRecord` (per-source payload) → fallible `normalize()` per
record → `GeoTemporalEvent` successes + `ingest_log` rows for failures →
DuckDB `events` → bucket aggregation (H3 res 3 × 6-hour bucket) →
`region_buckets` → renderer layers and the inspector.

## Threading model

- **UI thread**: egui only. Never blocks on queries; sends `StorageCmd`s and
  polls response channels each frame.
- **Storage actor**: one OS thread owning the single DuckDB connection.
  `duckdb::Connection` is `!Sync`; the actor serializes all access. Results
  are sent back over channels and the actor fires a repaint notifier.
- **Ingest thread**: a long-lived worker with a current-thread tokio runtime.
  It loads the fixtures once (the permanent offline base) and then, when
  **online mode** is enabled, runs a `select!` loop driven by control messages
  (toggle online / fetch now) and the 15-minute feed cadence. Each live cycle
  is rate-limited (`governor`), fetches GDELT DOC attention + the latest Events
  dump, normalizes, and streams an incremental batch plus a `SourceStatus` back
  to the UI. Fetch failures degrade gracefully: the last-known data stays on
  screen and the worker backs off (exponential + jitter, honoring
  `Retry-After`). The worker never touches storage — the UI ingests batches, so
  dedup by event id makes overlapping re-fetches idempotent.

## Cross-process rule (M4) — implemented

**DuckDB is single-writer-per-file across processes.** The desktop and the
services never share a `.duckdb` file read-write. The M4 worker service owns
its own ingest database and publishes **immutable date-partitioned Parquet
snapshots** as the sole handoff surface; the api service reads those snapshots
directly. The M2 Parquet export layout is reused (`export_parquet`), not
rewritten.

```
┌── services/workers ──┐   publish/                    ┌── services/api ──┐
│ owns worker.duckdb   │   ├── LATEST  (pointer)  ─────│ read-only        │
│ ingest fixtures+GDELT│──▶│ v<millis>/               │ read_parquet per │
│ publish_snapshot()   │   │   manifest.json          │ request; 503 til │
│ after every cycle    │   │   events/date=…/*.parquet│ first snapshot   │
└──────────────────────┘   │   region_buckets/…       └──────┬───────────┘
                           │   baselines.parquet             │ GET /health
   keep_last prunes old ───┤   v<older>/  …                  │ /meta /buckets
   version dirs            └──────────────────────────       ▼ /events (JSON)
```

`StorageHandle::publish_snapshot(root, keep_last)` writes a new `v<millis>/`
directory (same hive-partitioned shape as `export_parquet`) plus a
`manifest.json`, then atomically repoints `root/LATEST` via
write-temp-then-rename (atomic on Windows and POSIX). Each version directory is
immutable once published, so the api reads it lock-free: every request opens a
fresh in-memory DuckDB, resolves `LATEST`, and runs `read_parquet(...)` — it
never opens a `.duckdb` file. Contract and endpoints: [API.md](API.md).

## Crate map

| Crate | Role |
|---|---|
| `crates/core-types` | Domain types: `GeoTemporalEvent`, enums, `TimeWindow`, `RegionBucket`, `SignalSource` trait, `RawRecord`. No I/O. |
| `crates/geo-utils` | Equirectangular viewport math, H3 cell assignment, antimeridian splitting, country point-in-polygon lookup. egui-free. |
| `crates/source-fixtures` | Offline fixture adapter + `generate-fixtures` bin (35 days of synthetic data). |
| `crates/source-gdelt` | M3 ✅: DOC 2.0 JSON API (`doc`) + 15-minute Events CSV-zip dumps (`events`), country/FIPS geocoding (`country`), and rate-limit/backoff/cadence policy (`sched`). Keyless; parsing/normalization pure and offline-testable, only `fetch*` touch the network. |
| `crates/source-acled` | M5: feature-gated (`live`) ACLED adapter — OAuth password/refresh grants (`ACLED_EMAIL`/`ACLED_PASSWORD`), paged windowed reads, pure normalization that never stores `notes`. |
| `crates/source-noaa` | M5: feature-gated (`live`) NOAA/NWS active alerts — keyless, US coverage; polygon alerts only (zone-scoped alerts yield no events). |
| `crates/analytics` | Bucket aggregation (M1); scoring, baselines, spike detection (M2). Pure functions. |
| `crates/storage` | DuckDB actor (migrations, appender, queries, Parquet export) + rusqlite settings DB. |
| `crates/renderer` | egui **layer library** (not a wgpu engine): cached-mesh basemap/heatmap/marker layers. |
| `apps/global-signal-desktop` | eframe shell wiring ingest → storage → layers → panels. |
| `services/workers` | M4 ✅: ingest worker owning its own DuckDB; ingests fixtures + live GDELT (reusing `source-gdelt`), publishes a versioned Parquet snapshot after every cycle. |
| `services/api` | M4 ✅: axum read API (`/health` `/meta` `/buckets` `/events`) over the worker's published snapshots via `read_parquet`; never opens a `.duckdb` file. See [API.md](API.md). |

## Rendering strategy

eframe 0.35 (wgpu backend, the default since 0.35; `glow` remains eframe's
documented fallback for problem drivers). The renderer crate caches
tessellated geometry in **lon/lat space** (triangulation via earcut for
country fills, fan triangulation for H3 cells, quads for markers) and maps it
to screen space with a cheap affine transform when the viewport changes —
equirectangular projection is affine in lon/lat, so pan/zoom is O(vertices)
of `mul-add`, and nothing re-tessellates per frame. `egui_wgpu` paint
callbacks are the escape hatch if a layer ever outgrows this; none should
before M3.

Dependency rule: eframe 0.35 rides wgpu 29. Never bump wgpu independently of
eframe; upgrades happen in one dedicated PR per egui release.

## Offline-first invariant

Fixture mode is a permanent supported path, not a development crutch: it is
the regression harness for every later milestone. Anything that breaks
`cargo run` with no network is a regression.
