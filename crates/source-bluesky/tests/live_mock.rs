//! `live`-feature integration tests against a local mock Jetstream WebSocket
//! server: message delivery into the shared accumulator, malformed-frame
//! tolerance, and completed-vs-pending window draining over the real
//! `start_stream`/`fetch` network path, plus the stop half of that
//! lifecycle. No real network — run with
//! `cargo test -p source-bluesky --features live`.
#![cfg(feature = "live")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use core_types::{RawRecord, SignalSource, SourceFilters, TimeWindow};
use futures_util::{SinkExt, StreamExt};
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
    src.start_stream();

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
    src.start_stream();

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

// ---- lifecycle: switching the source off must stop the socket -------------

/// A server that keeps accepting, and keeps each connection open until the
/// client's end goes away.
struct MockServer {
    url: String,
    /// Connections accepted.
    connections: Arc<AtomicUsize>,
    /// Connections the *client* ended. This is the observation the stop test
    /// needs: a source that reports itself stopped while its socket is still
    /// open is exactly the defect being fixed.
    closed: Arc<AtomicUsize>,
}

/// `frames` are delivered to the **first** connection only, so a reconnect
/// does not re-deliver posts a test has already accounted for.
async fn serve_holding(frames: Vec<String>) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock jetstream server");
    let addr = listener.local_addr().expect("mock addr");
    let connections = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let (accepted, ended) = (Arc::clone(&connections), Arc::clone(&closed));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let nth = accepted.fetch_add(1, Ordering::SeqCst);
            let frames = if nth == 0 { frames.clone() } else { Vec::new() };
            let ended = Arc::clone(&ended);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                for frame in frames {
                    if ws.send(Message::Text(frame.into())).await.is_err() {
                        return;
                    }
                }
                // Read until the client goes away. A dropped socket surfaces
                // either as a stream end or as a reset; both mean closed.
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        break;
                    }
                }
                ended.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    MockServer {
        url: format!("ws://{addr}/"),
        connections,
        closed,
    }
}

/// A server that closes every connection immediately, so the stream task
/// spends its time in the reconnect backoff.
async fn serve_closing() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock jetstream server");
    let addr = listener.local_addr().expect("mock addr");
    let connections = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                    let _ = ws.close(None).await;
                }
            });
        }
    });
    (format!("ws://{addr}/"), connections)
}

#[tokio::test]
async fn stopping_closes_the_socket_and_starting_again_opens_exactly_one() {
    let server = serve_holding(Vec::new()).await;
    let src = BlueskySource::new(chatter::DEFAULT_WINDOW_SECS)
        .unwrap()
        .with_endpoint(server.url.clone());

    assert!(src.start_stream(), "the first start owns the task");
    wait_for(Duration::from_secs(5), || {
        server.connections.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(src.is_streaming());

    // Re-asserting "on" must not open a second socket counting the same
    // firehose into the same accumulator, which would double every number.
    assert!(!src.start_stream());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);

    assert!(src.stop_stream().await, "a task was running");
    assert!(!src.is_streaming());
    // `stop_stream` guarantees the client socket is dropped, which sends the
    // FIN; the server noticing still needs its own task to be scheduled. The
    // point of the assertion is that the connection really ends - a source
    // reporting itself stopped while the firehose keeps arriving is the
    // defect this lifecycle exists to fix.
    wait_for(Duration::from_secs(5), || {
        server.closed.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(!src.stop_stream().await, "stopping twice is not an error");

    assert!(src.start_stream(), "re-enabling starts a task again");
    wait_for(Duration::from_secs(5), || {
        server.connections.load(Ordering::SeqCst) >= 2
    })
    .await;
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        2,
        "exactly one task, so exactly one new connection"
    );
    src.stop_stream().await;
}

#[tokio::test]
async fn a_partly_counted_window_does_not_survive_the_source_going_off() {
    let server = serve_holding(vec![post_message(
        "protest breaks out in Nairobi",
        Utc::now().timestamp_micros(),
    )])
    .await;
    let src = BlueskySource::new(1)
        .unwrap()
        .with_endpoint(server.url.clone());

    src.start_stream();
    wait_for(Duration::from_secs(5), || src.stats().0 >= 1).await;
    assert!(
        src.fetch(window(), &SourceFilters::default())
            .await
            .unwrap()
            .is_empty(),
        "the window the post landed in is still in progress"
    );

    src.stop_stream().await;
    // Long enough that the window the post was counted into has completed.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    src.start_stream();

    let after = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert!(
        after.is_empty(),
        "posts counted before the source went off must not publish after it          comes back on: {after:?}"
    );
    src.stop_stream().await;
}

#[tokio::test]
async fn the_reconnect_wait_does_not_hold_the_task_open() {
    let (url, connections) = serve_closing().await;
    let src = BlueskySource::new(chatter::DEFAULT_WINDOW_SECS)
        .unwrap()
        .with_endpoint(url);

    src.start_stream();
    // One connection made and dropped puts the task in its reconnect sleep,
    // which is two seconds at the bottom of the backoff and five minutes at
    // the top.
    wait_for(Duration::from_secs(5), || {
        connections.load(Ordering::SeqCst) >= 1
    })
    .await;

    let started = std::time::Instant::now();
    assert!(src.stop_stream().await);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "stopping waited out the reconnect backoff ({:?})",
        started.elapsed()
    );
}
