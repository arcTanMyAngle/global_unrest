//! Place tokens: the "where" half of a chatter match.
//!
//! Crude, deliberate, and auditable — a word-window lookup against real
//! Natural Earth place names. There is **no** NLP location inference here and
//! there must never be: guessing where a person is from what they wrote is
//! exactly the capability this project refuses to build
//! (docs/SAFETY_AND_PRIVACY.md). A post that matches no token contributes to
//! no aggregate at all rather than being placed somewhere plausible.

use std::collections::HashMap;

use core_types::LocationPrecision;
use geo_utils::{CityIndex, CountryIndex};

/// A resolvable place and its real coordinate.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    /// ISO 3166-1 alpha-3 of the place, or of its containing country.
    pub country_iso: String,
    pub lat: f64,
    pub lon: f64,
    pub precision: LocationPrecision,
}

/// Common names for countries that Natural Earth spells differently, mapped
/// to the ISO alpha-3 whose **Natural Earth centroid** is then used. Aliases
/// add spellings, never coordinates — the coordinate still comes from the
/// bundled geometry, per the same rule the IODA work followed.
///
/// Kept short and defensible. Notable exclusion: "us" is not an alias for the
/// United States, because it is an extremely common English pronoun.
///
/// The 2026-08-13 widening pass added the abbreviated ones. Natural Earth
/// spells a dozen country names for a map label rather than for prose —
/// "S. Sudan", "Dem. Rep. Congo", "Eq. Guinea", "Bosnia and Herz.", "W.
/// Sahara" — and nobody writes a post that way, so those countries were
/// simply unreachable before. Every entry is checked by
/// `every_alias_resolves_to_a_bundled_country`: an ISO that matches nothing
/// in the bundled file does nothing at all, silently.
const COUNTRY_ALIASES: &[(&str, &str)] = &[
    ("united states", "USA"),
    ("usa", "USA"),
    ("america", "USA"),
    ("uk", "GBR"),
    ("britain", "GBR"),
    ("great britain", "GBR"),
    // England/Scotland/Wales have no separate Natural Earth admin-0 geometry;
    // at country precision they resolve to the UK centroid.
    ("england", "GBR"),
    ("scotland", "GBR"),
    ("wales", "GBR"),
    ("uae", "ARE"),
    ("drc", "COD"),
    ("dr congo", "COD"),
    ("ivory coast", "CIV"),
    // The only Natural Earth country name carrying a diacritic, so the plain
    // ASCII spelling needs saying explicitly.
    ("cote d ivoire", "CIV"),
    ("burma", "MMR"),
    ("holland", "NLD"),
    ("czech republic", "CZE"),
    // Names Natural Earth abbreviates for the map label.
    ("south sudan", "SSD"),
    ("democratic republic of congo", "COD"),
    ("democratic republic of the congo", "COD"),
    ("congo kinshasa", "COD"),
    ("republic of congo", "COG"),
    ("congo brazzaville", "COG"),
    ("central african republic", "CAF"),
    ("equatorial guinea", "GNQ"),
    ("bosnia", "BIH"),
    ("bosnia and herzegovina", "BIH"),
    ("dominican republic", "DOM"),
    ("western sahara", "ESH"),
    ("northern cyprus", "CYN"),
    ("solomon islands", "SLB"),
    ("falkland islands", "FLK"),
    // Renamed or commonly written another way.
    ("turkiye", "TUR"),
    ("türkiye", "TUR"),
    ("macedonia", "MKD"),
    ("swaziland", "SWZ"),
    ("east timor", "TLS"),
    ("dprk", "PRK"),
    ("russian federation", "RUS"),
    ("trinidad", "TTO"),
    // Gaza and the West Bank have no separate admin-0 geometry either, so
    // both resolve to the Palestine centroid at Country precision. That is
    // coarse, and coarse is the honest answer: hard rule 4 forbids inventing
    // a coordinate the source data does not contain.
    ("gaza", "PSE"),
    ("gaza strip", "PSE"),
    ("west bank", "PSE"),
];

