//! Map rendering layers for egui: cached-mesh basemap, heatmap, markers.
//!
//! This is a **layer library**, not a wgpu engine. Geometry is tessellated
//! once into lon/lat space ([`GeoMesh`]) and converted to screen-space
//! `epaint::Mesh` with a cheap affine transform whenever the viewport
//! changes — equirectangular projection is affine in lon/lat, so pan/zoom
//! costs one mul-add per vertex and nothing re-tessellates per frame.
//! Country border strokes are the one deliberate exception (thin polylines,
//! cheap for epaint).

pub mod basemap;
pub mod heatmap;
pub mod markers;

pub use basemap::BasemapLayer;
pub use heatmap::HeatmapLayer;
pub use markers::{MarkerInput, MarkerLayer};

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, Pos2};
use geo_utils::Affine;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("geojson: {0}")]
    Geojson(String),
    #[error("geometry: {0}")]
    Geometry(String),
}

/// Dark map style. Heat ramp is a 4-stop sequential thermal ramp (dark →
/// amber → orange → near-white) chosen to stay legible on the dark basemap
/// and to avoid red/green ambiguity.
#[derive(Debug, Clone)]
pub struct MapStyle {
    pub background: Color32,
    pub land_fill: Color32,
    pub border: Color32,
    pub border_width: f32,
    pub graticule: Color32,
    pub heat_alpha: u8,
    pub marker_protest: Color32,
    pub marker_conflict: Color32,
    pub marker_disruption: Color32,
    pub marker_other: Color32,
    pub marker_attention: Color32,
}

impl Default for MapStyle {
    fn default() -> Self {
        Self {
            background: Color32::from_rgb(11, 14, 20),
            land_fill: Color32::from_rgb(30, 36, 46),
            border: Color32::from_rgb(58, 68, 82),
            border_width: 0.6,
            graticule: Color32::from_rgb(24, 29, 38),
            heat_alpha: 110,
            marker_protest: Color32::from_rgb(255, 196, 61),
            marker_conflict: Color32::from_rgb(255, 92, 92),
            marker_disruption: Color32::from_rgb(96, 176, 255),
            marker_other: Color32::from_rgb(158, 158, 170),
            marker_attention: Color32::from_rgb(186, 130, 255),
        }
    }
}

impl MapStyle {
    pub fn marker_color(&self, kind: core_types::EventKind) -> Color32 {
        use core_types::EventKind::*;
        match kind {
            Protest => self.marker_protest,
            Conflict => self.marker_conflict,
            Disruption => self.marker_disruption,
            NewsAttention => self.marker_attention,
            Other => self.marker_other,
        }
    }
}

/// Sequential thermal ramp for heat intensity t ∈ [0, 1].
pub fn heat_color(t: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 4] = [
        (0.0, [40, 32, 72]),    // deep indigo
        (0.35, [140, 62, 92]),  // plum
        (0.7, [235, 132, 52]),  // orange
        (1.0, [252, 232, 164]), // pale amber
    ];
    let t = t.clamp(0.0, 1.0);
    for pair in STOPS.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * f) as u8;
            return Color32::from_rgb(lerp(c0[0], c1[0]), lerp(c0[1], c1[1]), lerp(c0[2], c1[2]));
        }
    }
    Color32::from_rgb(252, 232, 164)
}

