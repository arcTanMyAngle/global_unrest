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
pub mod script;
pub mod topic;

use chrono::{DateTime, Utc};
use core_types::{
    ChannelClass, ChatterRollup, EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationRole,
    NormalizeError, SignalFamily, SourceId, event_id,
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
    /// (place index, topic index, channel class, window start) -> post count.
    /// A BTreeMap so `drain` emits rollups in a deterministic order.
    ///
    /// Class is in the **key**, not on the rollup, because this accumulator is
    /// shared across every channel a source sweeps: a monitor's posts and a
    /// combatant's would otherwise be summed before any rollup exists, and no
    /// later field could unpick them. See docs/SIGNAL_MODEL.md.
    counts: std::collections::BTreeMap<(usize, usize, ChannelClass, i64), u32>,
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
    ///
    /// `class` is the provenance of the *channel* the post came from — never
    /// anything about its author. Callers must state it; there is no default,
    /// because assuming `Monitor` would fabricate provenance the source never
    /// asserted. A firehose with no per-channel notion passes
    /// [`ChannelClass::Unspecified`].
    pub fn observe(&mut self, text: &str, ts: DateTime<Utc>, class: ChannelClass) -> bool {
        self.scanned += 1;
        let words = tokenize(text);
        // Scripts that do not delimit words arrive as one whitespace token per
        // clause, so the word path above cannot see inside them; `runs` is
        // empty (and allocates nothing) for everything else. Both borrow
        // `text` for this call only — see the module docs.
        let runs = script::runs(text);
        if words.is_empty() && runs.is_empty() {
            return false;
        }
        let Some(place_idx) = self
            .places
            .find(&words)
            .or_else(|| self.places.find_in_runs(&runs))
        else {
            return false;
        };
        let Some(topic_idx) = self
            .topics
            .find(&words)
            .or_else(|| self.topics.find_in_runs(&runs))
        else {
            return false;
        };
        let window = window_start(ts.timestamp(), self.window_secs);
        *self
            .counts
            .entry((place_idx, topic_idx, class, window))
            .or_insert(0) += 1;
        self.matched += 1;
        true
    }

    /// Drain the counters for every window that has **finished** by `now`,
    /// leaving the in-progress window still accumulating.
    ///
    /// Completed-only is a correctness requirement, not tidiness. A rollup's
    /// derived event id is `(place, topic, class, window_start)`, so publishing a
    /// half-counted window would claim that id; the rest of that window would
    /// then be discarded by storage's dedup-by-id and those posts would
    /// vanish. Draining whole windows only means every window is published
    /// exactly once, with its full count, no matter how often this is called.
    pub fn drain_completed(&mut self, now: DateTime<Utc>) -> Vec<ChatterRollup> {
        let cutoff = window_start(now.timestamp(), self.window_secs);
        // BTreeMap ordering is by (place, topic, window), so a completed
        // window cannot be found by a range scan — partition explicitly.
        let mut completed = std::collections::BTreeMap::new();
        let mut pending = std::collections::BTreeMap::new();
        for (key, count) in std::mem::take(&mut self.counts) {
            if key.3 < cutoff {
                completed.insert(key, count);
            } else {
                pending.insert(key, count);
            }
        }
        self.counts = pending;
        self.rollups_from(completed)
    }

    /// Drain everything, finished or not. Tests and shutdown paths only —
    /// live callers want [`ChatterAccumulator::drain_completed`].
    pub fn drain_all(&mut self) -> Vec<ChatterRollup> {
        let counts = std::mem::take(&mut self.counts);
        self.rollups_from(counts)
    }

    fn rollups_from(
        &self,
        counts: std::collections::BTreeMap<(usize, usize, ChannelClass, i64), u32>,
    ) -> Vec<ChatterRollup> {
        counts
            .into_iter()
            .map(
                |((place_idx, topic_idx, channel_class, window), post_count)| {
                    let place = self.places.place(place_idx);
                    ChatterRollup {
                        place_name: place.name.clone(),
                        country_iso: place.country_iso.clone(),
                        lat: place.lat,
                        lon: place.lon,
                        precision: place.precision,
                        topic: self.topics.label(topic_idx).to_owned(),
                        channel_class,
                        window_start_epoch_s: window,
                        window_secs: self.window_secs,
                        post_count,
                    }
                },
            )
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
/// Chatter is its **own family** ([`SignalFamily::Chatter`]), not media
/// attention and not a discrete event. It was previously written as
/// [`EventKind::NewsAttention`], which is how post volume reached article
/// totals, outlet diversity and the Daily Events attention section — "16
/// media-attention records across zero identified outlet domains". Post
/// volume lands in `volume_count`, counted in posts, and enters neither the
/// unrest score nor the generic spike baseline.
///
/// No headline is written. A generated summary is a claim the row cannot
/// support; the UI composes its label from the rollup's own place, topic and
/// count at render time.
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

    // Class is part of the id: two class-specific rollups for the same
    // place/topic/window are different observations, and without it the
    // second would be discarded by storage's dedup-by-id.
    let source_event_id = format!(
        "{}-{}-{}-{}",
        rollup.place_name,
        rollup.topic,
        rollup.channel_class.as_str(),
        rollup.window_start_epoch_s
    );
    let ev = GeoTemporalEvent {
        id: event_id(source, &source_event_id),
        source,
        source_event_id,
        family: SignalFamily::Chatter,
        kind: EventKind::Chatter,
        themes: vec!["chatter".to_owned(), rollup.topic.clone()],
        ts_utc,
        ingested_at: Utc::now(),
        lat: rollup.lat,
        lon: rollup.lon,
        // A place named in posts, never a location taken from any person.
        location_role: LocationRole::MentionedPlace,
        location_precision: rollup.precision,
        // Keyword place-matching is deliberately crude; say so in the number
        // the UI already shows rather than implying gazetteer-grade accuracy.
        location_confidence: 0.5,
        country_iso: rollup.country_iso.clone(),
        admin1: None,
        h3_cell,
        // Posts, per the family's volume unit — not articles.
        volume_count: rollup.post_count,
        // One stream, so there is exactly one "outlet" behind every rollup.
        distinct_source_count: 1,
        severity: None,
        // Deliberately none: see this function's docs.
        headline: None,
        outlet_domains: Vec::new(),
        urls: Vec::new(),
    };
    ev.validate()?;
    Ok(vec![ev])
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
        assert!(!acc.observe(
            "just posted a photo of my lunch",
            ts(0),
            ChannelClass::Unspecified
        ));
        // Place with no topic.
        assert!(!acc.observe(
            "landed in Kyiv this morning",
            ts(0),
            ChannelClass::Unspecified
        ));
        // Topic with no place.
        assert!(!acc.observe(
            "there is a protest happening",
            ts(0),
            ChannelClass::Unspecified
        ));
        // Both.
        assert!(acc.observe(
            "big protest in Kyiv right now",
            ts(0),
            ChannelClass::Unspecified
        ));
        assert_eq!(acc.scanned(), 4);
        assert_eq!(acc.matched(), 1);
    }

    #[test]
    fn counts_group_by_place_topic_and_window_then_drain() {
        let mut acc = accumulator();
        // Same window (300s): two posts about the same place and topic.
        acc.observe("protest in Kyiv", ts(1_000), ChannelClass::Unspecified);
        acc.observe(
            "another protest in Kyiv",
            ts(1_100),
            ChannelClass::Unspecified,
        );
        // Same place and topic, next window.
        acc.observe(
            "protest in Kyiv again",
            ts(1_000 + 300),
            ChannelClass::Unspecified,
        );
        // Same window, different topic.
        acc.observe("flooding in Kyiv", ts(1_000), ChannelClass::Unspecified);

        let rollups = acc.drain_all();
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
        assert!(acc.drain_all().is_empty());
        assert_eq!(acc.matched(), 4);
    }

    #[test]
    fn drain_completed_leaves_the_in_progress_window_alone() {
        let mut acc = accumulator();
        // Window [900, 1200) is finished; [1200, 1500) is still running.
        acc.observe("protest in Kyiv", ts(1_000), ChannelClass::Unspecified);
        acc.observe("protest in Kyiv", ts(1_250), ChannelClass::Unspecified);

        let now = ts(1_300);
        let first = acc.drain_completed(now);
        assert_eq!(first.len(), 1, "only the finished window drains");
        assert_eq!(first[0].window_start_epoch_s, 900);

        // Draining again mid-window publishes nothing, so the running window
        // cannot be published half-counted and then lost to dedup-by-id.
        assert!(acc.drain_completed(now).is_empty());

        // More posts land in the still-open window and are not lost.
        acc.observe("protest in Kyiv", ts(1_400), ChannelClass::Unspecified);
        let second = acc.drain_completed(ts(1_600));
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].window_start_epoch_s, 1_200);
        assert_eq!(second[0].post_count, 2, "both posts in that window");
    }

    /// The end-to-end reason `script` exists: a post in an unsegmented script
    /// counts, and it produces the same `(place, topic, window)` rollup a
    /// Latin post would. The output contract does not move.
    #[test]
    fn posts_in_unsegmented_scripts_count() {
        let mut acc = accumulator();
        // Burmese: "an earthquake struck in Yangon".
        assert!(acc.observe("ရန်ကုန်မြို့မှာ ငလျင်လှုပ်ခဲ့သည်", ts(0), ChannelClass::Unspecified));
        // Japanese, no spaces at all: "residents evacuated for the typhoon".
        assert!(acc.observe(
            "東京で台風のため住民が避難した",
            ts(0),
            ChannelClass::Unspecified
        ));
        // Thai: "flooding in Bangkok".
        assert!(acc.observe("น้ำท่วมกรุงเทพมหานคร", ts(0), ChannelClass::Unspecified));
        // A place with no topic still does not count, in any script.
        assert!(!acc.observe("東京の天気はいいですね", ts(0), ChannelClass::Unspecified));

        let rollups = acc.drain_all();
        assert_eq!(rollups.len(), 3);
        let yangon = rollups.iter().find(|r| r.place_name == "Yangon").unwrap();
        assert_eq!(yangon.topic, "earthquake");
        assert_eq!(yangon.post_count, 1);
        // The rollup carries gazetteer values, never anything from the post.
        assert_eq!(yangon.country_iso, "MMR");
        assert_eq!(yangon.precision, LocationPrecision::City);
    }

    /// A mixed-script post prefers the word path, so adding the script tables
    /// cannot change which place an existing post resolved to.
    #[test]
    fn the_word_path_wins_on_a_mixed_post() {
        let m = matcher();
        let text = "protest in Kyiv, reported from 東京";
        let words = tokenize(text);
        let idx = m
            .find(&words)
            .or_else(|| m.find_in_runs(&script::runs(text)))
            .unwrap();
        assert_eq!(m.place(idx).name, "Kyiv");
    }

    #[test]
    fn one_place_and_one_topic_per_post() {
        let mut acc = accumulator();
        // Three places and two topics in one post still counts exactly once.
        acc.observe(
            "protest and flooding in Kyiv, Berlin and Sudan",
            ts(0),
            ChannelClass::Unspecified,
        );
        let rollups = acc.drain_all();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].post_count, 1);
    }

    /// What one post costs, against the real tables. Not a gate — CI has no
    /// stable performance floor — so it is `#[ignore]`d and run by hand:
    /// `cargo test -p chatter --release observe_cost -- --ignored --nocapture`.
    /// The numbers this printed are recorded in docs/DATA_MODEL.md.
    #[test]
    #[ignore = "timing measurement, not a correctness gate"]
    fn observe_cost() {
        let mut acc = accumulator();
        let cases = [
            (
                "latin, no match",
                "an ordinary english post about lunch and the weather today ".repeat(4),
            ),
            ("latin, match", "big protest in Kyiv right now ".repeat(8)),
            (
                "cjk, no match",
                "今日はとてもいい天気で、公園を歩いてきました。".repeat(4),
            ),
            (
                "cjk, match",
                "東京で台風のため住民が避難しています。".repeat(4),
            ),
            ("burmese, match", "ရန်ကုန်မြို့မှာ ငလျင်လှုပ်ခဲ့သည် ".repeat(4)),
        ];
        let places = matcher();
        let topics = TopicMatcher::new();
        let iterations = 50_000;
        for (label, text) in &cases {
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(acc.observe(
                    std::hint::black_box(text),
                    ts(0),
                    ChannelClass::Unspecified,
                ));
            }
            let whole = start.elapsed() / iterations;

            // The script path on its own, to separate what segmentation costs
            // from what the pre-existing word-window path already cost.
            let start = std::time::Instant::now();
            for _ in 0..iterations {
                let runs = script::runs(std::hint::black_box(text));
                std::hint::black_box(places.find_in_runs(&runs));
                std::hint::black_box(topics.find_in_runs(&runs));
            }
            let scripts = start.elapsed() / iterations;

            println!(
                "{label}: {} chars, {whole:?}/post observe, of which {scripts:?} is the script path",
                text.chars().count()
            );
        }
    }

    #[test]
    fn normalize_produces_a_chatter_event_with_no_post_content() {
        let rollup = ChatterRollup {
            place_name: "Kyiv".into(),
            country_iso: "UKR".into(),
            lat: 50.45,
            lon: 30.52,
            precision: LocationPrecision::City,
            topic: "protest".into(),
            channel_class: ChannelClass::Unspecified,
            window_start_epoch_s: 1_786_500_000,
            window_secs: 300,
            post_count: 42,
        };
        let events = normalize_rollup(&rollup, SourceId::Bluesky).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        // Chatter is its own family — not media attention, not an event.
        assert_eq!(e.family, SignalFamily::Chatter);
        assert_eq!(e.kind, EventKind::Chatter);
        assert_eq!(e.volume_count, 42);
        assert_eq!(e.source, SourceId::Bluesky);
        assert_eq!(e.themes, vec!["chatter", "protest"]);
        assert_eq!(e.ts_utc.timestamp(), 1_786_500_000);
        // No per-post identifiers or content can reach storage.
        assert!(e.urls.is_empty());
        assert!(e.outlet_domains.is_empty());
        assert!(e.headline.is_none());

        // Stable id: same rollup re-ingested is idempotent.
        let again = normalize_rollup(&rollup, SourceId::Bluesky).unwrap();
        assert_eq!(again[0].id, e.id);
    }

    fn rollup_of(class: ChannelClass, post_count: u32) -> ChatterRollup {
        ChatterRollup {
            place_name: "Kyiv".into(),
            country_iso: "UKR".into(),
            lat: 50.45,
            lon: 30.52,
            precision: LocationPrecision::City,
            topic: "protest".into(),
            channel_class: class,
            window_start_epoch_s: 900,
            window_secs: 300,
            post_count,
        }
    }

    /// Monitor and combatant volume must not be summed. They are summed
    /// inside the accumulator unless class is part of its key — no field on
    /// the rollup could separate them after the fact.
    #[test]
    fn channel_class_separates_counts_before_any_rollup_exists() {
        let mut acc = accumulator();
        acc.observe("protest in Kyiv", ts(1_000), ChannelClass::Monitor);
        acc.observe("protest in Kyiv", ts(1_050), ChannelClass::Monitor);
        acc.observe("protest in Kyiv", ts(1_100), ChannelClass::Combatant);

        let mut rollups = acc.drain_all();
        rollups.sort_by_key(|r| r.channel_class);
        assert_eq!(rollups.len(), 2, "same place/topic/window, two classes");
        let monitor = rollups
            .iter()
            .find(|r| r.channel_class == ChannelClass::Monitor)
            .unwrap();
        let combatant = rollups
            .iter()
            .find(|r| r.channel_class == ChannelClass::Combatant)
            .unwrap();
        assert_eq!(monitor.post_count, 2);
        assert_eq!(combatant.post_count, 1);
    }

    /// Two class-specific rollups for the same place/topic/window are
    /// different observations; if class were left out of the derived id the
    /// second would be silently dropped by storage's dedup-by-id.
    #[test]
    fn class_is_part_of_the_derived_event_id() {
        let a = normalize_rollup(&rollup_of(ChannelClass::Monitor, 2), SourceId::Telegram).unwrap();
        let b =
            normalize_rollup(&rollup_of(ChannelClass::Combatant, 1), SourceId::Telegram).unwrap();
        assert_ne!(a[0].id, b[0].id);
        assert_ne!(a[0].source_event_id, b[0].source_event_id);
    }

    /// The defect this whole split exists to remove: chatter was stored as
    /// `NewsAttention` with a synthetic headline and post counts in the
    /// article column.
    #[test]
    fn chatter_is_never_media_attention() {
        let ev = &normalize_rollup(&rollup_of(ChannelClass::Unspecified, 7), SourceId::Bluesky)
            .unwrap()[0];
        assert_eq!(ev.family, SignalFamily::Chatter);
        assert_eq!(ev.kind, EventKind::Chatter);
        assert!(!ev.family.enters_attention());
        assert!(!ev.family.enters_unrest());
        assert!(!ev.family.enters_generic_spike());
        assert!(!ev.family.in_digest());
        assert_eq!(ev.volume_count, 7, "posts, not articles");
        assert_eq!(ev.family.volume_unit(), core_types::VolumeUnit::Posts);
        assert_eq!(ev.location_role, LocationRole::MentionedPlace);
        assert!(
            ev.headline.is_none(),
            "no synthetic headline — the UI composes its own label"
        );
        assert!(ev.outlet_domains.is_empty());
        assert!(ev.validate().is_ok());
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
            channel_class: ChannelClass::Unspecified,
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
