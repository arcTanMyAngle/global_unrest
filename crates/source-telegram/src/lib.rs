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
//!
//! # Where the network stops
//!
//! [`ChannelReader`] is the seam between "what Telegram said" and "what this
//! crate does about it". Everything above it — the per-channel high-water
//! marks, the first-sweep-vs-incremental decision, the error swallowing that
//! keeps one dead channel from degrading the rest, the drain of completed
//! chatter windows, and the media leg's second video check — lives in
//! [`ChannelOrchestrator`] and [`search_all`] here, ungated, and is exercised
//! by `tests/orchestration.rs` against a fake reader under plain
//! `cargo test -p source-telegram`. Below it, `live.rs` holds only the
//! grammers glue: resolve, iterate, map.
//!
//! Keep it that way. If a change to [`ChannelReader`]'s signature needs a
//! grammers type, the signature is wrong — the seam exists precisely so the
//! layer above it never has to name one.

#[cfg(feature = "live")]
pub mod file_session;
#[cfg(feature = "live")]
mod live;
pub mod media;
#[cfg(feature = "live")]
pub use file_session::FileSession;
#[cfg(feature = "live")]
pub use live::TelegramSource;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use chatter::ChatterAccumulator;
use chrono::{DateTime, Utc};
use core_types::{ChannelClass, RawRecord, SourceError};
use media_search::{MediaHit, MediaQuery};

use crate::media::ChannelVideo;

/// Don't ingest a channel's entire history the first time it's swept — just
/// the most recent handful, enough to prime the per-channel high-water mark.
pub const FIRST_SWEEP_LIMIT: usize = 30;

/// Bound on how many new messages one poll pulls per channel. At this
/// source's poll cadence a channel would need to be extremely active to hit
/// this; if it does, the remainder is picked up next cycle — undercounting,
/// not overcounting, is the safe direction to bound in.
pub const PER_CYCLE_LIMIT: usize = 200;

/// The two reads this crate makes of a Telegram channel, with every grammers
/// type kept on the far side.
///
/// Implemented for real by `live.rs` over `grammers_client::Client`, and by a
/// fake in `tests/orchestration.rs`. Callers take `&impl ChannelReader`, so
/// the futures need no `Send` bound — same reasoning as
/// [`core_types::SignalSource`], and the grammers futures underneath are not
/// `Send` anyway.
#[allow(async_fn_in_trait)]
pub trait ChannelReader {
    /// Stream one channel's history, newest-first from the top when `after`
    /// is `None` and oldest-first from just past `after` when it is `Some`,
    /// stopping at `limit` messages.
    ///
    /// **Each message is handed to `on_message` and dropped; this must never
    /// return the messages.** That is a product-rule constraint, not a style
    /// preference (CLAUDE.md rule 2): observing and dropping in the same call
    /// is what keeps up to [`PER_CYCLE_LIMIT`] message texts from being
    /// materialized at once. A `Vec<String>` here would be a chatter-boundary
    /// regression even if nothing ever read it.
    ///
    /// A channel that cannot be resolved is `Ok(())` with nothing delivered,
    /// not an error — it is absent, not broken. Errors are for a read that
    /// actually failed, and messages already handed over before one stay
    /// counted.
    async fn sweep_history(
        &self,
        channel: &str,
        after: Option<i32>,
        limit: usize,
        on_message: &mut dyn FnMut(i32, &str, DateTime<Utc>),
    ) -> Result<(), SourceError>;

    /// Server-side video search over one channel, bounded to `query`'s window
    /// and [`media::PER_CHANNEL_LIMIT`].
    ///
    /// Returning a `Vec` is allowed here and only here: materialized results
    /// are the documented Media exception (CLAUDE.md rule 7). The server-side
    /// filter is not trusted on its own, so the caller re-checks each
    /// attachment — see [`media::playable_hits`].
    async fn search_videos(
        &self,
        channel: &str,
        text: &str,
        query: &MediaQuery,
    ) -> Result<Vec<ChannelVideo>, SourceError>;
}

