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
            for (alias, iso_a3) in COUNTRY_ALIASES {
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
