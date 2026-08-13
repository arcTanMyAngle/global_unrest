//! Event marker layer: batched screen-space convex polygons, one mesh for
//! all points — never thousands of individual `Shape::Circle`s.
//!
//! Callers must only feed City/Exact-precision records (the precision
//! rendering contract is enforced upstream in the storage query).
//!
//! Color encodes [`EventKind`], shape encodes the reporting source — see
//! [`crate::MarkerGlyph`].

use core_types::EventKind;
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Painter, Pos2, Shape};
use geo_utils::Affine;

use crate::{MapStyle, MarkerGlyph, MeshCache, affine_key, visible_world_offsets};

#[derive(Debug, Clone)]
pub struct MarkerInput {
    pub lon: f64,
    pub lat: f64,
    pub kind: EventKind,
    /// 0..1; scales marker size a little (e.g. from article count). Used as
    /// the sizing driver only when `severity` is `None`.
    pub weight: f32,
    /// 0..1 when the source provides one; takes priority over `weight` for
    /// sizing so e.g. a high-fatality ACLED battle reads larger than a
    /// 0-fatality protest. `None` falls back to `weight`.
    pub severity: Option<f32>,
    /// Opacity multiplier in [0, 1] — recency fade during playback (1.0 =
    /// full opacity, the value outside playback so pausing shows full
    /// detail).
    pub alpha: f32,
    /// Outline shape, derived from the record's live source. The second
    /// encoding channel alongside the kind-colored fill.
    pub glyph: MarkerGlyph,
    /// Index back into the caller's point list (for hover/click lookups).
    pub source_index: usize,
}

pub struct MarkerLayer {
    points: Vec<MarkerInput>,
    cache: MeshCache,
}

const BASE_HALF_PX: f32 = 2.5;
const MAX_EXTRA_PX: f32 = 3.0;

/// Screen half-extent for a marker whose sizing driver (severity, else
/// weight) is `size_t`. Public so the legend can draw its severity ramp at the
/// exact sizes the map uses instead of guessing at them.
pub fn marker_half_px(size_t: f32) -> f32 {
    BASE_HALF_PX + MAX_EXTRA_PX * size_t.clamp(0.0, 1.0)
}