/// The ingest leg's state and orchestration: one accumulator, one high-water
/// mark per channel, and the sweep loop over [`ALLOWED_CHANNELS`].
///
/// Ungated on purpose. `TelegramSource` owns one of these and delegates to it;
/// the tests own one and drive it with a fake [`ChannelReader`].
pub struct ChannelOrchestrator {
    accumulator: Mutex<ChatterAccumulator>,
    /// Highest message id already processed per channel, so a poll only
    /// walks messages newer than what was already counted. Deliberately
    /// **not** persisted to disk: on restart each channel is swept from
    /// scratch (bounded to [`FIRST_SWEEP_LIMIT`]), but any chatter window
    /// that already published re-derives the same `source_event_id` and is
    /// discarded by storage's dedup-by-id (the same corrections-reuse-ids
    /// behavior ACLED relies on) — safe, just occasionally redundant work,
    /// never double counted.
    last_seen: Mutex<HashMap<String, i32>>,
}

impl ChannelOrchestrator {
    #[must_use]
    pub fn new(accumulator: ChatterAccumulator) -> Self {
        Self {
            accumulator: Mutex::new(accumulator),
            last_seen: Mutex::new(HashMap::new()),
        }
    }

    /// Build over the bundled gazetteer/topic lists.
    pub fn from_bundled(window_secs: i64) -> Result<Self, SourceError> {
        let accumulator = ChatterAccumulator::from_bundled(window_secs)
            .map_err(|e| SourceError::Other(format!("building chatter matcher: {e}")))?;
        Ok(Self::new(accumulator))
    }

    fn lock_accumulator(&self) -> MutexGuard<'_, ChatterAccumulator> {
        self.accumulator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_last_seen(&self) -> MutexGuard<'_, HashMap<String, i32>> {
        self.last_seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The high-water mark currently held for `channel`, if it has been swept.
    ///
    /// Read-only bookkeeping: a message id, never a message.
    #[must_use]
    pub fn mark(&self, channel: &str) -> Option<i32> {
        self.lock_last_seen().get(channel).copied()
    }

    /// Sweep every allowlisted channel in order.
    pub async fn sweep_all(&self, reader: &impl ChannelReader) {
        for channel in ALLOWED_CHANNELS {
            self.sweep_channel(reader, *channel).await;
        }
    }

    /// Sweep one channel: pull messages newer than its high-water mark (or,
    /// on first contact, just the most recent [`FIRST_SWEEP_LIMIT`]), feed
    /// matching text into the accumulator, and advance the mark.
    ///
    /// Failures are logged and swallowed rather than propagated — one
    /// unreachable or renamed channel must not degrade the rest of
    /// [`ALLOWED_CHANNELS`]. Whatever arrived before a mid-sweep failure is
    /// still counted and still advances the mark; re-reading it next cycle
    /// would double-count it.
    async fn sweep_channel(&self, reader: &impl ChannelReader, channel: Channel) {
        let name = channel.name;
        let last_id = self.mark(name);
        let limit = if last_id.is_some() {
            PER_CYCLE_LIMIT
        } else {
            FIRST_SWEEP_LIMIT
        };

        let mut sweep = ChannelSweep::new(last_id);
        let outcome = {
            // The closure is the whole point: text is borrowed, folded into
            // the accumulator, and gone before the next message arrives.
            let mut on_message = |id: i32, text: &str, date: DateTime<Utc>| {
                sweep.observe(&mut self.lock_accumulator(), id, text, date, channel.class);
            };
            reader
                .sweep_history(name, last_id, limit, &mut on_message)
                .await
        };
        if let Err(e) = outcome {
            tracing::warn!(channel = name, error = %e, "telegram channel sweep failed; skipping");
        }
        if let Some(newest) = sweep.finish() {
            self.lock_last_seen().insert(name.to_owned(), newest);
        }
        tracing::info!(
            channel = name,
            scanned = sweep.scanned(),
            "telegram channel swept"
        );
    }

