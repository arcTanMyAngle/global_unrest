//! Live Earth Signals — live-data-only desktop app.
//!
//! Media attention is an imperfect, biased proxy — not ground truth. The UI
//! keeps "media attention" and "event data" separated and shows score
//! components (M1: raw counts) rather than a single opaque number.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod daily_events;
mod digest;
mod how_to_read;
mod ingest;
mod map_view;
mod media;
mod media_page;
mod panels;
mod sparkline;
mod style;
mod timeline_strip;
mod video;

use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    // Local development credentials/config remain in the gitignored `.env`.
    // Existing process environment variables win, so deployments can keep
    // injecting secrets without a file.
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename(".env.local");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn")),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Live Earth Signals")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "live-earth-signals",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)?))),
    )
}
