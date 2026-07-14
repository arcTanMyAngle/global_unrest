//! Country basemap: Natural Earth GeoJSON → earcut-triangulated fill mesh
//! (cached) + thin border polylines.

use egui::{Painter, Pos2, Shape, Stroke};
use geo_utils::Affine;
use geojson::{GeoJson, GeometryValue, PolygonType};

use crate::{GeoMesh, MapStyle, MeshCache, RenderError, affine_key, visible_world_offsets};

pub struct BasemapLayer {
    fills: GeoMesh,
    /// Border rings in lon/lat. Drawn as thin epaint line strips per frame —
    /// cheap; the expensive part (fill triangulation) is cached.
    borders: Vec<Vec<[f32; 2]>>,
    cache: MeshCache,
}

impl BasemapLayer {
    /// Build from a Natural Earth countries FeatureCollection. Triangulation
    /// (earcut) happens once, here.
    pub fn from_geojson_str(raw: &str, style: &MapStyle) -> Result<Self, RenderError> {
        let gj: GeoJson = raw
            .parse()
            .map_err(|e| RenderError::Geojson(format!("{e}")))?;
        let GeoJson::FeatureCollection(fc) = gj else {
            return Err(RenderError::Geojson("expected FeatureCollection".into()));
        };

        let mut fills = GeoMesh::default();
        let mut borders = Vec::new();
        for feature in &fc.features {
            let Some(geometry) = feature.geometry.as_ref() else {
                continue;
            };
            match &geometry.value {
                GeometryValue::Polygon { coordinates } => {
                    add_polygon(&mut fills, &mut borders, coordinates, style)?;
                }
                GeometryValue::MultiPolygon { coordinates } => {
                    for rings in coordinates {
                        add_polygon(&mut fills, &mut borders, rings, style)?;
                    }
                }
                _ => {}
            }
        }
        tracing::info!(
            vertices = fills.vertex_count(),
            triangles = fills.indices.len() / 3,
            rings = borders.len(),
            "basemap tessellated"
        );
        Ok(Self {
            fills,
            borders,
            cache: MeshCache::default(),
        })
    }

    pub fn paint(&mut self, painter: &Painter, aff: &Affine, screen_w: f32, style: &MapStyle) {
        let offsets = visible_world_offsets(aff, screen_w);
        let mut key = affine_key(aff);
        key ^= offsets.len() as u64;

        let fills = &self.fills;
        let meshes = self.cache.get_or_build(key, || {
            offsets.iter().map(|&o| fills.to_mesh(aff, o)).collect()
        });
        for mesh in meshes {
            painter.add(Shape::mesh(mesh.clone()));
        }

        let stroke = Stroke::new(style.border_width, style.border);
        for &offset in &offsets {
            for ring in &self.borders {
                let points: Vec<Pos2> = ring
                    .iter()
                    .map(|p| {
                        let (x, y) = aff.apply(f64::from(p[0]) + offset, f64::from(p[1]));
                        Pos2::new(x, y)
                    })
                    .collect();
                painter.add(Shape::line(points, stroke));
            }
        }
    }
}

/// Triangulate one polygon (exterior + holes) with earcut and record its
/// exterior ring for border strokes.
fn add_polygon(
    fills: &mut GeoMesh,
    borders: &mut Vec<Vec<[f32; 2]>>,
    rings: &PolygonType,
    style: &MapStyle,
) -> Result<(), RenderError> {
    if rings.is_empty() || rings[0].len() < 4 {
        return Ok(());
    }

    let mut flat: Vec<f64> = Vec::new();
    let mut hole_indices: Vec<usize> = Vec::new();
    let mut vertices: Vec<[f32; 2]> = Vec::new();
    for (ring_idx, ring) in rings.iter().enumerate() {
        if ring_idx > 0 {
            hole_indices.push(vertices.len());
        }
        for pos in ring {
            let coords = pos.as_slice();
            if coords.len() < 2 {
                return Err(RenderError::Geometry("position with < 2 coords".into()));
            }
            flat.push(coords[0]);
            flat.push(coords[1]);
            vertices.push([coords[0] as f32, coords[1] as f32]);
        }
    }

    let triangles = earcutr::earcut(&flat, &hole_indices, 2)
        .map_err(|e| RenderError::Geometry(format!("earcut: {e:?}")))?;
    let indices: Vec<u32> = triangles.iter().map(|&i| i as u32).collect();
    fills.push_polygon(&vertices, &indices, style.land_fill);

    borders.push(
        rings[0]
            .iter()
            .map(|p| {
                let c = p.as_slice();
                [c[0] as f32, c[1] as f32]
            })
            .collect(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangulates_polygon_with_hole() {
        // A square with a square hole: earcut yields 8 triangles.
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature", "properties": {},
            "geometry": {"type": "Polygon", "coordinates": [
              [[0,0],[10,0],[10,10],[0,10],[0,0]],
              [[4,4],[6,4],[6,6],[4,6],[4,4]]
            ]}
          }]
        }"#;
        let layer = BasemapLayer::from_geojson_str(sample, &MapStyle::default()).unwrap();
        assert_eq!(layer.fills.indices.len() % 3, 0);
        assert!(layer.fills.indices.len() / 3 >= 6, "hole must be cut out");
        assert_eq!(
            layer.borders.len(),
            1,
            "only exterior ring becomes a border"
        );
    }

    #[test]
    fn multipolygon_features_are_flattened() {
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [{
            "type": "Feature", "properties": {},
            "geometry": {"type": "MultiPolygon", "coordinates": [
              [[[0,0],[1,0],[1,1],[0,1],[0,0]]],
              [[[5,5],[6,5],[6,6],[5,6],[5,5]]]
            ]}
          }]
        }"#;
        let layer = BasemapLayer::from_geojson_str(sample, &MapStyle::default()).unwrap();
        assert_eq!(layer.borders.len(), 2);
        assert!(!layer.fills.is_empty());
    }
}
