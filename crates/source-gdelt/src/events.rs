//! GDELT Events 2.0 15-minute CSV-zip dumps — the discrete-event path.
//!
//! This is a **separate code path** from the DOC JSON API ([`crate::doc`]).
//! GDELT publishes a new tab-separated Events file every 15 minutes as a
//! single-entry `.CSV.zip`; `http://data.gdeltproject.org/gdeltv2/lastupdate.txt`
//! names the current one. Each row is one CAMEO-coded event with real
//! `ActionGeo` coordinates and a geo type that maps to our precision levels.
//!
//! We keep only rows that are genuine unrest signals — **protests and
//! material-conflict** events (see [`cameo_kind`]) — and skip cooperative and
//! low-grade verbal events rather than flooding the store with them (the brief
//! is about unrest/event signals, and this bounds volume for retention). A
//! skipped row is `Ok(vec![])`, not a failure; a genuinely malformed row is an
//! `Err` the caller writes to `ingest_log`. Nothing is silently dropped.
//!
//! Parsing/normalization/unzip here are pure and offline-testable; only
//! [`crate::GdeltSource::fetch_events`] touches the network.

use std::io::Read;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, NormalizeError, SourceError,
    SourceId, event_id,
};

use crate::country;

/// GDELT 2.0 Events rows have 61 tab-separated columns. Column indices we read
/// (0-based), from the GDELT 2.0 Event codebook:
const COL_GLOBALEVENTID: usize = 0;
const COL_SQLDATE: usize = 1;
const COL_EVENTROOTCODE: usize = 28;
const COL_GOLDSTEIN: usize = 30;
const COL_NUMSOURCES: usize = 32;
const COL_NUMARTICLES: usize = 33;
const COL_ACTIONGEO_TYPE: usize = 51;
const COL_ACTIONGEO_COUNTRYCODE: usize = 53;
const COL_ACTIONGEO_ADM1CODE: usize = 54;
const COL_ACTIONGEO_LAT: usize = 56;
const COL_ACTIONGEO_LONG: usize = 57;
const COL_DATEADDED: usize = 59;
const COL_SOURCEURL: usize = 60;
/// Minimum column count for a well-formed row (indices 0..=60).
const MIN_COLUMNS: usize = 61;

/// One entry named in `lastupdate.txt`: `<size> <md5> <url>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpRef {
    pub size: u64,
    pub md5: String,
    pub url: String,
}

