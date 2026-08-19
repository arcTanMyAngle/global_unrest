//! Core domain types shared by every crate: events, windows, buckets, sources.
//!
//! This crate does no I/O. Shared domain semantics that both `analytics` and
//! `storage` must agree on (H3 resolution, time-bucket size) live here so
//! neither depends on the other.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod attribution;
pub use attribution::{ACCESS_DATE_SLOT, AttributionSubject, SourceAttribution, attribution_for};

mod media;
pub use media::{Embed, embed_for, is_video_url};

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
    Noaa,
    Ioda,
    Bluesky,
    Telegram,
}

impl SourceId {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::Fixtures => "fixtures",
            SourceId::Gdelt => "gdelt",
            SourceId::Acled => "acled",
            SourceId::Noaa => "noaa",
            SourceId::Ioda => "ioda",
            SourceId::Bluesky => "bluesky",
            SourceId::Telegram => "telegram",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fixtures" => Some(SourceId::Fixtures),
            "gdelt" => Some(SourceId::Gdelt),
            "acled" => Some(SourceId::Acled),
            "noaa" => Some(SourceId::Noaa),
            "ioda" => Some(SourceId::Ioda),
            "bluesky" => Some(SourceId::Bluesky),
            "telegram" => Some(SourceId::Telegram),
            _ => None,
        }
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of observation a record is — the top-level signal axis.
///
/// This is the machine-checkable form of product rule 1: media attention,
/// discrete events, official alerts, aggregate chatter, and measurements are
/// different kinds of thing and are never summed into one number. See
/// docs/SIGNAL_MODEL.md for the full contract (units, scoring membership,
/// digest membership, storage).
///
/// [`EventKind`] is the *within-family* subtype, not an independent axis;
/// [`SignalFamily::permits`] is the whole valid set of pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalFamily {
    /// How much press coverage a place/topic got — an observation about
    /// coverage, not about the world.
    MediaAttention,
    /// A discrete real-world occurrence someone recorded.
    RecordedEvent,
    /// An authority announcing a hazard for an area it is responsible for.
    OfficialAlert,
    /// Aggregate social post volume. Counts only — see [`ChatterRollup`].
    Chatter,
    /// A laboratory/sensor result. Declared so the taxonomy is complete and
    /// long-form storage accepts it without migration; no source produces one
    /// today, and none should be added without checking the source's terms.
    Measurement,
}

impl SignalFamily {
    pub const ALL: [SignalFamily; 5] = [
        SignalFamily::MediaAttention,
        SignalFamily::RecordedEvent,
        SignalFamily::OfficialAlert,
        SignalFamily::Chatter,
        SignalFamily::Measurement,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SignalFamily::MediaAttention => "media_attention",
            SignalFamily::RecordedEvent => "recorded_event",
            SignalFamily::OfficialAlert => "official_alert",
            SignalFamily::Chatter => "chatter",
            SignalFamily::Measurement => "measurement",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "media_attention" => Some(SignalFamily::MediaAttention),
            "recorded_event" => Some(SignalFamily::RecordedEvent),
            "official_alert" => Some(SignalFamily::OfficialAlert),
            "chatter" => Some(SignalFamily::Chatter),
            "measurement" => Some(SignalFamily::Measurement),
            _ => None,
        }
    }

    /// Human-readable label for UI.
    pub fn label(self) -> &'static str {
        match self {
            SignalFamily::MediaAttention => "Media attention",
            SignalFamily::RecordedEvent => "Recorded event",
            SignalFamily::OfficialAlert => "Official alert",
            SignalFamily::Chatter => "Aggregate chatter",
            SignalFamily::Measurement => "Measurement",
        }
    }

    /// Is this `kind` valid inside this family?
    ///
    /// The authoritative family/kind matrix. Normalization rejects any pair
    /// this returns `false` for, so the stored column cannot drift away from
    /// docs/SIGNAL_MODEL.md.
    pub fn permits(self, kind: EventKind) -> bool {
        match self {
            SignalFamily::MediaAttention => matches!(kind, EventKind::NewsAttention),
            SignalFamily::RecordedEvent => matches!(
                kind,
                EventKind::Protest | EventKind::Conflict | EventKind::Disruption | EventKind::Other
            ),
            SignalFamily::OfficialAlert => matches!(kind, EventKind::Alert),
            SignalFamily::Chatter => matches!(kind, EventKind::Chatter),
            SignalFamily::Measurement => matches!(kind, EventKind::Measurement),
        }
    }

    /// What `volume_count` counts for this family. Units differ per family and
    /// are never added across families.
    pub fn volume_unit(self) -> VolumeUnit {
        match self {
            SignalFamily::MediaAttention => VolumeUnit::Articles,
            SignalFamily::RecordedEvent => VolumeUnit::Records,
            SignalFamily::OfficialAlert => VolumeUnit::Alerts,
            SignalFamily::Chatter => VolumeUnit::Posts,
            SignalFamily::Measurement => VolumeUnit::Samples,
        }
    }

    /// Does this family contribute to `attention_score` and the attention-only
    /// coverage columns (`article_count`, `source_count`, `distinct_outlets`)?
    pub fn enters_attention(self) -> bool {
        matches!(self, SignalFamily::MediaAttention)
    }

    /// Does this family contribute to `unrest_score`?
    ///
    /// Official alerts deliberately do not: a weather warning is a
    /// jurisdiction announcing a hazard, not an occurrence of civil unrest.
    pub fn enters_unrest(self) -> bool {
        matches!(self, SignalFamily::RecordedEvent)
    }

    /// Does this family feed the generic (cross-family) spike baseline and
    /// therefore `combined_score`?
    ///
    /// Chatter does not. It has its own per-family baseline instead, so post
    /// volume can never move the headline number.
    pub fn enters_generic_spike(self) -> bool {
        matches!(
            self,
            SignalFamily::MediaAttention | SignalFamily::RecordedEvent
        )
    }

    /// Does this family reach the Daily Events digest at all?
    ///
    /// Chatter does not, which is why no chatter-specific withholding rule is
    /// needed on the third-party request path.
    pub fn in_digest(self) -> bool {
        matches!(
            self,
            SignalFamily::MediaAttention
                | SignalFamily::RecordedEvent
                | SignalFamily::OfficialAlert
        )
    }
}

