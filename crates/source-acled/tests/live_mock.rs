//! `live`-feature integration tests against a local mock ACLED server:
//! OAuth grant, token caching, 401 re-auth, pagination, and 429 mapping.
//! No real network, no credentials — run with
//! `cargo test -p source-acled --features live`.
#![cfg(feature = "live")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use core_types::{RawRecord, SignalSource, SourceError, SourceFilters, TimeWindow};
use serde_json::{Value, json};
use source_acled::AcledSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve canned HTTP responses. The handler sees `"METHOD /path?query"` plus
/// the raw request (headers + body) and returns a complete response body via
/// [`http_json`].
async fn serve<F>(handler: F) -> String
where
    F: Fn(&str, &str) -> String + Send + Sync + 'static,
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
                let mut buf = vec![0u8; 64 * 1024];
                let mut n = 0;
                // Read until the header block and any Content-Length body arrive.
                loop {
                    match sock.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(r) => n += r,
                    }
                    let text = String::from_utf8_lossy(&buf[..n]);
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let body_len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if n >= head_end + 4 + body_len {
                            break;
                        }
                    }
                    if n == buf.len() {
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
                let resp = handler(&target, &req);
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A complete HTTP/1.1 response with a JSON body.
fn http_json(status: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn token_body(access: &str) -> String {
    json!({
        "token_type": "Bearer",
        "expires_in": 86400,
        "access_token": access,
        "refresh_token": "refresh-1",
    })
    .to_string()
}

fn row(id: &str) -> Value {
    json!({
        "event_id_cnty": id,
        "event_date": "2026-07-10",
        "disorder_type": "Demonstrations",
        "event_type": "Protests",
        "sub_event_type": "Peaceful protest",
        "iso": "404",
        "country": "Kenya",
        "admin1": "Nairobi",
        "latitude": "-1.2864",
        "longitude": "36.8172",
        "geo_precision": "1",
        "source": "Outlet One",
        "fatalities": "0"
    })
}

fn data_body(rows: &[Value]) -> String {
    json!({"status": 200, "success": true, "count": rows.len(), "data": rows}).to_string()
}

fn window() -> TimeWindow {
    TimeWindow::new(
        chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        chrono::DateTime::parse_from_rfc3339("2026-07-15T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
}

fn source_against(base: &str) -> AcledSource {
    AcledSource::new("user@example.test", "hunter2")
        .unwrap()
        .with_token_url(format!("{base}/oauth/token"))
        .with_read_url(format!("{base}/api/acled/read"))
}

#[tokio::test]
async fn authenticates_then_pages_until_a_short_page() {
    let tokens = Arc::new(AtomicUsize::new(0));
    let t = Arc::clone(&tokens);
    let base = serve(move |target, req| {
        if target.starts_with("POST /oauth/token") {
            t.fetch_add(1, Ordering::SeqCst);
            assert!(
                req.contains("grant_type=password"),
                "expected password grant"
            );
            assert!(req.contains("client_id=acled"));
            return http_json("200 OK", "", &token_body("tok-1"));
        }
        assert!(
            req.contains("authorization: Bearer tok-1")
                || req.contains("Authorization: Bearer tok-1"),
            "read must carry the bearer token: {req}"
        );
        // Page 1 is full (2 rows at limit 2) → page 2 is short → stop.
        if target.contains("page=1") {
            http_json("200 OK", "", &data_body(&[row("KEN1"), row("KEN2")]))
        } else if target.contains("page=2") {
            http_json("200 OK", "", &data_body(&[row("KEN3")]))
        } else {
            panic!("unexpected page request: {target}")
        }
    })
    .await;

    let src = source_against(&base).with_page_limit(2);
    let raws = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(raws.len(), 3);
    assert_eq!(tokens.load(Ordering::SeqCst), 1, "exactly one auth call");
    assert!(matches!(&raws[0], RawRecord::AcledJson(_)));

    // The adapter's normalize handles what its fetch produced.
    let events = src.normalize(&raws[0]).unwrap();
    assert_eq!(events[0].country_iso, "KEN");
    assert_eq!(events[0].kind, core_types::EventKind::Protest);
}

#[tokio::test]
async fn second_fetch_reuses_the_cached_token() {
    let tokens = Arc::new(AtomicUsize::new(0));
    let t = Arc::clone(&tokens);
    let base = serve(move |target, _req| {
        if target.starts_with("POST /oauth/token") {
            t.fetch_add(1, Ordering::SeqCst);
            http_json("200 OK", "", &token_body("tok-1"))
        } else {
            http_json("200 OK", "", &data_body(&[row("KEN1")]))
        }
    })
    .await;

    let src = source_against(&base);
    src.fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    src.fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(
        tokens.load(Ordering::SeqCst),
        1,
        "token must be cached across fetches"
    );
}

#[tokio::test]
async fn stale_token_401_triggers_one_reauth() {
    let tokens = Arc::new(AtomicUsize::new(0));
    let t = Arc::clone(&tokens);
    let base = serve(move |target, req| {
        if target.starts_with("POST /oauth/token") {
            let n = t.fetch_add(1, Ordering::SeqCst);
            return http_json("200 OK", "", &token_body(&format!("tok-{}", n + 1)));
        }
        // The first token is rejected (server-side revocation); the second works.
        if req.contains("Bearer tok-1") {
            http_json("401 Unauthorized", "", "{}")
        } else {
            http_json("200 OK", "", &data_body(&[row("KEN1")]))
        }
    })
    .await;

    let src = source_against(&base);
    let raws = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(raws.len(), 1);
    assert_eq!(
        tokens.load(Ordering::SeqCst),
        2,
        "one re-auth after the 401"
    );
}

#[tokio::test]
async fn rate_limit_maps_to_rate_limited_with_retry_after() {
    let base = serve(move |target, _req| {
        if target.starts_with("POST /oauth/token") {
            http_json("200 OK", "", &token_body("tok-1"))
        } else {
            http_json("429 Too Many Requests", "retry-after: 120\r\n", "{}")
        }
    })
    .await;

    let src = source_against(&base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SourceError::RateLimited {
                retry_after_secs: Some(120)
            }
        ),
        "expected RateLimited(120), got {err:?}"
    );
}

#[tokio::test]
async fn bad_credentials_surface_a_clear_error() {
    let base = serve(move |target, _req| {
        assert!(target.starts_with("POST /oauth/token"));
        http_json("401 Unauthorized", "", r#"{"error":"invalid_grant"}"#)
    })
    .await;

    let src = source_against(&base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("ACLED_EMAIL"),
        "auth error should point at the env vars: {text}"
    );
    assert!(
        !text.contains("hunter2"),
        "password must never leak: {text}"
    );
}

#[test]
fn read_url_covers_the_window_inclusively() {
    let src = AcledSource::new("user@example.test", "pw")
        .unwrap()
        .with_page_limit(100);
    let url = src.read_url_for(window(), 3).unwrap();
    let q = url.query().unwrap();
    // Half-open [Jul 1, Jul 15) → inclusive BETWEEN Jul 1 .. Jul 14.
    assert!(q.contains("event_date=2026-07-01%7C2026-07-14"), "{q}");
    assert!(q.contains("event_date_where=BETWEEN"), "{q}");
    assert!(q.contains("limit=100"), "{q}");
    assert!(q.contains("page=3"), "{q}");
}
