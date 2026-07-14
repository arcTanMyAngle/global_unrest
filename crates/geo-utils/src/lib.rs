//! Geospatial utilities: equirectangular viewport math, H3 cell assignment,
//! antimeridian-safe cell boundaries, and country point-in-polygon lookup.
//!
//! This crate is egui-free and I/O-free; it operates on data handed to it.

use geo::{BoundingRect, Contains};
use h3o::{CellIndex, LatLng, Resolution};

#[derive(Debug, thiserror::Error)]
pub enum GeoError {
    #[error("invalid lat/lon: lat={lat}, lon={lon}")]
    InvalidLatLng { lat: f64, lon: f64 },
    #[error("invalid H3 resolution: {0}")]
    InvalidResolution(u8),
    #[error("invalid H3 cell index: {0:#x}")]
    InvalidCell(u64),
    #[error("geojson error: {0}")]
    Geojson(String),
}

// ---------------------------------------------------------------------------
// H3
// ---------------------------------------------------------------------------

/// Assign the H3 cell containing (lat, lon) at `res`.
///
/// Rejects out-of-range coordinates. (h3o itself only rejects non-finite
/// values and silently normalizes e.g. lat 999° onto the sphere — we want
/// garbage records to fail into `ingest_log`, not to land somewhere legal.)
pub fn cell_for_latlon(lat: f64, lon: f64, res: u8) -> Result<u64, GeoError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(GeoError::InvalidLatLng { lat, lon });
    }
    let resolution = Resolution::try_from(res).map_err(|_| GeoError::InvalidResolution(res))?;
    let ll = LatLng::new(lat, lon).map_err(|_| GeoError::InvalidLatLng { lat, lon })?;
    Ok(ll.to_cell(resolution).into())
}

/// Cell boundary as (lon, lat) pairs, **antimeridian-normalized**: vertices
/// are kept contiguous by shifting longitudes ±360°, so a cell straddling
/// ±180° comes back as one connected ring whose lons may leave [-180, 180].
/// Renderers draw a wrapped copy shifted by ∓360° to cover both map edges.
pub fn cell_boundary_lonlat(cell: u64) -> Result<Vec<(f64, f64)>, GeoError> {
    let idx = CellIndex::try_from(cell).map_err(|_| GeoError::InvalidCell(cell))?;
    let boundary = idx.boundary();
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(boundary.len());
    for ll in boundary.iter() {
        let lat = ll.lat();
        let mut lon = ll.lng();
        if let Some(&(prev_lon, _)) = out.last() {
            // Keep each vertex within 180° of the previous one.
            while lon - prev_lon > 180.0 {
                lon -= 360.0;
            }
            while lon - prev_lon < -180.0 {
                lon += 360.0;
            }
        }
        out.push((lon, lat));
    }
    Ok(out)
}

/// Cell centroid as (lon, lat).
pub fn cell_center_lonlat(cell: u64) -> Result<(f64, f64), GeoError> {
    let idx = CellIndex::try_from(cell).map_err(|_| GeoError::InvalidCell(cell))?;
    let ll = LatLng::from(idx);
    Ok((ll.lng(), ll.lat()))
}

// ---------------------------------------------------------------------------
// Equirectangular viewport
// ---------------------------------------------------------------------------

/// Equirectangular (plate carrée) viewport: projection is **affine in
/// lon/lat**, which is what makes cached-mesh rendering cheap — a viewport
/// change is one mul-add per vertex. Screen y grows downward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapViewport {
    pub center_lon: f64,
    pub center_lat: f64,
    /// Degrees of longitude per screen pixel (zoom). Smaller = closer.
    pub deg_per_px: f64,
    pub screen_w: f32,
    pub screen_h: f32,
}

/// Affine coefficients mapping lon/lat → screen px: `x = a*lon + b`,
/// `y = c*lat + d`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
}

impl Affine {
    #[inline]
    pub fn apply(&self, lon: f64, lat: f64) -> (f32, f32) {
        (
            (self.a * lon + self.b) as f32,
            (self.c * lat + self.d) as f32,
        )
    }
}

pub const MIN_DEG_PER_PX: f64 = 0.002;
pub const MAX_DEG_PER_PX: f64 = 1.0;

impl MapViewport {
    /// Fit the whole world (360°) into `screen_w`, centered at (0, 0)°,
    /// with an upper bound so tiny windows don't over-zoom-out.
    pub fn fit_world(screen_w: f32, screen_h: f32) -> Self {
        let w = screen_w.max(64.0);
        let deg_per_px = (360.0 / f64::from(w)).clamp(MIN_DEG_PER_PX, MAX_DEG_PER_PX);
        Self {
            center_lon: 0.0,
            center_lat: 0.0,
            deg_per_px,
            screen_w: w,
            screen_h: screen_h.max(64.0),
        }
    }

