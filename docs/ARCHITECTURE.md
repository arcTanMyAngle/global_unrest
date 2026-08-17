# Architecture

Live Earth Signals is a desktop-first Rust workspace for examining media
attention, reported events, official alerts, and aggregate chatter without
collapsing them into a claim of ground truth. The desktop is live-data-only;
fixtures support tests and the worker's fixture-based smoke path.

## Desktop runtime

~~~mermaid
flowchart LR
    subgraph Sources["Live ingest sources"]
        GDELT["GDELT: attention + events"]
        ACLED["ACLED: authorized event data"]
        NOAA["NOAA/NWS: official alerts"]
        IODA["IODA: outage events"]
        BSKY["Bluesky: aggregate chatter"]
        TG["Telegram: aggregate chatter"]
    end

    subgraph App["global-signal-desktop"]
        INGEST["ingest worker"]
        UI["egui UI: map, timeline, inspector"]
        STORAGE[("storage actor: DuckDB")]
        FACTS["daily facts query"]
        DIGEST["Daily Events page"]
        CACHE["local digest cache"]
        QUERY["place + topic + time window"]
        MEDIA["media-search worker"]
        PLAYER["media page + player"]
    end

    Sources --> INGEST --> STORAGE
    STORAGE --> UI
    STORAGE --> FACTS
    FACTS -->|"explicit Generate click"| GEMINI["Google Gemini API"]
    GEMINI --> DIGEST
    DIGEST --> CACHE --> STORAGE

    QUERY --> MEDIA --> PLAYER
    GDELT -. "on-demand video lookup" .-> MEDIA
    BSKY -. "on-demand public-post lookup" .-> MEDIA
    TG -. "configured allowlist only" .-> MEDIA
~~~

The desktop enables all live-source feature paths by default. Keyless sources
can start immediately; ACLED requires authorized OAuth credentials, Telegram
requires a pre-created local MTProto session, and Daily Events needs
GEMINI_API_KEY only when the user requests a digest. The keyless Media page
queries GDELT and Bluesky only after an explicit search; its Telegram leg is
available only with that same configured local session. The desktop never
switches to fixtures when a source is unavailable.

### Threading model

- **UI thread:** owns egui state and never blocks. It sends storage commands
  and polls replies each frame.
- **Storage actor:** one OS thread owns the DuckDB connection, applies
  migrations, inserts normalized records, rebuilds buckets, serves queries,
  exports Parquet, and persists the Daily Events cache. DuckDB connections are
  not shared between threads.
- **Ingest worker:** a long-lived current-thread Tokio runtime. It receives
  online/fetch-now control messages and polls or drains sources at their own
  cadence. It sends normalized batches and source status back to the UI; only
  the UI hands batches to storage.
- **Digest worker:** a separate background task. It calls Google Gemini only
  after an explicit Generate click, returns a parsed two-section digest to the
  UI, and never opens storage itself.
- **Media-search worker:** a separate current-thread Tokio task with no
  cadence. It handles one explicit, place-scoped query at a time and returns
  transient hits to the UI. It never opens storage. Results remain in process
  memory until the next search replaces them or the app exits.

The ingest worker keeps cached data visible if a request fails. GDELT runs on
its feed cadence; NOAA every 10 minutes; IODA and Telegram every 15 minutes;
ACLED every 12 hours; Bluesky continuously accumulates and drains completed
five-minute windows. Media lookup has no background cadence.

## Data flow and evidence boundaries

Each ingest source converts its payload into a RawRecord, normalizes records
individually, then sends successful GeoTemporalEvent values and failures
separately. Failures are recorded in the ingest log rather than silently
dropped.

The storage actor turns events into H3 resolution-3, six-hour RegionBucket
rows. Attention observations and discrete event records remain separate in
both scoring and UI. Country/admin precision records shade regions; only
city/exact records can become point markers.

Daily Events is intentionally outside the ingest flow. It reads a selected
UTC day from storage, sends only bounded facts to Google Gemini when explicitly
requested, and caches one generated digest per UTC day locally. A later
explicit regeneration replaces that cache row. The output schema has separate
media-attention and event-data fields. ACLED and Bluesky/Telegram rows are
withheld from third-party processing and contribute only permitted aggregate
counts. See [SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md).

The Media page is a separate, deliberately narrow research flow rather than a
SignalSource. It does not create GeoTemporalEvents, feed the map, or write
DuckDB, Parquet, API, or cache data. An explicit user query may return a
public video link, short display label, time, and outlet/channel attribution
for one place and bounded time window. News results and unverified public
social posts are displayed separately. On Windows, supported links can use a
provider's published embed inside the app; unsupported links and non-Windows
builds keep the browser fallback. The feature does not extract media streams
or add post-level data to the aggregate chatter ingest path.

