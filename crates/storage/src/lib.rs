//! DuckDB analytics storage behind a dedicated actor thread, plus a small
//! rusqlite settings store.
//!
//! `duckdb::Connection` is `!Sync`, so a single OS thread owns it and
//! serializes all access; callers talk to it through [`StorageHandle`] and
//! get non-blocking [`Reply`] handles back. The UI polls `Reply::try_take`
//! each frame; tests use `Reply::wait`.
//!
//! DuckDB is **single-writer per file across processes**: the desktop app
//! owns its database exclusively through M3 (docs/ARCHITECTURE.md).

mod settings;

pub use settings::SettingsDb;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

use chrono::Utc;
use core_types::{
    EventKind, GeoTemporalEvent, IngestFailure, LocationPrecision, RegionBucket, bucket_start_epoch,
};
use duckdb::{Connection, params};

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_scores.sql")),
];

/// Cap on rows returned to the UI in one query, as a memory safety valve.
const MAX_POINT_ROWS: usize = 100_000;
/// Rows examined for a region detail; plenty for one cell and one window.
const MAX_DETAIL_ROWS: usize = 5_000;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("duckdb: {0}")]
    Duck(#[from] duckdb::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage actor unavailable: {0}")]
    Actor(String),
    #[error("corrupt row: {0}")]
    Corrupt(String),
}

/// Result of one ingest batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestReport {
    pub inserted: usize,
    /// Events whose id already existed (idempotent re-ingest).
    pub duplicates: usize,
    /// Failed records written to `ingest_log`.
    pub failures: usize,
    /// Events dropped by the retention cap this batch (0 when disabled).
    pub pruned: usize,
}

/// Slim row for the marker layer. Only City/Exact-precision records are
/// returned as points (precision rendering contract).
#[derive(Debug, Clone)]
pub struct EventPoint {
    pub id: u64,
    pub lat: f64,
    pub lon: f64,
    pub kind: EventKind,
    pub precision: LocationPrecision,
    pub confidence: f32,
    pub ts_epoch_s: i64,
    pub article_count: u32,
    pub headline: Option<String>,
}

/// One headline row in the region inspector.
#[derive(Debug, Clone)]
pub struct HeadlineRow {
    pub ts_epoch_s: i64,
    pub kind: EventKind,
    pub headline: String,
    pub outlet_domains: Vec<String>,
    pub confidence: f32,
    pub precision: LocationPrecision,
    pub article_count: u32,
}

/// Aggregated detail for one region (H3 cell) over a window.
#[derive(Debug, Clone, Default)]
pub struct RegionDetail {
    pub h3_cell: u64,
    pub counts_by_kind: Vec<(EventKind, u32)>,
    pub top_themes: Vec<(String, u32)>,
    pub headlines: Vec<HeadlineRow>,
    pub distinct_outlets: u32,
    pub mean_confidence: f32,
    pub total_articles: u64,
    /// Window-composed score components (`analytics::compose_window` over
    /// this cell's stored buckets); `None` when the window holds no buckets.
    pub scores: Option<analytics::WindowScores>,
    /// Share of the cell's records geocoded only to country/admin1 level.
    /// High values earn a low-confidence badge in the UI.
    pub coarse_share: f32,
    /// Trailing 28-day median (records per 6 h) behind the newest bucket in
    /// the window — shown alongside the spike bar for context.
    pub baseline_hint: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct IngestLogRow {
    pub ts_epoch_s: i64,
    pub source: String,
    pub reason: String,
    pub raw_excerpt: String,
}

/// Result of a Parquet session export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub dir: PathBuf,
    pub events: u64,
    pub buckets: u64,
    pub baselines: u64,
}

/// Result of a versioned snapshot publish under a publish root (M4 handoff:
/// `services/workers` calls this; `services/api` reads the `LATEST` pointer
/// it writes). See docs/API.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReport {
    /// Snapshot version directory name, e.g. `v1752624000123`.
    pub version: String,
    /// `{root}/{version}` — the same hive-partitioned layout as
    /// [`ExportReport`]/`export_parquet`.
    pub dir: PathBuf,
    pub events: u64,
    pub buckets: u64,
    pub baselines: u64,
    pub published_at_epoch_s: i64,
}

/// One persisted baseline row (trailing median as of the newest data day).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BaselineDbRow {
    pub h3_cell: u64,
    pub tod_bucket: u8,
    pub baseline: f64,
    pub sample_days: u32,
    pub computed_at_epoch_s: i64,
}

/// Epoch-seconds window `[start, end)` as used by all queries.
pub type EpochWindow = (i64, i64);

enum Cmd {
    Ingest {
        events: Vec<GeoTemporalEvent>,
        failures: Vec<IngestFailure>,
        reply: mpsc::Sender<Result<IngestReport, StorageError>>,
    },
    SetRetention {
        days: Option<u32>,
    },
    TimeExtent {
        reply: mpsc::Sender<Result<Option<EpochWindow>, StorageError>>,
    },
    QueryBuckets {
        window: EpochWindow,
        themes: Option<Vec<String>>,
        reply: mpsc::Sender<Result<Vec<RegionBucket>, StorageError>>,
    },
    QueryPoints {
        window: EpochWindow,
        kinds: Option<Vec<EventKind>>,
        themes: Option<Vec<String>>,
        min_confidence: f32,
        reply: mpsc::Sender<Result<Vec<EventPoint>, StorageError>>,
    },
    ThemeVocab {
        reply: mpsc::Sender<Result<Vec<(String, u32)>, StorageError>>,
    },
    RegionDetail {
        h3_cell: u64,
        window: EpochWindow,
        reply: mpsc::Sender<Result<RegionDetail, StorageError>>,
    },
    IngestLog {
        limit: usize,
        reply: mpsc::Sender<Result<(u64, Vec<IngestLogRow>), StorageError>>,
    },
    Baselines {
        h3_cell: u64,
        reply: mpsc::Sender<Result<Vec<BaselineDbRow>, StorageError>>,
    },
    ExportParquet {
        dir: PathBuf,
        reply: mpsc::Sender<Result<ExportReport, StorageError>>,
    },
    PublishSnapshot {
        root: PathBuf,
        keep_last: Option<usize>,
        reply: mpsc::Sender<Result<PublishReport, StorageError>>,
    },
    Shutdown,
}

