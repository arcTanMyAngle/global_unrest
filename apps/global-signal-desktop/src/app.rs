//! App state machine: ingest → storage → queries → layers → panels.
//!
//! The UI thread never blocks: every storage call returns a `Reply` that is
//! polled once per frame, and the storage actor requests a repaint whenever
//! a reply lands.

use std::sync::mpsc;

use core_types::{
    BUCKET_SECS, EventKind, GeoTemporalEvent, IngestFailure, RegionBucket, bucket_start_epoch,
};
use geo_utils::CountryIndex;
use renderer::{BasemapLayer, HeatmapLayer, MapStyle, MarkerInput, MarkerLayer};
use serde::{Deserialize, Serialize};
use storage::{
    EpochWindow, EventPoint, ExportReport, IngestLogRow, IngestReport, RegionDetail, Reply,
    SettingsDb, StorageHandle,
};

use crate::ingest::{self, IngestHandle, IngestMsg, SourceStatus};
use crate::map_view::MapView;

/// Natural Earth 1:110m countries (public domain; attribution in README).
pub const NE_COUNTRIES: &str =
    include_str!("../../../assets/natural_earth/ne_110m_admin_0_countries.geojson");

pub enum Phase {
    Loading(String),
    Ready,
    Error(String),
}

/// One normalized batch awaiting ingest (events + normalization failures).
type Batch = (Vec<GeoTemporalEvent>, Vec<IngestFailure>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeatMetric {
    Attention,
    Events,
    /// Peak distinct outlet domains in any 6 h bucket of the window.
    Diversity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    pub protest: bool,
    pub conflict: bool,
    pub disruption: bool,
    pub other: bool,
    /// Show news-attention observations as point markers too (the heatmap
    /// always carries attention; markers default to discrete events only).
    pub attention_markers: bool,
    pub min_confidence: f32,
    pub show_heatmap: bool,
    pub show_markers: bool,
    pub heat_metric: HeatMetric,
    /// Selected themes; empty = no theme filtering. `serde(default)` keeps
    /// settings saved before M2 loadable.
    #[serde(default)]
    pub themes: Vec<String>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            protest: true,
            conflict: true,
            disruption: true,
            other: true,
            attention_markers: false,
            min_confidence: 0.0,
            show_heatmap: true,
            show_markers: true,
            heat_metric: HeatMetric::Attention,
            themes: Vec::new(),
        }
    }
}

impl Filters {
    pub fn kinds_for_query(&self) -> Vec<EventKind> {
        let mut kinds = Vec::new();
        if self.protest {
            kinds.push(EventKind::Protest);
        }
        if self.conflict {
            kinds.push(EventKind::Conflict);
        }
        if self.disruption {
            kinds.push(EventKind::Disruption);
        }
        if self.other {
            kinds.push(EventKind::Other);
        }
        if self.attention_markers {
            kinds.push(EventKind::NewsAttention);
        }
        kinds
    }
}

/// Timeline window length in buckets (6h each); `None` = whole extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLen {
    H6,
    D1,
    D3,
    D7,
    All,
}

impl WindowLen {
    pub const CHOICES: [WindowLen; 5] = [
        WindowLen::H6,
        WindowLen::D1,
        WindowLen::D3,
        WindowLen::D7,
        WindowLen::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WindowLen::H6 => "6 hours",
            WindowLen::D1 => "1 day",
            WindowLen::D3 => "3 days",
            WindowLen::D7 => "7 days",
            WindowLen::All => "all data",
        }
    }

    pub fn buckets(self, total: i64) -> i64 {
        match self {
            WindowLen::H6 => 1,
            WindowLen::D1 => 4,
            WindowLen::D3 => 12,
            WindowLen::D7 => 28,
            WindowLen::All => total,
        }
        .min(total.max(1))
    }
}

pub struct Timeline {
    pub len: WindowLen,
    pub start_bucket: i64,
    pub playing: bool,
    pub accum: f32,
}

pub struct App {
    pub store: StorageHandle,
    pub settings: SettingsDb,
    pub map: MapView,
    pub countries: CountryIndex,
    data_dir: std::path::PathBuf,

    pending_export: Option<Reply<ExportReport>>,
    /// Human-readable outcome of the last Parquet export, for the status UI.
    pub export_status: Option<String>,

