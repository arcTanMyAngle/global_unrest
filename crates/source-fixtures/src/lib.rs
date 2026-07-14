//! Offline fixture source: reads committed JSON fixtures and normalizes them.
//!
//! Fixture files carry a `les-fixture-v1` wrapper with `records` whose
//! `shape` field selects the normalization path:
//! - `gdelt_doc`   — GDELT DOC-style attention observation
//! - `acled_event` — ACLED-style discrete event record
//!
//! All fixture data is synthetic; outlets use reserved `.example` domains.
//! Normalization is fallible **per record** — callers partition failures
//! into `ingest_log` and continue.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, NormalizeError, RawRecord,
    SignalSource, SourceError, SourceFilters, SourceId, TimeWindow, event_id,
};
use serde_json::Value;

pub const FIXTURE_SCHEMA: &str = "les-fixture-v1";

/// Reads fixture JSON files from disk. `fetch` honors the window for records
/// whose timestamp parses; records with broken timestamps pass through so
/// `normalize` can report them into `ingest_log`.
pub struct FixtureSource {
    files: Vec<PathBuf>,
}

impl FixtureSource {
    pub fn from_files(files: Vec<PathBuf>) -> Self {
        Self { files }
    }

    /// Standard fixture layout: every `*.json` directly in `dir` plus
    /// `dir/generated/*.json`.
    pub fn from_dir(dir: &Path) -> std::io::Result<Self> {
        let mut files = Vec::new();
        for d in [dir.to_path_buf(), dir.join("generated")] {
            if !d.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&d)? {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e == "json") {
                    files.push(path);
                }
            }
        }
        files.sort();
        Ok(Self { files })
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    fn load_file(path: &Path) -> Result<Vec<Value>, SourceError> {
        let raw = std::fs::read_to_string(path)?;
        let doc: Value = serde_json::from_str(&raw)
            .map_err(|e| SourceError::Other(format!("{}: invalid JSON: {e}", path.display())))?;
        let schema = doc.get("schema").and_then(Value::as_str).unwrap_or("");
        if schema != FIXTURE_SCHEMA {
            return Err(SourceError::Other(format!(
                "{}: expected schema `{FIXTURE_SCHEMA}`, found `{schema}`",
                path.display()
            )));
        }
        let records = doc
            .get("records")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                SourceError::Other(format!("{}: missing `records` array", path.display()))
            })?;
        Ok(records)
    }
}

impl SignalSource for FixtureSource {
    fn id(&self) -> SourceId {
        SourceId::Fixtures
    }

    async fn fetch(
        &self,
        window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        // Local file reads; fine to do synchronously inside the async fn.
        let mut out = Vec::new();
        for path in &self.files {
            for record in Self::load_file(path)? {
                match record_timestamp(&record) {
                    Some(ts) if !window.contains(ts) => {}
                    // In-window, or unparseable (kept so normalize logs it).
                    _ => out.push(RawRecord::FixtureJson(record)),
                }
            }
        }
        tracing::info!(
            files = self.files.len(),
            records = out.len(),
            "fixtures fetched"
        );
        Ok(out)
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        let RawRecord::FixtureJson(value) = raw else {
            return Err(NormalizeError::InvalidValue {
                field: "record",
                detail: "fixture source received a non-fixture record".into(),
            });
        };
        match value.get("shape").and_then(Value::as_str) {
            Some("gdelt_doc") => normalize_gdelt_doc(value).map(|e| vec![e]),
            Some("acled_event") => normalize_acled_event(value).map(|e| vec![e]),
            Some(other) => Err(NormalizeError::InvalidValue {
                field: "shape",
                detail: format!("unknown shape `{other}`"),
            }),
            None => Err(NormalizeError::MissingField("shape")),
        }
    }
}

/// Best-effort timestamp extraction for window filtering in `fetch`.
fn record_timestamp(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(s) = v.get("seendate").and_then(Value::as_str) {
        return parse_rfc3339(s).ok();
    }
    if let Some(d) = v.get("event_date").and_then(Value::as_str) {
        let t = v
            .get("event_time")
            .and_then(Value::as_str)
            .unwrap_or("00:00:00");
        return parse_date_time(d, t).ok();
    }
    None
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, NormalizeError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| NormalizeError::InvalidValue {
            field: "seendate",
            detail: format!("`{s}`: {e}"),
        })
}

