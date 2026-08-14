//! Topic keywords: the "what is this about" half of a chatter match.
//!
//! Deliberately a small, fixed, auditable table rather than anything learned
//! or inferred. A post only contributes to an aggregate if it mentions a
//! known place **and** a known topic, so this list is also the main defence
//! against place-name false positives ("Turkey" in a recipe post).

/// A named topic and the keywords that count as mentioning it.
///
/// Multi-word keywords are written space-separated and matched as a word
/// window, so "tear gas" matches the two adjacent words, not the substring.
pub struct Topic {
    pub label: &'static str,
    pub keywords: &'static [&'static str],
}

/// The topic table.
///
/// Seeded from the same signal classes the GDELT DOC query already tracks
/// (protest / unrest / flood / earthquake / wildfire / election / strike) so
/// chatter is comparable against the existing attention layer, plus an
/// `outage` topic that pairs with the IODA layer. Widened 2026-08-13 with the
/// hazard and violence classes the original nine missed entirely — storm,
/// volcano, landslide, drought, displacement, explosion, outbreak, crime —
/// each of which was silently discarding every post that named only it.
///
/// Every entry here is pure table growth. Adding topics changes what gets
/// counted, never what gets kept: the output is still a
/// `(place, topic, window)` count and no keyword can widen that.
///
/// Keyword choices worth knowing about:
/// - "march" is excluded from `protest` — it is a month and a common verb.
/// - "polls" is excluded from `election` — it is as often opinion polling.
/// - "strike" is kept despite airstrike/lightning/bowling senses, because a
///   place match is always required alongside it.
/// - bare "storm" is excluded — "storm the building", "storm out" — while the
///   named storm types ("hurricane", "typhoon", "cyclone") are unambiguous.
/// - bare "landslide" is excluded because an election landslide is at least as
///   common in political posts as a hillside one; the plural and "mudslide"
///   are not used that way.
/// - "fire" is excluded from `wildfire` — gunfire, firing, fire someone; the
///   compound forms carry the meaning.
///
/// Where two topics could claim the same words, the longer keyword wins,
/// because matching tries the longest window first at each position: "drone
/// strike" is `conflict`, a bare "strike" is `strike`.
pub const TOPICS: &[Topic] = &[
    Topic {
        label: "protest",
        keywords: &[
            "protest",
            "protests",
            "protester",
            "protesters",
            "protestor",
            "protestors",
            "demonstration",
            "demonstrations",
            "demonstrators",
            "protesting",
            "rally",
            "riot",
            "riots",
            "rioting",
            "uprising",
        ],
    },
    Topic {
        label: "unrest",
        keywords: &[
            "unrest",
            "clashes",
            "crackdown",
            "curfew",
            "tear gas",
            "state of emergency",
            "martial law",
            "looting",
            "riot police",
            "stampede",
        ],
    },
    Topic {
        label: "conflict",
        keywords: &[
            "airstrike",
            "airstrikes",
            "shelling",
            "ceasefire",
            "militants",
            "insurgents",
            "offensive",
            "counteroffensive",
            "drone strike",
            "drone strikes",
            "rocket attack",
            "artillery",
            "mortar",
            "gunfire",
            "ambush",
        ],
    },
    Topic {
        label: "flood",
        keywords: &[
            "flood",
            "floods",
            "flooding",
            "floodwaters",
            "inundated",
            "flash flood",
            "flash floods",
            "deluge",
        ],
    },
    Topic {
        label: "earthquake",
        keywords: &[
            "earthquake",
            "quake",
            "aftershock",
            "aftershocks",
            "tremor",
            "epicentre",
            "epicenter",
            "seismic",
        ],
    },
    Topic {
        label: "wildfire",
        keywords: &[
            "wildfire",
            "wildfires",
            "bushfire",
            "bushfires",
            "forest fire",
            "forest fires",
            "brush fire",
            "brush fires",
        ],
    },
    Topic {
        label: "storm",
        keywords: &[
            "hurricane",
            "hurricanes",
            "typhoon",
            "typhoons",
            "cyclone",
            "cyclones",
            "tornado",
            "tornadoes",
            "storm surge",
        ],
    },
    Topic {
        label: "volcano",
        keywords: &[
            "volcano",
            "volcanic",
            "eruption",
            "erupting",
            "ashfall",
            "lava",
            "pyroclastic",
        ],
    },
    Topic {
        label: "landslide",
        keywords: &[
            "landslides",
            "mudslide",
            "mudslides",
            "rockslide",
            "rockslides",
            "debris flow",
        ],
    },
    Topic {
        label: "drought",
        keywords: &["drought", "droughts", "famine", "water shortage"],
    },
    Topic {
        label: "displacement",
        keywords: &[
            "refugees",
            "displaced",
            "evacuation",
            "evacuations",
            "evacuated",
            "evacuating",
            "fleeing",
        ],
    },
    Topic {
        label: "explosion",
        keywords: &[
            "explosion",
            "explosions",
            "blast",
            "detonated",
            "car bomb",
            "suicide bombing",
            "ied",
        ],
    },
    Topic {
        label: "outbreak",
        keywords: &[
            "outbreak",
            "epidemic",
            "cholera",
            "measles",
            "quarantine",
            "vaccination campaign",
        ],
    },
    Topic {
        label: "crime",
        keywords: &[
            "cartel",
            "cartels",
            "narco",
            "gang violence",
            "kidnapping",
            "extortion",
            "homicides",
        ],
    },
    Topic {
        label: "election",
        keywords: &[
            "election",
            "elections",
            "ballot",
            "recount",
            "runoff",
            "referendum",
            "electoral",
            "polling station",
            "polling stations",
        ],
    },
    Topic {
        label: "strike",
        keywords: &[
            "strike",
            "strikes",
            "strikers",
            "walkout",
            "picket",
            "general strike",
            "industrial action",
            "work stoppage",
        ],
    },
    Topic {
        label: "outage",
        keywords: &[
            "outage",
            "outages",
            "blackout",
            "blackouts",
            "internet shutdown",
            "power cut",
            "power cuts",
            "grid failure",
        ],
    },
];

