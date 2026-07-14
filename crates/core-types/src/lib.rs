//! Core domain types shared by every crate: events, windows, buckets, sources.
//!
//! This crate does no I/O. Shared domain semantics that both `analytics` and
//! `storage` must agree on (H3 resolution, time-bucket size) live here so
//! neither depends on the other.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Canonical H3 resolution for region keys. Parents are derived, never stored.
pub const H3_RESOLUTION: u8 = 3;

/// Region-bucket width in seconds (6 hours). See docs/SCORING.md for why.
pub const BUCKET_SECS: i64 = 6 * 3600;

/// Floor an epoch-seconds timestamp to its bucket start.
pub fn bucket_start_epoch(epoch_secs: i64) -> i64 {
    epoch_secs.div_euclid(BUCKET_SECS) * BUCKET_SECS
}

/// Deterministic FNV-1a 64-bit hash. Used for stable event ids so that
/// re-ingesting the same source record is idempotent. Never use `std`'s
/// default hasher here: it is randomly seeded per process.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Stable event id from source + source-native id.
pub fn event_id(source: SourceId, source_event_id: &str) -> u64 {
    let mut buf = Vec::with_capacity(source_event_id.len() + 8);
    buf.extend_from_slice(source.as_str().as_bytes());
    buf.push(0);
    buf.extend_from_slice(source_event_id.as_bytes());
    fnv1a64(&buf)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    Fixtures,
    Gdelt,
    Acled,
}

impl SourceId {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::Fixtures => "fixtures",
            SourceId::Gdelt => "gdelt",
            SourceId::Acled => "acled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fixtures" => Some(SourceId::Fixtures),
            "gdelt" => Some(SourceId::Gdelt),
            "acled" => Some(SourceId::Acled),
            _ => None,
        }
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Coarse cross-source event taxonomy.
///
/// `NewsAttention` records are attention *observations* (how much coverage a
/// place got in a window), not discrete real-world events. Scoring and the UI
/// keep the two classes separate; see docs/DATA_MODEL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    NewsAttention,
    Protest,
    Conflict,
    Disruption,
    Other,
}

impl EventKind {
    pub const ALL: [EventKind; 5] = [
        EventKind::NewsAttention,
        EventKind::Protest,
        EventKind::Conflict,
        EventKind::Disruption,
        EventKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::NewsAttention => "news_attention",
            EventKind::Protest => "protest",
            EventKind::Conflict => "conflict",
            EventKind::Disruption => "disruption",
            EventKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "news_attention" => Some(EventKind::NewsAttention),
            "protest" => Some(EventKind::Protest),
            "conflict" => Some(EventKind::Conflict),
            "disruption" => Some(EventKind::Disruption),
            "other" => Some(EventKind::Other),
            _ => None,
        }
    }

    /// Attention observation (media coverage signal)?
    pub fn is_attention(self) -> bool {
        matches!(self, EventKind::NewsAttention)
    }

    /// Discrete real-world event record?
    pub fn is_discrete_event(self) -> bool {
        !self.is_attention()
    }

    /// Human-readable label for UI.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::NewsAttention => "News attention",
            EventKind::Protest => "Protest",
            EventKind::Conflict => "Conflict",
            EventKind::Disruption => "Disruption",
            EventKind::Other => "Other",
        }
    }
}

/// How precisely the source geocoded this record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationPrecision {
    Country,
    Admin1,
    City,
    Exact,
}

impl LocationPrecision {
    pub fn as_str(self) -> &'static str {
        match self {
            LocationPrecision::Country => "country",
            LocationPrecision::Admin1 => "admin1",
            LocationPrecision::City => "city",
            LocationPrecision::Exact => "exact",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "country" => Some(LocationPrecision::Country),
            "admin1" => Some(LocationPrecision::Admin1),
            "city" => Some(LocationPrecision::City),
            "exact" => Some(LocationPrecision::Exact),
            _ => None,
        }
    }

    /// The precision rendering contract (docs/DATA_MODEL.md): only records
    /// geocoded to at least city level may render as point markers. Coarser
    /// records contribute to region shading only, so country centroids never
    /// appear as fake hotspots.
    pub fn renders_as_point(self) -> bool {
        matches!(self, LocationPrecision::City | LocationPrecision::Exact)
    }
}

