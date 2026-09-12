//! Slippy-tile basemap worker (docs/BASEMAP.md §6): fetches NASA GIBS
//! EPSG:4326 shaded-relief tiles for the visible viewport, decodes the JPEG,
//! and hands [`egui::ColorImage`]s back to the UI thread for upload.
//!
//! Like [`crate::digest`] and [`crate::media`] this is a long-lived thread
//! with a current-thread Tokio runtime and a `std::sync::mpsc` reply channel.
//! Unlike them it is driven by *panning*, not by an explicit action: the UI
//! sends the current wanted-tile list as a **replacement** (never a queue), so
//! a fast pan cancels stale work instead of accumulating it. In-flight
//! fetches are bounded ([`CONCURRENT_FETCHES`]), and a result that arrives for
//! a tile that has since left the wanted set is simply dropped.
//!
//! The worker never opens storage and never touches the disk — Phase 2 is
//! session-only; the disk cache is Phase 3 (docs/BASEMAP.md §4). The UI
//! thread's only work is draining the channel and calling `ctx.load_texture`,
//! bounded to [`UPLOADS_PER_FRAME`] uploads per frame, so no network call, no
//! decode, and no filesystem read ever happens on the UI thread.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::Duration;

use egui::{ColorImage, TextureHandle, TextureId};
use renderer::{TileId, TileMatrixSet};
use tokio::sync::mpsc as tokio_mpsc;

/// NASA GIBS `ASTER_GDEM_Color_Shaded_Relief` in the `31.25m` EPSG:4326
/// matrix set: 512 px tiles, level 0 = 2×1 (180° tiles), halving per level,
/// max level 11. Static, undated ASTER GDEM shaded relief — deliberately not
/// a dated true-colour layer, so the imagery cannot be read as a photograph
/// *of* the event (docs/BASEMAP.md §2 honesty decision). Finer than the
/// BlueMarble basemap: level 8 (≈0.00137°/px) reaches under the app's zoom
/// floor. The renderer's tile math is parameterized by this same value, so
/// the fetch URL and the drawn layer cannot drift apart.
pub const TILESET: TileMatrixSet = TileMatrixSet::new(512, 2, 1, 11);

/// Cap on resident decoded textures: 96 × 1 MiB (512² RGBA) ≈ 96 MiB of VRAM
/// (docs/BASEMAP.md §4).
pub const RESIDENT_TEXTURE_CAP: usize = 96;

/// Texture uploads allowed in one frame, so a burst of tile arrivals cannot
/// spike frame time (docs/BASEMAP.md §4).
pub const UPLOADS_PER_FRAME: usize = 4;

/// How many tiles the worker fetches at once (docs/BASEMAP.md §6).
const CONCURRENT_FETCHES: usize = 2;

/// GIBS WMTS REST base for the static shaded-relief layer in the `31.25m`
/// matrix set. Tile order is `{z}/{row}/{col}` (row before column), the
/// documented GIBS REST pattern (docs/BASEMAP.md §2).
const GIBS_BASE: &str = "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
ASTER_GDEM_Color_Shaded_Relief/default/default/31.25m";

/// The REST URL for one tile: `{base}/{z}/{row}/{col}.jpg`.
fn tile_url(id: TileId) -> String {
    format!("{GIBS_BASE}/{}/{}/{}.jpg", id.level, id.row, id.col)
}

/// Why a tile fetch failed, with enough structure for the worker to back off
/// instead of hammering the provider.
pub enum TileError {
    /// The provider asked us to slow down (HTTP 429). `retry_after` is the
    /// provider's own hint, when present and parseable.
    RateLimited { retry_after: Option<Duration> },
    /// Anything else — network, non-2xx, decode. User-safe reason.
    Failed(String),
}

/// How long to hold off a single tile after a non-rate-limit failure, so a
/// persistently bad tile cannot retry in a tight loop.
const TILE_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Cooldown floor and ceiling after a 429: doubles per consecutive rate
/// limit, capped, so a burst backs off without going silent forever.
const COOLDOWN_BASE: Duration = Duration::from_secs(2);
const COOLDOWN_MAX: Duration = Duration::from_secs(60);

/// Backoff after a 429: the provider's `Retry-After` when present (capped),
/// else exponential doubling from [`COOLDOWN_BASE`] capped at
/// [`COOLDOWN_MAX`].
fn cooldown_for(consecutive_429: u32, hint: Option<Duration>) -> Duration {
    if let Some(h) = hint {
        return h.min(COOLDOWN_MAX);
    }
    let factor = 1u32 << consecutive_429.saturating_sub(1).min(6);
    COOLDOWN_BASE.saturating_mul(factor).min(COOLDOWN_MAX)
}

