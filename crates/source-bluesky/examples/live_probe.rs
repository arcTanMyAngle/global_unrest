//! Manual live check against the real Jetstream firehose.
//!
//! ```sh
//! cargo run -p source-bluesky --features live --example live_probe -- 60
//! ```
//!
//! Runs the real stream task for N seconds (default 60) and prints only
//! aggregate output: scan/match totals and the drained rollups. It never
//! prints, stores, or returns post text or author identity — the same
//! guarantee the source itself makes. Not part of CI; this exists so a
//! change to the matcher can be sanity-checked against real traffic.

#[cfg(not(feature = "live"))]
fn main() {
    eprintln!("build with --features live");
}

#[cfg(feature = "live")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use core_types::{SignalSource, SourceFilters, TimeWindow};
    use source_bluesky::BlueskySource;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(60);

    let source = BlueskySource::from_env().expect("build source");
    let stream = source.spawn_stream();
    println!("streaming for {secs}s...");
    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;

    let now = chrono::Utc::now();
    let window = TimeWindow::new(now - chrono::Duration::seconds(secs as i64), now);
    let raws = source
        .fetch(window, &SourceFilters::default())
        .await
        .expect("drain");

    let (scanned, matched) = source.stats();
    let rate = if scanned > 0 {
        100.0 * matched as f64 / scanned as f64
    } else {
        0.0
    };
    println!("\nscanned {scanned} posts, matched {matched} ({rate:.3}%)");
    println!("{} rollups:", raws.len());
    let mut events = 0usize;
    for raw in &raws {
        match source.normalize(raw) {
            Ok(evs) => {
                for e in &evs {
                    // Window included: the same place+topic legitimately
                    // appears twice when a run straddles a flush boundary.
                    println!(
                        "  {:>4} posts  {:<12} {:<12} {:<8} window={}  {}",
                        e.article_count,
                        e.themes.get(1).cloned().unwrap_or_default(),
                        e.country_iso,
                        e.location_precision.label(),
                        e.ts_utc.format("%H:%M:%S"),
                        e.headline.clone().unwrap_or_default(),
                    );
                }
                events += evs.len();
            }
            Err(e) => println!("  normalize failed: {e}"),
        }
    }
    println!("{events} events normalized");
    stream.abort();
}
