//! One cached plain-language digest per UTC calendar day, over the signals
//! this project already stores. An explicit regeneration replaces that day's
//! cached digest.
//!
//! Everything here except [`live`] is pure: the facts type storage fills in,
//! the prompt rendered from it, the exact request body, and the response
//! parser. Only `live.rs` (feature `live`) touches the network.
//!
//! Three constraints shape this crate, and each is enforced in code rather
//! than left to the prompt:
//!
//! 1. **Media attention and event data never blend.** The model is asked for
//!    two separately-schema'd strings, not one summary, so a blended answer
//!    is not a shape the response can take (see [`output_schema`]). This is
//!    the same hard rule the divergence layer and the region ledger follow.
//! 2. **ACLED is never redistributed.** [`row_level_permitted`] withholds
//!    row-level ACLED text from the request body entirely; ACLED reaches the
//!    model only as counts, which are our derived statistics, not ACLED's
//!    records.
//! 3. **Nothing person-level leaves the machine.** The facts type has no
//!    field that can carry an author, handle, user id, or message text — the
//!    streaming chatter sources are aggregate-only by construction upstream,
//!    and this crate has no API that could reintroduce identity.

use std::fmt::Write as _;

use chrono::NaiveDate;
use core_types::SourceId;
use serde_json::{Value, json};

#[cfg(feature = "live")]
pub mod live;

#[cfg(feature = "live")]
pub use live::GeminiDigester;

/// Google Generative Language API base. `LES_GEMINI_ENDPOINT` overrides it
/// (the mock-server tests point this at a local server).
pub const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
/// The only credential this feature needs, env-var only like every other
/// keyed source in this workspace.
pub const API_KEY_ENV: &str = "GEMINI_API_KEY";
/// Chosen off the free tier. Deliberately a current model rather than the
/// long-familiar `gemini-2.5-flash`, which now returns 404 "no longer
/// available to new users" — a model id here has a shelf life, and a 404 on
/// generate is the symptom to look for first.
///
/// The free tier's request cap is *per model per project per day* (20, quota
/// id `GenerateRequestsPerDayPerProjectPerModel-FreeTier`), so a day spent
/// debugging against one model exhausts that model and no other. Switching
/// this id is therefore a real workaround for a 429, not a superstition —
/// and a 429 here does not mean the key or the project is out of budget.
pub const MODEL: &str = "gemini-3.6-flash";
/// Caps thinking *and* response text together. A digest is two short
/// sections; the headroom is for thinking.
pub const MAX_TOKENS: u32 = 4_096;
/// Writing two short paragraphs from pre-aggregated counts is not a reasoning
/// task, and thinking tokens count against [`MAX_TOKENS`] alongside the
/// answer. `low` holds `thoughtsTokenCount` to double digits here; it is a
/// floor, not an off switch, so the headroom in `MAX_TOKENS` still matters.
/// It must be sent nested under `thinkingConfig` — at the top level of
/// `generationConfig` it is an unknown field and 400s.
pub const THINKING_LEVEL: &str = "low";

/// Full `generateContent` URL for [`MODEL`] under `base`.
///
/// Split from the base so the mock-server tests can point at a local socket
/// and still exercise the real path (the model id travels in the URL on this
/// API, not in the body).
pub fn api_url(base: &str) -> String {
    format!(
        "{}/models/{MODEL}:generateContent",
        base.trim_end_matches('/')
    )
}

/// Places carried into the prompt per section. Beyond this the tail is noise
/// the model would have to weigh anyway.
pub const MAX_PLACES: usize = 12;
/// Headline metadata rows carried into the prompt (title + outlet domain).
pub const MAX_HEADLINES: usize = 40;
/// Discrete-event rows carried into the prompt.
pub const MAX_NOTABLE: usize = 40;