/// Non-blocking reply handle. Poll `try_take` from the UI; `wait` in tests.
pub struct Reply<T>(mpsc::Receiver<Result<T, StorageError>>);

impl<T> Reply<T> {
    pub fn try_take(&self) -> Option<Result<T, StorageError>> {
        match self.0.try_recv() {
            Ok(v) => Some(v),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(StorageError::Actor("reply channel dropped".into())))
            }
        }
    }

    pub fn wait(self) -> Result<T, StorageError> {
        self.0
            .recv()
            .unwrap_or_else(|e| Err(StorageError::Actor(format!("reply channel dropped: {e}"))))
    }
}

/// Handle to the storage actor thread. Cloneable; dropping the last clone
/// shuts the actor down.
pub struct StorageHandle {
    tx: mpsc::Sender<Cmd>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl StorageHandle {
    /// Open (or create) the DuckDB database, run pending migrations, and
    /// start the actor thread. `notifier` fires after every reply is sent —
    /// the desktop passes `ctx.request_repaint()` so results are painted
    /// promptly; tests pass a no-op.
    pub fn open(
        db_path: Option<PathBuf>,
        notifier: Box<dyn Fn() + Send>,
    ) -> Result<Self, StorageError> {
        let conn = match &db_path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        migrate(&conn)?;

        let (tx, rx) = mpsc::channel::<Cmd>();
        let join = std::thread::Builder::new()
            .name("storage-actor".into())
            .spawn(move || actor_loop(conn, rx, notifier))
            .map_err(StorageError::Io)?;
        Ok(Self {
            tx,
            join: Some(join),
        })
    }

    fn send(&self, cmd: Cmd) {
        // If the actor died the reply channel drops and callers see
        // StorageError::Actor on take/wait.
        let _ = self.tx.send(cmd);
    }

    pub fn ingest(
        &self,
        events: Vec<GeoTemporalEvent>,
        failures: Vec<IngestFailure>,
    ) -> Reply<IngestReport> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::Ingest {
            events,
            failures,
            reply,
        });
        Reply(rx)
    }

    /// Set the retention cap in days (applied on every subsequent ingest);
    /// `None` or 0 disables pruning (keep everything). Fire-and-forget.
    pub fn set_retention(&self, days: Option<u32>) {
        self.send(Cmd::SetRetention { days });
    }

    /// (min, max+1) event timestamp — i.e. a half-open window covering all
    /// data — or None when the store is empty.
    pub fn time_extent(&self) -> Reply<Option<EpochWindow>> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::TimeExtent { reply });
        Reply(rx)
    }

    /// Bucket rows in a window. With `themes`, buckets are recomputed over
    /// only the events carrying one of those themes — including baselines
    /// and spike, so a theme's spike reads "vs. that theme's own baseline".
    pub fn query_buckets(
        &self,
        window: EpochWindow,
        themes: Option<Vec<String>>,
    ) -> Reply<Vec<RegionBucket>> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::QueryBuckets {
            window,
            themes,
            reply,
        });
        Reply(rx)
    }

    pub fn query_points(
        &self,
        window: EpochWindow,
        kinds: Option<Vec<EventKind>>,
        themes: Option<Vec<String>>,
        min_confidence: f32,
    ) -> Reply<Vec<EventPoint>> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::QueryPoints {
            window,
            kinds,
            themes,
            min_confidence,
            reply,
        });
        Reply(rx)
    }

    /// Distinct themes across all events with usage counts, most-used first.
    pub fn theme_vocab(&self) -> Reply<Vec<(String, u32)>> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::ThemeVocab { reply });
        Reply(rx)
    }

    pub fn region_detail(&self, h3_cell: u64, window: EpochWindow) -> Reply<RegionDetail> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::RegionDetail {
            h3_cell,
            window,
            reply,
        });
        Reply(rx)
    }

    /// Total ingest-log row count plus the most recent `limit` rows.
    pub fn ingest_log(&self, limit: usize) -> Reply<(u64, Vec<IngestLogRow>)> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::IngestLog { limit, reply });
        Reply(rx)
    }

    /// The four persisted time-of-day baselines for one cell.
    pub fn baselines(&self, h3_cell: u64) -> Reply<Vec<BaselineDbRow>> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::Baselines { h3_cell, reply });
        Reply(rx)
    }

    /// Export the session to Parquet under `dir` (must not already contain
    /// data): `events/` and `region_buckets/` as hive `date=YYYY-MM-DD`
    /// partitions plus `baselines.parquet`. This layout is the M4 handoff
    /// surface — the worker will publish the same shape (docs/PLAN.md §7).
    pub fn export_parquet(&self, dir: PathBuf) -> Reply<ExportReport> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::ExportParquet { dir, reply });
        Reply(rx)
    }

    /// Publish the current session as a new versioned snapshot under `root`
    /// (`{root}/v<millis>/...`), then atomically repoint `{root}/LATEST` at
    /// it. `keep_last` (`None` = keep all) prunes older version directories
    /// after a successful publish. This is the M4 cross-process handoff: the
    /// worker calls this after every ingest cycle, and `services/api` only
    /// ever reads immutable snapshots this produced — never a `.duckdb` file
    /// (docs/ARCHITECTURE.md's single-writer rule).
    pub fn publish_snapshot(
        &self,
        root: PathBuf,
        keep_last: Option<usize>,
    ) -> Reply<PublishReport> {
        let (reply, rx) = mpsc::channel();
        self.send(Cmd::PublishSnapshot {
            root,
            keep_last,
            reply,
        });
        Reply(rx)
    }
}

