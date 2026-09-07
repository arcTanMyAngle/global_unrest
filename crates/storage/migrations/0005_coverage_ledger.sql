-- v5 (M9.1 A4): coverage ledger (docs/ROADMAP.md § M9.1).
--
-- One row per coverage *attempt* for a GDELT leg (`doc`, `events`, `gkg`),
-- keyed by what was fetched and the adapter that fetched it. A scalar
-- "backfilled" marker cannot express gaps, failed windows, truncation, or a
-- query change; this can. GKG/Events windows are the immutable 15-minute
-- files — the filename is the window unit and the ETag its content hash
-- (docs/GDELT_GEO_GKG.md); DOC's rows exist because of the measured
-- 429/truncation behaviour that same document records.
--
-- Plain CREATE TABLE like `ingest_log` (0001), not the shadow-table style of
-- 0004: there is no existing table to migrate.
CREATE TABLE IF NOT EXISTS coverage_ledger (
    provider VARCHAR NOT NULL,        -- 'doc' | 'events' | 'gkg'
    config_hash VARCHAR NOT NULL,     -- identity of the query/config fetched
    adapter_version VARCHAR NOT NULL, -- bump when a leg's output shape changes
    window_start BIGINT NOT NULL,     -- epoch seconds, inclusive
    window_end BIGINT NOT NULL,       -- epoch seconds, exclusive
    status VARCHAR NOT NULL,          -- 'ok' | 'failed' | 'truncated'
    detail VARCHAR,                   -- ETag / filename / error excerpt
    recorded_at_epoch_s BIGINT NOT NULL,
    PRIMARY KEY (provider, config_hash, adapter_version, window_start, window_end, status)
);
