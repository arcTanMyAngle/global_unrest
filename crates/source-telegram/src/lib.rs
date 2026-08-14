//! Telegram public-channel source — aggregate chatter volume, third
//! real-time source after IODA and Bluesky.
//!
//! Unlike Bluesky's Jetstream firehose, Telegram has no keyless public
//! stream: reading an arbitrary public channel's history requires a real
//! MTProto session (phone-number account, not a bot — Telegram's Bot API
//! only delivers messages from channels that channel's own admin has added
//! the bot to, which rules out third-party channels this project doesn't
//! control). The live path is feature-gated behind `live` like every other
//! optional source; see [`live::TelegramSource`] for the session/auth story
//! and `examples/login_setup.rs` for the one-time interactive login.
//!
//! **The ingest path is aggregate-only by construction, same as
//! `source-bluesky`.** Message text is handed straight to
//! [`chatter::ChatterAccumulator::observe`] and dropped in the same call;
//! nothing on that path returns message text, sender identity, or a message
//! URL to a caller, and its `(place, topic, window) -> count` boundary has
//! not moved.
//!
//! [`media`] is the one exception, added by explicit user direction: an
//! on-demand lookup that returns a caption, a post URL, and a channel
//! attribution for one named place, so a person can watch footage published
//! about it. Nothing there runs on a timer, nothing it returns is stored,
//! and it never reads a message's sender. That module's docs and
//! docs/SAFETY_AND_PRIVACY.md's "On-demand media lookup" section state the
//! full bounds — read both, plus hard rule 6, before changing either path.

#[cfg(feature = "live")]
pub mod file_session;
#[cfg(feature = "live")]
mod live;
pub mod media;
#[cfg(feature = "live")]
pub use file_session::FileSession;
#[cfg(feature = "live")]
pub use live::TelegramSource;

/// Per-channel bookkeeping for one history sweep.
///
/// Message text is borrowed only while it is passed to `acc`; this type keeps
/// only the channel-local high-water mark and count needed by the caller.
pub struct ChannelSweep {
    last_id: Option<i32>,
    newest: Option<i32>,
    scanned: u32,
}

impl ChannelSweep {
    pub fn new(last_id: Option<i32>) -> Self {
        Self {
            last_id,
            newest: last_id,
            scanned: 0,
        }
    }

    /// Fold one message into the current sweep.
    ///
    /// Blank text still advances the high-water mark, but is never handed to
    /// the accumulator.
    pub fn observe(
        &mut self,
        acc: &mut chatter::ChatterAccumulator,
        id: i32,
        text: &str,
        date: chrono::DateTime<chrono::Utc>,
    ) {
        self.scanned += 1;
        self.newest = Some(self.newest.map_or(id, |newest| newest.max(id)));
        if !text.trim().is_empty() {
            acc.observe(text, date);
        }
    }

    pub fn scanned(&self) -> u32 {
        self.scanned
    }

    /// Return an updated high-water mark only when this sweep advanced it.
    pub fn finish(&self) -> Option<i32> {
        match (self.last_id, self.newest) {
            (Some(last_id), Some(newest)) if newest > last_id => Some(newest),
            (None, Some(newest)) => Some(newest),
            _ => None,
        }
    }
}

