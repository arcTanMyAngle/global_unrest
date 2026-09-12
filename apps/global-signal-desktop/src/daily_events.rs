//! The "Daily Events" page: one cached model-written digest per UTC calendar
//! day, replaceable by an explicit regeneration — laid out to read like a
//! newspaper, because the model is asked to write like one.
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
//! 2. **Generated text is framed as generated.** The byline (model,
//!    generation time) sits above the prose, not buried under it, and the
//!    caveat about media attention being a biased proxy is part of the page
//!    rather than something the model may or may not have said.
//!
//! The print-paper look is carried entirely by frames, rules, and type
//! scale — no images, no textures — so a frame's work is the few painter
//! calls egui was going to make anyway.

use daily_digest::{DayDigest, DayKey};
use egui::{Align2, Color32, FontId, Frame, Margin, RichText, Sense, Stroke, TextStyle, Vec2};
use storage::DigestDay;

use crate::app::App;

/// Page chrome: the "paper" the column is printed on, and the ink it is
/// printed in. Dark-newsroom rather than sepia so the page still sits inside
/// the app's theme, but lit like a page under a lamp.
const PAPER: Color32 = Color32::from_rgb(30, 30, 35);
const INK: Color32 = Color32::from_rgb(226, 222, 210);
const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
/// Hairlines and rules, kept faint so the type carries the page.
const RULE: Color32 = Color32::from_rgb(74, 76, 88);
const HEADING_ATTENTION: Color32 = Color32::from_rgb(150, 190, 255);
const HEADING_EVENTS: Color32 = Color32::from_rgb(255, 176, 120);
const ERROR_FG: Color32 = Color32::from_rgb(255, 120, 120);
/// The card a section is set in — a half-step off the paper so the two
/// columns read as laid-in plates, not floating boxes.
const SECTION_BG: Color32 = Color32::from_rgb(37, 37, 44);
/// The drop cap borrows the ink of the paper itself.
const DROP_CAP: Color32 = Color32::from_rgb(226, 222, 210);

/// Days offered in the picker. A digest is a daily overview, so a couple of
/// months of history is more than any reader works through — and every extra
/// row is a day the user could spend an API call on by accident.
pub const DAY_LIMIT: usize = 60;

