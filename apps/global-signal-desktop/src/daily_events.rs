//! The "Daily Events" page: one cached model-written digest per UTC calendar
//! day, replaceable by an explicit regeneration.
//!
//! This is the project's only *interpretive* surface — everywhere else the UI
//! paints stored records, here a language model writes prose about them. Two
//! consequences shape this file:
//!
//! 1. **The two sections are drawn separately and always with their counts.**
//!    Media attention and event data get their own headed blocks, each
//!    labelled with the number of records it was written from. The schema, the
//!    cache table, and the storage queries already enforce the split (see
//!    `daily_digest::output_schema` and `migrations/0003_daily_digest.sql`);
//!    this is the last of the four layers, and the only one the reader sees.
//! 2. **Generated text is framed as generated.** The provenance line (model,
//!    generation time) sits above the prose, not buried under it, and the
//!    caveat about media attention being a biased proxy is part of the page
//!    rather than something the model may or may not have said.

use daily_digest::{DayDigest, DayKey};
use egui::{Color32, RichText};
use storage::DigestDay;

use crate::app::App;

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
const HEADING_ATTENTION: Color32 = Color32::from_rgb(150, 190, 255);
const HEADING_EVENTS: Color32 = Color32::from_rgb(255, 176, 120);
const ERROR_FG: Color32 = Color32::from_rgb(255, 120, 120);

/// Days offered in the picker. A digest is a daily overview, so a couple of
/// months of history is more than any reader works through — and every extra
/// row is a day the user could spend an API call on by accident.
pub const DAY_LIMIT: usize = 60;

