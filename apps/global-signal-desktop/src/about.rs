//! "About" — attributions, licence, version.
//!
//! Every line here comes from `core_types::attribution`, which is the single
//! table of who owns what data. Nothing is restated locally, because a second
//! copy of an attribution string is a second copy that can drift out of date
//! while still looking authoritative. The one exception is the bundled
//! basemap, and it is called out as such below.
//!
//! Where an upstream's terms mandate a fixed citation, that string is rendered
//! verbatim — not paraphrased, not reflowed into a sentence of ours. That is
//! the whole reason `SourceAttribution::attribution_text` is `Option`: `Some`
//! means "print exactly this".

use crate::app::App;
use core_types::{ACCESS_DATE_SLOT, AttributionSubject, SourceAttribution, attribution_for};
use egui::{Color32, RichText, Ui};

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
const TEXT_LEAD: Color32 = Color32::from_rgb(216, 221, 231);

/// Public-domain data bundled into the binary rather than fetched, so it has
/// no row in the source table (that table is about *feeds*). Credited here
/// because it is on screen in every frame the map draws.
const BUNDLED: &[(&str, &str, &str)] = &[(
    "Natural Earth",
    "1:110m admin-0 countries and populated places, compiled into the binary \
     as the offline basemap and the country/city gazetteer.",
    "https://www.naturalearthdata.com/",
)];

/// Subjects worth showing a person. Fixtures exist in the table only to keep
/// it exhaustive over `SourceId` (CLAUDE.md: "the desktop does not load
/// them"), so listing them here would credit a source that never contributed
/// a record on screen.
///
/// Iterating a `const` array of `Copy` subjects allocates nothing per frame;
/// keep it that way.
fn listed() -> impl Iterator<Item = (AttributionSubject, SourceAttribution)> {
    AttributionSubject::ALL
        .into_iter()
        .filter(|s| {
            !matches!(
                s,
                AttributionSubject::Source(core_types::SourceId::Fixtures)
            )
        })
        .map(|s| (s, attribution_for(s)))
}

/// Label for the leg a row describes — the same upstream can appear twice
/// (GDELT as a scheduled source and as the Media page's video lookup) and the
/// two are governed separately, so the distinction is on screen.
fn leg(subject: AttributionSubject) -> &'static str {
    match subject {
        AttributionSubject::Source(_) => "scheduled source",
        AttributionSubject::GoogleGemini => "Daily Events (on request)",
        AttributionSubject::MediaGdelt
        | AttributionSubject::MediaBluesky
        | AttributionSubject::MediaTelegram => "Media page (on request)",
    }
}

/// UTC date of the fetch that put this subject's rows on screen, for the one
/// citation template that carries a date slot.
///
/// Read from the status lines the worker already sends — no query, no clock
/// call. Only a scheduled source has a fetch to date: the on-request legs run
/// per user action and persist nothing, so there is no "accessed on" for them
/// to claim.
fn accessed_on(app: &App, subject: AttributionSubject) -> Option<chrono::NaiveDate> {
    let AttributionSubject::Source(id) = subject else {
        return None;
    };
    let epoch = app
        .source_statuses
        .iter()
        .find(|s| s.source == id)?
        .last_success_epoch_s?;
    chrono::DateTime::from_timestamp(epoch, 0).map(|dt| dt.date_naive())
}

