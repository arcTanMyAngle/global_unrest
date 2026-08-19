//! The Media page: ask for one place, get the video published about it, play
//! it here.
//!
//! Nothing on this page runs on a timer. A search happens when a person types
//! a place and presses the button, the results live in `App` until the next
//! search replaces them, and none of it is written to the database — see
//! `media_search`'s module docs and docs/SAFETY_AND_PRIVACY.md's "On-demand
//! media lookup" section for why that shape was chosen over storing links.
//!
//! **Layout is constrained by the player.** The embedded webview is a native
//! child window that paints over everything egui draws in its rectangle
//! (`crate::video`, "Airspace"), so every control lives in the left panel or
//! above the player rect — never on top of it — and the player is hidden
//! outright whenever an egui window is open, since egui would draw that window
//! *underneath* it.

use chrono::{Duration, Utc};
use egui::{Color32, RichText};
use media_search::MediaQuery;

use crate::app::App;
use crate::video::PlaybackRequest;

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);
const ERROR_FG: Color32 = Color32::from_rgb(255, 120, 120);
/// Published articles and public posts get the same two colours the digest
/// page uses for attention vs. events — same distinction, same visual cue.
const NEWS_FG: Color32 = Color32::from_rgb(150, 190, 255);
const SOCIAL_FG: Color32 = Color32::from_rgb(255, 176, 120);

/// The windows the picker offers, as (label, hours).
///
/// Deliberately coarse and bounded: every option is a window a public search
/// API will answer in one call, and the longest is short enough that a query
/// stays a look at one place rather than a harvest of its history.
pub const WINDOWS: [(&str, i64); 4] = [
    ("last 24 hours", 24),
    ("last 3 days", 72),
    ("last 7 days", 168),
    ("last 30 days", 720),
];

/// Per-provider result cap for one search.
pub const RESULT_LIMIT: usize = 25;

/// How many "busiest place" shortcuts to offer.
const BUSIEST_PLACES: usize = 6;

/// Below this the embed is not worth showing, so the player rect never
/// collapses to a sliver on a short window.
const PLAYER_MIN_HEIGHT: f32 = 200.0;

pub fn window_label(hours: i64) -> &'static str {
    WINDOWS
        .iter()
        .find(|(_, h)| *h == hours)
        .map(|(label, _)| *label)
        .unwrap_or("custom window")
}

