//! The central map widget: viewport interactions (pan/zoom), layer painting,
//! marker hover tooltips, and cell selection.

use core_types::H3_RESOLUTION;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use geo_utils::MapViewport;
use renderer::{BasemapLayer, HaloLayer, HeatmapLayer, MapStyle, MarkerLayer};
use storage::EventPoint;

/// Seconds a fly-to takes. Long enough to keep the viewer oriented (the point
/// of animating instead of jumping), short enough not to feel like waiting.
const FLY_SECS: f32 = 0.6;

/// Zoom a fly-to settles at, unless the map is already closer — flying should
/// never pull the viewer *back* from a detail view they chose. At ~1200 px
/// wide this frames roughly 24° of longitude, so a res-3 cell reads clearly
/// while its surroundings stay on screen.
const FLY_DEG_PER_PX: f64 = 0.02;

/// An in-progress fly-to (docs/VISUALIZATION.md V2 item 6). Bounded: `t`
/// advances to 1, the viewport snaps exactly to the target, and the flight is
/// dropped — nothing here requests a repaint once it has landed.
struct Flight {
    from_lon: f64,
    from_lat: f64,
    from_deg_per_px: f64,
    to_lon: f64,
    to_lat: f64,
    to_deg_per_px: f64,
    t: f32,
}

pub struct MapView {
    pub viewport: Option<MapViewport>,
    pub basemap: BasemapLayer,
    pub heatmap: HeatmapLayer,
    pub markers: MarkerLayer,
    pub spike_halos: HaloLayer,
    /// Rows behind the marker layer, indexed by `MarkerInput::source_index`.
    pub marker_rows: Vec<EventPoint>,
    pub style: MapStyle,
    flight: Option<Flight>,
}

/// Ease-in-out on [0, 1]: the viewport accelerates away and decelerates in,
/// which reads as travel rather than as a cut.
fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