impl Drop for StorageHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn actor_loop(conn: Connection, rx: mpsc::Receiver<Cmd>, notifier: Box<dyn Fn() + Send>) {
    // Retention cap in days, held by the actor (the connection's owner). `None`
    // keeps everything (fixture default); online mode sets a finite window.
    let mut retention_days: Option<u32> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Ingest {
                events,
                failures,
                reply,
            } => {
                let _ = reply.send(do_ingest(&conn, &events, &failures, retention_days));
            }
            Cmd::SetRetention { days } => {
                retention_days = days.filter(|d| *d > 0);
                continue; // no reply, no repaint needed
            }
            Cmd::TimeExtent { reply } => {
                let _ = reply.send(do_time_extent(&conn));
            }
            Cmd::QueryBuckets {
                window,
                themes,
                reply,
            } => {
                let _ = reply.send(do_query_buckets(&conn, window, themes.as_deref()));
            }
            Cmd::QueryPoints {
                window,
                kinds,
                themes,
                min_confidence,
                reply,
            } => {
                let _ = reply.send(do_query_points(
                    &conn,
                    window,
                    kinds.as_deref(),
                    themes.as_deref(),
                    min_confidence,
                ));
            }
            Cmd::ThemeVocab { reply } => {
                let _ = reply.send(do_theme_vocab(&conn));
            }
            Cmd::RegionDetail {
                h3_cell,
                window,
                reply,
            } => {
                let _ = reply.send(do_region_detail(&conn, h3_cell, window));
            }
            Cmd::IngestLog { limit, reply } => {
                let _ = reply.send(do_ingest_log(&conn, limit));
            }
            Cmd::Baselines { h3_cell, reply } => {
                let _ = reply.send(do_baselines(&conn, h3_cell));
            }
            Cmd::ExportParquet { dir, reply } => {
                let _ = reply.send(do_export_parquet(&conn, dir));
            }
            Cmd::PublishSnapshot {
                root,
                keep_last,
                reply,
            } => {
                let _ = reply.send(do_publish_snapshot(&conn, root, keep_last));
            }
            Cmd::Shutdown => break,
        }
        notifier();
    }
}

fn migrate(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version BIGINT PRIMARY KEY,
            applied_at_epoch_s BIGINT NOT NULL
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT coalesce(max(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;
    for (version, sql) in MIGRATIONS {
        if *version > current {
            tracing::info!(version, "applying storage migration");
            conn.execute_batch(sql)?;
            conn.execute(
                "INSERT INTO schema_version (version, applied_at_epoch_s) VALUES (?, ?)",
                params![version, Utc::now().timestamp()],
            )?;
        }
    }
    Ok(())
}

/// u64 ↔ BIGINT bit-cast helpers (lossless round-trip).
fn u64_to_db(v: u64) -> i64 {
    v as i64
}

fn u64_from_db(v: i64) -> u64 {
    v as u64
}

fn do_ingest(
    conn: &Connection,
    events: &[GeoTemporalEvent],
    failures: &[IngestFailure],
    retention_days: Option<u32>,
) -> Result<IngestReport, StorageError> {
    // Idempotent re-ingest: drop events whose id is already present.
    // (The appender has no ON CONFLICT path, so dedup up front.)
    let mut existing: HashSet<u64> = HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT id FROM events")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        for row in rows {
            existing.insert(u64_from_db(row?));
        }
    }

    let mut inserted = 0usize;
    let mut duplicates = 0usize;
    {
        let mut appender = conn.appender("events")?;
        let mut batch_seen: HashSet<u64> = HashSet::new();
        for ev in events {
            if existing.contains(&ev.id) || !batch_seen.insert(ev.id) {
                duplicates += 1;
                continue;
            }
            appender.append_row(params![
                u64_to_db(ev.id),
                ev.source.as_str(),
                ev.source_event_id,
                ev.kind.as_str(),
                serde_json::to_string(&ev.themes).unwrap_or_else(|_| "[]".into()),
                ev.ts_utc.timestamp(),
                ev.ingested_at.timestamp(),
                ev.lat,
                ev.lon,
                ev.location_precision.as_str(),
                ev.location_confidence,
                ev.country_iso,
                ev.admin1,
                u64_to_db(ev.h3_cell),
                ev.article_count,
                ev.distinct_source_count,
                ev.severity,
                ev.headline,
                serde_json::to_string(&ev.outlet_domains).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&ev.urls).unwrap_or_else(|_| "[]".into()),
            ])?;
            inserted += 1;
        }
        appender.flush()?;
    }

    for failure in failures {
        conn.execute(
            "INSERT INTO ingest_log (ts_epoch_s, source, reason, raw_excerpt) VALUES (?, ?, ?, ?)",
            params![
                failure.occurred_at.timestamp(),
                failure.source.as_str(),
                failure.reason,
                failure.raw_excerpt,
            ],
        )?;
    }

    // Apply retention before rescoring so buckets/baselines are computed over
    // exactly the retained events (no dangling buckets for pruned days).
    let pruned = match retention_days {
        Some(days) => prune_events(conn, i64::from(days))?,
        None => 0,
    };

    rebuild_buckets(conn)?;

    tracing::info!(
        inserted,
        duplicates,
        failures = failures.len(),
        pruned,
        "ingest complete"
    );
    Ok(IngestReport {
        inserted,
        duplicates,
        failures: failures.len(),
        pruned,
    })
}

/// Drop events older than `retention_days` relative to the newest event, so the
/// table stays bounded at online volumes (~100k/day). Retention ≥ the 28-day
/// baseline window (docs/SCORING.md) keeps recent baselines fully warm; shorter
/// windows are allowed but degrade the oldest retained buckets to cold start.
/// Returns the number of rows pruned.
fn prune_events(conn: &Connection, retention_days: i64) -> Result<usize, StorageError> {
    let max_ts: Option<i64> =
        conn.query_row("SELECT max(ts_epoch_s) FROM events", [], |r| r.get(0))?;
    let Some(max_ts) = max_ts else {
        return Ok(0);
    };
    let cutoff = max_ts - retention_days.saturating_mul(86_400);
    let pruned = conn.execute("DELETE FROM events WHERE ts_epoch_s < ?", params![cutoff])?;
    if pruned > 0 {
        tracing::info!(pruned, retention_days, "pruned events past retention");
    }
    Ok(pruned)
}

