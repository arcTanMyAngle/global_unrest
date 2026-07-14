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

## RegionBucket

Aggregate keyed by `(h3_cell res 3, bucket_start)` with a **6-hour** bucket.
Physical key is H3-only; country rollups are queries/views, never a second
physical table. M1 carries raw counts (`event_count`, `attention_count`,
`article_count`, `distinct_source_count` upper bound); M2 adds the score
components (attention, unrest, spike, combined — stored separately, shown
separately).

## DuckDB schema (analytics store)

- `schema_version(version, applied_at)` — migration ledger.
- `events` — one row per `GeoTemporalEvent`; `themes`/`outlet_domains`/`urls`
  stored as JSON text; timestamps as epoch seconds (`BIGINT`).
- `region_buckets` — recomputed from `events` after ingest (SQL `GROUP BY`).
- `baselines` — M2: per (h3_cell, time-of-day bucket) robust baselines.
- `ingest_log` — one row per failed/refused record: source, reason, raw
  excerpt, timestamp. Normalization failures are never silently dropped.

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
