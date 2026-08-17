//! Manual check against the real public APIs — never part of `cargo test`.
//!
//! ```sh
//! cargo run -p media-search --features live --example media_live_probe -- Colombia earthquake 72
//! ```
//!
//! Exists because both legs fail *quietly* in ways an offline golden test
//! cannot reproduce: GDELT DOC rejects a malformed query with HTTP 200 and a
//! sentence of prose, and Bluesky's search silently returns nothing when a
//! cursor/filter pair is wrong. This prints exactly what came back, so a
//! change to either query shape can be confirmed before it reaches the UI.
//!
//! It prints URLs, which is the point: this is the on-demand, place-scoped
//! lookup path, not the ingest path. Nothing here is written anywhere.

// Same shape as the other crates' live probes: without the feature there is
// no network half to run, and an ordinary `cargo test --workspace` still
// builds every example.
#[cfg(not(feature = "live"))]
fn main() {
    eprintln!("build with --features live");
}

#[cfg(feature = "live")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Duration, Utc};
    use media_search::{MediaQuery, MediaSearch};

    let mut args = std::env::args().skip(1);
    let place = args.next().unwrap_or_else(|| "Colombia".to_string());
    // `-` means "no topic": a shell that drops empty arguments (PowerShell
    // does) would otherwise shift the window count into the topic slot.
    let topic = match args.next().unwrap_or_default().as_str() {
        "-" => String::new(),
        t => t.to_string(),
    };
    let hours: i64 = args
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or(72)
        .clamp(1, 720);

    let end = Utc::now();
    let query = MediaQuery {
        place: place.clone(),
        topic: topic.clone(),
        start: end - Duration::hours(hours),
        end,
        limit: 25,
    };
    println!("place={place:?} topic={topic:?} window={hours}h");
    println!(
        "gdelt query expression: {}",
        media_search::gdelt::query_expression(&place, &topic).unwrap_or_default()
    );

    let search = MediaSearch::new()?;

    // Each leg is reported on its own so a rate-limited provider is visibly
    // distinct from a provider that answered with nothing.
    for (label, result) in [
        ("gdelt", search.gdelt(&query).await),
        ("bluesky", search.bluesky(&query).await),
    ] {
        match result {
            Ok(hits) => {
                println!("\n--- {label}: {} hits ---", hits.len());
                for hit in hits {
                    println!(
                        "  {} [{}] {}\n      {}",
                        hit.ts_utc.format("%m-%d %H:%M"),
                        hit.origin,
                        hit.title,
                        hit.url
                    );
                }
            }
            Err(e) => println!("\n--- {label}: FAILED: {e} ---"),
        }
    }
    Ok(())
}