// ---------------------------------------------------------------------------
// Feature-gated HTTP handle — the same stub-module pattern the live sources
// and the media/digest workers use, so the worker body stays free of `cfg`
// arms. With the feature off `make()` yields `None` and the toggle says so.
// ---------------------------------------------------------------------------

#[cfg(feature = "tiles-live")]
mod api {
    use super::*;
    use zune_jpeg::JpegDecoder;

    pub struct TileFetcher {
        http: reqwest::Client,
    }

    pub const BUILT: bool = true;

    pub fn make() -> Option<TileFetcher> {
        let http = match reqwest::Client::builder()
            .user_agent(concat!(
                "live-earth-signals/",
                env!("CARGO_PKG_VERSION"),
                " (civic-data research dashboard)"
            ))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!("tile fetcher unavailable: {e}");
                return None;
            }
        };
        Some(TileFetcher { http })
    }

    impl TileFetcher {
        /// Fetch and decode one tile. `Err` is structured so the worker can
        /// back off on 429s instead of retrying in a tight loop.
        pub async fn tile(&self, id: TileId) -> Result<ColorImage, TileError> {
            let url = tile_url(id);
            let resp = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| TileError::Failed(e.to_string()))?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                return Err(TileError::RateLimited { retry_after });
            }
            if !resp.status().is_success() {
                return Err(TileError::Failed(format!("HTTP {}", resp.status())));
            }
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| TileError::Failed(e.to_string()))?;
            decode(&bytes).map_err(TileError::Failed)
        }
    }

    fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
        // `JpegDecoder` reads from any `BufRead + Seek`; a `Cursor` over the
        // bytes is the in-memory form, and `&[u8]` alone is not `Seek`.
        let mut decoder = JpegDecoder::new(std::io::Cursor::new(bytes));
        let pixels = decoder.decode().map_err(|e| format!("jpeg: {e}"))?;
        let (width, height) = decoder.dimensions().ok_or("jpeg: no image info")?;
        if width == 0 || height == 0 {
            return Err("jpeg: empty image".into());
        }
        Ok(ColorImage::from_rgb([width, height], &pixels))
    }
}

#[cfg(not(feature = "tiles-live"))]
mod api {
    use super::*;

    pub struct TileFetcher;

    pub const BUILT: bool = false;

    pub fn make() -> Option<TileFetcher> {
        None
    }

    impl TileFetcher {
        pub async fn tile(&self, _: TileId) -> Result<ColorImage, TileError> {
            unreachable!("built without the tiles-live feature")
        }
    }
}

/// Why imagery is unavailable, in the words the toggle shows.
pub fn unavailable_reason() -> &'static str {
    if api::BUILT {
        "The tile fetcher could not start its HTTP client — see the log."
    } else {
        "This build has the `tiles-live` feature off, so it cannot download imagery."
    }
}

/// One decoded tile from the worker.
pub struct TileMsg {
    pub tile: TileId,
    /// `Err` carries a user-safe reason; successful payloads are already
    /// decoded, so the UI thread does no image work beyond the upload.
    pub image: Result<ColorImage, String>,
}

enum Ctl {
    /// The tiles the UI still needs. A *replacement*, not a queue: a tile no
    /// longer in the list is abandoned.
    SetVisible(Vec<TileId>),
}

/// UI-side handle. Dropping it stops the worker.
pub struct TilesHandle {
    ctl: tokio_mpsc::UnboundedSender<Ctl>,
    available: bool,
}

impl TilesHandle {
    /// Whether this build can fetch imagery at all (feature on *and* the HTTP
    /// client built).
    pub fn available(&self) -> bool {
        self.available
    }

    /// Replace the wanted-tile set. Tiles no longer wanted are dropped.
    pub fn set_visible(&self, tiles: Vec<TileId>) {
        let _ = self.ctl.send(Ctl::SetVisible(tiles));
    }
}

/// Resident decoded textures, LRU-bounded (docs/BASEMAP.md §4). Owns the
/// [`TextureHandle`]s so the GPU textures stay alive; `ids` is the
/// renderer-facing lookup, kept in lockstep with the handles.
pub struct TileCache {
    ids: HashMap<TileId, TextureId>,
    handles: HashMap<TileId, TextureHandle>,
    /// Least-recently-used first.
    order: VecDeque<TileId>,
    cap: usize,
    /// Bumped whenever the drawn tile set changes, so the tile mesh cache
    /// rebuilds once per change rather than never or every frame.
    pub generation: u64,
}

