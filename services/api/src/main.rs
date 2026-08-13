//! M4 read API: axum over the Parquet snapshots `services/workers` publishes
//! (docs/API.md). Never opens a `.duckdb` file — DuckDB is
//! single-writer-per-file across processes (docs/ARCHITECTURE.md). Each
//! request resolves the current snapshot from a `LATEST` pointer file and
//! runs `read_parquet(...)` against it on a fresh in-memory connection; there
//! is no persistent connection or cache to invalidate, since every snapshot
//! is immutable once published.
//!
//! M7 layered a hardening stack on top (docs/API.md): request timeout,
//! concurrency cap, per-IP rate limit, CORS, response compression, request
//! tracing, graceful shutdown, and a snapshot-version `ETag` on every
//! response.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use core_types::{EventKind, LocationPrecision, bucket_start_epoch};
use duckdb::{Connection, params};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// Rows examined per `/events` request, as a memory safety valve (mirrors
/// `storage::MAX_POINT_ROWS`). Also the ceiling pagination `limit` clamps to.
const MAX_POINT_ROWS: i64 = 100_000;

/// `/events` page size when the `limit` query param is omitted.
const DEFAULT_EVENTS_PAGE_SIZE: i64 = 500;

/// Per-request wall-clock budget before the middleware aborts it with `408`.
/// Every query here runs against a small, already-local Parquet snapshot, so
/// this is generous headroom, not a tuned SLA.
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Caps requests served concurrently; beyond this, new requests queue via
/// ordinary `Service` backpressure instead of opening unbounded DuckDB
/// connections.
const MAX_CONCURRENT_REQUESTS: usize = 64;

/// Per-IP token bucket (`tower_governor`, keyed on peer IP by default):
/// sustained requests/second, and the burst it can spend before throttling.
const RATE_LIMIT_PER_SECOND: u64 = 10;
const RATE_LIMIT_BURST: u32 = 20;

/// `/health` flags the snapshot as `stale` past this age. The worker
/// republishes after every ingest cycle (GDELT-cadence, well under an
/// hour), so this is generous headroom against normal cadence/backoff
/// jitter, not a tuned SLA — same kind of judgment call as
/// `analytics::weights::SPIKE_HALO_THRESHOLD`.
const HEALTH_STALE_THRESHOLD_SECS: i64 = 3600;

#[derive(Clone)]
struct AppState {
    publish_root: PathBuf,
    /// Default `false`: `/events` never returns ACLED-sourced rows, in code
    /// rather than relying on operator discipline — ACLED data is not
    /// redistributable and must never be served publicly
    /// (docs/SAFETY_AND_PRIVACY.md). Escape hatch for a private/authorized
    /// deployment only, via `LES_API_ALLOW_ACLED=1`. `/buckets`/`/meta`
    /// cannot be filtered this way — `region_buckets` aggregates every
    /// source's contribution into one cell/window score with no per-source
    /// breakdown (core_types::RegionBucket), so keeping ACLED out of those
    /// figures is a worker-side policy (never run the publicly-reachable
    /// worker with `acled-live`), not an api-side one.
    allow_acled: bool,
    /// `Clone`, cheap (an `Arc` internally) — shared with every request via
    /// `State`, rendered by `GET /metrics`.
    metrics_handle: PrometheusHandle,
    /// Pre-serialized once at startup (never changes at runtime — the route
    /// table is fixed), served verbatim by `GET /openapi.json`. `Arc<str>`
    /// so `AppState`'s per-request `Clone` (every handler using `State`
    /// pays it) doesn't copy the whole spec.
    openapi_json: Arc<str>,
}

