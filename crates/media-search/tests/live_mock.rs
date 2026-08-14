//! `live`-feature integration tests against local mock GDELT/Bluesky servers:
//! what actually goes on the wire, and how each provider's failure modes
//! surface. No real network, no credentials — run with
//! `cargo test -p media-search --features live`.
//!
//! The offline unit tests already cover URL building and parsing separately.
//! What only shows up here is the seam between them: that a leg which fails
//! does not empty the other leg's results, that a rejection is named rather
//! than read as "nothing happened in this place", and that the merged list
//! keeps its provider split.
#![cfg(feature = "live")]

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use core_types::SourceError;
use media_search::{MediaQuery, MediaSearch, Provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve canned HTTP responses. The handler sees `"METHOD /path?query"` and
/// returns a complete response via [`http_body`].
async fn serve<F>(handler: F) -> String
where
    F: Fn(&str) -> String + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                // GETs only: the request is header-only, so the first read that
                // reaches the blank line has the whole thing.
                let mut buf = vec![0u8; 16 * 1024];
                let mut n = 0;
                loop {
                    match sock.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(r) => n += r,
                    }
                    if String::from_utf8_lossy(&buf[..n]).contains("\r\n\r\n") || n == buf.len() {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.rsplit_once(" HTTP/"))
                    .map(|(t, _)| t.to_owned())
                    .unwrap_or_default();
                let _ = sock.write_all(handler(&target).as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A complete HTTP/1.1 response.
fn http_body(status: &str, content_type: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn json_ok(body: &str) -> String {
    http_body("200 OK", "application/json", "", body)
}

fn query() -> MediaQuery {
    MediaQuery {
        place: "Colombia".into(),
        topic: "earthquake".into(),
        start: Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap(),
        end: Utc.with_ymd_and_hms(2026, 8, 13, 6, 30, 0).unwrap(),
        limit: 25,
    }
}

const GDELT_BODY: &str = r#"{"articles":[
    {"url":"https://www.youtube.com/watch?v=abc","title":"Quake footage from Bogota",
     "seendate":"20260812T090000Z","domain":"youtube.com"},
    {"url":"https://news.example.org/story","title":"Wire report",
     "seendate":"20260812T090000Z","domain":"news.example.org"}
]}"#;

const BLUESKY_BODY: &str = r#"{"posts":[
    {"uri":"at://did:plc:aaa/app.bsky.feed.post/rk1",
     "author":{"handle":"reporter.example"},
     "record":{"text":"Damage in the old town"},
     "indexedAt":"2026-08-12T10:00:00Z",
     "embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/w/d/c/playlist.m3u8"}}
]}"#;

/// Route by path so one mock server can stand in for both providers.
async fn both(gdelt: String, bluesky: String) -> MediaSearch {
    let base = serve(move |target| {
        if target.contains("/doc") {
            gdelt.clone()
        } else {
            bluesky.clone()
        }
    })
    .await;
    MediaSearch::new()
        .expect("build client")
        .with_gdelt_endpoint(format!("{base}/doc"))
        .with_bluesky_endpoint(format!("{base}/xrpc/app.bsky.feed.searchPosts"))
}

#[tokio::test]
async fn both_legs_merge_and_keep_their_provider_split() {
    let search = both(json_ok(GDELT_BODY), json_ok(BLUESKY_BODY)).await;
    let (hits, problems) = search.search(&query()).await;

    assert!(problems.is_empty(), "{problems:?}");
    // The non-video article is dropped, so both survivors are playable.
    assert_eq!(hits.len(), 2, "{hits:#?}");
    let providers: Vec<Provider> = hits.iter().map(|h| h.provider).collect();
    assert!(providers.contains(&Provider::Gdelt));
    assert!(providers.contains(&Provider::Bluesky));
    // The page lists news and posts under separate headings; that split is
    // this flag, not the ordering.
    assert!(hits.iter().any(|h| !h.provider.is_social()));
    assert!(hits.iter().any(|h| h.provider.is_social()));
}

