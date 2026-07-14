//! The central map widget: viewport interactions (pan/zoom), layer painting,
//! marker hover tooltips, and cell selection.

use core_types::H3_RESOLUTION;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use geo_utils::MapViewport;
use renderer::{BasemapLayer, HeatmapLayer, MapStyle, MarkerLayer};
use storage::EventPoint;

pub struct MapView {
    pub viewport: Option<MapViewport>,
    pub basemap: BasemapLayer,
    pub heatmap: HeatmapLayer,
    pub markers: MarkerLayer,
    /// Rows behind the marker layer, indexed by `MarkerInput::source_index`.
    pub marker_rows: Vec<EventPoint>,
    pub style: MapStyle,
}

/// What the user did to the map this frame.
#[derive(Default)]
pub struct MapActions {
    /// Click selected this H3 cell (res 3).
    pub selected_cell: Option<u64>,
    /// Geo position of the click, for country labeling.
    pub clicked_lonlat: Option<(f64, f64)>,
}

impl MapView {
    pub fn new(basemap: BasemapLayer, style: MapStyle) -> Self {
        Self {
            viewport: None,
            basemap,
            heatmap: HeatmapLayer::empty(),
            markers: MarkerLayer::new(Vec::new()),
            marker_rows: Vec::new(),
            style,
        }
    }

    /// Paint the map into the available space and handle interactions.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        selected_cell: Option<u64>,
        show_heatmap: bool,
        show_markers: bool,
    ) -> MapActions {
        let size = ui.available_size().max(Vec2::new(64.0, 64.0));
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let rect = response.rect;

        // Viewport: create on first frame, track window resizes.
        let vp = self.viewport.get_or_insert_with(|| {
            let mut v = MapViewport::fit_world(rect.width(), rect.height());
            // Fill the height rather than the width if the window is tall.
            let fit_h = 180.0 / f64::from(rect.height().max(64.0));
            v.deg_per_px = v.deg_per_px.min(fit_h.max(geo_utils::MIN_DEG_PER_PX));
            v
        });
        vp.set_screen(rect.width(), rect.height());
        let max_deg_per_px = (360.0 / f64::from(rect.width().max(64.0)))
            .max(180.0 / f64::from(rect.height().max(64.0)))
            .min(geo_utils::MAX_DEG_PER_PX);

        // --- interactions ---
        if response.dragged() {
            let d = response.drag_delta();
            vp.pan_pixels(d.x, d.y);
        }
        if let Some(hover) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta().y);
            let pinch = ui.input(|i| i.zoom_delta());
            let factor = f64::from(pinch) * (f64::from(scroll) * 0.0022).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let local = hover - rect.min;
                vp.zoom_around(local.x, local.y, factor);
            }
            if vp.deg_per_px > max_deg_per_px {
                vp.deg_per_px = max_deg_per_px;
            }
        }
        let vp = *self.viewport.as_ref().expect("viewport initialized above");

        // Affine mapping lon/lat directly to *screen* coordinates (the
        // painter is not translated), so fold the rect origin into it.
        let mut aff = vp.affine();
        aff.b += f64::from(rect.min.x);
        aff.d += f64::from(rect.min.y);

        // --- layers (background → heat → borders/markers → overlays) ---
        painter.rect_filled(rect, 0.0, self.style.background);
        self.basemap
            .paint(&painter, &aff, rect.width(), &self.style);
        if show_heatmap {
            self.heatmap.paint(&painter, &aff, rect.width());
        }
        if show_markers {
            self.markers
                .paint(&painter, &aff, rect.width(), &self.style);
        }
        if let Some(cell) = selected_cell {
            self.draw_cell_outline(&painter, &aff, rect.width(), cell);
        }

        // --- hover tooltip (custom-painted; no per-frame layout churn) ---
        let mut actions = MapActions::default();
        if let Some(hover) = response.hover_pos() {
            if show_markers
                && let Some(hit) = self.markers.hit_test(&aff, rect.width(), hover, 8.0)
                && let Some(row) = self.marker_rows.get(hit.source_index)
            {
                self.draw_tooltip(&painter, rect, hover, row);
            }
            if response.clicked() {
                let local = hover - rect.min;
                let (lon, lat) = vp.unproject(local.x, local.y);
                let lon = wrap_lon(lon);
                if (-90.0..=90.0).contains(&lat)
                    && let Ok(cell) = geo_utils::cell_for_latlon(lat, lon, H3_RESOLUTION)
                {
                    actions.selected_cell = Some(cell);
                    actions.clicked_lonlat = Some((lon, lat));
                }
            }
        }

        actions
    }

    fn draw_cell_outline(
        &self,
        painter: &egui::Painter,
        aff: &geo_utils::Affine,
        screen_w: f32,
        cell: u64,
    ) {
        let Ok(ring) = geo_utils::cell_boundary_lonlat(cell) else {
            return;
        };
        for offset in renderer::visible_world_offsets(aff, screen_w) {
            let points: Vec<Pos2> = ring
                .iter()
                .map(|&(lon, lat)| {
                    let (x, y) = aff.apply(lon + offset, lat);
                    Pos2::new(x, y)
                })
                .collect();
            painter.add(Shape::closed_line(
                points,
                Stroke::new(1.5, Color32::from_rgb(240, 240, 250)),
            ));
        }
    }

    fn draw_tooltip(&self, painter: &egui::Painter, rect: Rect, at: Pos2, row: &EventPoint) {
        let when = chrono::DateTime::from_timestamp(row.ts_epoch_s, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default();
        let title = row.headline.as_deref().unwrap_or("(no headline)");
        let lines = [
            format!("{} · {}", row.kind.label(), when),
            truncate(title, 60),
            format!(
                "{} articles · confidence {:.0}%",
                row.article_count,
                f64::from(row.confidence) * 100.0
            ),
        ];

        let font = FontId::proportional(12.0);
        let width = lines
            .iter()
            .map(|l| {
                painter
                    .layout_no_wrap(l.clone(), font.clone(), Color32::WHITE)
                    .rect
                    .width()
            })
            .fold(0.0f32, f32::max);
        let line_h = 16.0;
        let pad = 8.0;
        let box_size = Vec2::new(width + pad * 2.0, line_h * lines.len() as f32 + pad * 2.0);
        let mut origin = at + Vec2::new(14.0, 10.0);
        if origin.x + box_size.x > rect.max.x {
            origin.x = at.x - box_size.x - 6.0;
        }
        if origin.y + box_size.y > rect.max.y {
            origin.y = at.y - box_size.y - 6.0;
        }
        let tip = Rect::from_min_size(origin, box_size);
        painter.rect_filled(tip, 4.0, Color32::from_rgba_unmultiplied(16, 20, 28, 235));
        painter.rect_stroke(
            tip,
            4.0,
            Stroke::new(1.0, self.style.marker_color(row.kind)),
            egui::StrokeKind::Inside,
        );
        for (i, line) in lines.iter().enumerate() {
            let color = if i == 0 {
                self.style.marker_color(row.kind)
            } else {
                Color32::from_rgb(220, 224, 232)
            };
            painter.text(
                tip.min + Vec2::new(pad, pad + line_h * i as f32),
                Align2::LEFT_TOP,
                line,
                font.clone(),
                color,
            );
        }
    }
}

pub fn wrap_lon(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
