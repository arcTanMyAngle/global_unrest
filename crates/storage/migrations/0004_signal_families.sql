-- v4 (M9): signal families (docs/SIGNAL_MODEL.md).
--
-- `events` gains two NOT NULL classification columns and renames
-- `article_count` to `volume_count`. DuckDB cannot add a NOT NULL column to an
-- existing table, so this is a shadow table: create, backfill with an
-- exhaustive CASE, swap, drop. The whole file runs inside one implicit
-- transaction via execute_batch.
--
-- The backfill classifies per *record*, not per source, because `gdelt` is two
-- adapters: DOC attention rows and Events dump records share a source id.

CREATE TABLE events_v4 (
    id BIGINT PRIMARY KEY,
    source VARCHAR NOT NULL,
    source_event_id VARCHAR NOT NULL,
    -- The observation axis. Scoring asks this, never `kind`.
    family VARCHAR NOT NULL,
    kind VARCHAR NOT NULL,
    -- What this row's lat/lon is a statement about.
    location_role VARCHAR NOT NULL,
    themes VARCHAR NOT NULL,
    ts_epoch_s BIGINT NOT NULL,
    ingested_at_epoch_s BIGINT NOT NULL,
    lat DOUBLE NOT NULL,
    lon DOUBLE NOT NULL,
    location_precision VARCHAR NOT NULL,
    location_confidence REAL NOT NULL,
    country_iso VARCHAR NOT NULL,
    admin1 VARCHAR,
    h3_cell BIGINT NOT NULL,
    -- Volume in the family's own unit (articles/records/alerts/posts/samples).
    -- Never summed across families.
    volume_count INTEGER NOT NULL,
    distinct_source_count INTEGER NOT NULL,
    severity REAL,
    headline VARCHAR,
    outlet_domains VARCHAR NOT NULL,
    urls VARCHAR NOT NULL
);

INSERT INTO events_v4
SELECT
    id,
    source,
    source_event_id,
    CASE
        WHEN source IN ('bluesky', 'telegram') THEN 'chatter'
        WHEN source = 'noaa' THEN 'official_alert'
        WHEN kind = 'news_attention' THEN 'media_attention'
        ELSE 'recorded_event'
    END AS family,
    -- Kind is the within-family subtype, so it moves with the family: chatter
    -- rows were written as `news_attention`, NOAA alerts as `disruption`.
    CASE
        WHEN source IN ('bluesky', 'telegram') THEN 'chatter'
        WHEN source = 'noaa' THEN 'alert'
        ELSE kind
    END AS kind,
    CASE
        WHEN source IN ('bluesky', 'telegram') THEN 'mentioned_place'
        WHEN source = 'noaa' THEN 'reporting_jurisdiction'
        -- GDELT DOC geocodes to `sourcecountry` — the publisher, not the
        -- story. Quarantined from the spatial attention layer until the
        -- GDELT geography work replaces it.
        WHEN source = 'gdelt' AND kind = 'news_attention' THEN 'publisher_origin'
        WHEN kind = 'news_attention' THEN 'mentioned_place'
        ELSE 'event_site'
    END AS location_role,
    themes,
    ts_epoch_s,
    ingested_at_epoch_s,
    lat,
    lon,
    location_precision,
    location_confidence,
    country_iso,
    admin1,
    h3_cell,
    article_count AS volume_count,
    -- A chatter rollup names no outlet, so it has no distinct source count.
    CASE WHEN source IN ('bluesky', 'telegram') THEN 0 ELSE distinct_source_count END,
    severity,
    -- Chatter's stored headline was synthesized by the normalizer ("N posts
    -- mentioned X"), never observed. The UI composes that label at render
    -- time now; the row stops claiming metadata it never had.
    CASE WHEN source IN ('bluesky', 'telegram') THEN NULL ELSE headline END,
    CASE WHEN source IN ('bluesky', 'telegram') THEN '[]' ELSE outlet_domains END,
    urls
FROM events;

DROP TABLE events;
ALTER TABLE events_v4 RENAME TO events;
CREATE INDEX idx_events_ts ON events (ts_epoch_s);
CREATE INDEX idx_events_cell ON events (h3_cell);

-- Derived tables: dropped and recreated rather than altered (see 0002). Their
-- contents are now wrong in meaning as well as shape — chatter used to add to
-- article totals and the spike baseline — so a rebuild is mandatory, not an
-- optimization.
DROP TABLE IF EXISTS region_buckets;
CREATE TABLE region_buckets (
    h3_cell BIGINT NOT NULL,
    bucket_start BIGINT NOT NULL,
    event_count INTEGER NOT NULL,       -- unrest-bearing records only
    attention_count INTEGER NOT NULL,   -- media-attention records only
    article_count BIGINT NOT NULL,      -- attention-only by construction
    source_count BIGINT NOT NULL,       -- attention-only by construction
    distinct_outlets INTEGER NOT NULL,  -- attention-only by construction
    attention_score REAL NOT NULL,
    unrest_score REAL NOT NULL,
    spike_score REAL NOT NULL,
    combined_score REAL NOT NULL,
    baseline REAL NOT NULL,
    spike_cold_start BOOLEAN NOT NULL,
    PRIMARY KEY (h3_cell, bucket_start)
);

DROP TABLE IF EXISTS baselines;
CREATE TABLE baselines (
    h3_cell BIGINT NOT NULL,
    tod_bucket TINYINT NOT NULL,
    baseline DOUBLE NOT NULL,
    sample_days INTEGER NOT NULL,
    computed_at_epoch_s BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, tod_bucket)
);

-- Per-family counts and baselines, long-form: a sixth family must not cost a
-- schema migration, and a per-family deficit against a per-family baseline is
-- exactly the shape silence detection needs.
DROP TABLE IF EXISTS family_buckets;
CREATE TABLE family_buckets (
    h3_cell BIGINT NOT NULL,
    bucket_start BIGINT NOT NULL,
    family VARCHAR NOT NULL,
    record_count INTEGER NOT NULL,
    volume_count BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, bucket_start, family)
);

DROP TABLE IF EXISTS family_baselines;
CREATE TABLE family_baselines (
    h3_cell BIGINT NOT NULL,
    tod_bucket TINYINT NOT NULL,
    family VARCHAR NOT NULL,
    baseline DOUBLE NOT NULL,
    sample_days INTEGER NOT NULL,
    computed_at_epoch_s BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, tod_bucket, family)
);

-- Migration does not itself rebuild the derived tables (that runs on ingest or
-- purge), so it leaves a marker the storage actor honours before serving any
-- query. Without it the app would answer from empty derived tables and call
-- that "no signal".
CREATE TABLE IF NOT EXISTS storage_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO storage_meta (key, value) VALUES ('derived_rebuild_required', '1')
    ON CONFLICT (key) DO UPDATE SET value = '1';

-- Cached digest prose written before v4 describes chatter rollups as media
-- attention. Stamp the facts-schema version cached rows were generated under
-- and drop everything below the current one, so stale prose can never be
-- presented as current.
ALTER TABLE daily_digest ADD COLUMN facts_schema_version INTEGER;
DELETE FROM daily_digest;