/// Nationality adjectives, mapped the same way as [`COUNTRY_ALIASES`]: a
/// spelling only, with the coordinate still coming from the bundled country
/// geometry.
///
/// News chatter says "Sudanese army" far more often than "the army in Sudan",
/// so the demonym is frequently the only place token a post carries.
///
/// One rule decides membership: **include a demonym whose only common English
/// reading is the nationality.** That excludes, deliberately:
/// - "polish", "danish" — a verb and a pastry.
/// - "american", "indian" — too broad, and both have a strong non-country
///   reading (the hemisphere, Native American, the Indian Ocean).
/// - "chinese", "japanese", "thai", "spanish", "french", "german", "italian",
///   "greek", "portuguese", "korean" — everyday language and cuisine labels.
///   Each of those countries is already reachable by its own one-word name,
///   so the demonym adds almost nothing and risks a restaurant post.
/// - "congolese" — genuinely ambiguous between COD and COG.
///
/// `chadian`, `jordanian` and `georgian` are included on purpose: they
/// partly recover three of the four countries [`AMBIGUOUS_TOKENS`] drops,
/// without reintroducing the given-name collision that made the bare token
/// unusable.
const COUNTRY_ADJECTIVES: &[(&str, &str)] = &[
    // Europe and the Caucasus.
    ("ukrainian", "UKR"),
    ("russian", "RUS"),
    ("belarusian", "BLR"),
    ("moldovan", "MDA"),
    ("lithuanian", "LTU"),
    ("latvian", "LVA"),
    ("estonian", "EST"),
    ("finnish", "FIN"),
    ("swedish", "SWE"),
    ("norwegian", "NOR"),
    ("icelandic", "ISL"),
    ("irish", "IRL"),
    ("austrian", "AUT"),
    ("swiss", "CHE"),
    ("belgian", "BEL"),
    ("hungarian", "HUN"),
    ("romanian", "ROU"),
    ("bulgarian", "BGR"),
    ("serbian", "SRB"),
    ("bosnian", "BIH"),
    ("croatian", "HRV"),
    ("albanian", "ALB"),
    ("kosovar", "KOS"),
    ("macedonian", "MKD"),
    ("slovak", "SVK"),
    ("slovenian", "SVN"),
    ("cypriot", "CYP"),
    ("armenian", "ARM"),
    ("azerbaijani", "AZE"),
    ("georgian", "GEO"),
    // Middle East and North Africa.
    ("turkish", "TUR"),
    ("israeli", "ISR"),
    ("palestinian", "PSE"),
    ("syrian", "SYR"),
    ("lebanese", "LBN"),
    ("iranian", "IRN"),
    ("iraqi", "IRQ"),
    ("yemeni", "YEM"),
    ("saudi", "SAU"),
    ("emirati", "ARE"),
    ("qatari", "QAT"),
    ("kuwaiti", "KWT"),
    ("omani", "OMN"),
    ("jordanian", "JOR"),
    ("egyptian", "EGY"),
    ("libyan", "LBY"),
    ("tunisian", "TUN"),
    ("algerian", "DZA"),
    ("moroccan", "MAR"),
    // Sub-Saharan Africa.
    ("sudanese", "SDN"),
    ("somali", "SOM"),
    ("ethiopian", "ETH"),
    ("eritrean", "ERI"),
    ("kenyan", "KEN"),
    ("ugandan", "UGA"),
    ("rwandan", "RWA"),
    ("tanzanian", "TZA"),
    ("zambian", "ZMB"),
    ("zimbabwean", "ZWE"),
    ("mozambican", "MOZ"),
    ("angolan", "AGO"),
    ("malagasy", "MDG"),
    ("nigerian", "NGA"),
    ("nigerien", "NER"),
    ("ghanaian", "GHA"),
    ("malian", "MLI"),
    ("burkinabe", "BFA"),
    ("senegalese", "SEN"),
    ("ivorian", "CIV"),
    ("cameroonian", "CMR"),
    ("chadian", "TCD"),
    // Asia and the Pacific.
    ("afghan", "AFG"),
    ("pakistani", "PAK"),
    ("bangladeshi", "BGD"),
    ("nepali", "NPL"),
    ("sri lankan", "LKA"),
    ("burmese", "MMR"),
    ("vietnamese", "VNM"),
    ("cambodian", "KHM"),
    ("filipino", "PHL"),
    ("indonesian", "IDN"),
    ("malaysian", "MYS"),
    ("taiwanese", "TWN"),
    ("mongolian", "MNG"),
    ("kazakh", "KAZ"),
    ("uzbek", "UZB"),
    ("tajik", "TJK"),
    ("kyrgyz", "KGZ"),
    ("turkmen", "TKM"),
    ("australian", "AUS"),
    // The Americas.
    ("mexican", "MEX"),
    ("guatemalan", "GTM"),
    ("honduran", "HND"),
    ("salvadoran", "SLV"),
    ("nicaraguan", "NIC"),
    ("costa rican", "CRI"),
    ("panamanian", "PAN"),
    ("haitian", "HTI"),
    ("cuban", "CUB"),
    ("jamaican", "JAM"),
    ("venezuelan", "VEN"),
    ("colombian", "COL"),
    ("ecuadorian", "ECU"),
    ("peruvian", "PER"),
    ("bolivian", "BOL"),
    ("chilean", "CHL"),
    ("argentine", "ARG"),
    ("argentinian", "ARG"),
    ("brazilian", "BRA"),
    ("uruguayan", "URY"),
    ("paraguayan", "PRY"),
];

