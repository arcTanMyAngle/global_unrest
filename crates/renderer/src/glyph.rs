//! Marker glyphs: the *shape* channel of the marker encoding.
//!
//! Markers carry two independent channels. Fill **color** stays the
//! [`core_types::EventKind`] palette (unchanged since M1); **shape** encodes
//! which live source reported the record. Before this, a Bluesky and a
//! Telegram observation were pixel-identical violet diamonds — both are
//! `NewsAttention` — so no screenshot could attribute a marker to its feed,
//! the gap every GUI verification round has had to close with a database
//! query instead (docs/VISUALIZATION.md V3 item 8).
//!
//! The unit corner sets are **equal-area on purpose**: marker size encodes
//! severity, so a triangle and a diamond at the same severity must read as the
//! same size. Sizing by half-extent instead would make shape leak into the
//! size channel and quietly corrupt the severity reading.

use core_types::SourceId;

/// Every glyph below is normalized to an area of 2 — the area of the original
/// half-extent-1 diamond, so marker sizing is unchanged by the shape channel.
/// `every_glyph_has_the_same_area` pins it.
///
/// Half-side of an equal-area square: `(2s)^2 = 2`, so `s = 1/sqrt(2)`.
const SQ: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Circumradius of an equal-area equilateral triangle:
/// `(3*sqrt(3)/4) * R^2 = 2`.
const TRI_R: f32 = 1.240_807;
/// `TRI_R * cos(30°)` and `TRI_R * sin(30°)` — the two base vertices.
const TRI_X: f32 = 1.074_57;
const TRI_Y: f32 = 0.620_403;

/// Marker outline shape, one per live source that can produce point markers.
///
/// Sources that are structurally region-only (NOAA, IODA — Admin1/Country
/// precision, never City/Exact) have no glyph of their own: they can never
/// reach the marker layer under the precision rendering contract. They map to
/// [`MarkerGlyph::Diamond`] only so the mapping is total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerGlyph {
    /// Curated event data (ACLED).
    Diamond,
    /// Machine-coded news events (GDELT Events).
    Square,
    /// Bluesky aggregate chatter.
    TriangleUp,
    /// Telegram aggregate chatter.
    TriangleDown,
}

impl MarkerGlyph {
    pub fn for_source(source: SourceId) -> Self {
        match source {
            SourceId::Acled => Self::Diamond,
            SourceId::Gdelt => Self::Square,
            SourceId::Bluesky => Self::TriangleUp,
            SourceId::Telegram => Self::TriangleDown,
            // Region-only sources (and fixtures, which never enter the desktop
            // runtime) never render as points; see the type-level note.
            SourceId::Noaa | SourceId::Ioda | SourceId::Fixtures => Self::Diamond,
        }
    }

    /// Corner offsets in glyph-local units, y **screen-down**, wound
    /// consistently so a triangle fan from vertex 0 is valid (all four glyphs
    /// are convex).
    pub fn unit_corners(self) -> &'static [[f32; 2]] {
        match self {
            Self::Diamond => &[[0.0, -1.0], [1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]],
            Self::Square => &[[-SQ, -SQ], [SQ, -SQ], [SQ, SQ], [-SQ, SQ]],
            Self::TriangleUp => &[[0.0, -TRI_R], [TRI_X, TRI_Y], [-TRI_X, TRI_Y]],
            Self::TriangleDown => &[[0.0, TRI_R], [-TRI_X, -TRI_Y], [TRI_X, -TRI_Y]],
        }
    }

    /// Which feed this shape stands for, for the legend.
    pub fn source_label(self) -> &'static str {
        match self {
            Self::Diamond => "ACLED (curated conflict/protest events)",
            Self::Square => "GDELT (machine-coded news events)",
            Self::TriangleUp => "Bluesky (aggregate chatter)",
            Self::TriangleDown => "Telegram (aggregate chatter)",
        }
    }

    /// Every glyph, in legend order.
    pub const ALL: [Self; 4] = [
        Self::Diamond,
        Self::Square,
        Self::TriangleUp,
        Self::TriangleDown,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The area every glyph is normalized to; see the constants above.
    const UNIT_AREA: f32 = 2.0;

    fn shoelace(corners: &[[f32; 2]]) -> f32 {
        let n = corners.len();
        let mut sum = 0.0;
        for i in 0..n {
            let (a, b) = (corners[i], corners[(i + 1) % n]);
            sum += a[0] * b[1] - b[0] * a[1];
        }
        (sum / 2.0).abs()
    }

    /// Size is the severity channel. If shapes had different areas, severity
    /// would be unreadable across sources.
    #[test]
    fn every_glyph_has_the_same_area() {
        for g in MarkerGlyph::ALL {
            let a = shoelace(g.unit_corners());
            assert!(
                (a - UNIT_AREA).abs() < 1e-4,
                "{g:?} area {a} != {UNIT_AREA}"
            );
        }
    }

    #[test]
    fn corners_are_convex_and_fannable() {
        for g in MarkerGlyph::ALL {
            let c = g.unit_corners();
            assert!(c.len() >= 3, "{g:?} needs at least a triangle");
            let n = c.len();
            // Every cross product of consecutive edges must share one sign.
            let mut signs = c.iter().enumerate().map(|(i, _)| {
                let (p, q, r) = (c[i], c[(i + 1) % n], c[(i + 2) % n]);
                let (ux, uy) = (q[0] - p[0], q[1] - p[1]);
                let (vx, vy) = (r[0] - q[0], r[1] - q[1]);
                (ux * vy - uy * vx).signum()
            });
            let first = signs.next().unwrap();
            assert!(signs.all(|s| s == first), "{g:?} is not convex");
        }
    }

    #[test]
    fn the_two_chatter_sources_get_different_shapes() {
        // The whole point of the shape channel: a screenshot must be able to
        // tell these two apart, and neither may collide with an event source.
        let bs = MarkerGlyph::for_source(SourceId::Bluesky);
        let tg = MarkerGlyph::for_source(SourceId::Telegram);
        assert_ne!(bs, tg);
        for other in [SourceId::Acled, SourceId::Gdelt] {
            let o = MarkerGlyph::for_source(other);
            assert_ne!(o, bs);
            assert_ne!(o, tg);
        }
    }
}