/// May this source's **row-level** text be sent to a third-party API?
///
/// Counts derived from any source are ours to summarize; the records
/// themselves are not always ours to forward. ACLED's terms forbid
/// redistribution (CLAUDE.md hard rule), and the chatter sources never had
/// row-level text to begin with — `crates/chatter` discards it in the same
/// call that counts it.
pub fn row_level_permitted(source: SourceId) -> bool {
    match source {
        // Public, keyless feeds: GDELT article metadata, NWS alert headlines,
        // IODA outage labels.
        SourceId::Gdelt | SourceId::Noaa | SourceId::Ioda => true,
        SourceId::Acled => false,
        SourceId::Bluesky | SourceId::Telegram => false,
        // Synthetic data never reaches the desktop runtime at all; refusing
        // it here means a fixture row can never be described as real news.
        SourceId::Fixtures => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DigestError {
    #[error(
        "{API_KEY_ENV} is not set — the Daily Events digest needs a Gemini API key (env var only)"
    )]
    MissingKey,
    #[error("gemini http: {0}")]
    Http(String),
    #[error("gemini api: {0}")]
    Api(String),
    #[error("gemini rate limited (retry after {retry_after_secs:?}s)")]
    RateLimited { retry_after_secs: Option<u64> },
    /// The model declined to answer. Returned as HTTP 200 with an empty or
    /// truncated `candidates`, so it must be detected from `finishReason`
    /// (or `promptFeedback.blockReason`) before anything reads `parts[0]`.
    #[error("the model declined to produce a digest ({0})")]
    Refused(String),
    #[error("unparseable model response: {0}")]
    Parse(String),
    #[error("nothing to summarize for {0}")]
    NoData(NaiveDate),
}

/// One country's contribution to a day, in the units its section is measured
/// in. Country-level by design: the digest is a *daily* overview, and H3
/// cells do not name anything a reader recognizes.
///
/// `articles` is zero for event places — an event record has no article
/// count, and inventing one would blur the two halves this crate exists to
/// keep apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceCount {
    pub country_iso: String,
    pub records: u64,
    pub articles: u64,
}

/// Article metadata — never an article body (CLAUDE.md hard rule), and only
/// from sources [`row_level_permitted`] allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlineFact {
    pub country_iso: String,
    pub outlet_domain: String,
    pub headline: String,
}

/// One discrete event, reduced to the structural fields the map already
/// shows. `label` is the source's own event label, not a narrative.
///
/// A day repeats the same label constantly — a flood warning is reissued for
/// every affected zone — so identical `(country, kind, source, label)` rows
/// are collapsed into one entry and `occurrences` carries how many there
/// were. Forty repetitions of "Flood Warning" is a count wearing a name, and
/// it spends the whole sample saying one thing.
#[derive(Debug, Clone, PartialEq)]
pub struct EventFact {
    pub country_iso: String,
    pub kind: String,
    pub source: String,
    pub label: Option<String>,
    pub severity: Option<f32>,
    /// Rows collapsed into this entry; at least 1.
    pub occurrences: u64,
}

/// The media-attention half of a day. Counts of *coverage*, not of events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttentionFacts {
    pub records: u64,
    pub articles: u64,
    pub distinct_outlets: u32,
    pub top_places: Vec<PlaceCount>,
    pub headlines: Vec<HeadlineFact>,
}

/// The event-data half of a day. Counts of *observed events*, from event
/// datasets and monitors.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFacts {
    pub records: u64,
    /// How many of `records` are official alerts rather than observed events.
    /// Broken out, and named in the prompt, because a jurisdiction issuing a
    /// warning is not civil unrest and must not be narrated as one
    /// (docs/SIGNAL_MODEL.md). Alerts stay in this section because they are
    /// still things that happened -- Daily Events stays two-sectioned
    /// (CLAUDE.md product rule 6).
    pub official_alerts: u64,
    pub by_kind: Vec<(String, u64)>,
    pub by_source: Vec<(String, u64)>,
    pub top_places: Vec<PlaceCount>,
    pub notable: Vec<EventFact>,
    /// Sources that contributed to the counts above but whose rows were
    /// withheld from the request body ([`row_level_permitted`]). Named in the
    /// prompt so the model knows the notable list is not the whole picture.
    pub counts_only_sources: Vec<(String, u64)>,
}

