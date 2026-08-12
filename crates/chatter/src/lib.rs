//! Aggregate-only chatter accumulation for streaming social sources.
//!
//! Every other source in this workspace stores one record per upstream
//! record and aggregates later in `storage::score_buckets`. Streaming social
//! sources invert that: posts are counted **in memory as they stream past**
//! and only the periodic rollup is ever stored.
//!
//! That inversion is a safety requirement, not a performance choice.
//! Bluesky posts and Telegram channel messages about live unrest are often
//! written by the protesters, journalists, and dissidents inside those
//! events, for whom being identified can be dangerous. This crate therefore
//! guarantees, by construction:
//!
//! - post text is borrowed for the duration of one [`ChatterAccumulator::observe`]
//!   call and never stored, copied into a field, or logged;
//! - author handles, DIDs, user ids, post ids, and URLs are never passed in
//!   at all — [`ChatterAccumulator::observe`] takes only text and a timestamp;
//! - the only thing that leaves this crate is a [`ChatterRollup`]: a count
//!   for a (place, topic, window) triple.
//!
//! Place attribution is crude word matching against real gazetteer names
//! (see [`place`]), never inferred from writing style, language, or content.
//! See docs/SAFETY_AND_PRIVACY.md.

pub mod place;
pub mod topic;

use chrono::{DateTime, Utc};
use core_types::{
    ChatterRollup, EventKind, GeoTemporalEvent, H3_RESOLUTION, NormalizeError, SourceId, event_id,
};

pub use place::{Place, PlaceMatcher};
pub use topic::{Topic, TopicMatcher};

/// Natural Earth 1:110m countries and major populated places (public domain),
/// bundled so the matcher needs no network access and no hand-typed
/// coordinate table.
pub const NE_COUNTRIES: &str =
    include_str!("../../../assets/natural_earth/ne_110m_admin_0_countries.geojson");
pub const NE_PLACES: &str =
    include_str!("../../../assets/natural_earth/ne_110m_populated_places_simple.geojson");

/// Default flush cadence: aggregates cover five minutes of stream.
///
/// Short enough that a burst is visible while it is still news, long enough
/// that one window holds a meaningful count rather than statistical noise.
pub const DEFAULT_WINDOW_SECS: i64 = 300;

/// Split text into lowercase alphanumeric words.
///
/// Unicode-aware `to_lowercase`/`is_alphanumeric`, so "Côte" stays one token
/// and matches the Natural Earth spelling directly. No diacritic folding is
/// attempted: Natural Earth already publishes ASCII transliterations for city
/// names (`nameascii`), and the one accented country name has an explicit
/// ASCII alias, so both spellings are in the table already.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Scan `words` left to right, trying the longest window first at each
/// position, and return the first successful `lookup`.
///
/// Longest-first matters: "united states" must win over "united" alone, and
/// "general strike" over "strike".
pub(crate) fn find_window<T>(
    words: &[String],
    max_words: usize,
    lookup: impl Fn(&Vec<String>) -> Option<T>,
) -> Option<T> {
    for start in 0..words.len() {
        let longest = max_words.min(words.len() - start);
        for len in (1..=longest).rev() {
            let window = words[start..start + len].to_vec();
            if let Some(hit) = lookup(&window) {
                return Some(hit);
            }
        }
    }
    None
}

/// Floor `epoch_secs` to the start of its flush window.
pub fn window_start(epoch_secs: i64, window_secs: i64) -> i64 {
    if window_secs <= 0 {
        return epoch_secs;
    }
    epoch_secs.div_euclid(window_secs) * window_secs
}

/// In-memory counter over a live post stream. See the module docs for the
/// privacy guarantees this type exists to enforce.
pub struct ChatterAccumulator {
    places: PlaceMatcher,
    topics: TopicMatcher,
    window_secs: i64,
    /// (place index, topic index, window start) -> post count. A BTreeMap so
    /// `drain` emits rollups in a deterministic order.
    counts: std::collections::BTreeMap<(usize, usize, i64), u32>,
    scanned: u64,
    matched: u64,
}

