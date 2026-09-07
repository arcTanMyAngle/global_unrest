//! EPSG:4326 tile math and the imagery compositing layer (docs/BASEMAP.md).
//!
//! A 4326 tile's footprint is a lon/lat rectangle, and the equirectangular
//! affine maps that rectangle to an axis-aligned screen rectangle exactly —
//! one textured quad per tile, no resampling, no warp. `TileLayer` is
//! therefore a bounded set of `epaint::Mesh`es (one per visible tile, capped)
//! rather than a `GeoMesh`: each mesh carries its own texture id, which
//! `GeoMesh` cannot express (it has no UVs and assumes one draw).
//!
//! This module is **network-free** and owns no textures. The desktop app
//! supplies the texture id lookup each frame (Phase 2 of the basemap fetches
//! and decodes them; Phase 1 loads them from a local directory). A tile with
//! no texture yet is simply not drawn, so a missing tile degrades to the
//! vector basemap underneath — the layer ordering in `MapView::show` is what
//! makes that a continuous map, never a hole.

use std::collections::HashMap;

use egui::epaint::{Mesh, Vertex};
use egui::{Color32, Pos2, TextureId};
use geo_utils::Affine;

use crate::{MeshCache, affine_key};

/// Most imagery tiles drawn in one frame. If the finest level selection would
/// exceed this, the layer drops to the next coarser level rather than skip
/// tiles, so the world is never partially covered by a rendering decision
/// (docs/BASEMAP.md §6).
pub const VISIBLE_TILE_CAP: usize = 48;

/// One tile in an EPSG:4326 matrix set. `col` counts east from the
/// antimeridian, `row` counts south from the north pole, `level` is 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub level: u8,
    pub col: u32,
    pub row: u32,
}

/// A visible tile plus the *actual* west edge it should be drawn at. `id.col`
/// is the wrapped column (so the antimeridian copy reuses the same texture);
/// `west` may lie outside `[-180, 180)` for a wrapped world copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleTile {
    pub id: TileId,
    pub west: f64,
}

/// A square EPSG:4326 tile matrix. Level 0 covers the world in
/// `level0_cols × level0_rows` tiles, each `tile_size` px; every level halves
/// the resolution (doubles the grid). All sizes and extents derive from those
/// three facts, so the math stays correct for whatever a provider's
/// GetCapabilities actually declares (docs/BASEMAP.md §1, §2). Values are
/// supplied by the caller; nothing here hard-codes a provider.
#[derive(Debug, Clone, Copy)]
pub struct TileMatrixSet {
    pub tile_size: u32,
    pub level0_cols: u32,
    pub level0_rows: u32,
    pub max_level: u8,
}

impl TileMatrixSet {
    pub const fn new(tile_size: u32, level0_cols: u32, level0_rows: u32, max_level: u8) -> Self {
        Self {
            tile_size,
            level0_cols,
            level0_rows,
            max_level,
        }
    }

    /// Columns and rows at `level`.
    pub fn matrix_size(self, level: u8) -> (u32, u32) {
        let k = 1u32 << level.min(self.max_level);
        (self.level0_cols * k, self.level0_rows * k)
    }

    /// Degrees of longitude per pixel at `level`.
    pub fn deg_per_px(self, level: u8) -> f64 {
        let (cols, _) = self.matrix_size(level);
        360.0 / (f64::from(cols) * f64::from(self.tile_size))
    }

    /// The finest level whose tile resolution is no finer than the viewport's
    /// (`deg_per_px >= viewport_deg_per_px`), so imagery never uploads more
    /// pixels than the screen can show. Falls back to level 0 when the
    /// viewport is zoomed out beyond the coarsest level, and to `max_level`
    /// when it out-resolves the ladder (the zoom cap keeps this from
    /// happening in practice).
    pub fn select_level(self, viewport_deg_per_px: f64) -> u8 {
        let mut best = 0;
        for level in 0..=self.max_level {
            if self.deg_per_px(level) >= viewport_deg_per_px {
                best = level;
            }
        }
        best
    }

