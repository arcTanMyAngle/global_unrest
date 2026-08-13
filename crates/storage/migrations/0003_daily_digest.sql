-- Cache for the "Daily Events" page: one generated digest per UTC calendar
-- day. Cached because generating one costs a paid API call — the page reads
-- this table and only calls out when a day has no row.
--
-- `media_attention` and `event_data` are two columns, never one. That is the
-- project's attention/event separation expressed in the schema: there is no
-- combined-summary column for a caller to accidentally write or read.
--
-- The record counts the digest was generated against are stored beside the
-- prose so the page can never display generated text without the numbers it
-- was written from, even for a digest generated days ago.
CREATE TABLE IF NOT EXISTS daily_digest (
    -- 'YYYY-MM-DD', UTC. One row per day; regenerating replaces it.
    day_utc TEXT PRIMARY KEY,
    -- Which model wrote it, so a stale digest is identifiable after a bump.
    model TEXT NOT NULL,
    generated_at_epoch_s BIGINT NOT NULL,
    media_attention TEXT NOT NULL,
    event_data TEXT NOT NULL,
    attention_records BIGINT NOT NULL,
    event_records BIGINT NOT NULL
);