    pub fn affine(&self) -> Affine {
        let a = 1.0 / self.deg_per_px;
        let b = f64::from(self.screen_w) / 2.0 - self.center_lon / self.deg_per_px;
        // Latitude increases upward; screen y increases downward.
        let c = -1.0 / self.deg_per_px;
        let d = f64::from(self.screen_h) / 2.0 + self.center_lat / self.deg_per_px;
        Affine { a, b, c, d }
    }

    pub fn project(&self, lon: f64, lat: f64) -> (f32, f32) {
        self.affine().apply(lon, lat)
    }

    pub fn unproject(&self, x: f32, y: f32) -> (f64, f64) {
        let aff = self.affine();
        let lon = (f64::from(x) - aff.b) / aff.a;
        let lat = (f64::from(y) - aff.d) / aff.c;
        (lon, lat)
    }

    /// Pan by screen pixels (positive dx drags content rightward, i.e. the
    /// center moves west).
    pub fn pan_pixels(&mut self, dx: f32, dy: f32) {
        self.center_lon -= f64::from(dx) * self.deg_per_px;
        self.center_lat += f64::from(dy) * self.deg_per_px;
        self.clamp();
    }

    /// Zoom by `factor` (>1 zooms in) keeping the geo point under the given
    /// screen position fixed.
    pub fn zoom_around(&mut self, x: f32, y: f32, factor: f64) {
        let (anchor_lon, anchor_lat) = self.unproject(x, y);
        self.deg_per_px = (self.deg_per_px / factor).clamp(MIN_DEG_PER_PX, MAX_DEG_PER_PX);
        // Re-solve center so the anchor stays under (x, y).
        let half_w = f64::from(self.screen_w) / 2.0;
        let half_h = f64::from(self.screen_h) / 2.0;
        self.center_lon = anchor_lon - (f64::from(x) - half_w) * self.deg_per_px;
        self.center_lat = anchor_lat + (f64::from(y) - half_h) * self.deg_per_px;
        self.clamp();
    }

    pub fn set_screen(&mut self, w: f32, h: f32) {
        self.screen_w = w.max(64.0);
        self.screen_h = h.max(64.0);
    }

    fn clamp(&mut self) {
        self.center_lat = self.center_lat.clamp(-90.0, 90.0);
        // Keep the center longitude wrapped for sanity.
        while self.center_lon > 180.0 {
            self.center_lon -= 360.0;
        }
        while self.center_lon < -180.0 {
            self.center_lon += 360.0;
        }
    }
}

// ---------------------------------------------------------------------------
// Country lookup (Natural Earth GeoJSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CountryInfo {
    /// ISO 3166-1 alpha-3 (falls back to Natural Earth ADM0_A3 where NE
    /// publishes "-99", e.g. France and Norway in some editions).
    pub iso_a3: String,
    pub name: String,
}

struct CountryShape {
    info: CountryInfo,
    bbox: geo::Rect<f64>,
    geom: geo::MultiPolygon<f64>,
}

/// Point-in-polygon country index over Natural Earth countries.
pub struct CountryIndex {
    shapes: Vec<CountryShape>,
}

