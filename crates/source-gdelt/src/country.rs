//! GDELT `sourcecountry` → (ISO 3166-1 alpha-3, representative centroid).
//!
//! The DOC 2.0 `artlist` feed geocodes attention only to the **source
//! country** (the publisher's country), as a full English country name. We
//! resolve that to an ISO-A3 code and an approximate country centroid so the
//! record can be assigned an H3 cell. These records are always emitted at
//! `LocationPrecision::Country`, so the centroid only shades the region — it
//! never renders as a point (docs/DATA_MODEL.md precision contract), which is
//! why an approximate representative point is sufficient and honest.
//!
//! Unknown country names are **not** guessed: normalization fails per record
//! and the raw payload lands in `ingest_log`. The table is intentionally
//! extensible — add rows as new source countries appear in the wild.

/// A resolved country: ISO-A3 plus a representative interior point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Country {
    pub iso_a3: &'static str,
    pub lat: f64,
    pub lon: f64,
}

/// Resolve a GDELT `sourcecountry` value. Matching is case-insensitive on the
/// trimmed name, and a small alias set covers the common alternate spellings
/// GDELT and callers use ("USA", "UK", "South Korea", …).
pub fn resolve(name: &str) -> Option<Country> {
    let key = name.trim().to_ascii_lowercase();
    let canon = ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, canon)| *canon)
        .unwrap_or(key.as_str());
    TABLE.iter().find(|(n, _)| *n == canon).map(|(_, c)| *c)
}

/// Alternate spelling → canonical lowercase name present in [`TABLE`].
const ALIASES: &[(&str, &str)] = &[
    ("usa", "united states"),
    ("u.s.", "united states"),
    ("u.s.a.", "united states"),
    ("america", "united states"),
    ("united states of america", "united states"),
    ("uk", "united kingdom"),
    ("u.k.", "united kingdom"),
    ("britain", "united kingdom"),
    ("great britain", "united kingdom"),
    ("russian federation", "russia"),
    ("korea, south", "south korea"),
    ("republic of korea", "south korea"),
    ("czech republic", "czechia"),
    ("uae", "united arab emirates"),
];

/// Canonical lowercase country name → resolved country. Centroids are
/// approximate representative interior points (degrees, WGS84). Coverage is a
/// pragmatic global set; extend as needed.
const TABLE: &[(&str, Country)] = &[
    ("united states", c("USA", 39.5, -98.35)),
    ("united kingdom", c("GBR", 54.0, -2.0)),
    ("france", c("FRA", 46.6, 2.2)),
    ("germany", c("DEU", 51.2, 10.4)),
    ("spain", c("ESP", 40.0, -3.7)),
    ("portugal", c("PRT", 39.4, -8.2)),
    ("italy", c("ITA", 42.8, 12.6)),
    ("ireland", c("IRL", 53.2, -8.0)),
    ("netherlands", c("NLD", 52.1, 5.3)),
    ("belgium", c("BEL", 50.6, 4.6)),
    ("switzerland", c("CHE", 46.8, 8.2)),
    ("austria", c("AUT", 47.6, 14.1)),
    ("poland", c("POL", 52.0, 19.0)),
    ("sweden", c("SWE", 62.0, 15.0)),
    ("norway", c("NOR", 64.5, 12.0)),
    ("finland", c("FIN", 64.0, 26.0)),
    ("denmark", c("DNK", 56.0, 9.5)),
    ("greece", c("GRC", 39.1, 21.8)),
    ("ukraine", c("UKR", 48.4, 31.2)),
    ("russia", c("RUS", 61.5, 105.3)),
    ("turkey", c("TUR", 39.0, 35.2)),
    ("egypt", c("EGY", 26.8, 30.8)),
    ("israel", c("ISR", 31.5, 34.9)),
    ("saudi arabia", c("SAU", 24.0, 45.0)),
    ("united arab emirates", c("ARE", 23.9, 54.3)),
    ("iran", c("IRN", 32.4, 53.7)),
    ("iraq", c("IRQ", 33.2, 43.7)),
    ("nigeria", c("NGA", 9.1, 8.7)),
    ("kenya", c("KEN", -0.02, 37.9)),
    ("ethiopia", c("ETH", 9.1, 40.5)),
    ("south africa", c("ZAF", -29.0, 24.0)),
    ("china", c("CHN", 35.9, 104.2)),
    ("japan", c("JPN", 36.2, 138.3)),
    ("south korea", c("KOR", 36.5, 127.8)),
    ("india", c("IND", 22.4, 78.7)),
    ("pakistan", c("PAK", 30.4, 69.3)),
    ("bangladesh", c("BGD", 23.7, 90.4)),
    ("indonesia", c("IDN", -2.5, 118.0)),
    ("philippines", c("PHL", 12.9, 121.8)),
    ("thailand", c("THA", 15.9, 100.9)),
    ("vietnam", c("VNM", 16.0, 107.8)),
    ("malaysia", c("MYS", 4.2, 108.0)),
    ("australia", c("AUS", -25.7, 134.5)),
    ("new zealand", c("NZL", -41.5, 172.8)),
    ("fiji", c("FJI", -17.7, 178.0)),
    ("canada", c("CAN", 56.1, -106.3)),
    ("mexico", c("MEX", 23.6, -102.5)),
    ("brazil", c("BRA", -10.8, -52.9)),
    ("argentina", c("ARG", -38.4, -63.6)),
    ("chile", c("CHL", -35.7, -71.5)),
    ("colombia", c("COL", 4.1, -73.0)),
    ("czechia", c("CZE", 49.8, 15.5)),
];