fn parse_date_time(date: &str, time: &str) -> Result<DateTime<Utc>, NormalizeError> {
    let d =
        NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|e| NormalizeError::InvalidValue {
            field: "event_date",
            detail: format!("`{date}`: {e}"),
        })?;
    let t =
        NaiveTime::parse_from_str(time, "%H:%M:%S").map_err(|e| NormalizeError::InvalidValue {
            field: "event_time",
            detail: format!("`{time}`: {e}"),
        })?;
    Ok(Utc.from_utc_datetime(&d.and_time(t)))
}

fn req_str<'v>(v: &'v Value, field: &'static str) -> Result<&'v str, NormalizeError> {
    v.get(field)
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField(field))
}

fn req_f64(v: &Value, field: &'static str) -> Result<f64, NormalizeError> {
    v.get(field)
        .and_then(Value::as_f64)
        .ok_or(NormalizeError::MissingField(field))
}

fn opt_u32(v: &Value, field: &str) -> u32 {
    v.get(field)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

fn str_list(v: &Value, field: &str) -> Vec<String> {
    v.get(field)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn validated_coords(v: &Value) -> Result<(f64, f64), NormalizeError> {
    let lat = req_f64(v, "lat")?;
    let lon = req_f64(v, "lon")?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(NormalizeError::InvalidCoordinates { lat, lon });
    }
    Ok((lat, lon))
}

fn h3_for(lat: f64, lon: f64) -> Result<u64, NormalizeError> {
    geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION).map_err(|e| NormalizeError::InvalidValue {
        field: "lat/lon",
        detail: format!("h3 assignment failed: {e}"),
    })
}

/// GDELT DOC-style attention observation → `NewsAttention` record.
fn normalize_gdelt_doc(v: &Value) -> Result<GeoTemporalEvent, NormalizeError> {
    let source_event_id = req_str(v, "record_id")?.to_owned();
    let ts_utc = parse_rfc3339(req_str(v, "seendate")?)?;
    let (lat, lon) = validated_coords(v)?;
    let (precision, confidence) = match req_str(v, "geo_precision")? {
        "city" => (LocationPrecision::City, 0.85),
        "admin1" => (LocationPrecision::Admin1, 0.6),
        "country" => (LocationPrecision::Country, 0.4),
        other => {
            return Err(NormalizeError::InvalidValue {
                field: "geo_precision",
                detail: format!("`{other}` (expected city|admin1|country)"),
            });
        }
    };
    Ok(GeoTemporalEvent {
        id: event_id(SourceId::Fixtures, &source_event_id),
        source: SourceId::Fixtures,
        source_event_id,
        kind: EventKind::NewsAttention,
        themes: str_list(v, "themes")
            .iter()
            .map(|t| t.to_lowercase())
            .collect(),
        ts_utc,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision: precision,
        location_confidence: confidence,
        country_iso: req_str(v, "country_iso")?.to_owned(),
        admin1: v.get("admin1").and_then(Value::as_str).map(str::to_owned),
        h3_cell: h3_for(lat, lon)?,
        article_count: opt_u32(v, "num_articles"),
        distinct_source_count: opt_u32(v, "num_sources"),
        severity: None,
        headline: v.get("title").and_then(Value::as_str).map(str::to_owned),
        outlet_domains: str_list(v, "domains"),
        urls: str_list(v, "urls"),
    })
}

