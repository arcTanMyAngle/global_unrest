-- v1: core analytics tables.
-- u64 domain values (event id, h3 cell) are stored as BIGINT via bit-cast;
-- the Rust layer round-trips them losslessly.

CREATE TABLE IF NOT EXISTS events (
    id BIGINT PRIMARY KEY,
    source VARCHAR NOT NULL,
    source_event_id VARCHAR NOT NULL,
    kind VARCHAR NOT NULL,
    themes VARCHAR NOT NULL,            -- JSON array text
    ts_epoch_s BIGINT NOT NULL,
    ingested_at_epoch_s BIGINT NOT NULL,
    lat DOUBLE NOT NULL,
    lon DOUBLE NOT NULL,
    location_precision VARCHAR NOT NULL,
    location_confidence REAL NOT NULL,
    country_iso VARCHAR NOT NULL,
    admin1 VARCHAR,
    h3_cell BIGINT NOT NULL,
    article_count INTEGER NOT NULL,
    distinct_source_count INTEGER NOT NULL,
    severity REAL,
    headline VARCHAR,                   -- metadata only, never article bodies
    outlet_domains VARCHAR NOT NULL,    -- JSON array text
    urls VARCHAR NOT NULL               -- JSON array text
);

CREATE INDEX IF NOT EXISTS idx_events_ts ON events (ts_epoch_s);
CREATE INDEX IF NOT EXISTS idx_events_cell ON events (h3_cell);

-- Recomputed from events after ingest (SQL GROUP BY); the analytics crate's
-- aggregate_buckets is the reference implementation this must match.
CREATE TABLE IF NOT EXISTS region_buckets (
    h3_cell BIGINT NOT NULL,
    bucket_start BIGINT NOT NULL,
    event_count INTEGER NOT NULL,
    attention_count INTEGER NOT NULL,
    article_count BIGINT NOT NULL,
    source_count BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, bucket_start)
);

-- M2: robust per-(cell, time-of-day-bucket) baselines land here.
CREATE TABLE IF NOT EXISTS baselines (
    h3_cell BIGINT NOT NULL,
    tod_bucket TINYINT NOT NULL,
    baseline DOUBLE NOT NULL,
    computed_at_epoch_s BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, tod_bucket)
);

-- Failed/refused records: never silently dropped.
CREATE TABLE IF NOT EXISTS ingest_log (
    ts_epoch_s BIGINT NOT NULL,
    source VARCHAR NOT NULL,
    reason VARCHAR NOT NULL,
    raw_excerpt VARCHAR NOT NULL
);
