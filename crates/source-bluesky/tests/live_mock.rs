//! `live`-feature integration tests against a local mock Jetstream WebSocket
//! server: message delivery into the shared accumulator, malformed-frame
//! tolerance, and completed-vs-pending window draining over the real
//! `spawn_stream`/`fetch` network path. No real network — run with
//! `cargo test -p source-bluesky --features live`.
#![cfg(feature = "live")]

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use core_types::{RawRecord, SignalSource, SourceFilters, TimeWindow};
use futures_util::SinkExt;
use source_bluesky::BlueskySource;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// A real-shaped Jetstream `commit`/`create` message. Field names and
/// nesting match a live socket capture; the text is written here, not
/// quoted from anyone's real post.
fn post_message(text: &str, time_us: i64) -> String {
    format!(
        r#"{{"did":"did:plc:example","time_us":{time_us},"kind":"commit",
         "commit":{{"rev":"3mstxtkr3im2f","operation":"create",
         "collection":"app.bsky.feed.post","rkey":"3mstxt7v5a22l",
         "record":{{"$type":"app.bsky.feed.post","createdAt":"2026-08-12T01:24:20.473Z",
         "langs":["en"],"text":"{text}"}},"cid":"bafyreiexample"}}}}"#
    )
}

/// A timestamp safely inside an already-completed window, regardless of the
/// window size a test picks.
fn old_time_us() -> i64 {
    (Utc::now() - ChronoDuration::hours(1)).timestamp_micros()
}

/// Accept one connection, send `frames` in order, then close.
async fn serve_once(frames: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock jetstream server");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let mut ws = tokio_tungstenite::accept_async(stream)
            .await
            .expect("mock websocket handshake");
        for frame in frames {
            if ws.send(Message::Text(frame.into())).await.is_err() {
                return;
            }
        }
        let _ = ws.close(None).await;
    });
    // A trailing slash keeps the request-target's path component non-empty
    // once `subscribe_url` appends `?wantedCollections=...` — an origin-form
    // request-target with only a query and no leading `/` is invalid.
    format!("ws://{addr}/")
}

/// Poll `done` until it is true or `timeout` elapses.
async fn wait_for(timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition did not become true within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn window() -> TimeWindow {
    // `fetch` ignores the window argument for a streamed source (a stream
    // has no addressable past); any value exercises that path.
    TimeWindow::new(Utc::now() - ChronoDuration::days(1), Utc::now())
}

#[tokio::test]
async fn streamed_posts_are_scanned_and_a_completed_rollup_is_drained() {
    let url = serve_once(vec![
        post_message("huge protest in Kyiv today", old_time_us()),
        "not json at all".to_owned(),
        post_message("my sourdough finally rose", old_time_us()),
        // Another collection: skipped, not scanned.
        r#"{"kind":"commit","time_us":1,"commit":{"operation":"create","collection":"app.bsky.feed.like","record":{"text":"protest in Kyiv"}}}"#.to_owned(),
    ])
    .await;

    let src = BlueskySource::new(chatter::DEFAULT_WINDOW_SECS)
        .unwrap()
        .with_endpoint(url);
    let _stream = src.spawn_stream();

    wait_for(Duration::from_secs(5), || src.stats().0 >= 2).await;

    let raws = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(raws.len(), 1, "only the matching post produces a rollup");
    match &raws[0] {
        RawRecord::ChatterRollup(r) => {
            assert_eq!(r.place_name, "Kyiv");
            assert_eq!(r.topic, "protest");
            assert_eq!(r.post_count, 1);
        }
        other => panic!("expected a chatter rollup, got {other:?}"),
    }

    let (scanned, matched) = src.stats();
    assert_eq!(scanned, 2, "malformed and skipped frames are not scanned");
    assert_eq!(matched, 1);
}

#[tokio::test]
async fn an_in_progress_window_stays_pending_until_it_completes() {
    let url = serve_once(vec![post_message(
        "protest breaks out in Nairobi",
        Utc::now().timestamp_micros(),
    )])
    .await;

    let src = BlueskySource::new(1).unwrap().with_endpoint(url);
    let _stream = src.spawn_stream();

    wait_for(Duration::from_secs(5), || src.stats().0 >= 1).await;

    let immediate = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert!(
        immediate.is_empty(),
        "the window the post landed in is still in progress"
    );

    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let after = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(after.len(), 1, "the window has now completed");
}
