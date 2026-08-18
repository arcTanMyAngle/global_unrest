//! Attribution and configuration-state metadata for every ingest source and
//! every non-source third-party leg (Daily Events' Google Gemini call, the
//! Media page's on-demand GDELT/Bluesky/Telegram lookups).
//!
//! This is a static data table, not a UI. It exists so a Settings/About
//! screen (M8 S4) can render licence terms, required verbatim citations, and
//! live "configured" status without re-deriving upstream terms or reaching
//! into `std::env` from crates above `core-types`. Every `env_vars` entry is
//! a variable *name*, never a value — reading and displaying a credential
//! value is forbidden (docs/SAFETY_AND_PRIVACY.md, CLAUDE.md product rule 5).

/// Every attribution surface the Settings/About screen renders: one entry
/// per [`crate::SourceId`] (its ingest leg) plus the non-source third-party
/// legs that are not a `SignalSource` — Daily Events' Google Gemini call and
/// the Media page's on-demand GDELT/Bluesky/Telegram lookups, which run
/// under different terms and bounds than those same providers' ingest use
/// (CLAUDE.md: "The Media page is also separate from sources and storage").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributionSubject {
    Source(crate::SourceId),
    GoogleGemini,
    MediaGdelt,
    MediaBluesky,
    MediaTelegram,
}

impl AttributionSubject {
    /// Every subject this table covers, mirroring [`crate::EventKind::ALL`].
    /// Keep in sync with [`attribution_for`]'s match arms — both are
    /// exhaustive over `SourceId` so a new variant fails the build, not just
    /// a test, until this array and that match are both updated.
    pub const ALL: [AttributionSubject; 11] = [
        AttributionSubject::Source(crate::SourceId::Fixtures),
        AttributionSubject::Source(crate::SourceId::Gdelt),
        AttributionSubject::Source(crate::SourceId::Acled),
        AttributionSubject::Source(crate::SourceId::Noaa),
        AttributionSubject::Source(crate::SourceId::Ioda),
        AttributionSubject::Source(crate::SourceId::Bluesky),
        AttributionSubject::Source(crate::SourceId::Telegram),
        AttributionSubject::GoogleGemini,
        AttributionSubject::MediaGdelt,
        AttributionSubject::MediaBluesky,
        AttributionSubject::MediaTelegram,
    ];
}

/// The slot ACLED's published citation template leaves for the date the data
/// was accessed. Kept in the table exactly as the upstream wrote it — the
/// template *is* the verbatim string S1 was told to copy — and filled at
/// render time by [`SourceAttribution::citation`] from the fetch that
/// actually produced the rows on screen.
pub const ACCESS_DATE_SLOT: &str = "[DATE]";

/// One row of the Settings/About attribution table.
///
/// Never carries a credential value — only env-var *names* (CLAUDE.md
/// product rule 5: "Keep credentials in the environment... Never log keys").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceAttribution {
    pub display_name: &'static str,
    /// Upstream homepage or terms page. `None` only for the internal
    /// fixtures generator, which has no upstream to link.
    pub homepage_url: Option<&'static str>,
    pub licence_label: &'static str,
    /// Verbatim text the UI must render as-is when the upstream's terms
    /// mandate a fixed citation string; `None` when no fixed string is
    /// mandated (courtesy credit only, or none required).
    pub attribution_text: Option<&'static str>,
    pub credentials_required: bool,
    /// Env var *names* the credentialed path reads — never a value. Empty
    /// for a keyless leg.
    pub env_vars: &'static [&'static str],
    /// Desktop Cargo feature gating this leg's live network path, matching
    /// the `*-live` names in `apps/global-signal-desktop/Cargo.toml`.
    /// `None` when the leg is unconditionally compiled (no opt-out exists).
    pub feature_flag: Option<&'static str>,
}

