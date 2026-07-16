# M4 services API (contract)

`services/api` is a read-only axum HTTP API over the Parquet snapshots
`services/workers` publishes. It never opens a `.duckdb` file — DuckDB is
single-writer-per-file across processes (docs/ARCHITECTURE.md), so the
worker's ingest database is never shared. Each request opens a fresh
in-memory DuckDB connection and runs `read_parquet(...)` against the
snapshot named by a `LATEST` pointer file; there is no persistent
connection or cache to invalidate.

This is intentionally a **narrower** read surface than the desktop's
`StorageHandle` queries: it covers what M4's acceptance criteria need
(`docker compose up` serves the API; the desktop can consume it) without
duplicating storage's per-region headline/theme aggregation
(`RegionDetail`) against a second backend. That fuller inspector detail
stays desktop-only (direct `StorageHandle` access) until a real need to
serve it over HTTP shows up.

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
every ingest cycle (fixture load at startup, then every live GDELT cycle);
`LATEST` is updated via write-temp-then-rename, which is atomic on both
Windows and POSIX, so the api never observes a half-written pointer. Each
version directory is immutable once published — the api can read it
without any lock.

## Error envelope

Non-2xx responses are `{"error": "<message>"}` with a matching HTTP status.
`503` means no snapshot has been published yet (worker hasn't completed
its first ingest cycle) — expected briefly after `docker compose up`.

## Endpoints

### `GET /health`

Readiness probe (used by the Compose healthcheck). Reads `LATEST` +
`manifest.json` only — no Parquet query.

- `200` — `{"status": "ok", "snapshot": {"version": "v...", "published_at_epoch_s": 1752624000, "events": 11043, "buckets": 812, "baselines": 8}}`
- `503` — `{"error": "no snapshot published yet"}`

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

Query params:

| Param | Required | Notes |
|---|---|---|
| `start`, `end` | yes | Epoch seconds, half-open `[start, end)`. |
| `kinds` | no | Comma-separated `EventKind` strings (e.g. `protest,conflict`). |
| `themes` | no | Comma-separated; record matches if any theme is in the list. |
| `min_confidence` | no | `f32`, default `0.0`. |

- `200` — array of `{"id": u64, "lat": f64, "lon": f64, "kind": str, "precision": str, "confidence": f32, "ts_epoch_s": i64, "article_count": u32, "headline": str | null}`
- `400` / `503` as above

## What M4 does not expose (by design)

- Per-region headline/theme/outlet breakdown (`RegionDetail`) — desktop-only.
- `ingest_log` — not part of the Parquet handoff (only `events`,
  `region_buckets`, `baselines` are exported); failed-record debugging stays
  a worker-log/desktop concern.
- Writes of any kind — the api is read-only by construction (no DuckDB file
  ever opened for write, no ingest endpoint).