/// SQL fragment appended to the `/events` query. Not built from request
/// input — derived once from server config at startup — so string
/// interpolation here carries no injection risk.
const ACLED_EXCLUSION_CLAUSE: &str = "AND source <> 'acled'";

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
    let allow_acled = std::env::var("LES_API_ALLOW_ACLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if allow_acled {
        tracing::warn!(
            "LES_API_ALLOW_ACLED is set — this instance will serve ACLED-sourced /events rows; \
             ACLED data must never be served publicly (docs/SAFETY_AND_PRIVACY.md)"
        );
    }

    tracing::info!(publish_root = %publish_root.display(), bind, "api starting");

    let metrics_handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("prometheus recorder install (in-process render only, no listener)");

    // Built once at startup from the same `#[utoipa::path]`-annotated
    // handlers the router below registers — `OpenApiRouter` here is used
    // purely to collect the spec, not to route (that stays the plain
    // `Router` below, which already existed pre-M7 and every other layer
    // is already wired against).
    let openapi = OpenApiRouter::<AppState>::new()
        .routes(routes!(health))
        .routes(routes!(meta))
        .routes(routes!(buckets))
        .routes(routes!(events))
        .routes(routes!(metrics_handler))
        .into_openapi();
    let openapi_json: Arc<str> = serde_json::to_string(&openapi)
        .expect("OpenApi always serializes")
        .into();

    let state = AppState {
        publish_root,
        allow_acled,
        metrics_handle,
        openapi_json,
    };

    let governor_conf = GovernorConfigBuilder::default()
        .per_second(RATE_LIMIT_PER_SECOND)
        .burst_size(RATE_LIMIT_BURST)
        .finish()
        .expect("rate-limit config is a fixed valid quota, not user input");
    let cors = CorsLayer::new()
        .allow_methods([Method::GET])
        .allow_origin(Any);

    // Layers stack outside-in in the order they're added last-to-first:
    // trace (outermost, sees every request incl. timeouts/rejections) ->
    // timeout -> concurrency cap -> per-IP rate limit -> compression -> cors
    // -> etag -> routes (innermost).
    let app = Router::new()
        .route("/health", get(health))
        .route("/meta", get(meta))
        .route("/buckets", get(buckets))
        .route("/events", get(events))
        .route("/metrics", get(metrics_handler))
        .route("/openapi.json", get(openapi_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            etag_middleware,
        ))
        .layer(middleware::from_fn(record_request_metrics))
        .layer(cors)
        .layer(CompressionLayer::new())
        .layer(GovernorLayer::new(governor_conf))
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("api listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

/// Waits for Ctrl+C or (on Unix) `SIGTERM`, then lets `axum::serve` drain
/// in-flight requests before the process exits — the same signal a
/// container orchestrator sends before a hard kill.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received, draining in-flight requests");
}

/// Stamps every response with the current snapshot version as a quoted
/// `ETag`, and short-circuits to `304` when the caller's `If-None-Match`
/// already matches it — the snapshot is immutable once published, so an
/// unchanged version guarantees an unchanged response body for any of these
/// read-only endpoints. Resolves the version itself (rather than trusting a
/// handler to report it) so `/health`'s `503` (no snapshot yet) still gets a
/// pass-through response with no `ETag`, instead of every handler
/// duplicating this lookup.
async fn etag_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let version = tokio::task::spawn_blocking({
        let root = state.publish_root.clone();
        move || resolve_snapshot(&root).ok().map(|(version, _)| version)
    })
    .await
    .ok()
    .flatten();

    if let Some(version) = &version {
        let expected = format!("\"{version}\"");
        if headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            == Some(expected.as_str())
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }

    let mut response = next.run(request).await;
    if let Some(version) = version
        && let Ok(value) = HeaderValue::from_str(&format!("\"{version}\""))
    {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// Records a request count and latency histogram, labeled by path/method/
/// status. Every route here is a fixed literal (no `/events/:id`-style path
/// params), so the label cardinality is bounded by the route table, not by
/// request input.
async fn record_request_metrics(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().to_string();
    let start = std::time::Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16().to_string();
    metrics::counter!("http_requests_total", "path" => path.clone(), "method" => method.clone(), "status" => status)
        .increment(1);
    metrics::histogram!("http_request_duration_seconds", "path" => path, "method" => method)
        .record(start.elapsed().as_secs_f64());
    response
}

/// Prometheus exposition text: `http_requests_total{path,method,status}`
/// and `http_request_duration_seconds{path,method}`.
#[utoipa::path(
    get,
    path = "/metrics",
    responses((status = 200, description = "Prometheus exposition text"))
)]
async fn metrics_handler(State(state): State<AppState>) -> String {
    state.metrics_handle.render()
}