impl Default for TileCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TileCache {
    pub fn new() -> Self {
        Self {
            ids: HashMap::new(),
            handles: HashMap::new(),
            order: VecDeque::new(),
            cap: RESIDENT_TEXTURE_CAP,
            generation: 0,
        }
    }

    /// The renderer-facing lookup of resident tiles.
    pub fn ids(&self) -> &HashMap<TileId, TextureId> {
        &self.ids
    }

    pub fn contains(&self, id: &TileId) -> bool {
        self.handles.contains_key(id)
    }

    /// Insert a decoded tile (or refresh an existing one). Returns the id of
    /// the tile evicted to stay under the cap, if any. Bumps `generation`
    /// only when the drawn set actually changes.
    pub fn insert(&mut self, id: TileId, handle: TextureHandle) -> Option<TileId> {
        let existed = self.handles.contains_key(&id);
        let tex_id = handle.id();
        self.handles.insert(id, handle);
        self.ids.insert(id, tex_id);
        self.order.retain(|t| *t != id);
        self.order.push_back(id);

        let mut evicted = None;
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                // Dropping the handle frees the GPU texture.
                self.handles.remove(&old);
                self.ids.remove(&old);
                evicted = Some(old);
            } else {
                break;
            }
        }
        if !existed || evicted.is_some() {
            self.generation = self.generation.wrapping_add(1);
        }
        evicted
    }
}

/// Spawn the tile worker. `wake` (a repaint request) fires after every
/// message so the UI polls promptly.
pub fn spawn(wake: impl Fn() + Send + 'static) -> (mpsc::Receiver<TileMsg>, TilesHandle) {
    let (tx_res, rx_res) = mpsc::channel();
    let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();

    let fetcher = api::make();
    let available = fetcher.is_some();

    std::thread::Builder::new()
        .name("tiles".into())
        .spawn(move || {
            let Some(fetcher) = fetcher else {
                // Drop the receivers so a stray request fails fast rather than
                // being queued for a worker that is not running.
                return;
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("tiles tokio runtime: {e}");
                    return;
                }
            };
            runtime.block_on(worker(fetcher, tx_res, rx_ctl, wake));
        })
        .expect("spawn tiles thread");

    (
        rx_res,
        TilesHandle {
            ctl: tx_ctl,
            available,
        },
    )
}

async fn worker(
    fetcher: api::TileFetcher,
    tx: mpsc::Sender<TileMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
) {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    use std::collections::HashSet;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::time::Instant;

    // A boxed fetch. Local (`!Send`) because the worker runs on a
    // current-thread runtime and the futures borrow the shared client — the
    // same shape [`crate::media`] uses for its three provider legs.
    type Fetch<'a> = Pin<Box<dyn Future<Output = (TileId, Result<ColorImage, TileError>)> + 'a>>;

    let mut wanted: Vec<TileId> = Vec::new();
    let mut in_flight: FuturesUnordered<Fetch<'_>> = FuturesUnordered::new();
    let mut in_flight_ids: HashSet<TileId> = HashSet::new();
    // Tiles to leave alone until this instant (per-tile failure backoff).
    let mut retry_after: HashMap<TileId, Instant> = HashMap::new();
    // After a 429, pause all new fetches until this instant.
    let mut cooldown_until: Option<Instant> = None;
    let mut consecutive_429: u32 = 0;

    loop {
        // Adopt the latest wanted-set replacement (drain bursts so a run of
        // pan frames collapses to the newest set, never a queue).
        let mut ctl: Option<Ctl> = None;
        loop {
            match rx_ctl.try_recv() {
                Ok(c) => ctl = Some(c),
                Err(tokio_mpsc::error::TryRecvError::Empty) => break,
                Err(tokio_mpsc::error::TryRecvError::Disconnected) => return,
            }
        }
        if let Some(Ctl::SetVisible(tiles)) = ctl {
            wanted = dedup(tiles);
        }

        // Top up to the concurrency cap from the latest wanted set, skipping
        // anything still cooling down and any per-tile backoff that has not
        // elapsed. A tile already in flight is never re-requested.
        let now = Instant::now();
        if cooldown_until.is_none_or(|c| now >= c) {
            while in_flight_ids.len() < CONCURRENT_FETCHES {
                let next = wanted.iter().copied().find(|t| {
                    !in_flight_ids.contains(t) && retry_after.get(t).is_none_or(|at| now >= *at)
                });
                let Some(next) = next else { break };
                in_flight_ids.insert(next);
                in_flight.push(Box::pin(fetch_tile(&fetcher, next)));
            }
        }

        tokio::select! {
            biased;
            done = in_flight.next(), if !in_flight.is_empty() => {
                let Some((tile, result)) = done else {
                    unreachable!("guarded by !in_flight.is_empty()");
                };
                in_flight_ids.remove(&tile);
                match result {
                    Ok(img) => {
                        consecutive_429 = 0;
                        retry_after.remove(&tile);
                        // A tile that has since left the wanted set is stale
                        // — drop it rather than painting a place the user has
                        // already panned away from.
                        if wanted.contains(&tile) {
                            if tx.send(TileMsg { tile, image: Ok(img) }).is_err() {
                                return; // UI gone
                            }
                            wake();
                        }
                    }
                    Err(TileError::RateLimited { retry_after: hint }) => {
                        consecutive_429 += 1;
                        let backoff = cooldown_for(consecutive_429, hint);
                        cooldown_until = Some(Instant::now() + backoff);
                        tracing::warn!(tile = ?tile, ?backoff, "GIBS rate limit; backing off");
                    }
                    Err(TileError::Failed(problem)) => {
                        retry_after.insert(tile, Instant::now() + TILE_RETRY_DELAY);
                        tracing::debug!(tile = ?tile, "tile fetch failed: {problem}");
                    }
                }
            }
            ctl = rx_ctl.recv() => {
                let Some(Ctl::SetVisible(tiles)) = ctl else { return };
                wanted = dedup(tiles);
            }
        }
    }
}

