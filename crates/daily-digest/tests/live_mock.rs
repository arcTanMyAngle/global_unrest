//! `live`-feature integration tests against a local mock Anthropic Messages
//! server: request shape, header auth, thinking-block filtering, refusal
//! handling, and 429/401 mapping. No real network, no API key — run with
//! `cargo test -p daily-digest --features live`.
#![cfg(feature = "live")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use daily_digest::{
    AnthropicDigester, AttentionFacts, DayKey, DigestError, DigestFacts, EventFact, EventFacts,
    HeadlineFact, MODEL, PlaceCount,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve canned HTTP responses. The handler sees `"METHOD /path?query"` plus
/// the raw request (headers + body) and returns a complete response via
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
                let mut buf = vec![0u8; 256 * 1024];
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

/// The JSON body of a captured request.
fn request_json(raw: &str) -> Value {
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    serde_json::from_str(body).expect("request body is JSON")
}

const TEST_KEY: &str = "sk-ant-test-DO-NOT-LEAK";

fn digester(base: &str) -> AnthropicDigester {
    AnthropicDigester::new(TEST_KEY.to_owned(), base.to_owned()).expect("build client")
}

fn facts() -> DigestFacts {
    DigestFacts {
        day_utc: DayKey::parse("2026-08-12").unwrap(),
        attention: AttentionFacts {
            records: 140,
            articles: 1_020,
            distinct_outlets: 61,
            top_places: vec![PlaceCount {
                country_iso: "KEN".into(),
                records: 30,
                articles: 210,
            }],
            headlines: vec![HeadlineFact {
                country_iso: "KEN".into(),
                outlet_domain: "example.test".into(),
                headline: "Transit strike enters second day".into(),
            }],
        },
        events: EventFacts {
            records: 22,
            by_kind: vec![("protest".into(), 16), ("disruption".into(), 6)],
            by_source: vec![("acled".into(), 16), ("ioda".into(), 6)],
            top_places: vec![PlaceCount {
                country_iso: "SDN".into(),
                records: 6,
                articles: 0,
            }],
            notable: vec![EventFact {
                country_iso: "SDN".into(),
                kind: "disruption".into(),
                source: "ioda".into(),
                label: Some("national outage".into()),
                severity: Some(0.75),
            }],
            counts_only_sources: vec![("acled".into(), 16)],
        },
    }
}

/// The two-section structured output, as the API returns it: a text block
/// whose content is JSON matching `output_config.format.schema`.
fn ok_response() -> String {
    let text = json!({
        "media_attention": "Coverage concentrated on Kenya across 61 outlets.",
        "event_data": "Twenty-two recorded events, sixteen of them protests."
    })
    .to_string();
    http_json(
        "200 OK",
        "",
        &json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": MODEL,
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}],
        })
        .to_string(),
    )
}

#[tokio::test]
async fn happy_path_returns_both_sections_separately() {
    let base = serve(|_target, _req| ok_response()).await;
    let digest = digester(&base)
        .generate(&facts(), 1_786_500_000)
        .await
        .expect("digest");

    assert_eq!(digest.day_utc.key(), "2026-08-12");
    assert_eq!(digest.model, MODEL);
    assert_eq!(digest.generated_at_epoch_s, 1_786_500_000);
    assert!(digest.media_attention.contains("61 outlets"));
    assert!(digest.event_data.contains("Twenty-two recorded events"));
    // The counts the prose was written against travel with it, so the page can
    // never show the text without the numbers behind it.
    assert_eq!(digest.attention_records, 140);
    assert_eq!(digest.event_records, 22);
}

#[tokio::test]
async fn request_carries_the_documented_headers_and_no_rejected_parameters() {
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let base = serve(move |target, req| {
        *sink.lock().unwrap() = format!("{target}\n{req}");
        ok_response()
    })
    .await;
    digester(&base).generate(&facts(), 0).await.expect("digest");

    let captured = seen.lock().unwrap().clone();
    let lower = captured.to_ascii_lowercase();
    assert!(captured.starts_with("POST /"), "posts: {captured}");
    assert!(lower.contains(&format!("x-api-key: {}", TEST_KEY.to_ascii_lowercase())));
    assert!(lower.contains("anthropic-version: 2023-06-01"));

    let body = request_json(&captured);
    assert_eq!(body["model"], MODEL);
    // These are 400-triggers on this model family; a regression here would
    // fail every real request while the mock tests stayed green if we only
    // asserted on the happy-path response.
    for banned in ["temperature", "top_p", "top_k", "thinking"] {
        assert!(body.get(banned).is_none(), "`{banned}` was sent");
    }
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    let required = body["output_config"]["format"]["schema"]["required"]
        .as_array()
        .expect("required list");
    assert_eq!(required.len(), 2);
    assert!(required.iter().any(|v| v == "media_attention"));
    assert!(required.iter().any(|v| v == "event_data"));
}