/// The single normalized record every source adapter produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeoTemporalEvent {
    pub id: u64,
    pub source: SourceId,
    pub source_event_id: String,
    pub kind: EventKind,
    pub themes: Vec<String>,
    pub ts_utc: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub lat: f64,
    pub lon: f64,
    pub location_precision: LocationPrecision,
    /// 0.0–1.0
    pub location_confidence: f32,
    /// ISO 3166-1 alpha-3.
    pub country_iso: String,
    pub admin1: Option<String>,
    /// H3 cell at [`H3_RESOLUTION`], stored as the raw u64 index.
    pub h3_cell: u64,
    pub article_count: u32,
    pub distinct_source_count: u32,
    /// 0.0–1.0 when the source provides one.
    pub severity: Option<f32>,
    /// Metadata only — never article bodies (docs/SAFETY_AND_PRIVACY.md).
    pub headline: Option<String>,
    pub outlet_domains: Vec<String>,
    pub urls: Vec<String>,
}

/// Half-open UTC time window `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeWindow {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self { start, end }
    }

    pub fn contains(&self, ts: DateTime<Utc>) -> bool {
        ts >= self.start && ts < self.end
    }

    pub fn duration_secs(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }
}

/// Aggregate for one (H3 res-3 cell, 6-hour bucket). M1 carries raw counts;
/// M2 adds the score components (stored separately, shown separately).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RegionBucket {
    pub h3_cell: u64,
    /// Bucket start, epoch seconds, floored with [`bucket_start_epoch`].
    pub bucket_start: i64,
    /// Discrete event records (protest/conflict/disruption/other).
    pub event_count: u32,
    /// News-attention observations.
    pub attention_count: u32,
    /// Sum of article counts across all records.
    pub article_count: u64,
    /// Sum of per-record distinct outlet counts (an upper bound on true
    /// distinct outlets; exact de-duplication needs raw outlet sets).
    pub source_count: u64,
}

/// Filters a caller passes to `SignalSource::fetch`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceFilters {
    /// Restrict to these kinds; `None` = all.
    pub kinds: Option<Vec<EventKind>>,
    /// Substring-match theme filter; `None` = all.
    pub themes: Option<Vec<String>>,
    /// Drop records below this location confidence.
    pub min_location_confidence: Option<f32>,
}

/// Raw, source-shaped payload prior to normalization. Self-contained (no
/// per-source crate types) so `core-types` stays at the bottom of the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RawRecord {
    /// One record from a committed fixture file.
    FixtureJson(serde_json::Value),
    /// One article/attention record from the GDELT DOC 2.0 JSON API (M3).
    GdeltDocJson(serde_json::Value),
    /// One CSV row from a GDELT Events 15-minute dump (M3).
    GdeltEventCsv(String),
    /// One event record from the ACLED API (M5, authorized access only).
    AcledJson(serde_json::Value),
}