/// Recompute region_buckets and baselines from events by running the
/// analytics reference pipeline (`analytics::score_buckets`) over the whole
/// events table and persisting the result. One implementation, no SQL twin
/// to keep in sync. Reading everything back is fine at fixture/M3 scale
/// (~1e5–1e6 rows); make this incremental if ingest ever gets hot.
fn rebuild_buckets(conn: &Connection) -> Result<(), StorageError> {
    let events = read_score_events(conn)?;
    let scored = analytics::score_buckets(&events);

    conn.execute("DELETE FROM region_buckets", [])?;
    {
        let mut app = conn.appender("region_buckets")?;
        for b in &scored.buckets {
            app.append_row(params![
                u64_to_db(b.h3_cell),
                b.bucket_start,
                b.event_count as i32,
                b.attention_count as i32,
                b.article_count as i64,
                b.source_count as i64,
                b.distinct_outlets as i32,
                b.attention_score,
                b.unrest_score,
                b.spike_score,
                b.combined_score,
                b.baseline,
                b.spike_cold_start,
            ])?;
        }
        app.flush()?;
    }

    conn.execute("DELETE FROM baselines", [])?;
    {
        let computed_at = Utc::now().timestamp();
        let mut app = conn.appender("baselines")?;
        for r in &scored.baselines {
            app.append_row(params![
                u64_to_db(r.h3_cell),
                i32::from(r.tod_bucket),
                r.baseline,
                r.sample_days as i32,
                computed_at,
            ])?;
        }
        app.flush()?;
    }
    Ok(())
}

/// Read back the event columns that scoring consumes.
fn read_score_events(conn: &Connection) -> Result<Vec<analytics::ScoreEvent>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT h3_cell, ts_epoch_s, kind, article_count, distinct_source_count,
                location_confidence, severity, location_precision, themes, outlet_domains
         FROM events",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, f32>(5)?,
            r.get::<_, Option<f32>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (cell, ts, kind, articles, sources, conf, severity, precision, themes, outlets) = row?;
        out.push(analytics::ScoreEvent {
            h3_cell: u64_from_db(cell),
            ts_epoch_s: ts,
            kind: parse_kind(&kind)?,
            article_count: articles.max(0) as u32,
            distinct_source_count: sources.max(0) as u32,
            location_confidence: conf,
            severity,
            renders_as_point: parse_precision(&precision)?.renders_as_point(),
            themes: serde_json::from_str(&themes).unwrap_or_default(),
            outlet_domains: serde_json::from_str(&outlets).unwrap_or_default(),
        });
    }
    Ok(out)
}

fn do_time_extent(conn: &Connection) -> Result<Option<EpochWindow>, StorageError> {
    let (min, max): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT min(ts_epoch_s), max(ts_epoch_s) FROM events",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(match (min, max) {
        (Some(a), Some(b)) => Some((a, b + 1)),
        _ => None,
    })
}

fn do_query_buckets(
    conn: &Connection,
    window: EpochWindow,
    themes: Option<&[String]>,
) -> Result<Vec<RegionBucket>, StorageError> {
    let Some(themes) = themes else {
        return select_buckets(conn, window, None);
    };
    // Theme-filtered view: re-run the scoring pipeline over only the events
    // carrying a selected theme (full history, so the theme's baselines and
    // spike stay meaningful), then trim to the window.
    let mut events = read_score_events(conn)?;
    events.retain(|ev| ev.themes.iter().any(|t| themes.contains(t)));
    let from = bucket_start_epoch(window.0);
    let mut buckets = analytics::score_buckets(&events).buckets;
    buckets.retain(|b| b.bucket_start >= from && b.bucket_start < window.1);
    Ok(buckets)
}

fn do_theme_vocab(conn: &Connection) -> Result<Vec<(String, u32)>, StorageError> {
    let mut stmt = conn.prepare("SELECT themes FROM events")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for row in rows {
        let themes: Vec<String> = serde_json::from_str(&row?).unwrap_or_default();
        for theme in themes {
            *counts.entry(theme).or_insert(0) += 1;
        }
    }
    let mut vocab: Vec<(String, u32)> = counts.into_iter().collect();
    vocab.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(vocab)
}

/// Bucket rows in a window, optionally restricted to one cell.
fn select_buckets(
    conn: &Connection,
    window: EpochWindow,
    h3_cell: Option<u64>,
) -> Result<Vec<RegionBucket>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT h3_cell, bucket_start, event_count, attention_count, article_count, source_count,
                distinct_outlets, attention_score, unrest_score, spike_score, combined_score,
                baseline, spike_cold_start
         FROM region_buckets
         WHERE bucket_start >= ? AND bucket_start < ?
           AND h3_cell = coalesce(?, h3_cell)
         ORDER BY h3_cell, bucket_start",
    )?;
    // Include the bucket the window start falls into.
    let from = bucket_start_epoch(window.0);
    let rows = stmt.query_map(params![from, window.1, h3_cell.map(u64_to_db)], |r| {
        Ok(RegionBucket {
            h3_cell: u64_from_db(r.get(0)?),
            bucket_start: r.get(1)?,
            event_count: r.get::<_, i64>(2)? as u32,
            attention_count: r.get::<_, i64>(3)? as u32,
            article_count: r.get::<_, i64>(4)? as u64,
            source_count: r.get::<_, i64>(5)? as u64,
            distinct_outlets: r.get::<_, i64>(6)? as u32,
            attention_score: r.get(7)?,
            unrest_score: r.get(8)?,
            spike_score: r.get(9)?,
            combined_score: r.get(10)?,
            baseline: r.get(11)?,
            spike_cold_start: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn parse_kind(s: &str) -> Result<EventKind, StorageError> {
    EventKind::parse(s).ok_or_else(|| StorageError::Corrupt(format!("unknown kind `{s}`")))
}

fn parse_precision(s: &str) -> Result<LocationPrecision, StorageError> {
    LocationPrecision::parse(s)
        .ok_or_else(|| StorageError::Corrupt(format!("unknown precision `{s}`")))
}

fn do_query_points(
    conn: &Connection,
    window: EpochWindow,
    kinds: Option<&[EventKind]>,
    themes: Option<&[String]>,
    min_confidence: f32,
) -> Result<Vec<EventPoint>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, lat, lon, kind, location_precision, location_confidence,
                ts_epoch_s, article_count, headline, themes
         FROM events
         WHERE ts_epoch_s >= ? AND ts_epoch_s < ?
           AND location_precision IN ('city', 'exact')
           AND location_confidence >= ?
         ORDER BY ts_epoch_s
         LIMIT ?",
    )?;
    let rows = stmt.query_map(
        params![window.0, window.1, min_confidence, MAX_POINT_ROWS],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, f32>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, String>(9)?,
            ))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (id, lat, lon, kind, precision, confidence, ts, articles, headline, themes_s) = row?;
        let kind = parse_kind(&kind)?;
        if let Some(filter) = kinds
            && !filter.contains(&kind)
        {
            continue;
        }
        if let Some(filter) = themes {
            let event_themes: Vec<String> = serde_json::from_str(&themes_s).unwrap_or_default();
            if !event_themes.iter().any(|t| filter.contains(t)) {
                continue;
            }
        }
        out.push(EventPoint {
            id: u64_from_db(id),
            lat,
            lon,
            kind,
            precision: parse_precision(&precision)?,
            confidence,
            ts_epoch_s: ts,
            article_count: articles as u32,
            headline,
        });
    }
    Ok(out)
}

