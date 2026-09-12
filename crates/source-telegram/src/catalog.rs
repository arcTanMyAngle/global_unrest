//! Classified channel catalog — the M10 successor to the compiled-in
//! [`crate::ALLOWED_CHANNELS`] handle list.
//!
//! A TOML file, not a handle list, because a handle list cannot carry the
//! two facts every channel here must assert: its **class** (whose messaging
//! its posting rate tracks — see docs/SIGNAL_MODEL.md) and its **region**
//! (where the channel's beat is, as *provenance* — never the geolocation of
//! any post). An entry without both is **rejected at load time**, not
//! defaulted: silently assuming `Monitor` is exactly how a combatant's
//! self-reporting ends up summed into a neutral aggregate.
//!
//! One catalog feeds both legs of this crate (ROADMAP M10): the ingest
//! poller ([`crate::ChannelOrchestrator`]) and the on-demand media lookup
//! ([`crate::search_all`]), so a channel is never counted under one
//! provenance and searched under another.
//!
//! Pure by design: parsing and validation need no Telegram session, so the
//! load-time rejection rules are unit-tested here without the `live`
//! feature.

use core_types::ChannelClass;

/// One catalog entry, as loaded and validated.
///
/// Owns its strings (unlike the `&'static` compiled-in list) because it is
/// read from a file at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Stable identifier for logs and coverage bookkeeping. Unique per file.
    pub id: String,
    /// Bare public username, no `@`, whitespace, or `/` — the same
    /// constraint [`crate::media::post_url`] relies on.
    pub handle: String,
    /// Who runs the channel, as provenance. Mandatory, never guessed.
    pub class: ChannelClass,
    /// The channel's beat, as provenance: `"global"`, `"ukraine"`, …
    /// **Never** the geolocation of a post — a post is placed only by its
    /// own text through the chatter matcher.
    pub region: String,
    /// Poll cadence in seconds. Currently informational: the orchestrator
    /// sweeps the whole catalog each cycle, and per-channel cadences land
    /// with the M10 scheduler. Kept mandatory so a catalog written now does
    /// not silently acquire a wrong cadence later.
    pub cadence_secs: u64,
}

/// The raw TOML shape. Every field is mandatory *on the wire* so that a
/// missing `class` or `region` is a parse error naming the entry, not a
/// defaulted value — the "rejected, not defaulted" rule.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    channel: Vec<RawChannel>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChannel {
    id: String,
    handle: String,
    class: String,
    region: String,
    cadence_secs: u64,
}

/// Parse and validate a catalog file's contents.
///
/// An error names the offending entry where one can be identified. A file
/// that parses but fails validation — an unknown class, a malformed handle,
/// a blank region, a zero cadence, or a duplicated id/handle — fails the
/// whole load: a partially-loaded catalog is how one misclassified channel
/// quietly poisons an aggregate.
pub fn parse(text: &str) -> Result<Vec<CatalogEntry>, String> {
    let raw: RawCatalog = toml::from_str(text).map_err(|e| format!("invalid catalog TOML: {e}"))?;
    let mut entries = Vec::with_capacity(raw.channel.len());
    for ch in &raw.channel {
        entries.push(validate(ch)?);
    }
    for what in ["id", "handle"] {
        let mut seen = std::collections::HashSet::with_capacity(entries.len());
        for entry in &entries {
            let value = match what {
                "id" => entry.id.as_str(),
                _ => entry.handle.as_str(),
            };
            if !seen.insert(value) {
                return Err(format!(
                    "duplicate {what} `{value}` — one channel, one entry"
                ));
            }
        }
    }
    Ok(entries)
}

