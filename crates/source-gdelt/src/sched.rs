//! Client-side politeness and fetch scheduling for the live GDELT loop.
//!
//! GDELT is free with attribution and asks callers not to hammer it; its feeds
//! refresh every 15 minutes. This module holds the source-agnostic *policy* the
//! app's ingest worker drives (docs/PLAN.md §11 step 3):
//!
//! - [`request_limiter`] — a `governor` rate limiter spacing live requests.
//! - [`Backoff`] — exponential backoff with jitter that honors a server
//!   `Retry-After`, so a rate-limited or failing source is retried politely.
//! - [`until_next_slot`] — align the poll cadence to the feed's 15-minute
//!   boundaries (poll shortly after data lands, not continuously).
//! - [`backfill_windows`] — tile a historical range into fetch windows for a
//!   manual backfill.
//!
//! Everything here is deterministic and unit-tested: the limiter with a fake
//! clock, the rest as pure functions.

use std::time::Duration;

use chrono::{DateTime, Utc};
use core_types::{SourceError, TimeWindow};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

/// Minimum spacing between live GDELT requests (client-side politeness).
pub const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(5);

/// GDELT feeds refresh every 15 minutes; the online loop polls on this cadence.
pub const FEED_INTERVAL_SECS: i64 = 15 * 60;

/// Data usually lands a little after each 15-minute boundary; poll this many
/// seconds past the boundary so the freshest dump is available.
pub const FEED_LAG_SECS: i64 = 90;

/// Fraction of the delay added as (deterministic) jitter, to avoid aliasing
/// retries against the server's own windows.
const JITTER_FRACTION: f64 = 0.5;

/// A direct rate limiter that allows one request per [`MIN_REQUEST_INTERVAL`].
/// The worker calls `limiter.until_ready().await` before every live request.
pub fn request_limiter() -> DefaultDirectRateLimiter {
    let quota = Quota::with_period(MIN_REQUEST_INTERVAL).expect("non-zero request interval");
    RateLimiter::direct(quota)
}

/// Exponential backoff with a ceiling that honors a server `Retry-After`.
///
/// The worker calls [`Backoff::after_error`] when a fetch fails to learn how
/// long to wait before retrying, and [`Backoff::reset`] after a success.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            attempt: 0,
        }
    }

    /// Back to the first-retry delay (call after a successful fetch).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Number of retries scheduled so far (0 before the first error).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Delay to wait after `err`, then advance the attempt counter.
    ///
    /// A server `RateLimited { retry_after }` overrides the exponential
    /// schedule (still capped). `jitter01 ∈ [0, 1)` scales the added jitter
    /// deterministically — callers pass a random draw; tests pass a fixed
    /// value. Computed in `f64` seconds so large attempts saturate at the cap
    /// instead of overflowing.
    pub fn after_error(&mut self, err: &SourceError, jitter01: f64) -> Duration {
        let cap = self.cap.as_secs_f64();
        let base_secs = match err {
            SourceError::RateLimited {
                retry_after_secs: Some(secs),
            } => *secs as f64,
            // base · 2^attempt (attempt clamped so the power stays finite).
            _ => self.base.as_secs_f64() * 2f64.powi(self.attempt.min(30) as i32),
        };
        let capped = base_secs.min(cap);
        self.attempt = self.attempt.saturating_add(1);
        let jitter = capped * JITTER_FRACTION * jitter01.clamp(0.0, 1.0);
        Duration::from_secs_f64(capped + jitter)
    }
}

impl Default for Backoff {
    /// 30 s first retry, capped at 15 min (one feed interval).
    fn default() -> Self {
        Self::new(
            Duration::from_secs(30),
            Duration::from_secs(FEED_INTERVAL_SECS as u64),
        )
    }
}

/// Seconds from `now_epoch_secs` until the next `interval`-aligned slot plus
/// `lag` — i.e. shortly after the upcoming feed boundary. Always strictly
/// positive, so the loop never spins.
pub fn until_next_slot(now_epoch_secs: i64, interval_secs: i64, lag_secs: i64) -> i64 {
    let interval = interval_secs.max(1);
    let next_boundary = (now_epoch_secs.div_euclid(interval) + 1) * interval;
    (next_boundary + lag_secs - now_epoch_secs).max(1)
}

