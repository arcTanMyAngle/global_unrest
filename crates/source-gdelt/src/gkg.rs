//! GDELT GKG 2.1 15-minute CSV-zip dumps — the story-location attention path.
//!
//! DOC resolves only the *publisher's* country; GKG 2.1 is the dataset that
//! names the places each article actually mentions (`V2EnhancedLocations`),
//! with per-mention precision. See `docs/GDELT_GEO_GKG.md` and the A2 finding
//! for why this replaces the GEO 2.0 aggregate (which no longer exists).
//!
//! Each row is one article; each distinct (article, place) mention becomes one
//! [`EventKind::NewsAttention`] record with
//! [`LocationRole::MentionedPlace`], so attention shades the places a story is
//! *about* rather than the outlet's country. Country-type mentions carry
//! centroid coordinates and are emitted at [`LocationPrecision::Country`] —
//! they shade a region, never render as a point (docs/SIGNAL_MODEL.md).
//!
//! Themes are a property of the article, not of any one place, so every
//! mention of an article carries the same document-level theme set — no
//! theme-to-location edge is invented (docs/GDELT_GEO_GKG.md).
//!
//! Parsing and normalization are pure and offline-testable; only
//! [`crate::GdeltSource::fetch_gkg`] touches the network.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use core_types::{
    EventKind, GeoTemporalEvent, H3_RESOLUTION, LocationPrecision, LocationRole, NormalizeError,
    SignalFamily, SourceId, event_id,
};

use crate::{country, events};

/// GKG 2.1 rows are tab-separated with 25 columns; these are the 0-based
/// indices this module reads (from the GKG 2.1 codebook):
const COL_GKGRECORDID: usize = 0;
const COL_DATE: usize = 1;
const COL_SOURCECOMMONNAME: usize = 3;
const COL_DOCUMENTIDENTIFIER: usize = 4;
const COL_THEMES: usize = 7;
const COL_V2ENHANCEDLOCATIONS: usize = 10;
const COL_V2EXTRASXML: usize = 23;
/// Minimum column count for a well-formed row (indices 0..=23).
const MIN_COLUMNS: usize = 24;

/// The GKG export URL among the `lastupdate.txt` dump refs (`*.gkg.csv.zip`).
/// `lastupdate.txt` lists all three 15-minute dumps (Events, Mentions, GKG);
/// this finds the GKG one.
pub fn gkg_url(refs: &[events::DumpRef]) -> Option<&str> {
    refs.iter()
        .map(|r| r.url.as_str())
        .find(|u| u.ends_with(".gkg.csv.zip"))
}

/// Normalize one GKG row into zero or more attention records.
///
/// Returns one record per distinct (article, place) mention, `Ok(vec![])` for
/// a row with no usable locations (skipped, not a failure — ~21% of rows have
/// no location field), or `Err` for a genuinely malformed row.
pub fn normalize(row: &str) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
    let cols: Vec<&str> = row.split('\t').collect();
    if cols.len() < MIN_COLUMNS {
        return Err(NormalizeError::InvalidValue {
            field: "columns",
            detail: format!("{} columns, expected at least {MIN_COLUMNS}", cols.len()),
        });
    }
    let get = |i: usize| cols[i].trim();

    let record_id = get(COL_GKGRECORDID);
    if record_id.is_empty() {
        return Err(NormalizeError::MissingField("gkgrecordid"));
    }
    let ts_utc = parse_date(get(COL_DATE))?;
    let domain = get(COL_SOURCECOMMONNAME);
    let doc_url = get(COL_DOCUMENTIDENTIFIER);
    let headline = extract_page_title(get(COL_V2EXTRASXML));
    let themes = document_themes(get(COL_THEMES));

    let mentions = parse_mentions(get(COL_V2ENHANCEDLOCATIONS));
    if mentions.is_empty() {
        return Ok(Vec::new());
    }

    let outlet_domains = if domain.is_empty() {
        Vec::new()
    } else {
        vec![domain.to_owned()]
    };
    let urls = if doc_url.is_empty() {
        Vec::new()
    } else {
        vec![doc_url.to_owned()]
    };

    let mut out = Vec::with_capacity(mentions.len());
    for (i, mention) in mentions.into_iter().enumerate() {
        let h3_cell =
            geo_utils::cell_for_latlon(mention.lat, mention.lon, H3_RESOLUTION).map_err(|e| {
                NormalizeError::InvalidValue {
                    field: "v2enhancedlocations",
                    detail: format!("h3 assignment failed: {e}"),
                }
            })?;
        // `GKGRECORDID` alone collides across one article's mentions, so the
        // per-mention index makes the event id (and its derived `id`) unique.
        let source_event_id = format!("{record_id}#{i}");
        let ev = GeoTemporalEvent {
            id: event_id(SourceId::Gdelt, &source_event_id),
            source: SourceId::Gdelt,
            source_event_id,
            family: SignalFamily::MediaAttention,
            kind: EventKind::NewsAttention,
            themes: themes.clone(),
            ts_utc,
            ingested_at: Utc::now(),
            lat: mention.lat,
            lon: mention.lon,
            location_role: LocationRole::MentionedPlace,
            location_precision: mention.precision,
            location_confidence: mention.confidence,
            country_iso: mention.country_iso,
            admin1: mention.admin1,
            h3_cell,
            // One record per (article, place) mention; attention volume is
            // therefore counted in article-place mentions (docs/DATA_MODEL.md).
            volume_count: 1,
            distinct_source_count: 1,
            severity: None,
            headline: headline.clone(),
            outlet_domains: outlet_domains.clone(),
            urls: urls.clone(),
        };
        ev.validate()?;
        out.push(ev);
    }
    Ok(out)
}

