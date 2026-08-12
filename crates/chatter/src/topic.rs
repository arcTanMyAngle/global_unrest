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
/// `outage` topic that pairs with the IODA layer.
///
/// Keyword choices worth knowing about:
/// - "march" is excluded from `protest` — it is a month and a common verb.
/// - "polls" is excluded from `election` — it is as often opinion polling.
/// - "strike" is kept despite airstrike/lightning/bowling senses, because a
///   place match is always required alongside it.
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
        ],
    },
    Topic {
        label: "flood",
        keywords: &["flood", "floods", "flooding", "floodwaters", "inundated"],
    },
    Topic {
        label: "earthquake",
        keywords: &["earthquake", "quake", "aftershock", "aftershocks", "tremor"],
    },
    Topic {
        label: "wildfire",
        keywords: &["wildfire", "wildfires", "bushfire", "bushfires"],
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
        ],
    },
    Topic {
        label: "strike",
        keywords: &["strike", "strikes", "walkout", "picket", "general strike"],
    },
    Topic {
        label: "outage",
        keywords: &[
            "outage",
            "outages",
            "blackout",
            "blackouts",
            "internet shutdown",
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