/// Former or exonymic city names, mapped to the **Natural Earth city name**
/// they refer to. Same rule again: a spelling, never a coordinate.
///
/// Deliberately tiny. The bundled 1:110m gazetteer is 243 places, essentially
/// capitals and a handful of megacities, so most of what chatter names —
/// Aleppo, Kharkiv, Rafah, Culiacán — is simply not in it and cannot be added
/// here without hand-typing a coordinate, which this crate does not do. Those
/// posts fall back to their country token or go uncounted.
const CITY_ALIASES: &[(&str, &str)] = &[
    ("bombay", "Mumbai"),
    ("calcutta", "Kolkata"),
    ("bangalore", "Bengaluru"),
    ("peking", "Beijing"),
    ("rangoon", "Yangon"),
    ("kiev", "Kyiv"),
    ("astana", "Nur-Sultan"),
];

/// Place tokens dropped after the table is built, because the token is a
/// common English word or given name and the place reading is unreliable
/// even with a topic keyword alongside it.
///
/// Each entry is a real collision, not a hypothetical:
/// - `male` — Malé, capital of the Maldives, ASCII-folded by Natural Earth
///   itself into the English word "male".
/// - `chad`, `jordan` — countries that are also very common given names.
/// - `georgia` — the country, a US state, and a given name.
///
/// Losing these places is the honest trade: a false "unrest in Chad" spike
/// built from posts about someone named Chad is worse than no signal.
const AMBIGUOUS_TOKENS: &[&str] = &["male", "chad", "jordan", "georgia"];

/// Word-window lookup from place tokens to [`Place`].
pub struct PlaceMatcher {
    places: Vec<Place>,
    by_token: HashMap<Vec<String>, usize>,
    max_words: usize,
}

impl PlaceMatcher {
    /// Build the token table from the bundled gazetteers.
    ///
    /// Cities are inserted first and countries second, so a token claimed by
    /// both resolves to the **country** — a bare "Panama" or "Monaco" in a
    /// sentence far more often means the country, and for city-states the two
    /// coordinates are nearly identical anyway.
    pub fn from_indexes(countries: &CountryIndex, cities: &CityIndex) -> Self {
        let mut places: Vec<Place> = Vec::new();
        let mut by_token: HashMap<Vec<String>, usize> = HashMap::new();
        let mut max_words = 1usize;

        // `is_country` decides collisions without re-reading `places`, so the
        // closure never needs to borrow the vector it is filling.
        let mut insert = |token: &str, idx: usize, is_country: bool| {
            let words = crate::tokenize(token);
            if words.is_empty() {
                return;
            }
            max_words = max_words.max(words.len());
            match by_token.entry(words) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if is_country {
                        e.insert(idx);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(idx);
                }
            }
        };

        for city in cities.iter() {
            let idx = places.len();
            places.push(Place {
                name: city.name.clone(),
                country_iso: city.iso_a3.clone(),
                lat: city.lat,
                lon: city.lon,
                precision: LocationPrecision::City,
            });
            insert(&city.name, idx, false);
            for alt in &city.alt_names {
                insert(alt, idx, false);
            }
            for (alias, city_name) in CITY_ALIASES {
                if *city_name == city.name {
                    insert(alias, idx, false);
                }
            }
        }

        for (info, (lon, lat)) in countries.iter_with_centroid() {
            let idx = places.len();
            places.push(Place {
                name: info.name.clone(),
                country_iso: info.iso_a3.clone(),
                lat,
                lon,
                precision: LocationPrecision::Country,
            });
            insert(&info.name, idx, true);
            for (alias, iso_a3) in COUNTRY_ALIASES.iter().chain(COUNTRY_ADJECTIVES) {
                if *iso_a3 == info.iso_a3 {
                    insert(alias, idx, true);
                }
            }
        }

        for token in AMBIGUOUS_TOKENS {
            by_token.remove(&crate::tokenize(token));
        }

