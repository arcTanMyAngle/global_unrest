//! Panels: top filter bar, bottom timeline, right inspector, central map,
//! and the ingest-log window.

use chrono::DateTime;
use core_types::{AttributionSubject, attribution_for};
use core_types::{EventKind, SignalFamily};
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Vec2};

use crate::app::{App, HeatMetric, LEDGER_PAGE_SIZE, Page, Phase, WindowLen};

const TEXT_DIM: Color32 = Color32::from_rgb(148, 155, 168);

/// Above this share of coarse-precision (country/admin1) records, a cell's
/// detail gets a low-confidence badge.
const COARSE_SHARE_BADGE: f32 = 0.5;

const BADGE_BG: Color32 = Color32::from_rgb(72, 52, 20);
const BADGE_FG: Color32 = Color32::from_rgb(255, 196, 110);

fn fmt_ts(epoch_s: i64) -> String {
    DateTime::from_timestamp(epoch_s, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| format!("t={epoch_s}"))
}

fn web_url(raw: &str) -> Option<url::Url> {
    let parsed = url::Url::parse(raw).ok()?;
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
        Some(parsed)
    } else {
        None
    }
}

fn link_host(raw: &str) -> String {
    web_url(raw)
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| "source".into())
}

fn youtube_search_url(query: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    format!("https://www.youtube.com/results?search_query={encoded}")
}

/// Compact relative time vs. `now` (both epoch seconds): "12s ago", "in 14m".
fn fmt_relative(epoch_s: i64, now: i64) -> String {
    let d = epoch_s - now;
    let mag = d.unsigned_abs();
    let unit = if mag < 60 {
        format!("{mag}s")
    } else if mag < 3600 {
        format!("{}m", mag / 60)
    } else if mag < 86_400 {
        format!("{}h", mag / 3600)
    } else {
        format!("{}d", mag / 86_400)
    };
    if d < 0 {
        format!("{unit} ago")
    } else {
        format!("in {unit}")
    }
}

/// Small amber low-confidence badge.
fn badge(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(format!(" {text} "))
            .small()
            .color(BADGE_FG)
            .background_color(BADGE_BG),
    );
}

/// One labeled score bar. All score components are in [0, 1].
fn score_bar(ui: &mut egui::Ui, label: &str, value: f32, text: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [86.0, 14.0],
            egui::Label::new(RichText::new(label).small().color(TEXT_DIM)),
        );
        ui.add(
            egui::ProgressBar::new(value.clamp(0.0, 1.0))
                .desired_height(13.0)
                .text(RichText::new(text).small()),
        );
    });
}

