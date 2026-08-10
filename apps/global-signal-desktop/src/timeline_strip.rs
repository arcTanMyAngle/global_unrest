//! Timeline histogram strip: replaces the bare time-window slider with a
//! stacked per-bucket event histogram (discrete kinds only), a thin
//! attention-count line overlay on its own scale, a translucent window
//! brush, and a playhead. Pure epaint drawing (rects/lines), no meshes —
//! ~a few hundred buckets at most, trivial per frame.

use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use renderer::MapStyle;

use crate::app::{HISTOGRAM_STACK_KINDS, HistogramBucket, Timeline};

const HEIGHT: f32 = 40.0;
const V_PAD: f32 = 3.0;
const BRUSH_COLOR: Color32 = Color32::from_rgba_premultiplied(110, 140, 220, 70);
const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(240, 240, 250);
const ATTENTION_LINE_COLOR: Color32 = Color32::from_rgb(186, 130, 255);
const EMPTY_TEXT_COLOR: Color32 = Color32::from_rgb(148, 155, 168);

/// Paint the strip and handle drag/click-to-scrub. Returns `true` when the
/// window start changed (the caller should `mark_dirty()`).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    width: f32,
    histogram: &[HistogramBucket],
    style: &MapStyle,
    timeline: &mut Timeline,
    window_len_buckets: i64,
    max_start: i64,
) -> bool {
    let (response, painter) =
        ui.allocate_painter(Vec2::new(width.max(1.0), HEIGHT), Sense::click_and_drag());
    let rect = response.rect;
    let total = histogram.len() as i64;

    if histogram.is_empty() {
        painter.text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            "no data yet",
            egui::FontId::proportional(11.0),
            EMPTY_TEXT_COLOR,
        );
        return false;
    }

    let col_w = rect.width() / total as f32;
    let plot_h = HEIGHT - 2.0 * V_PAD;
    let max_stack = histogram
        .iter()
        .map(|b| b.event_counts.iter().sum::<u32>())
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let max_attention = histogram
        .iter()
        .map(|b| b.attention_count)
        .max()
        .unwrap_or(0)
        .max(1) as f32;

    // Stacked bars, discrete kinds only — attention is never mixed into this
    // stack (docs/VISUALIZATION.md V1 item 1 / CLAUDE.md's attention/event
    // separation).
    for (i, b) in histogram.iter().enumerate() {
        let x0 = rect.min.x + i as f32 * col_w;
        let mut y = rect.max.y - V_PAD;
        for (slot, &kind) in HISTOGRAM_STACK_KINDS.iter().enumerate() {
            let count = b.event_counts[slot];
            if count == 0 {
                continue;
            }
            let h = (count as f32 / max_stack) * plot_h;
            let bar = Rect::from_min_max(Pos2::new(x0, y - h), Pos2::new(x0 + col_w, y));
            painter.rect_filled(bar, 0.0, style.marker_color(kind));
            y -= h;
        }
    }

    // Attention: thin line overlay on its own scale.
    let points: Vec<Pos2> = histogram
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let x = rect.min.x + (i as f32 + 0.5) * col_w;
            let t = b.attention_count as f32 / max_attention;
            Pos2::new(x, rect.max.y - V_PAD - t * plot_h)
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(1.0, ATTENTION_LINE_COLOR)));

    // Current-window brush.
    let bx0 = rect.min.x + timeline.start_bucket as f32 * col_w;
    let bx1 = rect.min.x + (timeline.start_bucket + window_len_buckets).min(total) as f32 * col_w;
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(bx0, rect.min.y), Pos2::new(bx1, rect.max.y)),
        0.0,
        BRUSH_COLOR,
    );

    // Playhead (window start).
    let px = rect.min.x + timeline.start_bucket as f32 * col_w;
    painter.add(Shape::line_segment(
        [Pos2::new(px, rect.min.y), Pos2::new(px, rect.max.y)],
        Stroke::new(1.5, PLAYHEAD_COLOR),
    ));

    let mut changed = false;
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let idx = ((pos.x - rect.min.x) / col_w).floor() as i64;
        let start = idx.clamp(0, max_start.max(0));
        if start != timeline.start_bucket {
            timeline.start_bucket = start;
            changed = true;
        }
    }
    changed
}
