//! Event marker layer: batched screen-space quads (diamonds), one mesh for
//! all points — never thousands of individual `Shape::Circle`s.
//!
//! Callers must only feed City/Exact-precision records (the precision
//! rendering contract is enforced upstream in the storage query).

use core_types::EventKind;
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Painter, Pos2, Shape};
use geo_utils::Affine;

use crate::{MapStyle, MeshCache, affine_key, visible_world_offsets};

#[derive(Debug, Clone)]
pub struct MarkerInput {
    pub lon: f64,
    pub lat: f64,
    pub kind: EventKind,
    /// 0..1; scales marker size a little (e.g. from article count).
    pub weight: f32,
    /// Index back into the caller's point list (for hover/click lookups).
    pub source_index: usize,
}

pub struct MarkerLayer {
    points: Vec<MarkerInput>,
    cache: MeshCache,
}

const BASE_HALF_PX: f32 = 2.5;
const MAX_EXTRA_PX: f32 = 3.0;

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

/// One diamond (rotated quad) per point, batched into a single mesh.
fn build_mesh(points: &[MarkerInput], aff: &Affine, lon_offset: f64, style: &MapStyle) -> Mesh {
    let mut mesh = Mesh::default();
    mesh.vertices.reserve(points.len() * 4);
    mesh.indices.reserve(points.len() * 6);
    for p in points {
        let (x, y) = aff.apply(p.lon + lon_offset, p.lat);
        let half = BASE_HALF_PX + MAX_EXTRA_PX * p.weight.clamp(0.0, 1.0);
        let color = style.marker_color(p.kind);
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend_from_slice(&[
            Vertex {
                pos: Pos2::new(x, y - half),
                uv: WHITE_UV,
                color,
            },
            Vertex {
                pos: Pos2::new(x + half, y),
                uv: WHITE_UV,
                color,
            },
            Vertex {
                pos: Pos2::new(x, y + half),
                uv: WHITE_UV,
                color,
            },
            Vertex {
                pos: Pos2::new(x - half, y),
                uv: WHITE_UV,
                color,
            },
        ]);
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
                source_index: 0,
            },
            MarkerInput {
                lon: 36.82,
                lat: -1.29,
                kind: EventKind::Conflict,
                weight: 1.0,
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
                source_index: i,
            })
            .collect();
        let vp = MapViewport::fit_world(1600.0, 900.0);
        let start = std::time::Instant::now();
        let mesh = build_mesh(&points, &vp.affine(), 0.0, &MapStyle::default());
        let elapsed = start.elapsed();
        assert_eq!(mesh.vertices.len(), 40_000);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "10k-point mesh build took {elapsed:?} (budget 100ms)"
        );
    }
}