impl ChatterAccumulator {
    pub fn new(places: PlaceMatcher, topics: TopicMatcher, window_secs: i64) -> Self {
        Self {
            places,
            topics,
            window_secs,
            counts: std::collections::BTreeMap::new(),
            scanned: 0,
            matched: 0,
        }
    }

    /// Build over the bundled Natural Earth data.
    pub fn from_bundled(window_secs: i64) -> Result<Self, geo_utils::GeoError> {
        let countries = geo_utils::CountryIndex::from_geojson_str(NE_COUNTRIES)?;
        let cities = geo_utils::CityIndex::from_geojson_str(NE_PLACES)?;
        Ok(Self::new(
            PlaceMatcher::from_indexes(&countries, &cities),
            TopicMatcher::new(),
            window_secs,
        ))
    }

    /// Count one post.
    ///
    /// `text` is borrowed for this call only — nothing derived from it is
    /// retained beyond an integer counter. Returns whether it matched.
    ///
    /// A post counts only if it names both a known place and a known topic;
    /// one place and one topic per post, so a post cannot inflate several
    /// aggregates at once.
    pub fn observe(&mut self, text: &str, ts: DateTime<Utc>) -> bool {
        self.scanned += 1;
        let words = tokenize(text);
        if words.is_empty() {
            return false;
        }
        let Some(place_idx) = self.places.find(&words) else {
            return false;
        };
        let Some(topic_idx) = self.topics.find(&words) else {
            return false;
        };
        let window = window_start(ts.timestamp(), self.window_secs);
        *self
            .counts
            .entry((place_idx, topic_idx, window))
            .or_insert(0) += 1;
        self.matched += 1;
        true
    }

    /// Drain the counters into rollups, emptying the accumulator.
    pub fn drain(&mut self) -> Vec<ChatterRollup> {
        let counts = std::mem::take(&mut self.counts);
        counts
            .into_iter()
            .map(|((place_idx, topic_idx, window), post_count)| {
                let place = self.places.place(place_idx);
                ChatterRollup {
                    place_name: place.name.clone(),
                    country_iso: place.country_iso.clone(),
                    lat: place.lat,
                    lon: place.lon,
                    precision: place.precision,
                    topic: self.topics.label(topic_idx).to_owned(),
                    window_start_epoch_s: window,
                    window_secs: self.window_secs,
                    post_count,
                }
            })
            .collect()
    }

    /// Posts scanned since construction (the denominator behind the counts).
    pub fn scanned(&self) -> u64 {
        self.scanned
    }

    /// Posts that matched a place and a topic since construction.
    pub fn matched(&self) -> u64 {
        self.matched
    }

    /// Rollups currently pending a flush.
    pub fn pending(&self) -> usize {
        self.counts.len()
    }
}

