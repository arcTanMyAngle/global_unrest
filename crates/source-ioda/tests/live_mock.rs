//! `live`-feature integration tests against a local mock IODA server:
//! query params, `data` parsing, empty/missing `data`, API errors, and 429
//! mapping. No real network — run with
//! `cargo test -p source-ioda --features live`.
#![cfg(feature = "live")]

use std::sync::Arc;

use core_types::{RawRecord, SignalSource, SourceError, SourceFilters, TimeWindow};
use serde_json::{Value, json};
use source_ioda::IodaSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve one canned HTTP response for every accepted connection. The handler
/// sees `"METHOD /path?query"` plus the raw request (headers) and returns a
/// complete response body via [`http_json`].
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
                loop {
                    match sock.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(r) => n += r,
                    }
                    let text = String::from_utf8_lossy(&buf[..n]);
                    if text.find("\r\n\r\n").is_some() {
                        break; // GET requests have no body to wait for.
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

fn outage(country: &str, start: i64) -> Value {
    json!({
        "location": format!("country/{country}"),
        "location_name": "United States",
        "start": start,
        "duration": 1800,
        "uncertainty": null,
        "method": "median",
        "datasource": "ping-slash24",
        "status": 0,
        "fraction": null,
        "score": 753.1987405640424,
        "overlaps_window": false
    })
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

#[tokio::test]
async fn fetch_requests_country_codf_window_and_normalizes_outages() {
    let expected = window();
    let expected_from = expected.start.timestamp().to_string();
    let expected_until = expected.end.timestamp().to_string();
    let base = serve(move |target, _req| {
        assert!(target.starts_with("GET /outages/events?"), "{target}");
        assert!(target.contains("entityType=country"), "{target}");
        assert!(
            target.contains(&format!("from={expected_from}")),
            "{target}"
        );
        assert!(
            target.contains(&format!("until={expected_until}")),
            "{target}"
        );
        assert!(target.contains("format=codf"), "{target}");
        http_json(
            "200 OK",
            "",
            &json!({"data": [outage("US", 1_754_811_000), outage("CA", 1_754_812_800)]})
                .to_string(),
        )
    })
    .await;

    let src = IodaSource::new()
        .unwrap()
        .with_endpoint(format!("{base}/outages/events"));
    let raws = src
        .fetch(expected, &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(raws.len(), 2);
    assert!(matches!(&raws[0], RawRecord::IodaEventJson(_)));

    let events = src.normalize(&raws[0]).unwrap();
    assert_eq!(
        events[0].source_event_id,
        "US-1754811000-ping-slash24-median"
    );
    assert_eq!(events[0].kind, core_types::EventKind::Disruption);
    assert_eq!(
        events[0].location_precision,
        core_types::LocationPrecision::Country
    );
    assert_eq!(events[0].country_iso, "USA");
}

#[tokio::test]
async fn missing_or_null_data_yields_no_records() {
    for body in [json!({}), json!({"data": null})] {
        let body = body.to_string();
        let base = serve(move |_target, _req| http_json("200 OK", "", &body)).await;
        let src = IodaSource::new().unwrap().with_endpoint(base);
        let raws = src
            .fetch(window(), &SourceFilters::default())
            .await
            .unwrap();
        assert!(raws.is_empty());
    }
}

#[tokio::test]
async fn non_array_data_is_an_error() {
    let base = serve(|_target, _req| {
        http_json("200 OK", "", &json!({"data": "not-an-array"}).to_string())
    })
    .await;

    let src = IodaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::Other(_)), "{err:?}");
}

#[tokio::test]
async fn api_error_field_is_an_error() {
    let base = serve(|_target, _req| {
        http_json(
            "200 OK",
            "",
            &json!({"error": "invalid interval"}).to_string(),
        )
    })
    .await;

    let src = IodaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::Other(_)), "{err:?}");
    assert!(err.to_string().contains("invalid interval"), "{err}");
}

#[tokio::test]
async fn rate_limit_maps_to_rate_limited_with_retry_after() {
    let base =
        serve(|_target, _req| http_json("429 Too Many Requests", "retry-after: 90\r\n", "{}"))
            .await;

    let src = IodaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SourceError::RateLimited {
                retry_after_secs: Some(90)
            }
        ),
        "expected RateLimited(90), got {err:?}"
    );
}

#[tokio::test]
async fn server_error_maps_to_http_error() {
    let base = serve(|_target, _req| http_json("503 Service Unavailable", "", "{}")).await;

    let src = IodaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::Http(_)), "{err:?}");
}