    pub phase: Phase,
    ingest_rx: Option<mpsc::Receiver<IngestMsg>>,
    ingest_handle: IngestHandle,
    /// Batches waiting to be handed to the storage actor (one ingest in flight
    /// at a time). Fed by fixture load + live GDELT cycles.
    ingest_queue: std::collections::VecDeque<Batch>,
    /// Live GDELT online mode; drives the ingest worker.
    pub online: bool,
    /// Latest live-source status for the UI.
    pub source_status: Option<SourceStatus>,
    /// Events-table retention cap in days (`None` = keep everything). Applied to
    /// the storage actor; persisted in settings.
    pub retention_days: Option<u32>,
    pending_ingest: Option<Reply<IngestReport>>,
    pub ingest_report: Option<IngestReport>,
    pending_log: Option<Reply<(u64, Vec<IngestLogRow>)>>,
    pub ingest_log: Option<(u64, Vec<IngestLogRow>)>,
    pub show_log_window: bool,

    pending_extent: Option<Reply<Option<EpochWindow>>>,
    /// Bucket-aligned data extent `[start, end)`.
    pub extent: Option<EpochWindow>,

    pending_vocab: Option<Reply<Vec<(String, u32)>>>,
    /// Distinct themes with usage counts, most-used first (from the data).
    pub theme_vocab: Option<Vec<(String, u32)>>,

    pub timeline: Timeline,
    pub filters: Filters,
    last_saved_filters: Filters,
    dirty: bool,

    pending_buckets: Option<Reply<Vec<RegionBucket>>>,
    pending_points: Option<Reply<Vec<EventPoint>>>,
    pub bucket_count: usize,
    /// Buckets of the current window, kept so the heatmap can re-aggregate
    /// at a different H3 rollup resolution when the zoom crosses a threshold
    /// without re-querying storage.
    window_buckets: Vec<RegionBucket>,
    /// H3 resolution the heatmap layer was last built at.
    heat_res: u8,

    pub selected_cell: Option<u64>,
    pub selected_label: Option<String>,
    pending_detail: Option<Reply<RegionDetail>>,
    pub detail: Option<RegionDetail>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let style = MapStyle::default();
        let basemap = BasemapLayer::from_geojson_str(NE_COUNTRIES, &style)
            .map_err(|e| anyhow::anyhow!("basemap: {e}"))?;
        let countries = CountryIndex::from_geojson_str(NE_COUNTRIES)
            .map_err(|e| anyhow::anyhow!("country index: {e}"))?;

        let data_dir = match std::env::var("LES_DATA_DIR") {
            Ok(d) => std::path::PathBuf::from(d),
            Err(_) => {
                directories::ProjectDirs::from("org", "LiveEarthSignals", "live-earth-signals")
                    .map(|d| d.data_local_dir().to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("var"))
            }
        };
        tracing::info!(dir = %data_dir.display(), "data directory");

        let ctx = cc.egui_ctx.clone();
        let store = StorageHandle::open(
            Some(data_dir.join("signals.duckdb")),
            Box::new(move || ctx.request_repaint()),
        )?;
        let settings = SettingsDb::open(&data_dir.join("settings.sqlite"))?;
        let filters: Filters = settings.get("filters")?.unwrap_or_default();

        // Retention: env override wins, else the saved setting, else unbounded.
        let retention_days: Option<u32> = match std::env::var("LES_RETENTION_DAYS") {
            Ok(s) => s.trim().parse::<u32>().ok().filter(|d| *d > 0),
            Err(_) => settings.get("retention_days")?.flatten(),
        };
        store.set_retention(retention_days);

        // Always spawn the worker; if fixtures are missing it reports the fatal
        // error itself (keeps the online-mode handle unconditional).
        let fixtures_dir =
            ingest::find_fixtures_dir().unwrap_or_else(|| std::path::PathBuf::from("fixtures"));
        let ctx = cc.egui_ctx.clone();
        let (ingest_rx, ingest_handle) = ingest::spawn(fixtures_dir, move || ctx.request_repaint());
        let phase = Phase::Loading("loading fixtures…".into());