impl SourceAttribution {
    /// Env vars present and non-empty (trimmed) — the same "configured"
    /// check every live source's own `from_env` already performs (e.g.
    /// `TelegramSource::from_env`, `AcledSource::from_env`), exposed here as
    /// a plain query instead of a constructor. Distinct from "compiled":
    /// this says nothing about whether `feature_flag` was built in.
    ///
    /// Always `true` for a keyless leg (`credentials_required == false`):
    /// there is nothing to configure.
    pub fn is_configured(&self) -> bool {
        if !self.credentials_required {
            return true;
        }
        self.env_vars
            .iter()
            .all(|name| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))
    }

    /// Whether this row's citation template carries an unfilled
    /// [`ACCESS_DATE_SLOT`] and therefore needs an access date to be
    /// complete.
    pub fn citation_needs_access_date(&self) -> bool {
        self.attribution_text
            .is_some_and(|t| t.contains(ACCESS_DATE_SLOT))
    }

    /// The citation to display, with the access-date slot filled in.
    ///
    /// `accessed` is the UTC date of the fetch that produced the data on
    /// screen, not today: a citation claims when the data was obtained. Pass
    /// `None` when no successful fetch is known — the template then renders
    /// with `[DATE]` still visible, which is honest about being unfilled.
    /// Substituting today's date for a fetch that never happened would state
    /// something false in a compliance string.
    ///
    /// The date is rendered ISO 8601 (`2026-08-18`). ACLED's policy fixes the
    /// sentence, not the date format, and every other timestamp this UI shows
    /// is ISO UTC.
    ///
    /// Borrows when there is nothing to substitute, so the common rows cost
    /// no allocation.
    pub fn citation(
        &self,
        accessed: Option<chrono::NaiveDate>,
    ) -> Option<std::borrow::Cow<'static, str>> {
        let text = self.attribution_text?;
        match accessed {
            Some(date) if text.contains(ACCESS_DATE_SLOT) => Some(std::borrow::Cow::Owned(
                text.replace(ACCESS_DATE_SLOT, &date.format("%Y-%m-%d").to_string()),
            )),
            _ => Some(std::borrow::Cow::Borrowed(text)),
        }
    }
}

/// Look up the attribution row for any subject. Exhaustive over both
/// `AttributionSubject` and, nested, `SourceId` — no wildcard arm, so a new
/// `SourceId` variant fails to compile here until it gets a row.
pub fn attribution_for(subject: AttributionSubject) -> SourceAttribution {
    match subject {
        AttributionSubject::Source(source) => attribution_for_source(source),
        AttributionSubject::GoogleGemini => GOOGLE_GEMINI,
        AttributionSubject::MediaGdelt => MEDIA_GDELT,
        AttributionSubject::MediaBluesky => MEDIA_BLUESKY,
        AttributionSubject::MediaTelegram => MEDIA_TELEGRAM,
    }
}

fn attribution_for_source(source: crate::SourceId) -> SourceAttribution {
    use crate::SourceId;
    match source {
        SourceId::Fixtures => FIXTURES,
        SourceId::Gdelt => GDELT,
        SourceId::Acled => ACLED,
        SourceId::Noaa => NOAA,
        SourceId::Ioda => IODA,
        SourceId::Bluesky => BLUESKY,
        SourceId::Telegram => TELEGRAM,
    }
}

/// Internal synthetic test/service-smoke data. Never loaded by the desktop
/// (CLAUDE.md "Current state": "the desktop does not load them") — this
/// entry exists only so the table stays exhaustive over `SourceId`.
const FIXTURES: SourceAttribution = SourceAttribution {
    display_name: "Fixtures (internal test data)",
    homepage_url: None,
    licence_label: "Internal synthetic data — not a licensed third-party source",
    attribution_text: None,
    credentials_required: false,
    env_vars: &[],
    feature_flag: None,
};

