//! IODA (Internet Outage Detection and Analysis, Georgia Tech Internet
//! Intelligence Research Lab) source adapter — an optional layer.
//!
//! `api.ioda.inetintel.cc.gatech.edu/v2/outages/events` is a keyless public
//! API that detects macroscopic internet outages per country in
//! near-real-time. The live path is **feature-gated behind `live`** like
//! `source-acled`/`source-noaa`; [`normalize_event`] is pure and always
//! compiled.
//!
//! Geometry honesty: IODA gives ISO 3166-1 alpha-2 **country** codes only —
//! no finer geometry. Events normalize at [`LocationPrecision::Country`],
//! so they shade regions on the map and never render as point markers
//! (the same rendering contract every other coarse-precision source
//! follows). The country's centroid comes from the bundled Natural Earth
//! polygon data (a real geometric centroid, computed via `geo::Centroid` in
//! `geo-utils`), never a hand-typed/guessed coordinate; an unknown country
//! code fails normalization rather than guessing one.

#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
pub use live::IodaSource;

use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, LocationRole, NormalizeError,
    SignalFamily, SourceId, event_id,
};
use geo_utils::CountryIndex;
use serde_json::Value;

/// The production outage-events endpoint.
pub const EVENTS_URL: &str = "https://api.ioda.inetintel.cc.gatech.edu/v2/outages/events";

/// Natural Earth 1:110m countries (public domain), bundled so this crate can
/// resolve IODA's ISO alpha-2 country codes to a real geometric centroid
/// without any network access or hand-typed coordinate table.
pub const NE_COUNTRIES: &str =
    include_str!("../../../assets/natural_earth/ne_110m_admin_0_countries.geojson");

/// IODA's `score` is an unbounded anomaly magnitude (observed live range
/// ~700 for a brief blip to ~233,000 for a total national blackout) — squash
/// it onto \[0, 1\] with a log scale anchored to that observed range. Below
/// `IODA_SCORE_FLOOR` reads as the floor (still an outage, but data-thin);
/// at/above `IODA_SCORE_CEIL` saturates at 1.0.
pub mod weights {
    pub const IODA_SCORE_FLOOR: f64 = 100.0;
    pub const IODA_SCORE_CEIL: f64 = 100_000.0;
}

/// `score` → severity in \[0, 1\], log-scale between the named floor/ceil.
pub fn severity_from_score(score: f64) -> f32 {
    let floor = weights::IODA_SCORE_FLOOR;
    let ceil = weights::IODA_SCORE_CEIL;
    let t = (score.max(floor).ln() - floor.ln()) / (ceil.ln() - floor.ln());
    t.clamp(0.0, 1.0) as f32
}

