# Data model

## GeoTemporalEvent (core-types)

The single normalized record every source adapter produces.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Deterministic FNV-1a hash of `(source, source_event_id)` — re-ingesting the same record is idempotent. |
| `source` | `SourceId` | `Fixtures` \| `Gdelt` \| `Acled`. |
| `source_event_id` | `String` | Source-native identifier. |
| `kind` | `EventKind` | `NewsAttention` \| `Protest` \| `Conflict` \| `Disruption` \| `Other`. |
| `themes` | `Vec<String>` | Coarse topic tags from the source. |
| `ts_utc` | `DateTime<Utc>` | Event/observation time. |
| `ingested_at` | `DateTime<Utc>` | Set at normalization. |
| `lat`, `lon` | `f64` | WGS84. |
| `location_precision` | `LocationPrecision` | `Country` \| `Admin1` \| `City` \| `Exact`. |
| `location_confidence` | `f32` | 0–1. |
| `country_iso` | `String` | ISO 3166-1 alpha-3. |
| `admin1` | `Option<String>` | |
| `h3_cell` | `u64` | H3 cell at **resolution 3** (canonical); parents derived, never stored. |
| `article_count` | `u32` | See counting semantics below. |
| `distinct_source_count` | `u32` | Distinct outlets. |
| `severity` | `Option<f32>` | 0–1 when the source provides one. |
| `headline` | `Option<String>` | Metadata only — **never article bodies**. |
| `outlet_domains` | `Vec<String>` | |
| `urls` | `Vec<String>` | Links back to sources. |

### Counting semantics — attention vs. events

`NewsAttention` records are **attention observations** (how much coverage a
place/topic got in a window), not discrete real-world events. Event-kind
records (`Protest`/`Conflict`/`Disruption`) are discrete occurrences whose
`article_count`/`distinct_source_count` describe coverage *of that event*.
Scoring treats the two classes separately (attention_score vs unrest_score);
mixing them double-counts. The UI keeps "media attention" and "event data"
visually separated for the same reason.

### Precision rendering contract

Sources often geocode to country/admin centroids. Rendering a
`Country`/`Admin1`-precision record as a point paints a fake hotspot in the
middle of a country. The contract, enforced in the renderer: **only `City`
and `Exact` records render as point markers; `Country` and `Admin1` records
contribute to region-level shading only.**

## GDELT normalization (M3)

Two independent `source-gdelt` paths produce `GeoTemporalEvent`s (both keyless,
fallible per record → `ingest_log`):

- **DOC 2.0 `artlist` JSON** → `NewsAttention`. The DOC feed carries no
  per-article coordinates, so each article is geocoded to its **source
  country** at `Country` precision (confidence 0.4). This is honest about the
  feed's granularity and matches the precision rendering contract (country
  attention shades regions, never fake point hotspots). `source_event_id` is
  the article URL (stable dedup key). Themes come from the query (a DOC query
  is usually *for* a theme), lower-cased. Unknown source countries fail per
  record — never guessed. See `source-gdelt::country`.
- **Events 2.0 15-minute CSV-zip dumps** → discrete events. Each 61-column row
  has real `ActionGeo` coordinates; the geo type maps to precision (1=Country,
  2/5=Admin1, 3/4=City). **Only unrest signals are kept**: CAMEO
  `EventRootCode` 14 → `Protest`, 15–16 → `Disruption`, 17–20 → `Conflict`;
  cooperative and low-grade verbal roots are *skipped* (not stored, not
  failed), which keeps the store focused and bounds volume. `severity` is
  derived from the Goldstein scale (hostile half → [0,1]); `source_event_id`
  is `GLOBALEVENTID`; FIPS `ActionGeo_CountryCode` → ISO-A3 via
  `source-gdelt::country`. Events dumps carry no GKG themes, so `themes` is
  empty.

## RegionBucket

Aggregate keyed by `(h3_cell res 3, bucket_start)` with a **6-hour** bucket.
Physical key is H3-only; country rollups are queries/views, never a second
physical table (the heatmap's world-zoom rollup to H3 res 1/2 derives
parents via `geo_utils::cell_parent` at display time). Carries:

- raw counts: `event_count`, `attention_count`, `article_count`,
  `source_count` (summed upper bound) and `distinct_outlets` (exact
  distinct outlet domains);
- M2 score components, each in [0, 1], stored separately and shown
  separately: `attention_score`, `unrest_score`, `spike_score` (0.5 =
  neutral), `combined_score`, plus `baseline` (the spike denominator as of
  this bucket's day) and `spike_cold_start` (see SCORING.md).

## DuckDB schema (analytics store)

- `schema_version(version, applied_at)` — migration ledger.
- `events` — one row per `GeoTemporalEvent`; `themes`/`outlet_domains`/`urls`
  stored as JSON text; timestamps as epoch seconds (`BIGINT`). Under a
  **retention cap** (M3, online volumes ~100k/day) rows older than *N* days
  from the newest event are pruned on each ingest before rescoring; a cap ≥ the
  28-day baseline window keeps recent baselines warm. Default: keep everything.
- `region_buckets` — recomputed from `events` after every ingest by running
  `analytics::score_buckets` (the single reference implementation — there
  is deliberately no SQL twin to keep in sync).
- `baselines` — per (h3_cell, time-of-day bucket): the current trailing
  28-day median and its `sample_days` (< `MIN_BASELINE_DAYS` ⇒ cold start).
- `ingest_log` — one row per failed/refused record: source, reason, raw
  excerpt, timestamp. Normalization failures are never silently dropped.

## Parquet session export (M4 handoff layout)

`StorageHandle::export_parquet` (UI: "export parquet") writes a session as:

```
session-<UTC stamp>/
  events/date=YYYY-MM-DD/*.parquet          (hive-partitioned, UTC dates)
  region_buckets/date=YYYY-MM-DD/*.parquet  (scores included)
  baselines.parquet
```

Re-readable with `read_parquet('…/**/*.parquet', hive_partitioning=1)`
(roundtrip-tested). This is the exact layout the M4 worker will publish —
DuckDB is single-writer per file, so Parquet partitions, never a shared
`.duckdb`, are the process boundary.

## SQLite (settings.db)

App settings only: window geometry, last filters, data paths. Never analytics
data.

## Fixtures

- `fixtures/gdelt_sample.json`, `fixtures/acled_sample.json` — small
  hand-readable samples documenting each shape (attention observations vs
  event records).
- `fixtures/generated/events_35d.json` — ~35 days of synthetic data from
  `cargo run -p source-fixtures --bin generate-fixtures`. 35 days exists so
  M2's 28-day baselines work against fixtures without regeneration. Includes
  deliberate `Country`-precision centroid records to exercise the precision
  rendering contract, and two deliberately malformed records (bad
  coordinates; missing shape) to exercise `ingest_log`.
- `fixtures/regions_sample.geojson` — tiny region polygons for geo tests.
- Synthetic outlets use reserved `.example` domains; nothing imitates a real
  publication.