    /// Tile column/row containing `(lon, lat)`. Longitude is wrapped into
    /// `[0, 360)`; latitude is clamped to `±90` so the poles never yield an
    /// out-of-range row.
    pub fn tile_index(self, lon: f64, lat: f64, level: u8) -> TileId {
        let (cols, rows) = self.matrix_size(level);
        let lon = (lon + 180.0).rem_euclid(360.0);
        let lat = lat.clamp(-90.0, 90.0);
        let col = ((lon / 360.0) * f64::from(cols)).floor() as u32;
        let row = (((90.0 - lat) / 180.0) * f64::from(rows)).floor() as u32;
        TileId {
            level,
            col: col.min(cols - 1),
            row: row.min(rows - 1),
        }
    }

    /// Lon/lat footprint of a tile: `(west, north, east, south)`. Rows never
    /// wrap, so this is canonical even for a wrapped column id.
    pub fn footprint(self, id: TileId) -> (f64, f64, f64, f64) {
        let (cols, rows) = self.matrix_size(id.level);
        let w = 360.0 / f64::from(cols);
        let h = 180.0 / f64::from(rows);
        let west = -180.0 + f64::from(id.col) * w;
        let north = 90.0 - f64::from(id.row) * h;
        (west, north, west + w, north - h)
    }
}

/// Inverse-affine one screen point to lon/lat. The affine's `c` is negative
/// (screen y grows downward), so this mirrors `geo_utils::MapViewport::unproject`.
fn aff_unproject(aff: &Affine, x: f32, y: f32) -> (f64, f64) {
    let lon = (f64::from(x) - aff.b) / aff.a;
    let lat = (f64::from(y) - aff.d) / aff.c;
    (lon, lat)
}

/// Tiles whose footprints intersect the visible lon/lat window, with column
/// wrapping so the antimeridian copies reuse the same texture. The west edge
/// carried per tile is the *instance* west edge, which may sit outside
/// `[-180, 180)` — the affine maps it to the correct screen position directly.
pub fn visible_tiles(
    set: TileMatrixSet,
    aff: &Affine,
    screen_w: f32,
    screen_h: f32,
    level: u8,
) -> Vec<VisibleTile> {
    let (lon0, lat0) = aff_unproject(aff, 0.0, 0.0);
    let (lon1, lat1) = aff_unproject(aff, screen_w, screen_h);

    // Shrink by an epsilon so a viewport whose edge lands exactly on ±180°
    // doesn't pull in a zero-width world copy (mirrors
    // `crate::visible_world_offsets`).
    const EPS: f64 = 1e-9;
    let west = lon0.min(lon1) + EPS;
    let east = lon0.max(lon1) - EPS;
    let south = lat0.min(lat1).clamp(-90.0, 90.0);
    let north = lat0.max(lat1).clamp(-90.0, 90.0);

    let (cols, rows) = set.matrix_size(level);
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let w = 360.0 / f64::from(cols);
    let h = 180.0 / f64::from(rows);

    let col0 = ((west + 180.0) / w).floor() as i64;
    let col1 = ((east + 180.0) / w).floor() as i64;
    let row0 = (((90.0 - north) / h).floor() as i64).clamp(0, rows as i64 - 1);
    let row1 = (((90.0 - south) / h).floor() as i64).clamp(0, rows as i64 - 1);

    let mut out = Vec::new();
    for row in row0.min(row1)..=row0.max(row1) {
        for col_inst in col0..=col1 {
            out.push(VisibleTile {
                id: TileId {
                    level,
                    col: col_inst.rem_euclid(cols as i64) as u32,
                    row: row as u32,
                },
                west: -180.0 + col_inst as f64 * w,
            });
        }
    }
    out
}

/// The level the tile layer should draw at: `select_level`, then walk coarser
/// until the visible count fits [`VISIBLE_TILE_CAP`].
pub fn select_level_for_viewport(
    set: TileMatrixSet,
    aff: &Affine,
    screen_w: f32,
    screen_h: f32,
) -> u8 {
    let viewport_deg_per_px = (1.0 / aff.a).abs();
    let mut level = set.select_level(viewport_deg_per_px);
    while level > 0 && visible_tiles(set, aff, screen_w, screen_h, level).len() > VISIBLE_TILE_CAP {
        level -= 1;
    }
    level
}

