//! Country basemap: Natural Earth GeoJSON → earcut-triangulated fill mesh
//! (cached) + thin border polylines.

use egui::{Painter, Pos2, Shape, Stroke};
use geo_utils::Affine;
use geojson::{GeoJson, GeometryValue, PolygonType};

use crate::{GeoMesh, MapStyle, MeshCache, RenderError, affine_key, visible_world_offsets};

/// One country outline ring, tagged with the country it belongs to so the
/// border hierarchy can pick it out (docs/VISUALIZATION.md V3 item 9).
struct BorderRing {
    points: Vec<[f32; 2]>,
    /// ISO 3166-1 alpha-3, resolved exactly the way `geo_utils::CountryIndex`
    /// resolves it — the two have to agree or emphasis silently matches
    /// nothing. `None` when Natural Earth publishes neither code.
    iso_a3: Option<String>,
}

pub struct BasemapLayer {
    fills: GeoMesh,
    /// Border rings in lon/lat. Drawn as thin epaint line strips per frame —
    /// cheap; the expensive part (fill triangulation) is cached.
    borders: Vec<BorderRing>,
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
            let iso_a3 = feature_iso_a3(feature);
            match &geometry.value {
                GeometryValue::Polygon { coordinates } => {
                    add_polygon(&mut fills, &mut borders, coordinates, style, &iso_a3)?;
                }
                GeometryValue::MultiPolygon { coordinates } => {
                    for rings in coordinates {
                        add_polygon(&mut fills, &mut borders, rings, style, &iso_a3)?;
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

    /// Paint fills and borders. `emphasis` is an ISO-A3 code whose rings are
    /// drawn brighter and heavier, and **after** every other ring so they are
    /// never overdrawn by a neighbour sharing the boundary.
    pub fn paint(
        &mut self,
        painter: &Painter,
        aff: &Affine,
        screen_w: f32,
        style: &MapStyle,
        emphasis: Option<&str>,
    ) {
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

        let project = |ring: &BorderRing, offset: f64| -> Vec<Pos2> {
            ring.points
                .iter()
                .map(|p| {
                    let (x, y) = aff.apply(f64::from(p[0]) + offset, f64::from(p[1]));
                    Pos2::new(x, y)
                })
                .collect()
        };
        let is_emphasized = |ring: &BorderRing| match (emphasis, ring.iso_a3.as_deref()) {
            (Some(want), Some(have)) => want == have,
            _ => false,
        };

        let base = Stroke::new(style.border_width, style.border);
        for &offset in &offsets {
            for ring in self.borders.iter().filter(|r| !is_emphasized(r)) {
                painter.add(Shape::line(project(ring, offset), base));
            }
        }
        if emphasis.is_none() {
            return;
        }
        let strong = Stroke::new(
            style.border_width * EMPHASIS_WIDTH_FACTOR,
            style.border_emphasis,
        );
        for &offset in &offsets {
            for ring in self.borders.iter().filter(|r| is_emphasized(r)) {
                painter.add(Shape::line(project(ring, offset), strong));
            }
        }
    }
}

/// How much heavier an emphasized border is than an ordinary one. Enough to
/// be unmistakable at world zoom, not enough to read as a data layer.
const EMPHASIS_WIDTH_FACTOR: f32 = 2.4;

/// Natural Earth's ISO-A3 for a feature, with the same `-99` → `ADM0_A3`
/// fallback `geo_utils::CountryIndex` applies. Kept in lockstep with that
/// function by `emphasis_codes_match_the_country_index` in the tests below.
fn feature_iso_a3(feature: &geojson::Feature) -> Option<String> {
    let prop = |key: &str| -> Option<String> {
        feature
            .properties
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    match prop("ISO_A3") {
        Some(v) if v != "-99" => Some(v),
        _ => prop("ADM0_A3"),
    }
}

/// Triangulate one polygon (exterior + holes) with earcut and record its
/// exterior ring for border strokes.
fn add_polygon(
    fills: &mut GeoMesh,
    borders: &mut Vec<BorderRing>,
    rings: &PolygonType,
    style: &MapStyle,
    iso_a3: &Option<String>,
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

    borders.push(BorderRing {
        points: rings[0]
            .iter()
            .map(|p| {
                let c = p.as_slice();
                [c[0] as f32, c[1] as f32]
            })
            .collect(),
        iso_a3: iso_a3.clone(),
    });
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
            "type": "Feature", "properties": {"ISO_A3": "IDN"},
            "geometry": {"type": "MultiPolygon", "coordinates": [
              [[[0,0],[1,0],[1,1],[0,1],[0,0]]],
              [[[5,5],[6,5],[6,6],[5,6],[5,5]]]
            ]}
          }]
        }"#;
        let layer = BasemapLayer::from_geojson_str(sample, &MapStyle::default()).unwrap();
        assert_eq!(layer.borders.len(), 2);
        // Every island of a country carries that country's code, so emphasis
        // lights up the whole country rather than one of its rings.
        assert!(
            layer
                .borders
                .iter()
                .all(|r| r.iso_a3.as_deref() == Some("IDN"))
        );
        assert!(!layer.fills.is_empty());
    }

    /// Border emphasis is looked up by the code `geo_utils::CountryIndex`
    /// hands back. If the two resolve `-99` differently the emphasis matches
    /// nothing and fails *silently*, which is exactly the kind of bug a test
    /// has to catch.
    #[test]
    fn emphasis_codes_match_the_country_index() {
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [
            {"type": "Feature",
             "properties": {"ISO_A3": "-99", "ADM0_A3": "FRA", "NAME": "France"},
             "geometry": {"type": "Polygon",
              "coordinates": [[[0,0],[10,0],[10,10],[0,10],[0,0]]]}},
            {"type": "Feature",
             "properties": {"ISO_A3": "KEN", "ADM0_A3": "KEN", "NAME": "Kenya"},
             "geometry": {"type": "Polygon",
              "coordinates": [[[30,-5],[40,-5],[40,5],[30,5],[30,-5]]]}}
          ]
        }"#;
        let layer = BasemapLayer::from_geojson_str(sample, &MapStyle::default()).unwrap();
        let index = geo_utils::CountryIndex::from_geojson_str(sample).unwrap();
        for (lon, lat) in [(5.0, 5.0), (35.0, 0.0)] {
            let from_index = &index.country_at(lon, lat).expect("inside a polygon").iso_a3;
            assert!(
                layer
                    .borders
                    .iter()
                    .any(|r| r.iso_a3.as_deref() == Some(from_index.as_str())),
                "no border ring tagged {from_index}"
            );
        }
    }
}