pub fn show(app: &App, ui: &mut Ui) {
    ui.label(RichText::new("Live Earth Signals").strong().size(16.0));
    ui.label(
        RichText::new(format!(
            "version {} · licensed {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_LICENSE"),
        ))
        .color(TEXT_DIM)
        .small(),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "This application displays data owned by other people. Their terms \
             are listed below, and where a citation is mandated it is printed \
             exactly as required.",
        )
        .color(TEXT_DIM),
    );
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .max_height(440.0)
        .show(ui, |ui| {
            ui.label(RichText::new("Required attributions").strong());
            ui.label(
                RichText::new(
                    "Rendered verbatim, as the upstream terms require. Do not \
                     reword these when quoting the application.",
                )
                .color(TEXT_DIM)
                .small(),
            );
            for (subject, attr) in listed() {
                // Borrows for every row but ACLED's, whose template has a
                // date slot to fill; one small string while this window is
                // open is the whole cost.
                let Some(text) = attr.citation(accessed_on(app, subject)) else {
                    continue;
                };
                ui.add_space(5.0);
                ui.label(
                    RichText::new(format!("{} — {}", attr.display_name, leg(subject)))
                        .color(TEXT_LEAD)
                        .small(),
                );
                // Monospace so it reads as a quoted citation rather than as
                // our own prose, and is obviously the thing to copy.
                ui.label(RichText::new(text.as_ref()).monospace().color(TEXT_LEAD));
                if text.contains(ACCESS_DATE_SLOT) {
                    // Nothing has been fetched from this source in this
                    // install, so there is no access date to assert. Say who
                    // has to fill it rather than inventing one.
                    ui.label(
                        RichText::new(
                            "No successful fetch here yet — put in the date you \
                             accessed the data before quoting this.",
                        )
                        .color(TEXT_DIM)
                        .small(),
                    );
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Sources and terms").strong());
            for (subject, attr) in listed() {
                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new(attr.display_name).color(TEXT_LEAD));
                    ui.label(RichText::new(leg(subject)).color(TEXT_DIM).small());
                    if let Some(url) = attr.homepage_url {
                        ui.hyperlink_to(RichText::new("terms ↗").small(), url);
                    }
                });
                ui.label(RichText::new(attr.licence_label).color(TEXT_DIM).small());
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new("Bundled data").strong());
            for (name, what, url) in BUNDLED {
                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new(*name).color(TEXT_LEAD));
                    ui.hyperlink_to(RichText::new("site ↗").small(), *url);
                });
                ui.label(RichText::new(*what).color(TEXT_DIM).small());
                ui.label(
                    RichText::new("Public domain; no attribution required, given anyway.")
                        .color(TEXT_DIM)
                        .small(),
                );
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "What the data can and cannot support is in the \"How to read \
                     this map\" window. The privacy and source-terms policy is in \
                     docs/SAFETY_AND_PRIVACY.md; per-source configuration is on the \
                     Settings screen.",
                )
                .color(TEXT_DIM)
                .small(),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of this screen. If a mandated citation stops being rendered,
    /// the application is out of compliance with an upstream's terms while
    /// still showing an "About" box that looks complete.
    #[test]
    fn every_mandated_citation_is_listed() {
        let mandated: Vec<_> = AttributionSubject::ALL
            .into_iter()
            .filter(|s| attribution_for(*s).attribution_text.is_some())
            .collect();
        assert!(
            !mandated.is_empty(),
            "the attribution table has no mandated citations at all"
        );
        for subject in mandated {
            assert!(
                listed().any(|(s, _)| s == subject),
                "{subject:?} mandates a citation but About filters it out"
            );
        }
    }

    /// Fixtures are the only subject this screen may drop, and only because
    /// the desktop never loads them.
    #[test]
    fn only_fixtures_are_omitted() {
        assert_eq!(listed().count() + 1, AttributionSubject::ALL.len());
        assert!(
            !listed().any(|(_, a)| a.display_name.contains("Fixtures")),
            "fixtures credited on a live-only desktop"
        );
    }

    /// A citation with a date slot can only be completed from a scheduled
    /// source's `last_success`. If a slot ever appears on an on-request leg,
    /// `accessed_on` has nothing to fill it with and the screen would show a
    /// bare `[DATE]` forever.
    #[test]
    fn every_dated_citation_belongs_to_a_scheduled_source() {
        for (subject, attr) in listed() {
            if attr.citation_needs_access_date() {
                assert!(
                    matches!(subject, AttributionSubject::Source(_)),
                    "{subject:?} needs an access date but has no scheduled fetch to date it from"
                );
            }
        }
    }

    /// `RichText` renders markup literally, so stray emphasis characters would
    /// appear on screen as punctuation.
    #[test]
    fn attribution_text_is_plain() {
        for (_, attr) in listed() {
            if let Some(text) = attr.attribution_text {
                assert!(!text.contains("**"), "{text}");
            }
        }
    }
}