/// What a family's `volume_count` counts. Display-only; the unit is implied by
/// [`SignalFamily`], never stored as free text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeUnit {
    Articles,
    Records,
    Alerts,
    Posts,
    Samples,
}

impl VolumeUnit {
    /// Singular/plural label for UI ("3 posts", "1 article").
    pub fn label(self, n: u64) -> &'static str {
        match (self, n) {
            (VolumeUnit::Articles, 1) => "article",
            (VolumeUnit::Articles, _) => "articles",
            (VolumeUnit::Records, 1) => "record",
            (VolumeUnit::Records, _) => "records",
            (VolumeUnit::Alerts, 1) => "alert",
            (VolumeUnit::Alerts, _) => "alerts",
            (VolumeUnit::Posts, 1) => "post",
            (VolumeUnit::Posts, _) => "posts",
            (VolumeUnit::Samples, 1) => "sample",
            (VolumeUnit::Samples, _) => "samples",
        }
    }
}

/// What a record's coordinates actually mean.
///
/// A lat/lon is not self-describing: the publisher's country, a place named in
/// an article, and the site of an event are all "the location" of some record,
/// and treating them alike puts stories on the wrong side of the world. See
/// docs/SIGNAL_MODEL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationRole {
    /// Where the thing happened.
    EventSite,
    /// A place the coverage refers to.
    MentionedPlace,
    /// Where the outlet is based — *not* where the story is about.
    PublisherOrigin,
    /// The area an authority issued this record for.
    ReportingJurisdiction,
}

impl LocationRole {
    pub const ALL: [LocationRole; 4] = [
        LocationRole::EventSite,
        LocationRole::MentionedPlace,
        LocationRole::PublisherOrigin,
        LocationRole::ReportingJurisdiction,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LocationRole::EventSite => "event_site",
            LocationRole::MentionedPlace => "mentioned_place",
            LocationRole::PublisherOrigin => "publisher_origin",
            LocationRole::ReportingJurisdiction => "reporting_jurisdiction",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "event_site" => Some(LocationRole::EventSite),
            "mentioned_place" => Some(LocationRole::MentionedPlace),
            "publisher_origin" => Some(LocationRole::PublisherOrigin),
            "reporting_jurisdiction" => Some(LocationRole::ReportingJurisdiction),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LocationRole::EventSite => "Event site",
            LocationRole::MentionedPlace => "Mentioned place",
            LocationRole::PublisherOrigin => "Publisher origin",
            LocationRole::ReportingJurisdiction => "Reporting jurisdiction",
        }
    }

    /// May a record with this role be placed on the map as a statement about
    /// *that place*?
    ///
    /// `PublisherOrigin` may not: shading the publisher's country and calling
    /// it attention for that place is simply wrong. GDELT DOC records carry
    /// this role until the GDELT geography work replaces them with a real
    /// story location.
    pub fn is_spatially_meaningful(self) -> bool {
        !matches!(self, LocationRole::PublisherOrigin)
    }
}