/// Everything one day's digest is generated from. Built by `storage` in a
/// single pass over `events`; nothing else is sent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DigestFacts {
    pub day_utc: DayKey,
    pub attention: AttentionFacts,
    pub events: EventFacts,
}

impl DigestFacts {
    /// A day with no records at all. Callers must not spend an API call on it.
    pub fn is_empty(&self) -> bool {
        self.attention.records == 0 && self.events.records == 0
    }
}

/// A UTC calendar day. Newtype rather than a bare `NaiveDate` so the
/// `YYYY-MM-DD` storage key has exactly one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DayKey(pub NaiveDate);

impl Default for DayKey {
    fn default() -> Self {
        Self(NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch date is valid"))
    }
}

impl DayKey {
    /// The UTC day containing `epoch_s`.
    pub fn from_epoch(epoch_s: i64) -> Self {
        Self(
            chrono::DateTime::from_timestamp(epoch_s, 0)
                .unwrap_or_default()
                .date_naive(),
        )
    }

    pub fn parse(s: &str) -> Option<Self> {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(Self)
    }

    pub fn key(self) -> String {
        self.0.format("%Y-%m-%d").to_string()
    }

    /// Half-open `[start, end)` epoch-seconds window for this day.
    pub fn window(self) -> (i64, i64) {
        let start = self
            .0
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc()
            .timestamp();
        (start, start + 86_400)
    }
}

impl std::fmt::Display for DayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

/// A finished digest, as stored and as displayed. The two sections stay two
/// fields all the way through — there is no combined string anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayDigest {
    pub day_utc: DayKey,
    pub model: String,
    pub generated_at_epoch_s: i64,
    pub media_attention: String,
    pub event_data: String,
    /// Record counts the digest was written against. Displayed beside the
    /// prose so the text is never shown without the numbers behind it.
    pub attention_records: u64,
    pub event_records: u64,
}

/// The two sections as the model returns them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestSections {
    pub media_attention: String,
    pub event_data: String,
}

/// Response schema. The separation rule is structural: there is no field the
/// model could put a blended summary in.
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "media_attention": {
                "type": "string",
                "description": "4-8 sentences on what the world's news coverage \
                                concentrated on this day, in coverage terms only \
                                (articles, outlets, where coverage clustered). \
                                Name the countries, outlet domains, and headline \
                                subjects you were given rather than restating \
                                totals. Never assert that an event happened on the \
                                strength of coverage alone."
            },
            "event_data": {
                "type": "string",
                "description": "4-8 sentences on the discrete events recorded this \
                                day by the event datasets and monitors, in event \
                                terms only (counts by kind, where, which dataset). \
                                Name the individual alerts, outages, and other \
                                labelled rows you were given, with their severities, \
                                rather than only their totals. Never use coverage \
                                volume as evidence of an event."
            }
        },
        "required": ["media_attention", "event_data"],
        "additionalProperties": false
    })
}

pub const SYSTEM_PROMPT: &str = "\
You are writing the daily digest for Live Earth Signals, a civic-data \
research dashboard. You are given aggregate counts the dashboard computed \
from public data sources for one UTC calendar day.

Rules, in priority order:

1. Media attention and event data are separate quantities and are reported in \
separate fields. Media attention counts news coverage; event data counts \
events recorded by event datasets and monitors. Never merge them, never \
present one as evidence for the other, and never produce a single combined \
judgement of how significant a place or a day was.
2. Media attention is a biased proxy, not ground truth. Coverage volume \
reflects newsroom capacity, language, and audience as much as it reflects \
events. Say what was covered, not what happened, in the media_attention field.
3. Use only the facts given. Do not add background, causes, actors, or \
outcomes from your own knowledge, and do not speculate about what the numbers \
imply. If the day is thin, say it is thin.
4. Name places and datasets. Never name or describe individual people, and \
never characterise the users, authors, or members of any platform.
5. Prefer named specifics to bare aggregates. The facts include labelled rows \
as well as totals: weather-alert names, internet-outage labels, headline text \
and outlet domains, each with its country and severity. Use them. Say which \
alerts, which outages, which outlets and which countries — a total is context \
for a specific, not a substitute for one. Rule 4 still binds: describe what a \
headline concerned without naming the people in it. Where a list is empty, say \
so plainly rather than reaching for an example. Plain declarative prose, no \
headings, no bullet lists, no markdown, and cite the counts alongside the \
specifics you name.";