/// One resolved location mention.
struct Mention {
    precision: LocationPrecision,
    confidence: f32,
    lat: f64,
    lon: f64,
    country_iso: String,
    admin1: Option<String>,
}

/// Parse `V2EnhancedLocations`: `;`-separated mentions, each `#`-separated into
/// `type|fullname|countrycode|adm1code|adm2code|lat|lon|featureid|charoffset`.
///
/// Mentions without a usable type or coordinates are skipped, not errors. The
/// same place is listed once per character offset it appears at, so mentions
/// are deduped to distinct (lat, lon) pairs per article (the A2 spike measured
/// 6,827 raw mentions collapsing to 2,109 distinct article-place pairs).
fn parse_mentions(field: &str) -> Vec<Mention> {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut out = Vec::new();
    for part in field.split(';') {
        let sub: Vec<&str> = part.split('#').collect();
        // Need through `lon` (index 6); `charoffset` (8) is optional.
        if sub.len() < 7 {
            continue;
        }
        let Some((precision, confidence)) = events::geo_precision(sub[0].trim()) else {
            continue;
        };
        let Ok(lat) = sub[5].trim().parse::<f64>() else {
            continue;
        };
        let Ok(lon) = sub[6].trim().parse::<f64>() else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        if !seen.insert((lat.to_bits(), lon.to_bits())) {
            continue;
        }
        let country_iso = country::iso3_from_fips(sub[2].trim())
            .unwrap_or("")
            .to_owned();
        let admin1 = match precision {
            LocationPrecision::Country => None,
            _ => {
                let code = sub[3].trim();
                (!code.is_empty()).then(|| code.to_owned())
            }
        };
        out.push(Mention {
            precision,
            confidence,
            lat,
            lon,
            country_iso,
            admin1,
        });
    }
    out
}

/// GKG `DATE`: 14-digit `YYYYMMDDHHMMSS` UTC.
fn parse_date(s: &str) -> Result<DateTime<Utc>, NormalizeError> {
    NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S")
        .map(|naive| Utc.from_utc_datetime(&naive))
        .map_err(|e| NormalizeError::InvalidValue {
            field: "date",
            detail: format!("`{s}`: {e}"),
        })
}