impl MarkerLayer {
    pub fn new(points: Vec<MarkerInput>) -> Self {
        Self {
            points,
            cache: MeshCache::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn paint(&mut self, painter: &Painter, aff: &Affine, screen_w: f32, style: &MapStyle) {
        if self.points.is_empty() {
            return;
        }
        let offsets = visible_world_offsets(aff, screen_w);
        let mut key = affine_key(aff);
        key ^= offsets.len() as u64;
        let points = &self.points;
        let meshes = self.cache.get_or_build(key, || {
            offsets
                .iter()
                .map(|&off| build_mesh(points, aff, off, style))
                .collect()
        });
        for mesh in meshes {
            painter.add(Shape::mesh(mesh.clone()));
        }
    }

    /// Nearest marker within `radius_px` of a screen position, if any.
    /// Linear scan — fine for the ≤100k point cap, and only runs on
    /// hover/click, not per vertex per frame.
    pub fn hit_test(
        &self,
        aff: &Affine,
        screen_w: f32,
        pos: Pos2,
        radius_px: f32,
    ) -> Option<&MarkerInput> {
        let mut best: Option<(f32, &MarkerInput)> = None;
        for offset in visible_world_offsets(aff, screen_w) {
            for p in &self.points {
                let (x, y) = aff.apply(p.lon + offset, p.lat);
                let d2 = (x - pos.x).powi(2) + (y - pos.y).powi(2);
                if d2 <= radius_px * radius_px && best.is_none_or(|(bd2, _)| d2 < bd2) {
                    best = Some((d2, p));
                }
            }
        }
        best.map(|(_, p)| p)
    }
}

/// One convex glyph polygon per point, fan-triangulated from its first corner
/// and batched into a single mesh.
fn build_mesh(points: &[MarkerInput], aff: &Affine, lon_offset: f64, style: &MapStyle) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.vertices.reserve(points.len() * 4);
    mesh.indices.reserve(points.len() * 6);
    for p in points {
        let (x, y) = aff.apply(p.lon + lon_offset, p.lat);
        let half = marker_half_px(p.severity.unwrap_or(p.weight));
        let color = style.marker_color(p.kind).gamma_multiply(p.alpha);
        let corners = p.glyph.unit_corners();
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend(corners.iter().map(|c| Vertex {
            pos: Pos2::new(x + c[0] * half, y + c[1] * half),
            uv: WHITE_UV,
            color,
        }));
        for i in 1..corners.len() as u32 - 1 {
            mesh.indices
                .extend_from_slice(&[base, base + i, base + i + 1]);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_utils::MapViewport;

    fn layer() -> MarkerLayer {
        MarkerLayer::new(vec![
            MarkerInput {
                lon: 2.35,
                lat: 48.85,
                kind: EventKind::Protest,
                weight: 0.5,
                severity: None,
                alpha: 1.0,
                glyph: MarkerGlyph::Diamond,
                source_index: 0,
            },
            MarkerInput {
                lon: 36.82,
                lat: -1.29,
                kind: EventKind::Conflict,
                weight: 1.0,
                severity: None,
                alpha: 1.0,
                glyph: MarkerGlyph::Diamond,
                source_index: 1,
            },
        ])
    }

    #[test]
    fn builds_one_quad_per_point() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let mesh = build_mesh(&layer().points, &vp.affine(), 0.0, &MapStyle::default());
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.indices.len(), 12);
    }

    /// A 3-corner glyph must fan into exactly one triangle — the loop bound
    /// off by one here would either drop the glyph or index out of the mesh.
    #[test]
    fn triangle_glyphs_emit_one_triangle_and_stay_in_bounds() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let points = vec![
            MarkerInput {
                glyph: MarkerGlyph::TriangleUp,
                ..layer().points[0].clone()
            },
            MarkerInput {
                glyph: MarkerGlyph::TriangleDown,
                ..layer().points[1].clone()
            },
        ];
        let mesh = build_mesh(&points, &vp.affine(), 0.0, &MapStyle::default());
        assert_eq!(mesh.vertices.len(), 6);
        assert_eq!(mesh.indices.len(), 6);
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len())
        );
    }

    /// Two sources whose markers share a kind (both chatter feeds are
    /// `NewsAttention`) must still differ geometrically — that difference is
    /// the only thing a screenshot can use to tell them apart.
    #[test]
    fn different_glyphs_produce_different_geometry_at_the_same_point() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let style = MapStyle::default();
        let at = |glyph| {
            vec![MarkerInput {
                lon: 0.0,
                lat: 0.0,
                kind: EventKind::NewsAttention,
                weight: 0.5,
                severity: None,
                alpha: 1.0,
                glyph,
                source_index: 0,
            }]
        };
        let up = build_mesh(&at(MarkerGlyph::TriangleUp), &vp.affine(), 0.0, &style);
        let down = build_mesh(&at(MarkerGlyph::TriangleDown), &vp.affine(), 0.0, &style);
        let ys = |m: &Mesh| m.vertices.iter().map(|v| v.pos.y).collect::<Vec<_>>();
        assert_ne!(ys(&up), ys(&down));
        // ...while the color channel still says only "news attention".
        assert_eq!(up.vertices[0].color, down.vertices[0].color);
    }

    #[test]
    fn severity_overrides_weight_for_sizing_when_present() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let aff = vp.affine();
        let style = MapStyle::default();
        let low_weight_high_severity = vec![MarkerInput {
            lon: 0.0,
            lat: 0.0,
            kind: EventKind::Conflict,
            weight: 0.0, // would be near-base size without severity
            severity: Some(1.0),
            alpha: 1.0,
            glyph: MarkerGlyph::Diamond,
            source_index: 0,
        }];
        let base_size_no_severity = vec![MarkerInput {
            lon: 0.0,
            lat: 0.0,
            kind: EventKind::Conflict,
            weight: 0.0,
            severity: None,
            alpha: 1.0,
            glyph: MarkerGlyph::Diamond,
            source_index: 0,
        }];
        let big = build_mesh(&low_weight_high_severity, &aff, 0.0, &style);
        let small = build_mesh(&base_size_no_severity, &aff, 0.0, &style);
        // Half-extent is (vertex.x - center.x) on the +x vertex (index 1).
        let half_big = big.vertices[1].pos.x - big.vertices[3].pos.x;
        let half_small = small.vertices[1].pos.x - small.vertices[3].pos.x;
        assert!(
            half_big > half_small,
            "severity 1.0 must render larger than weight 0.0 with no severity"
        );
    }

    #[test]
    fn alpha_fades_marker_opacity() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let aff = vp.affine();
        let style = MapStyle::default();
        let faded = vec![MarkerInput {
            lon: 0.0,
            lat: 0.0,
            kind: EventKind::Conflict,
            weight: 0.5,
            severity: None,
            alpha: 0.35,
            glyph: MarkerGlyph::Diamond,
            source_index: 0,
        }];
        let full = vec![MarkerInput {
            alpha: 1.0,
            ..faded[0].clone()
        }];
        let faded_mesh = build_mesh(&faded, &aff, 0.0, &style);
        let full_mesh = build_mesh(&full, &aff, 0.0, &style);
        assert!(
            faded_mesh.vertices[0].color.a() < full_mesh.vertices[0].color.a(),
            "a faded marker must be more transparent than a full-opacity one"
        );
    }

    #[test]
    fn hit_test_finds_nearest_and_respects_radius() {
        let vp = MapViewport::fit_world(1000.0, 500.0);
        let aff = vp.affine();
        let l = layer();
        let (x, y) = aff.apply(2.35, 48.85);
        let hit = l
            .hit_test(&aff, 1000.0, Pos2::new(x + 2.0, y), 6.0)
            .unwrap();
        assert_eq!(hit.source_index, 0);
        assert!(
            l.hit_test(&aff, 1000.0, Pos2::new(x + 50.0, y), 6.0)
                .is_none()
        );
    }

    #[test]
    fn perf_smoke_mesh_build_under_budget_for_10k_points() {
        // M1 acceptance: cached-mesh rebuild for 10k points must be cheap
        // (it happens on viewport change, not per frame). Generous budget
        // to avoid CI flakes; catches accidental per-point pathologies.
        let points: Vec<MarkerInput> = (0..10_000)
            .map(|i| MarkerInput {
                lon: (i % 360) as f64 - 180.0,
                lat: ((i * 7) % 170) as f64 - 85.0,
                kind: EventKind::Protest,
                weight: 0.5,
                severity: None,
                alpha: 1.0,
                glyph: MarkerGlyph::ALL[i % MarkerGlyph::ALL.len()],
                source_index: i,
            })
            .collect();
        let vp = MapViewport::fit_world(1600.0, 900.0);
        let start = std::time::Instant::now();
        let mesh = build_mesh(&points, &vp.affine(), 0.0, &MapStyle::default());
        let elapsed = start.elapsed();
        // 2500 points per glyph; the two quads contribute 4 vertices each and
        // the two triangles 3, so 2500 * (4 + 4 + 3 + 3).
        assert_eq!(mesh.vertices.len(), 35_000);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "10k-point mesh build took {elapsed:?} (budget 100ms)"
        );
    }
}