/// Coarse within-family record subtype.
///
/// Not an independent axis — every kind belongs to exactly one
/// [`SignalFamily`], and [`SignalFamily::permits`] is the valid set. Do not add
/// a `matches!(kind, ...)` branch that decides scoring or display membership;
/// ask the family instead, so a new family cannot silently inherit behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    NewsAttention,
    Protest,
    Conflict,
    Disruption,
    Other,
    /// Aggregate social post volume.
    Chatter,
    /// An official hazard/emergency announcement.
    Alert,
    /// A laboratory or sensor result.
    Measurement,
}

impl EventKind {
    pub const ALL: [EventKind; 8] = [
        EventKind::NewsAttention,
        EventKind::Protest,
        EventKind::Conflict,
        EventKind::Disruption,
        EventKind::Other,
        EventKind::Chatter,
        EventKind::Alert,
        EventKind::Measurement,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::NewsAttention => "news_attention",
            EventKind::Protest => "protest",
            EventKind::Conflict => "conflict",
            EventKind::Disruption => "disruption",
            EventKind::Other => "other",
            EventKind::Chatter => "chatter",
            EventKind::Alert => "alert",
            EventKind::Measurement => "measurement",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "news_attention" => Some(EventKind::NewsAttention),
            "protest" => Some(EventKind::Protest),
            "conflict" => Some(EventKind::Conflict),
            "disruption" => Some(EventKind::Disruption),
            "other" => Some(EventKind::Other),
            "chatter" => Some(EventKind::Chatter),
            "alert" => Some(EventKind::Alert),
            "measurement" => Some(EventKind::Measurement),
            _ => None,
        }
    }

    /// The one family this kind belongs to — the inverse of
    /// [`SignalFamily::permits`], which the matrix test holds to agree.
    pub fn family(self) -> SignalFamily {
        match self {
            EventKind::NewsAttention => SignalFamily::MediaAttention,
            EventKind::Protest | EventKind::Conflict | EventKind::Disruption | EventKind::Other => {
                SignalFamily::RecordedEvent
            }
            EventKind::Chatter => SignalFamily::Chatter,
            EventKind::Alert => SignalFamily::OfficialAlert,
            EventKind::Measurement => SignalFamily::Measurement,
        }
    }

    /// Human-readable label for UI.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::NewsAttention => "News attention",
            EventKind::Protest => "Protest",
            EventKind::Conflict => "Conflict",
            EventKind::Disruption => "Disruption",
            EventKind::Other => "Other",
            EventKind::Chatter => "Chatter",
            EventKind::Alert => "Alert",
            EventKind::Measurement => "Measurement",
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

    /// Human-readable label for UI (tooltips, badges).
    pub fn label(self) -> &'static str {
        match self {
            LocationPrecision::Country => "Country",
            LocationPrecision::Admin1 => "Admin-1",
            LocationPrecision::City => "City",
            LocationPrecision::Exact => "Exact",
        }
    }
}

/// The single normalized record every source adapter produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeoTemporalEvent {
    pub id: u64,
    pub source: SourceId,
    pub source_event_id: String,
    /// What kind of observation this is. Decides scoring, digest, and display
    /// membership — ask this, never `kind`.
    pub family: SignalFamily,
    /// Within-family subtype. Must satisfy `family.permits(kind)`.
    pub kind: EventKind,
    pub themes: Vec<String>,
    pub ts_utc: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub lat: f64,
    pub lon: f64,
    /// What the coordinates mean. `PublisherOrigin` records are not statements
    /// about the place they point at.
    pub location_role: LocationRole,
    pub location_precision: LocationPrecision,
    /// 0.0–1.0
    pub location_confidence: f32,
    /// ISO 3166-1 alpha-3.
    pub country_iso: String,
    pub admin1: Option<String>,
    /// H3 cell at [`H3_RESOLUTION`], stored as the raw u64 index.
    pub h3_cell: u64,
    /// How much of the family's own unit this record represents — articles,
    /// records, alerts, posts, or samples ([`SignalFamily::volume_unit`]).
    /// Never summed across families.
    pub volume_count: u32,
    /// Distinct outlets behind this record. Meaningful for `MediaAttention`
    /// and for coverage *of* a `RecordedEvent`; zero elsewhere.
    pub distinct_source_count: u32,
    /// 0.0–1.0 when the source provides one.
    pub severity: Option<f32>,
    /// Metadata only — never article bodies (docs/SAFETY_AND_PRIVACY.md), and
    /// never a string this crate invented to fill the field.
    pub headline: Option<String>,
    pub outlet_domains: Vec<String>,
    pub urls: Vec<String>,
}

