//! M4 read API: axum over the Parquet snapshots `services/workers` publishes
//! (docs/API.md). Never opens a `.duckdb` file — DuckDB is
//! single-writer-per-file across processes (docs/ARCHITECTURE.md). Each
//! request resolves the current snapshot from a `LATEST` pointer file and
//! runs `read_parquet(...)` against it on a fresh in-memory connection; there
//! is no persistent connection or cache to invalidate, since every snapshot
//! is immutable once published.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use core_types::{EventKind, LocationPrecision, bucket_start_epoch};
use duckdb::{Connection, params};
use serde::{Deserialize, Serialize};

/// Rows examined per `/events` request, as a memory safety valve (mirrors
/// `storage::MAX_POINT_ROWS`).
const MAX_POINT_ROWS: i64 = 100_000;

#[derive(Clone)]
struct AppState {
    publish_root: PathBuf,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let publish_root: PathBuf = std::env::var("LES_PUBLISH_DIR")
        .map(PathBuf::from)
        .map_err(|_| {
            anyhow::anyhow!("LES_PUBLISH_DIR not set — point it at services/workers' publish root")
        })?;
    let bind = std::env::var("LES_API_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    tracing::info!(publish_root = %publish_root.display(), bind, "api starting");

    let state = AppState { publish_root };
    let app = Router::new()
        .route("/health", get(health))
        .route("/meta", get(meta))
        .route("/buckets", get(buckets))
        .route("/events", get(events))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

enum ApiError {
    NoSnapshot,
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::NoSnapshot => (
                StatusCode::SERVICE_UNAVAILABLE,
                "no snapshot published yet".to_string(),
            ),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<duckdb::Error> for ApiError {
    fn from(e: duckdb::Error) -> Self {
        ApiError::Internal(format!("duckdb: {e}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: String,
    published_at_epoch_s: i64,
    events: u64,
    buckets: u64,
    baselines: u64,
}

/// Read the `LATEST` pointer and return the snapshot directory it names.
fn resolve_snapshot(root: &Path) -> Result<(String, PathBuf), ApiError> {
    let version = std::fs::read_to_string(root.join("LATEST")).map_err(|_| ApiError::NoSnapshot)?;
    let version = version.trim().to_string();
    let dir = root.join(&version);
    Ok((version, dir))
}

/// A filesystem path as a single-quoted DuckDB SQL string literal (mirrors
/// `storage::sql_path`).
fn sql_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace('\'', "''")
}

fn glob(snapshot_dir: &Path, table: &str) -> String {
    format!("{}/{table}/**/*.parquet", sql_path(snapshot_dir))
}

async fn health(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let (version, dir) = resolve_snapshot(&state.publish_root)?;
        let bytes = std::fs::read(dir.join("manifest.json"))
            .map_err(|e| ApiError::Internal(format!("manifest: {e}")))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::Internal(format!("manifest json: {e}")))?;
        debug_assert_eq!(manifest.version, version);
        Ok(Json(
            serde_json::json!({ "status": "ok", "snapshot": manifest }),
        ))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

async fn meta(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let (_, dir) = resolve_snapshot(&state.publish_root)?;
        let conn = Connection::open_in_memory()?;
        let events_glob = glob(&dir, "events");

        let (min, max): (Option<i64>, Option<i64>) = conn.query_row(
            &format!(
                "SELECT min(ts_epoch_s), max(ts_epoch_s) FROM read_parquet('{events_glob}', hive_partitioning=1)"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let time_extent = match (min, max) {
            (Some(a), Some(b)) => Some(serde_json::json!({ "start_epoch_s": a, "end_epoch_s": b + 1 })),
            _ => None,
        };

        let mut stmt = conn.prepare(&format!(
            "SELECT themes FROM read_parquet('{events_glob}', hive_partitioning=1)"
        ))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for row in rows {
            let themes: Vec<String> = serde_json::from_str(&row?).unwrap_or_default();
            for theme in themes {
                *counts.entry(theme).or_insert(0) += 1;
            }
        }
        let mut themes: Vec<(String, u32)> = counts.into_iter().collect();
        themes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let themes: Vec<_> = themes
            .into_iter()
            .map(|(theme, count)| serde_json::json!({ "theme": theme, "count": count }))
            .collect();

        Ok(Json(
            serde_json::json!({ "time_extent": time_extent, "themes": themes }),
        ))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

#[derive(Debug, Deserialize)]
struct BucketsQuery {
    start: i64,
    end: i64,
    h3_cell: Option<u64>,
}

async fn buckets(
    State(state): State<AppState>,
    Query(q): Query<BucketsQuery>,
) -> Result<Json<Vec<core_types::RegionBucket>>, ApiError> {
    if q.end <= q.start {
        return Err(ApiError::BadRequest("end must be > start".into()));
    }
    tokio::task::spawn_blocking(move || {
        let (_, dir) = resolve_snapshot(&state.publish_root)?;
        let conn = Connection::open_in_memory()?;
        let bucket_glob = glob(&dir, "region_buckets");
        let mut stmt = conn.prepare(&format!(
            "SELECT h3_cell, bucket_start, event_count, attention_count, article_count,
                    source_count, distinct_outlets, attention_score, unrest_score,
                    spike_score, combined_score, baseline, spike_cold_start
             FROM read_parquet('{bucket_glob}', hive_partitioning=1)
             WHERE bucket_start >= ? AND bucket_start < ?
               AND h3_cell = coalesce(?, h3_cell)
             ORDER BY h3_cell, bucket_start"
        ))?;
        let from = bucket_start_epoch(q.start);
        let rows = stmt.query_map(params![from, q.end, q.h3_cell.map(|v| v as i64)], |r| {
            Ok(core_types::RegionBucket {
                h3_cell: r.get::<_, i64>(0)? as u64,
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
        let out = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(Json(out))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

#[derive(Debug, Serialize)]
struct EventPointDto {
    id: u64,
    lat: f64,
    lon: f64,
    kind: EventKind,
    precision: LocationPrecision,
    confidence: f32,
    ts_epoch_s: i64,
    article_count: u32,
    headline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    start: i64,
    end: i64,
    kinds: Option<String>,
    themes: Option<String>,
    #[serde(default)]
    min_confidence: f32,
}

async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventPointDto>>, ApiError> {
    if q.end <= q.start {
        return Err(ApiError::BadRequest("end must be > start".into()));
    }
    let kinds: Option<Vec<EventKind>> = match q.kinds {
        Some(s) => Some(
            s.split(',')
                .map(|k| {
                    EventKind::parse(k.trim())
                        .ok_or_else(|| ApiError::BadRequest(format!("unknown kind `{k}`")))
                })
                .collect::<Result<_, _>>()?,
        ),
        None => None,
    };
    let themes: Option<Vec<String>> = q
        .themes
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect());

    tokio::task::spawn_blocking(move || {
        let (_, dir) = resolve_snapshot(&state.publish_root)?;
        let conn = Connection::open_in_memory()?;
        let events_glob = glob(&dir, "events");
        let mut stmt = conn.prepare(&format!(
            "SELECT id, lat, lon, kind, location_precision, location_confidence,
                    ts_epoch_s, article_count, headline, themes
             FROM read_parquet('{events_glob}', hive_partitioning=1)
             WHERE ts_epoch_s >= ? AND ts_epoch_s < ?
               AND location_precision IN ('city', 'exact')
               AND location_confidence >= ?
             ORDER BY ts_epoch_s
             LIMIT ?"
        ))?;
        let rows = stmt.query_map(
            params![q.start, q.end, q.min_confidence, MAX_POINT_ROWS],
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
            let (id, lat, lon, kind, precision, confidence, ts, articles, headline, themes_s) =
                row?;
            let kind = EventKind::parse(&kind)
                .ok_or_else(|| ApiError::Internal(format!("corrupt kind `{kind}`")))?;
            if let Some(filter) = &kinds
                && !filter.contains(&kind)
            {
                continue;
            }
            if let Some(filter) = &themes {
                let event_themes: Vec<String> = serde_json::from_str(&themes_s).unwrap_or_default();
                if !event_themes.iter().any(|t| filter.contains(t)) {
                    continue;
                }
            }
            let precision = LocationPrecision::parse(&precision)
                .ok_or_else(|| ApiError::Internal(format!("corrupt precision `{precision}`")))?;
            out.push(EventPointDto {
                id: id as u64,
                lat,
                lon,
                kind,
                precision,
                confidence,
                ts_epoch_s: ts,
                article_count: articles as u32,
                headline,
            });
        }
        Ok(Json(out))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}
