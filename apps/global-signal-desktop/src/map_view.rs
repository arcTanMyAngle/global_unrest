//! The central map widget: viewport interactions (pan/zoom), layer painting,
//! marker hover tooltips, and cell selection.

use std::sync::Arc;

use core_types::H3_RESOLUTION;
use egui::{Align2, Color32, FontId, Galley, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use geo_utils::{CountryIndex, MapViewport};
use renderer::{
    AlertLayer, BasemapLayer, GraticuleLayer, HaloLayer, HeatmapLayer, MapStyle, MarkerLayer,
};
use storage::EventPoint;

/// Point size of a country label. Small and low-contrast on purpose: labels
/// are orientation, never a data layer.
const LABEL_FONT_PX: f32 = 11.0;

/// Most country labels drawn in one frame. Collision culling usually stops
/// well short of this; it caps the work when it does not.
const MAX_LABELS: usize = 40;

/// Padding added around a label's box when testing it against labels already
/// placed, so two survivors never sit shoulder to shoulder.
const LABEL_PAD_PX: f32 = 5.0;

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

/// One country label: where it goes, how big it is, and the galley to blit.
struct MapLabel {
    lon: f64,
    lat: f64,
    /// Bounding-box extent in square degrees — the collision ranking key, so
    /// a large country keeps its label and a small neighbour loses it. Not an
    /// area; see `CountryIndex::iter_with_extent`.
    extent: f64,
    galley: Arc<Galley>,
}

/// Everything the map needs from the app for one frame. A struct rather than
/// a dozen positional `bool`s — V3 added four of them and the call sites had
/// become unreadable.
pub struct MapInputs<'a> {
    pub selected_cell: Option<u64>,
    pub show_heatmap: bool,
    pub show_markers: bool,
    pub show_spike_halos: bool,
    pub show_alerts: bool,
    pub show_graticule: bool,
    pub show_labels: bool,
    /// Dim everything outside the selected cell.
    pub focus_selection: bool,
    /// Resolves the hovered point to a country, for border emphasis and the
    /// label cache.
    pub countries: &'a CountryIndex,
}