/// Build one quad mesh per visible tile that has a texture. A tile with no
/// texture yet is skipped — the vector basemap beneath it shows through.
fn build_quads(
    set: TileMatrixSet,
    tiles: &[VisibleTile],
    aff: &Affine,
    textures: &HashMap<TileId, TextureId>,
) -> Vec<Mesh> {
    let mut meshes = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let Some(&tex) = textures.get(&tile.id) else {
            continue;
        };
        let (cols, rows) = set.matrix_size(tile.id.level);
        let w = 360.0 / f64::from(cols);
        let h = 180.0 / f64::from(rows);
        let west = tile.west;
        let north = 90.0 - f64::from(tile.id.row) * h;
        let (x0, y0) = aff.apply(west, north);
        let (x1, y1) = aff.apply(west + w, north - h);

        // Texture v=0 is the top of the image (north); the north edge samples
        // v=0 and the south edge samples v=1.
        let v = |x: f32, y: f32, u: f32, v: f32| Vertex {
            pos: Pos2::new(x, y),
            uv: Pos2::new(u, v),
            color: Color32::WHITE,
        };
        let mut mesh = Mesh {
            vertices: vec![
                v(x0, y0, 0.0, 0.0), // top-left
                v(x1, y0, 1.0, 0.0), // top-right
                v(x1, y1, 1.0, 1.0), // bottom-right
                v(x0, y1, 0.0, 1.0), // bottom-left
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        };
        mesh.texture_id = tex;
        meshes.push(mesh);
    }
    meshes
}

/// The imagery layer. Owns no textures and makes no network calls: each frame
/// it computes the visible tile set (capped, level-selected, world-wrapped)
/// and draws a textured quad for every tile the caller has a texture for. The
/// caller reads [`TileLayer::visible`] to decide what to fetch/load — a
/// *replacement* list, never a queue, so a fast pan cancels stale work
/// (docs/BASEMAP.md §6).
pub struct TileLayer {
    pub tileset: TileMatrixSet,
    /// The visible tile set from the last paint — the app's fetch list.
    pub visible: Vec<VisibleTile>,
    cache: MeshCache,
}

impl TileLayer {
    pub fn new(tileset: TileMatrixSet) -> Self {
        Self {
            tileset,
            visible: Vec::new(),
            cache: MeshCache::default(),
        }
    }