async fn fetch_tile(
    fetcher: &api::TileFetcher,
    tile: TileId,
) -> (TileId, Result<ColorImage, TileError>) {
    let result = fetcher.tile(tile).await;
    (tile, result)
}

fn dedup(tiles: Vec<TileId>) -> Vec<TileId> {
    let mut seen = std::collections::HashSet::new();
    tiles.into_iter().filter(|t| seen.insert(*t)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tileset_matches_the_gibs_31_25m_matrix() {
        // The renderer and the fetch URL must agree on the matrix geometry.
        // These are the published `31.25m` EPSG:4326 values (docs/BASEMAP.md §2).
        assert_eq!(TILESET.matrix_size(0), (2, 1));
        assert_eq!(TILESET.matrix_size(11), (4096, 2048));
        // Level 0 is 180°-wide tiles at 512 px → 0.3515625°/px.
        assert!((TILESET.deg_per_px(0) - 0.3515625).abs() < 1e-9);
        // Level 8 (≈0.00137°/px) is the level under the app's zoom floor.
        assert!((TILESET.deg_per_px(8) - 0.001373291015625).abs() < 1e-12);
        assert_eq!(TILESET.max_level, 11);
    }

    #[test]
    fn tile_url_uses_row_before_column_and_31_25m() {
        // GIBS REST tile order is {z}/{row}/{col}, and the matrix set name is
        // `31.25m` — not `500m` or any other spelling. A row/col swap here
        // draws the world mirrored or shifted; this pins the order.
        let url = tile_url(TileId {
            level: 3,
            col: 7,
            row: 2,
        });
        assert_eq!(
            url,
            "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
ASTER_GDEM_Color_Shaded_Relief/default/default/31.25m/3/2/7.jpg"
        );
    }

    #[test]
    fn cooldown_doubles_and_caps_and_honors_retry_after() {
        assert_eq!(cooldown_for(1, None), Duration::from_secs(2));
        assert_eq!(cooldown_for(2, None), Duration::from_secs(4));
        assert_eq!(cooldown_for(3, None), Duration::from_secs(8));
        // Exponential doubling caps out rather than going silent forever.
        assert_eq!(cooldown_for(100, None), COOLDOWN_MAX);
        // A provider hint is honoured, and capped too.
        assert_eq!(
            cooldown_for(1, Some(Duration::from_secs(9))),
            Duration::from_secs(9)
        );
        assert_eq!(
            cooldown_for(1, Some(Duration::from_secs(999))),
            COOLDOWN_MAX
        );
    }

    #[test]
    fn dedup_keeps_first_occurrence_and_order() {
        let a = TileId {
            level: 1,
            col: 0,
            row: 0,
        };
        let b = TileId {
            level: 1,
            col: 1,
            row: 0,
        };
        assert_eq!(dedup(vec![a, b, a, b, a]), vec![a, b]);
    }
}