pub struct MapView {
    pub viewport: Option<MapViewport>,
    pub basemap: BasemapLayer,
    pub heatmap: HeatmapLayer,
    /// NOAA/NWS weather alerts. A layer of its own so weather never reads as
    /// unrest (docs/VISUALIZATION.md V3 item 8).
    pub alerts: AlertLayer,
    pub markers: MarkerLayer,
    pub spike_halos: HaloLayer,
    /// Rows behind the marker layer, indexed by `MarkerInput::source_index`.
    pub marker_rows: Vec<EventPoint>,
    pub style: MapStyle,
    flight: Option<Flight>,
    /// Country labels, laid out **once** and blitted thereafter. Text layout
    /// is the expensive part and none of it depends on the viewport, so doing
    /// it per frame would be the text equivalent of re-tessellating a mesh
    /// every frame (docs/VISUALIZATION.md perf guardrail).
    labels: Vec<MapLabel>,
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
            alerts: AlertLayer::empty(),
            markers: MarkerLayer::new(Vec::new()),
            spike_halos: HaloLayer::new(Vec::new()),
            marker_rows: Vec::new(),
            style,
            flight: None,
            labels: Vec::new(),
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
    pub fn show(&mut self, ui: &mut Ui, inputs: &MapInputs<'_>) -> MapActions {
        let selected_cell = inputs.selected_cell;
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

        // The hovered country drives the border hierarchy; a selection holds
        // the emphasis when the cursor leaves the map, so clicking a region
        // doesn't make its outline flicker away.
        let emphasis = response
            .hover_pos()
            .map(|h| {
                let local = h - rect.min;
                let (lon, lat) = vp.unproject(local.x, local.y);
                (wrap_lon(lon), lat)
            })
            .or_else(|| selected_cell.and_then(|c| geo_utils::cell_center_lonlat(c).ok()))
            .and_then(|(lon, lat)| inputs.countries.country_at(lon, lat))
            .map(|c| c.iso_a3.clone());

        // --- layers (background → grid → land → heat → alerts → markers) ---
        painter.rect_filled(rect, 0.0, self.style.background);
        // Under the land fill: the graticule is a backdrop, not an overlay.
        if inputs.show_graticule {
            GraticuleLayer::paint(&painter, &aff, rect, &self.style);
        }
        self.basemap.paint(
            &painter,
            &aff,
            rect.width(),
            &self.style,
            emphasis.as_deref(),
        );
        if inputs.show_heatmap {
            self.heatmap.paint(&painter, &aff, rect.width());
        }
        // Above the heatmap so the alert tint is visibly *on* the shading it
        // shares a cell with, below the markers so it never buries a point.
        if inputs.show_alerts {
            self.alerts.paint(&painter, &aff, rect.width(), &self.style);
        }
        if inputs.show_markers {
            self.markers
                .paint(&painter, &aff, rect.width(), &self.style);
        }
        if inputs.show_spike_halos && !self.spike_halos.is_empty() {
            let time_secs = ui.input(|i| i.time);
            self.spike_halos
                .paint(&painter, &aff, rect.width(), &self.style, time_secs);
            // Keep the pulse animating; bounded to a slow tick and only
            // while halos are actually shown.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Focus dimming covers the data layers but not the labels or the
        // selection outline, so the region being focused on stays legible.
        if inputs.focus_selection
            && let Some(cell) = selected_cell
        {
            self.dim_outside_cell(&painter, &aff, rect, cell);
        }
        if inputs.show_labels {
            self.ensure_labels(&painter, inputs.countries);
            self.draw_labels(&painter, &aff, rect);
        }
        if let Some(cell) = selected_cell {
            self.draw_cell_outline(&painter, &aff, rect.width(), cell);
        }

        // --- hover tooltip (custom-painted; no per-frame layout churn) ---
        let mut actions = MapActions::default();
        if let Some(hover) = response.hover_pos() {
            if inputs.show_markers
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

    /// Lay out the country labels once. Text and font never change, so this
    /// runs on the first labelled frame and never again.
    fn ensure_labels(&mut self, painter: &egui::Painter, countries: &CountryIndex) {
        if !self.labels.is_empty() || countries.is_empty() {
            return;
        }
        let font = FontId::proportional(LABEL_FONT_PX);
        self.labels = countries
            .iter_with_extent()
            .map(|(info, (lon, lat), extent)| MapLabel {
                lon,
                lat,
                extent,
                galley: painter.layout_no_wrap(
                    info.name.clone(),
                    font.clone(),
                    self.style.label_color,
                ),
            })
            .collect();
        // Biggest first, so when two labels collide the one that survives is
        // the one a reader is more likely to be orienting by.
        self.labels
            .sort_by(|a, b| b.extent.total_cmp(&a.extent).then(a.lon.total_cmp(&b.lon)));
    }

    /// Blit the cached galleys, dropping any that would overlap one already
    /// placed. Greedy and O(placed) per candidate, with both ends capped by
    /// `MAX_LABELS` — no layout, no allocation of new text per frame.
    fn draw_labels(&self, painter: &egui::Painter, aff: &geo_utils::Affine, rect: Rect) {
        let offsets = renderer::visible_world_offsets(aff, rect.width());
        let mut placed: Vec<Rect> = Vec::with_capacity(MAX_LABELS);
        for label in &self.labels {
            if placed.len() >= MAX_LABELS {
                break;
            }
            for &offset in &offsets {
                let (x, y) = aff.apply(label.lon + offset, label.lat);
                let at = Rect::from_center_size(Pos2::new(x, y), label.galley.size());
                if !label_fits(&placed, at, rect) {
                    continue;
                }
                painter.galley(at.min, label.galley.clone(), self.style.label_color);
                placed.push(at);
            }
        }
    }

    /// Wash everything outside the selected cell's screen bounding box.
    ///
    /// The box, not the hexagon: four rectangles are four shapes a frame,
    /// whereas masking to the exact cell would mean building a "world minus
    /// hexagon" polygon on every viewport change. The cell's own outline is
    /// drawn afterwards, so the exact selection is never ambiguous.
    fn dim_outside_cell(
        &self,
        painter: &egui::Painter,
        aff: &geo_utils::Affine,
        rect: Rect,
        cell: u64,
    ) {
        let Ok(ring) = geo_utils::cell_boundary_lonlat(cell) else {
            return;
        };
        // Use the world copy nearest the viewport center, so a cell that also
        // appears in a wrapped copy doesn't dim across the seam.
        let center_x = rect.center().x;
        let mut best: Option<(f32, Rect)> = None;
        for offset in renderer::visible_world_offsets(aff, rect.width()) {
            let mut bounds: Option<Rect> = None;
            for &(lon, lat) in &ring {
                let (x, y) = aff.apply(lon + offset, lat);
                let p = Pos2::new(x, y);
                bounds = Some(match bounds {
                    Some(b) => b.union(Rect::from_min_max(p, p)),
                    None => Rect::from_min_max(p, p),
                });
            }
            if let Some(b) = bounds {
                let d = (b.center().x - center_x).abs();
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, b));
                }
            }
        }
        let Some((_, focus)) = best else {
            return;
        };
        for band in focus_bands(rect, focus.intersect(rect)) {
            if band.width() > 0.0 && band.height() > 0.0 {
                painter.rect_filled(band, 0.0, self.style.focus_dim);
            }
        }
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
                "{} {} · confidence {:.0}%",
                row.volume_count,
                row.family.volume_unit().label(u64::from(row.volume_count)),
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

/// Whether a candidate label may be drawn: fully on screen, and clear of
/// every label already placed (plus [`LABEL_PAD_PX`] of breathing room).
fn label_fits(placed: &[Rect], at: Rect, bounds: Rect) -> bool {
    bounds.contains_rect(at) && {
        let padded = at.expand(LABEL_PAD_PX);
        !placed.iter().any(|p| p.intersects(padded))
    }
}

/// The four rectangles of `rect` that lie outside `focus` (top, bottom, left,
/// right). Their union is `rect \ focus` exactly — the focused region is never
/// covered, which is the property that keeps dimming from hiding the thing the
/// user selected.
fn focus_bands(rect: Rect, focus: Rect) -> [Rect; 4] {
    [
        Rect::from_min_max(rect.min, Pos2::new(rect.max.x, focus.min.y)),
        Rect::from_min_max(Pos2::new(rect.min.x, focus.max.y), rect.max),
        Rect::from_min_max(
            Pos2::new(rect.min.x, focus.min.y),
            Pos2::new(focus.min.x, focus.max.y),
        ),
        Rect::from_min_max(
            Pos2::new(focus.max.x, focus.min.y),
            Pos2::new(rect.max.x, focus.max.y),
        ),
    ]
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

    /// Dimming must never touch the selection. If a band overlapped the focus
    /// rect, turning focus mode on would darken the one region the user asked
    /// to look at.
    #[test]
    fn focus_bands_cover_everything_except_the_focus() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 60.0));
        let focus = Rect::from_min_max(Pos2::new(30.0, 20.0), Pos2::new(50.0, 40.0));
        let bands = focus_bands(rect, focus);
        for b in bands {
            assert!(
                !b.intersects(focus.shrink(0.01)),
                "band {b:?} covers the focus"
            );
        }
        // The bands tile the remainder: sample points outside the focus and
        // confirm each lands in some band.
        for (x, y) in [
            (5.0, 5.0),
            (40.0, 5.0),
            (95.0, 55.0),
            (5.0, 30.0),
            (95.0, 30.0),
            (40.0, 55.0),
        ] {
            let p = Pos2::new(x, y);
            assert!(bands.iter().any(|b| b.contains(p)), "({x}, {y}) undimmed");
        }
        // Area accounting: the four bands must sum to rect minus focus.
        let area = |r: Rect| r.width().max(0.0) * r.height().max(0.0);
        let total: f32 = bands.iter().map(|b| area(*b)).sum();
        assert!((total - (area(rect) - area(focus))).abs() < 1e-3, "{total}");
    }

    /// A cell filling the whole viewport dims nothing — the degenerate case
    /// where every band collapses to zero width or height.
    #[test]
    fn focus_bands_are_empty_when_the_cell_fills_the_view() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 60.0));
        for b in focus_bands(rect, rect) {
            assert!(b.width() <= 0.0 || b.height() <= 0.0, "{b:?}");
        }
    }

    #[test]
    fn labels_are_dropped_when_they_collide_or_leave_the_view() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(200.0, 100.0));
        let first = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(40.0, 12.0));
        assert!(label_fits(&[], first, bounds));
        // Overlapping outright.
        assert!(!label_fits(&[first], first, bounds));
        // Adjacent but inside the padding gap.
        let snug = Rect::from_min_size(
            Pos2::new(first.max.x + LABEL_PAD_PX * 0.5, 10.0),
            Vec2::new(40.0, 12.0),
        );
        assert!(!label_fits(&[first], snug, bounds));
        // Clear of the padding.
        let clear = Rect::from_min_size(
            Pos2::new(first.max.x + LABEL_PAD_PX * 2.0 + 1.0, 10.0),
            Vec2::new(40.0, 12.0),
        );
        assert!(label_fits(&[first], clear, bounds));
        // Partly off screen: a half-drawn country name is worse than none.
        let clipped = Rect::from_min_size(Pos2::new(180.0, 10.0), Vec2::new(40.0, 12.0));
        assert!(!label_fits(&[], clipped, bounds));
    }

    #[test]
    fn fly_to_without_a_viewport_is_a_no_op() {
        let mut view = MapView::new(test_basemap(), MapStyle::default());
        view.fly_to(10.0, 10.0);
        assert!(!view.advance_flight(1.0 / 60.0));
    }
}