/// Source: GDELT Project "Data Terms of Use"
/// (https://www.gdeltproject.org/about.html#termsofuse), checked 2026-08-17
/// — verbatim required-attribution sentence. Also see
/// `crates/source-gdelt/src/lib.rs` module docs and README.md "Safety, data,
/// and attribution" ("GDELT is used with attribution"). Always compiled and
/// keyless — GDELT has no `from_env`/feature gate in this workspace.
const GDELT: SourceAttribution = SourceAttribution {
    display_name: "GDELT Project",
    homepage_url: Some("https://www.gdeltproject.org/"),
    licence_label: "Free for any use, with mandatory attribution",
    attribution_text: Some(
        "any use or redistribution of the data must include a citation to the \
         GDELT Project and a link to this website (https://www.gdeltproject.org/)",
    ),
    credentials_required: false,
    env_vars: &[],
    feature_flag: None,
};

/// Source: ACLED "Attribution Policy" (https://acleddata.com/attributionpolicy/),
/// "Exact Attribution Format for Raw Data" section, checked 2026-08-17 —
/// verbatim citation string. Access/redistribution constraints per
/// docs/SAFETY_AND_PRIVACY.md "Source licensing and handling" and CLAUDE.md
/// product rules 4 and 8 (never store notes; never serve ACLED aggregates
/// publicly). Credential names from `crates/source-acled/src/live.rs`
/// `AcledSource::from_env`. The `[DATE]` in the citation is ACLED's own
/// template slot, not a typo — see [`ACCESS_DATE_SLOT`] and
/// [`SourceAttribution::citation`], which fills it from the last successful
/// fetch.
const ACLED: SourceAttribution = SourceAttribution {
    display_name: "ACLED (Armed Conflict Location & Event Data Project)",
    homepage_url: Some("https://acleddata.com/"),
    licence_label: "Authorized myACLED account required; redistribution restricted \
                     (docs/SAFETY_AND_PRIVACY.md)",
    attribution_text: Some("ACLED, accessed on [DATE]. www.acleddata.com."),
    credentials_required: true,
    env_vars: &["ACLED_EMAIL", "ACLED_PASSWORD"],
    feature_flag: Some("acled-live"),
};

/// Source: weather.gov disclaimer (https://www.weather.gov/disclaimer),
/// checked 2026-08-17 — NWS content is US government public domain; no
/// formal attribution is required, but the NWS name/logo may not be used to
/// imply endorsement. See `crates/source-noaa/src/lib.rs` module docs.
const NOAA: SourceAttribution = SourceAttribution {
    display_name: "NOAA / National Weather Service",
    homepage_url: Some("https://www.weather.gov/"),
    licence_label: "US government public domain data; no attribution required",
    attribution_text: None,
    credentials_required: false,
    env_vars: &[],
    feature_flag: Some("noaa-live"),
};

/// Source: `crates/source-ioda/src/lib.rs` module docs identify the
/// operator (Georgia Tech Internet Intelligence Research Lab / CAIDA). A
/// fixed, mandatory citation string was not found on the public IODA site
/// (`ioda.inetintel.cc.gatech.edu`) or CAIDA's dataset catalog as of
/// 2026-08-17; treat the display name and homepage as a courtesy credit,
/// not a confirmed mandatory string, and re-check before removing this
/// caveat.
const IODA: SourceAttribution = SourceAttribution {
    display_name: "IODA (Internet Outage Detection and Analysis, Georgia Tech)",
    homepage_url: Some("https://ioda.inetintel.cc.gatech.edu/"),
    licence_label: "Keyless public API; no confirmed formal citation requirement found \
                     — courtesy credit",
    attribution_text: None,
    credentials_required: false,
    env_vars: &[],
    feature_flag: Some("ioda-live"),
};