/// One frame's horizontal edge insets for the reading column. egui re-runs
/// this every frame, so the column tracks the window with no resize handling
/// and no stored layout state.
const COLUMN_MIN_MARGIN: f32 = 28.0;
const COLUMN_MAX_WIDTH: f32 = 780.0;

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

        egui::CentralPanel::default()
            .frame(Frame::new().fill(PAPER))
            .show(ui, |ui| {
                // The reading column: full width while narrow, capped and
                // centred once wide — measured from the live width each
                // frame, so resizing costs nothing but the frames that were
                // being painted anyway.
                let avail = ui.available_width();
                let column = avail.min(COLUMN_MAX_WIDTH);
                let side = ((avail - column) / 2.0).max(COLUMN_MIN_MARGIN);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // `Margin` is i8-per-side, far too small for a wide
                        // window's gutters — and a horizontal strip or a
                        // nested Frame would both break the column's
                        // top-down flow. `allocate_new_ui` gives the column
                        // its own bounded, offset child Ui instead, which
                        // keeps vertical layout intact at any width.
                        ui.add_space(6.0);
                        let width = ui.available_width();
                        let (id, rect) =
                            ui.allocate_space(Vec2::new(width, ui.available_height().max(1.0)));
                        let column_rect = egui::Rect::from_min_size(
                            rect.min + Vec2::new(side, 0.0),
                            Vec2::new(column, rect.height()),
                        );
                        let mut column_ui =
                            ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(column_rect));
                        self.digest_body(&mut column_ui);
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

    /// The reading pane for the selected day: masthead, then the day's
    /// column.
    fn digest_body(&mut self, ui: &mut egui::Ui) {
        self.masthead(ui);

        let Some(day) = self.digest_day else {
            ui.add_space(18.0);
            ui.label(
                RichText::new("Pick a day on the left to open its edition.")
                    .italics()
                    .color(TEXT_DIM),
            );
            return;
        };

        let counts = self.digest_days.iter().find(|d| d.day == day).copied();
        self.front_page_head(ui, day, counts);
        self.digest_actions(ui, day, counts);

        if let Some(err) = &self.digest_error {
            ui.add_space(6.0);
            ui.colored_label(ERROR_FG, err);
        }

        ui.add_space(10.0);
        match self.digest.clone() {
            Some(digest) if digest.day_utc == day => self.digest_sections(ui, &digest),
            _ if self.digest_generating == Some(day) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(
                            "the overnight desk is writing this day's edition… \
                             (one API call, up to a minute)",
                        )
                        .italics()
                        .color(TEXT_DIM),
                    );
                });
            }
            _ if self.digest_loading() => {
                ui.label(RichText::new("loading…").color(TEXT_DIM));
            }
            _ => {
                ui.label(
                    RichText::new("No edition has been written for this day yet.")
                        .italics()
                        .color(TEXT_DIM),
                );
            }
        }
    }

    /// The paper's nameplate and standing line — always on screen, with or
    /// without a day selected, so the page reads as a paper even before it
    /// has an edition in it.
    fn masthead(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.label(
                RichText::new("DAILY EVENTS")
                    .font(FontId::proportional(34.0))
                    .strong()
                    .color(INK),
            );
            ui.label(
                RichText::new("the overnight wire of Live Earth Signals · one edition per UTC day")
                    .small()
                    .italics()
                    .color(TEXT_DIM),
            );
        });
        // The classic nameplate rule: one heavy line, one hairline beneath.
        ui.add_space(6.0);
        rule(ui, 2.5);
        ui.add_space(2.0);
        rule(ui, 0.75);
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "A model-written account of one day of stored records. It reads the same \
                 database the map does; it adds no facts of its own, and it is not a news \
                 report.",
            )
            .small()
            .color(TEXT_DIM),
        );
        ui.add_space(4.0);
    }

    /// Dateline, headline figures, and the byline — the block a newspaper
    /// puts between the nameplate and the first column.
    fn front_page_head(&self, ui: &mut egui::Ui, day: DayKey, counts: Option<DigestDay>) {
        ui.add_space(4.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(day.0.format("%A, %B %-d, %Y").to_string())
                    .font(FontId::proportional(22.0))
                    .strong()
                    .color(INK),
            );
            ui.label(
                RichText::new(format!("{} · UTC edition", day.key()))
                    .small()
                    .color(TEXT_DIM),
            );
        });

        if let Some(c) = counts {
            ui.add_space(8.0);
            // The day's figures, set as a stat band — a centred row with a
            // rule above and below, so it reads as furniture, not prose.
            rule(ui, 0.75);
            ui.add_space(6.0);
            let att = format_count(c.attention_records);
            let evt = format_count(c.event_records);
            let tot = format_count(c.attention_records + c.event_records);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    stat(ui, "media-attention records", &att, HEADING_ATTENTION);
                    ui.add_space(20.0);
                    stat(ui, "event records", &evt, HEADING_EVENTS);
                    ui.add_space(20.0);
                    stat(ui, "total on file", &tot, INK);
                });
            });
            ui.add_space(6.0);
            rule(ui, 0.75);
        }
        ui.add_space(4.0);
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
                    RichText::new("Nothing stored for this day — nothing to write from.")
                        .color(TEXT_DIM),
                );
                return;
            }
            let label = if have {
                "regenerate edition"
            } else {
                "write today's edition"
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
                    RichText::new("cached — reopening this edition costs nothing")
                        .small()
                        .color(TEXT_DIM),
                );
            }
        });
    }

    /// The edition itself: byline, then the two sections, each headed, each
    /// carrying the record count it was written from, each set in its own
    /// laid-in card with a drop cap.
    fn digest_sections(&self, ui: &mut egui::Ui, digest: &DayDigest) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "by the overnight desk — {} · filed {}",
                digest.model,
                chrono::DateTime::from_timestamp(digest.generated_at_epoch_s, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "at an unknown time".into()),
            ))
            .small()
            .italics()
            .color(TEXT_DIM),
        );
        ui.add_space(12.0);

        section(
            ui,
            "Media attention",
            HEADING_ATTENTION,
            digest.attention_records,
            "how much coverage a place drew — a biased proxy for what happened, \
             not a record of it. Counted where the outlet is published, not \
             where the story happened.",
            &digest.media_attention,
        );
        ui.add_space(16.0);
        section(
            ui,
            "Event data",
            HEADING_EVENTS,
            digest.event_records,
            "reported occurrences from the event sources, independent of how \
             much coverage they drew. Includes official alerts, which are \
             warnings issued by an agency rather than observed incidents.",
            &digest.event_data,
        );

        ui.add_space(18.0);
        rule(ui, 0.75);
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "These two are counted and written separately and are never combined into \
                 one figure. A place can be loud in one and quiet in the other; that gap is \
                 the point, not an error.",
            )
            .small()
            .italics()
            .color(TEXT_DIM),
        );
    }
}

/// One heading figure in the stat band: the number large in its section
/// colour, the measure small and dim beneath it.
fn stat(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(value)
                .font(FontId::proportional(20.0))
                .strong()
                .color(color),
        );
        ui.label(RichText::new(label).small().color(TEXT_DIM));
    });
}