#[tokio::test]
async fn the_request_carries_the_place_the_window_and_the_domain_filter() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let base = serve(move |target| {
        sink.lock().unwrap().push(target.to_owned());
        if target.contains("/doc") {
            json_ok(GDELT_BODY)
        } else {
            json_ok(BLUESKY_BODY)
        }
    })
    .await;
    let search = MediaSearch::new()
        .expect("build client")
        .with_gdelt_endpoint(format!("{base}/doc"))
        .with_bluesky_endpoint(format!("{base}/xrpc/app.bsky.feed.searchPosts"));
    search.search(&query()).await;

    let targets = seen.lock().unwrap().clone();
    let doc = targets
        .iter()
        .find(|t| t.contains("/doc"))
        .expect("doc call");
    assert!(doc.starts_with("GET "), "{doc}");
    assert!(doc.contains("colombia+earthquake"), "{doc}");
    // The domain filter is the whole reason this query finds video where the
    // ingest path's article query never does.
    assert!(doc.contains("domain%3Ayoutube.com"), "{doc}");
    assert!(doc.contains("startdatetime=20260810000000"), "{doc}");
    assert!(doc.contains("enddatetime=20260813063000"), "{doc}");
    // A quoted phrase inside the OR group is what GDELT rejects with a 200 and
    // a sentence of prose, so it must never reach the wire.
    assert!(!doc.contains("%22"), "quoted phrase on the wire: {doc}");

    let bsky = targets
        .iter()
        .find(|t| t.contains("xrpc"))
        .expect("bsky call");
    assert!(bsky.contains("q=colombia+earthquake"), "{bsky}");
    assert!(
        bsky.contains("since=2026-08-10T00%3A00%3A00%2B00%3A00"),
        "{bsky}"
    );
}

#[tokio::test]
async fn one_failing_provider_does_not_empty_the_other() {
    // GDELT rate-limited, Bluesky fine: the panel must still show the post and
    // say plainly which provider is missing.
    let search = both(
        http_body(
            "429 Too Many Requests",
            "text/plain",
            "retry-after: 30\r\n",
            "slow down",
        ),
        json_ok(BLUESKY_BODY),
    )
    .await;
    let (hits, problems) = search.search(&query()).await;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].provider, Provider::Bluesky);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("news:"), "{problems:?}");
}

#[tokio::test]
async fn a_rate_limit_carries_its_retry_after() {
    let search = both(
        http_body(
            "429 Too Many Requests",
            "text/plain",
            "retry-after: 30\r\n",
            "slow down",
        ),
        json_ok(BLUESKY_BODY),
    )
    .await;
    let err = search.gdelt(&query()).await.unwrap_err();
    assert!(
        matches!(
            err,
            SourceError::RateLimited {
                retry_after_secs: Some(30)
            }
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_gdelt_query_rejection_is_named_rather_than_read_as_no_results() {
    // DOC answers a malformed query with HTTP *200* and one line of prose. If
    // that ever reads as an empty result set, the page quietly claims nothing
    // was published about a place.
    let search = both(
        http_body(
            "200 OK",
            "text/plain",
            "",
            "One or more of your parenthetical clauses had an error in it.\n",
        ),
        json_ok(BLUESKY_BODY),
    )
    .await;
    let (hits, problems) = search.search(&query()).await;
    assert_eq!(hits.len(), 1, "the bluesky leg still returns");
    assert!(
        problems[0].contains("GDELT rejected the media query"),
        "{problems:?}"
    );
}

#[tokio::test]
async fn a_bluesky_error_payload_is_named_too() {
    let search = both(
        json_ok(GDELT_BODY),
        json_ok(r#"{"error":"InvalidRequest","message":"Error: q is required"}"#),
    )
    .await;
    let (hits, problems) = search.search(&query()).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].provider, Provider::Gdelt);
    assert!(
        problems[0].contains("Bluesky rejected the media query"),
        "{problems:?}"
    );
}

#[tokio::test]
async fn a_query_without_a_place_never_reaches_the_network() {
    let hits_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&hits_seen);
    let base = serve(move |_target| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        json_ok(GDELT_BODY)
    })
    .await;
    let search = MediaSearch::new()
        .expect("build client")
        .with_gdelt_endpoint(format!("{base}/doc"))
        .with_bluesky_endpoint(format!("{base}/xrpc/app.bsky.feed.searchPosts"));

    let mut query = query();
    query.place = "   ".into();
    let (hits, problems) = search.search(&query).await;
    assert!(hits.is_empty());
    assert_eq!(problems.len(), 1);
    assert_eq!(hits_seen.load(std::sync::atomic::Ordering::SeqCst), 0);
}