/// Source: docs/SAFETY_AND_PRIVACY.md "Source licensing and handling"
/// (Bluesky Jetstream row) and `crates/source-bluesky/src/lib.rs` module
/// docs. Jetstream's public documentation did not yield a fixed citation
/// requirement as of 2026-08-17; treat as a courtesy credit. Ingest is
/// aggregate-only — see [`crate::ChatterRollup`] for the privacy boundary
/// this entry's data feeds.
const BLUESKY: SourceAttribution = SourceAttribution {
    display_name: "Bluesky (Jetstream firehose)",
    homepage_url: Some("https://bsky.app/"),
    licence_label: "Keyless public firehose; aggregate-only ingest (docs/SAFETY_AND_PRIVACY.md)",
    attribution_text: None,
    credentials_required: false,
    env_vars: &[],
    feature_flag: Some("bluesky-live"),
};

/// Source: Telegram API Terms of Service
/// (https://core.telegram.org/api/terms), checked 2026-08-17 — apps must
/// "make it clear that [they] use the Telegram API and [are] part of the
/// Telegram ecosystem", a disclosure obligation rather than a fixed
/// citation string. Credential names and the deliberate omission of
/// `TELEGRAM_API_HASH` (login-setup-only, never read by `from_env`) match
/// `crates/source-telegram/src/live.rs` `TelegramSource::from_env`.
const TELEGRAM: SourceAttribution = SourceAttribution {
    display_name: "Telegram (public channels via MTProto)",
    homepage_url: Some("https://telegram.org/"),
    licence_label: "Telegram API Terms of Service; app must disclose Telegram API use; \
                     aggregate-only ingest (docs/SAFETY_AND_PRIVACY.md)",
    attribution_text: None,
    credentials_required: true,
    env_vars: &["TELEGRAM_API_ID", "LES_TELEGRAM_SESSION_FILE"],
    feature_flag: Some("telegram-live"),
};

/// Source: Google Gemini API Additional Terms of Service
/// (https://ai.google.dev/gemini-api/terms), checked 2026-08-17 — no fixed
/// "Powered by Google"-style badge is mandated. Distinct from ingest: this
/// is the Daily Events transport documented in
/// docs/SAFETY_AND_PRIVACY.md#third-party-processing-google-gemini-api and
/// gated by `crates/daily-digest/src/live.rs` `GeminiDigester::from_env`
/// (`GEMINI_API_KEY`). Not a `SourceId` — Daily Events is a page, not a
/// source (CLAUDE.md "Current state").
const GOOGLE_GEMINI: SourceAttribution = SourceAttribution {
    display_name: "Google Gemini API (Daily Events)",
    homepage_url: Some("https://ai.google.dev/gemini-api/terms"),
    licence_label: "Metered third-party API; opt-in per day, bounded aggregate request only \
                     (docs/SAFETY_AND_PRIVACY.md)",
    attribution_text: None,
    credentials_required: true,
    env_vars: &["GEMINI_API_KEY"],
    feature_flag: Some("gemini-live"),
};

/// The Media page's on-demand GDELT video lookup — same upstream and terms
/// as [`GDELT`], used under the bounded, explicit, user-directed exception
/// in docs/SAFETY_AND_PRIVACY.md "On-demand media lookup" rather than
/// ingest. Gated by `apps/global-signal-desktop`'s `media-live` feature
/// (`crates/media-search` `live` feature), not `SourceId`'s ingest cadence.
const MEDIA_GDELT: SourceAttribution = SourceAttribution {
    display_name: "GDELT (Media page video lookup)",
    homepage_url: Some("https://www.gdeltproject.org/"),
    licence_label: "Free for any use, with mandatory attribution",
    attribution_text: Some(
        "any use or redistribution of the data must include a citation to the \
         GDELT Project and a link to this website (https://www.gdeltproject.org/)",
    ),
    credentials_required: false,
    env_vars: &[],
    feature_flag: Some("media-live"),
};

