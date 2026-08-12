//! Bluesky Jetstream source adapter — real-time aggregate chatter volume.
//!
//! Jetstream is a keyless public WebSocket firehose of Bluesky repository
//! commits, filterable server-side to one collection. This is the first
//! **streaming** source in the workspace: instead of polling a window, a
//! long-lived socket task counts matching posts into a [`ChatterAccumulator`]
//! as they arrive, and [`SignalSource::fetch`] drains that accumulator. The
//! stream therefore stays behind the same poll-shaped interface every other
//! source uses.
//!
//! **Aggregate-only, by construction.** No function in this crate returns
//! post text, author DIDs/handles, post ids, or URLs to a caller — not even
//! privately. [`observe_message`] parses a message, feeds the text straight
//! into the accumulator, and drops it inside the same call, so there is no
//! API surface through which an individual post could be retained. See the
//! `chatter` crate docs and docs/SAFETY_AND_PRIVACY.md for why this is a
//! safety requirement rather than a style preference.

#[cfg(feature = "live")]
mod live;
#[cfg(feature = "live")]
pub use live::BlueskySource;

use chatter::ChatterAccumulator;
use chrono::DateTime;
use serde_json::Value;

/// Public Jetstream instances, tried in order and rotated on reconnect.
///
/// Verified reachable live before this crate was written; the operators run
/// several regional instances, so a single host being down is expected and
/// survivable rather than fatal.
pub const JETSTREAM_ENDPOINTS: &[&str] = &[
    "wss://jetstream2.us-east.bsky.network/subscribe",
    "wss://jetstream1.us-east.bsky.network/subscribe",
    "wss://jetstream2.us-west.bsky.network/subscribe",
    "wss://jetstream1.us-west.bsky.network/subscribe",
];

/// The one collection this source subscribes to. Filtering server-side keeps
/// the socket to posts instead of every repo commit on the network.
pub const WANTED_COLLECTION: &str = "app.bsky.feed.post";

/// What one Jetstream message did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageOutcome {
    /// Not a new post (an identity/account event, a delete, another
    /// collection) — deliberately ignored.
    Skipped,
    /// A new post was scanned; `matched` says whether it hit a place+topic.
    Scanned { matched: bool },
    /// Well-formed JSON is expected here; anything else is counted and
    /// dropped rather than killing the stream.
    Malformed,
}

/// Parse one Jetstream message and count it, if it is a new post.
///
/// Takes the accumulator rather than returning the text on purpose: the post
/// body is borrowed from the parsed JSON, handed to
/// [`ChatterAccumulator::observe`], and dropped when this function returns.
/// There is intentionally no variant of this function that hands text back.
///
/// The message's own `time_us` is used as the timestamp, not the record's
/// `createdAt`: `createdAt` is written by the posting client and can be
/// wrong, backdated, or in the future, while `time_us` is the firehose's own
/// ordering clock.
pub fn observe_message(raw: &str, acc: &mut ChatterAccumulator) -> MessageOutcome {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return MessageOutcome::Malformed;
    };
    if value.get("kind").and_then(Value::as_str) != Some("commit") {
        return MessageOutcome::Skipped;
    }
    let Some(commit) = value.get("commit") else {
        return MessageOutcome::Skipped;
    };
    if commit.get("operation").and_then(Value::as_str) != Some("create") {
        return MessageOutcome::Skipped;
    }
    if commit.get("collection").and_then(Value::as_str) != Some(WANTED_COLLECTION) {
        return MessageOutcome::Skipped;
    }
    let Some(text) = commit
        .get("record")
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
    else {
        return MessageOutcome::Skipped;
    };
    // Image-only posts carry an empty text field; nothing to match.
    if text.trim().is_empty() {
        return MessageOutcome::Skipped;
    }
    let Some(ts) = value
        .get("time_us")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_micros)
    else {
        return MessageOutcome::Malformed;
    };
    MessageOutcome::Scanned {
        matched: acc.observe(text, ts),
    }
}

/// Build the subscribe URL for `endpoint` with the server-side collection
/// filter applied.
pub fn subscribe_url(endpoint: &str) -> String {
    format!("{endpoint}?wantedCollections={WANTED_COLLECTION}")
}