async fn openapi_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        state.openapi_json.to_string(),
    )
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// Pure so it's unit-testable without wall-clock time or a real snapshot —
/// `health`'s own manual check depends on both.
fn snapshot_staleness(now_epoch_s: i64, published_at_epoch_s: i64) -> (i64, bool) {
    let age_s = (now_epoch_s - published_at_epoch_s).max(0);
    (age_s, age_s > HEALTH_STALE_THRESHOLD_SECS)
}

/// Readiness probe: reads `LATEST` + `manifest.json` only, no Parquet
/// query. `stale` surfaces snapshot age as data rather than a status code,
/// so it stays distinguishable from `503` (no snapshot at all).
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Snapshot present; `stale` set past HEALTH_STALE_THRESHOLD_SECS"),
        (status = 503, description = "No snapshot published yet"),
    )
)]
async fn health(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let (version, dir) = resolve_snapshot(&state.publish_root)?;
        let bytes = std::fs::read(dir.join("manifest.json"))
            .map_err(|e| ApiError::Internal(format!("manifest: {e}")))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::Internal(format!("manifest json: {e}")))?;
        debug_assert_eq!(manifest.version, version);
        let now_epoch_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(manifest.published_at_epoch_s);
        let (snapshot_age_s, stale) =
            snapshot_staleness(now_epoch_s, manifest.published_at_epoch_s);
        Ok(Json(serde_json::json!({
            "status": "ok",
            "snapshot": manifest,
            "snapshot_age_s": snapshot_age_s,
            "stale": stale,
        })))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

/// Time extent and theme vocabulary across the whole retained snapshot.
#[utoipa::path(
    get,
    path = "/meta",
    responses(
        (status = 200, description = "Time extent + per-theme counts"),
        (status = 503, description = "No snapshot published yet"),
    )
)]
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

#[derive(Debug, Deserialize, IntoParams)]
struct BucketsQuery {
    start: i64,
    end: i64,
    h3_cell: Option<u64>,
}

/// `RegionBucket` rows (core-types, unchanged JSON shape) in a half-open
/// window, optionally restricted to one H3 cell. See docs/DATA_MODEL.md
/// for `RegionBucket`'s fields — it isn't schema'd here to avoid adding an
/// OpenAPI-only dependency to `core-types`, which is otherwise a pure,
/// I/O-free domain-types crate.
#[utoipa::path(
    get,
    path = "/buckets",
    params(BucketsQuery),
    responses(
        (status = 200, description = "RegionBucket[] for the window"),
        (status = 400, description = "end must be > start"),
        (status = 503, description = "No snapshot published yet"),
    )
)]
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

