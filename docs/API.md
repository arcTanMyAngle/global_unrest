# Services API

The desktop does not currently consume this HTTP API. It has its own local
storage path; this service is a separate, narrow read surface over
worker-published snapshots.

`services/api` is a read-only axum HTTP API over the Parquet snapshots
`services/workers` publishes. It never opens a `.duckdb` file — DuckDB is
single-writer-per-file across processes (docs/ARCHITECTURE.md), so the
worker's ingest database is never shared. Each request opens a fresh
in-memory DuckDB connection and runs `read_parquet(...)` against the
snapshot named by a `LATEST` pointer file; there is no persistent
connection or cache to invalidate.

This is intentionally a **narrower** read surface than the desktop's
`StorageHandle` queries. It keeps the service contract focused without
duplicating storage's per-region headline/theme aggregation
(`RegionDetail`) against a second backend. That fuller inspector detail
stays desktop-only (direct `StorageHandle` access) until a real need to
serve it over HTTP shows up.

Daily Events cache rows and Media-page lookup hits are also desktop-only. They
never enter a worker snapshot and have no services API endpoint.

## Snapshot handoff layout (published by `services/workers`)

```
{publish_root}/
  LATEST                      -- text file: the current version name, e.g. "v1752624000123"
  v<millis>/
    manifest.json             -- {version, published_at_epoch_s, events, buckets, baselines}
    events/date=YYYY-MM-DD/*.parquet
    region_buckets/date=YYYY-MM-DD/*.parquet
    baselines.parquet
  v<older millis>/            -- kept until LES_PUBLISH_KEEP_LAST is exceeded, then pruned
```

Produced by `storage::StorageHandle::publish_snapshot` (same hive-partitioned
shape as the M2 session export). The worker publishes a new version after
fixture load at startup and after successful enabled live-source cycles that
add records;
`LATEST` is updated via write-temp-then-rename, which is atomic on both
Windows and POSIX, so the api never observes a half-written pointer. Each
version directory is immutable once published — the api can read it
without any lock.

After the startup fixture publish, the worker republishes whenever a successful
enabled source cycle adds records. This includes GDELT and any worker features
compiled for ACLED, NOAA, IODA, Bluesky, or Telegram.

## Error envelope

Non-2xx responses are `{"error": "<message>"}` with a matching HTTP status.
`503` means no snapshot has been published yet (worker hasn't completed
its first ingest cycle) — expected briefly after `docker compose up`.
`408` means the middleware's request timeout fired (see below) and `429`
means the per-IP rate limit rejected the request; both come from
`tower-http`/`tower_governor` directly rather than `ApiError`, so their
bodies don't necessarily match the `{"error": ...}` shape exactly.

## Middleware stack (M7, shipped)

Applied to every route, outermost first: request tracing (`tower-http`,
one structured log line per request/response), a `408` timeout
(`REQUEST_TIMEOUT_SECS = 30`), a concurrency cap
(`MAX_CONCURRENT_REQUESTS = 64` in-flight requests; beyond that, new
requests queue rather than opening unbounded DuckDB connections), a
per-IP token-bucket rate limit (`tower_governor`, keyed on peer IP —
`RATE_LIMIT_PER_SECOND = 10` sustained, `RATE_LIMIT_BURST = 20`; the
server binds via `into_make_service_with_connect_info::<SocketAddr>()` so
the real peer address reaches the limiter), gzip response compression,
and a permissive `GET`-only CORS policy (safe with no credentialed
requests anywhere in this api). Graceful shutdown drains in-flight
requests on Ctrl+C or (Unix) `SIGTERM` before the process exits.

**`ETag` / conditional GET.** Normal responses after a snapshot exists carry an `ETag` set to the
current snapshot version (e.g. `"v1752624000123"`) — a snapshot is
immutable once published, so an unchanged version guarantees an unchanged
body. Send `If-None-Match: "<version>"` on a repeat request; a match short
-circuits to `304 Not Modified` with no body, skipping the Parquet query
entirely.

**Conditional GET detail.** Once a snapshot exists, non-304 responses carry
its quoted ETag. A matching If-None-Match request returns 304 before the
handler and metrics layer, with no body and no ETag header. This is an
optimization for snapshot-backed clients rather than an API promise that a
304 echoes its validator.

## Endpoints

### `GET /health`

Readiness probe (used by the Compose healthcheck). Reads `LATEST` +
`manifest.json` only — no Parquet query. Always `200` once a snapshot
exists, even when it's stale — staleness is surfaced as data (`stale`,
`snapshot_age_s`) for a monitor to alert on, not folded into the status
code, so it stays distinguishable from "no snapshot at all" (`503`).
`snapshot_age_s > 3600` (`HEALTH_STALE_THRESHOLD_SECS`) sets `stale: true`
— generous headroom over the worker's normal GDELT-cadence publish
interval, not a tuned SLA.

- `200` — `{"status": "ok", "snapshot": {"version": "v...", "published_at_epoch_s": 1752624000, "events": 11043, "buckets": 812, "baselines": 8}, "snapshot_age_s": i64, "stale": bool}`
- `503` — `{"error": "no snapshot published yet"}`

### `GET /metrics`

Prometheus text-format metrics (`metrics-exporter-prometheus`, in-process
render — no separate listener/port): `http_requests_total{path,method,status}`
and `http_request_duration_seconds{path,method}`, recorded for every
request by the same middleware stack (see above).

