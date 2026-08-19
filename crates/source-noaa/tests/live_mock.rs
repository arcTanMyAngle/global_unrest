//! `live`-feature integration tests against a local mock NOAA/NWS server:
//! query params, GeoJSON `features` parsing, empty/missing `features`, and
//! 429 mapping. No real network — run with
//! `cargo test -p source-noaa --features live`.
#![cfg(feature = "live")]

use std::sync::Arc;

use core_types::{RawRecord, SignalSource, SourceError, SourceFilters, TimeWindow};
use serde_json::{Value, json};
use source_noaa::NoaaSource;
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
        "HTTP/1.1 {status}\r\ncontent-type: application/geo+json\r\n{extra_headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn feature(id: &str) -> Value {
    json!({
        "id": format!("https://api.weather.gov/alerts/{id}"),
        "type": "Feature",
        "geometry": {
            "type": "Polygon",
            "coordinates": [[
                [-122.6, 38.2], [-122.2, 38.2], [-122.2, 38.6], [-122.6, 38.6], [-122.6, 38.2]
            ]]
        },
        "properties": {
            "id": id,
            "event": "Flood Warning",
            "severity": "Severe",
            "onset": "2026-07-10T06:00:00-07:00",
            "geocode": { "UGC": ["CAZ018"] }
        }
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
async fn fetch_requests_actual_alerts_and_normalizes_them() {
    let base = serve(|target, req| {
        assert!(target.starts_with("GET /alerts/active"), "{target}");
        assert!(target.contains("status=actual"), "{target}");
        assert!(target.contains("message_type=alert%2Cupdate"), "{target}");
        assert!(
            req.contains("accept: application/geo+json")
                || req.contains("Accept: application/geo+json"),
            "expected geo+json accept header: {req}"
        );
        http_json(
            "200 OK",
            "",
            &json!({"features": [feature("urn:1"), feature("urn:2")]}).to_string(),
        )
    })
    .await;

    let src = NoaaSource::new()
        .unwrap()
        .with_endpoint(format!("{base}/alerts/active"));
    let raws = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert_eq!(raws.len(), 2);
    assert!(matches!(&raws[0], RawRecord::NoaaAlertJson(_)));

    let events = src.normalize(&raws[0]).unwrap();
    assert_eq!(events[0].source_event_id, "urn:1");
    assert_eq!(events[0].family, core_types::SignalFamily::OfficialAlert);
    assert_eq!(events[0].kind, core_types::EventKind::Alert);
}

#[tokio::test]
async fn missing_features_field_yields_no_records() {
    let base = serve(|_target, _req| http_json("200 OK", "", &json!({}).to_string())).await;

    let src = NoaaSource::new().unwrap().with_endpoint(base);
    let raws = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap();
    assert!(raws.is_empty());
}

#[tokio::test]
async fn non_array_features_is_an_error() {
    let base = serve(|_target, _req| {
        http_json(
            "200 OK",
            "",
            &json!({"features": "not-an-array"}).to_string(),
        )
    })
    .await;

    let src = NoaaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::Other(_)), "{err:?}");
}

#[tokio::test]
async fn rate_limit_maps_to_rate_limited_with_retry_after() {
    let base =
        serve(|_target, _req| http_json("429 Too Many Requests", "retry-after: 90\r\n", "{}"))
            .await;

    let src = NoaaSource::new().unwrap().with_endpoint(base);
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

    let src = NoaaSource::new().unwrap().with_endpoint(base);
    let err = src
        .fetch(window(), &SourceFilters::default())
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::Http(_)), "{err:?}");
}
