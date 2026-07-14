//! Live Earth Signals — desktop app (Milestone 1: offline fixture mode).
//!
//! Media attention is an imperfect, biased proxy — not ground truth. The UI
//! keeps "media attention" and "event data" separated and shows score
//! components (M1: raw counts) rather than a single opaque number.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod ingest;
mod map_view;
mod panels;

use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
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
