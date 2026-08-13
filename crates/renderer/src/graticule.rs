//! Graticule: meridians and parallels, for orientation on a basemap with no
//! tiles under it (docs/VISUALIZATION.md V3 item 9).
//!
//! Equirectangular is affine in lon/lat, so a meridian is exactly a vertical
//! screen line and a parallel exactly a horizontal one — each is a single
//! two-point segment, no projection sampling and nothing to cache. The spacing
//! adapts to zoom so the grid never crowds, and only lines that fall inside
//! the viewport are generated, which is what bounds the per-frame cost.

use egui::{Painter, Pos2, Rect, Shape, Stroke};
use geo_utils::Affine;

use crate::MapStyle;

/// Candidate spacings in degrees, coarsest last. The finest spacing whose
/// on-screen gap clears [`MIN_SPACING_PX`] wins.
const STEPS_DEG: [f64; 6] = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0];

/// Smallest on-screen gap between grid lines. Below this the graticule stops
/// being orientation and starts being texture.
const MIN_SPACING_PX: f64 = 70.0;

/// Hard cap per axis. Unreachable given [`MIN_SPACING_PX`] and a sane viewport
/// — a guardrail against a degenerate affine, not an expected limit.
const MAX_LINES_PER_AXIS: usize = 64;

/// Width of an ordinary grid line, and of the equator / prime meridian.
const LINE_WIDTH: f32 = 0.8;
const MAJOR_WIDTH: f32 = 1.0;

pub struct GraticuleLayer;

impl GraticuleLayer {
    /// Spacing in degrees for a given scale.
    pub fn step_for(deg_per_px: f64) -> f64 {
        STEPS_DEG
            .into_iter()
            .find(|s| s / deg_per_px >= MIN_SPACING_PX)
            .unwrap_or(*STEPS_DEG.last().expect("STEPS_DEG is non-empty"))
    }