## Worker/API boundary

DuckDB is single-writer-per-file across processes. The desktop, worker, and API
never share a DuckDB file.

~~~mermaid
flowchart LR
    FIX["Fixtures: worker startup and smoke tests"]
    LIVE["GDELT + worker-enabled live sources"]
    WORKER["services/workers
owns worker.duckdb"]
    SNAP[("versioned Parquet snapshots
LATEST pointer + manifest")]
    API["services/api
read-only Axum API"]

    FIX --> WORKER
    LIVE --> WORKER
    WORKER --> SNAP --> API
~~~

The worker loads fixtures at startup, then ingests GDELT and any live-source
features compiled for the worker. It publishes an immutable snapshot whenever
a successful cycle adds data. Each snapshot has date-partitioned events and
region buckets, baselines, a manifest, and an atomically updated LATEST
pointer.

The API resolves LATEST per request, opens an in-memory DuckDB connection, and
reads Parquet only. It exposes health, metadata, buckets, paginated events,
Prometheus metrics, and OpenAPI. Middleware provides tracing, timeout,
concurrency limits, per-IP rate limiting, compression, CORS, conditional GET
using the snapshot ETag, and graceful shutdown. See [API.md](API.md) for the
contract and the ACLED public-serving guard.

## Crate map

| Crate or package | Role |
|---|---|
| crates/core-types | Domain types, source traits, event identifiers, precision, RegionBucket, and safe video/embed classification. No I/O. |
| crates/geo-utils | Equirectangular viewport math, H3 assignment, antimeridian handling, country lookup, and bundled city/country indexes. |
| crates/source-fixtures | Deterministic test fixtures and generator. It is never linked into the production desktop path. |
| crates/source-gdelt | GDELT DOC attention and Events dump client, normalization, cadence, rate limiting, and backoff. |
| crates/source-acled | Authorized ACLED OAuth adapter; never stores ACLED notes. |
| crates/source-noaa | NOAA/NWS active alerts adapter; usable polygon alerts only. |
| crates/source-ioda | Keyless IODA outage adapter with country-precision severity. |
| crates/chatter | Pure aggregate-before-storage place/topic matching and completed-window accumulation. |
| crates/source-bluesky | Bluesky Jetstream stream that drains aggregate chatter windows. |
| crates/source-telegram | Curated public-channel MTProto adapter for chatter rollups and the narrowly scoped, on-demand media leg. |
| crates/media-search | Place-scoped, on-demand GDELT/Bluesky video lookup. It is not a SignalSource and has no storage. |
| crates/analytics | Pure bucket aggregation, scores, baselines, spikes, and divergence helpers. |
| crates/storage | DuckDB actor, migrations, queries, Parquet snapshot publishing, and local settings. |
| crates/daily-digest | Daily fact types, bounded prompt construction, response parsing, and optional Google Gemini transport. |
| crates/renderer | Cached egui basemap, heatmap, alert, marker, halo, graticule, and glyph layers. |
| apps/global-signal-desktop | Eframe application that connects ingest, storage, renderer, Daily Events, and Media UI. |
| services/workers | Separate ingest process that owns a service DuckDB database and publishes snapshots. |
| services/api | Read-only Axum service over worker snapshots. |

## Rendering strategy

The renderer uses cached epaint meshes in longitude/latitude space, with cheap
affine transforms for an equirectangular viewport. Pan and zoom do not trigger
per-frame geometry tessellation. Layer-specific overlays such as alert
outlines, graticules, labels, marker glyphs, and halos have bounded work.

V1-V3 add a timeline histogram, anomaly halos, severity sizing, recency fade,
attention-vs-unrest divergence, top movers, regional sparklines, a paged event
ledger, source-shaped markers, NOAA alert overlay, legend, orientation aids,
and a reading guide. The precision contract and attention/event separation
apply to every layer.

eframe 0.36 and wgpu 30 move together. Do not upgrade wgpu independently.

## Runtime invariants

- The desktop map and its DuckDB database contain live-source records only.
- Fixtures remain a deterministic regression harness and an explicit
  worker/service smoke input.
- No source is allowed to silently fabricate a precise location. Country and
  admin records shade regions rather than rendering at guessed points.
- The API never opens the worker database; Parquet snapshots are the only
  cross-process handoff.
- Generated prose is a separate, labelled interpretation aid. It is never
  treated as an event source or rendered as a map caption.
- Media lookup is user-directed and transient. Its post-level public links
  never enter aggregate ingestion, DuckDB, Parquet, logs, or the services API.