- `200` — Prometheus exposition text, `text/plain`

### `GET /openapi.json`

The OpenAPI 3 spec for this api, generated by `utoipa` from the
`#[utoipa::path]` annotations on each handler above — this document is
meant to stay in sync with that spec mechanically, not just by convention.
Built once at startup (the route table is fixed) and served verbatim.
`RegionBucket` (the `/buckets` response) isn't schema'd in detail — see
docs/DATA_MODEL.md instead — to avoid adding an OpenAPI-only dependency to
`core-types`, which is otherwise a pure, I/O-free domain-types crate.

- `200` — OpenAPI 3 JSON

### `GET /meta`

Time extent and theme vocabulary across the whole retained snapshot (mirrors
the desktop's `StorageHandle::time_extent` + `theme_vocab`).

- `200` — `{"time_extent": {"start_epoch_s": i64, "end_epoch_s": i64} | null, "themes": [{"theme": "elections", "count": 123}, ...]}`

### `GET /buckets`

`RegionBucket` rows (core-types, unchanged JSON shape — same struct the
desktop renders) in a half-open window, optionally restricted to one cell.
No theme filtering in M4 (that requires re-running `analytics::score_buckets`
over theme-filtered events, which the desktop still does directly against
live storage; may move here later if a consumer needs it).

Query params:

| Param | Required | Notes |
|---|---|---|
| `start`, `end` | yes | Epoch seconds, half-open `[start, end)`. |
| `h3_cell` | no | Restrict to one H3 res-3 cell (decimal `u64`). |

- `200` — `RegionBucket[]` (see docs/DATA_MODEL.md for fields)
- `400` — bad/missing params
- `503` — no snapshot yet

### `GET /events`

Point-renderable event rows (mirrors `StorageHandle::query_points`): only
`city`/`exact` precision records, capped at 100k rows.

**ACLED rows are excluded by default, in SQL, regardless of what the
published snapshot contains** — ACLED data is not redistributable
(docs/SAFETY_AND_PRIVACY.md) and must never be served publicly. The
exclusion is enforced in `main.rs`, not left to the operator remembering
not to enable `acled-live` on a public worker; a private/authorized
deployment can opt back in with `LES_API_ALLOW_ACLED=1` (logs a warning on
startup when set). This is a defense-in-depth check on top of, not a
replacement for, the existing policy that a publicly-reachable worker
never runs `acled-live` at all.

**`/buckets` and `/meta` cannot be filtered the same way.** `region_buckets`
aggregates every contributing source's counts into one score per
`(h3_cell, bucket_start)` with no per-source breakdown
(`core_types::RegionBucket` has no source field), so an ACLED event folded
into a cell's `event_count`/`unrest_score` can't be subtracted back out at
the api layer — only by never including ACLED in the aggregation in the
first place. Keeping ACLED out of those figures is therefore a worker-side
policy (don't run the publicly-reachable worker with `acled-live`), not
something this api can enforce.

Query params:

| Param | Required | Notes |
|---|---|---|
| `start`, `end` | yes | Epoch seconds, half-open `[start, end)`. |
| `kinds` | no | Comma-separated `EventKind` strings (e.g. `protest,conflict`). |
| `themes` | no | Comma-separated; record matches if any theme is in the list. |
| `min_confidence` | no | `f32`, default `0.0`. |
| `offset` | no | `i64`, default `0`. |
| `limit` | no | `i64`, default `500`, clamped to `[1, 100000]`. |

**Response is a page, not a bare array** (M7): `{"total": usize, "offset":
i64, "limit": i64, "rows": [...]}`, `rows` shaped as before
(`{"id": u64, "lat": f64, "lon": f64, "kind": str, "family": str,
"location_role": str, "precision": str, "confidence": f32, "ts_epoch_s":
i64, "volume_count": u32, "headline": str | null}`). **`family` and
`volume_count` must be read together**: the volume is in that family's own
unit (articles, records, alerts, posts) and is never comparable across
families — see docs/SIGNAL_MODEL.md. `location_role` says what the
coordinates are a statement about; a `publisher_origin` row locates the
outlet, not the story. `kind` remains the within-family subtype, so `kinds=`
still filters as before; there is no `families=` parameter yet. Pages are ordered `(ts_epoch_s DESC, id DESC)` — the same
tiebreak `storage::region_events` uses, and for the same reason: without
the `id`, rows sharing a timestamp can repeat or vanish across pages.
`kinds`/`themes` are applied before pagination (so `total` and page
boundaries reflect the fully-filtered set), but the underlying SQL fetch
is still capped at the 100k-row safety valve above — if more than 100k
rows match the time window/precision/confidence filters alone, rows
beyond that cap are invisible to `total` and to every page, filtered or
not.

- `200` — `EventsPage` as above
- `400` / `503` as above

Metrics record requests that reach the metrics middleware. A conditional-GET
304 short-circuits before that layer and is therefore not represented in the
request counter.

## What the API does not expose (by design)

- Per-region headline/theme/outlet breakdown (`RegionDetail`) — desktop-only.
- `ingest_log` — not part of the Parquet handoff (only `events`,
  `region_buckets`, `baselines` are exported); failed-record debugging stays
  a worker-log/desktop concern.
- Writes of any kind — the api is read-only by construction (no DuckDB file
  ever opened for write, no ingest endpoint).
