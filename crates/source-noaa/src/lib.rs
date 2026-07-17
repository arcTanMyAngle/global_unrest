//! NOAA/NWS active-alerts source adapter (M5, optional layer).
//!
//! `api.weather.gov/alerts/active` is keyless US-government public-domain
//! data; the API's only ask is a descriptive `User-Agent`. The live path is
//! **feature-gated behind `live`** like `source-acled`; [`normalize_alert`]
//! is pure and always compiled.
//!
//! Coverage honesty: this feed is US + territories only — a documented
//! coverage bias (docs/SAFETY_AND_PRIVACY.md), not a global weather layer.
//! Alerts normalize as [`EventKind::Disruption`] with weather themes.
//!
//! Geometry honesty: many alerts are zone-scoped with **no polygon**. We
//! never invent coordinates, so those normalize to *zero* events by design
//! (an alert without a polygon yields `Ok(vec![])`, not an error — it is
//! well-formed, just outside our precision contract). Polygon alerts land at
//! the polygon centroid with [`LocationPrecision::Admin1`], shading regions
//! only, never point markers.

#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
pub use live::NoaaSource;

use chrono::{DateTime, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, NormalizeError, SourceId,
    event_id,
};
use serde_json::Value;

/// The production active-alerts endpoint (GeoJSON).
pub const ALERTS_URL: &str = "https://api.weather.gov/alerts/active";

/// Normalize one GeoJSON alert feature → zero or one events.
///
/// Zero when the alert carries no polygon geometry (zone-scoped alerts —
/// we refuse to guess coordinates). One event otherwise: `Disruption` at the
/// polygon centroid, `Admin1` precision (region shading only), severity from
/// NWS `severity`, themes `["noaa", "weather", <event slug>]`.
pub fn normalize_alert(v: &Value) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
    let props = v
        .get("properties")
        .and_then(Value::as_object)
        .ok_or(NormalizeError::MissingField("properties"))?;

    let Some((lat, lon)) = polygon_centroid(v.get("geometry")) else {
        return Ok(Vec::new()); // zone-scoped alert: no polygon, no event
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(NormalizeError::InvalidCoordinates { lat, lon });
    }
    let h3_cell = geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).map_err(|e| {
        NormalizeError::InvalidValue {
            field: "geometry",
            detail: format!("h3 assignment failed: {e}"),
        }
    })?;

    let source_event_id = props
        .get("id")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField("properties.id"))?
        .to_owned();

    // Alert timing: onset when stated, else effective, else sent.
    let ts_field = ["onset", "effective", "sent"]
        .iter()
        .find_map(|f| props.get(*f).and_then(Value::as_str))
        .ok_or(NormalizeError::MissingField("onset/effective/sent"))?;
    let ts_utc: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_field)
        .map_err(|e| NormalizeError::InvalidValue {
            field: "onset",
            detail: format!("`{ts_field}`: {e}"),
        })?
        .with_timezone(&Utc);

    let event_label = props
        .get("event")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField("properties.event"))?;

    // NWS severity scale → 0..1 (Unknown ⇒ no severity claim).
    let severity = match props.get("severity").and_then(Value::as_str) {
        Some("Extreme") => Some(1.0),
        Some("Severe") => Some(0.75),
        Some("Moderate") => Some(0.5),
        Some("Minor") => Some(0.25),
        _ => None,
    };

    let mut themes = vec!["noaa".to_owned(), "weather".to_owned()];
    let slug = slugify(event_label);
    if !slug.is_empty() && !themes.contains(&slug) {
        themes.push(slug);
    }

    // First UGC zone's state prefix (e.g. "CAZ006" → "CA") as admin1.
    let admin1 = props
        .get("geocode")
        .and_then(|g| g.get("UGC"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .filter(|u| u.len() >= 2 && u[..2].bytes().all(|b| b.is_ascii_uppercase()))
        .map(|u| u[..2].to_owned());

    // The feature's top-level `id` is the alert's canonical API URL.
    let urls = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("http"))
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default();

    Ok(vec![GeoTemporalEvent {
        id: event_id(SourceId::Noaa, &source_event_id),
        source: SourceId::Noaa,
        source_event_id,
        kind: EventKind::Disruption,
        themes,
        ts_utc,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision: LocationPrecision::Admin1,
        location_confidence: 0.65,
        country_iso: "USA".to_owned(),
        admin1,
        h3_cell,
        article_count: 1,
        distinct_source_count: 1,
        severity,
        // The alert type label (e.g. "Flood Warning") — headline metadata only.
        headline: Some(event_label.to_owned()),
        outlet_domains: vec!["weather.gov".to_owned()],
        urls,
    }])
}

/// Centroid (vertex mean) of the first exterior ring of a `Polygon` /
/// `MultiPolygon`. `None` for missing/null/other geometry. A vertex mean is
/// sufficient for Admin1-precision region shading — never a point marker.
fn polygon_centroid(geometry: Option<&Value>) -> Option<(f64, f64)> {
    let g = geometry?;
    let ring = match g.get("type").and_then(Value::as_str)? {
        "Polygon" => g.get("coordinates")?.get(0)?,
        "MultiPolygon" => g.get("coordinates")?.get(0)?.get(0)?,
        _ => return None,
    };
    let ring = ring.as_array()?;
    // GeoJSON rings repeat the first vertex at the end; skip the duplicate.
    let n = ring.len().checked_sub(1).filter(|n| *n >= 3)?;
    let (mut lat_sum, mut lon_sum) = (0.0, 0.0);
    for pos in &ring[..n] {
        let lon = pos.get(0).and_then(Value::as_f64)?;
        let lat = pos.get(1).and_then(Value::as_f64)?;
        lon_sum += lon;
        lat_sum += lat;
    }
    Some((lat_sum / n as f64, lon_sum / n as f64))
}