    /// Draw the grid covering `rect`. `aff` must already carry the rect origin
    /// (as it does everywhere else in the map widget).
    pub fn paint(painter: &Painter, aff: &Affine, rect: Rect, style: &MapStyle) {
        if aff.a.abs() < f64::EPSILON || aff.c.abs() < f64::EPSILON {
            return;
        }
        let deg_per_px = 1.0 / aff.a.abs();
        let step = Self::step_for(deg_per_px);

        let lon_at = |x: f32| (f64::from(x) - aff.b) / aff.a;
        let lat_at = |y: f32| (f64::from(y) - aff.d) / aff.c;
        let x_at = |lon: f64| (aff.a * lon + aff.b) as f32;
        let y_at = |lat: f64| (aff.c * lat + aff.d) as f32;

        let minor = Stroke::new(LINE_WIDTH, style.graticule);
        let major = Stroke::new(MAJOR_WIDTH, style.graticule_major);

        // Meridians. The range is deliberately *not* clamped to ±180: past the
        // antimeridian the viewport shows a world copy, and its grid has to
        // continue across the seam or the copy reads as a different map.
        let (lon0, lon1) = (lon_at(rect.min.x), lon_at(rect.max.x));
        let (lon0, lon1) = (lon0.min(lon1), lon0.max(lon1));
        for k in ticks(lon0, lon1, step) {
            let lon = k * step;
            let x = x_at(lon);
            // The prime meridian and its ±360° copies all mark longitude 0.
            let is_major = (lon % 360.0).abs() < f64::EPSILON;
            painter.add(Shape::LineSegment {
                points: [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                stroke: if is_major { major } else { minor },
            });
        }

        // Parallels, clamped to the poles — there is no map outside them.
        let (lat0, lat1) = (lat_at(rect.max.y), lat_at(rect.min.y));
        let (lat0, lat1) = (lat0.min(lat1).max(-90.0), lat0.max(lat1).min(90.0));
        for k in ticks(lat0, lat1, step) {
            let lat = k * step;
            let y = y_at(lat);
            let is_major = lat.abs() < f64::EPSILON;
            painter.add(Shape::LineSegment {
                points: [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
                stroke: if is_major { major } else { minor },
            });
        }
    }
}

/// Multiples of `step` inside `[lo, hi]`, as integer tick indices, capped.
fn ticks(lo: f64, hi: f64, step: f64) -> impl Iterator<Item = f64> {
    let k0 = (lo / step).ceil();
    let k1 = (hi / step).floor();
    let count = ((k1 - k0 + 1.0).max(0.0) as usize).min(MAX_LINES_PER_AXIS);
    (0..count).map(move |i| k0 + i as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_utils::MapViewport;

    #[test]
    fn spacing_gets_finer_as_the_view_zooms_in_and_never_crowds() {
        let mut previous = f64::INFINITY;
        for deg_per_px in [1.0, 0.225, 0.08, 0.02, 0.005, 0.002] {
            let step = GraticuleLayer::step_for(deg_per_px);
            assert!(step <= previous, "step grew while zooming in");
            previous = step;
            // Either the gap clears the minimum, or we are already at the
            // coarsest spacing there is and cannot back off further.
            let gap = step / deg_per_px;
            assert!(
                gap >= MIN_SPACING_PX || step == STEPS_DEG[STEPS_DEG.len() - 1],
                "deg_per_px={deg_per_px} gives a {gap}px gap at step {step}"
            );
        }
    }

    #[test]
    fn tick_range_is_inclusive_and_capped() {
        let t: Vec<f64> = ticks(-31.0, 31.0, 30.0).collect();
        assert_eq!(t, vec![-1.0, 0.0, 1.0]);
        // Empty when no multiple falls inside.
        assert_eq!(ticks(1.0, 29.0, 30.0).count(), 0);
        // A degenerate range cannot blow up the frame.
        assert!(ticks(-1e9, 1e9, 1.0).count() <= MAX_LINES_PER_AXIS);
    }

    /// The whole perf claim: line count is bounded by the *viewport*, not by
    /// how far the user has zoomed in.
    #[test]
    fn line_count_stays_bounded_at_every_zoom() {
        let (screen_w, screen_h) = (1600.0f32, 900.0f32);
        // The map widget never lets the view go wider than one whole world, so
        // that clamp is the real ceiling on deg_per_px, not MAX_DEG_PER_PX.
        let widest = 360.0 / f64::from(screen_w);
        for deg_per_px in [widest, 0.08, 0.02, 0.002, geo_utils::MIN_DEG_PER_PX] {
            let vp = MapViewport {
                center_lon: 0.0,
                center_lat: 0.0,
                deg_per_px,
                screen_w,
                screen_h,
            };
            let aff = vp.affine();
            let step = GraticuleLayer::step_for(deg_per_px);
            let lon_span = f64::from(screen_w) * deg_per_px;
            let lat_span = f64::from(screen_h) * deg_per_px;
            let meridians = ticks(-lon_span / 2.0, lon_span / 2.0, step).count();
            let parallels = ticks(-lat_span / 2.0, lat_span / 2.0, step).count();
            assert!(
                meridians <= MAX_LINES_PER_AXIS && parallels <= MAX_LINES_PER_AXIS,
                "deg_per_px={deg_per_px}: {meridians} meridians, {parallels} parallels"
            );
            // A full-world view is the busiest case, and even it stays sparse.
            let expected_max = (lon_span / step).ceil() as usize + 1;
            assert!(meridians <= expected_max);
            assert!(aff.a > 0.0 && aff.c < 0.0, "affine orientation changed");
        }
        // Zoomed all the way in, the grid is a handful of lines, not a mesh.
        let step = GraticuleLayer::step_for(geo_utils::MIN_DEG_PER_PX);
        let span = f64::from(screen_w) * geo_utils::MIN_DEG_PER_PX;
        assert!(
            ticks(0.0, span, step).count() <= 8,
            "step {step} for {span}°"
        );
    }
}