impl App {
    pub fn media_page(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        // Panel order: side first, central last (egui 0.35).
        egui::Panel::left("media_search")
            .resizable(true)
            .default_size(360.0)
            .show(ui, |ui| {
                self.media_search_panel(ui);
            });
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.media_player_panel(ui, frame);
        });
    }

    fn media_search_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Find footage");
        ui.label(
            RichText::new(
                "Nothing is fetched until you ask. Pick a place and a time window; \
                 the search pulls public video links for that place only, shows them \
                 here, and saves none of them.",
            )
            .color(TEXT_DIM)
            .small(),
        );
        ui.add_space(6.0);

        if !self.media_handle.available() {
            ui.colored_label(
                ERROR_FG,
                RichText::new(crate::media::unavailable_reason()).small(),
            );
            ui.add_space(6.0);
        }

        // Where the most action happened, straight from the map's own top
        // movers — the same numbers the Map page ranks, offered as a shortcut
        // so "which place should I look at" has an answer before you type.
        let busiest = self.busiest_places();
        if !busiest.is_empty() {
            ui.label(
                RichText::new("busiest places in the current time window")
                    .color(TEXT_DIM)
                    .small(),
            );
            ui.horizontal_wrapped(|ui| {
                for name in busiest {
                    if ui.small_button(&name).clicked() {
                        self.media_place = name;
                    }
                }
            });
            ui.add_space(6.0);
        }

        let mut submit = false;
        ui.label(RichText::new("place").color(TEXT_DIM).small());
        let place = ui.add(
            egui::TextEdit::singleline(&mut self.media_place)
                .hint_text("Colombia, Port-au-Prince, …")
                .desired_width(f32::INFINITY),
        );
        submit |= place.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        ui.label(RichText::new("topic (optional)").color(TEXT_DIM).small());
        let topic = ui.add(
            egui::TextEdit::singleline(&mut self.media_topic)
                .hint_text("earthquake, protest, flood")
                .desired_width(f32::INFINITY),
        );
        submit |= topic.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("media_window")
                .selected_text(window_label(self.media_window_hours))
                .show_ui(ui, |ui| {
                    for (label, hours) in WINDOWS {
                        ui.selectable_value(&mut self.media_window_hours, hours, label);
                    }
                });
            let ready = self.media_handle.available() && !self.media.searching;
            if ui.add_enabled(ready, egui::Button::new("search")).clicked() {
                submit = true;
            }
            if self.media.searching {
                ui.spinner();
            }
        });
        if submit && self.media_handle.available() && !self.media.searching {
            self.start_media_search();
        }

        ui.add_space(6.0);
        if let Some(status) = &self.media.status {
            ui.label(RichText::new(status).color(TEXT_DIM).small());
        }
        // One provider failing is not "nothing happened here", so a rate-limited
        // API is named rather than folded into an empty result list.
        for problem in &self.media.problems {
            ui.colored_label(ERROR_FG, RichText::new(problem).small());
        }
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.media_results_list(ui);
            });
    }

    fn media_results_list(&mut self, ui: &mut egui::Ui) {
        if self.media.hits.is_empty() {
            if !self.media.searching {
                ui.label(
                    RichText::new("No results on screen yet.")
                        .color(TEXT_DIM)
                        .small(),
                );
            }
            return;
        }
        // Articles and public posts are listed separately and never
        // interleaved: a wire story and an anonymous post are not the same
        // claim, the same reason the map keeps attention and event data apart.
        self.media_section(ui, "news video", NEWS_FG, false);
        self.media_section(ui, "public posts", SOCIAL_FG, true);
    }

    fn media_section(&mut self, ui: &mut egui::Ui, heading: &str, color: Color32, social: bool) {
        let indices: Vec<usize> = self
            .media
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.provider.is_social() == social)
            .map(|(i, _)| i)
            .collect();
        if indices.is_empty() {
            return;
        }
        ui.add_space(4.0);
        ui.label(RichText::new(heading).color(color).strong());
        if social {
            ui.label(
                RichText::new("Unverified public posts — a lead, not a source.")
                    .color(TEXT_DIM)
                    .small(),
            );
        }
        for i in indices {
            // Cloned rather than borrowed: selecting mutates the session,
            // and late results renumber the list under it anyway - which is
            // why the selection is held by URL rather than by this index.
            let hit = self.media.hits[i].clone();
            let selected = self.media.is_selected(&hit);
            let label = format!("{}  {}", hit.ts_utc.format("%m-%d %H:%M"), hit.title);
            let hover = format!("{} · {}\n{}", hit.provider.label(), hit.origin, hit.url);
            if ui
                .selectable_label(selected, RichText::new(label).small())
                .on_hover_text(hover)
                .clicked()
            {
                self.media.select(&hit);
            }
        }
    }

    fn media_player_panel(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        // An egui window would be painted *under* the child webview, so the
        // player stands down entirely while one is open rather than covering it.
        if self.show_log_window || self.show_how_to_read {
            self.media_player.hide();
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("The player is paused while another window is open.")
                        .color(TEXT_DIM),
                );
            });
            return;
        }

        let Some(hit) = self.media.selected_hit().cloned() else {
            self.media_player.hide();
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(
                        "Search a place on the left, then pick a result to play it here.",
                    )
                    .color(TEXT_DIM),
                );
            });
            return;
        };

        ui.label(RichText::new(&hit.title).strong());
        ui.horizontal_wrapped(|ui| {
            let provider_fg = if hit.provider.is_social() {
                SOCIAL_FG
            } else {
                NEWS_FG
            };
            ui.label(
                RichText::new(hit.provider.label())
                    .color(provider_fg)
                    .small(),
            );
            ui.label(
                RichText::new(format!(
                    "· {} · {} UTC",
                    hit.origin,
                    hit.ts_utc.format("%Y-%m-%d %H:%M")
                ))
                .color(TEXT_DIM)
                .small(),
            );
            // Always reachable, not just on failure: some embeds refuse to play
            // in a webview at all, and the original link is the honest escape.
            ui.hyperlink_to(RichText::new("open in browser ↗").small(), &hit.url);
        });
        ui.add_space(6.0);

        let request = PlaybackRequest::new(&hit.url);
        let height = ui.available_height().max(PLAYER_MIN_HEIGHT);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        // Painted before the webview is positioned: on the first frame of a new
        // clip the child window has not composited yet, and black is a better
        // hole than whatever was underneath.
        ui.painter().rect_filled(rect, 4.0, Color32::BLACK);

        let failure = if request.is_embeddable() {
            self.media_player
                .show(frame, rect, ui.ctx().pixels_per_point(), &request)
                .err()
        } else {
            Some("No player can be embedded for this link — use “open in browser”.".to_string())
        };
        if let Some(failure) = failure {
            // Nothing is showing, so the child window must not sit over the
            // message explaining why.
            self.media_player.hide();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                failure,
                egui::FontId::proportional(13.0),
                ERROR_FG,
            );
        }
    }

    /// Country names behind the map's current top movers, newest ranking first
    /// and de-duplicated (several H3 cells routinely land in one country).
    fn busiest_places(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (mover, _) in &self.top_movers {
            let Some(name) = geo_utils::cell_center_lonlat(mover.h3_cell)
                .ok()
                .and_then(|(lon, lat)| self.countries.country_at(lon, lat))
                .map(|country| country.name.to_string())
            else {
                continue;
            };
            if !out.contains(&name) {
                out.push(name);
            }
            if out.len() == BUSIEST_PLACES {
                break;
            }
        }
        out
    }

    /// Hand one query to the media worker. The only thing on this page that
    /// touches the network, and it only ever runs from a click or Enter.
    pub fn start_media_search(&mut self) {
        let end = Utc::now();
        let query = MediaQuery {
            place: self.media_place.clone(),
            topic: self.media_topic.clone(),
            start: end - Duration::hours(self.media_window_hours),
            end,
            limit: RESULT_LIMIT,
        };
        if !query.is_valid() {
            self.media.reject("Type a place to search for.");
            return;
        }
        self.media_player.hide();
        // The generation comes back from the dispatch itself, so the page
        // knows which search it is showing before the first provider answers;
        // anything stamped with an older one is discarded on arrival.
        let generation = self.media_handle.search(query);
        self.media.begin(generation, &self.media_place);
        self.media.status = Some(format!(
            "searching {} · {}…",
            self.media_place.trim(),
            window_label(self.media_window_hours)
        ));
    }
}