/// Curated public-channel allowlist. Every handle here was live-verified
/// (its public `t.me/s/<handle>` preview checked for real, active, on-topic
/// content) during the session that added this source — not taken on a
/// blog's or search summary's word. Add to this list the same way: verify
/// before trusting, and prefer real subscriber counts/recent post dates over
/// a description alone.
///
/// Two things the 2026-08-13 widening pass learned the hard way, both worth
/// repeating on the next pass:
///
/// - **Read the timestamps, not just "posts are visible."** Three of the
///   original eight had gone quiet — one for over four years — while still
///   rendering a full page of old posts. A dormant channel costs a
///   `resolve_username` and a search call on every sweep and returns nothing.
///   Parse the preview's `<time datetime=…>` attributes and compare to today.
/// - **An empty `t.me/s/<handle>` page does not mean the channel is gone.**
///   Many channels simply switch the web preview off; `https://t.me/<handle>`
///   (no `/s/`) still shows the title and subscriber count, and MTProto can
///   still read them. It does mean *this* verification method can't see the
///   content, which is reason enough not to add one — but not reason to drop
///   one already verified (see `borderlandbeat`).
///
/// Also worth knowing: a handle that looks perfect can be a squatted or
/// abandoned name rather than the outlet you meant. `insightcrime` (25
/// subscribers, crypto-spam description), `sudantribune` (25 subscribers,
/// description: "username for sale"), `Faytuks`, `Excelsior`, and
/// `volcaholic1` all resolve to something unrelated to the well-known name.
/// The subscriber count and description are the cheap tell.
///
/// Deliberately excluded, with reasons (do not re-add without addressing
/// the reason first):
/// - `globalconflictmonitor` — real but tiny (~74 subscribers) and one post
///   referenced its own admin being "apprehended by police" with an
///   unresolved, unclear backstory. Too thin and murky to trust.
/// - `RSFSudan` — this is the Rapid Support Forces' *own* channel, a
///   combatant accused of war crimes in Sudan's civil war, not a neutral
///   monitor. A combatant's self-reporting is not an aggregate-chatter
///   signal, it's that combatant's messaging.
/// - `southfronteng`, `intelslava`, `eurasianist`, `BellumActaNews`,
///   `rnintel` — self-described partisan/"alternative narrative" framing
///   (their own channel descriptions say so). Feeding a partisan-optimized
///   posting account into an aggregate "chatter volume" signal quietly
///   biases that signal toward whatever that account's audience amplifies —
///   the opposite of what an aggregate signal should be.
/// - `middleeastobserver` — looked good in a secondhand description
///   ("balanced reporting"), but its actual public preview showed the
///   channel dead since 2018. A reminder that this list must be
///   live-verified, not description-verified.
/// - `GeoConfirmed` — a reputable name in open-source geolocation
///   verification, but its public preview page returned no content this
///   session, so its readability here couldn't actually be confirmed.
///   Revisit from inside a live session rather than re-adding on reputation
///   alone.
/// - `WarMonitors` — large (155K subscribers) and genuinely active, but its
///   own description sells in-channel ad slots through a bot and names a
///   gambling site as its sponsor. Placement being purchasable is a
///   different problem from partisanship and a worse one for a volume
///   signal: it means posting rate can be bought outright.
/// - `disclosetv` — large and active, but it aggregates viral claims with no
///   sourcing and frames itself as theatre ("the grand theater of our
///   time"). Volume there tracks what is spreading, not what is happening.
/// - `addisstandard` — an unattended bot mirror of the Addis Standard site
///   (its own description says "automatically posted by bot" and "read
///   cautiously"), and quiet for ~6 months. Ethiopia coverage is still a gap.
/// - `SentDefender` — resolves to `osintdefender`, already on the list. A
///   channel's second username is a duplicate, and sweeping it twice would
///   double-count every message it carries.
/// - `bnonews`, `AuroraIntel`, `bellingcat` — all real, all readable, all
///   dormant here (last posts 2026-02, 2025-06 and 2024-01 respectively).
///   The organisations are active elsewhere; these Telegram mirrors are not.
/// - `Militarylandnet`, `SudanWarMonitor`, `myanmarnow`, `noelreports`,
///   `IntelPointAlert`, `TheInsiderPaper`, `middleeasteye`, `visegrad24` —
///   web preview off, so the same situation as `GeoConfirmed`: nothing here
///   says they're bad, only that this check couldn't see them.
///
/// Coverage gaps as of 2026-08-13, in rough priority order for the next
/// pass: the Caribbean (nothing at all), South Asia, West Africa/the Sahel
/// (lost when `osintsahel` was dropped), and Ethiopia/the wider Horn beyond
/// Somalia.
pub const ALLOWED_CHANNELS: &[&str] = &[
    // Global/multi-region aggregators.
    "liveuamap",
    "ClashReport",
    "osintdefender",
    "insiderpaper", // global breaking news; heavy video, named outlet
    // Regional.
    "AMK_Mapping",     // Russia-Ukraine + Middle East
    "AlertaMundoNews", // Latin America + world, Spanish-language, video-heavy
    "garoweonline",    // Somalia and East Africa
    // Underreported/"forgotten story" beats, deliberately included even
    // though smaller than the aggregators above.
    "borderlandbeat", // Mexican cartel violence, citizen journalism since 2009
    // The three Myanmar outlets post mostly in Burmese, so expect little
    // ingest signal from them until chatter's topic tokens gain Burmese
    // equivalents (see chatter::TopicMatcher). They earn their place on the
    // media side, where the search term is usually a Latin-script place name.
    "MyanmarWitness", // human-rights reporting, geolocation-led
    "DVBTV",          // Democratic Voice of Burma — exile outlet, junta-banned
    "khitthitnews",   // Khit Thit Media — high volume and very fresh
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chatter::{ChatterAccumulator, DEFAULT_WINDOW_SECS};
    use chrono::{DateTime, Utc};

    use super::{ALLOWED_CHANNELS, ChannelSweep};

    const FIRST_ID: i32 = 17;
    const HIGH_ID: i32 = 42;
    const LOW_ID: i32 = 9;
    const LAST_ID: i32 = 100;
    const NEWER_ID: i32 = 101;
    const OLDER_ID: i32 = 99;
    const FINISHED_MESSAGE_TS: i64 = 1_000;
    const OPEN_MESSAGE_TS: i64 = 1_250;
    const OPEN_WINDOW_NOW: i64 = 1_300;

    fn accumulator() -> ChatterAccumulator {
        ChatterAccumulator::from_bundled(DEFAULT_WINDOW_SECS).unwrap()
    }

    fn ts(epoch_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(epoch_secs, 0).unwrap()
    }

    #[test]
    fn first_sweep_finishes_at_the_highest_id_seen() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(None);

        sweep.observe(&mut acc, FIRST_ID, "", ts(FINISHED_MESSAGE_TS));
        sweep.observe(&mut acc, HIGH_ID, "", ts(FINISHED_MESSAGE_TS));
        sweep.observe(&mut acc, LOW_ID, "", ts(FINISHED_MESSAGE_TS));

        assert_eq!(sweep.scanned(), 3);
        assert_eq!(sweep.finish(), Some(HIGH_ID));
    }

    #[test]
    fn high_water_mark_does_not_regress_for_out_of_order_messages() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(Some(LAST_ID));

        sweep.observe(&mut acc, NEWER_ID, "", ts(FINISHED_MESSAGE_TS));
        sweep.observe(&mut acc, LOW_ID, "", ts(FINISHED_MESSAGE_TS));

        assert_eq!(sweep.finish(), Some(NEWER_ID));
    }

    #[test]
    fn finish_is_none_without_a_newer_message() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(Some(LAST_ID));

        sweep.observe(&mut acc, OLDER_ID, "", ts(FINISHED_MESSAGE_TS));

        assert_eq!(sweep.finish(), None);
    }

    #[test]
    fn blank_messages_are_scanned_without_creating_rollups() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(None);

        sweep.observe(&mut acc, FIRST_ID, "", ts(FINISHED_MESSAGE_TS));
        sweep.observe(&mut acc, HIGH_ID, " \t\n ", ts(FINISHED_MESSAGE_TS));

        assert_eq!(sweep.scanned(), 2);
        assert_eq!(acc.scanned(), 0);
        assert!(acc.drain_all().is_empty());
    }

    #[test]
    fn only_messages_with_a_place_and_topic_roll_up() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(None);

        sweep.observe(
            &mut acc,
            FIRST_ID,
            "protest in Kyiv",
            ts(FINISHED_MESSAGE_TS),
        );
        sweep.observe(&mut acc, HIGH_ID, "travel to Kyiv", ts(FINISHED_MESSAGE_TS));
        sweep.observe(&mut acc, LOW_ID, "protest today", ts(FINISHED_MESSAGE_TS));

        let rollups = acc.drain_all();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].post_count, 1);
        assert_eq!(rollups[0].place_name, "Kyiv");
        assert_eq!(rollups[0].topic, "protest");
    }

    #[test]
    fn drain_completed_keeps_the_open_window_pending() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(None);

        sweep.observe(&mut acc, FIRST_ID, "protest in Kyiv", ts(OPEN_MESSAGE_TS));

        assert!(acc.drain_completed(ts(OPEN_WINDOW_NOW)).is_empty());
    }

    #[test]
    fn allowed_channels_are_unique_bare_usernames() {
        assert!(!ALLOWED_CHANNELS.is_empty());
        let unique = ALLOWED_CHANNELS.iter().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), ALLOWED_CHANNELS.len());
        assert!(ALLOWED_CHANNELS.iter().all(|channel| {
            !channel.contains(['@', '/']) && !channel.chars().any(char::is_whitespace)
        }));
    }
}
