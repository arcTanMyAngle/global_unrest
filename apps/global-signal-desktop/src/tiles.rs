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

use egui::{ColorImage, TextureHandle, TextureId};
use renderer::{TileId, TileMatrixSet};
use tokio::sync::mpsc as tokio_mpsc;

/// NASA GIBS `BlueMarble_ShadedRelief_Bathymetry` in the `500m` EPSG:4326
/// matrix set: 512 px tiles, level 0 = 2×1 (180° tiles), halving per level,
/// max level 7. Static, undated shaded relief with bathymetry — deliberately
/// not a dated true-colour layer, so the imagery cannot be read as a
/// photograph *of* the event (docs/BASEMAP.md §2 honesty decision). The
/// renderer's tile math is parameterized by this same value, so the fetch URL
/// and the drawn layer cannot drift apart.
pub const TILESET: TileMatrixSet = TileMatrixSet::new(512, 2, 1, 7);

/// Cap on resident decoded textures: 96 × 1 MiB (512² RGBA) ≈ 96 MiB of VRAM
/// (docs/BASEMAP.md §4).
pub const RESIDENT_TEXTURE_CAP: usize = 96;

/// Texture uploads allowed in one frame, so a burst of tile arrivals cannot
/// spike frame time (docs/BASEMAP.md §4).
pub const UPLOADS_PER_FRAME: usize = 4;

/// How many tiles the worker fetches at once (docs/BASEMAP.md §6).
const CONCURRENT_FETCHES: usize = 2;

/// GIBS WMTS REST base for the static shaded-relief layer in the `500m`
/// matrix set. Tile order is `{z}/{row}/{col}` (row before column), the
/// documented GIBS REST pattern (docs/BASEMAP.md §2).
const GIBS_BASE: &str = "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
BlueMarble_ShadedRelief_Bathymetry/default/default/500m";

/// The REST URL for one tile: `{base}/{z}/{row}/{col}.jpg`.
fn tile_url(id: TileId) -> String {
    format!("{GIBS_BASE}/{}/{}/{}.jpg", id.level, id.row, id.col)
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
        /// Fetch and decode one tile. `Err` carries a user-safe, key-free
        /// reason (never the URL of a failed request beyond the tile id).
        pub async fn tile(&self, id: TileId) -> Result<ColorImage, String> {
            let url = tile_url(id);
            let resp = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
            decode(&bytes)
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
        pub async fn tile(&self, _: TileId) -> Result<ColorImage, String> {
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

    // A boxed fetch. Local (`!Send`) because the worker runs on a
    // current-thread runtime and the futures borrow the shared client — the
    // same shape [`crate::media`] uses for its three provider legs.
    type Fetch<'a> = Pin<Box<dyn Future<Output = (TileId, Result<ColorImage, String>)> + 'a>>;

    let mut wanted: Vec<TileId> = Vec::new();
    let mut in_flight: FuturesUnordered<Fetch<'_>> = FuturesUnordered::new();
    let mut in_flight_ids: HashSet<TileId> = HashSet::new();

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

        // Top up to the concurrency cap from the latest wanted set. A tile
        // already in flight is never re-requested.
        while in_flight_ids.len() < CONCURRENT_FETCHES {
            let Some(next) = wanted.iter().copied().find(|t| !in_flight_ids.contains(t)) else {
                break;
            };
            in_flight_ids.insert(next);
            in_flight.push(Box::pin(fetch_tile(&fetcher, next)));
        }

        tokio::select! {
            biased;
            done = in_flight.next(), if !in_flight.is_empty() => {
                let Some((tile, result)) = done else {
                    unreachable!("guarded by !in_flight.is_empty()");
                };
                in_flight_ids.remove(&tile);
                // A result for a tile that has since left the wanted set is
                // stale — drop it rather than painting a place the user has
                // already panned away from.
                if wanted.contains(&tile) && tx.send(TileMsg { tile, image: result }).is_err() {
                    return; // UI gone
                }
                // `wake` only on the kept path so a dropped stale tile does
                // not force a pointless repaint.
                if wanted.contains(&tile) {
                    wake();
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
) -> (TileId, Result<ColorImage, String>) {
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
    fn tileset_matches_the_gibs_500m_matrix() {
        // The renderer and the fetch URL must agree on the matrix geometry.
        // These are the published `500m` EPSG:4326 values (docs/BASEMAP.md §2).
        assert_eq!(TILESET.matrix_size(0), (2, 1));
        assert_eq!(TILESET.matrix_size(7), (256, 128));
        // Level 0 is 180°-wide tiles at 512 px → 0.3515625°/px.
        assert!((TILESET.deg_per_px(0) - 0.3515625).abs() < 1e-9);
        // The finest level stays coarser than the app's zoom floor is not
        // asserted here (that is a renderer/geo-utils constant); instead pin
        // that max level is 7, matching the matrix set the URL names.
        assert_eq!(TILESET.max_level, 7);
    }

    #[test]
    fn tile_url_uses_row_before_column_and_500m() {
        // GIBS REST tile order is {z}/{row}/{col}, and the matrix set name is
        // `500m` — not `EPSG4326_250m` or any other spelling. A row/col swap
        // here draws the world mirrored or shifted; this pins the order.
        let url = tile_url(TileId {
            level: 3,
            col: 7,
            row: 2,
        });
        assert_eq!(
            url,
            "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
BlueMarble_ShadedRelief_Bathymetry/default/default/500m/3/2/7.jpg"
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
