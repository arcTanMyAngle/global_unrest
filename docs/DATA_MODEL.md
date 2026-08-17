# Data model

## GeoTemporalEvent (core-types)

The single normalized record every source adapter produces.

| Field | Type | Notes |
|---|---|---|
| `id` | `u64` | Deterministic FNV-1a hash of `(source, source_event_id)` — re-ingesting the same record is idempotent. |
| `source` | `SourceId` | `Fixtures` \| `Gdelt` \| `Acled` \| `Noaa` \| `Ioda` \| `Bluesky` \| `Telegram`. |
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

### Current source identifiers

The current SourceId enum includes Fixtures, Gdelt, Acled, Noaa, Ioda,
Bluesky, and Telegram. Fixtures are test/service-smoke input only; the
desktop removes legacy fixture rows and never loads them at runtime.

### Source attribution (core-types)

`AttributionSubject`/`SourceAttribution`/`attribution_for` in
`crates/core-types/src/attribution.rs` are a static data table, not I/O: one
row per `SourceId` ingest leg plus the non-source third-party legs that are
not a `SignalSource` — the Daily Events Google Gemini call, and the Media
page's on-demand GDELT/Bluesky/Telegram lookups, which run under different
terms and bounds than those providers' ingest use.

| Field | Type | Notes |
|---|---|---|
| `display_name` | `&'static str` | |
| `homepage_url` | `Option<&'static str>` | `None` only for the internal Fixtures entry. |
| `licence_label` | `&'static str` | Short human-readable terms summary. |
| `attribution_text` | `Option<&'static str>` | Verbatim upstream citation string when one is mandated (GDELT, ACLED); `None` otherwise — never a paraphrase. |
| `credentials_required` | `bool` | |
| `env_vars` | `&'static [&'static str]` | Env var **names** the credentialed path reads — never a value (product rule 5). |
| `feature_flag` | `Option<&'static str>` | Desktop Cargo feature gating the live network path (e.g. `acled-live`); `None` when unconditionally compiled. |

`SourceAttribution::is_configured()` reports whether the required env vars
are set and non-empty — the same check every live source's own `from_env`
already performs, exposed as a query. It is distinct from "compiled": that is
`feature_flag`, a build-time fact this type does not itself inspect. A
keyless leg is always "configured".

`attribution_for_source` matches every `SourceId` variant with no wildcard
arm, so adding a variant without a row fails the build; `attribution.rs`'s
tests additionally check the table stays populated, exactly-once per
`SourceId`, and that no `env_vars` entry has a value's shape.

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

## IODA normalization

`source-ioda` polls the keyless `GET /outages/events?entityType=country`
endpoint (Internet Outage Detection and Analysis, Georgia Tech) and produces
one `Disruption` event per outage record. Two honesty rules distinguish this
source from the others:

- **Country-only geocoding, so never a point marker.** IODA identifies a
  location only as `country/<ISO alpha-2>` — no finer geometry exists to
  report. Events therefore normalize at `Country` precision and, per the
  precision rendering contract above, only ever shade a region on the map —
  they are excluded from marker rendering by design, not by bug. The
  centroid used for that shading is a real geometric centroid of the
  country's Natural Earth polygon (`geo::Centroid`, computed once from the
  bundled `ne_110m_admin_0_countries.geojson`, the same asset the basemap
  and click-to-inspect country lookup already use) — never a hand-typed
  coordinate. An ISO alpha-2 code IODA reports that isn't in that dataset
  fails normalization rather than guessing a location.
- **Unbounded severity, log-scaled.** IODA's `score` field is an
  unnormalized anomaly magnitude with no fixed range (observed live from
  ~700 for a brief blip to ~233,000 for a total national blackout).
  `source_ioda::severity_from_score` squashes it onto `[0, 1]` with a log
  scale anchored to two named constants,
  `source_ioda::weights::IODA_SCORE_FLOOR` (100.0) and `IODA_SCORE_CEIL`
  (100,000.0): at/below the floor reads as the severity floor (0.0), at/above
  the ceiling saturates at 1.0. This is the first continuous-range severity
  normalization in the codebase (NOAA's is a 4-bucket categorical match on a
  bounded NWS enum; GDELT Events derives severity from the bounded Goldstein
  scale) — golden-tested at the floor, the ceiling, and a real sampled
  midpoint.

`source_event_id` is a composite key (`{country}-{start}-{datasource}-
{method}`) since IODA's `codf` response format carries no explicit event id.
`themes` carries `["ioda", "internet_outage", <datasource>, <method>]` so
the existing theme filter doubles as provenance filtering (which detection
method/data source flagged the outage) without any new schema.

## Chatter normalization (Bluesky, and any future social stream)