        let mut app = Self {
            store,
            settings,
            map: MapView::new(basemap, style),
            countries,
            data_dir,
            pending_export: None,
            export_status: None,
            phase,
            ingest_rx: Some(ingest_rx),
            ingest_handle,
            ingest_queue: std::collections::VecDeque::new(),
            online: false,
            source_status: None,
            retention_days,
            pending_ingest: None,
            ingest_report: None,
            pending_log: None,
            ingest_log: None,
            show_log_window: false,
            pending_extent: None,
            extent: None,
            pending_vocab: None,
            theme_vocab: None,
            timeline: Timeline {
                len: WindowLen::D1,
                start_bucket: 0,
                playing: false,
                accum: 0.0,
            },
            last_saved_filters: filters.clone(),
            filters,
            dirty: false,
            pending_buckets: None,
            pending_points: None,
            bucket_count: 0,
            window_buckets: Vec::new(),
            heat_res: core_types::H3_RESOLUTION,
            selected_cell: None,
            selected_label: None,
            pending_detail: None,
            detail: None,
        };

        // Optional auto-start of live mode (headless verification/automation).
        if std::env::var("LES_ONLINE").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes")) {
            app.set_online(true);
        }
        Ok(app)
    }

    pub fn total_buckets(&self) -> i64 {
        self.extent
            .map(|(s, e)| ((e - s) / BUCKET_SECS).max(1))
            .unwrap_or(0)
    }

    pub fn current_window(&self) -> Option<EpochWindow> {
        let (start, _) = self.extent?;
        let len = self.timeline.len.buckets(self.total_buckets());
        let ws = start + self.timeline.start_bucket * BUCKET_SECS;
        Some((ws, ws + len * BUCKET_SECS))
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Toggle live GDELT online mode and tell the ingest worker.
    pub fn set_online(&mut self, on: bool) {
        self.online = on;
        self.ingest_handle.set_online(on);
    }

    /// Request an immediate live fetch (manual refresh; only acts when online).
    pub fn fetch_now(&self) {
        self.ingest_handle.fetch_now();
    }

    /// Change the events retention cap: apply to storage and persist. The next
    /// ingest prunes to the new window.
    pub fn set_retention(&mut self, days: Option<u32>) {
        if self.retention_days == days {
            return;
        }
        self.retention_days = days;
        self.store.set_retention(days);
        if let Err(e) = self.settings.set("retention_days", &days) {
            tracing::warn!("saving retention: {e}");
        }
    }

    /// Kick off a Parquet session export into a fresh timestamped directory
    /// under the app data dir (the M4 handoff layout).
    pub fn start_export(&mut self) {
        if self.pending_export.is_some() {
            return;
        }
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let dir = self
            .data_dir
            .join("exports")
            .join(format!("session-{stamp}"));
        self.export_status = Some("exporting…".into());
        self.pending_export = Some(self.store.export_parquet(dir));
    }

    /// Poll every async reply; drive the phase machine.
    fn poll_async(&mut self) {
        // 1a. Drain all worker messages: queue batches, apply status, note a
        // fatal fixture failure. The worker stays alive for live GDELT cycles.
        if let Some(rx) = &self.ingest_rx {
            loop {
                match rx.try_recv() {
                    Ok(IngestMsg::Loaded {
                        events,
                        failures,
                        origin,
                    }) => {
                        tracing::debug!(origin, events = events.len(), "batch queued for ingest");
                        self.ingest_queue.push_back((events, failures));
                    }
                    Ok(IngestMsg::Status(status)) => {
                        self.online = status.online;
                        self.source_status = Some(status);
                    }
                    Ok(IngestMsg::Failed(msg)) => {
                        if !matches!(self.phase, Phase::Ready) {
                            self.phase = Phase::Error(msg);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.ingest_rx = None;
                        break;
                    }
                }
            }
        }

        // 1b. One ingest in flight at a time: hand the next queued batch to the
        // storage actor. New live batches wait their turn (no double-ingest).
        if self.pending_ingest.is_none()
            && let Some((events, failures)) = self.ingest_queue.pop_front()
        {
            if !matches!(self.phase, Phase::Ready) {
                self.phase = Phase::Loading("storing events…".into());
            }
            self.pending_ingest = Some(self.store.ingest(events, failures));
        }

        // 2. Storage ingest finished → learn the data extent + failure log.
        if let Some(reply) = &self.pending_ingest
            && let Some(result) = reply.try_take()
        {
            self.pending_ingest = None;
            match result {
                Ok(report) => {
                    self.ingest_report = Some(report);
                    self.pending_extent = Some(self.store.time_extent());
                    self.pending_log = Some(self.store.ingest_log(20));
                    self.pending_vocab = Some(self.store.theme_vocab());
                }
                Err(e) => self.phase = Phase::Error(format!("ingest: {e}")),
            }
        }

        if let Some(reply) = &self.pending_extent
            && let Some(result) = reply.try_take()
        {
            self.pending_extent = None;
            match result {
                Ok(Some((min_ts, max_ts))) => {
                    let start = bucket_start_epoch(min_ts);
                    let end = bucket_start_epoch(max_ts - 1) + BUCKET_SECS;
                    self.extent = Some((start, end));
                    let total = self.total_buckets();
                    let len = self.timeline.len.buckets(total);
                    self.timeline.start_bucket = (total - len).max(0);
                    self.phase = Phase::Ready;
                    self.dirty = true;
                }
                Ok(None) => {
                    self.phase = Phase::Error("store is empty after ingest".into());
                }
                Err(e) => self.phase = Phase::Error(format!("extent: {e}")),
            }
        }

        if let Some(reply) = &self.pending_log
            && let Some(result) = reply.try_take()
        {
            self.pending_log = None;
            if let Ok(log) = result {
                self.ingest_log = Some(log);
            }
        }

        if let Some(reply) = &self.pending_vocab
            && let Some(result) = reply.try_take()
        {
            self.pending_vocab = None;
            match result {
                Ok(vocab) => self.theme_vocab = Some(vocab),
                Err(e) => tracing::error!("theme vocab: {e}"),
            }
        }

        if let Some(reply) = &self.pending_export
            && let Some(result) = reply.try_take()
        {
            self.pending_export = None;
            self.export_status = Some(match result {
                Ok(r) => format!("exported {} events → {}", r.events, r.dir.display()),
                Err(e) => format!("export failed: {e}"),
            });
        }

        // 3. Window queries → rebuild layers.
        if let Some(reply) = &self.pending_buckets
            && let Some(result) = reply.try_take()
        {
            self.pending_buckets = None;
            match result {
                Ok(buckets) => {
                    self.window_buckets = buckets;
                    self.rebuild_heatmap();
                }
                Err(e) => tracing::error!("bucket query: {e}"),
            }
        }
        if let Some(reply) = &self.pending_points
            && let Some(result) = reply.try_take()
        {
            self.pending_points = None;
            match result {
                Ok(points) => self.rebuild_markers(points),
                Err(e) => tracing::error!("point query: {e}"),
            }
        }
        if let Some(reply) = &self.pending_detail
            && let Some(result) = reply.try_take()
        {
            self.pending_detail = None;
            match result {
                Ok(detail) => self.detail = Some(detail),
                Err(e) => tracing::error!("detail query: {e}"),
            }
        }
    }

    fn fire_queries(&mut self) {
        let Some(window) = self.current_window() else {
            return;
        };
        let themes = (!self.filters.themes.is_empty()).then(|| self.filters.themes.clone());
        self.pending_buckets = Some(self.store.query_buckets(window, themes.clone()));
        self.pending_points = Some(self.store.query_points(
            window,
            Some(self.filters.kinds_for_query()),
            themes,
            self.filters.min_confidence,
        ));
        if let Some(cell) = self.selected_cell {
            self.pending_detail = Some(self.store.region_detail(cell, window));
        }
    }

    /// Heatmap display resolution for a zoom level: res-3 cells shrink to a
    /// few pixels at world zoom, so roll up to coarser H3 parents (derived
    /// via `geo_utils::cell_parent`; only res 3 is ever stored).
    fn heat_resolution(deg_per_px: f64) -> u8 {
        if deg_per_px >= 0.25 {
            1
        } else if deg_per_px >= 0.08 {
            2
        } else {
            core_types::H3_RESOLUTION
        }
    }

    fn rebuild_heatmap(&mut self) {
        let buckets = &self.window_buckets;
        self.bucket_count = buckets.len();
        let deg_per_px = self.map.viewport.as_ref().map_or(0.225, |v| v.deg_per_px);
        self.heat_res = Self::heat_resolution(deg_per_px);
        let mut per_cell: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        for b in buckets {
            let Ok(cell) = geo_utils::cell_parent(b.h3_cell, self.heat_res) else {
                continue; // cells were validated at ingest
            };
            let entry = per_cell.entry(cell).or_insert(0);
            match self.filters.heat_metric {
                HeatMetric::Attention => *entry += u64::from(b.attention_count),
                HeatMetric::Events => *entry += u64::from(b.event_count),
                // Distinct counts sum across neither buckets nor child
                // cells; show the peak 6 h diversity instead.
                HeatMetric::Diversity => *entry = (*entry).max(u64::from(b.distinct_outlets)),
            }
        }
        per_cell.retain(|_, v| *v > 0);
        let max = per_cell.values().copied().max().unwrap_or(0);
        if max == 0 {
            self.map.heatmap = HeatmapLayer::empty();
            return;
        }
        let denom = ((max + 1) as f32).ln();
        let cells: Vec<(u64, f32)> = per_cell
            .into_iter()
            .map(|(cell, v)| (cell, ((v + 1) as f32).ln() / denom))
            .collect();
        self.map.heatmap = HeatmapLayer::from_cells(&cells, &self.map.style);
    }

    fn rebuild_markers(&mut self, points: Vec<EventPoint>) {
        let article_norm = 81f32.ln(); // saturates at 80 articles
        let inputs: Vec<MarkerInput> = points
            .iter()
            .enumerate()
            .map(|(i, p)| MarkerInput {
                lon: p.lon,
                lat: p.lat,
                kind: p.kind,
                weight: ((p.article_count + 1) as f32).ln() / article_norm,
                source_index: i,
            })
            .collect();
        self.map.markers = MarkerLayer::new(inputs);
        self.map.marker_rows = points;
    }

    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.timeline.playing || self.extent.is_none() {
            return;
        }
        const SECS_PER_STEP: f32 = 0.4;
        self.timeline.accum += ctx.input(|i| i.stable_dt).min(0.25);
        let total = self.total_buckets();
        let len = self.timeline.len.buckets(total);
        let max_start = (total - len).max(0);
        while self.timeline.accum >= SECS_PER_STEP {
            self.timeline.accum -= SECS_PER_STEP;
            self.timeline.start_bucket += 1;
            if self.timeline.start_bucket > max_start {
                self.timeline.start_bucket = 0; // loop the replay
            }
            self.dirty = true;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    fn persist_settings(&mut self) {
        if self.filters != self.last_saved_filters {
            if let Err(e) = self.settings.set("filters", &self.filters) {
                tracing::warn!("saving filters: {e}");
            }
            self.last_saved_filters = self.filters.clone();
        }
    }

    pub fn select_cell(&mut self, cell: u64, lonlat: Option<(f64, f64)>) {
        self.selected_cell = Some(cell);
        self.detail = None;
        self.selected_label = lonlat.and_then(|(lon, lat)| {
            self.countries
                .country_at(lon, lat)
                .map(|c| format!("{} ({})", c.name, c.iso_a3))
        });
        if let Some(window) = self.current_window() {
            self.pending_detail = Some(self.store.region_detail(cell, window));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_async();
        self.advance_playback(&ctx);

        // Panel order matters in egui 0.35: sides first, central last.
        self.top_bar(ui);
        self.timeline_panel(ui);
        self.inspector_panel(ui);
        self.central_map(ui);
        self.log_window(&ctx);

        // Zoom crossed a rollup threshold → re-aggregate the cached buckets
        // at the new display resolution (no storage round-trip).
        if matches!(self.phase, Phase::Ready) {
            let deg_per_px = self.map.viewport.as_ref().map_or(0.225, |v| v.deg_per_px);
            if Self::heat_resolution(deg_per_px) != self.heat_res {
                self.rebuild_heatmap();
            }
        }

        if self.dirty && matches!(self.phase, Phase::Ready) {
            self.dirty = false;
            self.fire_queries();
        }
        self.persist_settings();
    }
}
