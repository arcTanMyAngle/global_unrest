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

/// Topic keywords in scripts that do not delimit words, matched as substrings
/// inside a script run rather than as whitespace tokens (see [`crate::script`]
/// for why the word path cannot see them at all).
///
/// A separate table rather than a field on [`Topic`], because the two are
/// matched by different machinery and the entries are checked by different
/// tests; `every_script_keyword_names_a_real_topic` fails if a label here does
/// not exist in [`TOPICS`].
///
/// Scope rule, and it is narrow on purpose: **a term whose only common reading
/// is the topic**, in the language actually written in that script. This is
/// not a translation of [`TOPICS`] — most entries there have no unambiguous
/// single-word equivalent, and a bad one costs more than a missing one. Both
/// simplified and traditional Han spellings are listed where they differ,
/// because neither converts to the other by substring.
///
/// Deliberately absent: any term for "fire" that also covers a house fire
/// (only the forest-fire compounds are here), and any bare word that is a
/// common verb in its own language.
const SCRIPT_TOPIC_KEYWORDS: &[(&str, &str)] = &[
    // Burmese (Myanmar). The three Myanmar channels in
    // `source_telegram::ALLOWED_CHANNELS` post almost entirely in this script.
    ("ဆန္ဒပြ", "protest"),
    ("သပိတ်", "strike"),
    ("တိုက်ပွဲ", "conflict"),
    ("ငလျင်", "earthquake"),
    ("ရေကြီး", "flood"),
    ("ရေလွှမ်း", "flood"),
    ("မုန်တိုင်း", "storm"),
    ("တောမီး", "wildfire"),
    ("မီးပြတ်", "outage"),
    ("ရွေးကောက်ပွဲ", "election"),
    ("စစ်ဘေးရှောင်", "displacement"),
    ("ဗုံးပေါက်", "explosion"),
    ("ဗုံးခွဲ", "explosion"),
    // Thai.
    ("ประท้วง", "protest"),
    ("ชุมนุม", "protest"),
    ("จลาจล", "unrest"),
    ("ปะทะ", "unrest"),
    ("น้ำท่วม", "flood"),
    ("แผ่นดินไหว", "earthquake"),
    ("พายุ", "storm"),
    ("ไฟป่า", "wildfire"),
    ("ภูเขาไฟ", "volcano"),
    ("ดินถล่ม", "landslide"),
    ("ภัยแล้ง", "drought"),
    ("อพยพ", "displacement"),
    ("ระเบิด", "explosion"),
    ("เลือกตั้ง", "election"),
    ("ไฟดับ", "outage"),
    // Khmer.
    ("បាតុកម្ម", "protest"),
    ("ទឹកជំនន់", "flood"),
    ("រញ្ជួយដី", "earthquake"),
    ("ព្យុះ", "storm"),
    ("ផ្ទុះ", "explosion"),
    ("បោះឆ្នោត", "election"),
    // Lao.
    ("ປະທ້ວງ", "protest"),
    // Only the verb "flooded": the noun spelling varies between U+0EB3 and a
    // decomposed U+0ECD U+0EB2, and matching the verb sidesteps that entirely.
    ("ຖ້ວມ", "flood"),
    ("ແຜ່ນດິນໄຫວ", "earthquake"),
    ("ພາຍຸ", "storm"),
    ("ເລືອກຕັ້ງ", "election"),
    // Chinese and Japanese.
    ("抗议", "protest"),
    ("抗議", "protest"),
    ("示威", "protest"),
    ("デモ", "protest"),
    ("骚乱", "unrest"),
    ("騷亂", "unrest"),
    ("暴动", "unrest"),
    ("暴動", "unrest"),
    ("冲突", "conflict"),
    ("衝突", "conflict"),
    ("空袭", "conflict"),
    ("空襲", "conflict"),
    ("炮击", "conflict"),
    ("砲擊", "conflict"),
    ("地震", "earthquake"),
    ("余震", "earthquake"),
    ("餘震", "earthquake"),
    ("洪水", "flood"),
    ("洪灾", "flood"),
    ("洪災", "flood"),
    ("浸水", "flood"),
    ("泛滥", "flood"),
    ("氾濫", "flood"),
    // 台風 is the Japanese spelling, 台风 simplified, 颱風 traditional.
    ("台风", "storm"),
    ("台風", "storm"),
    ("颱風", "storm"),
    ("飓风", "storm"),
    ("颶風", "storm"),
    ("龙卷风", "storm"),
    ("龍捲風", "storm"),
    ("山火", "wildfire"),
    ("野火", "wildfire"),
    ("火山", "volcano"),
    ("喷发", "volcano"),
    ("噴發", "volcano"),
    ("噴火", "volcano"),
    ("山体滑坡", "landslide"),
    ("土石流", "landslide"),
    ("干旱", "drought"),
    ("乾旱", "drought"),
    ("难民", "displacement"),
    ("難民", "displacement"),
    ("疏散", "displacement"),
    ("避难", "displacement"),
    ("避難", "displacement"),
    ("爆炸", "explosion"),
    ("爆発", "explosion"),
    ("疫情", "outbreak"),
    ("选举", "election"),
    ("選舉", "election"),
    ("選挙", "election"),
    ("罢工", "strike"),
    ("罷工", "strike"),
    ("ストライキ", "strike"),
    ("停电", "outage"),
    ("停電", "outage"),
    ("断网", "outage"),
    ("斷網", "outage"),
];