Streaming social sources invert the usual ingest pipeline. Every other source
stores one `GeoTemporalEvent` per upstream record and lets
`storage::score_buckets` aggregate later; chatter sources aggregate
**before** storage and persist only the rollup. That inversion is required
by hard rule 6 in [SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md), not chosen
for performance. The separate, explicitly requested Media page is not part of
this pipeline; see [Transient media research](#transient-media-research).

`crates/chatter` owns the shared machinery used by both `source-bluesky`
and `source-telegram`:

- **`ChatterRollup` is the privacy boundary.** It is a count for a
  `(place, topic, window)` triple — `place_name`, `country_iso`, lat/lon,
  precision, `topic`, `window_start_epoch_s`, `window_secs`, `post_count`.
  There is deliberately no field for post text, author identity, or a URL,
  and `RawRecord::excerpt` formats the chatter line by hand rather than
  deriving `Debug`, so a future field cannot start leaking into
  `ingest_log`.
- **Matching requires a place *and* a topic.** Post text is tokenized into
  lowercase words and scanned with a 1..=N word window against a place
  table (Natural Earth country names + aliases + the 1:110m populated-places
  gazetteer) and a fixed topic-keyword table (`chatter::topic::TOPICS`,
  seeded from the same signal classes the GDELT DOC query tracks and widened
  2026-08-13 to storm / volcano / landslide / drought / displacement /
  explosion / outbreak / crime). Requiring
  both is the main false-positive defence — a place name alone matches
  recipes and given names. One place and one topic per post (leftmost,
  longest match), so a widely-shared multi-country post cannot inflate
  several aggregates at once. A pre-widening 5,918-post live sample matched
  16 posts (0.27%); it is a historical sanity sample, not an expected rate
  after vocabulary changes.
- **Named judgment calls.** Countries beat cities on a token collision
  ("Panama" the country, not Panama City). `AMBIGUOUS_TOKENS` drops
  `male`/`chad`/`jordan`/`georgia` — real collisions with an English word
  and common given names. "us" is deliberately not a United States alias.
  Aliases add spellings only; coordinates always come from bundled geometry.
  Three spelling tables feed the place side: `COUNTRY_ALIASES` (including the
  names Natural Earth abbreviates for a map label — "S. Sudan", "Dem. Rep.
  Congo", "Bosnia and Herz." — which no post ever writes that way),
  `COUNTRY_ADJECTIVES` (demonyms, since chatter says "Sudanese army" more
  often than "the army in Sudan"), and a deliberately tiny `CITY_ALIASES` of
  exonyms. Demonyms that are also everyday English words, language or cuisine
  labels, or ambiguous between two countries are excluded by name and reason.
  Every entry in all three is pinned by a test, because an alias naming an ISO
  the bundled file does not carry inserts nothing at all, silently.
- **Chatter is attention, not an event.** Rollups normalize to
  `EventKind::NewsAttention` with `post_count` in `article_count`, so they
  count in the attention component and never in the unrest component —
  the same class as GDELT article counts. `location_confidence` is 0.5,
  stating in the number the UI already shows that keyword place-matching is
  crude. `themes` is `["chatter", <topic>]`.
- **Only completed windows are published.** `source_event_id` is
  `{place}-{topic}-{window_start}`, so publishing a half-counted window
  would claim that id and the remainder would then be dropped by
  dedup-by-id. `ChatterAccumulator::drain_completed(now)` leaves the
  in-progress window accumulating; every window is published exactly once
  with its full count, however often a drain is called.

### Telegram (`source-telegram`) — the same rollup, a different mechanism

Where Bluesky is a keyless public firehose, Telegram has none: reading a
public channel's history requires a real MTProto session (a phone-number
account — Telegram's Bot API only delivers messages from channels its own
admin added the bot to, which rules out reading a third party's channel).
`crates/source-telegram` uses `grammers-client` (pure Rust, no TDLib/C++
dependency) purely to *read*: it never posts, never joins a channel, and
never touches anything outside `ALLOWED_CHANNELS`.

- **Poll-based, not streaming.** Unlike Bluesky's long-lived socket, each
  poll cycle (`TELEGRAM_POLL_SECS`, 15 minutes) sweeps
  `source_telegram::ALLOWED_CHANNELS` — a small, live-verified, curated
  allowlist, documented with excluded candidates and reasons right next to
  it — resolving each by username and walking new messages via MTProto's
  `iter_messages`.
- **A per-channel high-water mark, not a cursor.** Each channel tracks the
  highest message id already processed (in memory only, not persisted); a
  poll only walks messages newer than that. A restart re-sweeps a bounded
  number of recent messages per channel, but any chatter window that had
  already published re-derives the same `source_event_id` and is discarded
  by storage's dedup-by-id — safe, just occasionally redundant work, never
  double counted (the same corrections-reuse-ids behavior ACLED relies on).
- **Login is a one-time, out-of-band step.** Telegram account login needs a
  phone number and an SMS/app code — not something a long-lived worker or
  GUI app can do for itself. `examples/login_setup.rs` is a small
  interactive tool: run it once, it saves a local JSON session file at
  `LES_TELEGRAM_SESSION_FILE`. `TelegramSource` only ever *opens* that file;
  if it's missing or not yet authorized, `fetch` returns a clear error
  naming the setup command rather than trying to prompt for input from
  inside a GUI app. `TELEGRAM_API_HASH` is read only by that setup tool,
  never by the routine polling path.
- **The session store is this crate's own, not `grammers-session`'s.**
  `grammers-session`'s `SqliteSession` vendors a second static SQLite
  (`libsql-ffi`) alongside the one `rusqlite` already brings in for
  `storage`'s settings DB, and linking both into `global-signal-desktop`
  fails with duplicate `sqlite3_*` symbols. The crate is therefore pulled
  with `default-features = false`, and `source-telegram::file_session`
  implements the `Session` trait over a JSON file instead. The file holds a
  live login — treat it as a credential; it is gitignored.
- **Same ingest privacy boundary as Bluesky.** Message text goes straight into
  `ChatterAccumulator::observe` and is dropped in the same call; the ingest
  path returns no message text, sender identity, or message URL. The crate's
  separate, user-directed media module is the documented exception: it can
  return a public video-post URL, bounded label, and channel attribution to
  the transient Media page, but never a sender identity or a normalized event.
- **The boundary is a type signature, not a convention.** Both legs meet the
  network through the `ChannelReader` seam, and the two legs are shaped
  differently on purpose. The ingest leg's `sweep_history` hands each message
  to a `FnMut(i32, &str, DateTime<Utc>)` callback and returns nothing, so
  message text is borrowed and folded into the accumulator rather than
  collected — a returned `Vec<String>` there would materialize up to
  `PER_CYCLE_LIMIT` = 200 message bodies at once. The media leg's
  `search_videos` may return a `Vec<ChannelVideo>` because materialized
  results are already the documented exception; that struct carries id,
  caption, date, MIME type, file name, and an attachment flag, and
  deliberately no sender. Channel posts can carry a signing author, which is
  a named individual this project has no reason to surface.

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

## Daily Events cache

Migration 0003 adds the local daily_digest cache for the Daily Events page.
It is keyed by a UTC calendar day and records the model, generation time,
separate media-attention and event-data prose, and the two record counts the
prose was generated from. An explicit regeneration replaces that day's row.
There is intentionally no combined-summary column.

The cache is not part of the worker-to-API snapshot contract. It belongs to
the desktop's local analytics store and is never exposed by services/api.

## Transient media research

The Media page uses `media_search::MediaQuery` and `MediaHit`, not
`GeoTemporalEvent` or `ChatterRollup`. It has no DuckDB table, migration,
Parquet export, worker snapshot, or API representation.

- A `MediaQuery` contains a required place, optional topic, start and end
  timestamps, and a 25-result per-provider limit. The UI offers only
  24-hour, 3-day, 7-day, and 30-day windows.
- A `MediaHit` contains a public URL, bounded single-line display
  title/caption, provider (`Gdelt`, `Bluesky`, or `Telegram`), publication
  time, and public origin (outlet domain, Bluesky handle, or Telegram
  channel). It is temporary display data, not source metadata for the map.
- GDELT and Bluesky are queried only after the person presses Search. If a
  Telegram session is configured, `source-telegram` supplies a third,
  read-only allowlist leg. Its read-only session avoids a peer-cache writer
  race with scheduled ingest.
- Hits live only in desktop process memory until the next search replaces
  them or the app exits. They are never written to storage, an ingest log,
  Daily Events facts, a cache, or services/api.

This is the narrow exception described in
[SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md#on-demand-media-lookup). It does
not relax aggregate-before-storage behavior for Bluesky or Telegram ingest.

## Parquet session export (M4 handoff layout)

`StorageHandle::export_parquet` (UI: "export parquet") writes a session as:

```
session-<UTC stamp>/
  events/date=YYYY-MM-DD/*.parquet          (hive-partitioned, UTC dates)
  region_buckets/date=YYYY-MM-DD/*.parquet  (scores included)
  baselines.parquet
```

Re-readable with `read_parquet('…/**/*.parquet', hive_partitioning=1)`
(roundtrip-tested). This is the exact layout the M4 worker publishes —
DuckDB is single-writer per file, so Parquet partitions, never a shared
`.duckdb`, are the process boundary.

**M4 versioned publish** (`StorageHandle::publish_snapshot`, used by
`services/workers`) wraps this same export in an atomically-swapped snapshot:

```
{publish_root}/
  LATEST                  -- text pointer to the current version (atomic rename)
  v<millis>/
    manifest.json         -- {version, published_at_epoch_s, events, buckets, baselines}
    events/ region_buckets/ baselines.parquet   -- the export layout above
```

`services/api` reads only these snapshots (docs/API.md); older versions are
pruned past `keep_last`.

## SQLite (settings.sqlite)

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
