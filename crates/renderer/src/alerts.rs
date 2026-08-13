//! Weather-alert overlay: H3 cells carrying NOAA/NWS alerts, drawn as a
//! severity-tinted translucent fill inside a **dashed** outline
//! (docs/VISUALIZATION.md V3 item 8).
//!
//! NOAA alerts are `Disruption` events at Admin1 precision, so under the
//! precision rendering contract they can only shade regions — which meant they
//! were previously indistinguishable from unrest inside the general heatmap.
//! Weather must not read as unrest, hence a layer of its own with a cool hue
//! and a dashed outline no other layer uses.
//!
//! Perf contract: the fill is a cached [`GeoMesh`] like every other filled
//! layer. The outline is per-frame, and is bounded twice over — the caller
//! caps the cell count ([`ALERT_MAX_CELLS`]), and the dash length is derived
//! from each ring's *screen* perimeter so a ring emits exactly
//! [`DASHES_PER_RING`] dashes at any zoom. Passing a fixed dash length instead
//! would make a zoomed-in cell generate thousands of segments per frame.

use egui::{Painter, Pos2, Shape, Stroke};
use geo_utils::Affine;

use crate::{GeoMesh, MapStyle, MeshCache, affine_key, alert_color, visible_world_offsets};

/// Cap on alert cells drawn at once. NOAA's active-alert feed is US-only and
/// runs in the low hundreds of alerts, which roll up into far fewer cells;
/// this is a guardrail, not an expected limit.
pub const ALERT_MAX_CELLS: usize = 80;

/// Dash budget per cell ring. This is the *ceiling* at every zoom level, which
/// is what keeps the per-frame outline cost from growing as the user zooms in.
/// `Shape::dashed_line_many` splits any dash that straddles a corner, so the
/// real emitted count is up to this plus the ring's vertex count — still a
/// constant, which is the whole point.
const DASHES_PER_RING: f32 = 24.0;

/// Shortest dash period worth drawing. Below it the dashes blur into a solid
/// line, so a small ring gets fewer, longer dashes instead of the full budget
/// — it still reads as dashed, and it still costs less than the budget.
const MIN_DASH_PERIOD_PX: f32 = 3.0;

/// A ring smaller than this is a few pixels of fill; outlining it would add
/// noise, not information.
const MIN_RING_PERIMETER_PX: f32 = 12.0;

/// Fraction of each dash period that is drawn (the rest is the gap).
const DASH_DUTY: f32 = 0.55;

/// Outline width, deliberately heavier than `MapStyle::border_width` so the
/// alert boundary is not read as a country border.
const OUTLINE_WIDTH: f32 = 1.3;

pub struct AlertLayer {
    fill: GeoMesh,
    cache: MeshCache,
    /// Cell boundary rings in lon/lat, antimeridian-normalized by
    /// `geo_utils::cell_boundary_lonlat`.
    rings: Vec<Vec<[f32; 2]>>,
}

impl AlertLayer {
    pub fn empty() -> Self {
        Self {
            fill: GeoMesh::default(),
            cache: MeshCache::default(),
            rings: Vec::new(),
        }
    }

    /// Build from `(cell, severity 0..1)` pairs. Invalid cell ids are skipped
    /// (they were validated at ingest); input beyond [`ALERT_MAX_CELLS`] is
    /// dropped by the caller, not here.
    pub fn new(cells: &[(u64, f32)], style: &MapStyle) -> Self {
        let alpha = f32::from(style.alert_alpha) / 255.0;
        let mut fill = GeoMesh::default();
        let mut rings = Vec::new();
        for &(cell, severity) in cells {
            let Ok(ring) = geo_utils::cell_boundary_lonlat(cell) else {
                continue;
            };
            let Ok((clon, clat)) = geo_utils::cell_center_lonlat(cell) else {
                continue;
            };
            // Re-align the centroid with the normalized ring so fan triangles
            // don't span the world (same fix as `HeatmapLayer::build`).
            let mut clon = clon;
            if let Some(&(first_lon, _)) = ring.first() {
                while clon - first_lon > 180.0 {
                    clon -= 360.0;
                }
                while clon - first_lon < -180.0 {
                    clon += 360.0;
                }
            }
            let n = ring.len();
            let mut vertices: Vec<[f32; 2]> = Vec::with_capacity(n + 1);
            vertices.push([clon as f32, clat as f32]);
            vertices.extend(ring.iter().map(|&(lon, lat)| [lon as f32, lat as f32]));
            let mut indices: Vec<u32> = Vec::with_capacity(n * 3);
            for i in 0..n as u32 {
                indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n as u32]);
            }
            fill.push_polygon(
                &vertices,
                &indices,
                alert_color(severity).gamma_multiply(alpha),
            );
            rings.push(vertices[1..].to_vec());
        }
        Self {
            fill,
            cache: MeshCache::default(),
            rings,
        }
    }

    pub fn cell_count(&self) -> usize {
        self.rings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    pub fn paint(&mut self, painter: &Painter, aff: &Affine, screen_w: f32, style: &MapStyle) {
        if self.rings.is_empty() {
            return;
        }
        let offsets = visible_world_offsets(aff, screen_w);
        let mut key = affine_key(aff);
        key ^= offsets.len() as u64;
        let fill = &self.fill;
        let meshes = self.cache.get_or_build(key, || {
            offsets.iter().map(|&o| fill.to_mesh(aff, o)).collect()
        });
        for m in meshes {
            painter.add(Shape::mesh(m.clone()));
        }

        let stroke = Stroke::new(OUTLINE_WIDTH, style.alert_outline);
        let mut dashes = Vec::new();
        for &offset in &offsets {
            for ring in &self.rings {
                dashes.clear();
                dashed_ring(ring, aff, offset, stroke, &mut dashes);
                painter.extend(dashes.drain(..));
            }
        }
    }
}