/// Stable key for an affine transform, used to invalidate cached meshes.
pub fn affine_key(aff: &Affine) -> u64 {
    let mut h = core_types::fnv1a64(&aff.a.to_bits().to_le_bytes());
    for v in [aff.b, aff.c, aff.d] {
        h ^= core_types::fnv1a64(&v.to_bits().to_le_bytes());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// World-copy longitude offsets (in degrees) whose 360°-wide world strip
/// intersects the visible lon range. Lets panning across ±180° show the
/// wrapped copy. Clamped to ±2 copies as a safety valve.
pub fn visible_world_offsets(aff: &Affine, screen_w: f32) -> Vec<f64> {
    let lon_at = |x: f64| (x - aff.b) / aff.a;
    let (l0, l1) = (lon_at(0.0), lon_at(f64::from(screen_w)));
    // Shrink by an epsilon so a viewport whose edge lands exactly on ±180°
    // doesn't pull in a zero-width world copy.
    const EPS: f64 = 1e-9;
    let (lon_min, lon_max) = (l0.min(l1) + EPS, l0.max(l1) - EPS);
    let k_min = (((lon_min + 180.0) / 360.0).floor() as i32).clamp(-2, 2);
    let k_max = (((lon_max.max(lon_min) + 180.0) / 360.0).floor() as i32).clamp(-2, 2);
    (k_min..=k_max).map(|k| f64::from(k) * 360.0).collect()
}

/// Geometry tessellated once in lon/lat space. Producing a screen-space
/// mesh is one affine mul-add per vertex.
#[derive(Default, Clone)]
pub struct GeoMesh {
    /// (lon, lat) per vertex.
    pub positions: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Per-vertex color.
    pub colors: Vec<Color32>,
}

impl GeoMesh {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Append a triangulated polygon with a uniform color.
    pub fn push_polygon(&mut self, vertices: &[[f32; 2]], indices: &[u32], color: Color32) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(vertices);
        self.colors
            .extend(std::iter::repeat_n(color, vertices.len()));
        self.indices.extend(indices.iter().map(|i| i + base));
    }

    /// Build the screen-space mesh for one world-copy offset (degrees).
    pub fn to_mesh(&self, aff: &Affine, lon_offset: f64) -> Mesh {
        let vertices = self
            .positions
            .iter()
            .zip(&self.colors)
            .map(|(p, &color)| {
                let (x, y) = aff.apply(f64::from(p[0]) + lon_offset, f64::from(p[1]));
                Vertex {
                    pos: Pos2::new(x, y),
                    uv: WHITE_UV,
                    color,
                }
            })
            .collect();
        Mesh {
            indices: self.indices.clone(),
            vertices,
            ..Default::default()
        }
    }
}

/// Per-layer cache of screen-space meshes, invalidated when the affine (or
/// the layer's own data version) changes.
pub struct MeshCache {
    key: u64,
    meshes: Vec<Mesh>,
}

impl Default for MeshCache {
    fn default() -> Self {
        Self {
            key: u64::MAX,
            meshes: Vec::new(),
        }
    }
}

impl MeshCache {
    /// Rebuild via `build` when `key` changed; otherwise reuse.
    pub fn get_or_build(&mut self, key: u64, build: impl FnOnce() -> Vec<Mesh>) -> &[Mesh] {
        if self.key != key {
            self.meshes = build();
            self.key = key;
        }
        &self.meshes
    }

    pub fn invalidate(&mut self) {
        self.key = u64::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heat_color_endpoints_and_monotone_red() {
        let lo = heat_color(0.0);
        let hi = heat_color(1.0);
        assert_eq!((lo.r(), lo.g(), lo.b()), (40, 32, 72));
        assert_eq!((hi.r(), hi.g(), hi.b()), (252, 232, 164));
        // Red channel rises monotonically along the ramp.
        let mut last = 0u8;
        for i in 0..=10 {
            let c = heat_color(i as f32 / 10.0);
            assert!(c.r() >= last, "red channel must not decrease");
            last = c.r();
        }
    }

    #[test]
    fn geo_mesh_offsets_indices_when_appending() {
        let mut gm = GeoMesh::default();
        let quad = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let idx = [0, 1, 2, 0, 2, 3];
        gm.push_polygon(&quad, &idx, Color32::RED);
        gm.push_polygon(&quad, &idx, Color32::GREEN);
        assert_eq!(gm.vertex_count(), 8);
        assert_eq!(gm.indices.len(), 12);
        assert_eq!(gm.indices[6], 4, "second polygon indices must be offset");
        assert!(gm.indices.iter().all(|&i| (i as usize) < gm.vertex_count()));
    }

    #[test]
    fn to_mesh_applies_affine() {
        let mut gm = GeoMesh::default();
        gm.push_polygon(
            &[[10.0, 20.0], [11.0, 20.0], [11.0, 21.0]],
            &[0, 1, 2],
            Color32::WHITE,
        );
        // x = 2*lon + 100; y = -2*lat + 500.
        let aff = Affine {
            a: 2.0,
            b: 100.0,
            c: -2.0,
            d: 500.0,
        };
        let mesh = gm.to_mesh(&aff, 0.0);
        assert_eq!(mesh.vertices[0].pos, Pos2::new(120.0, 460.0));
        // World copy shifted +360°: x moves by 2*360 = 720 px.
        let wrapped = gm.to_mesh(&aff, 360.0);
        assert_eq!(wrapped.vertices[0].pos, Pos2::new(840.0, 460.0));
    }

    #[test]
    fn world_offsets_cover_antimeridian_view() {
        // 1000 px screen, 0.1°/px, centered at 179° → lon range 129°..229°.
        let vp = geo_utils::MapViewport {
            center_lon: 179.0,
            center_lat: 0.0,
            deg_per_px: 0.1,
            screen_w: 1000.0,
            screen_h: 600.0,
        };
        let offsets = visible_world_offsets(&vp.affine(), vp.screen_w);
        // Base world plus the +360 copy (for lons past 180).
        assert_eq!(offsets, vec![0.0, 360.0]);

        let centered = geo_utils::MapViewport::fit_world(1000.0, 600.0);
        assert_eq!(
            visible_world_offsets(&centered.affine(), centered.screen_w),
            vec![0.0]
        );
    }

    #[test]
    fn mesh_cache_rebuilds_only_on_key_change() {
        let mut cache = MeshCache::default();
        let mut builds = 0;
        for key in [1u64, 1, 1, 2] {
            cache.get_or_build(key, || {
                builds += 1;
                vec![Mesh::default()]
            });
        }
        assert_eq!(builds, 2);
    }
}
