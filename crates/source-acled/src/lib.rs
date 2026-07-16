//! ACLED source adapter (M5). Live, authorized ingestion of discrete
//! political-violence / demonstration events.
//!
//! Access requires a registered myACLED account and is **feature-gated behind
//! `live`** — the network path compiles out by default. ACLED retired API
//! keys in 2025: authentication is an OAuth password grant
//! (`ACLED_EMAIL` / `ACLED_PASSWORD` env vars, read by the binaries) that
//! yields a short-lived bearer token; data comes from the windowed `read`
//! endpoint as JSON pages (see [`live::AcledSource`]).
//!
//! Authorized use only (docs/SAFETY_AND_PRIVACY.md): requests are
//! rate-limited and paged politely, results require ACLED attribution, and
//! raw rows are never redistributed — normalization deliberately does **not**
//! store the `notes` narrative, only structural metadata.
//!
//! [`normalize_event`] is pure and always compiled, so golden tests run
//! offline with no feature flags; only the `live` module touches the network.

pub mod iso3;

#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
pub use live::AcledSource;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, NormalizeError, SourceId,
    event_id,
};
use serde_json::Value;

/// The production OAuth token endpoint (password + refresh grants).
pub const TOKEN_URL: &str = "https://acleddata.com/oauth/token";

/// The production windowed read endpoint.
pub const READ_URL: &str = "https://acleddata.com/api/acled/read";

/// Rows requested per page — ACLED's documented per-request maximum.
pub const PAGE_LIMIT: u32 = 5000;

/// Hard cap on pages fetched per window so one cycle can never run away.
pub const MAX_PAGES: u32 = 10;

/// Normalize one ACLED row (JSON object) → one discrete event.
///
/// Field reference: `event_id_cnty` (stable id), `event_date` (date only —
/// ACLED publishes no time of day, so events land at 12:00 UTC, the same
/// convention as the fixture source), `event_type`/`sub_event_type`,
/// `disorder_type`, `latitude`/`longitude` + `geo_precision` (1 = exact/town,
/// 2 = admin area, 3 = region), numeric `iso`, `admin1`, `fatalities`,
/// semicolon-separated `source`. ACLED serializes most values as strings;
/// numeric fields accept either representation.
///
/// The `notes` narrative is intentionally not read.
pub fn normalize_event(v: &Value) -> Result<GeoTemporalEvent, NormalizeError> {
    let source_event_id = req_str(v, "event_id_cnty")?.to_owned();
    let ts_utc = parse_event_date(req_str(v, "event_date")?)?;

    let lat = req_f64(v, "latitude")?;
    let lon = req_f64(v, "longitude")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(NormalizeError::InvalidCoordinates { lat, lon });
    }
    let h3_cell = geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).map_err(|e| {
        NormalizeError::InvalidValue {
            field: "latitude/longitude",
            detail: format!("h3 assignment failed: {e}"),
        }
    })?;

    let event_type = req_str(v, "event_type")?;
    // Same taxonomy mapping the fixture source golden-tests
    // (source-fixtures::normalize_acled_event); keep the two in sync.
    let kind = match event_type {
        "Protests" | "Riots" => EventKind::Protest,
        "Battles" | "Explosions/Remote violence" | "Violence against civilians" => {
            EventKind::Conflict
        }
        "Strategic developments" => EventKind::Disruption,
        _ => EventKind::Other,
    };

    // Same precision/confidence mapping as the fixture source.
    let (location_precision, location_confidence) = match req_u64_lenient(v, "geo_precision")? {
        1 => (LocationPrecision::City, 0.9),
        2 => (LocationPrecision::Admin1, 0.65),
        3 => (LocationPrecision::Country, 0.4),
        other => {
            return Err(NormalizeError::InvalidValue {
                field: "geo_precision",
                detail: format!("{other} (expected 1..=3)"),
            });
        }
    };

    // Numeric ISO → alpha-3. Codes outside ISO 3166-1 (e.g. ACLED's 0 for
    // Kosovo) keep the authoritative coordinates and an empty country_iso
    // rather than a guess — same policy as the GDELT Events FIPS path.
    let country_iso = match v.get("iso").and_then(u64_lenient) {
        Some(n) if n <= u64::from(u16::MAX) => iso3::alpha3(n as u16).unwrap_or("").to_owned(),
        _ => String::new(),
    };

    let sub_event_type = v
        .get("sub_event_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Themes: taxonomy slugs (provenance + filterable), never free text.
    let mut themes = vec!["acled".to_owned()];
    for label in [
        v.get("disorder_type").and_then(Value::as_str),
        Some(event_type),
        sub_event_type,
    ]
    .into_iter()
    .flatten()
    {
        let slug = slugify(label);
        if !slug.is_empty() && !themes.contains(&slug) {
            themes.push(slug);
        }
    }

    let fatalities = v.get("fatalities").and_then(u64_lenient).unwrap_or(0);
    // Transparent mapping shared with the fixture source: 25+ fatalities
    // saturates severity.
    let severity = ((fatalities as f32) * 0.04).min(1.0);

    // `source` is a semicolon-separated list of reporting-source names —
    // attribution metadata, kept for the diversity signal.
    let sources: Vec<String> = v
        .get("source")
        .and_then(Value::as_str)
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let source_count = sources.len().min(u32::MAX as usize) as u32;

    Ok(GeoTemporalEvent {
        id: event_id(SourceId::Acled, &source_event_id),
        source: SourceId::Acled,
        source_event_id,
        kind,
        themes,
        ts_utc,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision,
        location_confidence,
        country_iso,
        admin1: v
            .get("admin1")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        h3_cell,
        article_count: source_count,
        distinct_source_count: source_count,
        severity: Some(severity),
        // Structural label only (e.g. "Armed clash") — never the `notes` body.
        headline: Some(sub_event_type.unwrap_or(event_type).to_owned()),
        outlet_domains: sources,
        urls: Vec::new(),
    })
}

