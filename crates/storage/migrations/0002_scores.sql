-- v2 (M2): score components on region_buckets, sample counting on baselines.
-- Both tables are derived — fully rebuilt from `events` after every ingest —
-- so recreating them in place loses nothing durable. (DuckDB also cannot add
-- NOT NULL columns to an existing table, which rules out ALTER here.)

DROP TABLE IF EXISTS region_buckets;
CREATE TABLE region_buckets (
    h3_cell BIGINT NOT NULL,
    bucket_start BIGINT NOT NULL,
    event_count INTEGER NOT NULL,
    attention_count INTEGER NOT NULL,
    article_count BIGINT NOT NULL,
    source_count BIGINT NOT NULL,
    distinct_outlets INTEGER NOT NULL,
    -- Score components, each in [0, 1]; stored separately, shown separately.
    attention_score REAL NOT NULL,
    unrest_score REAL NOT NULL,
    spike_score REAL NOT NULL,      -- 0.5 = neutral (at baseline)
    combined_score REAL NOT NULL,   -- 0.40*attention + 0.45*unrest + 0.15*spike
    baseline REAL NOT NULL,         -- trailing 28d median for (cell, tod) as of this bucket
    spike_cold_start BOOLEAN NOT NULL,
    PRIMARY KEY (h3_cell, bucket_start)
);

DROP TABLE IF EXISTS baselines;
CREATE TABLE baselines (
    h3_cell BIGINT NOT NULL,
    tod_bucket TINYINT NOT NULL,    -- 0..=3: 00-06, 06-12, 12-18, 18-24 UTC
    baseline DOUBLE NOT NULL,       -- trailing 28d median as of the newest data day
    sample_days INTEGER NOT NULL,   -- days the median saw; < MIN_BASELINE_DAYS = cold start
    computed_at_epoch_s BIGINT NOT NULL,
    PRIMARY KEY (h3_cell, tod_bucket)
);