fn do_region_detail(
    conn: &Connection,
    h3_cell: u64,
    window: EpochWindow,
) -> Result<RegionDetail, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT kind, themes, headline, outlet_domains, location_confidence,
                location_precision, article_count, ts_epoch_s
         FROM events
         WHERE h3_cell = ? AND ts_epoch_s >= ? AND ts_epoch_s < ?
         ORDER BY article_count DESC, ts_epoch_s DESC
         LIMIT ?",
    )?;
    let rows = stmt.query_map(
        params![u64_to_db(h3_cell), window.0, window.1, MAX_DETAIL_ROWS],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f32>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
            ))
        },
    )?;

    let mut detail = RegionDetail {
        h3_cell,
        ..Default::default()
    };
    let mut kind_counts: std::collections::BTreeMap<&'static str, (EventKind, u32)> =
        std::collections::BTreeMap::new();
    let mut theme_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut outlets: HashSet<String> = HashSet::new();
    let mut conf_sum = 0.0f64;
    let mut n_rows = 0u32;
    let mut n_coarse = 0u32;

    for row in rows {
        let (kind_s, themes_s, headline, domains_s, confidence, precision_s, articles, ts) = row?;
        let kind = parse_kind(&kind_s)?;
        let precision = parse_precision(&precision_s)?;
        let themes: Vec<String> = serde_json::from_str(&themes_s).unwrap_or_default();
        let domains: Vec<String> = serde_json::from_str(&domains_s).unwrap_or_default();

        n_coarse += u32::from(!precision.renders_as_point());
        kind_counts.entry(kind.as_str()).or_insert((kind, 0)).1 += 1;
        for theme in themes {
            *theme_counts.entry(theme).or_insert(0) += 1;
        }
        for domain in &domains {
            outlets.insert(domain.clone());
        }
        conf_sum += f64::from(confidence);
        n_rows += 1;
        detail.total_articles += articles.max(0) as u64;

        if let Some(headline) = headline
            && detail.headlines.len() < 30
        {
            detail.headlines.push(HeadlineRow {
                ts_epoch_s: ts,
                kind,
                headline,
                outlet_domains: domains,
                confidence,
                precision,
                article_count: articles as u32,
            });
        }
    }

    detail.counts_by_kind = kind_counts.into_values().collect();
    let mut themes: Vec<(String, u32)> = theme_counts.into_iter().collect();
    themes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    themes.truncate(12);
    detail.top_themes = themes;
    detail.distinct_outlets = outlets.len() as u32;
    detail.mean_confidence = if n_rows > 0 {
        (conf_sum / f64::from(n_rows)) as f32
    } else {
        0.0
    };
    detail.coarse_share = if n_rows > 0 {
        n_coarse as f32 / n_rows as f32
    } else {
        0.0
    };

    // Window-composed score components from this cell's stored buckets.
    let buckets = select_buckets(conn, window, Some(h3_cell))?;
    detail.scores = analytics::compose_window(&buckets, window);
    detail.baseline_hint = buckets.last().map(|b| b.baseline);
    Ok(detail)
}

fn do_ingest_log(
    conn: &Connection,
    limit: usize,
) -> Result<(u64, Vec<IngestLogRow>), StorageError> {
    let total: i64 = conn.query_row("SELECT count(*) FROM ingest_log", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT ts_epoch_s, source, reason, raw_excerpt
         FROM ingest_log ORDER BY ts_epoch_s DESC LIMIT ?",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(IngestLogRow {
            ts_epoch_s: r.get(0)?,
            source: r.get(1)?,
            reason: r.get(2)?,
            raw_excerpt: r.get(3)?,
        })
    })?;
    Ok((total.max(0) as u64, rows.collect::<Result<Vec<_>, _>>()?))
}