    /// Paint the visible tiles and return how many quads were drawn. The
    /// caller paints the scrim only when this is non-zero, so an empty tile
    /// set (no textures yet, or the toggle off) never darkens the vector map.
    ///
    /// `texture_generation` is bumped by the caller whenever a texture is
    /// added or removed, so an arriving tile rebuilds the mesh set once
    /// rather than never (stale) or every frame (wasteful).
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        aff: &Affine,
        screen_w: f32,
        screen_h: f32,
        textures: &HashMap<TileId, TextureId>,
        texture_generation: u64,
    ) -> usize {
        let level = select_level_for_viewport(self.tileset, aff, screen_w, screen_h);
        self.visible = visible_tiles(self.tileset, aff, screen_w, screen_h, level);

        let mut key = affine_key(aff);
        key ^= u64::from(level) << 8;
        key ^= texture_generation.rotate_left(16);

        let tileset = self.tileset;
        let visible = &self.visible;
        let meshes = self
            .cache
            .get_or_build(key, || build_quads(tileset, visible, aff, textures));
        for mesh in meshes {
            painter.add(egui::Shape::mesh(mesh.clone()));
        }
        meshes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fabricated but self-consistent matrix set: 256 px tiles, level 0 =
    /// 2×1 (180° tiles), four levels. Easy to reason about by hand without
    /// asserting anything about a real provider's GetCapabilities.
    fn test_set() -> TileMatrixSet {
        TileMatrixSet::new(256, 2, 1, 4)
    }

    fn world_aff(center_lon: f64) -> Affine {
        let deg_per_px = 0.3; // 1200 px across 360°
        geo_utils::MapViewport {
            center_lon,
            center_lat: 0.0,
            deg_per_px,
            screen_w: 1200.0,
            screen_h: 600.0,
        }
        .affine()
    }

    #[test]
    fn matrix_size_doubles_each_level() {
        assert_eq!(test_set().matrix_size(0), (2, 1));
        assert_eq!(test_set().matrix_size(1), (4, 2));
        assert_eq!(test_set().matrix_size(2), (8, 4));
    }

    #[test]
    fn resolution_halves_each_level() {
        let r0 = test_set().deg_per_px(0);
        let r1 = test_set().deg_per_px(1);
        assert!((r1 - r0 / 2.0).abs() < 1e-12);
    }

    #[test]
    fn tile_index_places_a_known_point() {
        let id = test_set().tile_index(45.0, 45.0, 1);
        assert_eq!(
            id,
            TileId {
                level: 1,
                col: 2,
                row: 0
            }
        );
        let (w, n, e, s) = test_set().footprint(id);
        assert!((w..=e).contains(&45.0));
        assert!((s..=n).contains(&45.0));
    }

    #[test]
    fn tile_index_clamps_the_poles_and_wraps_longitude() {
        let north = test_set().tile_index(0.0, 90.0, 1);
        assert_eq!(north.row, 0);
        let south = test_set().tile_index(0.0, -90.0, 1);
        assert_eq!(south.row, 1);
        // 180° is the same column as -180° after wrapping.
        assert_eq!(test_set().tile_index(-180.0, 0.0, 1).col, 0);
        assert_eq!(test_set().tile_index(180.0, 0.0, 1).col, 0);
    }

    #[test]
    fn select_level_never_uploads_more_than_the_screen_shows() {
        // Viewport resolution 0.7°/px: level 0 (0.703°/px) is the finest
        // level whose pixels are no finer than the screen's.
        assert_eq!(test_set().select_level(0.7), 0);
        // Viewport 0.2°/px: level 1 (0.352°/px) is still coarser than the
        // screen, and level 2 (0.176°/px) is finer than it — so level 1.
        assert_eq!(test_set().select_level(0.2), 1);
        // Viewport 0.1°/px: level 2 (0.176°/px) is coarser than the screen,
        // and level 3 (0.088°/px) is finer — so level 2.
        assert_eq!(test_set().select_level(0.1), 2);
        // Zoomed far out: every level is finer than 5°/px → coarsest.
        assert_eq!(test_set().select_level(5.0), 0);
        // Zoomed past the ladder: finest available.
        assert_eq!(test_set().select_level(0.0001), 4);
    }

    #[test]
    fn visible_tiles_covers_a_full_world_with_exactly_two_tiles() {
        let tiles = visible_tiles(test_set(), &world_aff(0.0), 1200.0, 600.0, 0);
        assert_eq!(
            tiles.len(),
            2,
            "one full world is exactly two level-0 tiles"
        );
    }

    #[test]
    fn visible_tiles_wraps_the_antimeridian_reusing_textures() {
        // Centered on 180°: the view spans 0°..360°, i.e. one wrapped copy.
        let tiles = visible_tiles(test_set(), &world_aff(180.0), 1200.0, 600.0, 0);
        assert_eq!(tiles.len(), 2);
        // The two west edges are 0° and 180°, and their wrapped ids are
        // columns 1 and 0 respectively — the 180°..360° copy reuses column 0.
        let (w0, w1) = (
            tiles[0].west.min(tiles[1].west),
            tiles[0].west.max(tiles[1].west),
        );
        assert_eq!((w0, w1), (0.0, 180.0));
        let by_west = |west: f64| {
            tiles
                .iter()
                .find(|t| (t.west - west).abs() < 1e-9)
                .unwrap()
                .id
                .col
        };
        assert_eq!(by_west(0.0), 1);
        assert_eq!(by_west(180.0), 0);
    }

    #[test]
    fn select_level_for_viewport_respects_the_cap() {
        // A mid-zoom viewport where the finest selection would exceed the cap
        // must walk back to a coarser level. Fabricate a viewport narrow
        // enough that the finest level produces > 48 tiles.
        let deg_per_px = 0.02;
        let vp = geo_utils::MapViewport {
            center_lon: 0.0,
            center_lat: 0.0,
            deg_per_px,
            screen_w: 2000.0,
            screen_h: 1200.0,
        };
        let aff = vp.affine();
        let level = select_level_for_viewport(test_set(), &aff, 2000.0, 1200.0);
        let count = visible_tiles(test_set(), &aff, 2000.0, 1200.0, level).len();
        assert!(count <= VISIBLE_TILE_CAP, "{count} tiles at level {level}");
        assert!(level > 0, "the fine selection must have been backed off");
    }
}