/// Render the facts the model sees. Deterministic, and the exact text the
/// mock-server tests assert against.
pub fn render_facts(facts: &DigestFacts) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "UTC day: {}", facts.day_utc);
    let _ = writeln!(s);

    let a = &facts.attention;
    let _ = writeln!(s, "== MEDIA ATTENTION (news coverage observations) ==");
    let _ = writeln!(
        s,
        "attention records: {}; articles behind them: {}; distinct outlet domains: {}",
        a.records, a.articles, a.distinct_outlets
    );
    if a.top_places.is_empty() {
        let _ = writeln!(s, "top countries by attention: none");
    } else {
        let _ = writeln!(s, "top countries by attention (ISO-A3):");
        for p in &a.top_places {
            let _ = writeln!(
                s,
                "  {} records={} articles={}",
                p.country_iso, p.records, p.articles
            );
        }
    }
    if a.headlines.is_empty() {
        let _ = writeln!(s, "headline metadata: none available");
    } else {
        let _ = writeln!(
            s,
            "headline metadata (title and outlet domain only, no article text):"
        );
        for h in &a.headlines {
            let _ = writeln!(
                s,
                "  [{}] {} — {}",
                h.country_iso, h.headline, h.outlet_domain
            );
        }
    }
    let _ = writeln!(s);

    let e = &facts.events;
    let _ = writeln!(s, "== EVENT DATA (recorded events and official alerts) ==");
    let _ = writeln!(s, "event records: {}", e.records);
    if e.official_alerts > 0 {
        let _ = writeln!(
            s,
            "of which official alerts issued by an agency: {} (a warning, \
             not an observed incident - never describe these as unrest)",
            e.official_alerts
        );
    }
    let _ = writeln!(s, "by kind: {}", pairs(&e.by_kind));
    let _ = writeln!(s, "by dataset: {}", pairs(&e.by_source));
    if e.top_places.is_empty() {
        let _ = writeln!(s, "top countries by events: none");
    } else {
        let _ = writeln!(s, "top countries by events (ISO-A3):");
        for p in &e.top_places {
            let _ = writeln!(s, "  {} events={}", p.country_iso, p.records);
        }
    }
    if e.notable.is_empty() {
        let _ = writeln!(s, "event rows: none available");
    } else {
        let _ = writeln!(s, "event rows (structural fields only):");
        for ev in &e.notable {
            let _ = write!(s, "  [{}] {} via {}", ev.country_iso, ev.kind, ev.source);
            if let Some(label) = &ev.label {
                let _ = write!(s, " — {label}");
            }
            if let Some(sev) = ev.severity {
                let _ = write!(s, " (severity {sev:.2})");
            }
            if ev.occurrences > 1 {
                let _ = write!(s, " ×{}", ev.occurrences);
            }
            let _ = writeln!(s);
        }
    }
    if !e.counts_only_sources.is_empty() {
        let _ = writeln!(
            s,
            "counted above but not listed row-by-row (licence restricts redistribution): {}",
            pairs(&e.counts_only_sources)
        );
        let _ = writeln!(
            s,
            "Treat those counts as real. Their absence from the row list is a licence \
             constraint, not an absence of events."
        );
    }
    s
}