/// `const` constructor so the table above stays terse and readable.
const fn c(iso_a3: &'static str, lat: f64, lon: f64) -> Country {
    Country { iso_a3, lat, lon }
}

/// GDELT **Events** rows carry a FIPS 10-4 country code (`ActionGeo_CountryCode`)
/// rather than a full name; resolve it to ISO-A3. FIPS codes differ from ISO
/// in ways that bite (FIPS `AU`=Austria, `AS`=Australia; `CH`=China, `SZ`=
/// Switzerland; `CI`=Chile), so this is an explicit table. Unknown codes yield
/// `None` — the caller keeps the row's authoritative lat/lon and leaves
/// `country_iso` empty rather than guessing.
pub fn iso3_from_fips(fips: &str) -> Option<&'static str> {
    let key = fips.trim().to_ascii_uppercase();
    FIPS.iter().find(|(f, _)| *f == key).map(|(_, iso)| *iso)
}

/// FIPS 10-4 → ISO-A3 for the same country set as [`TABLE`].
const FIPS: &[(&str, &str)] = &[
    ("US", "USA"),
    ("UK", "GBR"),
    ("FR", "FRA"),
    ("GM", "DEU"),
    ("SP", "ESP"),
    ("PO", "PRT"),
    ("IT", "ITA"),
    ("EI", "IRL"),
    ("NL", "NLD"),
    ("BE", "BEL"),
    ("SZ", "CHE"),
    ("AU", "AUT"),
    ("PL", "POL"),
    ("SW", "SWE"),
    ("NO", "NOR"),
    ("FI", "FIN"),
    ("DA", "DNK"),
    ("GR", "GRC"),
    ("UP", "UKR"),
    ("RS", "RUS"),
    ("TU", "TUR"),
    ("EG", "EGY"),
    ("IS", "ISR"),
    ("SA", "SAU"),
    ("AE", "ARE"),
    ("IR", "IRN"),
    ("IZ", "IRQ"),
    ("NI", "NGA"),
    ("KE", "KEN"),
    ("ET", "ETH"),
    ("SF", "ZAF"),
    ("CH", "CHN"),
    ("JA", "JPN"),
    ("KS", "KOR"),
    ("IN", "IND"),
    ("PK", "PAK"),
    ("BG", "BGD"),
    ("ID", "IDN"),
    ("RP", "PHL"),
    ("TH", "THA"),
    ("VM", "VNM"),
    ("MY", "MYS"),
    ("AS", "AUS"),
    ("NZ", "NZL"),
    ("FJ", "FJI"),
    ("CA", "CAN"),
    ("MX", "MEX"),
    ("BR", "BRA"),
    ("AR", "ARG"),
    ("CI", "CHL"),
    ("CO", "COL"),
    ("EZ", "CZE"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_names_case_insensitively() {
        let fr = resolve("France").unwrap();
        assert_eq!(fr.iso_a3, "FRA");
        assert_eq!(resolve("  france  ").unwrap().iso_a3, "FRA");
        assert_eq!(resolve("FRANCE").unwrap().iso_a3, "FRA");
    }

    #[test]
    fn resolves_aliases() {
        assert_eq!(resolve("USA").unwrap().iso_a3, "USA");
        assert_eq!(resolve("United States of America").unwrap().iso_a3, "USA");
        assert_eq!(resolve("UK").unwrap().iso_a3, "GBR");
        assert_eq!(resolve("Russian Federation").unwrap().iso_a3, "RUS");
    }

    #[test]
    fn unknown_country_is_none() {
        assert!(resolve("Atlantis").is_none());
        assert!(resolve("").is_none());
    }

    #[test]
    fn every_centroid_is_in_range() {
        for (name, c) in TABLE {
            assert!(
                (-90.0..=90.0).contains(&c.lat) && (-180.0..=180.0).contains(&c.lon),
                "{name} has out-of-range centroid ({}, {})",
                c.lat,
                c.lon
            );
        }
    }

    #[test]
    fn aliases_point_at_real_rows() {
        for (alias, canon) in ALIASES {
            assert!(
                TABLE.iter().any(|(n, _)| n == canon),
                "alias `{alias}` targets missing canonical `{canon}`"
            );
        }
    }

    #[test]
    fn fips_resolves_to_iso3_including_the_tricky_ones() {
        assert_eq!(iso3_from_fips("FR"), Some("FRA"));
        assert_eq!(iso3_from_fips("us"), Some("USA"));
        // FIPS traps: AU/AS and CH/SZ and CI are famously not ISO.
        assert_eq!(iso3_from_fips("AU"), Some("AUT")); // Austria, not Australia
        assert_eq!(iso3_from_fips("AS"), Some("AUS")); // Australia
        assert_eq!(iso3_from_fips("CH"), Some("CHN")); // China, not Switzerland
        assert_eq!(iso3_from_fips("SZ"), Some("CHE")); // Switzerland
        assert_eq!(iso3_from_fips("CI"), Some("CHL")); // Chile
        assert_eq!(iso3_from_fips("ZZ"), None);
    }

    #[test]
    fn every_fips_iso3_is_a_known_iso3() {
        let known: std::collections::HashSet<&str> = TABLE.iter().map(|(_, c)| c.iso_a3).collect();
        for (fips, iso) in FIPS {
            assert!(known.contains(iso), "FIPS `{fips}` -> unknown ISO `{iso}`");
        }
    }
}
