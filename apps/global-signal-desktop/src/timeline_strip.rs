//! Timeline histogram strip: replaces the bare time-window slider with a
//! stacked per-bucket histogram of things that happened (recorded events plus
//! official alerts), two thin line overlays each on its own scale — media
//! attention and aggregate chatter — a translucent window brush, and a
//! playhead. Pure epaint drawing (rects/lines), no meshes — ~a few hundred
//! buckets at most, trivial per frame.
//!
//! The three lanes are in three different units (records, articles, posts) and
//! are never summed or drawn on a shared scale; that is the whole reason
//! attention and chatter are lines rather than more bar segments
//! (docs/SIGNAL_MODEL.md).

use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use renderer::MapStyle;

use crate::app::{HISTOGRAM_STACK_KINDS, HistogramBucket, Timeline};

const V_PAD: f32 = 3.0;
const BRUSH_COLOR: Color32 = Color32::from_rgba_premultiplied(110, 140, 220, 70);
const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(240, 240, 250);
const EMPTY_TEXT_COLOR: Color32 = Color32::from_rgb(148, 155, 168);

/// Paint the strip and handle drag/click-to-scrub. Returns `true` when the
/// window start changed (the caller should `mark_dirty()`).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    width: f32,
    height: f32,
    histogram: &[HistogramBucket],
    style: &MapStyle,
    timeline: &mut Timeline,
    window_len_buckets: i64,
    max_start: i64,
) -> bool {
    let (response, painter) = ui.allocate_painter(
        Vec2::new(width.max(1.0), height.max(1.0)),
        Sense::click_and_drag(),
    );
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
    let plot_h = height.max(1.0) - 2.0 * V_PAD;
    let max_stack = histogram
        .iter()
        .map(|b| b.event_counts.iter().sum::<u32>())
        .max()
        .unwrap_or(0)
        .max(1) as f32;

    // Stacked bars: records and alerts only. Neither attention nor chatter is
    // ever mixed into this stack (docs/VISUALIZATION.md V1 item 1,
    // docs/SIGNAL_MODEL.md).
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

    // Attention and chatter: thin line overlays, each normalized against its
    // own maximum. Deliberately independent scales — a bucket where the two
    // lines meet says nothing, because articles and posts are not the same
    // quantity, and a shared scale would let post volume flatten coverage into
    // the baseline.
    let overlay = |value: fn(&HistogramBucket) -> u32, color: Color32| {
        let max = histogram.iter().map(value).max().unwrap_or(0);
        if max == 0 {
            // Nothing in this lane for the whole extent. Drawing a flat line
            // along the floor would read as "measured zero everywhere" for a
            // lane that may simply not be collecting.
            return;
        }
        let max = max as f32;
        let points: Vec<Pos2> = histogram
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let x = rect.min.x + (i as f32 + 0.5) * col_w;
                let t = value(b) as f32 / max;
                Pos2::new(x, rect.max.y - V_PAD - t * plot_h)
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.0, color)));
    };
    overlay(|b| b.attention_count, style.marker_attention);
    overlay(|b| b.chatter_count, style.marker_chatter);

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