/// Turn one rollup into the workspace's normalized event shape.
///
/// Chatter volume is an **attention** observation, the same class as GDELT's
/// article counts — never a discrete event record. `article_count` carries
/// the post count, and the headline is a generated summary of the aggregate;
/// no post text can reach it.
pub fn normalize_rollup(
    rollup: &ChatterRollup,
    source: SourceId,
) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
    let h3_cell =
        geo_utils::cell_for_latlon(rollup.lat, rollup.lon, H3_RESOLUTION).map_err(|e| {
            NormalizeError::InvalidValue {
                field: "location",
                detail: format!("h3 assignment failed: {e}"),
            }
        })?;
    let ts_utc = DateTime::from_timestamp(rollup.window_start_epoch_s, 0).ok_or(
        NormalizeError::InvalidValue {
            field: "window_start_epoch_s",
            detail: format!(
                "out-of-range unix timestamp `{}`",
                rollup.window_start_epoch_s
            ),
        },
    )?;
    if rollup.post_count == 0 {
        return Ok(Vec::new());
    }

    let source_event_id = format!(
        "{}-{}-{}",
        rollup.place_name, rollup.topic, rollup.window_start_epoch_s
    );
    Ok(vec![GeoTemporalEvent {
        id: event_id(source, &source_event_id),
        source,
        source_event_id,
        kind: EventKind::NewsAttention,
        themes: vec!["chatter".to_owned(), rollup.topic.clone()],
        ts_utc,
        ingested_at: Utc::now(),
        lat: rollup.lat,
        lon: rollup.lon,
        location_precision: rollup.precision,
        // Keyword place-matching is deliberately crude; say so in the number
        // the UI already shows rather than implying gazetteer-grade accuracy.
        location_confidence: 0.5,
        country_iso: rollup.country_iso.clone(),
        admin1: None,
        h3_cell,
        article_count: rollup.post_count,
        // One stream, so there is exactly one "outlet" behind every rollup.
        distinct_source_count: 1,
        severity: None,
        headline: Some(format!(
            "{} posts mentioned {} + {}",
            rollup.post_count, rollup.place_name, rollup.topic
        )),
        outlet_domains: Vec::new(),
        urls: Vec::new(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::LocationPrecision;
    use std::sync::OnceLock;

    fn accumulator() -> ChatterAccumulator {
        ChatterAccumulator::from_bundled(DEFAULT_WINDOW_SECS).unwrap()
    }

    fn matcher() -> &'static PlaceMatcher {
        static M: OnceLock<PlaceMatcher> = OnceLock::new();
        M.get_or_init(|| {
            let countries = geo_utils::CountryIndex::from_geojson_str(NE_COUNTRIES).unwrap();
            let cities = geo_utils::CityIndex::from_geojson_str(NE_PLACES).unwrap();
            PlaceMatcher::from_indexes(&countries, &cities)
        })
    }

    fn ts(epoch: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(epoch, 0).unwrap()
    }

    fn place_of(text: &str) -> Option<&'static Place> {
        let m = matcher();
        m.find(&tokenize(text)).map(|i| m.place(i))
    }

    #[test]
    fn tokenize_splits_on_punctuation_and_keeps_accents() {
        assert_eq!(tokenize("Protest in Kyiv!"), vec!["protest", "in", "kyiv"]);
        assert_eq!(tokenize("Côte d'Ivoire"), vec!["côte", "d", "ivoire"]);
        assert!(tokenize("   ...  ").is_empty());
    }

    #[test]
    fn country_wins_a_token_shared_with_a_city() {
        // "Panama" is both a country name and an alt name of Panama City.
        let place = place_of("panama").unwrap();
        assert_eq!(place.precision, LocationPrecision::Country);
        assert_eq!(place.country_iso, "PAN");
        // A city with no country collision still resolves at City precision.
        let kyiv = place_of("kyiv").unwrap();
        assert_eq!(kyiv.precision, LocationPrecision::City);
    }

    #[test]
    fn aliases_resolve_to_real_natural_earth_centroids() {
        let usa = place_of("usa").unwrap();
        assert_eq!(usa.country_iso, "USA");
        assert_eq!(place_of("united states").unwrap().country_iso, "USA");
        assert_eq!(place_of("britain").unwrap().country_iso, "GBR");
        assert_eq!(place_of("ivory coast").unwrap().country_iso, "CIV");
        assert_eq!(place_of("cote d ivoire").unwrap().country_iso, "CIV");
        // Longest-window matching: "united states" must not stop at a
        // shorter token, and the coordinate is a real one.
        assert!(usa.lat > 20.0 && usa.lat < 60.0, "lat {}", usa.lat);
        assert!(usa.lon < -60.0, "lon {}", usa.lon);
    }

    #[test]
    fn ambiguous_tokens_are_dropped_and_us_is_not_an_alias() {
        for token in ["male", "chad", "jordan", "georgia"] {
            assert!(place_of(token).is_none(), "{token} should be dropped");
        }
        // "us" is a pronoun, never the United States.
        assert!(place_of("us").is_none());
    }

    #[test]
    fn observe_requires_both_a_place_and_a_topic() {
        let mut acc = accumulator();
        assert!(!acc.observe("just posted a photo of my lunch", ts(0)));
        // Place with no topic.
        assert!(!acc.observe("landed in Kyiv this morning", ts(0)));
        // Topic with no place.
        assert!(!acc.observe("there is a protest happening", ts(0)));
        // Both.
        assert!(acc.observe("big protest in Kyiv right now", ts(0)));
        assert_eq!(acc.scanned(), 4);
        assert_eq!(acc.matched(), 1);
    }

    #[test]
    fn counts_group_by_place_topic_and_window_then_drain() {
        let mut acc = accumulator();
        // Same window (300s): two posts about the same place and topic.
        acc.observe("protest in Kyiv", ts(1_000));
        acc.observe("another protest in Kyiv", ts(1_100));
        // Same place and topic, next window.
        acc.observe("protest in Kyiv again", ts(1_000 + 300));
        // Same window, different topic.
        acc.observe("flooding in Kyiv", ts(1_000));

        let rollups = acc.drain();
        assert_eq!(rollups.len(), 3);
        let protest_first: Vec<_> = rollups
            .iter()
            .filter(|r| r.topic == "protest" && r.window_start_epoch_s == 900)
            .collect();
        assert_eq!(protest_first.len(), 1);
        assert_eq!(protest_first[0].post_count, 2);
        assert_eq!(protest_first[0].place_name, "Kyiv");
        assert_eq!(protest_first[0].window_secs, 300);

        // Draining empties the accumulator; the running totals survive.
        assert_eq!(acc.pending(), 0);
        assert!(acc.drain().is_empty());
        assert_eq!(acc.matched(), 4);
    }

    #[test]
    fn one_place_and_one_topic_per_post() {
        let mut acc = accumulator();
        // Three places and two topics in one post still counts exactly once.
        acc.observe("protest and flooding in Kyiv, Berlin and Sudan", ts(0));
        let rollups = acc.drain();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].post_count, 1);
    }

    #[test]
    fn normalize_produces_an_attention_event_with_no_post_content() {
        let rollup = ChatterRollup {
            place_name: "Kyiv".into(),
            country_iso: "UKR".into(),
            lat: 50.45,
            lon: 30.52,
            precision: LocationPrecision::City,
            topic: "protest".into(),
            window_start_epoch_s: 1_786_500_000,
            window_secs: 300,
            post_count: 42,
        };
        let events = normalize_rollup(&rollup, SourceId::Bluesky).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // Chatter is an attention observation, never a discrete event.
        assert_eq!(e.kind, EventKind::NewsAttention);
        assert!(e.kind.is_attention());
        assert_eq!(e.article_count, 42);
        assert_eq!(e.source, SourceId::Bluesky);
        assert_eq!(e.themes, vec!["chatter", "protest"]);
        assert_eq!(e.ts_utc.timestamp(), 1_786_500_000);
        // No per-post identifiers or content can reach storage.
        assert!(e.urls.is_empty());
        assert!(e.outlet_domains.is_empty());
        assert_eq!(
            e.headline.as_deref(),
            Some("42 posts mentioned Kyiv + protest")
        );

        // Stable id: same rollup re-ingested is idempotent.
        let again = normalize_rollup(&rollup, SourceId::Bluesky).unwrap();
        assert_eq!(again[0].id, e.id);
    }

    #[test]
    fn zero_count_and_bad_coordinates_never_become_events() {
        let mut rollup = ChatterRollup {
            place_name: "Nowhere".into(),
            country_iso: "UNK".into(),
            lat: 0.0,
            lon: 0.0,
            precision: LocationPrecision::Country,
            topic: "protest".into(),
            window_start_epoch_s: 0,
            window_secs: 300,
            post_count: 0,
        };
        assert!(
            normalize_rollup(&rollup, SourceId::Bluesky)
                .unwrap()
                .is_empty()
        );

        rollup.post_count = 5;
        rollup.lat = 999.0;
        assert!(normalize_rollup(&rollup, SourceId::Bluesky).is_err());
    }
}
