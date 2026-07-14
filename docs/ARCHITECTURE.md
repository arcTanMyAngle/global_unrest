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
- **Ingest thread**: runs a current-thread tokio runtime because
  `SignalSource::fetch` is async (live sources need it); fixtures resolve
  immediately.

## Cross-process rule (M4+)

**DuckDB is single-writer-per-file across processes.** The desktop and the
services must never share a `.duckdb` file read-write. From M4 on, the worker
service owns the ingest database and publishes **immutable date-partitioned
Parquet files** as the handoff surface; the desktop (or the api service)
queries those Parquet partitions directly. The M2 Parquet export uses this
same partitioning so the code is reused, not rewritten.

## Crate map

| Crate | Role |
|---|---|
| `crates/core-types` | Domain types: `GeoTemporalEvent`, enums, `TimeWindow`, `RegionBucket`, `SignalSource` trait, `RawRecord`. No I/O. |
| `crates/geo-utils` | Equirectangular viewport math, H3 cell assignment, antimeridian splitting, country point-in-polygon lookup. egui-free. |
| `crates/source-fixtures` | Offline fixture adapter + `generate-fixtures` bin (35 days of synthetic data). |
| `crates/source-gdelt` | M3: DOC 2.0 JSON API + 15-minute CSV-zip dump ingestion. Stub until then. |
| `crates/source-acled` | M5: feature-gated (`live`), requires registered ACLED authorization. Stub. |
| `crates/analytics` | Bucket aggregation (M1); scoring, baselines, spike detection (M2). Pure functions. |
| `crates/storage` | DuckDB actor (migrations, appender, queries, Parquet export) + rusqlite settings DB. |
| `crates/renderer` | egui **layer library** (not a wgpu engine): cached-mesh basemap/heatmap/marker layers. |
| `apps/global-signal-desktop` | eframe shell wiring ingest → storage → layers → panels. |
| `services/api`, `services/workers` | M4 stubs: axum read API and ingest worker. |

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
