//! Integration tests for the Telegram orchestration layer, driven through the
//! [`ChannelReader`] seam by a fake.
//!
//! These need **no features**: `chatter`, `media-search`, `core-types`,
//! `chrono`, and `tracing` are all non-optional dependencies of this crate, so
//! everything under test compiles and runs under plain
//! `cargo test -p source-telegram`. What is deliberately *not* covered here is
//! the grammers glue in `src/live.rs` — resolve, iterate, map — which only a
//! real MTProto session exercises honestly. See ROADMAP.md's scoping of this
//! gap for why a mock MTProto server was rejected.

use std::cell::RefCell;
use std::collections::HashMap;

use chatter::ChatterAccumulator;
use chrono::{DateTime, Utc};
use core_types::{RawRecord, SourceError};
use media_search::{MediaQuery, Provider};
use source_telegram::media::ChannelVideo;
use source_telegram::{
    ALLOWED_CHANNELS, ChannelOrchestrator, ChannelReader, FIRST_SWEEP_LIMIT, PER_CYCLE_LIMIT,
    search_all,
};

const WINDOW_SECS: i64 = 300;
/// Inside a window that has closed by [`OPEN_WINDOW_NOW`].
const FINISHED_MESSAGE_TS: i64 = 1_000;
/// Inside the window still in progress at [`OPEN_WINDOW_NOW`].
const OPEN_MESSAGE_TS: i64 = 1_250;
const OPEN_WINDOW_NOW: i64 = 1_300;

fn ts(epoch_secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(epoch_secs, 0).unwrap()
}

fn orchestrator() -> ChannelOrchestrator {
    ChannelOrchestrator::new(ChatterAccumulator::from_bundled(WINDOW_SECS).unwrap())
}

fn query(limit: usize) -> MediaQuery {
    MediaQuery {
        place: "Kyiv".into(),
        topic: "protest".into(),
        start: ts(0),
        end: ts(FINISHED_MESSAGE_TS * 2),
        limit,
    }
}

/// One message as the fake hands it over: what `sweep_history` streams.
#[derive(Clone)]
struct Msg {
    id: i32,
    text: String,
    date: DateTime<Utc>,
}

fn msg(id: i32, text: &str, epoch_secs: i64) -> Msg {
    Msg {
        id,
        text: text.into(),
        date: ts(epoch_secs),
    }
}

/// What one channel does when the fake is asked to read it.
#[derive(Clone, Default)]
struct Channel {
    history: Vec<Msg>,
    videos: Vec<ChannelVideo>,
    /// Fail the read after streaming `history`, the way a mid-sweep network
    /// error does.
    fails: bool,
}

/// What the orchestration actually asked for on one call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SweepCall {
    channel: String,
    after: Option<i32>,
    limit: usize,
}

/// A [`ChannelReader`] with no network: canned per-channel replies plus a
/// recording of every request the layer above made.
#[derive(Default)]
struct FakeReader {
    channels: HashMap<String, Channel>,
    /// Every channel not named in `channels` behaves like this.
    default_channel: Channel,
    sweeps: RefCell<Vec<SweepCall>>,
    searches: RefCell<Vec<String>>,
}

impl FakeReader {
    fn with(mut self, channel: &str, spec: Channel) -> Self {
        self.channels.insert(channel.to_owned(), spec);
        self
    }

    fn everywhere(mut self, spec: Channel) -> Self {
        self.default_channel = spec;
        self
    }

    fn spec(&self, channel: &str) -> &Channel {
        self.channels.get(channel).unwrap_or(&self.default_channel)
    }

    fn sweeps(&self) -> Vec<SweepCall> {
        self.sweeps.borrow().clone()
    }

    fn sweep_of(&self, channel: &str) -> SweepCall {
        self.sweeps()
            .into_iter()
            .find(|call| call.channel == channel)
            .unwrap_or_else(|| panic!("{channel} was never swept"))
    }
}

