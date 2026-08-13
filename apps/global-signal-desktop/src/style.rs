//! Shared UI style constants and the painted legend swatches.
//!
//! **Why these are painted and not text.** egui's bundled fonts carry no
//! geometric-shape glyphs, so `◆`, `●` and `■` render as missing-glyph boxes.
//! They looked acceptable only because a *colored* box still reads as a color
//! chip — the moment a swatch needed a real shape (source identity, the halo
//! ring, the dashed alert outline) the glyphs stopped carrying any
//! information. Painting them fixes that without bundling a font, and it also
//! removes a whole class of drift: [`glyph_swatch`] draws from
//! [`renderer::MarkerGlyph::unit_corners`], the same corner table the marker
//! mesh is built from, so the legend cannot come to disagree with the map.

use egui::{Color32, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use renderer::MarkerGlyph;

/// Side of a legend swatch, in points. Matches the cap height of egui's
/// default body text so a swatch and its label sit on one line.
pub const SWATCH: f32 = 13.0;

/// Half-extent a glyph is drawn at inside a swatch, leaving a little air so
/// the widest glyph doesn't touch the label.
const GLYPH_HALF: f32 = SWATCH * 0.42;

/// Dashes drawn around a swatch-sized alert cell. Small enough to read as
/// dashed at this size; the map layer sizes its own dashes from the ring.
const SWATCH_DASHES: f32 = 8.0;

/// Allocate a swatch-sized rect and hand its painter to `draw`.
fn swatch(ui: &mut Ui, draw: impl FnOnce(&egui::Painter, Rect)) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SWATCH), Sense::hover());
    draw(ui.painter(), rect);
    response
}

/// A marker exactly as the map draws it: kind color, source shape.
pub fn glyph_swatch(ui: &mut Ui, glyph: MarkerGlyph, color: Color32) -> Response {
    swatch(ui, |painter, rect| {
        let c = rect.center();
        let points = glyph
            .unit_corners()
            .iter()
            .map(|p| c + Vec2::new(p[0], p[1]) * GLYPH_HALF)
            .collect();
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    })
}

/// A marker at the size the map would draw it for `severity`, so the legend's
/// size ramp is the real ramp rather than a redrawn approximation of it.
pub fn severity_swatch(ui: &mut Ui, severity: f32, color: Color32) -> Response {
    swatch(ui, |painter, rect| {
        let c = rect.center();
        let half = renderer::marker_half_px(severity);
        let points = MarkerGlyph::Diamond
            .unit_corners()
            .iter()
            .map(|p| c + Vec2::new(p[0], p[1]) * half)
            .collect();
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    })
}

/// Filled dot — live-source status.
pub fn dot_swatch(ui: &mut Ui, color: Color32) -> Response {
    swatch(ui, |painter, rect| {
        painter.circle_filled(rect.center(), SWATCH * 0.28, color);
    })
}

/// Open ring — a spike halo.
pub fn ring_swatch(ui: &mut Ui, color: Color32) -> Response {
    swatch(ui, |painter, rect| {
        painter.circle_stroke(rect.center(), SWATCH * 0.34, Stroke::new(1.5, color));
    })
}

/// Filled square — a shaded region rather than a point marker.
pub fn region_swatch(ui: &mut Ui, color: Color32) -> Response {
    swatch(ui, |painter, rect| {
        painter.rect_filled(rect.shrink(SWATCH * 0.18), 1.0, color);
    })
}

/// Tinted fill inside a dashed outline — the NOAA weather-alert encoding.
pub fn alert_swatch(ui: &mut Ui, fill: Color32, outline: Color32) -> Response {
    swatch(ui, |painter, rect| {
        let body = rect.shrink(SWATCH * 0.14);
        painter.rect_filled(body, 1.0, fill);
        let corners = [
            body.left_top(),
            body.right_top(),
            body.right_bottom(),
            body.left_bottom(),
            body.left_top(),
        ];
        let perimeter = body.width() * 2.0 + body.height() * 2.0;
        let period = perimeter / SWATCH_DASHES;
        let mut dashes = Vec::new();
        Shape::dashed_line_many(
            &corners,
            Stroke::new(1.0, outline),
            period * 0.55,
            period * 0.45,
            &mut dashes,
        );
        painter.extend(dashes);
    })
}