impl App {
    /// The whole page: day picker on the left, the selected day's digest in
    /// the centre. Called instead of the map panels, so the map's timeline
    /// and inspector are not on screen at all here.
    pub fn daily_events_page(&mut self, ui: &mut egui::Ui) {
        // Panel order: side first, central last (egui 0.35).
        egui::Panel::left("digest_days")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("days with data").strong());
                ui.label(
                    RichText::new("UTC calendar days, newest first. ✓ = already written.")
                        .small()
                        .color(TEXT_DIM),
                );
                ui.separator();
                let days = self.digest_days.clone();
                if days.is_empty() {
                    ui.label(
                        RichText::new("No stored records yet. Let the live sources run first.")
                            .color(TEXT_DIM),
                    );
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for day in &days {
                        self.day_row(ui, day);
                    }
                });
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.digest_body(ui);
            });
        });
    }

    /// One selectable day in the picker, with the two record counts that
    /// decide whether it is worth a digest at all.
    fn day_row(&mut self, ui: &mut egui::Ui, day: &DigestDay) {
        let selected = self.digest_day == Some(day.day);
        let mark = if day.cached { "✓" } else { "  " };
        let label = format!("{mark} {}", day.day.key());
        let response = ui.selectable_label(selected, label).on_hover_text(format!(
            "{} media-attention records, {} event records",
            day.attention_records, day.event_records
        ));
        ui.indent(day.day.key(), |ui| {
            ui.label(
                RichText::new(format!(
                    "attention {} · events {}",
                    day.attention_records, day.event_records
                ))
                .small()
                .color(TEXT_DIM),
            );
        });
        if response.clicked() {
            self.select_digest_day(day.day);
        }
    }

    /// The reading pane for the selected day.
    fn digest_body(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Daily Events").heading());
        ui.label(
            RichText::new(
                "A model-written summary of one day of stored records. It reads the same \
                 database the map does; it adds no facts of its own, and it is not a news \
                 report.",
            )
            .color(TEXT_DIM),
        );
        ui.separator();

        let Some(day) = self.digest_day else {
            ui.label(RichText::new("Pick a day on the left.").color(TEXT_DIM));
            return;
        };

        let counts = self.digest_days.iter().find(|d| d.day == day).copied();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(day.key()).strong());
            if let Some(c) = counts {
                ui.label(
                    RichText::new(format!(
                        "· {} media-attention records · {} event records",
                        c.attention_records, c.event_records
                    ))
                    .color(TEXT_DIM),
                );
            }
        });

        self.digest_actions(ui, day, counts);

        if let Some(err) = &self.digest_error {
            ui.add_space(6.0);
            ui.colored_label(ERROR_FG, err);
        }

        ui.add_space(8.0);
        match self.digest.clone() {
            Some(digest) if digest.day_utc == day => self.digest_sections(ui, &digest),
            _ if self.digest_generating == Some(day) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("writing this day's digest… (one API call, up to a minute)")
                            .color(TEXT_DIM),
                    );
                });
            }
            _ if self.digest_loading() => {
                ui.label(RichText::new("loading…").color(TEXT_DIM));
            }
            _ => {
                ui.label(RichText::new("No digest written for this day yet.").color(TEXT_DIM));
            }
        }
    }

    /// Generate / regenerate, plus the reason the button is missing when it
    /// is. Regenerating is deliberately a separate, second-guess action: it
    /// spends another API call and overwrites the cached row.
    fn digest_actions(&mut self, ui: &mut egui::Ui, day: DayKey, counts: Option<DigestDay>) {
        let empty_day = counts.is_some_and(|c| c.attention_records + c.event_records == 0);
        let have = self.digest.as_ref().is_some_and(|d| d.day_utc == day);
        let busy = self.digest_busy();

        ui.horizontal_wrapped(|ui| {
            if !self.digest_handle.available() {
                ui.label(RichText::new(crate::digest::unavailable_reason()).color(TEXT_DIM));
                return;
            }
            if empty_day {
                ui.label(
                    RichText::new("Nothing stored for this day — nothing to summarize.")
                        .color(TEXT_DIM),
                );
                return;
            }
            let label = if have {
                "regenerate"
            } else {
                "generate digest"
            };
            let button = ui
                .add_enabled(!busy, egui::Button::new(label))
                .on_hover_text(
                    "Sends this day's aggregate counts and record fields to Google's Gemini API \
                 and caches the result. One call per click.",
                );
            if button.clicked() {
                self.start_digest(day);
            }
            if have {
                ui.label(
                    RichText::new("cached — reopening this day costs nothing")
                        .small()
                        .color(TEXT_DIM),
                );
            }
        });
    }

    /// The digest itself: provenance, then the two sections, each headed and
    /// each carrying the record count it was written from.
    fn digest_sections(&self, ui: &mut egui::Ui, digest: &DayDigest) {
        ui.label(
            RichText::new(format!(
                "written by {} at {}",
                digest.model,
                chrono::DateTime::from_timestamp(digest.generated_at_epoch_s, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "an unknown time".into()),
            ))
            .small()
            .color(TEXT_DIM),
        );
        ui.add_space(10.0);

        section(
            ui,
            "Media attention",
            HEADING_ATTENTION,
            digest.attention_records,
            "how much coverage a place drew — a biased proxy for what happened, \
             not a record of it. Counted where the outlet is published, not              where the story happened",
            &digest.media_attention,
        );
        ui.add_space(14.0);
        section(
            ui,
            "Event data",
            HEADING_EVENTS,
            digest.event_records,
            "reported occurrences from the event sources, independent of how much \
             coverage they drew. Includes official alerts, which are warnings              issued by an agency rather than observed incidents",
            &digest.event_data,
        );

        ui.add_space(14.0);
        ui.separator();
        ui.label(
            RichText::new(
                "These two are counted and written separately and are never combined into \
                 one figure. A place can be loud in one and quiet in the other; that gap is \
                 the point, not an error.",
            )
            .small()
            .color(TEXT_DIM),
        );
    }
}

/// One headed section. Free function rather than a method: it holds no app
/// state, and keeping it that way makes it structurally impossible for one
/// section to render anything belonging to the other.
fn section(
    ui: &mut egui::Ui,
    heading: &str,
    color: Color32,
    records: u64,
    caveat: &str,
    body: &str,
) {
    ui.label(RichText::new(heading).heading().color(color));
    ui.label(
        RichText::new(format!("from {records} records — {caveat}"))
            .small()
            .color(TEXT_DIM),
    );
    ui.add_space(4.0);
    if body.trim().is_empty() {
        ui.label(RichText::new("(the model returned nothing for this section)").color(TEXT_DIM));
    } else {
        ui.label(body);
    }
}