/// ACLED-style discrete event record.
fn normalize_acled_event(v: &Value) -> Result<GeoTemporalEvent, NormalizeError> {
    let source_event_id = req_str(v, "record_id")?.to_owned();
    let ts_utc = parse_date_time(
        req_str(v, "event_date")?,
        v.get("event_time")
            .and_then(Value::as_str)
            .unwrap_or("12:00:00"),
    )?;
    let (lat, lon) = validated_coords(v)?;
    let kind = match req_str(v, "event_type")? {
        "Protests" | "Riots" => EventKind::Protest,
        "Battles" | "Explosions/Remote violence" | "Violence against civilians" => {
            EventKind::Conflict
        }
        "Strategic developments" => EventKind::Disruption,
        _ => EventKind::Other,
    };
    // ACLED-style numeric precision: 1 = exact/town, 2 = admin area, 3 = region.
    let (precision, confidence) = match v.get("geo_precision").and_then(Value::as_u64) {
        Some(1) => (LocationPrecision::City, 0.9),
        Some(2) => (LocationPrecision::Admin1, 0.65),
        Some(3) => (LocationPrecision::Country, 0.4),
        other => {
            return Err(NormalizeError::InvalidValue {
                field: "geo_precision",
                detail: format!("{other:?} (expected 1..=3)"),
            });
        }
    };
    let fatalities = v.get("fatalities").and_then(Value::as_u64).unwrap_or(0);
    // Transparent, documented mapping: 25+ fatalities saturates severity.
    let severity = ((fatalities as f32) * 0.04).min(1.0);
    Ok(GeoTemporalEvent {
        id: event_id(SourceId::Fixtures, &source_event_id),
        source: SourceId::Fixtures,
        source_event_id,
        kind,
        themes: str_list(v, "tags")
            .iter()
            .map(|t| t.to_lowercase())
            .collect(),
        ts_utc,
        ingested_at: Utc::now(),
        lat,
        lon,
        location_precision: precision,
        location_confidence: confidence,
        country_iso: req_str(v, "country_iso")?.to_owned(),
        admin1: v.get("admin1").and_then(Value::as_str).map(str::to_owned),
        h3_cell: h3_for(lat, lon)?,
        article_count: opt_u32(v, "source_count"),
        distinct_source_count: opt_u32(v, "source_count"),
        severity: Some(severity),
        headline: v
            .get("notes_headline")
            .and_then(Value::as_str)
            .map(str::to_owned),
        outlet_domains: str_list(v, "domains"),
        urls: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn src() -> FixtureSource {
        FixtureSource::from_files(vec![])
    }

    #[test]
    fn golden_gdelt_doc_normalization() {
        let v = json!({
            "shape": "gdelt_doc",
            "record_id": "gdoc-000123",
            "seendate": "2026-05-01T06:15:00Z",
            "themes": ["PROTEST", "ECON_INFLATION"],
            "lat": 48.8566, "lon": 2.3522,
            "geo_precision": "city",
            "country_iso": "FRA",
            "admin1": "Île-de-France",
            "num_articles": 14,
            "num_sources": 6,
            "title": "[synthetic] Rally reported in city center",
            "domains": ["globalwire.example", "daily-ledger.example"],
            "urls": ["https://globalwire.example/a/123"]
        });
        let evs = src().normalize(&RawRecord::FixtureJson(v)).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.id, event_id(SourceId::Fixtures, "gdoc-000123"));
        assert_eq!(e.kind, EventKind::NewsAttention);
        assert_eq!(e.themes, vec!["protest", "econ_inflation"]);
        assert_eq!(
            e.ts_utc,
            Utc.with_ymd_and_hms(2026, 5, 1, 6, 15, 0).unwrap()
        );
        assert_eq!(e.location_precision, LocationPrecision::City);
        assert!((e.location_confidence - 0.85).abs() < 1e-6);
        assert_eq!(e.country_iso, "FRA");
        assert_eq!(e.article_count, 14);
        assert_eq!(e.distinct_source_count, 6);
        assert_eq!(e.severity, None);
        // Paris at res 3 — must be a valid res-3 cell.
        assert_eq!(
            e.h3_cell,
            geo_utils::cell_for_latlon(48.8566, 2.3522, 3).unwrap()
        );
    }

    #[test]
    fn golden_acled_event_normalization() {
        let v = json!({
            "shape": "acled_event",
            "record_id": "aevt-000042",
            "event_date": "2026-05-03",
            "event_time": "13:00:00",
            "event_type": "Riots",
            "sub_event_type": "Violent demonstration",
            "lat": -1.2921, "lon": 36.8219,
            "geo_precision": 1,
            "country_iso": "KEN",
            "admin1": "Nairobi",
            "fatalities": 5,
            "notes_headline": "[synthetic] Demonstration turned violent",
            "source_count": 3,
            "tags": ["Elections"]
        });
        let evs = src().normalize(&RawRecord::FixtureJson(v)).unwrap();
        let e = &evs[0];
        assert_eq!(e.kind, EventKind::Protest);
        assert_eq!(e.location_precision, LocationPrecision::City);
        assert_eq!(e.themes, vec!["elections"]);
        // 5 fatalities * 0.04 = 0.2 (hand-computed).
        assert!((e.severity.unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(e.article_count, 3);
        assert_eq!(e.country_iso, "KEN");
    }

    #[test]
    fn acled_kind_mapping() {
        for (ty, kind) in [
            ("Protests", EventKind::Protest),
            ("Battles", EventKind::Conflict),
            ("Explosions/Remote violence", EventKind::Conflict),
            ("Violence against civilians", EventKind::Conflict),
            ("Strategic developments", EventKind::Disruption),
            ("Something else", EventKind::Other),
        ] {
            let v = json!({
                "shape": "acled_event", "record_id": "x", "event_date": "2026-05-03",
                "event_type": ty, "lat": 0.0, "lon": 0.0, "geo_precision": 2,
                "country_iso": "UNK"
            });
            let evs = src().normalize(&RawRecord::FixtureJson(v)).unwrap();
            assert_eq!(evs[0].kind, kind, "event_type {ty}");
        }
    }

    #[test]
    fn malformed_records_fail_individually() {
        // Missing shape.
        let e = src()
            .normalize(&RawRecord::FixtureJson(json!({"record_id": "x"})))
            .unwrap_err();
        assert!(matches!(e, NormalizeError::MissingField("shape")));

        // Unknown shape.
        let e = src()
            .normalize(&RawRecord::FixtureJson(json!({"shape": "mystery"})))
            .unwrap_err();
        assert!(matches!(
            e,
            NormalizeError::InvalidValue { field: "shape", .. }
        ));

        // Coordinates out of range.
        let v = json!({
            "shape": "gdelt_doc", "record_id": "bad", "seendate": "2026-05-01T00:00:00Z",
            "lat": 999.0, "lon": 0.0, "geo_precision": "city", "country_iso": "UNK"
        });
        let e = src().normalize(&RawRecord::FixtureJson(v)).unwrap_err();
        assert!(matches!(e, NormalizeError::InvalidCoordinates { .. }));
    }

    #[test]
    fn fetch_filters_by_window_and_passes_unparseable_through() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("les-fixture-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.json");
        let doc = json!({
            "schema": FIXTURE_SCHEMA,
            "records": [
                {"shape":"gdelt_doc","record_id":"in","seendate":"2026-05-02T00:00:00Z",
                 "lat":0.0,"lon":0.0,"geo_precision":"city","country_iso":"UNK"},
                {"shape":"gdelt_doc","record_id":"out","seendate":"2026-06-02T00:00:00Z",
                 "lat":0.0,"lon":0.0,"geo_precision":"city","country_iso":"UNK"},
                {"shape":"gdelt_doc","record_id":"broken","seendate":"not-a-date",
                 "lat":0.0,"lon":0.0,"geo_precision":"city","country_iso":"UNK"}
            ]
        });
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(doc.to_string().as_bytes()).unwrap();
        drop(f);

        let source = FixtureSource::from_files(vec![path]);
        let window = TimeWindow::new(
            Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap(),
        );
        let records = futures_block_on(source.fetch(window, &SourceFilters::default()));
        let records = records.unwrap();
        // "in" (in window) + "broken" (unparseable, passed through) = 2.
        assert_eq!(records.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    use chrono::TimeZone;

    /// Minimal executor for the async fetch in tests (the future is ready
    /// immediately; no runtime needed).
    fn futures_block_on<F: Future>(fut: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        let mut fut = pin!(fut);
        loop {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return out;
            }
        }
    }
}