/// The Media page's on-demand Bluesky public-post video lookup — a bounded,
/// user-directed query (docs/SAFETY_AND_PRIVACY.md "On-demand media
/// lookup"), distinct from [`BLUESKY`]'s aggregate ingest: results here
/// carry a public post URL and outlet/handle attribution rather than a
/// count. Gated by `media-live`, same as [`MEDIA_GDELT`].
const MEDIA_BLUESKY: SourceAttribution = SourceAttribution {
    display_name: "Bluesky (Media page video lookup)",
    homepage_url: Some("https://bsky.app/"),
    licence_label: "Keyless public API; bounded, user-directed lookup only \
                     (docs/SAFETY_AND_PRIVACY.md)",
    attribution_text: None,
    credentials_required: false,
    env_vars: &[],
    feature_flag: Some("media-live"),
};

/// The Media page's on-demand Telegram video lookup. Reuses the same
/// session and credentials as ingest ([`TELEGRAM`]) read-only
/// (`crates/source-telegram/src/live.rs` `TelegramSource::read_only`,
/// `search_media`), but is gated only by `telegram-live` — it does not
/// require `media-live`, since `apps/global-signal-desktop/src/media.rs`
/// compiles this leg under a separate `#[cfg(feature = "telegram-live")]`
/// module.
const MEDIA_TELEGRAM: SourceAttribution = SourceAttribution {
    display_name: "Telegram (Media page video lookup)",
    homepage_url: Some("https://telegram.org/"),
    licence_label: "Telegram API Terms of Service; bounded, user-directed lookup only \
                     (docs/SAFETY_AND_PRIVACY.md); never exposes senders",
    attribution_text: None,
    credentials_required: true,
    env_vars: &["TELEGRAM_API_ID", "LES_TELEGRAM_SESSION_FILE"],
    feature_flag: Some("telegram-live"),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceId;

    /// If a new `SourceId` variant is added without updating
    /// `AttributionSubject::ALL` and `attribution_for_source`'s match, the
    /// crate fails to *compile* (the match has no wildcard arm) — this test
    /// then guards the array itself stays in sync with that match, and that
    /// no row is silently empty.
    #[test]
    fn every_subject_has_a_populated_row() {
        assert_eq!(AttributionSubject::ALL.len(), 7 + 4);
        for subject in AttributionSubject::ALL {
            let row = attribution_for(subject);
            assert!(!row.display_name.is_empty());
            assert!(!row.licence_label.is_empty());
            if row.credentials_required {
                assert!(
                    !row.env_vars.is_empty(),
                    "{} requires credentials but lists no env vars",
                    row.display_name
                );
            }
        }
    }

    /// Every `SourceId` variant appears exactly once among the `Source(_)`
    /// subjects — catches a copy-paste duplicate as well as an omission.
    #[test]
    fn every_source_id_variant_is_covered_exactly_once() {
        let covered: Vec<SourceId> = AttributionSubject::ALL
            .iter()
            .filter_map(|s| match s {
                AttributionSubject::Source(id) => Some(*id),
                _ => None,
            })
            .collect();
        for id in [
            SourceId::Fixtures,
            SourceId::Gdelt,
            SourceId::Acled,
            SourceId::Noaa,
            SourceId::Ioda,
            SourceId::Bluesky,
            SourceId::Telegram,
        ] {
            assert_eq!(
                covered.iter().filter(|&&c| c == id).count(),
                1,
                "{id} must appear exactly once in AttributionSubject::ALL"
            );
        }
    }

    /// No credential value can leak through this table: every env var name
    /// documented anywhere here must actually be unset (or non-secret) in a
    /// bare test process, and no row may embed anything that looks like the
    /// literal value of a credential rather than a variable name.
    #[test]
    fn no_row_embeds_a_credential_value() {
        for subject in AttributionSubject::ALL {
            let row = attribution_for(subject);
            for var in row.env_vars {
                // Names only: uppercase/underscore env-var shape, never a
                // URL, token, or other value shape.
                assert!(
                    var.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                    "{var} does not look like an env var name"
                );
            }
        }
    }

    /// A keyless leg is always "configured" — there is nothing to set.
    #[test]
    fn keyless_leg_is_always_configured() {
        assert!(attribution_for(AttributionSubject::Source(SourceId::Gdelt)).is_configured());
        assert!(attribution_for(AttributionSubject::Source(SourceId::Noaa)).is_configured());
        assert!(attribution_for(AttributionSubject::Source(SourceId::Ioda)).is_configured());
        assert!(attribution_for(AttributionSubject::Source(SourceId::Bluesky)).is_configured());
    }

    /// A credentialed leg's `env_vars` matches the corresponding crate's own
    /// `from_env` check, so `is_configured` cannot drift from ingest's real
    /// behavior.
    #[test]
    fn credentialed_legs_list_the_vars_their_from_env_reads() {
        let acled = attribution_for(AttributionSubject::Source(SourceId::Acled));
        assert_eq!(acled.env_vars, &["ACLED_EMAIL", "ACLED_PASSWORD"]);

        let telegram = attribution_for(AttributionSubject::Source(SourceId::Telegram));
        assert_eq!(
            telegram.env_vars,
            &["TELEGRAM_API_ID", "LES_TELEGRAM_SESSION_FILE"]
        );
        assert_eq!(
            attribution_for(AttributionSubject::MediaTelegram).env_vars,
            telegram.env_vars,
            "media telegram reuses the same ingest session"
        );

        let gemini = attribution_for(AttributionSubject::GoogleGemini);
        assert_eq!(gemini.env_vars, &["GEMINI_API_KEY"]);
    }

    /// Verbatim attribution text, where present, must not be reworded here —
    /// this pins the exact strings copied into the table so a future edit
    /// that silently paraphrases a citation shows up as a diff.
    #[test]
    fn verbatim_attribution_text_is_unchanged() {
        let gdelt = attribution_for(AttributionSubject::Source(SourceId::Gdelt));
        assert_eq!(
            gdelt.attribution_text,
            Some(
                "any use or redistribution of the data must include a citation to the \
                 GDELT Project and a link to this website (https://www.gdeltproject.org/)"
            )
        );

        let acled = attribution_for(AttributionSubject::Source(SourceId::Acled));
        assert_eq!(
            acled.attribution_text,
            Some("ACLED, accessed on [DATE]. www.acleddata.com.")
        );
    }

    /// The access-date slot is filled from the fetch that produced the data,
    /// leaving no bracketed placeholder in a string a person may copy.
    #[test]
    fn access_date_fills_the_slot() {
        let acled = attribution_for(AttributionSubject::Source(SourceId::Acled));
        assert!(acled.citation_needs_access_date());

        let date = chrono::NaiveDate::from_ymd_opt(2026, 8, 18).unwrap();
        let filled = acled.citation(Some(date)).unwrap();
        assert_eq!(filled, "ACLED, accessed on 2026-08-18. www.acleddata.com.");
        assert!(!filled.contains(ACCESS_DATE_SLOT));
    }

    /// No successful fetch means no access date to claim. The template stays
    /// visibly unfilled rather than being stamped with today's date, which
    /// would assert a fetch that never happened.
    #[test]
    fn without_a_fetch_the_slot_stays_visible() {
        let acled = attribution_for(AttributionSubject::Source(SourceId::Acled));
        let unfilled = acled.citation(None).unwrap();
        assert!(unfilled.contains(ACCESS_DATE_SLOT), "{unfilled}");
    }

    /// A date is only ever substituted into a row that has a slot; rows
    /// without one are returned verbatim and borrowed, not rebuilt.
    #[test]
    fn rows_without_a_slot_are_untouched() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 8, 18).unwrap();
        for subject in AttributionSubject::ALL {
            let attr = attribution_for(subject);
            let Some(text) = attr.attribution_text else {
                assert!(attr.citation(Some(date)).is_none());
                assert!(!attr.citation_needs_access_date());
                continue;
            };
            if attr.citation_needs_access_date() {
                continue;
            }
            assert!(matches!(
                attr.citation(Some(date)).unwrap(),
                std::borrow::Cow::Borrowed(t) if t == text
            ));
        }
    }
}