impl ChannelReader for FakeReader {
    async fn sweep_history(
        &self,
        channel: &str,
        after: Option<i32>,
        limit: usize,
        on_message: &mut dyn FnMut(i32, &str, DateTime<Utc>),
    ) -> Result<(), SourceError> {
        self.sweeps.borrow_mut().push(SweepCall {
            channel: channel.to_owned(),
            after,
            limit,
        });
        let spec = self.spec(channel);
        for m in &spec.history {
            on_message(m.id, &m.text, m.date);
        }
        if spec.fails {
            return Err(SourceError::Other(format!("@{channel} went away")));
        }
        Ok(())
    }

    async fn search_videos(
        &self,
        channel: &str,
        _text: &str,
        _query: &MediaQuery,
    ) -> Result<Vec<ChannelVideo>, SourceError> {
        self.searches.borrow_mut().push(channel.to_owned());
        let spec = self.spec(channel);
        if spec.fails {
            return Err(SourceError::Other(format!("@{channel} went away")));
        }
        Ok(spec.videos.clone())
    }
}

fn video(id: i32, mime: Option<&str>, epoch_secs: i64) -> ChannelVideo {
    ChannelVideo {
        id,
        caption: "footage".into(),
        date: ts(epoch_secs),
        mime_type: mime.map(str::to_owned),
        file_name: None,
        has_document: true,
    }
}

// --- the sweep request itself ------------------------------------------

#[tokio::test]
async fn a_first_sweep_asks_for_the_recent_head_with_no_offset() {
    let reader = FakeReader::default();
    let core = orchestrator();

    core.sweep_all(&reader).await;

    assert_eq!(reader.sweeps().len(), ALLOWED_CHANNELS.len());
    for call in reader.sweeps() {
        assert_eq!(call.after, None, "{} got an offset", call.channel);
        assert_eq!(call.limit, FIRST_SWEEP_LIMIT, "{}", call.channel);
    }
}

#[tokio::test]
async fn an_incremental_sweep_passes_the_stored_mark_and_the_larger_limit() {
    let channel = ALLOWED_CHANNELS[0].name;
    let reader = FakeReader::default().with(
        channel,
        Channel {
            history: vec![msg(7, "", FINISHED_MESSAGE_TS)],
            ..Channel::default()
        },
    );
    let core = orchestrator();

    core.sweep_all(&reader).await;
    assert_eq!(core.mark(channel), Some(7));

    core.sweep_all(&reader).await;

    let second = reader.sweeps()[ALLOWED_CHANNELS.len()].clone();
    assert_eq!(
        second,
        SweepCall {
            channel: channel.to_owned(),
            after: Some(7),
            limit: PER_CYCLE_LIMIT,
        }
    );
    // A channel that produced nothing on the first pass still has no mark, so
    // it asks for the head again rather than an offset of zero.
    assert_eq!(core.mark(ALLOWED_CHANNELS[1].name), None);
    assert_eq!(reader.sweep_of(ALLOWED_CHANNELS[1].name).after, None);
}

// --- the high-water mark ------------------------------------------------

#[tokio::test]
async fn the_mark_advances_to_the_newest_id_and_never_regresses() {
    let channel = ALLOWED_CHANNELS[0].name;
    let core = orchestrator();

    let first = FakeReader::default().with(
        channel,
        Channel {
            history: vec![
                msg(11, "", FINISHED_MESSAGE_TS),
                msg(42, "", FINISHED_MESSAGE_TS),
                msg(30, "", FINISHED_MESSAGE_TS),
            ],
            ..Channel::default()
        },
    );
    core.sweep_all(&first).await;
    assert_eq!(core.mark(channel), Some(42), "the newest id seen wins");

    // A later sweep that only turns up older messages must not walk it back.
    let older = FakeReader::default().with(
        channel,
        Channel {
            history: vec![msg(12, "", FINISHED_MESSAGE_TS)],
            ..Channel::default()
        },
    );
    core.sweep_all(&older).await;
    assert_eq!(core.mark(channel), Some(42));

    // Nor must a sweep that returns nothing at all.
    core.sweep_all(&FakeReader::default()).await;
    assert_eq!(core.mark(channel), Some(42));
}

// --- one dead channel must not degrade the rest -------------------------

