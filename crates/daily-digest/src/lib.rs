//! One plain-language digest per UTC calendar day, over the signals this
//! project already stores.
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
pub use live::AnthropicDigester;

/// Anthropic Messages API endpoint. `LES_ANTHROPIC_ENDPOINT` overrides it
/// (the mock-server tests point this at a local server).
pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
/// Required on every request; this is an API version, not a model version.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The only credential this feature needs, env-var only like every other
/// keyed source in this workspace.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const MODEL: &str = "claude-opus-5";
/// Caps thinking *and* response text together on this model family. A digest
/// is two short sections, so the headroom here is mostly for thinking.
pub const MAX_TOKENS: u32 = 8_192;
/// A daily digest over pre-aggregated counts is not a hard reasoning task,
/// and this runs unattended once per day per user — `medium` rather than the
/// API default (`high`) is a deliberate cost choice, not an oversight.
pub const EFFORT: &str = "medium";

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
        "{API_KEY_ENV} is not set — the Daily Events digest needs an Anthropic API key (env var only)"
    )]
    MissingKey,
    #[error("anthropic http: {0}")]
    Http(String),
    #[error("anthropic api: {0}")]
    Api(String),
    #[error("anthropic rate limited (retry after {retry_after_secs:?}s)")]
    RateLimited { retry_after_secs: Option<u64> },
    /// The model declined to answer. Returned as HTTP 200 with an empty or
    /// partial `content`, so it must be detected from `stop_reason` before
    /// anything reads `content[0]`.
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
#[derive(Debug, Clone, PartialEq)]
pub struct EventFact {
    pub country_iso: String,
    pub kind: String,
    pub source: String,
    pub label: Option<String>,
    pub severity: Option<f32>,
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
                "description": "2-5 sentences on what the world's news coverage \
                                concentrated on this day, in coverage terms only \
                                (articles, outlets, where coverage clustered). \
                                Never assert that an event happened on the strength \
                                of coverage alone."
            },
            "event_data": {
                "type": "string",
                "description": "2-5 sentences on the discrete events recorded this \
                                day by the event datasets and monitors, in event \
                                terms only (counts by kind, where, which dataset). \
                                Never use coverage volume as evidence of an event."
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
5. Plain declarative prose, no headings, no bullet lists, no markdown. Cite \
the counts you are describing.";

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
    let _ = writeln!(s, "== EVENT DATA (discrete recorded events) ==");
    let _ = writeln!(s, "event records: {}", e.records);
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

/// The exact JSON body sent to `POST /v1/messages`.
///
/// Deliberately absent, because this model family rejects them with a 400:
/// `temperature`, `top_p`, `top_k`, `thinking.budget_tokens`, and a trailing
/// assistant turn for prefill. Thinking is left unset, which runs the model's
/// adaptive default.
pub fn request_body(facts: &DigestFacts) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "messages": [{
            "role": "user",
            "content": render_facts(facts),
        }],
        "output_config": {
            "effort": EFFORT,
            "format": {
                "type": "json_schema",
                "schema": output_schema(),
            }
        }
    })
}

/// Pull the two sections out of a Messages API response.
///
/// `stop_reason` is checked before `content` is touched: a refusal arrives as
/// HTTP 200 with empty or partial content, and reading `content[0]` first
/// would report it as a parse failure.
pub fn parse_response(body: &Value) -> Result<DigestSections, DigestError> {
    match body.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => {
            let detail = body
                .get("stop_details")
                .and_then(|d| d.get("category"))
                .and_then(Value::as_str)
                .unwrap_or("no category given");
            return Err(DigestError::Refused(detail.to_owned()));
        }
        Some("max_tokens") => {
            return Err(DigestError::Parse(
                "response hit max_tokens before the digest was complete".into(),
            ));
        }
        _ => {}
    }

    // Thinking blocks share the content array with text blocks; only text
    // carries the answer.
    let text: String = body
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
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
                records: 18,
                by_kind: vec![("protest".into(), 12), ("disruption".into(), 6)],
                by_source: vec![("acled".into(), 12), ("ioda".into(), 6)],
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
        assert!(text[evt..].contains("event records: 18"));
        assert!(text[evt..].contains("protest=12"));
    }

    #[test]
    fn withheld_sources_are_declared_as_counted_but_unlisted() {
        let text = render_facts(&facts());
        assert!(text.contains("licence restricts redistribution): acled=12"));
        assert!(text.contains("Treat those counts as real"));
    }

    #[test]
    fn request_body_omits_every_parameter_this_model_rejects() {
        let body = request_body(&facts());
        for banned in ["temperature", "top_p", "top_k", "thinking"] {
            assert!(
                body.get(banned).is_none(),
                "`{banned}` must not be sent to {MODEL}"
            );
        }
        // Prefill (a trailing assistant turn) is rejected too.
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(body["model"], MODEL);
        // Effort and format are nested under output_config, not top-level.
        assert!(body.get("effort").is_none());
        assert_eq!(body["output_config"]["effort"], EFFORT);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
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
    fn parse_response_skips_thinking_blocks() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "weighing the counts"},
                {"type": "text", "text": r#"{"media_attention":"Coverage clustered.","event_data":"Twelve protests."}"#},
            ]
        });
        let out = parse_response(&body).unwrap();
        assert_eq!(out.media_attention, "Coverage clustered.");
        assert_eq!(out.event_data, "Twelve protests.");
    }

    #[test]
    fn parse_response_reports_a_refusal_rather_than_a_parse_failure() {
        let body = json!({
            "stop_reason": "refusal",
            "stop_details": {"category": "policy"},
            "content": []
        });
        let err = parse_response(&body).unwrap_err();
        assert!(
            matches!(&err, DigestError::Refused(c) if c == "policy"),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_response_rejects_a_truncated_response() {
        let body = json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "{\"media_attention\":\"half"}]
        });
        assert!(matches!(
            parse_response(&body).unwrap_err(),
            DigestError::Parse(_)
        ));
    }

    #[test]
    fn parse_response_rejects_an_empty_section() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": r#"{"media_attention":"   ","event_data":"x"}"#}]
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