#[tokio::test]
async fn withheld_sources_reach_the_prompt_as_counts_but_never_as_rows() {
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let base = serve(move |_t, req| {
        *sink.lock().unwrap() = req.to_owned();
        ok_response()
    })
    .await;
    digester(&base).generate(&facts(), 0).await.expect("digest");

    let body = request_json(&seen.lock().unwrap());
    let prompt = body["messages"][0]["content"].as_str().expect("prompt");
    // ACLED's count is present…
    assert!(prompt.contains("acled=16"));
    // …but the only row-level entry is the permitted IODA one.
    let rows = prompt
        .split("event rows (structural fields only):")
        .nth(1)
        .expect("row section");
    assert!(rows.contains("national outage"));
    assert!(
        !rows.to_ascii_lowercase().contains("[acled]") && !rows.contains("via acled"),
        "ACLED rows must never be forwarded to a third party: {rows}"
    );
}

#[tokio::test]
async fn thinking_blocks_are_skipped_rather_than_parsed() {
    let base = serve(|_t, _r| {
        let text = json!({"media_attention": "A.", "event_data": "B."}).to_string();
        http_json(
            "200 OK",
            "",
            &json!({
                "stop_reason": "end_turn",
                "content": [
                    {"type": "thinking", "thinking": "{\"media_attention\": \"not this\"}"},
                    {"type": "text", "text": text},
                ],
            })
            .to_string(),
        )
    })
    .await;
    let digest = digester(&base).generate(&facts(), 0).await.expect("digest");
    assert_eq!(digest.media_attention, "A.");
    assert_eq!(digest.event_data, "B.");
}

#[tokio::test]
async fn a_refusal_is_reported_as_a_refusal_not_a_parse_error() {
    // Refusals arrive as HTTP 200 with empty content; reading `content[0]`
    // first would misreport this as malformed output.
    let base = serve(|_t, _r| {
        http_json(
            "200 OK",
            "",
            &json!({
                "stop_reason": "refusal",
                "stop_details": {"category": "policy"},
                "content": [],
            })
            .to_string(),
        )
    })
    .await;
    let err = digester(&base).generate(&facts(), 0).await.unwrap_err();
    assert!(matches!(err, DigestError::Refused(_)), "got {err:?}");
}

#[tokio::test]
async fn rate_limit_surfaces_the_retry_after_header() {
    let base = serve(|_t, _r| {
        http_json(
            "429 Too Many Requests",
            "retry-after: 42\r\n",
            &json!({"type": "error", "error": {"message": "rate_limit_error"}}).to_string(),
        )
    })
    .await;
    let err = digester(&base).generate(&facts(), 0).await.unwrap_err();
    assert!(
        matches!(
            err,
            DigestError::RateLimited {
                retry_after_secs: Some(42)
            }
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn bad_credentials_name_the_env_var_without_echoing_the_key() {
    let base = serve(|_t, _r| {
        http_json(
            "401 Unauthorized",
            "",
            &json!({"type": "error", "error": {"message": "invalid x-api-key"}}).to_string(),
        )
    })
    .await;
    let err = digester(&base).generate(&facts(), 0).await.unwrap_err();
    let text = err.to_string();
    assert!(text.contains("ANTHROPIC_API_KEY"), "{text}");
    assert!(
        !text.contains(TEST_KEY),
        "the key must never be echoed: {text}"
    );
}

#[tokio::test]
async fn an_empty_day_never_spends_an_api_call() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let base = serve(move |_t, _r| {
        counter.fetch_add(1, Ordering::SeqCst);
        ok_response()
    })
    .await;
    let err = digester(&base)
        .generate(&DigestFacts::default(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, DigestError::NoData(_)), "got {err:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