/// Timestamp of a message, for tests and diagnostics.
#[cfg(test)]
fn message_time(raw: &str) -> Option<DateTime<chrono::Utc>> {
    serde_json::from_str::<Value>(raw)
        .ok()?
        .get("time_us")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_micros)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatter::DEFAULT_WINDOW_SECS;

    fn acc() -> ChatterAccumulator {
        ChatterAccumulator::from_bundled(DEFAULT_WINDOW_SECS).unwrap()
    }

    /// A real-shaped Jetstream message. Field names and nesting were copied
    /// from a live socket capture taken before this crate was written; the
    /// text is written here, not quoted from anyone's real post.
    fn post_message(text: &str) -> String {
        format!(
            r#"{{"did":"did:plc:example","time_us":1786509249463260,"kind":"commit",
             "commit":{{"rev":"3mstxtkr3im2f","operation":"create",
             "collection":"app.bsky.feed.post","rkey":"3mstxt7v5a22l",
             "record":{{"$type":"app.bsky.feed.post","createdAt":"2026-08-12T01:24:20.473Z",
             "langs":["en"],"text":"{text}"}},"cid":"bafyreiexample"}}}}"#
        )
    }

    #[test]
    fn counts_a_matching_post_create() {
        let mut a = acc();
        assert_eq!(
            observe_message(&post_message("huge protest in Kyiv today"), &mut a),
            MessageOutcome::Scanned { matched: true }
        );
        let rollups = a.drain_all();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].place_name, "Kyiv");
        assert_eq!(rollups[0].topic, "protest");
        assert_eq!(rollups[0].post_count, 1);
    }

    #[test]
    fn scans_but_does_not_count_an_unrelated_post() {
        let mut a = acc();
        assert_eq!(
            observe_message(&post_message("my sourdough finally rose"), &mut a),
            MessageOutcome::Scanned { matched: false }
        );
        assert_eq!(a.scanned(), 1);
        assert_eq!(a.matched(), 0);
        assert!(a.drain_all().is_empty());
    }

    #[test]
    fn skips_everything_that_is_not_a_new_post() {
        let mut a = acc();
        let cases = [
            // A delete, not a create.
            r#"{"kind":"commit","time_us":1786509249463260,"commit":{"operation":"delete","collection":"app.bsky.feed.post","record":{"text":"protest in Kyiv"}}}"#,
            // Another collection (a like, a follow).
            r#"{"kind":"commit","time_us":1786509249463260,"commit":{"operation":"create","collection":"app.bsky.feed.like","record":{"text":"protest in Kyiv"}}}"#,
            // Not a commit at all.
            r#"{"kind":"identity","time_us":1786509249463260,"did":"did:plc:example"}"#,
            // Image-only post: empty text.
            r#"{"kind":"commit","time_us":1786509249463260,"commit":{"operation":"create","collection":"app.bsky.feed.post","record":{"text":""}}}"#,
        ];
        for raw in cases {
            assert_eq!(
                observe_message(raw, &mut a),
                MessageOutcome::Skipped,
                "{raw}"
            );
        }
        assert_eq!(a.scanned(), 0, "skipped messages are not scanned posts");
    }

    #[test]
    fn malformed_messages_are_reported_not_fatal() {
        let mut a = acc();
        assert_eq!(
            observe_message("not json at all", &mut a),
            MessageOutcome::Malformed
        );
        // A post create with no usable stream timestamp.
        let no_ts = r#"{"kind":"commit","commit":{"operation":"create","collection":"app.bsky.feed.post","record":{"text":"protest in Kyiv"}}}"#;
        assert_eq!(observe_message(no_ts, &mut a), MessageOutcome::Malformed);
    }

    #[test]
    fn stream_time_is_used_not_client_created_at() {
        // createdAt in the fixture is 2026-08-12T01:24:20Z; time_us is
        // 1786509249463260us = 2026-08-12T04:34:09Z. The bucket must follow
        // time_us, because createdAt is client-supplied.
        let msg = post_message("protest in Kyiv");
        let stream_ts = message_time(&msg).unwrap();
        let mut a = acc();
        observe_message(&msg, &mut a);
        let rollups = a.drain_all();
        assert_eq!(
            rollups[0].window_start_epoch_s,
            chatter::window_start(stream_ts.timestamp(), DEFAULT_WINDOW_SECS)
        );
    }

    #[test]
    fn subscribe_url_filters_server_side() {
        assert_eq!(
            subscribe_url(JETSTREAM_ENDPOINTS[0]),
            "wss://jetstream2.us-east.bsky.network/subscribe?wantedCollections=app.bsky.feed.post"
        );
    }
}