/// Normalize one `data[]` item from the `/outages/events` response.
///
/// `countries` resolves IODA's `country/<alpha-2>` location to a real
/// centroid; an unrecognized code fails normalization (never guessed).
/// Non-country entities (IODA also supports `region`/`asn`/etc., but our own
/// query only ever asks for `entityType=country`) also fail rather than
/// silently coercing to a wrong precision.
pub fn normalize_event(
    v: &Value,
    countries: &CountryIndex,
) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
    let location = v
        .get("location")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField("location"))?;
    let (entity_type, entity_code) =
        location
            .split_once('/')
            .ok_or_else(|| NormalizeError::InvalidValue {
                field: "location",
                detail: format!("expected `type/code`, got `{location}`"),
            })?;
    if entity_type != "country" {
        return Err(NormalizeError::InvalidValue {
            field: "location",
            detail: format!("expected a country entity, got `{entity_type}`"),
        });
    }

    let Some((info, (lon, lat))) = countries.centroid_by_iso_a2(entity_code) else {
        return Err(NormalizeError::InvalidValue {
            field: "location",
            detail: format!("unknown ISO alpha-2 country code `{entity_code}`"),
        });
    };
    let h3_cell = geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).map_err(|e| {
        NormalizeError::InvalidValue {
            field: "location",
            detail: format!("h3 assignment failed: {e}"),
        }
    })?;

    let start = v
        .get("start")
        .and_then(Value::as_i64)
        .ok_or(NormalizeError::MissingField("start"))?;
    let ts_utc =
        chrono::DateTime::from_timestamp(start, 0).ok_or(NormalizeError::InvalidValue {
            field: "start",
            detail: format!("out-of-range unix timestamp `{start}`"),
        })?;
    let duration = v.get("duration").and_then(Value::as_i64).unwrap_or(0);

    let datasource = v
        .get("datasource")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField("datasource"))?;
    let method = v
        .get("method")
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField("method"))?;
    let score = v
        .get("score")
        .and_then(Value::as_f64)
        .ok_or(NormalizeError::MissingField("score"))?;

    let source_event_id = format!("{entity_code}-{start}-{datasource}-{method}");
    let themes = vec![
        "ioda".to_owned(),
        "internet_outage".to_owned(),
        datasource.to_owned(),
        method.to_owned(),
    ];

    let ev = GeoTemporalEvent {
        id: event_id(SourceId::Ioda, &source_event_id),
        source: SourceId::Ioda,
        source_event_id,
        // A measured outage is a discrete disruption that happened at a
        // place, not a measurement series — see docs/SIGNAL_MODEL.md.
        family: SignalFamily::RecordedEvent,
        kind: EventKind::Disruption,
        themes,
        ts_utc,
        ingested_at: chrono::Utc::now(),
        lat,
        lon,
        location_role: LocationRole::EventSite,
        location_precision: LocationPrecision::Country,
        location_confidence: 0.55,
        country_iso: info.iso_a3.clone(),
        admin1: None,
        h3_cell,
        volume_count: 1,
        distinct_source_count: 1,
        severity: Some(severity_from_score(score)),
        headline: Some(format!(
            "Internet connectivity anomaly ({datasource}/{method})"
        )),
        outlet_domains: vec!["ioda.inetintel.cc.gatech.edu".to_owned()],
        urls: vec![format!(
            "https://ioda.inetintel.cc.gatech.edu/country/{entity_code}?from={start}&until={}",
            start + duration
        )],
    };
    ev.validate()?;
    Ok(vec![ev])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::OnceLock;

    fn countries() -> &'static CountryIndex {
        static IDX: OnceLock<CountryIndex> = OnceLock::new();
        IDX.get_or_init(|| CountryIndex::from_geojson_str(NE_COUNTRIES).unwrap())
    }

    /// A real-shaped IODA outage event (values from a live sample this
    /// session; `location_name`/`status`/`fraction`/`overlaps_window`
    /// aren't consumed by normalization but are included for realism).
    fn sample_event() -> Value {
        json!({
            "location": "country/US",
            "location_name": "United States",
            "start": 1754811000,
            "duration": 1800,
            "uncertainty": null,
            "method": "median",
            "datasource": "ping-slash24",
            "status": 0,
            "fraction": null,
            "score": 753.1987405640424,
            "overlaps_window": false
        })
    }

    #[test]
    fn golden_country_event() {
        let evs = normalize_event(&sample_event(), countries()).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(
            e.id,
            event_id(SourceId::Ioda, "US-1754811000-ping-slash24-median")
        );
        assert_eq!(e.source, SourceId::Ioda);
        assert_eq!(e.kind, EventKind::Disruption);
        assert_eq!(e.location_precision, LocationPrecision::Country);
        assert_eq!(e.country_iso, "USA");
        assert_eq!(e.admin1, None);
        assert_eq!(
            e.ts_utc,
            chrono::DateTime::from_timestamp(1_754_811_000, 0).unwrap()
        );
        assert_eq!(
            e.themes,
            vec!["ioda", "internet_outage", "ping-slash24", "median"]
        );
        assert!(e.headline.as_deref().unwrap().contains("ping-slash24"));
        assert_eq!(e.urls.len(), 1);
        assert!(e.urls[0].starts_with("https://ioda.inetintel.cc.gatech.edu/country/US?"));
        // US centroid is somewhere in the continental US.
        assert!((-130.0..=-65.0).contains(&e.lon), "lon {}", e.lon);
        assert!((20.0..=55.0).contains(&e.lat), "lat {}", e.lat);
        let severity = e.severity.unwrap();
        assert!((0.0..=1.0).contains(&severity));
    }

    #[test]
    fn unknown_country_code_fails() {
        let mut event = sample_event();
        event["location"] = json!("country/ZZ");
        assert!(normalize_event(&event, countries()).is_err());
    }

    #[test]
    fn non_country_location_fails() {
        let mut event = sample_event();
        event["location"] = json!("region/3078");
        assert!(normalize_event(&event, countries()).is_err());

        event["location"] = json!("garbage-without-a-slash");
        assert!(normalize_event(&event, countries()).is_err());
    }

    #[test]
    fn missing_required_fields_fail() {
        for field in ["location", "start", "datasource", "method", "score"] {
            let mut event = sample_event();
            event.as_object_mut().unwrap().remove(field);
            assert!(
                normalize_event(&event, countries()).is_err(),
                "missing `{field}` should fail"
            );
        }
    }

    #[test]
    fn severity_score_floors_and_saturates() {
        assert_eq!(severity_from_score(0.0), 0.0);
        assert_eq!(severity_from_score(weights::IODA_SCORE_FLOOR), 0.0);
        assert_eq!(severity_from_score(weights::IODA_SCORE_CEIL), 1.0);
        assert_eq!(severity_from_score(weights::IODA_SCORE_CEIL * 10.0), 1.0);
        // A real sampled midpoint-ish score lands strictly inside (0, 1).
        let mid = severity_from_score(35_159.379);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        // Monotonic: higher score never yields lower severity.
        let mut last = 0.0f32;
        for s in [100.0, 500.0, 2_000.0, 10_000.0, 50_000.0, 100_000.0] {
            let sev = severity_from_score(s);
            assert!(sev >= last, "severity must not decrease: {s} -> {sev}");
            last = sev;
        }
    }
}