/// Read and validate a catalog file from disk.
#[cfg(feature = "live")]
pub fn load(path: &std::path::Path) -> Result<Vec<CatalogEntry>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("reading catalog `{}`: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn validate(raw: &RawChannel) -> Result<CatalogEntry, String> {
    let id = raw.id.trim();
    if id.is_empty() {
        return Err("a channel has a blank `id`".to_string());
    }
    let handle = raw.handle.trim();
    if handle.is_empty()
        || handle.starts_with('@')
        || handle.contains('/')
        || handle.chars().any(char::is_whitespace)
    {
        return Err(format!(
            "channel `{id}`: handle `{handle}` must be a bare username — no `@`, `/`, or whitespace"
        ));
    }
    let class = ChannelClass::parse(raw.class.trim()).ok_or_else(|| {
        format!(
            "channel `{id}`: unknown class `{}` — expected one of {}",
            raw.class,
            ChannelClass::ALL.map(ChannelClass::as_str).join(", ")
        )
    })?;
    // The neutral aggregate's floor: a catalog may mark a channel
    // Monitor/Outlet/etc., but `Unspecified` is "no provenance asserted",
    // which is precisely what a classified pack must not contain.
    if class == ChannelClass::Unspecified {
        return Err(format!(
            "channel `{id}`: class `unspecified` asserts no provenance — a catalog entry must name one"
        ));
    }
    let region = raw.region.trim();
    if region.is_empty() {
        return Err(format!(
            "channel `{id}`: blank `region` — provenance is mandatory, not defaulted"
        ));
    }
    if raw.cadence_secs == 0 {
        return Err(format!("channel `{id}`: `cadence_secs` must be positive"));
    }
    Ok(CatalogEntry {
        id: id.to_string(),
        handle: handle.to_string(),
        class,
        region: region.to_string(),
        cadence_secs: raw.cadence_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[[channel]]
id = "amk-mapping"
handle = "AMK_Mapping"
class = "monitor"
region = "ukraine"
cadence_secs = 900

[[channel]]
id = "kyiv-independent"
handle = "kyivindependent_official"
class = "outlet"
region = "ukraine"
cadence_secs = 900
"#;

    #[test]
    fn a_well_formed_catalog_loads() {
        let entries = parse(GOOD).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].handle, "AMK_Mapping");
        assert_eq!(entries[0].class, ChannelClass::Monitor);
        assert_eq!(entries[0].region, "ukraine");
        assert_eq!(entries[0].cadence_secs, 900);
    }

    #[test]
    fn a_missing_class_is_rejected_not_defaulted() {
        let text = GOOD.replace("class = \"monitor\"\n", "");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("class"), "unexpected error: {err}");
    }

    #[test]
    fn a_missing_region_is_rejected_not_defaulted() {
        let text = GOOD.replace("region = \"ukraine\"\n", "");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("region"), "unexpected error: {err}");
    }

    #[test]
    fn an_unknown_class_is_rejected() {
        let text = GOOD.replace("class = \"monitor\"", "class = \"reliable\"");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("unknown class"), "unexpected error: {err}");
        assert!(err.contains("amk-mapping"), "names the entry: {err}");
    }

    #[test]
    fn unspecified_class_is_rejected_as_no_provenance() {
        let text = GOOD.replace("class = \"monitor\"", "class = \"unspecified\"");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("no provenance"), "unexpected error: {err}");
    }

    #[test]
    fn partisan_combatant_and_state_classes_are_accepted() {
        for class in ["partisan", "combatant", "state"] {
            let text = GOOD.replace("class = \"monitor\"", &format!("class = \"{class}\""));
            let entries = parse(&text).unwrap();
            assert_eq!(entries[0].class.as_str(), class);
        }
    }

    #[test]
    fn a_decorated_handle_is_rejected() {
        for handle in ["@AMK_Mapping", "AMK Mapping", "AMK/Mapping"] {
            let text = GOOD.replace(
                "handle = \"AMK_Mapping\"",
                &format!("handle = \"{handle}\""),
            );
            let err = parse(&text).unwrap_err();
            assert!(err.contains("bare username"), "{handle}: {err}");
        }
    }

    #[test]
    fn a_zero_cadence_is_rejected() {
        let text = GOOD.replace("cadence_secs = 900", "cadence_secs = 0");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("cadence"), "unexpected error: {err}");
    }

    #[test]
    fn duplicate_ids_and_handles_are_rejected() {
        let dup_id = GOOD.replace("id = \"kyiv-independent\"", "id = \"amk-mapping\"");
        assert!(parse(&dup_id).unwrap_err().contains("duplicate id"));
        let dup_handle = GOOD.replace(
            "handle = \"kyivindependent_official\"",
            "handle = \"AMK_Mapping\"",
        );
        assert!(parse(&dup_handle).unwrap_err().contains("duplicate handle"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let text = GOOD.replace("cadence_secs = 900", "cadence_secs = 900\ntrusted = true");
        let err = parse(&text).unwrap_err();
        assert!(err.contains("trusted"), "unexpected error: {err}");
    }
}
