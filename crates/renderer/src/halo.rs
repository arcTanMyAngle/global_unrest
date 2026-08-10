//! Spike halos: slowly pulsing rings at the centroid of cells whose
//! `spike_score` clears `analytics::weights::SPIKE_HALO_THRESHOLD` — "what
//! is anomalous *right now*" (docs/VISUALIZATION.md V1 item 2).
//!
//! Deliberately **not** a [`crate::GeoMesh`]/[`crate::MeshCache`] layer: the
//! cell list is small (capped by `SPIKE_HALO_MAX_CELLS`) and the pulse needs
//! to vary every frame, so this draws plain epaint circle strokes each
//! frame — the same cheap-uncached-shape precedent as the basemap's country
//! border polylines.

use egui::{Painter, Pos2, Shape, Stroke};
use geo_utils::Affine;

use crate::{MapStyle, visible_world_offsets};

const MIN_RADIUS_PX: f32 = 6.0;
const MAX_RADIUS_PX: f32 = 14.0;
const PULSE_HZ: f64 = 0.5;
const STROKE_WIDTH: f32 = 1.5;

pub struct HaloLayer {
    /// (h3_cell, spike_score) — already filtered/capped by
    /// `analytics::spike_halo_cells`.
    cells: Vec<(u64, f32)>,
}

impl HaloLayer {
    pub fn new(cells: Vec<(u64, f32)>) -> Self {
        Self { cells }
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn paint(
        &self,
        painter: &Painter,
        aff: &Affine,
        screen_w: f32,
        style: &MapStyle,
        time_secs: f64,
    ) {
        if self.cells.is_empty() {
            return;
        }
        let radius = pulse_radius(time_secs);
        for offset in visible_world_offsets(aff, screen_w) {
            for &(cell, score) in &self.cells {
                let Ok((lon, lat)) = geo_utils::cell_center_lonlat(cell) else {
                    continue;
                };
                let (x, y) = aff.apply(lon + offset, lat);
                let color = style
                    .halo_color
                    .gamma_multiply(score_alpha(score) as f32 / 255.0);
                painter.add(Shape::circle_stroke(
                    Pos2::new(x, y),
                    radius,
                    Stroke::new(STROKE_WIDTH, color),
                ));
            }
        }
    }
}

/// Ring radius at `time_secs`, oscillating between `MIN_RADIUS_PX` and
/// `MAX_RADIUS_PX` at `PULSE_HZ`.
pub fn pulse_radius(time_secs: f64) -> f32 {
    let phase = (time_secs * PULSE_HZ * std::f64::consts::TAU).sin() as f32 * 0.5 + 0.5;
    MIN_RADIUS_PX + (MAX_RADIUS_PX - MIN_RADIUS_PX) * phase
}

/// Ring opacity proportional to spike score (already `>= threshold`, i.e.
/// well above neutral); never fully transparent so a halo at the threshold
/// is still visible.
pub fn score_alpha(score: f32) -> u8 {
    (score.clamp(0.0, 1.0) * 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_radius_stays_within_bounds() {
        for i in 0..40 {
            let t = f64::from(i) * 0.1;
            let r = pulse_radius(t);
            assert!((MIN_RADIUS_PX..=MAX_RADIUS_PX).contains(&r), "t={t} r={r}");
        }
    }

    #[test]
    fn pulse_radius_is_periodic() {
        let period = 1.0 / PULSE_HZ;
        let a = pulse_radius(1.23);
        let b = pulse_radius(1.23 + period);
        assert!((a - b).abs() < 1e-4);
    }

    #[test]
    fn score_alpha_scales_with_score_and_clamps() {
        assert_eq!(score_alpha(0.0), 0);
        assert_eq!(score_alpha(1.0), 255);
        assert_eq!(score_alpha(2.0), 255, "scores above 1.0 must clamp");
        let mid = score_alpha(0.8);
        assert!(mid > 190 && mid < 210, "got {mid}");
    }

    #[test]
    fn layer_reports_empty() {
        assert!(HaloLayer::new(Vec::new()).is_empty());
        assert!(!HaloLayer::new(vec![(1, 0.9)]).is_empty());
    }
}
