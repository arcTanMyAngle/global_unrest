//! Geospatial utilities: equirectangular viewport math, H3 cell assignment,
//! antimeridian-safe cell boundaries, and country point-in-polygon lookup.
//!
//! This crate is egui-free and I/O-free; it operates on data handed to it.

use geo::{BoundingRect, Centroid, Contains};
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

/// Parent of `cell` at the coarser resolution `res`. Parents are derived on
/// demand for display rollups — only res-3 cells are ever stored
/// (docs/DATA_MODEL.md). `res` must not exceed the cell's own resolution.
pub fn cell_parent(cell: u64, res: u8) -> Result<u64, GeoError> {
    let idx = CellIndex::try_from(cell).map_err(|_| GeoError::InvalidCell(cell))?;
    let resolution = Resolution::try_from(res).map_err(|_| GeoError::InvalidResolution(res))?;
    idx.parent(resolution)
        .map(u64::from)
        .ok_or(GeoError::InvalidResolution(res))
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
    /// ISO 3166-1 alpha-2, when Natural Earth publishes one (empty string
    /// for the rare "-99" gaps — never guessed).
    pub iso_a2: String,
    pub name: String,
}

struct CountryShape {
    info: CountryInfo,
    bbox: geo::Rect<f64>,
    geom: geo::MultiPolygon<f64>,
    /// Geometric centroid (lon, lat) of the country's polygon(s), precomputed
    /// once at load via `geo::Centroid` (area-weighted, not a vertex mean).
    centroid: Option<(f64, f64)>,
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
            let iso_a2 = match prop("ISO_A2") {
                Some(v) if v != "-99" => v,
                _ => String::new(),
            };
            let name = prop("NAME")
                .or_else(|| prop("ADMIN"))
                .unwrap_or_else(|| iso_a3.clone());
            let centroid = multi.centroid().map(|p| (p.x(), p.y()));
            shapes.push(CountryShape {
                info: CountryInfo {
                    iso_a3,
                    iso_a2,
                    name,
                },
                bbox,
                geom: multi,
                centroid,
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

    /// Country info + geometric centroid (lon, lat) for an ISO 3166-1
    /// alpha-2 code (case-insensitive, trimmed). `None` for an unknown or
    /// un-centroidable code — never guessed, matching the precision
    /// contract every source adapter follows.
    pub fn centroid_by_iso_a2(&self, code: &str) -> Option<(&CountryInfo, (f64, f64))> {
        let code = code.trim();
        self.shapes
            .iter()
            .find(|s| !s.info.iso_a2.is_empty() && s.info.iso_a2.eq_ignore_ascii_case(code))
            .and_then(|s| s.centroid.map(|c| (&s.info, c)))
    }

    /// Every country that has a usable centroid, as (info, (lon, lat)).
    ///
    /// Countries whose geometry yielded no centroid are skipped — callers
    /// building name→coordinate tables must not invent one.
    pub fn iter_with_centroid(&self) -> impl Iterator<Item = (&CountryInfo, (f64, f64))> {
        self.shapes
            .iter()
            .filter_map(|s| s.centroid.map(|c| (&s.info, c)))
    }
}

// ---------------------------------------------------------------------------
// City gazetteer (Natural Earth populated places)
// ---------------------------------------------------------------------------

/// One populated place from Natural Earth's 1:110m `populated_places` set.
#[derive(Debug, Clone)]
pub struct CityInfo {
    /// Canonical name, as Natural Earth spells it ("Malé", "São Paulo").
    pub name: String,
    /// Other spellings Natural Earth publishes for the same place: the ASCII
    /// transliteration (`nameascii`) plus the pipe-separated `namealt` list,
    /// deduped and with `name` itself removed. Text matchers need these —
    /// people type "Sao Paulo" far more often than "São Paulo".
    pub alt_names: Vec<String>,
    /// Containing country's name (`adm0name`).
    pub country_name: String,
    /// Containing country's ISO 3166-1 alpha-3 (`adm0_a3`).
    pub iso_a3: String,
    pub lon: f64,
    pub lat: f64,
    /// Natural Earth's maximum population estimate; 0 when unpublished.
    pub pop_max: u64,
}

/// Gazetteer of Natural Earth's ~240 major populated places.
///
/// Point data, so this is a name→coordinate lookup, not a point-in-polygon
/// index like [`CountryIndex`] — there are no city polygons at this scale.
pub struct CityIndex {
    cities: Vec<CityInfo>,
}

impl CityIndex {
    /// Parse the `ne_110m_populated_places_simple` FeatureCollection.
    ///
    /// Features without a Point geometry, or missing a name, are skipped
    /// rather than defaulted — a gazetteer entry with a guessed coordinate
    /// would be worse than no entry at all.
    pub fn from_geojson_str(raw: &str) -> Result<Self, GeoError> {
        let gj: geojson::GeoJson = raw.parse().map_err(|e| GeoError::Geojson(format!("{e}")))?;
        let geojson::GeoJson::FeatureCollection(fc) = gj else {
            return Err(GeoError::Geojson("expected a FeatureCollection".into()));
        };
        let mut cities = Vec::with_capacity(fc.features.len());
        for feature in fc.features {
            let Some(geometry) = feature.geometry.as_ref() else {
                continue;
            };
            // Reuse the same geo conversion CountryIndex uses rather than
            // destructuring geojson's Position newtype by hand.
            let geom: geo::Geometry<f64> =
                geo::Geometry::try_from(geometry).map_err(|e| GeoError::Geojson(format!("{e}")))?;
            let geo::Geometry::Point(point) = geom else {
                continue;
            };
            let prop = |key: &str| -> Option<String> {
                feature
                    .properties
                    .as_ref()
                    .and_then(|p| p.get(key))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            let Some(name) = prop("name") else {
                continue;
            };

            // `namealt` packs several spellings into one pipe-separated field.
            let mut alt_names: Vec<String> = Vec::new();
            let mut push_alt = |candidate: &str| {
                let candidate = candidate.trim();
                if !candidate.is_empty()
                    && candidate != name
                    && !alt_names.iter().any(|a: &String| a == candidate)
                {
                    alt_names.push(candidate.to_owned());
                }
            };
            if let Some(ascii) = prop("nameascii") {
                push_alt(&ascii);
            }
            if let Some(alt) = prop("namealt") {
                for part in alt.split('|') {
                    push_alt(part);
                }
            }

            let pop_max = feature
                .properties
                .as_ref()
                .and_then(|p| p.get("pop_max"))
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
                .unwrap_or(0);

            cities.push(CityInfo {
                name,
                alt_names,
                country_name: prop("adm0name").unwrap_or_default(),
                iso_a3: prop("adm0_a3").unwrap_or_else(|| "UNK".into()),
                lon: point.x(),
                lat: point.y(),
                pop_max,
            });
        }
        Ok(Self { cities })
    }

    pub fn len(&self) -> usize {
        self.cities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cities.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &CityInfo> {
        self.cities.iter()
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
    fn parent_rollup_contains_child_center() {
        let child = cell_for_latlon(48.8566, 2.3522, 3).unwrap();
        for res in [1u8, 2] {
            let parent = cell_parent(child, res).unwrap();
            let idx = CellIndex::try_from(parent).unwrap();
            assert_eq!(u8::from(idx.resolution()), res);
            // The parent at the child's own location must be that parent.
            let via_latlon = cell_for_latlon(48.8566, 2.3522, res).unwrap();
            assert_eq!(parent, via_latlon);
        }
        // Same resolution is the identity; finer resolutions are an error.
        assert_eq!(cell_parent(child, 3).unwrap(), child);
        assert!(cell_parent(child, 5).is_err());
        assert!(cell_parent(0xdead_beef, 1).is_err());
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
            {"type":"Feature","properties":{"ISO_A3":"-99","ISO_A2":"FR","ADM0_A3":"FRA","NAME":"France"},
             "geometry":{"type":"Polygon","coordinates":[[[-5,42],[9,42],[9,51],[-5,51],[-5,42]]]}},
            {"type":"Feature","properties":{"ISO_A3":"KEN","ISO_A2":"KE","NAME":"Kenya"},
             "geometry":{"type":"Polygon","coordinates":[[[33,-5],[42,-5],[42,5],[33,5],[33,-5]]]}},
            {"type":"Feature","properties":{"ISO_A3":"UNK","ISO_A2":"-99","NAME":"No A2"},
             "geometry":{"type":"Polygon","coordinates":[[[100,-5],[102,-5],[102,-3],[100,-3],[100,-5]]]}}
          ]
        }"#;
        let index = CountryIndex::from_geojson_str(sample).unwrap();
        assert_eq!(index.len(), 3);
        // The -99 quirk falls back to ADM0_A3.
        assert_eq!(index.country_at(2.35, 48.85).unwrap().iso_a3, "FRA");
        assert_eq!(index.country_at(36.82, -1.29).unwrap().iso_a3, "KEN");
        assert!(index.country_at(-140.0, 0.0).is_none());
    }

    #[test]
    fn centroid_by_iso_a2_resolves_case_insensitively_and_rejects_unknown() {
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [
            {"type":"Feature","properties":{"ISO_A3":"FRA","ISO_A2":"FR","NAME":"France"},
             "geometry":{"type":"Polygon","coordinates":[[[-5,42],[9,42],[9,51],[-5,51],[-5,42]]]}},
            {"type":"Feature","properties":{"ISO_A3":"UNK","ISO_A2":"-99","NAME":"No A2"},
             "geometry":{"type":"Polygon","coordinates":[[[100,-5],[102,-5],[102,-3],[100,-3],[100,-5]]]}}
          ]
        }"#;
        let index = CountryIndex::from_geojson_str(sample).unwrap();

        let (info, (lon, lat)) = index.centroid_by_iso_a2("fr").unwrap();
        assert_eq!(info.iso_a3, "FRA");
        // Centroid of a [-5,42]..[9,51] box is its midpoint (2, 46.5).
        assert!((lon - 2.0).abs() < 1e-9, "lon {lon}");
        assert!((lat - 46.5).abs() < 1e-9, "lat {lat}");

        // Never guessed: unknown code and the "-99" gap both miss.
        assert!(index.centroid_by_iso_a2("ZZ").is_none());
        assert!(index.centroid_by_iso_a2("-99").is_none());

        // Every yielded country carries a real centroid; the "-99" one still
        // has geometry, so it is present here — the filter is on centroid,
        // not on ISO codes.
        assert_eq!(index.iter_with_centroid().count(), 2);
    }

    #[test]
    fn city_index_keeps_alt_spellings_and_skips_unusable_features() {
        // Shaped exactly like ne_110m_populated_places_simple: `namealt` is
        // pipe-separated, `nameascii` is the transliteration.
        let sample = r#"{
          "type": "FeatureCollection",
          "features": [
            {"type":"Feature","properties":{"name":"Malé","nameascii":"Male","namealt":"Male",
              "adm0name":"Maldives","adm0_a3":"MDV","pop_max":133019},
             "geometry":{"type":"Point","coordinates":[73.5,4.17]}},
            {"type":"Feature","properties":{"name":"Panama City","nameascii":"Panama City",
              "namealt":"Ciudad de Panam|Panama","adm0name":"Panama","adm0_a3":"PAN","pop_max":1281647},
             "geometry":{"type":"Point","coordinates":[-79.53,8.97]}},
            {"type":"Feature","properties":{"name":"No Geometry","adm0_a3":"XXX"},
             "geometry":null},
            {"type":"Feature","properties":{"adm0name":"Nameless","adm0_a3":"YYY"},
             "geometry":{"type":"Point","coordinates":[1.0,2.0]}},
            {"type":"Feature","properties":{"name":"Not A Point","adm0_a3":"ZZZ"},
             "geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1],[0,0]]]}}
          ]
        }"#;
        let index = CityIndex::from_geojson_str(sample).unwrap();
        // Null geometry, missing name, and non-Point geometry are all skipped.
        assert_eq!(index.len(), 2);

        let male = index.iter().find(|c| c.name == "Malé").unwrap();
        assert_eq!(male.iso_a3, "MDV");
        assert!((male.lon - 73.5).abs() < 1e-9);
        assert!((male.lat - 4.17).abs() < 1e-9);
        // `nameascii` and `namealt` agree here, so the alt list dedupes to one.
        assert_eq!(male.alt_names, vec!["Male".to_owned()]);

        // Pipe-separated alternates are split; `name` itself is not repeated.
        let panama = index.iter().find(|c| c.name == "Panama City").unwrap();
        assert_eq!(
            panama.alt_names,
            vec!["Ciudad de Panam".to_owned(), "Panama".to_owned()]
        );
    }
}