impl GeoTemporalEvent {
    /// Reject a record whose family/kind pair is outside the matrix.
    ///
    /// Every adapter calls this at the end of `normalize`, so an invalid pair
    /// becomes an `ingest_log` failure instead of a stored row that later code
    /// has to defend against. See docs/SIGNAL_MODEL.md.
    pub fn validate(&self) -> Result<(), NormalizeError> {
        if !self.family.permits(self.kind) {
            return Err(NormalizeError::InvalidValue {
                field: "kind",
                detail: format!(
                    "kind `{}` is not valid in family `{}`",
                    self.kind.as_str(),
                    self.family.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Does this record belong on the map as a statement about its own
    /// coordinates? False for publisher-origin rows.
    pub fn is_spatially_meaningful(&self) -> bool {
        self.location_role.is_spatially_meaningful()
    }
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

/// Aggregate for one (H3 res-3 cell, 6-hour bucket): raw counts plus the M2
/// score components. Components are stored separately and always displayed
/// separately — never only the combined number (docs/SCORING.md).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RegionBucket {
    pub h3_cell: u64,
    /// Bucket start, epoch seconds, floored with [`bucket_start_epoch`].
    pub bucket_start: i64,
    /// `RecordedEvent` records only (protest/conflict/disruption/other).
    /// Official alerts and chatter are counted in `family_buckets`, not here.
    pub event_count: u32,
    /// `MediaAttention` observations only.
    pub attention_count: u32,
    /// Sum of `volume_count` across `MediaAttention` records only — real
    /// articles. Chatter post volume never reaches this column
    /// (docs/SIGNAL_MODEL.md).
    pub article_count: u64,
    /// Sum of per-record distinct outlet counts across `MediaAttention`
    /// records (an upper bound on true distinct outlets; exact de-duplication
    /// needs raw outlet sets).
    pub source_count: u64,
    /// Exact distinct outlet domains across the bucket's `MediaAttention`
    /// records (unlike `source_count`, which is a summed upper bound).
    pub distinct_outlets: u32,
    /// Attention component in [0, 1] — attention observations only.
    pub attention_score: f32,
    /// Unrest component in [0, 1] — discrete event records only.
    pub unrest_score: f32,
    /// Spike vs. trailing baseline, in [0, 1] with 0.5 neutral.
    pub spike_score: f32,
    /// 0.40·attention + 0.45·unrest + 0.15·spike.
    pub combined_score: f32,
    /// Spike denominator: trailing 28-day median records-per-bucket for this
    /// cell and time-of-day slot, as of this bucket's day.
    pub baseline: f32,
    /// Fewer than `MIN_BASELINE_DAYS` of history behind this bucket — spike
    /// was forced neutral and the UI shows a low-confidence badge.
    pub spike_cold_start: bool,
}

impl RegionBucket {
    /// All-zero bucket for a key; aggregation entries start from this.
    pub fn empty(h3_cell: u64, bucket_start: i64) -> Self {
        Self {
            h3_cell,
            bucket_start,
            event_count: 0,
            attention_count: 0,
            article_count: 0,
            source_count: 0,
            distinct_outlets: 0,
            attention_score: 0.0,
            unrest_score: 0.0,
            spike_score: 0.0,
            combined_score: 0.0,
            baseline: 0.0,
            spike_cold_start: false,
        }
    }
}

/// One (cell, bucket, family) count — the long-form per-family aggregate.
///
/// Long-form rather than one column per family on [`RegionBucket`] so that
/// adding a family is not a schema migration, and because silence detection
/// needs a deficit *per family against that family's own baseline*.
/// See docs/SIGNAL_MODEL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyBucket {
    pub h3_cell: u64,
    /// Bucket start, epoch seconds, floored with [`bucket_start_epoch`].
    pub bucket_start: i64,
    pub family: SignalFamily,
    /// Records in this family for this cell and bucket.
    pub record_count: u32,
    /// Sum of `volume_count` in this family's own unit
    /// ([`SignalFamily::volume_unit`]). Never added across families.
    pub volume_count: u64,
}

/// Where a chatter channel sits relative to what it reports on.
///
/// This is a property of a *channel*, not of any person: a monitoring
/// channel's posting rate tracks events, a combatant channel's tracks
/// messaging, and summing them yields a number that means neither. Class is
/// part of the aggregation key so the two are never added — see
/// docs/SIGNAL_MODEL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
    /// No provenance asserted. The default for open networks (Bluesky) and for
    /// rows that predate classification — never guessed at.
    Unspecified,
    /// Independent monitoring/OSINT reporting.
    Monitor,
    /// Established news outlet.
    Outlet,
    /// Aligned with a side but not a party to the fighting.
    Partisan,
    /// A party to the conflict, reporting on itself.
    Combatant,
    /// A government or state-run body.
    State,
}

impl ChannelClass {
    pub const ALL: [ChannelClass; 6] = [
        ChannelClass::Unspecified,
        ChannelClass::Monitor,
        ChannelClass::Outlet,
        ChannelClass::Partisan,
        ChannelClass::Combatant,
        ChannelClass::State,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChannelClass::Unspecified => "unspecified",
            ChannelClass::Monitor => "monitor",
            ChannelClass::Outlet => "outlet",
            ChannelClass::Partisan => "partisan",
            ChannelClass::Combatant => "combatant",
            ChannelClass::State => "state",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unspecified" => Some(ChannelClass::Unspecified),
            "monitor" => Some(ChannelClass::Monitor),
            "outlet" => Some(ChannelClass::Outlet),
            "partisan" => Some(ChannelClass::Partisan),
            "combatant" => Some(ChannelClass::Combatant),
            "state" => Some(ChannelClass::State),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChannelClass::Unspecified => "Unspecified",
            ChannelClass::Monitor => "Monitor",
            ChannelClass::Outlet => "Outlet",
            ChannelClass::Partisan => "Partisan",
            ChannelClass::Combatant => "Combatant",
            ChannelClass::State => "State",
        }
    }

    /// Does this channel's volume belong in the neutral aggregate?
    ///
    /// Partisan, combatant, and state channels are counted in their own claims
    /// lane: their posting rate tracks messaging, not events.
    pub fn is_neutral_lane(self) -> bool {
        matches!(
            self,
            ChannelClass::Unspecified | ChannelClass::Monitor | ChannelClass::Outlet
        )
    }
}

impl Default for ChannelClass {
    /// `Unspecified` — never `Monitor`. Defaulting an unclassified channel to
    /// `Monitor` would fabricate provenance the source never asserted.
    fn default() -> Self {
        ChannelClass::Unspecified
    }
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
    /// One GeoJSON alert feature from the NOAA/NWS active-alerts API (M5).
    NoaaAlertJson(serde_json::Value),
    /// One outage event from the IODA `/outages/events` API (keyless).
    IodaEventJson(serde_json::Value),
    /// One aggregate chatter count from a streaming social source.
    ///
    /// Unlike every other variant this is *not* one upstream record: the
    /// individual posts were counted in memory and discarded unread, and only
    /// this rollup ever leaves the source adapter. See [`ChatterRollup`].
    ChatterRollup(ChatterRollup),
}

/// One aggregate chatter count: how many posts in a flush window mentioned
/// both a known place and a known topic.
///
/// **This type is the privacy boundary for streaming social sources**
/// (docs/SAFETY_AND_PRIVACY.md). It carries a count and nothing else — never
/// post text, author handles/DIDs/user ids, post ids, or URLs. Individual
/// posts are matched as they stream past and dropped immediately; nothing
/// per-post is persisted, even transiently. Do not add a field here that
/// could identify a person or reproduce a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatterRollup {
    /// Display name of the matched place ("Kyiv", "Sudan").
    pub place_name: String,
    /// ISO 3166-1 alpha-3 of the place, or of its containing country.
    pub country_iso: String,
    pub lat: f64,
    pub lon: f64,
    /// `City` for a gazetteer city hit, `Country` for a country-name hit.
    pub precision: LocationPrecision,
    /// Topic label the matched keyword belongs to ("protest", "flood").
    pub topic: String,
    /// Provenance class of the channels these posts came from — a property of
    /// the channel, not of any person. Part of the aggregation key upstream,
    /// so classes are never summed together, and part of the derived event id,
    /// so two classes' rollups for the same place/topic/window do not collide.
    pub channel_class: ChannelClass,
    /// Flush-window start (epoch seconds), aligned to `window_secs` so the
    /// derived event id is stable across restarts and re-ingests.
    pub window_start_epoch_s: i64,
    pub window_secs: i64,
    /// Posts matched in this window — a count, never a list.
    pub post_count: u32,
}

impl RawRecord {
    /// Short excerpt for `ingest_log` (bounded so the log stays small).
    pub fn excerpt(&self, max_len: usize) -> String {
        let full = match self {
            RawRecord::FixtureJson(v)
            | RawRecord::GdeltDocJson(v)
            | RawRecord::AcledJson(v)
            | RawRecord::NoaaAlertJson(v)
            | RawRecord::IodaEventJson(v) => v.to_string(),
            RawRecord::GdeltEventCsv(s) => s.clone(),
            // Built by hand rather than derived-Debug'd so this can never
            // start echoing a future field into the ingest log.
            RawRecord::ChatterRollup(r) => format!(
                "chatter place={} topic={} class={} window_start={} count={}",
                r.place_name,
                r.topic,
                r.channel_class.as_str(),
                r.window_start_epoch_s,
                r.post_count
            ),
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
    // Most APIs omit `Retry-After` on a 429, so the `None` arm is the common
    // one, not the exceptional one — and it is shown to a person on the Media
    // page. The `{:?}` this used to use rendered that arm as the literal
    // "retry after Nones".
    #[error("rate limited{}", match .retry_after_secs {
        Some(secs) => format!("; retry after {secs}s"),
        None => String::new(),
    })]
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
        for f in SignalFamily::ALL {
            assert_eq!(SignalFamily::parse(f.as_str()), Some(f));
        }
        for r in LocationRole::ALL {
            assert_eq!(LocationRole::parse(r.as_str()), Some(r));
        }
        for c in ChannelClass::ALL {
            assert_eq!(ChannelClass::parse(c.as_str()), Some(c));
        }
    }

    /// The family/kind matrix in docs/SIGNAL_MODEL.md, held exactly.
    ///
    /// Every kind belongs to exactly one family, and `permits` agrees with
    /// `EventKind::family()` in both directions. Without this, the two could
    /// drift and a kind could become valid in two families at once.
    #[test]
    fn family_kind_matrix_is_a_partition() {
        for kind in EventKind::ALL {
            let owners: Vec<_> = SignalFamily::ALL
                .into_iter()
                .filter(|f| f.permits(kind))
                .collect();
            assert_eq!(
                owners.len(),
                1,
                "kind `{}` must belong to exactly one family, got {owners:?}",
                kind.as_str()
            );
            assert_eq!(
                owners[0],
                kind.family(),
                "permits() and family() disagree for `{}`",
                kind.as_str()
            );
        }
        // And every family owns at least one kind, so none is unreachable.
        for family in SignalFamily::ALL {
            assert!(
                EventKind::ALL.into_iter().any(|k| family.permits(k)),
                "family `{}` has no valid kind",
                family.as_str()
            );
        }
    }

    /// The scoring-membership half of the contract. These are the assertions
    /// that make "we added an enum" into "we separated the signals".
    #[test]
    fn scoring_membership_matches_the_contract() {
        // Chatter enters nothing generic: not unrest, not the generic spike,
        // not attention, not the digest.
        let chatter = SignalFamily::Chatter;
        assert!(!chatter.enters_unrest());
        assert!(!chatter.enters_generic_spike());
        assert!(!chatter.enters_attention());
        assert!(!chatter.in_digest());

        // Official alerts are not civil unrest, and do not spike the headline
        // number — but they do reach the digest's event section.
        let alert = SignalFamily::OfficialAlert;
        assert!(!alert.enters_unrest());
        assert!(!alert.enters_generic_spike());
        assert!(!alert.enters_attention());
        assert!(alert.in_digest());

        // Exactly one family drives unrest, and exactly one drives attention.
        assert_eq!(
            SignalFamily::ALL
                .into_iter()
                .filter(|f| f.enters_unrest())
                .collect::<Vec<_>>(),
            vec![SignalFamily::RecordedEvent]
        );
        assert_eq!(
            SignalFamily::ALL
                .into_iter()
                .filter(|f| f.enters_attention())
                .collect::<Vec<_>>(),
            vec![SignalFamily::MediaAttention]
        );

        // Measurement is declared but participates in nothing.
        let m = SignalFamily::Measurement;
        assert!(
            !m.enters_unrest()
                && !m.enters_generic_spike()
                && !m.enters_attention()
                && !m.in_digest()
        );
    }

    #[test]
    fn volume_units_are_family_specific() {
        assert_eq!(
            SignalFamily::MediaAttention.volume_unit(),
            VolumeUnit::Articles
        );
        assert_eq!(SignalFamily::Chatter.volume_unit(), VolumeUnit::Posts);
        assert_eq!(
            SignalFamily::OfficialAlert.volume_unit(),
            VolumeUnit::Alerts
        );
        // No two families that can be confused share a unit.
        assert_ne!(
            SignalFamily::MediaAttention.volume_unit(),
            SignalFamily::Chatter.volume_unit()
        );
        assert_eq!(VolumeUnit::Posts.label(1), "post");
        assert_eq!(VolumeUnit::Posts.label(3), "posts");
    }

    #[test]
    fn publisher_origin_is_not_a_statement_about_that_place() {
        assert!(!LocationRole::PublisherOrigin.is_spatially_meaningful());
        assert!(LocationRole::EventSite.is_spatially_meaningful());
        assert!(LocationRole::MentionedPlace.is_spatially_meaningful());
        assert!(LocationRole::ReportingJurisdiction.is_spatially_meaningful());
    }

    #[test]
    fn unclassified_channels_are_never_assumed_neutral_monitors() {
        assert_eq!(ChannelClass::default(), ChannelClass::Unspecified);
        assert!(ChannelClass::Unspecified.is_neutral_lane());
        for c in [
            ChannelClass::Partisan,
            ChannelClass::Combatant,
            ChannelClass::State,
        ] {
            assert!(
                !c.is_neutral_lane(),
                "{} must use the claims lane",
                c.as_str()
            );
        }
    }

    #[test]
    fn validate_rejects_off_matrix_pairs() {
        let mut ev = sample_event();
        assert!(ev.validate().is_ok());

        // The exact defect this contract exists to prevent: chatter volume
        // wearing a news-attention label.
        ev.family = SignalFamily::MediaAttention;
        ev.kind = EventKind::Chatter;
        assert!(ev.validate().is_err());

        ev.family = SignalFamily::Chatter;
        ev.kind = EventKind::NewsAttention;
        assert!(ev.validate().is_err());

        // Every off-matrix pair, not just the interesting ones.
        for family in SignalFamily::ALL {
            for kind in EventKind::ALL {
                let mut e = sample_event();
                e.family = family;
                e.kind = kind;
                assert_eq!(
                    e.validate().is_ok(),
                    family.permits(kind),
                    "validate() disagreed with the matrix for {}/{}",
                    family.as_str(),
                    kind.as_str()
                );
            }
        }
    }

    fn sample_event() -> GeoTemporalEvent {
        GeoTemporalEvent {
            id: event_id(SourceId::Fixtures, "evt-42"),
            source: SourceId::Fixtures,
            source_event_id: "evt-42".into(),
            family: SignalFamily::RecordedEvent,
            kind: EventKind::Protest,
            themes: vec!["labor".into()],
            ts_utc: Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap(),
            ingested_at: Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap(),
            lat: 48.85,
            lon: 2.35,
            location_role: LocationRole::EventSite,
            location_precision: LocationPrecision::City,
            location_confidence: 0.9,
            country_iso: "FRA".into(),
            admin1: Some("Île-de-France".into()),
            h3_cell: 0x83_1f_b4_ff_ff_ff_ff,
            volume_count: 12,
            distinct_source_count: 5,
            severity: Some(0.3),
            headline: Some("Synthetic headline".into()),
            outlet_domains: vec!["globalwire.example".into()],
            urls: vec!["https://globalwire.example/a/1".into()],
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
        let ev = sample_event();
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
