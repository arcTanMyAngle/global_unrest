//! Heatmap layer: H3 cells with normalized intensities → translucent
//! fan-triangulated cell polygons (cached GeoMesh).

use egui::{Painter, Shape};
use geo_utils::Affine;

use crate::{GeoMesh, MapStyle, MeshCache, affine_key, heat_color, visible_world_offsets};

pub struct HeatmapLayer {
    mesh: GeoMesh,
    cache: MeshCache,
    cells: usize,
}

impl HeatmapLayer {
    pub fn empty() -> Self {
        Self {
            mesh: GeoMesh::default(),
            cache: MeshCache::default(),
            cells: 0,
        }
    }

    /// Build from (cell, intensity 0..1) pairs. Cells whose boundary was
    /// antimeridian-normalized beyond ±180° are handled by the world-copy
    /// offsets at paint time, so each cell is tessellated exactly once.
    /// Invalid cell ids are skipped (they were validated at ingest).
    pub fn from_cells(cells: &[(u64, f32)], style: &MapStyle) -> Self {
        let mut mesh = GeoMesh::default();
        let mut built = 0usize;
        for &(cell, intensity) in cells {
            let Ok(ring) = geo_utils::cell_boundary_lonlat(cell) else {
                continue;
            };
            let Ok((clon, clat)) = geo_utils::cell_center_lonlat(cell) else {
                continue;
            };
            // The centroid comes back in [-180, 180]; re-align it with the
            // normalized ring so fan triangles don't span the world.
            let mut clon = clon;
            if let Some(&(first_lon, _)) = ring.first() {
                while clon - first_lon > 180.0 {
                    clon -= 360.0;
                }
                while clon - first_lon < -180.0 {
                    clon += 360.0;
                }
            }

            let color = heat_color(intensity).gamma_multiply(f32::from(style.heat_alpha) / 255.0);

            // Fan triangulation around the centroid: cells are star-shaped
            // from their center, so this is valid for hexagons/pentagons.
            let n = ring.len();
            let mut vertices: Vec<[f32; 2]> = Vec::with_capacity(n + 1);
            vertices.push([clon as f32, clat as f32]);
            for &(lon, lat) in &ring {
                vertices.push([lon as f32, lat as f32]);
            }
            let mut indices: Vec<u32> = Vec::with_capacity(n * 3);
            for i in 0..n as u32 {
                indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n as u32]);
            }
            mesh.push_polygon(&vertices, &indices, color);
            built += 1;
        }
        Self {
            mesh,
            cache: MeshCache::default(),
            cells: built,
        }
    }

    pub fn cell_count(&self) -> usize {
        self.cells
    }

    pub fn paint(&mut self, painter: &Painter, aff: &Affine, screen_w: f32) {
        if self.mesh.is_empty() {
            return;
        }
        let offsets = visible_world_offsets(aff, screen_w);
        let mut key = affine_key(aff);
        key ^= offsets.len() as u64;
        let mesh = &self.mesh;
        let meshes = self.cache.get_or_build(key, || {
            offsets.iter().map(|&o| mesh.to_mesh(aff, o)).collect()
        });
        for m in meshes {
            painter.add(Shape::mesh(m.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_fans_for_valid_cells_and_skips_garbage() {
        let paris = geo_utils::cell_for_latlon(48.85, 2.35, 3).unwrap();
        let fiji = geo_utils::cell_for_latlon(-17.5, 179.9, 3).unwrap();
        let layer = HeatmapLayer::from_cells(
            &[(paris, 0.8), (fiji, 0.4), (0xdead_beef, 1.0)],
            &MapStyle::default(),
        );
        assert_eq!(layer.cell_count(), 2);
        assert!(layer.mesh.indices.len().is_multiple_of(3));
        // Fan triangles must never span most of the world (antimeridian bug).
        let pos = &layer.mesh.positions;
        for tri in layer.mesh.indices.chunks(3) {
            let lons: Vec<f32> = tri.iter().map(|&i| pos[i as usize][0]).collect();
            let spread = lons.iter().cloned().fold(f32::MIN, f32::max)
                - lons.iter().cloned().fold(f32::MAX, f32::min);
            assert!(spread < 90.0, "triangle spans {spread}° of longitude");
        }
    }

    #[test]
    fn empty_layer_paints_nothing() {
        let layer = HeatmapLayer::empty();
        assert_eq!(layer.cell_count(), 0);
        assert!(layer.mesh.is_empty());
    }
}