impl CountryIndex {
    pub fn from_geojson_str(raw: &str) -> Result<Self, GeoError> {
        let gj: geojson::GeoJson = raw.parse().map_err(|e| GeoError::Geojson(format!("{e}")))?;
        let geojson::GeoJson::FeatureCollection(fc) = gj else {
            return Err(GeoError::Geojson("expected a FeatureCollection".into()));
        };
        let mut shapes = Vec::with_capacity(fc.features.len());
        for feature in fc.features {
            let Some(geometry) = feature.geometry.as_ref() else {
                continue;
            };
            let geom: geo::Geometry<f64> =
                geo::Geometry::try_from(geometry).map_err(|e| GeoError::Geojson(format!("{e}")))?;
            let multi = match geom {
                geo::Geometry::Polygon(p) => geo::MultiPolygon(vec![p]),
                geo::Geometry::MultiPolygon(mp) => mp,
                _ => continue,
            };
            let Some(bbox) = multi.bounding_rect() else {
                continue;
            };
            let prop = |key: &str| -> Option<String> {
                feature
                    .properties
                    .as_ref()
                    .and_then(|p| p.get(key))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };
            let iso_a3 = match prop("ISO_A3") {
                Some(v) if v != "-99" => v,
                _ => prop("ADM0_A3").unwrap_or_else(|| "UNK".into()),
            };
            let name = prop("NAME")
                .or_else(|| prop("ADMIN"))
                .unwrap_or_else(|| iso_a3.clone());
            shapes.push(CountryShape {
                info: CountryInfo { iso_a3, name },
                bbox,
                geom: multi,
            });
        }
        Ok(Self { shapes })
    }

    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Country containing (lon, lat), if any. Linear scan with a bbox
    /// pre-check; 177 Natural Earth countries make this plenty fast for
    /// click/hover use.
    pub fn country_at(&self, lon: f64, lat: f64) -> Option<&CountryInfo> {
        let pt = geo::Point::new(lon, lat);
        self.shapes
            .iter()
            .find(|s| s.bbox.contains(&pt) && s.geom.contains(&pt))
            .map(|s| &s.info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h3_cell_assignment_is_stable_and_res_correct() {
        let cell = cell_for_latlon(48.8566, 2.3522, 3).unwrap();
        let idx = CellIndex::try_from(cell).unwrap();
        assert_eq!(u8::from(idx.resolution()), 3);
        // Same point → same cell; nearby point in same cell region too.
        assert_eq!(cell, cell_for_latlon(48.8566, 2.3522, 3).unwrap());
    }

    #[test]
    fn h3_rejects_garbage() {
        assert!(cell_for_latlon(999.0, 0.0, 3).is_err());
        assert!(cell_for_latlon(0.0, 0.0, 99).is_err());
        assert!(cell_boundary_lonlat(0xdead_beef).is_err());
    }

    #[test]
    fn antimeridian_boundary_stays_contiguous() {
        // A cell containing (0°, 179.9°) hugs the antimeridian near Fiji.
        let cell = cell_for_latlon(0.0, 179.9, 3).unwrap();
        let ring = cell_boundary_lonlat(cell).unwrap();
        assert!(ring.len() >= 5);
        for pair in ring.windows(2) {
            let jump = (pair[1].0 - pair[0].0).abs();
            assert!(
                jump < 180.0,
                "boundary must not jump across the antimeridian: {jump}"
            );
        }
    }

    #[test]
    fn viewport_project_unproject_roundtrip() {
        let vp = MapViewport {
            center_lon: 10.0,
            center_lat: 20.0,
            deg_per_px: 0.25,
            screen_w: 800.0,
            screen_h: 600.0,
        };
        let (x, y) = vp.project(2.3522, 48.8566);
        let (lon, lat) = vp.unproject(x, y);
        // Screen coords are f32; ~1e-4° (≈11 m) roundtrip error is fine.
        assert!((lon - 2.3522).abs() < 1e-4, "{lon}");
        assert!((lat - 48.8566).abs() < 1e-4, "{lat}");
        // Center projects to screen center; north is up.
        let (cx, cy) = vp.project(10.0, 20.0);
        assert!((cx - 400.0).abs() < 1e-4 && (cy - 300.0).abs() < 1e-4);
        let (_, y_north) = vp.project(10.0, 30.0);
        assert!(y_north < cy, "greater latitude must be higher on screen");
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let mut vp = MapViewport::fit_world(1000.0, 500.0);
        let anchor_screen = (250.0_f32, 125.0_f32);
        let before = vp.unproject(anchor_screen.0, anchor_screen.1);
        vp.zoom_around(anchor_screen.0, anchor_screen.1, 2.0);
        let after = vp.unproject(anchor_screen.0, anchor_screen.1);
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
    }

    #[test]
    fn country_lookup_from_sample_polygons() {
        // Two rough boxes: "FRA-ish" around Paris, "KEN-ish" around Nairobi.
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [
            {"type":"Feature","properties":{"ISO_A3":"-99","ADM0_A3":"FRA","NAME":"France"},
             "geometry":{"type":"Polygon","coordinates":[[[-5,42],[9,42],[9,51],[-5,51],[-5,42]]]}},
            {"type":"Feature","properties":{"ISO_A3":"KEN","NAME":"Kenya"},
             "geometry":{"type":"Polygon","coordinates":[[[33,-5],[42,-5],[42,5],[33,5],[33,-5]]]}}
          ]
        }"#;
        let index = CountryIndex::from_geojson_str(sample).unwrap();
        assert_eq!(index.len(), 2);
        // The -99 quirk falls back to ADM0_A3.
        assert_eq!(index.country_at(2.35, 48.85).unwrap().iso_a3, "FRA");
        assert_eq!(index.country_at(36.82, -1.29).unwrap().iso_a3, "KEN");
        assert!(index.country_at(-140.0, 0.0).is_none());
    }
}