/// Extract `<PAGE_TITLE>…</PAGE_TITLE>` from `V2EXTRASXML`; `None` when absent
/// or empty. The title is metadata the feed provides; never a string this
/// crate invents (docs/SAFETY_AND_PRIVACY.md).
fn extract_page_title(xml: &str) -> Option<String> {
    let start = xml.find("<PAGE_TITLE>")? + "<PAGE_TITLE>".len();
    let end = start + xml[start..].find("</PAGE_TITLE>")?;
    let title = xml[start..end].trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// Document-level `Themes` (`;`-separated), lowercased and deduped. Shared by
/// every mention of the article — a theme is a property of the document, not
/// of any one location.
fn document_themes(field: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    field
        .split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 24-column GKG row with only the fields this module reads.
    fn row(fields: &[(usize, &str)]) -> String {
        let mut cols = vec![""; MIN_COLUMNS];
        for (i, v) in fields {
            cols[*i] = v;
        }
        cols.join("\t")
    }

    fn article_fields() -> Vec<(usize, &'static str)> {
        vec![
            (COL_GKGRECORDID, "20260819041500-3"),
            (COL_DATE, "20260819041500"),
            (COL_SOURCECOMMONNAME, "999thepoint.com"),
            (
                COL_DOCUMENTIDENTIFIER,
                "https://999thepoint.com/ixp/48/p/king-soopers-credit-card-ending/",
            ),
            (
                COL_THEMES,
                "TAX_FNCACT;NATURAL_DISASTER_CHILL;TAX_ECON_PRICE",
            ),
            (
                COL_V2EXTRASXML,
                "<PAGE_TITLE>King Soopers Credit Card Ending</PAGE_TITLE><PAGE_LINKS>https://k99.com/</PAGE_LINKS>",
            ),
        ]
    }

    #[test]
    fn city_mention_normalizes_as_mentioned_place_attention() {
        let mut fields = article_fields();
        fields.push((
            COL_V2ENHANCEDLOCATIONS,
            "4#Toowoomba, Queensland, Australia#AS#AS04#154695#-27.5606#151.954#-1605321#4031",
        ));
        let evs = normalize(&row(&fields)).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.family, SignalFamily::MediaAttention);
        assert_eq!(e.kind, EventKind::NewsAttention);
        assert_eq!(e.location_role, LocationRole::MentionedPlace);
        assert_eq!(e.location_precision, LocationPrecision::City);
        assert!(e.location_precision.renders_as_point());
        assert_eq!(e.country_iso, "AUS");
        assert_eq!(e.admin1.as_deref(), Some("AS04"));
        assert_eq!(e.source_event_id, "20260819041500-3#0");
        assert_eq!(
            e.headline.as_deref(),
            Some("King Soopers Credit Card Ending")
        );
        assert_eq!(
            e.themes,
            vec!["tax_fncact", "natural_disaster_chill", "tax_econ_price"]
        );
        assert_eq!(e.outlet_domains, vec!["999thepoint.com"]);
        assert_eq!(
            e.urls,
            vec!["https://999thepoint.com/ixp/48/p/king-soopers-credit-card-ending/"]
        );
        assert_eq!(e.volume_count, 1);
        assert_eq!(e.distinct_source_count, 1);
        assert_eq!(e.severity, None);
    }

    #[test]
    fn country_mention_shades_and_never_renders_as_point() {
        let mut fields = article_fields();
        fields.push((
            COL_V2ENHANCEDLOCATIONS,
            "1#United States#US#US##39.5#-98.35#US#0",
        ));
        let evs = normalize(&row(&fields)).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.location_precision, LocationPrecision::Country);
        assert!(!e.location_precision.renders_as_point());
        assert_eq!(e.country_iso, "USA");
        assert_eq!(e.admin1, None);
    }

    #[test]
    fn admin1_mention_carries_admin_code() {
        let mut fields = article_fields();
        fields.push((
            COL_V2ENHANCEDLOCATIONS,
            "2#Colorado, United States#US#USCO##39.0646#-105.327#CO#13",
        ));
        let evs = normalize(&row(&fields)).unwrap();
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.location_precision, LocationPrecision::Admin1);
        assert!(!e.location_precision.renders_as_point());
        assert_eq!(e.country_iso, "USA");
        assert_eq!(e.admin1.as_deref(), Some("USCO"));
    }

    #[test]
    fn one_article_many_distinct_mentions_yields_one_record_each() {
        let mut fields = article_fields();
        fields.push((
            COL_V2ENHANCEDLOCATIONS,
            "2#Kansas, United States#US#USKS##38.5111#-96.8005#KS#6;3#Kansas City, Kansas, United States#US#USKS#KS209#39.1142#-94.6275#478635#11",
        ));
        let evs = normalize(&row(&fields)).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].source_event_id, "20260819041500-3#0");
        assert_eq!(evs[1].source_event_id, "20260819041500-3#1");
        assert_eq!(evs[0].location_precision, LocationPrecision::Admin1);
        assert_eq!(evs[1].location_precision, LocationPrecision::City);
    }

    #[test]
    fn duplicate_offsets_of_the_same_place_collapse() {
        let mut fields = article_fields();
        fields.push((
            COL_V2ENHANCEDLOCATIONS,
            "2#Colorado, United States#US#USCO##39.0646#-105.327#CO#13;2#Colorado, United States#US#USCO##39.0646#-105.327#CO#1591;2#Colorado, United States#US#USCO##39.0646#-105.327#CO#1800",
        ));
        let evs = normalize(&row(&fields)).unwrap();
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn row_without_a_location_field_is_skipped() {
        let mut fields = article_fields();
        fields.push((COL_V2ENHANCEDLOCATIONS, ""));
        assert!(normalize(&row(&fields)).unwrap().is_empty());
    }

    #[test]
    fn malformed_row_fails_per_record() {
        assert!(matches!(
            normalize("just\tthree\tcols").unwrap_err(),
            NormalizeError::InvalidValue {
                field: "columns",
                ..
            }
        ));
    }

    #[test]
    fn gkg_url_selects_the_gkg_dump() {
        let txt = "\
50551 899f8323b97e9beb34186e39f2971071 http://data.gdeltproject.org/gdeltv2/20260819041500.export.CSV.zip\n\
62209 dfa29b51b44296c142d98952a1e777ab http://data.gdeltproject.org/gdeltv2/20260819041500.mentions.CSV.zip\n\
3154021 0a99c4f241ef45f063308a00544b006b http://data.gdeltproject.org/gdeltv2/20260819041500.gkg.csv.zip\n";
        let refs = events::parse_lastupdate(txt).unwrap();
        assert_eq!(
            gkg_url(&refs),
            Some("http://data.gdeltproject.org/gdeltv2/20260819041500.gkg.csv.zip")
        );
    }

    #[test]
    fn page_title_is_optional() {
        assert_eq!(extract_page_title(""), None);
        assert_eq!(
            extract_page_title("<PAGE_LINKS>https://k99.com/</PAGE_LINKS>"),
            None
        );
        assert_eq!(extract_page_title("<PAGE_TITLE></PAGE_TITLE>"), None);
    }
}