fn do_baselines(conn: &Connection, h3_cell: u64) -> Result<Vec<BaselineDbRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT tod_bucket, baseline, sample_days, computed_at_epoch_s
         FROM baselines WHERE h3_cell = ? ORDER BY tod_bucket",
    )?;
    let rows = stmt.query_map(params![u64_to_db(h3_cell)], |r| {
        Ok(BaselineDbRow {
            h3_cell,
            tod_bucket: r.get::<_, i32>(0)? as u8,
            baseline: r.get(1)?,
            sample_days: r.get::<_, i32>(2)?.max(0) as u32,
            computed_at_epoch_s: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// A filesystem path as a single-quoted DuckDB SQL string literal.
/// DuckDB accepts forward slashes on Windows; single quotes are doubled.
fn sql_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace('\'', "''")
}

fn do_export_parquet(conn: &Connection, dir: PathBuf) -> Result<ExportReport, StorageError> {
    std::fs::create_dir_all(&dir)?;
    let count = |table: &str| -> Result<u64, StorageError> {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    };
    let report = ExportReport {
        events: count("events")?,
        buckets: count("region_buckets")?,
        baselines: count("baselines")?,
        dir: dir.clone(),
    };

    // Hive `date=YYYY-MM-DD` partitions; the derived date is UTC.
    // make_timestamp(µs) keeps this timezone-setting-independent.
    let sql = format!(
        "COPY (SELECT *, strftime(make_timestamp(ts_epoch_s * 1000000), '%Y-%m-%d') AS date
               FROM events)
         TO '{d}/events' (FORMAT PARQUET, PARTITION_BY (date));
         COPY (SELECT *, strftime(make_timestamp(bucket_start * 1000000), '%Y-%m-%d') AS date
               FROM region_buckets)
         TO '{d}/region_buckets' (FORMAT PARQUET, PARTITION_BY (date));
         COPY baselines TO '{d}/baselines.parquet' (FORMAT PARQUET);",
        d = sql_path(&dir)
    );
    conn.execute_batch(&sql)?;
    tracing::info!(dir = %dir.display(), events = report.events, "parquet export complete");
    Ok(report)
}

/// A small JSON sidecar in each snapshot directory — lets `services/api`
/// answer `/health` from disk alone, no DuckDB read needed.
#[derive(serde::Serialize)]
struct SnapshotManifest {
    version: String,
    published_at_epoch_s: i64,
    events: u64,
    buckets: u64,
    baselines: u64,
}

fn do_publish_snapshot(
    conn: &Connection,
    root: PathBuf,
    keep_last: Option<usize>,
) -> Result<PublishReport, StorageError> {
    std::fs::create_dir_all(&root)?;
    let published_at_epoch_s = Utc::now().timestamp();
    // Millis (not seconds) so two publishes in the same second still land in
    // distinct, lexicographically-sortable version directories; nudge past
    // the rare exact-millis collision (e.g. back-to-back test publishes).
    let mut millis = Utc::now().timestamp_millis();
    let mut version = format!("v{millis}");
    while root.join(&version).exists() {
        millis += 1;
        version = format!("v{millis}");
    }
    let version_dir = root.join(&version);

    let export = do_export_parquet(conn, version_dir.clone())?;
    let manifest = SnapshotManifest {
        version: version.clone(),
        published_at_epoch_s,
        events: export.events,
        buckets: export.buckets,
        baselines: export.baselines,
    };
    std::fs::write(
        version_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )?;

    // Atomic pointer flip: write-temp-then-rename replaces `LATEST` in one
    // filesystem op on both Windows and POSIX, so `services/api` never
    // observes a half-written pointer.
    let tmp = root.join(".LATEST.tmp");
    std::fs::write(&tmp, &version)?;
    std::fs::rename(&tmp, root.join("LATEST"))?;

    if let Some(keep) = keep_last {
        prune_old_snapshots(&root, &version, keep)?;
    }

    tracing::info!(version = %version, events = export.events, "snapshot published");
    Ok(PublishReport {
        version,
        dir: version_dir,
        events: export.events,
        buckets: export.buckets,
        baselines: export.baselines,
        published_at_epoch_s,
    })
}

/// Remove version directories under `root` beyond the newest `keep` (the
/// just-published `current` version always survives). Best-effort: a failed
/// removal is logged, not fatal — a stray old snapshot just costs disk.
fn prune_old_snapshots(
    root: &std::path::Path,
    current: &str,
    keep: usize,
) -> Result<(), StorageError> {
    let mut versions: Vec<String> = std::fs::read_dir(root)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with('v'))
        .collect();
    versions.sort_unstable();
    versions.reverse(); // newest first
    debug_assert_eq!(versions.first().map(String::as_str), Some(current));
    for stale in versions.into_iter().skip(keep.max(1)) {
        let path = root.join(&stale);
        if let Err(e) = std::fs::remove_dir_all(&path) {
            tracing::warn!(version = %stale, error = %e, "failed to prune old snapshot");
        }
    }
    Ok(())
}

/// Convenience used by the ingest pipeline: normalize a batch of raw records
/// with a source, partitioning successes and failures.
pub fn partition_normalized<S: core_types::SignalSource>(
    source: &S,
    raws: &[core_types::RawRecord],
) -> (Vec<GeoTemporalEvent>, Vec<IngestFailure>) {
    let mut events = Vec::with_capacity(raws.len());
    let mut failures = Vec::new();
    for raw in raws {
        match source.normalize(raw) {
            Ok(mut evs) => events.append(&mut evs),
            Err(err) => failures.push(IngestFailure {
                source: source.id(),
                reason: err.to_string(),
                raw_excerpt: raw.excerpt(300),
                occurred_at: Utc::now(),
            }),
        }
    }
    (events, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use core_types::{BUCKET_SECS, SourceId, event_id};

    fn sample_event(seq: u32, kind: EventKind, hour: u32, cell: u64) -> GeoTemporalEvent {
        let ts = Utc.with_ymd_and_hms(2026, 6, 1, hour, 30, 0).unwrap();
        GeoTemporalEvent {
            id: event_id(SourceId::Fixtures, &format!("evt-{seq}")),
            source: SourceId::Fixtures,
            source_event_id: format!("evt-{seq}"),
            kind,
            themes: vec!["protest".into(), "labor".into()],
            ts_utc: ts,
            ingested_at: ts,
            lat: 48.85,
            lon: 2.35,
            location_precision: LocationPrecision::City,
            location_confidence: 0.85,
            country_iso: "FRA".into(),
            admin1: Some("Île-de-France".into()),
            h3_cell: cell,
            article_count: 10,
            distinct_source_count: 4,
            severity: None,
            headline: Some(format!("[synthetic] headline {seq}")),
            outlet_domains: vec!["globalwire.example".into(), "worldpost.example".into()],
            urls: vec![],
        }
    }

    fn failure() -> IngestFailure {
        IngestFailure {
            source: SourceId::Fixtures,
            reason: "coordinates out of range: lat=999, lon=0".into(),
            raw_excerpt: "{...}".into(),
            occurred_at: Utc::now(),
        }
    }

    fn open_mem() -> StorageHandle {
        StorageHandle::open(None, Box::new(|| {})).unwrap()
    }

    #[test]
    fn ingest_query_roundtrip_and_idempotency() {
        let store = open_mem();
        let events = vec![
            sample_event(1, EventKind::NewsAttention, 1, 100),
            sample_event(2, EventKind::Protest, 2, 100),
            sample_event(3, EventKind::Conflict, 8, 200),
        ];
        let report = store
            .ingest(events.clone(), vec![failure()])
            .wait()
            .unwrap();
        assert_eq!(
            report,
            IngestReport {
                inserted: 3,
                duplicates: 0,
                failures: 1,
                pruned: 0,
            }
        );

        // Re-ingest: everything deduplicates, nothing double-counts.
        let report2 = store.ingest(events, vec![]).wait().unwrap();
        assert_eq!(report2.inserted, 0);
        assert_eq!(report2.duplicates, 3);

        let extent = store.time_extent().wait().unwrap().unwrap();
        let day = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(extent.0, day + 3600 + 1800);

        // Buckets match the hand-computed aggregation: cell 100 bucket 0
        // holds 1 attention + 1 event; cell 200 bucket 1 holds 1 event.
        let buckets = store
            .query_buckets((day, day + 86_400), None)
            .wait()
            .unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].h3_cell, 100);
        assert_eq!(buckets[0].attention_count, 1);
        assert_eq!(buckets[0].event_count, 1);
        assert_eq!(buckets[0].article_count, 20);
        assert_eq!(buckets[1].h3_cell, 200);
        assert_eq!(buckets[1].bucket_start, day + BUCKET_SECS);

        // Scores were computed and persisted: mixed bucket has both
        // components; a single day of data is always spike-cold-start.
        assert!(buckets[0].attention_score > 0.0);
        assert!(buckets[0].unrest_score > 0.0);
        assert_eq!(buckets[0].distinct_outlets, 2);
        assert!(buckets[0].spike_cold_start);
        assert_eq!(buckets[0].spike_score, 0.5);

        // Baselines were persisted for every time-of-day slot of the cell:
        // one day of history, 2 records in the 00–06 slot, none elsewhere.
        let base = store.baselines(100).wait().unwrap();
        assert_eq!(base.len(), 4);
        assert_eq!(base[0].tod_bucket, 0);
        assert!((base[0].baseline - 2.0).abs() < 1e-9);
        assert!(base.iter().all(|r| r.sample_days == 1));
        assert!((base[1].baseline).abs() < 1e-9);

        // Ingest log kept the failure.
        let (total, rows) = store.ingest_log(10).wait().unwrap();
        assert_eq!(total, 1);
        assert!(rows[0].reason.contains("coordinates out of range"));
    }

    #[test]
    fn retention_prunes_old_events_but_keeps_recent_baselines_warm() {
        let store = open_mem();
        // 40 days of daily attention at one cell: 2 records/day in the 06–12
        // slot (07:00). Spread across days so pruning has something to remove.
        let mut events = Vec::new();
        let day0 = Utc.with_ymd_and_hms(2026, 6, 1, 7, 0, 0).unwrap();
        let mut seq = 0u32;
        for d in 0..40 {
            for _ in 0..2 {
                let mut e = sample_event(seq, EventKind::NewsAttention, 7, 100);
                e.ts_utc = day0 + chrono::Duration::days(d);
                e.id = event_id(SourceId::Fixtures, &format!("evt-{seq}"));
                e.source_event_id = format!("evt-{seq}");
                events.push(e);
                seq += 1;
            }
        }

        // No retention: all 80 events, nothing pruned.
        let r = store.ingest(events.clone(), vec![]).wait().unwrap();
        assert_eq!(r.inserted, 80);
        assert_eq!(r.pruned, 0);

        // Enable 30-day retention and re-ingest (all dedupe; prune then runs).
        // Newest event is day 39 (07:00); cutoff = day 9 (07:00). Days 0–8 are
        // strictly older ⇒ 9 days × 2 = 18 events pruned.
        store.set_retention(Some(30));
        let r2 = store.ingest(events.clone(), vec![]).wait().unwrap();
        assert_eq!(r2.inserted, 0);
        assert_eq!(r2.duplicates, 80);
        assert_eq!(r2.pruned, 18);

        // Extent now starts at day 9; 62 events (31 days × 2) remain.
        let (min_ts, _max) = store.time_extent().wait().unwrap().unwrap();
        assert_eq!(min_ts, (day0 + chrono::Duration::days(9)).timestamp());

        // Baselines stay warm: with 31 retained days behind the newest bucket,
        // the trailing 28-day median for the 06–12 slot is still full (28) and
        // reads 2 records/6 h — retention ≥ 28 days preserves this.
        let base = store.baselines(100).wait().unwrap();
        let slot1 = base.iter().find(|r| r.tod_bucket == 1).unwrap();
        assert_eq!(slot1.sample_days, 28);
        assert!((slot1.baseline - 2.0).abs() < 1e-9);

        // Steady state: the online loop only re-sends recent (forward-moving)
        // data, never events already past the cap. Re-ingesting an in-window
        // slice dedupes and prunes nothing.
        let recent: Vec<_> = events
            .iter()
            .filter(|e| e.ts_utc >= day0 + chrono::Duration::days(35))
            .cloned()
            .collect();
        let r3 = store.ingest(recent, vec![]).wait().unwrap();
        assert_eq!(r3.inserted, 0);
        assert_eq!(r3.pruned, 0);
    }

    #[test]
    fn point_query_respects_precision_confidence_and_kind() {
        let store = open_mem();
        let mut country_precision = sample_event(10, EventKind::Protest, 3, 300);
        country_precision.location_precision = LocationPrecision::Country;
        let mut low_conf = sample_event(11, EventKind::Protest, 3, 300);
        low_conf.location_confidence = 0.2;
        let events = vec![
            sample_event(12, EventKind::Protest, 3, 300),
            sample_event(13, EventKind::NewsAttention, 3, 300),
            country_precision,
            low_conf,
        ];
        store.ingest(events, vec![]).wait().unwrap();

        let day = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        let window = (day, day + 86_400);

        // Precision contract: country-precision rows never come back as points.
        let all = store.query_points(window, None, None, 0.0).wait().unwrap();
        assert_eq!(all.len(), 3);

        // Confidence floor.
        let confident = store.query_points(window, None, None, 0.5).wait().unwrap();
        assert_eq!(confident.len(), 2);

        // Kind filter.
        let protests = store
            .query_points(window, Some(vec![EventKind::Protest]), None, 0.0)
            .wait()
            .unwrap();
        assert_eq!(protests.len(), 2);
        assert!(protests.iter().all(|p| p.kind == EventKind::Protest));
    }

    #[test]
    fn region_detail_aggregates_one_cell() {
        let store = open_mem();
        let events = vec![
            sample_event(20, EventKind::Protest, 1, 400),
            sample_event(21, EventKind::Protest, 2, 400),
            sample_event(22, EventKind::NewsAttention, 3, 400),
            sample_event(23, EventKind::Conflict, 3, 999), // other cell
        ];
        store.ingest(events, vec![]).wait().unwrap();

        let day = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        let detail = store
            .region_detail(400, (day, day + 86_400))
            .wait()
            .unwrap();
        let total: u32 = detail.counts_by_kind.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 3);
        assert_eq!(detail.distinct_outlets, 2);
        assert_eq!(detail.headlines.len(), 3);
        assert_eq!(detail.total_articles, 30);
        assert!((detail.mean_confidence - 0.85).abs() < 1e-6);
        assert_eq!(detail.top_themes[0].1, 3); // protest & labor appear 3x each

        // Window-composed scores ride along: both components present, one
        // day of data ⇒ cold-start spike; all rows are city precision.
        let scores = detail.scores.expect("cell has buckets in window");
        assert!(scores.attention > 0.0);
        assert!(scores.unrest > 0.0);
        assert!(scores.spike_cold_start);
        assert_eq!(detail.coarse_share, 0.0);
        assert!(detail.baseline_hint.is_some());
    }

    #[test]
    fn theme_vocab_and_theme_filtered_queries() {
        let store = open_mem();
        let mut flood = sample_event(40, EventKind::NewsAttention, 1, 700);
        flood.themes = vec!["flood".into()];
        let events = vec![
            sample_event(41, EventKind::Protest, 1, 700), // themes: protest, labor
            sample_event(42, EventKind::Protest, 8, 700),
            flood,
        ];
        store.ingest(events, vec![]).wait().unwrap();

        // Vocabulary comes from the data, most-used first.
        let vocab = store.theme_vocab().wait().unwrap();
        assert_eq!(
            vocab,
            vec![
                ("labor".into(), 2),
                ("protest".into(), 2),
                ("flood".into(), 1)
            ]
        );

        let day = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        let window = (day, day + 86_400);

        // Theme-filtered buckets: only the flood record's bucket remains,
        // with counts recomputed over the filtered set.
        let buckets = store
            .query_buckets(window, Some(vec!["flood".into()]))
            .wait()
            .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].attention_count, 1);
        assert_eq!(buckets[0].event_count, 0);

        // Theme-filtered points: both protest events match "labor".
        let points = store
            .query_points(window, None, Some(vec!["labor".into()]), 0.0)
            .wait()
            .unwrap();
        assert_eq!(points.len(), 2);
        assert!(points.iter().all(|p| p.kind == EventKind::Protest));
    }

    #[test]
    fn parquet_export_is_date_partitioned_and_reimportable() {
        let store = open_mem();
        let mut day2 = sample_event(51, EventKind::Conflict, 3, 800);
        day2.ts_utc = Utc.with_ymd_and_hms(2026, 6, 2, 9, 0, 0).unwrap();
        let events = vec![
            sample_event(50, EventKind::NewsAttention, 1, 800),
            day2,
            sample_event(52, EventKind::Protest, 20, 900),
        ];
        store.ingest(events, vec![]).wait().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("session");
        let report = store.export_parquet(out.clone()).wait().unwrap();
        assert_eq!(report.events, 3);
        assert!(report.buckets >= 3);
        assert_eq!(report.baselines, 8); // 2 cells × 4 time-of-day slots

        // Hive date partitioning on disk (the M4 handoff layout).
        let partitions: Vec<String> = std::fs::read_dir(out.join("events"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(partitions.contains(&"date=2026-06-01".to_string()));
        assert!(partitions.contains(&"date=2026-06-02".to_string()));

        // A fresh DuckDB can read everything back, scores included.
        let conn = Connection::open_in_memory().unwrap();
        let glob = |sub: &str| format!("{}/{sub}/**/*.parquet", sql_path(&out));
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}', hive_partitioning=1)",
                    glob("events")
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
        let (buckets, scored): (i64, i64) = conn
            .query_row(
                &format!(
                    "SELECT count(*), count(*) FILTER (WHERE combined_score > 0)
                     FROM read_parquet('{}', hive_partitioning=1)",
                    glob("region_buckets")
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(buckets as u64, report.buckets);
        assert_eq!(scored, buckets, "score columns must survive the roundtrip");
        let baselines: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}/baselines.parquet')",
                    sql_path(&out)
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(baselines, 8);
    }

    #[test]
    fn publish_snapshot_versions_latest_pointer_and_prunes() {
        let store = open_mem();
        store
            .ingest(vec![sample_event(60, EventKind::Protest, 1, 500)], vec![])
            .wait()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("publish");

        let first = store
            .publish_snapshot(root.clone(), Some(2))
            .wait()
            .unwrap();
        assert_eq!(first.events, 1);
        assert!(root.join(&first.version).join("manifest.json").is_file());
        assert_eq!(
            std::fs::read_to_string(root.join("LATEST")).unwrap(),
            first.version
        );

        // A second publish repoints LATEST and both versions survive (keep_last=2).
        let second = store
            .publish_snapshot(root.clone(), Some(2))
            .wait()
            .unwrap();
        assert_ne!(first.version, second.version);
        assert_eq!(
            std::fs::read_to_string(root.join("LATEST")).unwrap(),
            second.version
        );
        assert!(root.join(&first.version).is_dir());

        // A third publish with keep_last=1 prunes everything but itself.
        let third = store
            .publish_snapshot(root.clone(), Some(1))
            .wait()
            .unwrap();
        assert!(!root.join(&first.version).exists());
        assert!(!root.join(&second.version).exists());
        assert!(root.join(&third.version).is_dir());

        // Re-readable via read_parquet, same as a plain export.
        let conn = Connection::open_in_memory().unwrap();
        let glob = format!(
            "{}/events/**/*.parquet",
            sql_path(&root.join(&third.version))
        );
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM read_parquet('{glob}', hive_partitioning=1)"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn persists_to_file_and_migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.duckdb");
        {
            let store = StorageHandle::open(Some(path.clone()), Box::new(|| {})).unwrap();
            store
                .ingest(vec![sample_event(30, EventKind::Protest, 1, 500)], vec![])
                .wait()
                .unwrap();
        }
        // Re-open: data survives, migrations re-run harmlessly.
        let store = StorageHandle::open(Some(path), Box::new(|| {})).unwrap();
        let extent = store.time_extent().wait().unwrap();
        assert!(extent.is_some());
    }
}