/// ACLED `event_date` is a date; events land at 12:00 UTC (fixture convention).
fn parse_event_date(date: &str) -> Result<DateTime<Utc>, NormalizeError> {
    let d =
        NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|e| NormalizeError::InvalidValue {
            field: "event_date",
            detail: format!("`{date}`: {e}"),
        })?;
    let noon = NaiveTime::from_hms_opt(12, 0, 0).expect("valid constant time");
    Ok(Utc.from_utc_datetime(&d.and_time(noon)))
}

/// Lowercase alphanumeric slug: `"Explosions/Remote violence"` →
/// `"explosions_remote_violence"`.
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

fn req_str<'v>(v: &'v Value, field: &'static str) -> Result<&'v str, NormalizeError> {
    v.get(field)
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField(field))
}

/// ACLED serializes numbers as strings in some formats; accept both.
fn f64_lenient(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn u64_lenient(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn req_f64(v: &Value, field: &'static str) -> Result<f64, NormalizeError> {
    v.get(field)
        .and_then(f64_lenient)
        .ok_or(NormalizeError::MissingField(field))
}

fn req_u64_lenient(v: &Value, field: &'static str) -> Result<u64, NormalizeError> {
    v.get(field)
        .and_then(u64_lenient)
        .ok_or(NormalizeError::MissingField(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A real-shaped ACLED row (string-typed values, as the API serializes).
    /// Content is synthetic.
    fn sample_row() -> Value {
        json!({
            "event_id_cnty": "YEM73601",
            "event_date": "2026-07-10",
            "year": "2026",
            "time_precision": "1",
            "disorder_type": "Political violence",
            "event_type": "Battles",
            "sub_event_type": "Armed clash",
            "actor1": "[synthetic] Force A",
            "actor2": "[synthetic] Force B",
            "iso": "887",
            "region": "Middle East",
            "country": "Yemen",
            "admin1": "Marib",
            "admin2": "Sirwah",
            "location": "Sirwah",
            "latitude": "15.4689",
            "longitude": "45.3247",
            "geo_precision": "1",
            "source": "Outlet One; Outlet Two",
            "source_scale": "National",
            "notes": "[synthetic] A narrative description that must never be stored.",
            "fatalities": "12",
            "timestamp": "1783720000"
        })
    }

    #[test]
    fn golden_battle_row() {
        let e = normalize_event(&sample_row()).unwrap();
        assert_eq!(e.id, event_id(SourceId::Acled, "YEM73601"));
        assert_eq!(e.source, SourceId::Acled);
        assert_eq!(e.source_event_id, "YEM73601");
        assert_eq!(e.kind, EventKind::Conflict);
        assert_eq!(
            e.ts_utc,
            Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap()
        );
        assert_eq!(e.location_precision, LocationPrecision::City);
        assert!((e.location_confidence - 0.9).abs() < 1e-6);
        assert_eq!(e.country_iso, "YEM");
        assert_eq!(e.admin1.as_deref(), Some("Marib"));
        assert_eq!(
            e.h3_cell,
            geo_utils::cell_for_latlon(15.4689, 45.3247, H3_RESOLUTION).unwrap()
        );
        // 12 fatalities × 0.04 = 0.48.
        assert!((e.severity.unwrap() - 0.48).abs() < 1e-6);
        assert_eq!(
            e.themes,
            vec!["acled", "political_violence", "battles", "armed_clash"]
        );
        assert_eq!(e.headline.as_deref(), Some("Armed clash"));
        assert_eq!(e.outlet_domains, vec!["Outlet One", "Outlet Two"]);
        assert_eq!(e.article_count, 2);
        assert_eq!(e.distinct_source_count, 2);
        assert!(e.urls.is_empty());
    }

    #[test]
    fn notes_narrative_is_never_stored() {
        let e = normalize_event(&sample_row()).unwrap();
        let debug = format!("{e:?}");
        assert!(
            !debug.contains("narrative description"),
            "notes leaked into the normalized event: {debug}"
        );
    }

    #[test]
    fn kind_mapping_covers_the_acled_taxonomy() {
        let mut row = sample_row();
        for (event_type, kind) in [
            ("Protests", EventKind::Protest),
            ("Riots", EventKind::Protest),
            ("Battles", EventKind::Conflict),
            ("Explosions/Remote violence", EventKind::Conflict),
            ("Violence against civilians", EventKind::Conflict),
            ("Strategic developments", EventKind::Disruption),
            ("Something new", EventKind::Other),
        ] {
            row["event_type"] = json!(event_type);
            assert_eq!(normalize_event(&row).unwrap().kind, kind, "{event_type}");
        }
    }

    #[test]
    fn geo_precision_maps_and_rejects() {
        let mut row = sample_row();
        row["geo_precision"] = json!("2");
        let e = normalize_event(&row).unwrap();
        assert_eq!(e.location_precision, LocationPrecision::Admin1);
        assert!((e.location_confidence - 0.65).abs() < 1e-6);

        // Native JSON numbers are accepted too.
        row["geo_precision"] = json!(3);
        let e = normalize_event(&row).unwrap();
        assert_eq!(e.location_precision, LocationPrecision::Country);

        row["geo_precision"] = json!("7");
        assert!(matches!(
            normalize_event(&row).unwrap_err(),
            NormalizeError::InvalidValue {
                field: "geo_precision",
                ..
            }
        ));
    }

    #[test]
    fn numeric_json_values_are_accepted() {
        let mut row = sample_row();
        row["latitude"] = json!(15.4689);
        row["longitude"] = json!(45.3247);
        row["iso"] = json!(887);
        row["fatalities"] = json!(12);
        let e = normalize_event(&row).unwrap();
        assert_eq!(e.country_iso, "YEM");
        assert!((e.severity.unwrap() - 0.48).abs() < 1e-6);
    }

    #[test]
    fn severity_saturates_at_25_fatalities() {
        let mut row = sample_row();
        row["fatalities"] = json!("60");
        assert_eq!(normalize_event(&row).unwrap().severity, Some(1.0));
        row["fatalities"] = json!("0");
        assert_eq!(normalize_event(&row).unwrap().severity, Some(0.0));
    }

    #[test]
    fn out_of_range_coordinates_fail() {
        let mut row = sample_row();
        row["latitude"] = json!("95.0");
        assert!(matches!(
            normalize_event(&row).unwrap_err(),
            NormalizeError::InvalidCoordinates { .. }
        ));
    }

    #[test]
    fn missing_required_fields_fail() {
        for field in ["event_id_cnty", "event_date", "event_type", "latitude"] {
            let mut row = sample_row();
            row.as_object_mut().unwrap().remove(field);
            assert!(
                normalize_event(&row).is_err(),
                "missing `{field}` should fail"
            );
        }
    }

    #[test]
    fn unknown_iso_keeps_coords_and_empty_country() {
        // ACLED uses iso 0 for Kosovo (no ISO 3166-1 assignment).
        let mut row = sample_row();
        row["iso"] = json!("0");
        let e = normalize_event(&row).unwrap();
        assert_eq!(e.country_iso, "");
        assert!((e.lat - 15.4689).abs() < 1e-9);
    }

    #[test]
    fn slugify_flattens_punctuation() {
        assert_eq!(
            slugify("Explosions/Remote violence"),
            "explosions_remote_violence"
        );
        assert_eq!(slugify("Battles"), "battles");
        assert_eq!(slugify("  "), "");
    }
}