/// Lowercase alphanumeric slug: `"Flood Warning"` → `"flood_warning"`.
fn slugify(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A real-shaped NWS alert feature (synthetic content).
    fn sample_alert() -> Value {
        json!({
            "id": "https://api.weather.gov/alerts/urn:oid:2.49.0.1.840.0.synthetic1",
            "type": "Feature",
            "geometry": {
                "type": "Polygon",
                "coordinates": [[
                    [-122.6, 38.2],
                    [-122.2, 38.2],
                    [-122.2, 38.6],
                    [-122.6, 38.6],
                    [-122.6, 38.2]
                ]]
            },
            "properties": {
                "id": "urn:oid:2.49.0.1.840.0.synthetic1",
                "event": "Flood Warning",
                "severity": "Severe",
                "onset": "2026-07-10T06:00:00-07:00",
                "effective": "2026-07-10T05:00:00-07:00",
                "sent": "2026-07-10T04:55:00-07:00",
                "areaDesc": "Napa County",
                "geocode": { "UGC": ["CAZ018"], "SAME": ["006055"] },
                "headline": "[synthetic] Flood Warning issued for Napa County"
            }
        })
    }

    #[test]
    fn golden_polygon_alert() {
        let evs = normalize_alert(&sample_alert()).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(
            e.id,
            event_id(SourceId::Noaa, "urn:oid:2.49.0.1.840.0.synthetic1")
        );
        assert_eq!(e.source, SourceId::Noaa);
        assert_eq!(e.kind, EventKind::Disruption);
        // Square centroid.
        assert!((e.lat - 38.4).abs() < 1e-9, "lat {}", e.lat);
        assert!((e.lon - -122.4).abs() < 1e-9, "lon {}", e.lon);
        assert_eq!(e.location_precision, LocationPrecision::Admin1);
        // Onset is 06:00 PDT = 13:00 UTC.
        assert_eq!(
            e.ts_utc,
            chrono::Utc.with_ymd_and_hms(2026, 7, 10, 13, 0, 0).unwrap()
        );
        assert_eq!(e.severity, Some(0.75));
        assert_eq!(e.themes, vec!["noaa", "weather", "flood_warning"]);
        assert_eq!(e.headline.as_deref(), Some("Flood Warning"));
        assert_eq!(e.admin1.as_deref(), Some("CA"));
        assert_eq!(e.country_iso, "USA");
        assert_eq!(e.urls.len(), 1);
        use chrono::TimeZone as _;
        assert_eq!(
            e.h3_cell,
            geo_utils::cell_for_latlon(38.4, -122.4, H3_RESOLUTION).unwrap()
        );
    }

    #[test]
    fn zone_scoped_alert_yields_no_event() {
        let mut alert = sample_alert();
        alert["geometry"] = Value::Null;
        assert!(normalize_alert(&alert).unwrap().is_empty());
        alert.as_object_mut().unwrap().remove("geometry");
        assert!(normalize_alert(&alert).unwrap().is_empty());
    }

    #[test]
    fn severity_scale_maps_and_unknown_is_none() {
        let mut alert = sample_alert();
        for (label, expected) in [
            ("Extreme", Some(1.0)),
            ("Severe", Some(0.75)),
            ("Moderate", Some(0.5)),
            ("Minor", Some(0.25)),
            ("Unknown", None),
        ] {
            alert["properties"]["severity"] = json!(label);
            assert_eq!(
                normalize_alert(&alert).unwrap()[0].severity,
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn multipolygon_uses_first_ring() {
        let mut alert = sample_alert();
        alert["geometry"] = json!({
            "type": "MultiPolygon",
            "coordinates": [[[
                [-100.0, 30.0], [-99.0, 30.0], [-99.0, 31.0], [-100.0, 31.0], [-100.0, 30.0]
            ]]]
        });
        let e = &normalize_alert(&alert).unwrap()[0];
        assert!((e.lat - 30.5).abs() < 1e-9);
        assert!((e.lon - -99.5).abs() < 1e-9);
    }

    #[test]
    fn missing_required_properties_fail() {
        for field in ["id", "event", "onset"] {
            let mut alert = sample_alert();
            let props = alert["properties"].as_object_mut().unwrap();
            props.remove(field);
            if field == "onset" {
                // Falls back to effective/sent — remove those too.
                props.remove("effective");
                props.remove("sent");
            }
            assert!(
                normalize_alert(&alert).is_err(),
                "missing `{field}` should fail"
            );
        }
    }

    #[test]
    fn degenerate_ring_yields_no_event() {
        let mut alert = sample_alert();
        alert["geometry"] = json!({
            "type": "Polygon",
            "coordinates": [[[-122.6, 38.2], [-122.6, 38.2]]]
        });
        assert!(normalize_alert(&alert).unwrap().is_empty());
    }
}