/// Word-window lookup over [`TOPICS`], plus substring lookup over
/// [`SCRIPT_TOPIC_KEYWORDS`] for scripts the word path cannot tokenize.
pub struct TopicMatcher {
    /// Keyword words -> index into `labels`.
    by_keyword: std::collections::HashMap<Vec<String>, usize>,
    scripts: crate::script::ScriptMatcher,
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
        let mut scripts = crate::script::ScriptMatcher::new();
        for (keyword, label) in SCRIPT_TOPIC_KEYWORDS {
            if let Some(idx) = labels.iter().position(|l| l == label) {
                scripts.insert(keyword, idx);
            }
        }

        Self {
            by_keyword,
            scripts,
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

    /// First topic mentioned inside an unsegmented-script run. Tried only
    /// after [`TopicMatcher::find`] fails, so a post that names a topic in
    /// Latin keeps the same answer it had before this path existed.
    pub fn find_in_runs(&self, runs: &[crate::script::Run<'_>]) -> Option<usize> {
        self.scripts.find(runs)
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

    fn script_label_of(text: &str) -> Option<&'static str> {
        let m = TopicMatcher::new();
        m.find_in_runs(&crate::script::runs(text))
            .map(|i| m.label(i))
    }

    /// A label typo here would silently drop the keyword: `TopicMatcher::new`
    /// can only insert a script keyword whose label it can find in `TOPICS`.
    #[test]
    fn every_script_keyword_names_a_real_topic() {
        for (keyword, label) in SCRIPT_TOPIC_KEYWORDS {
            assert!(
                TOPICS.iter().any(|t| t.label == *label),
                "script keyword `{keyword}` names unknown topic `{label}`"
            );
            assert_eq!(
                script_label_of(keyword),
                Some(*label),
                "script keyword `{keyword}` does not resolve to `{label}`"
            );
        }
    }

    /// A keyword mixing scripts (or containing a stray Latin letter) is
    /// dropped by `ScriptMatcher::insert` without a word of complaint, so the
    /// table has to be checked rather than trusted.
    #[test]
    fn script_keywords_are_single_class() {
        for (keyword, _) in SCRIPT_TOPIC_KEYWORDS {
            assert!(
                crate::script::keyword_class(keyword).is_some(),
                "`{keyword}` is empty or mixes scripts and would be ignored"
            );
        }
    }

    /// Real sentences, one per script family: the point of the whole exercise
    /// is that none of these contain a space where the keyword ends.
    #[test]
    fn topics_are_found_in_undelimited_sentences() {
        // Burmese: "an earthquake struck in Myanmar".
        assert_eq!(script_label_of("မြန်မာနိုင်ငံမှာ ငလျင်လှုပ်ခဲ့သည်"), Some("earthquake"));
        // Burmese: "the people came out to protest".
        assert_eq!(script_label_of("ပြည်သူများဆန္ဒပြကြသည်"), Some("protest"));
        // Thai: "flooding in many provinces".
        assert_eq!(script_label_of("น้ำท่วมหลายจังหวัด"), Some("flood"));
        // Thai: "a protest at the government house".
        assert_eq!(script_label_of("การประท้วงที่ทำเนียบรัฐบาล"), Some("protest"));
        // Khmer: "a demonstration by the workers".
        assert_eq!(script_label_of("បាតុកម្មរបស់កម្មករ"), Some("protest"));
        // Lao: "an earthquake was felt".
        assert_eq!(script_label_of("ຮູ້ສຶກແຜ່ນດິນໄຫວ"), Some("earthquake"));
        // Chinese: "a magnitude 7 earthquake struck" — no spaces at all.
        assert_eq!(script_label_of("发生7级地震"), Some("earthquake"));
        // Japanese: "residents are evacuating because of the typhoon".
        assert_eq!(
            script_label_of("台風のため住民が避難しています"),
            Some("storm")
        );
        // Nothing at all in an unrelated sentence.
        assert_eq!(script_label_of("今日はいい天気ですね"), None);
    }

    /// Longest-at-a-position applies to the script path too: 山火事 (Japanese
    /// wildfire) contains 山火 (Chinese wildfire), and both must land on the
    /// same topic rather than one shadowing the other into silence.
    #[test]
    fn overlapping_script_keywords_agree() {
        assert_eq!(script_label_of("山火事"), Some("wildfire"));
        assert_eq!(script_label_of("山火"), Some("wildfire"));
        // 火山 (volcano) is the same two characters reversed and stays apart.
        assert_eq!(script_label_of("火山"), Some("volcano"));
    }

    /// The word path still owns any post it can read: a Latin topic word in a
    /// mixed post is not overridden by the script table.
    #[test]
    fn the_word_path_is_unchanged_by_the_script_table() {
        assert_eq!(label_of("protest in Kyiv"), Some("protest"));
        // Script keywords are invisible to the word matcher, by construction:
        // they are never split into whitespace tokens.
        assert_eq!(label_of("地震"), None);
    }
}
