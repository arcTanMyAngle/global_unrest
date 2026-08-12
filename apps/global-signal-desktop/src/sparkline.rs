//! Region history sparkline: one selected cell's 6-h record counts over the
//! trailing baseline window, with its own trailing median drawn underneath as
//! a band. This is what makes the `spike` score component *visible* — the bar
//! that clears the band is the anomaly the score is reporting.
//!
//! Pure epaint rects/lines like `timeline_strip`, bounded at
//! `BASELINE_WINDOW_DAYS × 4` ≈ 112 slots, so it is trivial per frame and
//! never tessellates geometry (docs/VISUALIZATION.md perf guardrail).

use core_types::BUCKET_SECS;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use renderer::MapStyle;
use storage::RegionHistoryPoint;

const HEIGHT: f32 = 46.0;
const V_PAD: f32 = 3.0;

/// Discrete event records. Deliberately not one of the kind colors: this bar
/// aggregates all four kinds, so borrowing any single kind's hue would
/// misattribute the count. Public so the inspector's legend matches exactly.
pub const EVENTS_FILL: Color32 = Color32::from_rgb(222, 206, 164);
/// The "at or below normal" band under each bucket's own trailing median.
const BAND_FILL: Color32 = Color32::from_rgba_premultiplied(70, 84, 110, 120);
pub const BAND_LINE: Color32 = Color32::from_rgb(126, 146, 178);
/// Bottom tick for a bucket whose baseline is cold-start — no band is drawn
/// there, because there is no median to claim.
const COLD_TICK: Color32 = Color32::from_rgb(96, 88, 74);
const EMPTY_TEXT_COLOR: Color32 = Color32::from_rgb(148, 155, 168);

/// Paint the sparkline for `points` across the slot span `[from, until)`.
///
/// The span is passed in rather than derived from `points` so that buckets
/// with no records occupy their real position and read as gaps — deriving the
/// axis from present rows only would silently close them up and turn a quiet
/// fortnight into a dense one.
pub fn show(
    ui: &mut Ui,
    width: f32,
    points: &[RegionHistoryPoint],
    span: (i64, i64),
    style: &MapStyle,
) {
    let (response, painter) =
        ui.allocate_painter(Vec2::new(width.max(1.0), HEIGHT), Sense::hover());
    let rect = response.rect;

    let slots = ((span.1 - span.0) / BUCKET_SECS).max(1);
    if points.is_empty() {
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            "no history for this region yet",
            FontId::proportional(11.0),
            EMPTY_TEXT_COLOR,
        );
        return;
    }

    // Sub-pixel columns still have to paint something, or a busy region's
    // bars would vanish at narrow panel widths.
    let col_w = (rect.width() / slots as f32).max(1.0);
    let plot_h = HEIGHT - 2.0 * V_PAD;
    let base_y = rect.max.y - V_PAD;
    // One scale for records and baseline: they are the same quantity, so
    // drawing them on separate scales would make the comparison a lie.
    let max_v = points
        .iter()
        .map(|p| (p.records() as f32).max(p.baseline))
        .fold(1.0f32, f32::max);
    let x_of = |bucket_start: i64| {
        let i = (bucket_start - span.0) / BUCKET_SECS;
        rect.min.x + i as f32 * col_w
    };
    let y_of = |v: f32| base_y - (v / max_v) * plot_h;

    // Band first, so bars read as sitting on top of "normal".
    for p in points {
        let x0 = x_of(p.bucket_start);
        let x1 = x0 + col_w;
        if p.spike_cold_start {
            // No baseline behind this bucket: mark the absence rather than
            // drawing a band at whatever the stored value happens to be.
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, base_y - 1.0), Pos2::new(x1, base_y)),
                0.0,
                COLD_TICK,
            );
            continue;
        }
        let y = y_of(p.baseline);
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, base_y)),
            0.0,
            BAND_FILL,
        );
        painter.add(Shape::line_segment(
            [Pos2::new(x0, y), Pos2::new(x1, y)],
            Stroke::new(1.0, BAND_LINE),
        ));
    }

    // Bars: total records, split so the composition stays visible. Total is
    // the right height here — it is exactly the quantity `baseline` is a
    // median of and `spike_score` is computed from — but events and attention
    // keep distinct fills so the bar is never read as one undifferentiated
    // "activity" number.
    for p in points {
        if p.records() == 0 {
            continue;
        }
        let x0 = x_of(p.bucket_start);
        let x1 = x0 + col_w;
        let top = y_of(p.records() as f32);
        let split = y_of(p.attention_count as f32);
        if p.attention_count > 0 {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, split), Pos2::new(x1, base_y)),
                0.0,
                style.marker_attention,
            );
        }
        if p.event_count > 0 {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x1, split)),
                0.0,
                EVENTS_FILL,
            );
        }
    }

    painter.text(
        rect.right_top(),
        Align2::RIGHT_TOP,
        format!("peak {max_v:.0}/6 h"),
        FontId::proportional(9.0),
        EMPTY_TEXT_COLOR,
    );
}

/// Row-sized sparkline for a top-movers entry: dense 6-h record counts across
/// the *visible window* (not 28 days — this one comes from the already-loaded
/// buckets, so the panel stays query-free). Each bar is scaled to the series'
/// own peak, so it shows shape, never magnitude across rows.
pub fn mini(ui: &mut Ui, width: f32, series: &[u32]) {
    const MINI_HEIGHT: f32 = 14.0;
    let (response, painter) =
        ui.allocate_painter(Vec2::new(width.max(1.0), MINI_HEIGHT), Sense::hover());
    if series.is_empty() {
        return;
    }
    let rect = response.rect;
    let col_w = (rect.width() / series.len() as f32).max(1.0);
    let peak = (series.iter().copied().max().unwrap_or(1).max(1)) as f32;
    for (i, &v) in series.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let x0 = rect.min.x + i as f32 * col_w;
        let h = (v as f32 / peak) * rect.height();
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, rect.max.y - h),
                Pos2::new(x0 + col_w, rect.max.y),
            ),
            0.0,
            EVENTS_FILL,
        );
    }
}