/// Signed shortest longitude delta from `a` to `b`, in (-180, 180]. Flying
/// from 170°E to 170°W must cross the antimeridian (+20°), not sweep -340°
/// back across the whole world.
fn shortest_lon_delta(a: f64, b: f64) -> f64 {
    let mut d = (b - a) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    }
    if d <= -180.0 {
        d += 360.0;
    }
    d
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
            spike_halos: HaloLayer::new(Vec::new()),
            marker_rows: Vec::new(),
            style,
            flight: None,
        }
    }

    /// Start an animated fly-to centered on (`lon`, `lat`). A no-op before the
    /// first frame has established a viewport; replaces any flight already in
    /// progress, so rapid clicks in the top-movers panel retarget rather than
    /// queue up.
    pub fn fly_to(&mut self, lon: f64, lat: f64) {
        let Some(vp) = self.viewport else {
            return;
        };
        self.flight = Some(Flight {
            from_lon: vp.center_lon,
            from_lat: vp.center_lat,
            from_deg_per_px: vp.deg_per_px,
            // Carry the wrapped delta, not the raw target, so the lerp takes
            // the short way; `MapViewport::clamp` re-wraps on arrival.
            to_lon: vp.center_lon + shortest_lon_delta(vp.center_lon, lon),
            to_lat: lat.clamp(-90.0, 90.0),
            to_deg_per_px: vp.deg_per_px.min(FLY_DEG_PER_PX),
            t: 0.0,
        });
    }

    /// Advance an in-progress fly-to by one frame. Returns true while the
    /// flight is still running, which is the *only* thing here that asks for a
    /// repaint — an idle map stays idle.
    fn advance_flight(&mut self, dt: f32) -> bool {
        let Some(f) = &mut self.flight else {
            return false;
        };
        f.t = (f.t + dt / FLY_SECS).min(1.0);
        let e = f64::from(ease_in_out(f.t));
        let lon = f.from_lon + (f.to_lon - f.from_lon) * e;
        let lat = f.from_lat + (f.to_lat - f.from_lat) * e;
        // Zoom interpolates in log space: a linear deg-per-px ramp would
        // spend most of the flight at the wide end and snap in at the finish.
        let deg_per_px = f.from_deg_per_px * (f.to_deg_per_px / f.from_deg_per_px).powf(e);
        let landed = f.t >= 1.0;
        if landed {
            self.flight = None;
        }
        if let Some(vp) = &mut self.viewport {
            // The lerp runs on the unwrapped target so it can cross ±180°;
            // the viewport itself stays wrapped, which is the same view.
            vp.center_lon = wrap_lon(lon);
            vp.center_lat = lat;
            vp.deg_per_px = deg_per_px;
        }
        !landed
    }

    /// Paint the map into the available space and handle interactions.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        selected_cell: Option<u64>,
        show_heatmap: bool,
        show_markers: bool,
        show_spike_halos: bool,
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
        let mut gestured = false;
        if response.dragged() {
            let d = response.drag_delta();
            vp.pan_pixels(d.x, d.y);
            gestured = true;
        }
        if let Some(hover) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta().y);
            let pinch = ui.input(|i| i.zoom_delta());
            let factor = f64::from(pinch) * (f64::from(scroll) * 0.0022).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let local = hover - rect.min;
                vp.zoom_around(local.x, local.y, factor);
                gestured = true;
            }
            if vp.deg_per_px > max_deg_per_px {
                vp.deg_per_px = max_deg_per_px;
            }
        }

        // A fly-to yields to the hand on the map — the view must never fight
        // a pan or zoom the user started mid-flight.
        if gestured {
            self.flight = None;
        }
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        if self.advance_flight(dt) {
            ui.ctx().request_repaint();
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
        if show_spike_halos && !self.spike_halos.is_empty() {
            let time_secs = ui.input(|i| i.time);
            self.spike_halos
                .paint(&painter, &aff, rect.width(), &self.style, time_secs);
            // Keep the pulse animating; bounded to a slow tick and only
            // while halos are actually shown.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
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
        let mut detail_parts = vec![row.source.to_string(), row.precision.label().to_string()];
        if let Some(severity) = row.severity {
            detail_parts.push(format!("severity {severity:.2}"));
        }
        if row.has_video {
            detail_parts.push("🎥 video".to_string());
        }
        let lines = [
            format!("{} · {}", row.kind.label(), when),
            truncate(title, 60),
            format!(
                "{} articles · confidence {:.0}%",
                row.article_count,
                f64::from(row.confidence) * 100.0
            ),
            detail_parts.join(" · "),
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

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    /// The fly-to tests only exercise viewport math, so an empty basemap
    /// keeps them off the bundled Natural Earth data entirely.
    fn test_basemap() -> BasemapLayer {
        BasemapLayer::from_geojson_str(
            r#"{"type":"FeatureCollection","features":[]}"#,
            &MapStyle::default(),
        )
        .expect("empty feature collection")
    }

    #[test]
    fn fly_to_crosses_the_antimeridian_the_short_way() {
        // Fiji-ish to Kamchatka-ish: 20° east across the dateline, never 340°
        // west across the entire map.
        assert!((shortest_lon_delta(170.0, -170.0) - 20.0).abs() < EPS);
        assert!((shortest_lon_delta(-170.0, 170.0) + 20.0).abs() < EPS);
        assert!((shortest_lon_delta(-10.0, 30.0) - 40.0).abs() < EPS);
        assert!((shortest_lon_delta(0.0, 0.0)).abs() < EPS);
    }

    #[test]
    fn shortest_lon_delta_stays_within_half_a_turn() {
        for a in [-180.0, -73.5, 0.0, 12.0, 179.9] {
            for b in [-179.9, -45.0, 0.0, 88.0, 180.0] {
                let d = shortest_lon_delta(a, b);
                assert!(d > -180.0 && d <= 180.0, "{a}->{b} gave {d}");
                // Applying the delta must land on the requested longitude.
                assert!((wrap_lon(a + d) - wrap_lon(b)).abs() < 1e-9, "{a}->{b}");
            }
        }
    }

    #[test]
    fn ease_is_pinned_at_both_ends_and_monotonic() {
        assert!((ease_in_out(0.0)).abs() < 1e-6);
        assert!((ease_in_out(1.0) - 1.0).abs() < 1e-6);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-6);
        // Clamped outside [0, 1] so an overshooting dt can't fling the view.
        assert!((ease_in_out(-3.0)).abs() < 1e-6);
        assert!((ease_in_out(9.0) - 1.0).abs() < 1e-6);
        let mut prev = -1.0;
        for i in 0..=20 {
            let e = ease_in_out(i as f32 / 20.0);
            assert!(e >= prev, "not monotonic at {i}");
            prev = e;
        }
    }

    /// The flight must be *bounded*: it reaches the target exactly and then
    /// stops asking for frames. A fly-to that never settles would repaint the
    /// map forever (docs/VISUALIZATION.md perf guardrail).
    #[test]
    fn flight_lands_exactly_and_then_stops_requesting_frames() {
        let mut view = MapView::new(test_basemap(), MapStyle::default());
        view.viewport = Some(MapViewport::fit_world(1200.0, 600.0));
        let start_zoom = view.viewport.unwrap().deg_per_px;
        view.fly_to(30.5, 50.4);

        let dt = 1.0 / 60.0;
        let mut frames = 0;
        while view.advance_flight(dt) {
            frames += 1;
            assert!(frames < 600, "flight did not terminate");
        }
        assert!(frames > 1, "a fly-to should animate, not jump");

        let vp = view.viewport.unwrap();
        assert!((vp.center_lon - 30.5).abs() < 1e-6, "{}", vp.center_lon);
        assert!((vp.center_lat - 50.4).abs() < 1e-6, "{}", vp.center_lat);
        assert!(vp.deg_per_px <= start_zoom, "fly-to must not zoom out");
        assert!((vp.deg_per_px - FLY_DEG_PER_PX).abs() < 1e-9);
        // Settled: no flight left, so `show` stops requesting repaints.
        assert!(!view.advance_flight(dt));
    }

    #[test]
    fn fly_to_never_pulls_back_from_a_closer_view() {
        let mut view = MapView::new(test_basemap(), MapStyle::default());
        let mut vp = MapViewport::fit_world(1200.0, 600.0);
        vp.deg_per_px = FLY_DEG_PER_PX / 4.0; // user zoomed in past the default
        view.viewport = Some(vp);
        view.fly_to(-0.1, 51.5);
        while view.advance_flight(1.0 / 60.0) {}
        assert!((view.viewport.unwrap().deg_per_px - FLY_DEG_PER_PX / 4.0).abs() < 1e-12);
    }

    #[test]
    fn fly_to_without_a_viewport_is_a_no_op() {
        let mut view = MapView::new(test_basemap(), MapStyle::default());
        view.fly_to(10.0, 10.0);
        assert!(!view.advance_flight(1.0 / 60.0));
    }
}
