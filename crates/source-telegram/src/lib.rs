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
//! **Aggregate-only by construction, same as `source-bluesky`.** Message
//! text is handed straight to [`chatter::ChatterAccumulator::observe`] and
//! dropped in the same call; no function in this crate returns message
//! text, sender identity, or a message URL to a caller. See
//! docs/SAFETY_AND_PRIVACY.md hard rule 6 before changing anything here.

#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
pub use live::TelegramSource;

/// Curated public-channel allowlist. Every handle here was live-verified
/// (its public `t.me/s/<handle>` preview checked for real, active, on-topic
/// content) during the session that added this source — not taken on a
/// blog's or search summary's word. Add to this list the same way: verify
/// before trusting, and prefer real subscriber counts/recent post dates over
/// a description alone.
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
pub const ALLOWED_CHANNELS: &[&str] = &[
    // Global/multi-region conflict aggregators.
    "liveuamap",
    "ClashReport",
    "osintdefender",
    // Regional.
    "osintsahel",       // Sahel (Mali, Burkina Faso, Niger)
    "Osinttechnical",   // Ukraine equipment/loss verification
    "AMK_Mapping",      // Russia-Ukraine + Middle East
    // Underreported/"forgotten story" beats, deliberately included even
    // though smaller than the aggregators above.
    "borderlandbeat",   // Mexican cartel violence, citizen journalism since 2009
    "DVBTV",            // Democratic Voice of Burma — Myanmar, exile outlet
                         // banned by the junta; posts mostly in Burmese, so
                         // expect little signal until chatter's topic tokens
                         // gain Burmese equivalents (see chatter::TopicMatcher).
];