impl App {
    pub fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("topbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Live Earth Signals").strong());

                // Page switcher. Live-source controls stay on both pages
                // (ingest runs regardless of what is on screen); everything
                // below the separator is map-specific and hidden on the
                // Daily Events page.
                let mut page = self.page;
                ui.selectable_value(&mut page, Page::Map, "map");
                ui.selectable_value(&mut page, Page::DailyEvents, "daily events")
                    .on_hover_text(
                        "A model-written summary of one day of stored records, with media \
                         attention and event data kept in separate sections.",
                    );
                ui.selectable_value(&mut page, Page::Media, "media")
                    .on_hover_text(
                        "Look up public video for one place and time window, and play it \
                         in the app. Fetched only when you ask; nothing is stored.",
                    );
                self.set_page(page);
                ui.separator();

                // Pause/resume network polling. Cached rows are always real;
                // the desktop runtime never loads synthetic fixtures.
                let mut online = self.online;
                if ui
                    .checkbox(&mut online, "live updates")
                    .on_hover_text(
                        "Fetch GDELT, ACLED, NOAA, and IODA. Turning this off pauses \
                         network requests but keeps cached real data visible.",
                    )
                    .changed()
                {
                    self.set_online(online);
                }
                if self.online
                    && ui
                        .button("↻")
                        .on_hover_text("fetch the latest live data now")
                        .clicked()
                {
                    self.fetch_now();
                }
                self.source_status_label(ui);
                // Placed before the page early-return below: source state and
                // attributions are properties of the application, not of the
                // map, and must be reachable from Daily Events and Media too.
                if ui
                    .button("settings")
                    .on_hover_text("per-source state: compiled in, configured, cadence")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
                if ui
                    .button("about")
                    .on_hover_text("attributions, licence, version")
                    .clicked()
                {
                    self.show_about = !self.show_about;
                }
                if matches!(self.page, Page::DailyEvents | Page::Media) {
                    return;
                }
                ui.separator();

                let mut changed = false;
                changed |= ui
                    .checkbox(&mut self.filters.show_heatmap, "heatmap")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.show_markers, "markers")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.show_spike_halos, "spike halos")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.show_alerts, "NOAA alerts")
                    .on_hover_text(
                        "Active NOAA/NWS weather alerts as severity-tinted cells with a \
                         dashed outline. Weather, not unrest — a separate layer so the \
                         two never blend together. US coverage only.",
                    )
                    .changed();
                ui.menu_button("orientation", |ui| {
                    ui.label(
                        RichText::new("Offline basemap aids — no online tiles.")
                            .color(TEXT_DIM)
                            .small(),
                    );
                    changed |= ui
                        .checkbox(&mut self.filters.show_graticule, "graticule")
                        .on_hover_text("Meridians and parallels; spacing adapts to zoom.")
                        .changed();
                    changed |= ui
                        .checkbox(&mut self.filters.show_labels, "country labels")
                        .on_hover_text(
                            "Country names at their centroids. Labels that would \
                             collide are dropped largest-country-first, so a label \
                             missing here means it did not fit, not that there is \
                             no data.",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(&mut self.filters.focus_selection, "dim outside selection")
                        .on_hover_text(
                            "Wash the map outside the selected cell. Off by default \
                             — dimming hides real data.",
                        )
                        .changed();
                });
                ui.separator();

                ui.label(RichText::new("heat:").color(TEXT_DIM));
                changed |= ui
                    .selectable_value(
                        &mut self.filters.heat_metric,
                        HeatMetric::Attention,
                        "media attention",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(&mut self.filters.heat_metric, HeatMetric::Events, "events")
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut self.filters.heat_metric,
                        HeatMetric::Diversity,
                        "source diversity",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut self.filters.heat_metric,
                        HeatMetric::Divergence,
                        "attention ↔ unrest",
                    )
                    .on_hover_text(
                        "Where media attention outruns event data, and where events \
                         outrun attention. Ranks within this window, not raw scores. \
                         See the legend for how to read it.",
                    )
                    .changed();
                ui.separator();

                ui.label(RichText::new("markers:").color(TEXT_DIM));
                changed |= ui.checkbox(&mut self.filters.protest, "protest").changed();
                changed |= ui
                    .checkbox(&mut self.filters.conflict, "conflict")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.disruption, "disruption")
                    .changed();
                changed |= ui.checkbox(&mut self.filters.other, "other").changed();
                changed |= ui
                    .checkbox(&mut self.filters.attention_markers, "attention")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.chatter_markers, "chatter")
                    .on_hover_text(
                        "Aggregate social rollups: how many posts mentioned a place,                          with no author, text, or post behind them. Volume only -                          never coverage, never a report that something happened.",
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.video_only, "🎥 has video")
                    .on_hover_text("Only show markers whose record carries a classified video URL.")
                    .changed();
                ui.separator();

                let theme_label = if self.filters.themes.is_empty() {
                    "themes: all".to_string()
                } else {
                    format!("themes: {}", self.filters.themes.len())
                };
                ui.menu_button(theme_label, |ui| {
                    let Some(vocab) = &self.theme_vocab else {
                        ui.label(RichText::new("loading themes…").color(TEXT_DIM));
                        return;
                    };
                    if !self.filters.themes.is_empty() && ui.button("clear theme filter").clicked()
                    {
                        self.filters.themes.clear();
                        changed = true;
                    }
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (theme, count) in vocab {
                                let mut on = self.filters.themes.contains(theme);
                                if ui.checkbox(&mut on, format!("{theme} ({count})")).changed() {
                                    if on {
                                        self.filters.themes.push(theme.clone());
                                    } else {
                                        self.filters.themes.retain(|t| t != theme);
                                    }
                                    changed = true;
                                }
                            }
                        });
                });
                ui.separator();

                ui.label(RichText::new("min confidence").color(TEXT_DIM));
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.filters.min_confidence, 0.0..=1.0)
                            .fixed_decimals(2),
                    )
                    .changed();

                let retention_label = match self.retention_days {
                    Some(d) => format!("retention: {d}d"),
                    None => "retention: keep all".to_string(),
                };
                ui.menu_button(retention_label, |ui| {
                    ui.label(
                        RichText::new("Cap the events table (online volumes ~100k/day).")
                            .color(TEXT_DIM)
                            .small(),
                    );
                    let mut choice = self.retention_days;
                    let changed = ui
                        .selectable_value(&mut choice, None, "keep everything")
                        .clicked()
                        | ui.selectable_value(&mut choice, Some(30), "30 days")
                            .clicked()
                        | ui.selectable_value(&mut choice, Some(60), "60 days")
                            .clicked()
                        | ui.selectable_value(&mut choice, Some(90), "90 days")
                            .clicked();
                    ui.label(
                        RichText::new("≥ 30 days keeps the 28-day baselines fully warm.")
                            .color(TEXT_DIM)
                            .small(),
                    );
                    if changed {
                        self.set_retention(choice);
                        ui.close();
                    }
                });

                if ui.button("reset view").clicked() {
                    self.map.viewport = None;
                }
                // The `?` shortcut alone is not discoverable, and this window
                // is where the map's caveats live.
                if ui
                    .button("how to read this")
                    .on_hover_text("What the map shows, what it cannot show, and why (?)")
                    .clicked()
                {
                    self.show_how_to_read = true;
                }
                if ui
                    .button("export parquet")
                    .on_hover_text("write this session as date-partitioned Parquet")
                    .clicked()
                {
                    self.start_export();
                }
                if changed {
                    self.mark_dirty();
                }
            });
        });
    }

    /// Compact live-source status shown next to the online toggle: one dot
    /// per online source (hover for the cycle detail).
    fn source_status_label(&self, ui: &mut egui::Ui) {
        let mut any_online = false;
        for s in self.source_statuses.iter().filter(|s| s.online) {
            any_online = true;
            let color = if s.degraded || s.partial {
                Color32::from_rgb(255, 170, 90)
            } else {
                Color32::from_rgb(120, 210, 140)
            };
            crate::style::dot_swatch(ui, color).on_hover_text(&s.detail);
            ui.label(RichText::new(s.name).color(TEXT_DIM).small())
                .on_hover_text(&s.detail);
        }
        if !any_online {
            let label = if self.online {
                "connecting live sources…"
            } else {
                "live updates paused"
            };
            ui.label(RichText::new(label).color(TEXT_DIM).small());
        }
    }

    pub fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("timeline").show(ui, |ui| {
            let Some((extent_start, extent_end)) = self.extent else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("timeline — waiting for data").color(TEXT_DIM));
                });
                return;
            };
            let total = self.total_buckets();
            let len = self.timeline.len.buckets(total);
            let max_start = (total - len).max(0);
            self.timeline.start_bucket = self.timeline.start_bucket.clamp(0, max_start);

            ui.horizontal(|ui| {
                let icon = if self.timeline.playing { "⏸" } else { "▶" };
                if ui.button(icon).clicked() {
                    self.timeline.playing = !self.timeline.playing;
                    self.timeline.accum = 0.0;
                    if self.timeline.playing {
                        // Starting playback is explicit manual navigation —
                        // an ingest tick mid-playback must not yank the
                        // scrub position back to "now".
                        self.timeline.auto_follow = false;
                    }
                }

                let mut len_choice = self.timeline.len;
                egui::ComboBox::from_id_salt("window-len")
                    .selected_text(len_choice.label())
                    .show_ui(ui, |ui| {
                        for choice in WindowLen::CHOICES {
                            ui.selectable_value(&mut len_choice, choice, choice.label());
                        }
                    });
                if len_choice != self.timeline.len {
                    self.timeline.len = len_choice;
                    if self.timeline.auto_follow {
                        self.sync_window_to_now();
                    } else {
                        let len = self.timeline.len.buckets(total);
                        self.timeline.start_bucket =
                            self.timeline.start_bucket.min((total - len).max(0));
                    }
                    self.mark_dirty();
                }

                if !self.timeline.auto_follow
                    && ui
                        .button("⏵ now")
                        .on_hover_text("resume tracking the current moment")
                        .clicked()
                {
                    self.timeline.auto_follow = true;
                    self.sync_window_to_now();
                    self.mark_dirty();
                }

                if let Some((ws, we)) = self.current_window() {
                    ui.label(
                        RichText::new(format!("{}  →  {}", fmt_ts(ws), fmt_ts(we)))
                            .color(Color32::from_rgb(210, 214, 224))
                            .monospace(),
                    );
                }
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new("custom range (UTC):").color(TEXT_DIM).small());
                let start_edit = ui.add(
                    egui::TextEdit::singleline(&mut self.timeline.custom_start_input)
                        .hint_text("YYYY-MM-DD HH:MM")
                        .desired_width(130.0),
                );
                ui.label(RichText::new("→").color(TEXT_DIM));
                let end_edit = ui.add(
                    egui::TextEdit::singleline(&mut self.timeline.custom_end_input)
                        .hint_text("YYYY-MM-DD HH:MM")
                        .desired_width(130.0),
                );
                let applied_on_enter = (start_edit.lost_focus() || end_edit.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if start_edit.changed() || end_edit.changed() {
                    self.timeline.custom_range_error = None;
                }
                if ui.button("apply").clicked() || applied_on_enter {
                    self.apply_custom_range();
                }
                if let Some(err) = &self.timeline.custom_range_error {
                    ui.label(
                        RichText::new(err)
                            .color(Color32::from_rgb(255, 140, 120))
                            .small(),
                    );
                }
            });

            let strip_width = ui.available_width();
            let changed = crate::timeline_strip::show(
                ui,
                strip_width,
                &self.timeline_histogram,
                &self.map.style,
                &mut self.timeline,
                len,
                max_start,
            );
            if changed {
                // A drag/click scrub is explicit manual navigation, same as
                // starting playback or applying a typed custom range.
                self.timeline.auto_follow = false;
                self.mark_dirty();
            }

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "data: {} → {} · {} six-hour buckets",
                        fmt_ts(extent_start),
                        fmt_ts(extent_end),
                        total
                    ))
                    .color(TEXT_DIM)
                    .small(),
                );
            });
        });
    }

    pub fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.inspector_status(ui);
                    ui.separator();
                    self.top_movers_panel(ui);
                    ui.separator();
                    match self.selected_cell {
                        Some(cell) => self.inspector_selection(ui, cell),
                        None => {
                            ui.label(
                                RichText::new(
                                    "Click the map to inspect a region \
                                     (H3 cell, resolution 3).",
                                )
                                .color(TEXT_DIM),
                            );
                        }
                    }
                    ui.separator();
                    self.inspector_legend(ui);
                });
            });
    }

    fn inspector_status(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status");
        match &self.phase {
            Phase::Loading(msg) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(msg);
                });
            }
            Phase::Error(msg) => {
                ui.colored_label(Color32::from_rgb(255, 120, 120), msg);
            }
            Phase::Ready => {
                if let Some(r) = self.ingest_report {
                    let mut line = format!(
                        "Last ingest: {} inserted, {} duplicate",
                        r.inserted, r.duplicates
                    );
                    if r.pruned > 0 {
                        line.push_str(&format!(", {} pruned", r.pruned));
                    }
                    ui.label(line);
                } else if self.extent.is_none() {
                    ui.label("No live records stored yet.");
                }
                if let Some(days) = self.retention_days {
                    ui.label(
                        RichText::new(format!("retention: {days} days"))
                            .color(TEXT_DIM)
                            .small(),
                    );
                }
                if let Some((total, _)) = &self.ingest_log {
                    let label = format!("{total} records in ingest log");
                    if *total > 0 {
                        if ui.link(label).clicked() {
                            self.show_log_window = !self.show_log_window;
                        }
                    } else {
                        ui.label(RichText::new(label).color(TEXT_DIM));
                    }
                }
                ui.label(
                    RichText::new(format!("{} region-buckets in window", self.bucket_count))
                        .color(TEXT_DIM)
                        .small(),
                );
                if let Some(status) = &self.export_status {
                    ui.label(RichText::new(status).color(TEXT_DIM).small());
                }
                self.live_source_panel(ui);
            }
        }
    }

    /// Live-source indicators, one block per source: online/degraded state,
    /// last & next fetch, and per-source attribution. When a fetch fails the
    /// app keeps showing cached data and this panel makes the degraded state
    /// explicit (M3 acceptance).
    fn live_source_panel(&self, ui: &mut egui::Ui) {
        let now = chrono::Utc::now().timestamp();
        for s in &self.source_statuses {
            ui.add_space(6.0);
            ui.separator();
            ui.label(RichText::new(format!("Live source — {}", s.name)).strong());

            if !s.online {
                ui.label(RichText::new(&s.detail).color(TEXT_DIM).small());
                continue;
            }

            // A hollow ring for anything less than fully healthy, filled for
            // online: the shape carries the state even in a grayscale
            // screenshot, which a color-only dot did not.
            let (healthy, color, state) = if s.degraded {
                (
                    false,
                    Color32::from_rgb(255, 170, 90),
                    "degraded — showing cached real data",
                )
            } else if s.partial {
                (
                    false,
                    Color32::from_rgb(255, 170, 90),
                    "partial — one feed unavailable",
                )
            } else {
                (true, Color32::from_rgb(120, 210, 140), "online")
            };
            ui.horizontal(|ui| {
                if healthy {
                    crate::style::dot_swatch(ui, color);
                } else {
                    crate::style::ring_swatch(ui, color);
                }
                ui.label(RichText::new(state).color(color));
            });
            ui.label(RichText::new(&s.detail).color(TEXT_DIM).small());

            if let Some(t) = s.last_attempt_epoch_s {
                ui.label(
                    RichText::new(format!(
                        "last fetch: {} ({})",
                        fmt_ts(t),
                        fmt_relative(t, now)
                    ))
                    .color(TEXT_DIM)
                    .small(),
                );
            }
            if let Some(t) = s.last_success_epoch_s {
                ui.label(
                    RichText::new(format!("last success: {}", fmt_relative(t, now)))
                        .color(TEXT_DIM)
                        .small(),
                );
            }
            if let Some(t) = s.next_attempt_epoch_s {
                ui.label(
                    RichText::new(format!("next fetch: {}", fmt_relative(t, now)))
                        .color(TEXT_DIM)
                        .small(),
                );
            }
            // From `core_types::attribution`, not restated here: this panel
            // used to carry its own copy of four of these strings, which is
            // exactly how a mandated citation goes stale in one place while
            // looking correct in another.
            let attr = attribution_for(AttributionSubject::Source(s.source));
            // ACLED's mandated citation names the date the data was accessed;
            // this line sits beside that very fetch, so fill it from the same
            // status rather than printing the template's empty slot.
            let accessed = s
                .last_success_epoch_s
                .and_then(|e| DateTime::from_timestamp(e, 0))
                .map(|dt| dt.date_naive());
            ui.label(
                RichText::new(match attr.citation(accessed) {
                    Some(mandated) => format!("Data: {mandated}"),
                    None => format!("Data: {} — {}", attr.display_name, attr.licence_label),
                })
                .color(TEXT_DIM)
                .small(),
            );
        }
    }

    /// Human-readable name for a cell, from the bundled country polygons.
    /// Falls back to the cell id: over open ocean there is no country to name,
    /// and inventing one would be worse than showing the raw key.
    fn cell_label(&self, cell: u64) -> String {
        geo_utils::cell_center_lonlat(cell)
            .ok()
            .and_then(|(lon, lat)| self.countries.country_at(lon, lat))
            .map(|c| format!("{} ({})", c.name, c.iso_a3))
            .unwrap_or_else(|| format!("cell {cell:#x}"))
    }

    /// Ranked spike regions in the current window (docs/VISUALIZATION.md V2
    /// item 6). Sorted from the already-loaded buckets — this panel issues no
    /// query of its own. Clicking a row selects the cell and flies to it.
    fn top_movers_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Top movers");
        if self.top_movers.is_empty() {
            ui.label(
                RichText::new(
                    "No region in this window stands out against its own trailing \
                     baseline yet. Regions with under 7 days of history behind them \
                     are never ranked — there is nothing to compare them to.",
                )
                .color(TEXT_DIM)
                .small(),
            );
            return;
        }
        ui.label(
            RichText::new(
                "Strongest spike vs. each region's own 28-day baseline, within the \
                 visible time window. 0.50 = normal for that region.",
            )
            .color(TEXT_DIM)
            .small(),
        );
        ui.add_space(4.0);

        let mut clicked = None;
        for (m, series) in &self.top_movers {
            let selected = self.selected_cell == Some(m.h3_cell);
            let row = ui.selectable_label(
                selected,
                // No glyph prefix: egui's bundled fonts have no geometric
                // shapes, so a decorative one renders as a missing-glyph box
                // (the app's existing colored swatches survive that only
                // because a colored box still reads as a color chip).
                RichText::new(format!("{:.2}   {}", m.spike, self.cell_label(m.h3_cell))).strong(),
            );
            if row.clicked() {
                clicked = Some(m.h3_cell);
            }
            // Shape only: each row's bars are scaled to that row's own peak,
            // so heights are never comparable between rows.
            crate::sparkline::mini(ui, ui.available_width().min(200.0), series);
            // The evidence the score came from, in the units it was derived
            // from — a rank without its raw counts is not transparent.
            let d = m.delta();
            let delta_txt = if d >= 0.0 {
                format!("+{d:.0}")
            } else {
                format!("{d:.0}")
            };
            ui.label(
                RichText::new(format!(
                    "{delta_txt} records vs {:.1}/6 h baseline · {}",
                    m.baseline,
                    fmt_ts(m.bucket_start)
                ))
                .color(TEXT_DIM)
                .small(),
            );
            ui.add_space(3.0);
        }
        if let Some(cell) = clicked {
            self.select_and_fly(cell);
        }
    }

    fn inspector_selection(&mut self, ui: &mut egui::Ui, cell: u64) {
        ui.heading("Region");
        if let Some(label) = &self.selected_label {
            ui.label(RichText::new(label).strong());
        }
        ui.label(
            RichText::new(format!("H3 cell {cell:#x} · res 3"))
                .color(TEXT_DIM)
                .small(),
        );
        if ui.button("clear selection").clicked() {
            self.selected_cell = None;
            self.detail = None;
            return;
        }

        let Some(detail) = &self.detail else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("querying…");
            });
            return;
        };

        // One section per family, never a combined total (CLAUDE.md product
        // rule 1, docs/SIGNAL_MODEL.md). The counts below are in four
        // different units and are deliberately never added together.
        let family_total = |family: SignalFamily| -> u32 {
            detail
                .counts_by_kind
                .iter()
                .filter(|(k, _)| k.family() == family)
                .map(|(_, c)| *c)
                .sum()
        };

        ui.add_space(6.0);
        ui.label(RichText::new("Media attention").strong());
        ui.label(format!(
            "{} attention records · {} articles · {} distinct outlets",
            family_total(SignalFamily::MediaAttention),
            detail.total_articles,
            detail.distinct_outlets
        ));

        ui.add_space(6.0);
        ui.label(RichText::new("Event data").strong());
        let mut any_events = false;
        for (kind, count) in &detail.counts_by_kind {
            if kind.family() == SignalFamily::RecordedEvent {
                any_events = true;
                ui.horizontal(|ui| {
                    crate::style::dot_swatch(ui, self.map.style.marker_color(*kind));
                    ui.label(format!("{} × {}", count, kind.label()));
                });
            }
        }
        if !any_events {
            ui.label(RichText::new("none in window").color(TEXT_DIM));
        }

        // Official alerts are events, but they are not unrest — they are a
        // jurisdiction issuing a warning. Separate heading so a busy weather
        // day cannot read as a busy unrest day.
        let alerts = family_total(SignalFamily::OfficialAlert);
        if alerts > 0 {
            ui.add_space(6.0);
            ui.label(RichText::new("Official alerts").strong());
            ui.horizontal(|ui| {
                crate::style::dot_swatch(ui, self.map.style.marker_alert);
                ui.label(format!("{alerts} issued by an official source"));
            });
        }

        // Aggregate chatter: counts of posts, with no author, text, or post
        // id behind them, and no claim that anything happened here.
        let chatter_rollups = family_total(SignalFamily::Chatter);
        if chatter_rollups > 0 {
            ui.add_space(6.0);
            ui.label(RichText::new("Aggregate chatter").strong());
            ui.horizontal(|ui| {
                crate::style::dot_swatch(ui, self.map.style.marker_chatter);
                ui.label(format!(
                    "{} posts across {chatter_rollups} aggregate windows",
                    detail.chatter_posts
                ));
            });
            ui.label(
                RichText::new("volume only · not coverage, not a report of an event")
                    .color(TEXT_DIM)
                    .size(11.0),
            );
        }

        // Video is opt-in: show only URLs carried by real source metadata as
        // candidates, and never fetch or autoplay third-party content. A
        // manual external search is available even when no direct link was
        // attached, but is labeled unverified rather than silently joined to
        // this region's events.
        ui.add_space(6.0);
        ui.label(RichText::new("Related video and source media").strong());
        let mut video_links = Vec::new();
        let mut source_links = Vec::new();
        for row in &detail.source_links {
            for source_url in &row.urls {
                if web_url(source_url).is_none() {
                    continue;
                } else if core_types::is_video_url(source_url) {
                    video_links.push((row, source_url));
                } else {
                    source_links.push((row, source_url));
                }
            }
        }

        if video_links.is_empty() {
            ui.label(
                RichText::new("No direct video URL is attached to these records.").color(TEXT_DIM),
            );
        } else {
            ui.label(format!(
                "{} video candidate{} from source metadata",
                video_links.len(),
                if video_links.len() == 1 { "" } else { "s" }
            ));
            for (row, source_url) in video_links.into_iter().take(10) {
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to("▶ open video ↗", source_url);
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · {}",
                            link_host(source_url),
                            row.source,
                            fmt_ts(row.ts_epoch_s)
                        ))
                        .color(TEXT_DIM)
                        .small(),
                    );
                });
                if let Some(headline) = &row.headline {
                    ui.label(RichText::new(headline).small());
                }
            }
            ui.label(
                RichText::new(
                    "Candidate means the upstream record supplied the URL; review the footage and provenance before relying on it.",
                )
                .color(TEXT_DIM)
                .small(),
            );
        }

        let area = self.selected_label.as_deref().unwrap_or("selected area");
        let context = detail
            .source_links
            .iter()
            .find_map(|row| row.headline.as_deref())
            .or_else(|| detail.headlines.first().map(|row| row.headline.as_str()))
            .unwrap_or("incident event");
        let search_terms: String = format!("{area} {context} video")
            .chars()
            .take(220)
            .collect();
        ui.hyperlink_to(
            "search YouTube for related video ↗",
            youtube_search_url(&search_terms),
        );
        ui.label(
            RichText::new(
                "External search opens only when clicked. Its results are unverified and may show a different place, time, or event.",
            )
            .color(TEXT_DIM)
            .small(),
        );

        if !source_links.is_empty() {
            ui.collapsing(
                format!(
                    "source pages that may contain media ({})",
                    source_links.len()
                ),
                |ui| {
                    for (row, source_url) in source_links.into_iter().take(10) {
                        ui.horizontal_wrapped(|ui| {
                            ui.hyperlink_to(
                                format!("open {} ↗", link_host(source_url)),
                                source_url,
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} · {} · {}",
                                    row.source,
                                    row.kind.label(),
                                    fmt_ts(row.ts_epoch_s)
                                ))
                                .color(TEXT_DIM)
                                .small(),
                            );
                        });
                    }
                },
            );
        }

        // Score components — always all four, never only the combined number
        // (hard project rule; docs/SCORING.md).
        ui.add_space(6.0);
        ui.label(RichText::new("Signal components").strong());
        match &detail.scores {
            Some(s) => {
                if s.spike_cold_start {
                    badge(ui, "low confidence: baseline cold start (<7 days history)");
                }
                if detail.coarse_share > COARSE_SHARE_BADGE {
                    badge(
                        ui,
                        &format!(
                            "low confidence: {:.0}% coarse geocoding",
                            detail.coarse_share * 100.0
                        ),
                    );
                }
                score_bar(ui, "attention", s.attention, format!("{:.2}", s.attention));
                score_bar(ui, "unrest", s.unrest, format!("{:.2}", s.unrest));
                let spike_text = match detail.baseline_hint {
                    Some(b) => format!("{:.2} · baseline {b:.1}/6h · 0.50 = normal", s.spike),
                    None => format!("{:.2} · 0.50 = normal", s.spike),
                };
                score_bar(ui, "spike", s.spike, spike_text);
                score_bar(
                    ui,
                    "combined",
                    s.combined,
                    format!("{:.2} = 0.40·att + 0.45·unr + 0.15·spk", s.combined),
                );
                ui.label(
                    RichText::new(
                        "Composed from stored 6 h bucket scores, weighted by \
                         recency within the window (24 h half-life).",
                    )
                    .color(TEXT_DIM)
                    .small(),
                );
            }
            None => {
                ui.label(RichText::new("no bucket data in window").color(TEXT_DIM));
            }
        }

        self.region_sparkline(ui);

        ui.add_space(6.0);
        ui.label(RichText::new("Location confidence (mean)").strong());
        if detail.counts_by_kind.is_empty() {
            ui.label(RichText::new("N/A — no records in window").color(TEXT_DIM));
        } else {
            ui.add(
                egui::ProgressBar::new(detail.mean_confidence)
                    .text(format!("{:.0}%", f64::from(detail.mean_confidence) * 100.0)),
            );
        }

        if !detail.top_themes.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Top themes").strong());
            ui.horizontal_wrapped(|ui| {
                for (theme, count) in &detail.top_themes {
                    ui.label(
                        RichText::new(format!("{theme} ({count})"))
                            .background_color(Color32::from_rgb(36, 42, 54))
                            .color(Color32::from_rgb(200, 206, 218)),
                    );
                }
            });
        }

        if !detail.headlines.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Headlines (metadata only)").strong());
            for row in &detail.headlines {
                let color = self.map.style.marker_color(row.kind);
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(color, "▪");
                    ui.label(
                        RichText::new(fmt_ts(row.ts_epoch_s))
                            .color(TEXT_DIM)
                            .small(),
                    );
                });
                ui.label(&row.headline);
                if !row.outlet_domains.is_empty() {
                    ui.label(
                        RichText::new(row.outlet_domains.join(", "))
                            .color(TEXT_DIM)
                            .small(),
                    );
                }
                ui.add_space(4.0);
            }
        }

        // Last: the ledger is the only unbounded-length section, so putting it
        // anywhere earlier would bury everything under it. The `detail` borrow
        // above is dead by here, which is what lets the page jump apply.
        if let Some(offset) = self.event_ledger(ui) {
            self.set_ledger_offset(offset);
        }
    }

    /// 28-day record history for the selected region with its own trailing
    /// median underneath (docs/VISUALIZATION.md V2 item 7a) — the spike
    /// component, made visible.
    fn region_sparkline(&self, ui: &mut egui::Ui) {
        let Some(span) = self.history_span else {
            return;
        };
        ui.add_space(6.0);
        ui.label(RichText::new("Region history (28 days)").strong());
        crate::sparkline::show(
            ui,
            ui.available_width(),
            &self.region_history,
            span,
            &self.map.style,
        );
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(self.map.style.marker_attention, "▪");
            ui.label(RichText::new("attention").color(TEXT_DIM).small());
            ui.colored_label(crate::sparkline::EVENTS_FILL, "▪");
            ui.label(RichText::new("events").color(TEXT_DIM).small());
            ui.colored_label(crate::sparkline::BAND_LINE, "▬");
            ui.label(RichText::new("trailing median").color(TEXT_DIM).small());
        });
        ui.label(
            RichText::new(
                "Bar height is records per 6 h — the same quantity the baseline is \
                 a median of and the spike score is computed from — split so the \
                 attention and event shares stay distinct. Buckets with too little \
                 history behind them show a tick instead of a band: no baseline, \
                 no anomaly claim.",
            )
            .color(TEXT_DIM)
            .small(),
        );
    }

    /// Paginated ledger of the region's **discrete events** in the window
    /// (docs/VISUALIZATION.md V2 item 7b). Returns a requested page offset.
    ///
    /// Attention rows cannot appear here — `storage::region_events` excludes
    /// them in SQL, keeping the attention/event separation off the UI's
    /// good behaviour.
    fn event_ledger(&self, ui: &mut egui::Ui) -> Option<usize> {
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new("Event ledger").strong());
        let Some(page) = &self.ledger else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("querying…").color(TEXT_DIM).small());
            });
            return None;
        };
        if page.total == 0 {
            ui.label(
                RichText::new(
                    "No discrete event records for this region in the current window. \
                     Media attention is listed separately above — it is never an event.",
                )
                .color(TEXT_DIM)
                .small(),
            );
            return None;
        }

        let first = page.offset + 1;
        let last = page.offset + page.rows.len();
        ui.label(
            RichText::new(format!("{first}–{last} of {} · newest first", page.total))
                .color(TEXT_DIM)
                .small(),
        );

        for row in &page.rows {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // The map's own encoding, reused: kind color, source shape.
                crate::style::glyph_swatch(
                    ui,
                    renderer::MarkerGlyph::for_source(row.source),
                    self.map.style.marker_color(row.kind),
                );
                ui.label(RichText::new(row.kind.label()).small().strong());
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        fmt_ts(row.ts_epoch_s),
                        row.source.as_str(),
                        row.precision.label()
                    ))
                    .color(TEXT_DIM)
                    .small(),
                );
            });
            // ACLED's label lands here; its `notes` narrative is never fetched
            // or stored, so there is nothing else this could show.
            if let Some(headline) = &row.headline {
                ui.label(RichText::new(headline).small());
            }
            let mut facts = Vec::new();
            if let Some(sev) = row.severity {
                facts.push(format!("severity {sev:.2}"));
            }
            facts.push(format!("confidence {:.0}%", row.confidence * 100.0));
            if !row.outlet_domains.is_empty() {
                facts.push(row.outlet_domains.join(", "));
            }
            ui.label(RichText::new(facts.join(" · ")).color(TEXT_DIM).small());
            ui.horizontal_wrapped(|ui| {
                for url in row.urls.iter().filter(|u| web_url(u).is_some()).take(3) {
                    ui.hyperlink_to(RichText::new(format!("{} ↗", link_host(url))).small(), url);
                }
            });
        }

        // Paging, never an unbounded scroll: the query itself is limited to
        // one page, so an enormous region cannot drag the frame down.
        let mut jump = None;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let has_prev = page.offset > 0;
            let has_next = (last as u64) < page.total;
            if ui
                .add_enabled(has_prev, egui::Button::new("← newer"))
                .clicked()
            {
                jump = Some(page.offset.saturating_sub(LEDGER_PAGE_SIZE));
            }
            if ui
                .add_enabled(has_next, egui::Button::new("older →"))
                .clicked()
            {
                jump = Some(page.offset + LEDGER_PAGE_SIZE);
            }
        });
        jump
    }

    /// Horizontal color strip sampled from `color`, with a caption at each end.
    fn legend_ramp(
        ui: &mut egui::Ui,
        left: &str,
        right: &str,
        color: impl Fn(f32) -> egui::Color32,
    ) {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().min(220.0), 12.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        let steps = 32;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            let seg = Rect::from_min_max(
                Pos2::new(rect.min.x + rect.width() * t0, rect.min.y),
                Pos2::new(rect.min.x + rect.width() * t1, rect.max.y),
            );
            painter.rect_filled(seg, 0.0, color((t0 + t1) / 2.0));
        }
        painter.text(
            rect.left_bottom() + Vec2::new(0.0, 2.0),
            Align2::LEFT_TOP,
            left,
            FontId::proportional(10.0),
            TEXT_DIM,
        );
        painter.text(
            rect.right_bottom() + Vec2::new(0.0, 2.0),
            Align2::RIGHT_TOP,
            right,
            FontId::proportional(10.0),
            TEXT_DIM,
        );
        ui.add_space(12.0);
    }

    fn sequential_heat_legend(&self, ui: &mut egui::Ui) {
        let metric = match self.filters.heat_metric {
            HeatMetric::Attention => "media attention",
            HeatMetric::Events => "event count",
            HeatMetric::Diversity => "source diversity (peak distinct outlets / 6 h)",
            HeatMetric::Divergence => unreachable!("has its own legend"),
        };
        ui.label(RichText::new(format!("Heatmap · {metric} (log scale)")).small());
        Self::legend_ramp(ui, "low", "high", renderer::heat_color);
    }

    /// docs/VISUALIZATION.md V2 item 5. The reading *and* its caveat — this
    /// layer is a picture of coverage bias, so it must not be presented as a
    /// picture of the world (docs/SAFETY_AND_PRIVACY.md § "Known biases").
    fn divergence_legend(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Heatmap · attention ↔ unrest divergence").small());
        Self::legend_ramp(ui, "events lead", "attention leads", |t| {
            renderer::divergence_color(t * 2.0 - 1.0)
        });
        ui.label(
            RichText::new(
                "Each cell's media-attention and unrest components are ranked \
                 separately against every other cell in this window; the color is \
                 the gap between the two ranks. Violet = attention outruns the \
                 event record (covered, but little happening on the ground). \
                 Teal = events outrun attention (happening, but little covered).",
            )
            .color(TEXT_DIM)
            .small(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            crate::style::region_swatch(
                ui,
                renderer::divergence_color(0.0)
                    .gamma_multiply(renderer::DIVERGENCE_NO_DATA_DIM + 0.4),
            );
            ui.label(
                RichText::new("dimmed — one channel has no records here, so nothing is claimed")
                    .color(TEXT_DIM)
                    .small(),
            );
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Read this as a map of our own coverage, not of the world. Media \
                 density varies enormously by language and region, and this \
                 project's attention feed is geocoded to the publisher's country \
                 — so a teal cell may be genuinely under-reported, or simply \
                 outside what our sources index. Ranks are relative to the \
                 visible window: change the window and every cell can move. \
                 See docs/SAFETY_AND_PRIVACY.md § \"Known biases\".",
            )
            .color(TEXT_DIM)
            .small(),
        );
    }

    /// Small caption above a legend group.
    fn legend_group(ui: &mut egui::Ui, title: &str) {
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong().small());
    }

    /// One swatch + label row, with the swatch painted by `draw`.
    fn legend_row(
        ui: &mut egui::Ui,
        draw: impl FnOnce(&mut egui::Ui),
        text: &str,
        note: Option<&str>,
    ) {
        ui.horizontal(|ui| {
            draw(ui);
            ui.label(RichText::new(text).small());
            if let Some(note) = note {
                ui.label(RichText::new(note).color(TEXT_DIM).small());
            }
        });
    }

    /// The full encoding reference (docs/VISUALIZATION.md V3 item 8).
    ///
    /// Collapsible and open by default: it is long on purpose — every channel
    /// on the map is documented here, including the ones that mean "we are not
    /// claiming anything" — but it sits under the region inspector, so a user
    /// who has read it once should be able to fold it away.
    fn inspector_legend(&self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Legend").heading())
            .default_open(true)
            .show_unindented(ui, |ui| self.legend_body(ui));
    }

    fn legend_body(&self, ui: &mut egui::Ui) {
        let style = &self.map.style;

        Self::legend_group(ui, "Marker color — what kind of thing");
        // Each kind carries its family, because the family is what says how to
        // read the number attached to the marker: a chatter marker counts
        // posts and an attention marker counts articles, and those are never
        // the same quantity (docs/SIGNAL_MODEL.md). `Measurement` is declared
        // in the contract but has no source and no lane, so listing it would
        // promise a channel the map never draws.
        for kind in EventKind::ALL
            .into_iter()
            .filter(|k| *k != EventKind::Measurement)
        {
            let note = format!(
                "{} \u{b7} counted in {}",
                kind.family().label(),
                kind.family().volume_unit().label(2)
            );
            Self::legend_row(
                ui,
                |ui| {
                    crate::style::glyph_swatch(
                        ui,
                        renderer::MarkerGlyph::Diamond,
                        style.marker_color(kind),
                    );
                },
                kind.label(),
                Some(&note),
            );
        }

        Self::legend_group(ui, "Marker shape — which feed reported it");
        for glyph in renderer::MarkerGlyph::ALL {
            Self::legend_row(
                ui,
                |ui| {
                    crate::style::glyph_swatch(ui, glyph, Color32::from_rgb(210, 214, 224));
                },
                glyph.source_label(),
                None,
            );
        }
        ui.label(
            RichText::new(
                "Color and shape are independent: Bluesky and Telegram both \
                 report aggregate chatter, so they share that fill and are told \
                 apart only by their shape.",
            )
            .color(TEXT_DIM)
            .small(),
        );

        Self::legend_group(ui, "Marker size — severity");
        ui.horizontal(|ui| {
            for sev in [0.0f32, 0.5, 1.0] {
                crate::style::severity_swatch(ui, sev, style.marker_conflict);
            }
            ui.label(
                RichText::new("source-reported severity (e.g. ACLED fatalities)")
                    .color(TEXT_DIM)
                    .small(),
            );
        });
        ui.label(
            RichText::new(
                "Records with no severity fall back to sizing by article count. \
                 Size is never a claim about importance.",
            )
            .color(TEXT_DIM)
            .small(),
        );

        Self::legend_group(ui, "Overlays");
        Self::legend_row(
            ui,
            |ui| {
                crate::style::ring_swatch(ui, style.halo_color);
            },
            "Spike halo",
            Some("cell clearly above its own trailing baseline"),
        );
        Self::legend_row(
            ui,
            |ui| {
                crate::style::alert_swatch(
                    ui,
                    renderer::alert_color(0.9)
                        .gamma_multiply(f32::from(style.alert_alpha) / 255.0 + 0.35),
                    style.alert_outline,
                );
            },
            "NOAA/NWS weather alert",
            Some("US only"),
        );
        ui.horizontal(|ui| {
            for s in [0.0f32, 0.5, 1.0] {
                crate::style::region_swatch(ui, renderer::alert_color(s));
            }
            ui.label(
                RichText::new("alert severity: none claimed → extreme")
                    .color(TEXT_DIM)
                    .small(),
            );
        });
        ui.label(
            RichText::new(
                "Weather alerts get a cool palette and a dashed outline no other \
                 layer uses, so a storm warning is never read as unrest. Their \
                 darkest tint means the alert carried no severity rating — not \
                 that it is mild.",
            )
            .color(TEXT_DIM)
            .small(),
        );

        Self::legend_group(ui, "Heatmap");
        if self.filters.heat_metric == HeatMetric::Divergence {
            self.divergence_legend(ui);
        } else {
            self.sequential_heat_legend(ui);
        }

        Self::legend_group(ui, "Precision — what may be drawn where");
        Self::legend_row(
            ui,
            |ui| {
                crate::style::glyph_swatch(ui, renderer::MarkerGlyph::Diamond, style.marker_other);
            },
            "City / exact",
            Some("drawn as a point"),
        );
        Self::legend_row(
            ui,
            |ui| {
                crate::style::region_swatch(ui, renderer::heat_color(0.6));
            },
            "Country / admin",
            Some("shades a region, never a point"),
        );
        ui.label(
            RichText::new(
                "A country-precision record is placed at a centroid we did not \
                 observe, so drawing it as a dot would invent a location. NOAA \
                 and IODA are region-only for exactly this reason, which is why \
                 neither has a marker shape.",
            )
            .color(TEXT_DIM)
            .small(),
        );

        ui.add_space(12.0);
        let update_state = if self.online {
            "Live updates are enabled."
        } else {
            "Live updates are paused; cached real data remains visible."
        };
        let data_note = format!(
            "Media attention is an imperfect, biased proxy — not ground truth. \
             Attention and event data are computed and shown separately. \
             The desktop stores and displays live-source data only. {update_state}"
        );
        ui.label(RichText::new(data_note).color(TEXT_DIM).small());
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Basemap: Natural Earth (public domain). Data sources: GDELT, \
                 authorized ACLED, NOAA/NWS, IODA, Bluesky, and Telegram.",
            )
            .color(TEXT_DIM)
            .small(),
        );
    }

    /// Per-frame map inputs, gathered in one place so the two `map.show` call
    /// sites (loading and ready) can never drift apart.
    ///
    /// Takes the fields it reads rather than `&self`: `map.show` needs
    /// `&mut self.map`, and only field-level borrows are disjoint enough for
    /// the two to coexist.
    fn map_inputs<'a>(
        filters: &crate::app::Filters,
        selected_cell: Option<u64>,
        countries: &'a geo_utils::CountryIndex,
    ) -> crate::map_view::MapInputs<'a> {
        crate::map_view::MapInputs {
            selected_cell,
            show_heatmap: filters.show_heatmap,
            show_markers: filters.show_markers,
            show_spike_halos: filters.show_spike_halos,
            show_alerts: filters.show_alerts,
            show_graticule: filters.show_graticule,
            show_labels: filters.show_labels,
            focus_selection: filters.focus_selection,
            countries,
        }
    }

    pub fn central_map(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::NONE.fill(self.map.style.background))
            .show(ui, |ui| {
                match &self.phase {
                    Phase::Error(msg) => {
                        let msg = msg.clone();
                        ui.centered_and_justified(|ui| {
                            ui.colored_label(Color32::from_rgb(255, 120, 120), msg);
                        });
                        return;
                    }
                    Phase::Loading(msg) => {
                        // Keep painting the basemap under a loading notice.
                        let msg = msg.clone();
                        let actions = self.map.show(
                            ui,
                            &Self::map_inputs(&self.filters, self.selected_cell, &self.countries),
                        );
                        let _ = actions; // no selection while loading
                        let rect = ui.max_rect();
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            msg,
                            FontId::proportional(16.0),
                            Color32::from_rgb(210, 214, 224),
                        );
                        return;
                    }
                    Phase::Ready => {}
                }
                let actions = self.map.show(
                    ui,
                    &Self::map_inputs(&self.filters, self.selected_cell, &self.countries),
                );
                if let Some(cell) = actions.selected_cell {
                    self.select_cell(cell, actions.clicked_lonlat);
                }
            });
    }

    /// "How to read this map" (docs/VISUALIZATION.md V3 item 10).
    ///
    /// Dismissing it — by either button or the window's close control — is
    /// what records it as seen, so a user who closes it without reading is not
    /// nagged again, and `?` always brings it back.
    pub fn how_to_read_window(&mut self, ctx: &egui::Context) {
        if !self.show_how_to_read {
            return;
        }
        let mut open = true;
        let mut dismissed = false;
        egui::Window::new("How to read this map")
            .open(&mut open)
            .default_width(600.0)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                dismissed = crate::how_to_read::show(ui);
            });
        if dismissed || !open {
            self.show_how_to_read = false;
            self.mark_how_to_read_seen();
        }
    }

    /// Settings: per-source state and the enable switch.
    ///
    /// `settings_screen::show` borrows the app immutably (it reads status
    /// lines the worker already sent) and hands back the toggle the user
    /// flipped, which is applied here where `&mut self` is available.
    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut toggled = None;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                toggled = crate::settings_screen::show(self, ui);
            });
        if let Some((source, on)) = toggled {
            self.set_source_enabled(source, on);
        }
        self.show_settings = open;
    }

    /// About: attributions, licence, version.
    pub fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        egui::Window::new("About")
            .open(&mut open)
            .default_width(600.0)
            .show(ctx, |ui| crate::about::show(self, ui));
        self.show_about = open;
    }

    pub fn log_window(&mut self, ctx: &egui::Context) {
        if !self.show_log_window {
            return;
        }
        let mut open = true;
        egui::Window::new("Ingest log")
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                let Some((total, rows)) = &self.ingest_log else {
                    ui.label("no log loaded");
                    return;
                };
                ui.label(format!(
                    "{total} total records refused at normalization (most recent {}):",
                    rows.len()
                ));
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for row in rows {
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    fmt_ts(row.ts_epoch_s),
                                    row.source
                                ))
                                .color(TEXT_DIM)
                                .small(),
                            );
                            ui.colored_label(Color32::from_rgb(255, 170, 120), &row.reason);
                            ui.label(RichText::new(&row.raw_excerpt).small().monospace());
                            ui.separator();
                        }
                    });
            });
        if !open {
            self.show_log_window = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::youtube_search_url;

    #[test]
    fn external_video_search_encodes_user_visible_context() {
        let search = youtube_search_url("Mexico City protest & safety");
        assert!(search.starts_with("https://www.youtube.com/results?search_query="));
        assert!(!search.contains(' '));
        let parsed = url::Url::parse(&search).unwrap();
        let query = parsed
            .query_pairs()
            .find(|(key, _)| key == "search_query")
            .map(|(_, value)| value.into_owned());
        assert_eq!(query.as_deref(), Some("Mexico City protest & safety"));
    }
}