/// Word-window lookup over [`TOPICS`].
pub struct TopicMatcher {
    /// Keyword words -> index into `labels`.
    by_keyword: std::collections::HashMap<Vec<String>, usize>,
    labels: Vec<&'static str>,
    max_words: usize,
}

impl Default for TopicMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl TopicMatcher {
    pub fn new() -> Self {
        let mut by_keyword = std::collections::HashMap::new();
        let mut labels = Vec::with_capacity(TOPICS.len());
        let mut max_words = 1;
        for topic in TOPICS {
            let idx = labels.len();
            labels.push(topic.label);
            for keyword in topic.keywords {
                let words: Vec<String> = keyword.split_whitespace().map(str::to_owned).collect();
                if words.is_empty() {
                    continue;
                }
                max_words = max_words.max(words.len());
                by_keyword.entry(words).or_insert(idx);
            }
        }
        Self {
            by_keyword,
            labels,
            max_words,
        }
    }

    pub fn label(&self, idx: usize) -> &'static str {
        self.labels[idx]
    }

    /// First topic mentioned in `words`, scanning left to right and trying
    /// the longest keyword window first. Returns its index into `labels`.
    pub fn find(&self, words: &[String]) -> Option<usize> {
        crate::find_window(words, self.max_words, |w| self.by_keyword.get(w).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn label_of(text: &str) -> Option<&'static str> {
        let m = TopicMatcher::new();
        m.find(&crate::tokenize(text)).map(|i| m.label(i))
    }

    #[test]
    fn labels_are_unique() {
        let mut seen = HashSet::new();
        for topic in TOPICS {
            assert!(seen.insert(topic.label), "duplicate label {}", topic.label);
        }
    }

    /// A keyword listed under one topic but already claimed by an earlier one
    /// is silently ignored — `by_keyword` keeps the first insertion. Without
    /// this test a duplicate would look like it worked and quietly count
    /// toward the wrong topic forever.
    #[test]
    fn every_keyword_resolves_to_its_own_topic() {
        for topic in TOPICS {
            for keyword in topic.keywords {
                assert_eq!(
                    label_of(keyword),
                    Some(topic.label),
                    "keyword `{keyword}` does not resolve to `{}`",
                    topic.label
                );
            }
        }
    }

    /// Longest-window matching is what keeps overlapping keywords apart, and
    /// it is the reason "drone strike" can live under `conflict` while a bare
    /// "strike" stays its own topic.
    #[test]
    fn longer_keywords_win_over_the_shorter_ones_inside_them() {
        assert_eq!(label_of("a drone strike hit the depot"), Some("conflict"));
        assert_eq!(label_of("rail workers on strike"), Some("strike"));
        assert_eq!(label_of("a general strike was called"), Some("strike"));
        assert_eq!(label_of("flash flood warning"), Some("flood"));
        assert_eq!(label_of("power cut across the city"), Some("outage"));
    }

    /// The documented exclusions, pinned so a later widening pass has to
    /// delete the reasoning before it can delete the behaviour.
    #[test]
    fn ambiguous_words_are_not_topics() {
        for word in ["march", "polls", "storm", "landslide", "fire"] {
            assert!(label_of(word).is_none(), "`{word}` should not be a topic");
        }
    }
}