        Self {
            places,
            by_token,
            max_words,
        }
    }

    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }

    /// First place mentioned in `words`, scanning left to right and trying
    /// the longest token window first. Returns its index.
    ///
    /// One place per post by design: a post naming three countries counts
    /// once, toward the first. Counting each would inflate every aggregate
    /// that a widely-shared multi-country post touches.
    pub fn find(&self, words: &[String]) -> Option<usize> {
        crate::find_window(words, self.max_words, |w| self.by_token.get(w).copied())
    }

    pub fn place(&self, idx: usize) -> &Place {
        &self.places[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::OnceLock;

    fn matcher() -> &'static PlaceMatcher {
        static M: OnceLock<PlaceMatcher> = OnceLock::new();
        M.get_or_init(|| {
            let countries = CountryIndex::from_geojson_str(crate::NE_COUNTRIES).unwrap();
            let cities = CityIndex::from_geojson_str(crate::NE_PLACES).unwrap();
            PlaceMatcher::from_indexes(&countries, &cities)
        })
    }

    fn place_of(text: &str) -> Option<&'static Place> {
        let m = matcher();
        m.find(&crate::tokenize(text)).map(|i| m.place(i))
    }

    /// An alias whose ISO matches no bundled country is a silent no-op: the
    /// `if *iso_a3 == info.iso_a3` arm never fires and the spelling is simply
    /// never inserted. Natural Earth 1:110m omits many small states outright
    /// and publishes `-99` for several disputed ones (France, Kosovo, N.
    /// Cyprus, Somaliland), where `CountryIndex` falls back to `ADM0_A3` —
    /// so the code an alias must name is not always the one you would guess.
    #[test]
    fn every_alias_resolves_to_a_bundled_country() {
        for (alias, iso_a3) in COUNTRY_ALIASES.iter().chain(COUNTRY_ADJECTIVES) {
            let place = place_of(alias).unwrap_or_else(|| {
                panic!("alias `{alias}` -> {iso_a3} matched no bundled country")
            });
            assert_eq!(place.country_iso, *iso_a3, "alias `{alias}` landed wrong");
            assert_eq!(place.precision, LocationPrecision::Country);
            // The coordinate came from the geometry, so it is a real one.
            assert!(place.lat.abs() <= 90.0 && place.lon.abs() <= 180.0);
        }
    }

    /// Same failure mode on the city side: the alias is matched against
    /// `city.name` verbatim, so a renamed or misspelled target silently
    /// inserts nothing.
    #[test]
    fn every_city_alias_resolves_to_a_bundled_city() {
        for (alias, city_name) in CITY_ALIASES {
            let place = place_of(alias).unwrap_or_else(|| {
                panic!("city alias `{alias}` -> {city_name} matched no bundled city")
            });
            assert_eq!(place.name, *city_name, "city alias `{alias}` landed wrong");
            assert_eq!(place.precision, LocationPrecision::City);
        }
    }

    /// The ambiguous-token removal runs after every table is inserted, so a
    /// new alias or demonym cannot quietly put a dropped token back.
    #[test]
    fn no_table_entry_reintroduces_a_dropped_token() {
        let dropped: HashSet<&str> = AMBIGUOUS_TOKENS.iter().copied().collect();
        for (token, _) in COUNTRY_ALIASES
            .iter()
            .chain(COUNTRY_ADJECTIVES)
            .chain(CITY_ALIASES)
        {
            assert!(!dropped.contains(token), "`{token}` is an ambiguous token");
        }
        for token in AMBIGUOUS_TOKENS {
            assert!(place_of(token).is_none(), "`{token}` should be dropped");
        }
    }

    /// Demonyms are the point of the second table: news chatter names the
    /// nationality far more often than the country.
    #[test]
    fn demonyms_resolve_to_their_country() {
        assert_eq!(place_of("sudanese army").unwrap().country_iso, "SDN");
        assert_eq!(place_of("ukrainian forces").unwrap().country_iso, "UKR");
        // Recovered from behind an ambiguous bare token.
        assert_eq!(place_of("chadian troops").unwrap().country_iso, "TCD");
        // Excluded on purpose — a verb, a pastry, and two too-broad ones.
        for word in ["polish", "danish", "american", "indian", "congolese"] {
            assert!(place_of(word).is_none(), "`{word}` should not be a place");
        }
    }

    /// Natural Earth spells these for a map label, not for prose, so the
    /// alias is the only way a post can reach them.
    #[test]
    fn map_label_abbreviations_are_reachable_in_prose() {
        assert_eq!(place_of("south sudan").unwrap().country_iso, "SSD");
        assert_eq!(place_of("bosnia").unwrap().country_iso, "BIH");
        assert_eq!(place_of("dominican republic").unwrap().country_iso, "DOM");
        assert_eq!(place_of("equatorial guinea").unwrap().country_iso, "GNQ");
        // Both Congos, kept apart.
        assert_eq!(place_of("congo brazzaville").unwrap().country_iso, "COG");
        assert_eq!(
            place_of("democratic republic of the congo")
                .unwrap()
                .country_iso,
            "COD"
        );
    }
}