/// Tile `[from, to)` into consecutive [`TimeWindow`]s of at most `step`, for a
/// manual backfill. The final window is clamped to `to`. Empty if the range is
/// empty or `step` is zero.
pub fn backfill_windows(from: DateTime<Utc>, to: DateTime<Utc>, step: Duration) -> Vec<TimeWindow> {
    let step = match chrono::Duration::from_std(step) {
        Ok(d) if d > chrono::Duration::zero() && from < to => d,
        _ => return Vec::new(),
    };
    let mut windows = Vec::new();
    let mut start = from;
    while start < to {
        let end = (start + step).min(to);
        windows.push(TimeWindow::new(start, end));
        start = end;
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use governor::clock::FakeRelativeClock;

    #[test]
    fn limiter_spaces_requests_by_the_interval() {
        let clock = FakeRelativeClock::default();
        let quota = Quota::with_period(MIN_REQUEST_INTERVAL).unwrap();
        let lim = RateLimiter::direct_with_clock(quota, clock.clone());

        assert!(lim.check().is_ok(), "first request allowed");
        assert!(lim.check().is_err(), "immediate second request denied");
        clock.advance(MIN_REQUEST_INTERVAL);
        assert!(lim.check().is_ok(), "allowed again after the interval");
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(8));
        let err = SourceError::Http("boom".into());
        // 1, 2, 4, 8, then capped at 8 (no jitter).
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(1));
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(2));
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(4));
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(8));
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(8));
        assert_eq!(b.attempt(), 5);
        b.reset();
        assert_eq!(b.attempt(), 0);
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(1));
    }

    #[test]
    fn backoff_honors_retry_after_over_schedule() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(600));
        let err = SourceError::RateLimited {
            retry_after_secs: Some(120),
        };
        // Server's Retry-After wins over the exponential base.
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(120));
        // ...but is still capped.
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        assert_eq!(b.after_error(&err, 0.0), Duration::from_secs(60));
    }

    #[test]
    fn backoff_jitter_stays_within_half_of_delay() {
        let mut b = Backoff::new(Duration::from_secs(10), Duration::from_secs(600));
        let err = SourceError::Http("x".into());
        // Full jitter: 10 + 0.5·10 = 15.
        assert_eq!(b.after_error(&err, 1.0), Duration::from_secs(15));
    }

    #[test]
    fn until_next_slot_aligns_to_boundary_plus_lag() {
        // 12:00:00 UTC is exactly on a 15-min boundary.
        let boundary = Utc
            .with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
            .unwrap()
            .timestamp();
        // On the boundary: next slot is +15 min, +lag.
        assert_eq!(
            until_next_slot(boundary, FEED_INTERVAL_SECS, FEED_LAG_SECS),
            FEED_INTERVAL_SECS + FEED_LAG_SECS
        );
        // 5 minutes in: 10 min to the next boundary, + lag.
        assert_eq!(
            until_next_slot(boundary + 300, FEED_INTERVAL_SECS, FEED_LAG_SECS),
            600 + FEED_LAG_SECS
        );
        // Never returns a non-positive wait.
        assert!(until_next_slot(boundary, 0, 0) >= 1);
    }

    #[test]
    fn backfill_windows_tile_and_clamp() {
        let from = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 6, 1, 0, 40, 0).unwrap();
        let ws = backfill_windows(from, to, Duration::from_secs(15 * 60));
        // 0–15, 15–30, 30–40 (last clamped).
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].start, from);
        assert_eq!(ws[2].end, to);
        assert_eq!(ws[1].start, ws[0].end);
        // Degenerate inputs yield nothing.
        assert!(backfill_windows(to, from, Duration::from_secs(60)).is_empty());
        assert!(backfill_windows(from, to, Duration::ZERO).is_empty());
    }
}