#[tokio::test]
async fn a_failing_channel_is_skipped_while_the_rest_still_roll_up() {
    let dead = ALLOWED_CHANNELS[0].name;
    let live = ALLOWED_CHANNELS[1].name;
    let reader = FakeReader::default()
        .with(
            dead,
            Channel {
                fails: true,
                ..Channel::default()
            },
        )
        .with(
            live,
            Channel {
                history: vec![msg(5, "protest in Kyiv", FINISHED_MESSAGE_TS)],
                ..Channel::default()
            },
        );
    let core = orchestrator();

    core.sweep_all(&reader).await;

    assert_eq!(
        reader.sweeps().len(),
        ALLOWED_CHANNELS.len(),
        "the sweep continued past the failure"
    );
    assert_eq!(core.mark(dead), None);
    assert_eq!(core.mark(live), Some(5));

    let records = core.drain_completed(ts(OPEN_WINDOW_NOW));
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn messages_seen_before_a_mid_sweep_failure_stay_counted() {
    let channel = ALLOWED_CHANNELS[0].name;
    let reader = FakeReader::default().with(
        channel,
        Channel {
            history: vec![msg(5, "protest in Kyiv", FINISHED_MESSAGE_TS)],
            fails: true,
            ..Channel::default()
        },
    );
    let core = orchestrator();

    core.sweep_all(&reader).await;

    // Re-reading the same message next cycle would double-count it, so the
    // mark advances even though the read ended badly.
    assert_eq!(core.mark(channel), Some(5));
    assert_eq!(core.drain_completed(ts(OPEN_WINDOW_NOW)).len(), 1);
}

// --- draining ----------------------------------------------------------

#[tokio::test]
async fn only_completed_windows_drain_and_an_open_one_stays_pending() {
    let reader = FakeReader::default().with(
        ALLOWED_CHANNELS[0].name,
        Channel {
            history: vec![
                msg(1, "protest in Kyiv", FINISHED_MESSAGE_TS),
                msg(2, "flooding in Nairobi", OPEN_MESSAGE_TS),
            ],
            ..Channel::default()
        },
    );
    let core = orchestrator();

    core.sweep_all(&reader).await;
    // This is exactly what `SignalSource::fetch` does after `sweep_all`; it
    // differs only in passing `Utc::now()`, which a test cannot pin.
    let records = core.drain_completed(ts(OPEN_WINDOW_NOW));

    assert_eq!(
        records.len(),
        1,
        "the in-progress window is not drained yet"
    );
    match &records[0] {
        RawRecord::ChatterRollup(rollup) => {
            assert_eq!(rollup.place_name, "Kyiv");
            assert_eq!(rollup.topic, "protest");
            assert_eq!(rollup.post_count, 1);
        }
        other => panic!("expected a chatter rollup, got {other:?}"),
    }

    // Once its window has closed, the second one comes through.
    let later = core.drain_completed(ts(OPEN_MESSAGE_TS + WINDOW_SECS * 2));
    assert_eq!(later.len(), 1);
}

/// The chatter boundary, asserted at the layer that could break it.
///
/// `ChatterRollup` carries `(place, topic, window) -> count` and nothing else
/// — never post text, author identity, message ids, or URLs (CLAUDE.md rule 2,
/// docs/SAFETY_AND_PRIVACY.md hard rule 6). The ingest leg streams message
/// text through a callback and drops it in the same call precisely so it
/// cannot end up here. If this test fails, a message body has leaked into an
/// aggregate, and that is a product-rule violation, not a failing assertion to
/// relax.
#[tokio::test]
async fn no_raw_message_text_reaches_a_rollup() {
    const BODY: &str = "protest in Kyiv, filmed by a named eyewitness at 12 Example Street";
    let reader = FakeReader::default().with(
        ALLOWED_CHANNELS[0].name,
        Channel {
            history: vec![msg(1, BODY, FINISHED_MESSAGE_TS)],
            ..Channel::default()
        },
    );
    let core = orchestrator();

    core.sweep_all(&reader).await;
    let records = core.drain_completed(ts(OPEN_WINDOW_NOW));
    assert_eq!(records.len(), 1);

    let RawRecord::ChatterRollup(rollup) = &records[0] else {
        panic!("expected a chatter rollup");
    };
    // Debug covers every field, including any added later without reading this.
    let rendered = format!("{rollup:?}");
    for fragment in ["filmed", "eyewitness", "Example Street", "12"] {
        assert!(
            !rendered.contains(fragment),
            "message text leaked into a rollup: {rendered}"
        );
    }
    assert!(
        !rendered.contains(BODY),
        "message text leaked into a rollup"
    );
    // The place and topic labels are the aggregate; they come from the
    // gazetteer, not from the post.
    assert_eq!(rollup.place_name, "Kyiv");
    assert_eq!(rollup.topic, "protest");
    assert_eq!(rollup.post_count, 1);
    // And the ingest log excerpt is built by hand for the same reason.
    assert!(!records[0].excerpt(4_096).contains("eyewitness"));
}

// --- the media leg -----------------------------------------------------

#[tokio::test]
async fn the_server_filter_is_rechecked_before_a_row_promises_a_video() {
    let channel = ALLOWED_CHANNELS[0].name;
    let reader = FakeReader::default().with(
        channel,
        Channel {
            videos: vec![
                video(1, Some("video/mp4"), FINISHED_MESSAGE_TS),
                // Counted as video server-side, not playable in fact.
                video(2, Some("application/pdf"), FINISHED_MESSAGE_TS),
                ChannelVideo {
                    has_document: false,
                    ..video(3, Some("video/mp4"), FINISHED_MESSAGE_TS)
                },
            ],
            ..Channel::default()
        },
    );

    let hits = search_all(&reader, &query(10)).await.unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, format!("https://t.me/{channel}/1"));
    assert_eq!(hits[0].origin, format!("@{channel}"));
    assert_eq!(hits[0].provider, Provider::Telegram);
}