#[derive(Debug, Serialize, ToSchema)]
struct EventPointDto {
    id: u64,
    lat: f64,
    lon: f64,
    /// `EventKind` serializes as its snake_case name (e.g. `"protest"`);
    /// schema'd as a plain string here rather than pulling `utoipa` into
    /// `core-types` for one enum.
    #[schema(value_type = String)]
    kind: EventKind,
    /// `LocationPrecision` serializes as its snake_case name (e.g. `"city"`).
    #[schema(value_type = String)]
    precision: LocationPrecision,
    confidence: f32,
    ts_epoch_s: i64,
    article_count: u32,
    headline: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
struct EventsQuery {
    start: i64,
    end: i64,
    kinds: Option<String>,
    themes: Option<String>,
    #[serde(default)]
    min_confidence: f32,
    offset: Option<i64>,
    limit: Option<i64>,
}

/// Mirrors `storage::RegionEventsPage`'s shape: `total` lets a caller detect
/// the last page without an extra request.
#[derive(Debug, Serialize, ToSchema)]
struct EventsPage {
    total: usize,
    offset: i64,
    limit: i64,
    rows: Vec<EventPointDto>,
}

/// Point-renderable event rows (only `city`/`exact` precision), paginated
/// and ordered `(ts_epoch_s DESC, id DESC)` — see docs/API.md for the
/// pagination and ACLED-exclusion contract in full.
#[utoipa::path(
    get,
    path = "/events",
    params(EventsQuery),
    responses(
        (status = 200, description = "A page of event rows", body = EventsPage),
        (status = 400, description = "end must be > start, or an unknown `kinds` entry"),
        (status = 503, description = "No snapshot published yet"),
    )
)]
async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<EventsPage>, ApiError> {
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
        let acled_clause = if state.allow_acled {
            ""
        } else {
            ACLED_EXCLUSION_CLAUSE
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id, lat, lon, kind, location_precision, location_confidence,
                    ts_epoch_s, article_count, headline, themes
             FROM read_parquet('{events_glob}', hive_partitioning=1)
             WHERE ts_epoch_s >= ? AND ts_epoch_s < ?
               AND location_precision IN ('city', 'exact')
               AND location_confidence >= ?
               {acled_clause}
             ORDER BY ts_epoch_s, id
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
        // `kinds`/`themes` are filtered here in Rust (not SQL), so pagination
        // has to happen after that filter too, over the fully-matched set —
        // otherwise `total` would count pre-filter rows and a page could
        // silently return fewer than `limit` results. `(ts_epoch_s DESC, id
        // DESC)` mirrors the region-events ledger's tiebreak
        // (storage::do_region_events): without the `id`, rows sharing a
        // timestamp can repeat or vanish across pages.
        out.sort_by(|a, b| {
            b.ts_epoch_s
                .cmp(&a.ts_epoch_s)
                .then_with(|| b.id.cmp(&a.id))
        });
        let total = out.len();
        let offset = q.offset.unwrap_or(0).clamp(0, i64::MAX) as usize;
        let limit = q
            .limit
            .unwrap_or(DEFAULT_EVENTS_PAGE_SIZE)
            .clamp(1, MAX_POINT_ROWS) as usize;
        let rows = out.into_iter().skip(offset).take(limit).collect();
        Ok(Json(EventsPage {
            total,
            offset: offset as i64,
            limit: limit as i64,
            rows,
        }))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_staleness_is_false_right_after_publish() {
        let (age_s, stale) = snapshot_staleness(1_000_000, 1_000_000);
        assert_eq!(age_s, 0);
        assert!(!stale);
    }

    #[test]
    fn snapshot_staleness_flips_past_the_threshold() {
        let published = 1_000_000;
        let (age_s, stale) = snapshot_staleness(published + HEALTH_STALE_THRESHOLD_SECS, published);
        assert_eq!(age_s, HEALTH_STALE_THRESHOLD_SECS);
        assert!(!stale, "exactly at the threshold is not yet stale");

        let (age_s, stale) =
            snapshot_staleness(published + HEALTH_STALE_THRESHOLD_SECS + 1, published);
        assert_eq!(age_s, HEALTH_STALE_THRESHOLD_SECS + 1);
        assert!(stale);
    }

    #[test]
    fn snapshot_staleness_never_goes_negative() {
        // A published_at newer than "now" shouldn't happen, but a clock
        // skew (or a manifest write racing this read) shouldn't produce a
        // negative age either.
        let (age_s, stale) = snapshot_staleness(1_000_000, 1_000_500);
        assert_eq!(age_s, 0);
        assert!(!stale);
    }
}