    /// Drain the chatter windows that had closed by `now` into raw records.
    ///
    /// A window still in progress stays pending, so a count is published once,
    /// complete, rather than repeatedly as it grows.
    pub fn drain_completed(&self, now: DateTime<Utc>) -> Vec<RawRecord> {
        let rollups = self.lock_accumulator().drain_completed(now);
        tracing::info!(rollups = rollups.len(), "telegram chatter rollups drained");
        rollups.into_iter().map(RawRecord::ChatterRollup).collect()
    }
}

/// On-demand video lookup across the same allowlist — the user-directed
/// exception to this crate's aggregate-only rule (see [`media`]).
///
/// **No cadence.** This runs only when a person presses search for a named
/// place; nothing here is scheduled, and nothing it returns is stored.
///
/// A free function rather than a [`ChannelOrchestrator`] method because it
/// genuinely holds no state — that is the rule-7 bound made structural. It
/// keeps no marks, touches no accumulator, and leaves nothing behind between
/// searches.
///
/// Scope is deliberately narrow: uploaded video only. Posts that merely *link*
/// a video host are left to the GDELT and Bluesky legs in `media-search`,
/// which already cover exactly that and cover it across far more of the web
/// than a dozen channels could.
///
/// One dead channel must not empty the panel, so per-channel failures are
/// logged and skipped; the whole search fails only when every channel did.
pub async fn search_all(
    reader: &impl ChannelReader,
    query: &MediaQuery,
) -> Result<Vec<MediaHit>, SourceError> {
    if !query.is_valid() {
        return Ok(Vec::new());
    }
    let Some(text) = media::query_text(&query.place, &query.topic) else {
        return Ok(Vec::new());
    };

    let mut hits = Vec::new();
    let mut failed = 0usize;
    for channel in ALLOWED_CHANNELS {
        let name = channel.name;
        match reader.search_videos(name, &text, query).await {
            Ok(found) => hits.extend(media::playable_hits(name, &found)),
            Err(e) => {
                failed += 1;
                tracing::warn!(channel = name, error = %e, "telegram media search failed");
            }
        }
    }
    if failed == ALLOWED_CHANNELS.len() {
        return Err(SourceError::Other(
            "every telegram channel search failed — the session may have expired".to_string(),
        ));
    }

    let mut hits = media_search::merge(hits);
    hits.truncate(query.limit);
    Ok(hits)
}

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
        class: ChannelClass,
    ) {
        self.scanned += 1;
        self.newest = Some(self.newest.map_or(id, |newest| newest.max(id)));
        if !text.trim().is_empty() {
            // One accumulator is shared by every channel, so the class has to
            // travel with the message — by rollup time the counts are summed.
            acc.observe(text, date, class);
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
pub const ALLOWED_CHANNELS: &[Channel] = &[
    // Global/multi-region aggregators.
    Channel::new("liveuamap", ChannelClass::Monitor),
    Channel::new("ClashReport", ChannelClass::Monitor),
    Channel::new("osintdefender", ChannelClass::Monitor),
    // Global breaking news; heavy video, named outlet.
    Channel::new("insiderpaper", ChannelClass::Outlet),
    // Regional.
    // Russia-Ukraine + Middle East.
    Channel::new("AMK_Mapping", ChannelClass::Monitor),
    // Latin America + world, Spanish-language, video-heavy.
    Channel::new("AlertaMundoNews", ChannelClass::Outlet),
    // Somalia and East Africa.
    Channel::new("garoweonline", ChannelClass::Outlet),
    // Underreported/"forgotten story" beats, deliberately included even
    // though smaller than the aggregators above.
    // Mexican cartel violence, citizen journalism since 2009.
    Channel::new("borderlandbeat", ChannelClass::Outlet),
    // The three Myanmar outlets post mostly in Burmese. `chatter::script` can
    // now read Burmese place and topic tokens, so they register ingest signal,
    // but only for the terms in those tables — a Burmese post about anywhere
    // outside Myanmar is still unreachable. They also earn their place on the
    // media side, where the search term is usually a Latin-script place name.
    // Human-rights reporting, geolocation-led.
    Channel::new("MyanmarWitness", ChannelClass::Monitor),
    // Democratic Voice of Burma — exile outlet, junta-banned.
    Channel::new("DVBTV", ChannelClass::Outlet),
    // Khit Thit Media — high volume and very fresh.
    Channel::new("khitthitnews", ChannelClass::Outlet),
];

/// One allowlisted channel and its stated provenance.
///
/// Class is mandatory rather than defaulted: a channel's posting rate means
/// something different depending on who runs it — a monitor's tracks events,
/// a combatant's tracks messaging — and summing them produces a number that
/// means neither. It is a property of the channel, never of any person who
/// posts there. See docs/SIGNAL_MODEL.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub name: &'static str,
    pub class: ChannelClass,
}

impl Channel {
    const fn new(name: &'static str, class: ChannelClass) -> Self {
        Self { name, class }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chatter::{ChatterAccumulator, DEFAULT_WINDOW_SECS};
    use chrono::{DateTime, Utc};

    use core_types::ChannelClass;

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

        sweep.observe(
            &mut acc,
            FIRST_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            HIGH_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            LOW_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );

        assert_eq!(sweep.scanned(), 3);
        assert_eq!(sweep.finish(), Some(HIGH_ID));
    }

    #[test]
    fn high_water_mark_does_not_regress_for_out_of_order_messages() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(Some(LAST_ID));

        sweep.observe(
            &mut acc,
            NEWER_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            LOW_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );

        assert_eq!(sweep.finish(), Some(NEWER_ID));
    }

    #[test]
    fn finish_is_none_without_a_newer_message() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(Some(LAST_ID));

        sweep.observe(
            &mut acc,
            OLDER_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );

        assert_eq!(sweep.finish(), None);
    }

    #[test]
    fn blank_messages_are_scanned_without_creating_rollups() {
        let mut acc = accumulator();
        let mut sweep = ChannelSweep::new(None);

        sweep.observe(
            &mut acc,
            FIRST_ID,
            "",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            HIGH_ID,
            " \t\n ",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );

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
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            HIGH_ID,
            "travel to Kyiv",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );
        sweep.observe(
            &mut acc,
            LOW_ID,
            "protest today",
            ts(FINISHED_MESSAGE_TS),
            ChannelClass::Monitor,
        );

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

        sweep.observe(
            &mut acc,
            FIRST_ID,
            "protest in Kyiv",
            ts(OPEN_MESSAGE_TS),
            ChannelClass::Monitor,
        );

        assert!(acc.drain_completed(ts(OPEN_WINDOW_NOW)).is_empty());
    }

    /// The catalog carries provenance for every entry. `Unspecified` is the
    /// default the *type* has, so a channel left at it would silently mean
    /// "we never said" while looking configured.
    #[test]
    fn every_catalog_channel_declares_a_class() {
        assert!(
            ALLOWED_CHANNELS
                .iter()
                .all(|channel| channel.class != ChannelClass::Unspecified),
            "a catalog channel without an explicit class fabricates provenance"
        );
    }

    #[test]
    fn allowed_channels_are_unique_bare_usernames() {
        assert!(!ALLOWED_CHANNELS.is_empty());
        let unique = ALLOWED_CHANNELS
            .iter()
            .map(|channel| channel.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), ALLOWED_CHANNELS.len());
        assert!(ALLOWED_CHANNELS.iter().all(|channel| {
            !channel.name.contains(['@', '/']) && !channel.name.chars().any(char::is_whitespace)
        }));
    }
}