/// One headed section, set as a card on the paper. Free function rather than
/// a method: it holds no app state, and keeping it that way makes it
/// structurally impossible for one section to render anything belonging to
/// the other.
fn section(
    ui: &mut egui::Ui,
    heading: &str,
    color: Color32,
    records: u64,
    caveat: &str,
    body: &str,
) {
    Frame::new()
        .fill(SECTION_BG)
        .stroke(Stroke::new(1.0, RULE))
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            // A section opens like a department head: short colour rule,
            // then the name, then the standfirst saying what the count means.
            accent_rule(ui, color);
            ui.add_space(4.0);
            ui.label(
                RichText::new(heading)
                    .font(FontId::proportional(17.0))
                    .strong()
                    .color(color),
            );
            ui.label(
                RichText::new(format!("from {} records — {caveat}", format_count(records)))
                    .small()
                    .italics()
                    .color(TEXT_DIM),
            );
            ui.add_space(6.0);
            if body.trim().is_empty() {
                ui.label(
                    RichText::new("(the desk returned nothing for this section)")
                        .italics()
                        .color(TEXT_DIM),
                );
            } else {
                column_body(ui, body);
            }
        });
}

/// The section's prose, in the reading face of the page. Body text is set a
/// step larger and a shade brighter than the app default so the column, not
/// the chrome, is what the eye rests on.
fn column_body(ui: &mut egui::Ui, body: &str) {
    let paragraphs = body_paragraphs(body);
    let mut paragraphs = paragraphs.iter().map(String::as_str);
    let body_font = TextStyle::Body.resolve(ui.style());
    let line_h = ui
        .painter()
        .layout_no_wrap("Mg".to_owned(), body_font.clone(), INK)
        .size()
        .y;

    if let Some(first) = paragraphs.next() {
        // The drop cap: the opening glyph set large in a fixed-width gutter
        // at the head of the column, the rest of the paragraph in a bounded
        // child Ui beside it. A fixed gutter + child column wraps correctly
        // for a paragraph of any length — a measured space beside the glyph
        // does not, and clips a long paragraph to one line.
        let mut chars = first.chars();
        match chars.next() {
            Some(cap) if cap.is_alphabetic() => {
                let rest: String = chars.collect();
                let cap_text = cap.to_uppercase().collect::<String>();
                let cap_font = FontId::proportional(line_h * 2.8);
                let gutter = line_h * 2.0;

                let full = ui.available_width();
                let (id, rect) = ui.allocate_space(Vec2::new(full, 1.0));
                // The gutter glyph.
                ui.painter()
                    .text(rect.min, Align2::LEFT_TOP, cap_text, cap_font, DROP_CAP);
                // The rest of the paragraph, its own column.
                let text_rect = egui::Rect::from_min_size(
                    rect.min + Vec2::new(gutter, 0.0),
                    Vec2::new((full - gutter).max(1.0), f32::INFINITY),
                );
                let mut col = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(text_rect));
                col.label(RichText::new(rest).color(INK));
                let used = col.min_rect().height().max(line_h * 2.8);
                ui.allocate_space(Vec2::new(full, used));
            }
            _ => {
                ui.label(RichText::new(first).color(INK));
            }
        }
    }

    for paragraph in paragraphs {
        ui.add_space(6.0);
        ui.label(RichText::new(paragraph).color(INK));
    }
}

/// Break the section prose into display paragraphs. The model is asked for
/// blank-line breaks; when it returns one long block anyway, fall back to
/// grouping sentences so a 15-sentence column does not render as a single
/// wall of text.
fn body_paragraphs(body: &str) -> Vec<String> {
    let blocks: Vec<&str> = body
        .split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect();
    let mut out = Vec::new();
    for block in blocks {
        if block.contains('\n') {
            out.push(block.to_owned());
            continue;
        }
        let sentences = split_sentences(block);
        if sentences.len() <= 3 {
            out.push(block.to_owned());
        } else {
            for group in sentences.chunks(3) {
                out.push(group.concat());
            }
        }
    }
    out
}

/// Split a block into sentences, each keeping its trailing space, so the
/// groups rejoin into natural paragraphs. Splits on `. `, `! `, `? ` — the
/// model's prose is plain declarative text with no abbreviations worth
/// special-casing.
fn split_sentences(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = block.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek() == Some(&' ') {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// A full-width hairline across the column — one rect, once, per frame.
fn rule(ui: &mut egui::Ui, thickness: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, thickness), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, RULE);
}

/// The short coloured bar a section opens with — the print department head.
fn accent_rule(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(44.0, 3.0), Sense::hover());
    ui.painter().rect_filled(rect, 1.5, color);
}

/// Thousands-separated, so the stat band reads like print figures.
fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