/// Project one closed ring to screen space and emit a fixed number of dashes
/// along it. Returns without drawing for a ring that collapses to a point at
/// this zoom (a zero-length perimeter would make the dash length zero and
/// `dashed_line_many` iterate forever).
fn dashed_ring(
    ring: &[[f32; 2]],
    aff: &Affine,
    lon_offset: f64,
    stroke: Stroke,
    out: &mut Vec<Shape>,
) {
    let mut path: Vec<Pos2> = ring
        .iter()
        .map(|p| {
            let (x, y) = aff.apply(f64::from(p[0]) + lon_offset, f64::from(p[1]));
            Pos2::new(x, y)
        })
        .collect();
    let Some(&first) = path.first() else {
        return;
    };
    path.push(first); // close the ring: `dashed_line_many` walks an open path

    let perimeter: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    if perimeter < MIN_RING_PERIMETER_PX {
        return; // too small to outline legibly
    }
    let dashes = DASHES_PER_RING.min(perimeter / MIN_DASH_PERIOD_PX);
    let period = perimeter / dashes;
    Shape::dashed_line_many(
        &path,
        stroke,
        period * DASH_DUTY,
        period * (1.0 - DASH_DUTY),
        out,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_utils::MapViewport;

    fn cells() -> Vec<(u64, f32)> {
        vec![
            (geo_utils::cell_for_latlon(39.0, -95.0, 3).unwrap(), 1.0),
            (geo_utils::cell_for_latlon(-17.5, 179.9, 3).unwrap(), 0.25),
        ]
    }

    #[test]
    fn builds_a_fill_and_a_ring_per_valid_cell_and_skips_garbage() {
        let mut input = cells();
        input.push((0xdead_beef, 0.5));
        let layer = AlertLayer::new(&input, &MapStyle::default());
        assert_eq!(layer.cell_count(), 2);
        assert!(layer.fill.indices.len().is_multiple_of(3));
        // Same antimeridian property the heatmap fan has to hold.
        for tri in layer.fill.indices.chunks(3) {
            let lons: Vec<f32> = tri
                .iter()
                .map(|&i| layer.fill.positions[i as usize][0])
                .collect();
            let spread = lons.iter().cloned().fold(f32::MIN, f32::max)
                - lons.iter().cloned().fold(f32::MAX, f32::min);
            assert!(spread < 90.0, "triangle spans {spread}° of longitude");
        }
    }

    #[test]
    fn severity_drives_the_fill_tint() {
        let layer = AlertLayer::new(
            &[
                (geo_utils::cell_for_latlon(39.0, -95.0, 3).unwrap(), 0.0),
                (geo_utils::cell_for_latlon(45.0, -80.0, 3).unwrap(), 1.0),
            ],
            &MapStyle::default(),
        );
        let lo = layer.fill.colors[0];
        let hi = *layer.fill.colors.last().unwrap();
        assert!(hi.r() > lo.r() && hi.g() > lo.g() && hi.b() > lo.b());
    }

    /// The perf guardrail this layer exists under: per-frame outline cost must
    /// not grow when the user zooms in. A fixed dash *length* would let a
    /// zoomed-in cell emit thousands of segments a frame.
    #[test]
    fn dash_count_per_ring_stays_within_budget_at_every_zoom() {
        let layer = AlertLayer::new(&cells()[..1], &MapStyle::default());
        let ring = &layer.rings[0];
        let stroke = Stroke::new(1.0, egui::Color32::WHITE);
        let count_at = |deg_per_px| {
            let vp = MapViewport {
                center_lon: -95.0,
                center_lat: 39.0,
                deg_per_px,
                screen_w: 1200.0,
                screen_h: 700.0,
            };
            let mut out = Vec::new();
            dashed_ring(ring, &vp.affine(), 0.0, stroke, &mut out);
            out.len()
        };
        // Two zoom levels four decades apart, plus world zoom in between.
        let counts: Vec<usize> = [0.225, 0.02, 0.002, 0.000_02].map(count_at).to_vec();
        // Budget plus one split per corner (a dash straddling a vertex becomes
        // two segments) — the bound epaint's dasher can actually hit.
        let budget = DASHES_PER_RING as usize + ring.len();
        assert!(
            counts.iter().all(|&c| c <= budget),
            "over the per-ring budget of {budget}: {counts:?}"
        );
        assert!(counts.iter().all(|&c| c > 0), "no dashes drawn: {counts:?}");
        // Once the ring clears the budget's minimum period, the dash count is
        // pinned — the outline looks identical however far in the user zooms.
        assert_eq!(counts[1], counts[2], "{counts:?}");
        assert_eq!(counts[2], counts[3], "{counts:?}");
    }

    #[test]
    fn a_ring_that_collapses_to_a_point_draws_nothing() {
        let layer = AlertLayer::new(&cells()[..1], &MapStyle::default());
        // Absurdly zoomed out: the whole cell is sub-pixel.
        let aff = Affine {
            a: 1e-6,
            b: 0.0,
            c: -1e-6,
            d: 0.0,
        };
        let mut out = Vec::new();
        dashed_ring(
            &layer.rings[0],
            &aff,
            0.0,
            Stroke::new(1.0, egui::Color32::WHITE),
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn empty_layer_paints_nothing() {
        let layer = AlertLayer::empty();
        assert!(layer.is_empty());
        assert_eq!(layer.cell_count(), 0);
    }
}
