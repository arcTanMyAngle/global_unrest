//! Panels: top filter bar, bottom timeline, right inspector, central map,
//! and the ingest-log window.

use chrono::DateTime;
use core_types::EventKind;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Vec2};

use crate::app::{App, HeatMetric, Phase, WindowLen};

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

                // Live GDELT online toggle. Off = permanent offline fixture
                // base; on = fixtures + live GDELT ingest on the feed cadence.
                let mut online = self.online;
                if ui
                    .checkbox(&mut online, "GDELT live")
                    .on_hover_text(
                        "Fetch live GDELT attention + events (keyless, attributed). \
                         Fixtures always remain the offline base.",
                    )
                    .changed()
                {
                    self.set_online(online);
                }
                if self.online
                    && ui
                        .button("↻")
                        .on_hover_text("fetch the latest GDELT data now")
                        .clicked()
                {
                    self.fetch_now();
                }
                self.source_status_label(ui);
                ui.separator();

                let mut changed = false;
                changed |= ui
                    .checkbox(&mut self.filters.show_heatmap, "heatmap")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.filters.show_markers, "markers")
                    .changed();
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

                if ui.button("reset view").clicked() {
                    self.map.viewport = None;
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

    /// Compact live-source status shown next to the online toggle.
    fn source_status_label(&self, ui: &mut egui::Ui) {
        match &self.source_status {
            Some(s) if s.online => {
                let (dot, color) = if s.degraded {
                    ("●", Color32::from_rgb(255, 170, 90))
                } else {
                    ("●", Color32::from_rgb(120, 210, 140))
                };
                ui.colored_label(color, dot);
                ui.label(RichText::new(&s.detail).color(TEXT_DIM).small());
            }
            _ => {
                ui.label(
                    RichText::new("offline fixture mode")
                        .color(TEXT_DIM)
                        .small(),
                );
            }
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
                    let len = self.timeline.len.buckets(total);
                    self.timeline.start_bucket =
                        self.timeline.start_bucket.min((total - len).max(0));
                    self.mark_dirty();
                }

                let slider_width = (ui.available_width() - 340.0).max(120.0);
                ui.style_mut().spacing.slider_width = slider_width;
                let mut start = self.timeline.start_bucket;
                let resp =
                    ui.add(egui::Slider::new(&mut start, 0..=max_start.max(0)).show_value(false));
                if resp.changed() {
                    self.timeline.start_bucket = start;
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
                    ui.label(format!(
                        "{} events stored ({} new, {} duplicate)",
                        r.inserted + r.duplicates,
                        r.inserted,
                        r.duplicates
                    ));
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
            }
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

        // Media attention vs event data — always presented separately.
        let attention = detail
            .counts_by_kind
            .iter()
            .find(|(k, _)| k.is_attention())
            .map(|(_, c)| *c)
            .unwrap_or(0);
        ui.add_space(6.0);
        ui.label(RichText::new("Media attention").strong());
        ui.label(format!(
            "{attention} attention records · {} articles · {} distinct outlets",
            detail.total_articles, detail.distinct_outlets
        ));

        ui.add_space(6.0);
        ui.label(RichText::new("Event data").strong());
        let mut any_events = false;
        for (kind, count) in &detail.counts_by_kind {
            if kind.is_discrete_event() {
                any_events = true;
                ui.horizontal(|ui| {
                    let color = self.map.style.marker_color(*kind);
                    ui.colored_label(color, "●");
                    ui.label(format!("{} × {}", count, kind.label()));
                });
            }
        }
        if !any_events {
            ui.label(RichText::new("none in window").color(TEXT_DIM));
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

        ui.add_space(6.0);
        ui.label(RichText::new("Location confidence (mean)").strong());
        ui.add(
            egui::ProgressBar::new(detail.mean_confidence)
                .text(format!("{:.0}%", f64::from(detail.mean_confidence) * 100.0)),
        );

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
    }

    fn inspector_legend(&self, ui: &mut egui::Ui) {
        ui.heading("Legend");
        ui.label(RichText::new("Markers (city/exact precision only)").small());
        for kind in EventKind::ALL {
            ui.horizontal(|ui| {
                ui.colored_label(self.map.style.marker_color(kind), "◆");
                ui.label(RichText::new(kind.label()).small());
            });
        }
        ui.add_space(4.0);
        let metric = match self.filters.heat_metric {
            HeatMetric::Attention => "media attention",
            HeatMetric::Events => "event count",
            HeatMetric::Diversity => "source diversity (peak distinct outlets / 6 h)",
        };
        ui.label(RichText::new(format!("Heatmap · {metric} (log scale)")).small());
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
            painter.rect_filled(seg, 0.0, renderer::heat_color((t0 + t1) / 2.0));
        }
        painter.text(
            rect.left_bottom() + Vec2::new(0.0, 2.0),
            Align2::LEFT_TOP,
            "low",
            FontId::proportional(10.0),
            TEXT_DIM,
        );
        painter.text(
            rect.right_bottom() + Vec2::new(0.0, 2.0),
            Align2::RIGHT_TOP,
            "high",
            FontId::proportional(10.0),
            TEXT_DIM,
        );
        ui.add_space(16.0);

        ui.label(
            RichText::new(
                "Coarse-precision records (country/admin centroids) shade regions \
                 but never render as points.",
            )
            .color(TEXT_DIM)
            .small(),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Media attention is an imperfect, biased proxy — not ground truth. \
                 Attention and event data are computed and shown separately. \
                 All current data is synthetic fixture data.",
            )
            .color(TEXT_DIM)
            .small(),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("Basemap: Natural Earth (public domain).")
                .color(TEXT_DIM)
                .small(),
        );
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
                            self.selected_cell,
                            self.filters.show_heatmap,
                            self.filters.show_markers,
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
                    self.selected_cell,
                    self.filters.show_heatmap,
                    self.filters.show_markers,
                );
                if let Some(cell) = actions.selected_cell {
                    self.select_cell(cell, actions.clicked_lonlat);
                }
            });
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