/// Parse `lastupdate.txt` (three whitespace-separated columns per line).
/// Malformed lines are skipped; an entirely unparseable body is an error.
pub fn parse_lastupdate(txt: &str) -> Result<Vec<DumpRef>, SourceError> {
    let mut out = Vec::new();
    for line in txt.lines() {
        let mut parts = line.split_whitespace();
        let (Some(size), Some(md5), Some(url)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(size) = size.parse::<u64>() {
            out.push(DumpRef {
                size,
                md5: md5.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    if out.is_empty() {
        return Err(SourceError::Other(
            "lastupdate.txt named no dump files".into(),
        ));
    }
    Ok(out)
}

/// The Events export URL among the dump refs (`*.export.CSV.zip`).
pub fn export_url(refs: &[DumpRef]) -> Option<&str> {
    refs.iter()
        .map(|r| r.url.as_str())
        .find(|u| u.ends_with(".export.CSV.zip"))
}

/// Decompress a single-entry GDELT `.CSV.zip` to its CSV text. GDELT rows are
/// mostly ASCII but can carry stray bytes in place names, so decode lossily
/// rather than failing the whole dump on one bad byte.
pub fn unzip_csv(bytes: &[u8]) -> Result<String, SourceError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| SourceError::Other(format!("bad zip: {e}")))?;
    if archive.is_empty() {
        return Err(SourceError::Other("empty zip".into()));
    }
    let mut file = archive
        .by_index(0)
        .map_err(|e| SourceError::Other(format!("zip entry: {e}")))?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf).map_err(SourceError::Io)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Non-empty CSV lines (GDELT terminates rows with `\n`; tolerate `\r\n`).
pub fn rows(csv: &str) -> impl Iterator<Item = &str> {
    csv.lines().map(str::trim_end).filter(|l| !l.is_empty())
}

/// CAMEO EventRootCode → our coarse kind, keeping **only unrest signals**:
/// protests (root 14) and material-conflict events (roots 15–20). Cooperative
/// (01–08) and low-grade verbal (09–13) roots return `None` → the row is
/// skipped, not stored (rationale in this module's header).
pub fn cameo_kind(root_code: &str) -> Option<EventKind> {
    match root_code.trim().parse::<u8>().ok()? {
        14 => Some(EventKind::Protest),         // PROTEST
        15 | 16 => Some(EventKind::Disruption), // exhibit force / reduce relations
        17..=20 => Some(EventKind::Conflict),   // coerce / assault / fight / mass violence
        _ => None,                              // cooperation + weak verbal conflict
    }
}

/// ActionGeo type → precision + confidence. Types: 1 COUNTRY, 2 US-state,
/// 3 US-city, 4 world-city, 5 world-state. `None` = ungeocoded (skip the row).
fn geo_precision(type_code: &str) -> Option<(LocationPrecision, f32)> {
    match type_code.trim() {
        "1" => Some((LocationPrecision::Country, 0.4)),
        "2" | "5" => Some((LocationPrecision::Admin1, 0.6)),
        "3" | "4" => Some((LocationPrecision::City, 0.85)),
        _ => None,
    }
}

/// Normalize one Events CSV row.
///
/// Returns `Ok(vec![event])` for a kept unrest event, `Ok(vec![])` for a row
/// that is not an unrest signal or is ungeocoded (skipped, not an error), or
/// `Err` for a genuinely malformed row (too few columns, unparseable id/time,
/// or out-of-range coordinates on a geocoded row).
pub fn normalize(row: &str) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
    let cols: Vec<&str> = row.split('\t').collect();
    if cols.len() < MIN_COLUMNS {
        return Err(NormalizeError::InvalidValue {
            field: "columns",
            detail: format!("{} columns, expected at least {MIN_COLUMNS}", cols.len()),
        });
    }
    let get = |i: usize| cols[i].trim();

    // Unrest filter first: skip non-unrest rows cheaply before any work.
    let Some(kind) = cameo_kind(get(COL_EVENTROOTCODE)) else {
        return Ok(Vec::new());
    };
    // Ungeocoded rows carry no usable location; skip (not a failure).
    let Some((precision, confidence)) = geo_precision(get(COL_ACTIONGEO_TYPE)) else {
        return Ok(Vec::new());
    };

    let global_id = get(COL_GLOBALEVENTID);
    if global_id.is_empty() {
        return Err(NormalizeError::MissingField("globaleventid"));
    }
    let ts_utc =
        parse_dateadded(get(COL_DATEADDED)).or_else(|_| parse_sqldate(get(COL_SQLDATE)))?;
    let (lat, lon) = parse_coords(get(COL_ACTIONGEO_LAT), get(COL_ACTIONGEO_LONG))?;
    let h3_cell = geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).map_err(|e| {
        NormalizeError::InvalidValue {
            field: "actiongeo",
            detail: format!("h3 assignment failed: {e}"),
        }
    })?;

    let source_url = get(COL_SOURCEURL);
    let outlet_domains = domain_of(source_url).into_iter().collect();
    let urls = if source_url.is_empty() {
        Vec::new()
    } else {
        vec![source_url.to_owned()]
    };
    let admin1 = match precision {
        LocationPrecision::Country => None,
        _ => {
            let code = get(COL_ACTIONGEO_ADM1CODE);
            (!code.is_empty()).then(|| code.to_owned())
        }
    };

    Ok(vec![GeoTemporalEvent {
        id: event_id(SourceId::Gdelt, global_id),
        source: SourceId::Gdelt,
        source_event_id: global_id.to_owned(),
        kind,
        themes: Vec::new(), // Events dumps carry no GKG themes.
        ts_utc,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision: precision,
        location_confidence: confidence,
        country_iso: country::iso3_from_fips(get(COL_ACTIONGEO_COUNTRYCODE))
            .unwrap_or("")
            .to_owned(),
        admin1,
        h3_cell,
        article_count: parse_u32(get(COL_NUMARTICLES)),
        distinct_source_count: parse_u32(get(COL_NUMSOURCES)),
        severity: Some(goldstein_severity(get(COL_GOLDSTEIN))),
        headline: None, // titles live in the Mentions/GKG feeds, not Events.
        outlet_domains,
        urls,
    }])
}

/// GDELT `DATEADDED`: 14-digit `YYYYMMDDHHMMSS` UTC.
fn parse_dateadded(s: &str) -> Result<DateTime<Utc>, NormalizeError> {
    NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S")
        .map(|naive| Utc.from_utc_datetime(&naive))
        .map_err(|e| NormalizeError::InvalidValue {
            field: "dateadded",
            detail: format!("`{s}`: {e}"),
        })
}

/// GDELT `SQLDATE`: 8-digit `YYYYMMDD` (date only; midnight UTC).
fn parse_sqldate(s: &str) -> Result<DateTime<Utc>, NormalizeError> {
    NaiveDate::parse_from_str(s, "%Y%m%d")
        .map(|d| Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()))
        .map_err(|e| NormalizeError::InvalidValue {
            field: "sqldate",
            detail: format!("`{s}`: {e}"),
        })
}