impl RawRecord {
    /// Short excerpt for `ingest_log` (bounded so the log stays small).
    pub fn excerpt(&self, max_len: usize) -> String {
        let full = match self {
            RawRecord::FixtureJson(v) | RawRecord::GdeltDocJson(v) | RawRecord::AcledJson(v) => {
                v.to_string()
            }
            RawRecord::GdeltEventCsv(s) => s.clone(),
        };
        let mut cut = full;
        if cut.len() > max_len {
            let mut end = max_len;
            while !cut.is_char_boundary(end) {
                end -= 1;
            }
            cut.truncate(end);
            cut.push('…');
        }
        cut
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("http error: {0}")]
    Http(String),
    #[error("rate limited; retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("source not implemented until milestone {milestone}")]
    NotImplemented { milestone: &'static str },
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("missing field `{0}`")]
    MissingField(&'static str),
    #[error("invalid value for `{field}`: {detail}")]
    InvalidValue { field: &'static str, detail: String },
    #[error("coordinates out of range: lat={lat}, lon={lon}")]
    InvalidCoordinates { lat: f64, lon: f64 },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A record that failed normalization, destined for `ingest_log`.
/// Failures are recorded, never silently dropped.
#[derive(Debug)]
pub struct IngestFailure {
    pub source: SourceId,
    pub reason: String,
    pub raw_excerpt: String,
    pub occurred_at: DateTime<Utc>,
}

/// A source adapter. The set of sources is closed, so callers use concrete
/// types or a small enum wrapper rather than trait objects (`async fn` in
/// traits is not dyn-safe, and we don't need it to be).
#[allow(async_fn_in_trait)]
pub trait SignalSource {
    fn id(&self) -> SourceId;

    /// Fetch raw records for a window. Live adapters must respect source
    /// rate limits and terms; fixtures resolve immediately.
    async fn fetch(
        &self,
        window: TimeWindow,
        filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError>;

    /// Normalize one raw record. Fallible per record: callers partition
    /// failures into `ingest_log` and continue.
    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fnv1a64_matches_known_vectors() {
        // Published FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn event_id_is_deterministic_and_source_scoped() {
        let a = event_id(SourceId::Fixtures, "evt-1");
        let b = event_id(SourceId::Fixtures, "evt-1");
        let c = event_id(SourceId::Gdelt, "evt-1");
        assert_eq!(a, b);
        assert_ne!(
            a, c,
            "same source-native id from different sources must differ"
        );
    }

    #[test]
    fn bucket_floors_to_six_hours() {
        // 2026-01-02 07:30:00 UTC floors to 06:00.
        let ts = Utc.with_ymd_and_hms(2026, 1, 2, 7, 30, 0).unwrap();
        let floored = bucket_start_epoch(ts.timestamp());
        let expect = Utc.with_ymd_and_hms(2026, 1, 2, 6, 0, 0).unwrap();
        assert_eq!(floored, expect.timestamp());
        // Negative epochs (pre-1970) still floor downward.
        assert_eq!(bucket_start_epoch(-1), -BUCKET_SECS);
    }

    #[test]
    fn time_window_is_half_open() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let w = TimeWindow::new(start, end);
        assert!(w.contains(start));
        assert!(!w.contains(end));
        assert_eq!(w.duration_secs(), 86_400);
    }

    #[test]
    fn kind_and_precision_string_roundtrip() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(k));
        }
        for p in [
            LocationPrecision::Country,
            LocationPrecision::Admin1,
            LocationPrecision::City,
            LocationPrecision::Exact,
        ] {
            assert_eq!(LocationPrecision::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn precision_rendering_contract() {
        assert!(!LocationPrecision::Country.renders_as_point());
        assert!(!LocationPrecision::Admin1.renders_as_point());
        assert!(LocationPrecision::City.renders_as_point());
        assert!(LocationPrecision::Exact.renders_as_point());
    }

    #[test]
    fn geo_temporal_event_serde_roundtrip() {
        let ev = GeoTemporalEvent {
            id: event_id(SourceId::Fixtures, "evt-42"),
            source: SourceId::Fixtures,
            source_event_id: "evt-42".into(),
            kind: EventKind::Protest,
            themes: vec!["labor".into()],
            ts_utc: Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap(),
            ingested_at: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            lat: 48.85,
            lon: 2.35,
            location_precision: LocationPrecision::City,
            location_confidence: 0.9,
            country_iso: "FRA".into(),
            admin1: Some("Île-de-France".into()),
            h3_cell: 0x83_1f_b4_ff_ff_ff_ff,
            article_count: 12,
            distinct_source_count: 5,
            severity: Some(0.3),
            headline: Some("Synthetic headline".into()),
            outlet_domains: vec!["globalwire.example".into()],
            urls: vec!["https://globalwire.example/a/1".into()],
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: GeoTemporalEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn raw_record_excerpt_is_bounded_and_utf8_safe() {
        let rec = RawRecord::GdeltEventCsv("αβγδε".repeat(100));
        let ex = rec.excerpt(16);
        assert!(ex.len() <= 16 + '…'.len_utf8());
        assert!(ex.ends_with('…'));
    }
}