#[tokio::test]
async fn media_hits_merge_across_channels_and_truncate_to_the_query_limit() {
    let reader = FakeReader::default().everywhere(Channel {
        videos: vec![
            video(1, Some("video/mp4"), FINISHED_MESSAGE_TS),
            video(2, Some("video/mp4"), FINISHED_MESSAGE_TS + 60),
        ],
        ..Channel::default()
    });

    let all = search_all(&reader, &query(1_000)).await.unwrap();
    assert_eq!(all.len(), ALLOWED_CHANNELS.len() * 2);
    assert!(
        all.windows(2).all(|w| w[0].ts_utc >= w[1].ts_utc),
        "merged hits are newest-first"
    );

    let capped = search_all(&reader, &query(3)).await.unwrap();
    assert_eq!(capped.len(), 3);
}

#[tokio::test]
async fn one_failing_channel_is_skipped_but_every_channel_failing_is_an_error() {
    let partial = FakeReader::default()
        .everywhere(Channel {
            videos: vec![video(1, Some("video/mp4"), FINISHED_MESSAGE_TS)],
            ..Channel::default()
        })
        .with(
            ALLOWED_CHANNELS[0].name,
            Channel {
                fails: true,
                ..Channel::default()
            },
        );
    let survivors = search_all(&partial, &query(1_000)).await.unwrap();
    assert_eq!(
        survivors.len(),
        ALLOWED_CHANNELS.len() - 1,
        "a dead channel must not empty the panel"
    );

    let all_dead = FakeReader::default().everywhere(Channel {
        fails: true,
        ..Channel::default()
    });
    let err = search_all(&all_dead, &query(1_000)).await.unwrap_err();
    assert!(
        err.to_string().contains("every telegram channel search"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn an_unusable_query_never_reaches_a_channel() {
    let reader = FakeReader::default();

    // No place left after sanitising.
    let mut q = query(10);
    q.place = "***".into();
    assert!(search_all(&reader, &q).await.unwrap().is_empty());

    // End before start.
    let mut q = query(10);
    q.end = q.start;
    assert!(search_all(&reader, &q).await.unwrap().is_empty());

    assert!(
        reader.searches.borrow().is_empty(),
        "an invalid query must not be sent anywhere"
    );
}