fn parse_coords(lat: &str, lon: &str) -> Result<(f64, f64), NormalizeError> {
    let lat: f64 = lat.parse().map_err(|_| NormalizeError::InvalidValue {
        field: "actiongeo_lat",
        detail: format!("`{lat}`"),
    })?;
    let lon: f64 = lon.parse().map_err(|_| NormalizeError::InvalidValue {
        field: "actiongeo_long",
        detail: format!("`{lon}`"),
    })?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(NormalizeError::InvalidCoordinates { lat, lon });
    }
    Ok((lat, lon))
}

fn parse_u32(s: &str) -> u32 {
    s.parse::<u32>().unwrap_or(0)
}

/// Severity from the Goldstein scale (−10 hostile .. +10 cooperative): map the
/// hostile half to [0, 1]; cooperative/positive scores give 0. Documented,
/// transparent, and monotonic (docs/SCORING.md).
fn goldstein_severity(s: &str) -> f32 {
    let g: f32 = s.parse().unwrap_or(0.0);
    (-g / 10.0).clamp(0.0, 1.0)
}

/// Registrable-ish host of a URL, used as the single outlet domain. Best
/// effort: no host ⇒ no outlet.
fn domain_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|u| {
        u.host_str()
            .map(|h| h.trim_start_matches("www.").to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 61-column Events row with only the fields we read populated.
    fn row(fields: &[(usize, &str)]) -> String {
        let mut cols = vec![""; MIN_COLUMNS];
        for (i, v) in fields {
            cols[*i] = v;
        }
        cols.join("\t")
    }

    fn protest_row() -> String {
        row(&[
            (COL_GLOBALEVENTID, "1000001"),
            (COL_SQLDATE, "20260620"),
            (COL_EVENTROOTCODE, "14"),
            (COL_GOLDSTEIN, "-6.5"),
            (COL_NUMSOURCES, "6"),
            (COL_NUMARTICLES, "14"),
            (COL_ACTIONGEO_TYPE, "3"),
            (COL_ACTIONGEO_COUNTRYCODE, "FR"),
            (COL_ACTIONGEO_ADM1CODE, "FR11"),
            (COL_ACTIONGEO_LAT, "48.8566"),
            (COL_ACTIONGEO_LONG, "2.3522"),
            (COL_DATEADDED, "20260620081500"),
            (COL_SOURCEURL, "https://globalwire.example/a/1"),
        ])
    }

    #[test]
    fn normalizes_a_protest_row() {
        let evs = normalize(&protest_row()).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.source, SourceId::Gdelt);
        assert_eq!(e.source_event_id, "1000001");
        assert_eq!(e.id, event_id(SourceId::Gdelt, "1000001"));
        assert_eq!(e.kind, EventKind::Protest);
        assert_eq!(e.location_precision, LocationPrecision::City);
        assert!(e.location_precision.renders_as_point());
        assert_eq!(e.country_iso, "FRA");
        assert_eq!(e.admin1.as_deref(), Some("FR11"));
        assert_eq!(e.article_count, 14);
        assert_eq!(e.distinct_source_count, 6);
        assert_eq!(e.outlet_domains, vec!["globalwire.example"]);
        assert_eq!(e.urls, vec!["https://globalwire.example/a/1"]);
        assert!(e.themes.is_empty());
        // −6.5 Goldstein → 0.65 severity.
        assert!((e.severity.unwrap() - 0.65).abs() < 1e-6);
        assert_eq!(
            e.ts_utc,
            Utc.with_ymd_and_hms(2026, 6, 20, 8, 15, 0).unwrap()
        );
        assert_eq!(
            e.h3_cell,
            geo_utils::cell_for_latlon(48.8566, 2.3522, H3_RESOLUTION).unwrap()
        );
    }

    #[test]
    fn cameo_mapping_covers_kept_and_skipped_roots() {
        assert_eq!(cameo_kind("14"), Some(EventKind::Protest));
        assert_eq!(cameo_kind("15"), Some(EventKind::Disruption));
        assert_eq!(cameo_kind("16"), Some(EventKind::Disruption));
        assert_eq!(cameo_kind("17"), Some(EventKind::Conflict));
        assert_eq!(cameo_kind("18"), Some(EventKind::Conflict));
        assert_eq!(cameo_kind("19"), Some(EventKind::Conflict));
        assert_eq!(cameo_kind("20"), Some(EventKind::Conflict));
        // Cooperation and weak verbal conflict are not unrest signals.
        for r in ["01", "04", "08", "09", "11", "13"] {
            assert_eq!(cameo_kind(r), None, "root {r} should be skipped");
        }
        assert_eq!(cameo_kind("not-a-code"), None);
    }

    #[test]
    fn cooperation_row_is_skipped_not_failed() {
        let r = row(&[
            (COL_GLOBALEVENTID, "1000004"),
            (COL_EVENTROOTCODE, "04"), // consult (cooperation)
            (COL_ACTIONGEO_TYPE, "1"),
            (COL_ACTIONGEO_COUNTRYCODE, "US"),
            (COL_ACTIONGEO_LAT, "39.5"),
            (COL_ACTIONGEO_LONG, "-98.35"),
            (COL_DATEADDED, "20260620081500"),
        ]);
        assert!(normalize(&r).unwrap().is_empty());
    }

    #[test]
    fn ungeocoded_row_is_skipped() {
        let r = row(&[
            (COL_GLOBALEVENTID, "1000007"),
            (COL_EVENTROOTCODE, "19"),
            (COL_ACTIONGEO_TYPE, ""), // no geo
            (COL_DATEADDED, "20260620081500"),
        ]);
        assert!(normalize(&r).unwrap().is_empty());
    }

    #[test]
    fn country_precision_row_has_no_admin1() {
        let r = row(&[
            (COL_GLOBALEVENTID, "1000005"),
            (COL_EVENTROOTCODE, "18"),
            (COL_ACTIONGEO_TYPE, "1"),
            (COL_ACTIONGEO_COUNTRYCODE, "RS"),
            (COL_ACTIONGEO_ADM1CODE, "RS00"),
            (COL_ACTIONGEO_LAT, "61.524"),
            (COL_ACTIONGEO_LONG, "105.3188"),
            (COL_DATEADDED, "20260620110500"),
        ]);
        let e = &normalize(&r).unwrap()[0];
        assert_eq!(e.kind, EventKind::Conflict);
        assert_eq!(e.location_precision, LocationPrecision::Country);
        assert!(!e.location_precision.renders_as_point());
        assert_eq!(e.country_iso, "RUS");
        assert_eq!(e.admin1, None);
    }

    #[test]
    fn malformed_rows_fail_per_record() {
        // Too few columns.
        assert!(matches!(
            normalize("just\tthree\tcols").unwrap_err(),
            NormalizeError::InvalidValue {
                field: "columns",
                ..
            }
        ));

        // Out-of-range coordinates on a geocoded, kept row.
        let bad = row(&[
            (COL_GLOBALEVENTID, "1000006"),
            (COL_EVENTROOTCODE, "19"),
            (COL_ACTIONGEO_TYPE, "3"),
            (COL_ACTIONGEO_COUNTRYCODE, "FR"),
            (COL_ACTIONGEO_LAT, "999.0"),
            (COL_ACTIONGEO_LONG, "2.0"),
            (COL_DATEADDED, "20260620081500"),
        ]);
        assert!(matches!(
            normalize(&bad).unwrap_err(),
            NormalizeError::InvalidCoordinates { .. }
        ));
    }

    #[test]
    fn goldstein_severity_maps_hostile_half() {
        assert!((goldstein_severity("-10.0") - 1.0).abs() < 1e-6);
        assert!((goldstein_severity("-5.0") - 0.5).abs() < 1e-6);
        assert!(goldstein_severity("3.0").abs() < 1e-6); // cooperative ⇒ 0
        assert!(goldstein_severity("").abs() < 1e-6);
    }

    #[test]
    fn parse_lastupdate_and_export_selection() {
        let txt = "\
217053 f7e5…a1 http://data.gdeltproject.org/gdeltv2/20260620081500.export.CSV.zip
64221 9c2b…ff http://data.gdeltproject.org/gdeltv2/20260620081500.mentions.CSV.zip
1043887 3ab0…12 http://data.gdeltproject.org/gdeltv2/20260620081500.gkg.csv.zip";
        let refs = parse_lastupdate(txt).unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].size, 217053);
        assert_eq!(
            export_url(&refs),
            Some("http://data.gdeltproject.org/gdeltv2/20260620081500.export.CSV.zip")
        );
    }

    #[test]
    fn empty_lastupdate_errors() {
        assert!(parse_lastupdate("\n\n").is_err());
    }

    #[test]
    fn unzip_roundtrips_a_deflated_csv() {
        let csv = format!("{}\n{}\n", protest_row(), protest_row());
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("20260620081500.export.CSV", opts).unwrap();
            std::io::Write::write_all(&mut w, csv.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        let back = unzip_csv(&buf).unwrap();
        assert_eq!(back, csv);
        assert_eq!(rows(&back).count(), 2);
    }
}