fn pairs(v: &[(String, u64)]) -> String {
    if v.is_empty() {
        return "none".to_owned();
    }
    v.iter()
        .map(|(k, n)| format!("{k}={n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The exact JSON body sent to `POST …:generateContent`.
///
/// `responseJsonSchema` — **not** `responseSchema`. They are different fields:
/// `responseSchema` takes the OpenAPI-3.0 subset, which has no
/// `additionalProperties`, so routing this schema through it would drop the
/// one keyword the separation rule depends on and fail open, silently.
/// `responseJsonSchema` takes real JSON Schema and enforces it by constrained
/// decoding — verified against the live API with a prompt explicitly ordering
/// the model to add a third, blended field: the response still came back with
/// exactly the two properties.
///
/// This API rejects unknown `generationConfig` keys with a 400 naming the
/// field, so a typo here fails loudly rather than being ignored.
pub fn request_body(facts: &DigestFacts) -> Value {
    json!({
        "systemInstruction": {
            "parts": [{"text": SYSTEM_PROMPT}],
        },
        "contents": [{
            "role": "user",
            "parts": [{"text": render_facts(facts)}],
        }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseJsonSchema": output_schema(),
            "maxOutputTokens": MAX_TOKENS,
            "thinkingConfig": {"thinkingLevel": THINKING_LEVEL},
        }
    })
}

/// Pull the two sections out of a `generateContent` response.
///
/// Both block signals are checked before any `parts` are touched: a blocked
/// prompt comes back as HTTP 200 with `promptFeedback.blockReason` and *no*
/// candidates at all, and a blocked completion as a candidate whose
/// `finishReason` is not `STOP`. Indexing `parts[0]` first would report either
/// as malformed output and hide why it actually failed.
///
/// Any non-`STOP` reason other than `MAX_TOKENS` is treated as a refusal
/// rather than matched against a fixed list — the enum grows (`SAFETY`,
/// `RECITATION`, `PROHIBITED_CONTENT`, `BLOCKLIST`, `SPII`, `OTHER`, …), and
/// an unrecognised stop reason is never a reason to trust the payload.
pub fn parse_response(body: &Value) -> Result<DigestSections, DigestError> {
    if let Some(reason) = body
        .get("promptFeedback")
        .and_then(|f| f.get("blockReason"))
        .and_then(Value::as_str)
    {
        return Err(DigestError::Refused(format!("prompt blocked: {reason}")));
    }

    let candidate = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| DigestError::Parse("response carried no candidates".into()))?;

    match candidate.get("finishReason").and_then(Value::as_str) {
        None | Some("STOP") => {}
        Some("MAX_TOKENS") => {
            return Err(DigestError::Parse(
                "response hit maxOutputTokens before the digest was complete".into(),
            ));
        }
        Some(other) => return Err(DigestError::Refused(other.to_owned())),
    }

    // Thought parts share the array with answer parts and are flagged
    // `thought: true`; only the unflagged text carries the answer.
    let text: String = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(DigestError::Parse("response carried no text block".into()));
    }
    let parsed: Value = serde_json::from_str(text.trim())
        .map_err(|e| DigestError::Parse(format!("text block was not JSON: {e}")))?;
    let field = |name: &str| -> Result<String, DigestError> {
        parsed
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| DigestError::Parse(format!("`{name}` missing or empty")))
    };
    Ok(DigestSections {
        media_attention: field("media_attention")?,
        event_data: field("event_data")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> DigestFacts {
        DigestFacts {
            day_utc: DayKey::parse("2026-08-12").unwrap(),
            attention: AttentionFacts {
                records: 120,
                articles: 900,
                distinct_outlets: 44,
                top_places: vec![PlaceCount {
                    country_iso: "KEN".into(),
                    records: 30,
                    articles: 210,
                }],
                headlines: vec![HeadlineFact {
                    country_iso: "KEN".into(),
                    outlet_domain: "example.test".into(),
                    headline: "Nairobi transit strike enters second day".into(),
                }],
            },
            events: EventFacts {
                records: 22,
                official_alerts: 4,
                by_kind: vec![
                    ("protest".into(), 12),
                    ("disruption".into(), 6),
                    ("alert".into(), 4),
                ],
                by_source: vec![("acled".into(), 12), ("ioda".into(), 6), ("noaa".into(), 4)],
                top_places: vec![PlaceCount {
                    country_iso: "KEN".into(),
                    records: 9,
                    articles: 0,
                }],
                notable: vec![EventFact {
                    country_iso: "SDN".into(),
                    kind: "disruption".into(),
                    source: "ioda".into(),
                    label: Some("national outage".into()),
                    severity: Some(0.75),
                    occurrences: 3,
                }],
                counts_only_sources: vec![("acled".into(), 12)],
            },
        }
    }

    #[test]
    fn day_key_window_is_the_utc_calendar_day() {
        // Hand-computed: 2026-08-12T00:00:00Z is 20_677 whole days after the
        // Unix epoch — 56 * 365 + 14 leap days (1972..=2024) = 20_454 days to
        // 2026-01-01, plus 223 days (Jan..Jul = 212, plus 11 in August).
        const START: i64 = 20_677 * 86_400;
        let day = DayKey::parse("2026-08-12").unwrap();
        assert_eq!(day.window(), (START, START + 86_400));
    }

    #[test]
    fn day_key_round_trips_through_its_storage_string() {
        let day = DayKey::parse("2026-08-12").unwrap();
        assert_eq!(day.key(), "2026-08-12");
        assert_eq!(DayKey::parse(&day.key()), Some(day));
    }

    #[test]
    fn day_key_from_epoch_uses_utc_not_local_time() {
        // 23:59:59Z stays on the 12th; one second later is the 13th.
        let end = DayKey::parse("2026-08-12").unwrap().window().1;
        assert_eq!(DayKey::from_epoch(end - 1).key(), "2026-08-12");
        assert_eq!(DayKey::from_epoch(end).key(), "2026-08-13");
    }

    #[test]
    fn acled_and_chatter_rows_are_never_row_level_permitted() {
        assert!(!row_level_permitted(SourceId::Acled));
        assert!(!row_level_permitted(SourceId::Bluesky));
        assert!(!row_level_permitted(SourceId::Telegram));
        assert!(!row_level_permitted(SourceId::Fixtures));
        assert!(row_level_permitted(SourceId::Gdelt));
        assert!(row_level_permitted(SourceId::Noaa));
        assert!(row_level_permitted(SourceId::Ioda));
    }

    #[test]
    fn rendered_facts_keep_the_two_halves_in_labelled_sections() {
        let text = render_facts(&facts());
        let att = text.find("== MEDIA ATTENTION").expect("attention section");
        let evt = text.find("== EVENT DATA").expect("event section");
        assert!(att < evt, "attention section comes first");
        // Attention numbers stay in the attention half.
        assert!(text[att..evt].contains("attention records: 120"));
        assert!(text[att..evt].contains("articles behind them: 900"));
        // Event numbers stay in the event half.
        assert!(text[evt..].contains("event records: 22"));
        assert!(text[evt..].contains("protest=12"));
    }

    #[test]
    fn official_alerts_are_named_as_warnings_inside_the_event_section() {
        // Alerts share the event section (two sections only), so the prompt has
        // to say what they are or the model narrates a weather warning as
        // unrest -- docs/SIGNAL_MODEL.md.
        let text = render_facts(&facts());
        let evt = text.find("== EVENT DATA").expect("event section");
        let line = text[evt..]
            .lines()
            .find(|l| l.starts_with("of which official alerts"))
            .expect("alert line");
        assert!(line.contains(": 4"));
        assert!(text[evt..].contains("never describe these as unrest"));
    }

    #[test]
    fn a_day_without_alerts_says_nothing_about_them() {
        let mut f = facts();
        f.events.official_alerts = 0;
        let text = render_facts(&f);
        assert!(
            !text
                .lines()
                .any(|l| l.starts_with("of which official alerts"))
        );
    }

    #[test]
    fn withheld_sources_are_declared_as_counted_but_unlisted() {
        let text = render_facts(&facts());
        assert!(text.contains("licence restricts redistribution): acled=12"));
        assert!(text.contains("Treat those counts as real"));
    }

    #[test]
    fn request_body_routes_the_schema_through_the_field_that_enforces_it() {
        let body = request_body(&facts());
        let cfg = &body["generationConfig"];
        // `responseSchema` takes the OpenAPI-3.0 subset, which has no
        // `additionalProperties` — sending the schema there would drop the
        // separation wall silently. It must never appear.
        assert!(
            cfg.get("responseSchema").is_none(),
            "the schema must travel in responseJsonSchema, not responseSchema"
        );
        assert_eq!(cfg["responseJsonSchema"], output_schema());
        // Constrained decoding only engages when JSON output is requested.
        assert_eq!(cfg["responseMimeType"], "application/json");
        assert_eq!(cfg["maxOutputTokens"], MAX_TOKENS);
        assert_eq!(cfg["thinkingConfig"]["thinkingLevel"], THINKING_LEVEL);
        // The model id travels in the URL on this API; a `model` key in the
        // body is an unknown field and 400s.
        assert!(body.get("model").is_none());
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        // The instructions are a separate top-level field, not a pseudo-turn.
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], SYSTEM_PROMPT);
    }

    #[test]
    fn api_url_carries_the_model_and_tolerates_a_trailing_slash() {
        let expected = format!("http://127.0.0.1:1/models/{MODEL}:generateContent");
        assert_eq!(api_url("http://127.0.0.1:1"), expected);
        assert_eq!(api_url("http://127.0.0.1:1/"), expected);
    }

    #[test]
    fn output_schema_has_no_field_a_blended_summary_could_go_in() {
        let schema = output_schema();
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 2);
        assert!(props.contains_key("media_attention"));
        assert!(props.contains_key("event_data"));
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn parse_response_skips_thought_parts() {
        // The real API attaches a `thoughtSignature` to the *answer* part, so
        // the filter keys on the `thought` flag, not on the presence of some
        // thinking-related field.
        let body = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": "weighing the counts", "thought": true},
                    {
                        "text": r#"{"media_attention":"Coverage clustered.","event_data":"Twelve protests."}"#,
                        "thoughtSignature": "opaque",
                    },
                ]}
            }]
        });
        let out = parse_response(&body).unwrap();
        assert_eq!(out.media_attention, "Coverage clustered.");
        assert_eq!(out.event_data, "Twelve protests.");
    }

    #[test]
    fn parse_response_reports_a_refusal_rather_than_a_parse_failure() {
        // Completion-side block: a candidate exists but stopped for a reason
        // that is not STOP.
        let body = json!({
            "candidates": [{"finishReason": "SAFETY", "content": {"parts": []}}]
        });
        let err = parse_response(&body).unwrap_err();
        assert!(
            matches!(&err, DigestError::Refused(c) if c == "SAFETY"),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_response_reports_a_blocked_prompt_before_looking_for_candidates() {
        // Prompt-side block: HTTP 200 with no candidates at all.
        let body = json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"}});
        let err = parse_response(&body).unwrap_err();
        assert!(
            matches!(&err, DigestError::Refused(c) if c.contains("PROHIBITED_CONTENT")),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_response_rejects_a_truncated_response() {
        let body = json!({
            "candidates": [{
                "finishReason": "MAX_TOKENS",
                "content": {"parts": [{"text": "{\"media_attention\":\"half"}]}
            }]
        });
        assert!(matches!(
            parse_response(&body).unwrap_err(),
            DigestError::Parse(_)
        ));
    }

    #[test]
    fn parse_response_rejects_an_empty_section() {
        let body = json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": r#"{"media_attention":"   ","event_data":"x"}"#}
                ]}
            }]
        });
        let err = parse_response(&body).unwrap_err();
        assert!(err.to_string().contains("media_attention"), "{err}");
    }

    #[test]
    fn empty_facts_are_detected_before_a_call_is_spent() {
        assert!(DigestFacts::default().is_empty());
        assert!(!facts().is_empty());
    }
}
