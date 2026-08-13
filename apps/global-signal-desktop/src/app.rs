//! App state machine: ingest → storage → queries → layers → panels.
//!
//! The UI thread never blocks: every storage call returns a `Reply` that is
//! polled once per frame, and the storage actor requests a repaint whenever
//! a reply lands.

use std::sync::mpsc;

use core_types::{
    BUCKET_SECS, EventKind, GeoTemporalEvent, IngestFailure, RegionBucket, SourceId,
    bucket_start_epoch,
};
use geo_utils::CountryIndex;
use renderer::{BasemapLayer, HaloLayer, HeatmapLayer, MapStyle, MarkerInput, MarkerLayer};
use serde::{Deserialize, Serialize};
use storage::{
    EpochWindow, EventPoint, ExportReport, IngestLogRow, IngestReport, RegionDetail,
    RegionEventsPage, RegionHistoryPoint, Reply, SettingsDb, StorageHandle, TimelineHistogramPoint,
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
    /// Attention ↔ unrest divergence (docs/VISUALIZATION.md V2 item 5): a
    /// diverging metric, not an intensity, so it takes its own palette and
    /// its own build path in `rebuild_heatmap`.
    Divergence,
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
    /// Pulsing rings on cells whose spike score clears
    /// `analytics::weights::SPIKE_HALO_THRESHOLD`. `serde(default)` keeps
    /// settings saved before this toggle existed loadable.
    #[serde(default = "default_true")]
    pub show_spike_halos: bool,
    /// NOAA/NWS weather-alert cell overlay (docs/VISUALIZATION.md V3 item 8).
    /// `serde(default)` keeps settings saved before this toggle existed
    /// loadable.
    #[serde(default = "default_true")]
    pub show_alerts: bool,
    // --- orientation (docs/VISUALIZATION.md V3 item 9) ---
    #[serde(default = "default_true")]
    pub show_graticule: bool,
    #[serde(default = "default_true")]
    pub show_labels: bool,
    /// Dim the map outside the selected cell. Off by default: dimming hides
    /// real data, so it stays something the user turns on.
    #[serde(default)]
    pub focus_selection: bool,
    pub heat_metric: HeatMetric,
    /// Selected themes; empty = no theme filtering. `serde(default)` keeps
    /// settings saved before M2 loadable.
    #[serde(default)]
    pub themes: Vec<String>,
    /// Only show markers whose record carries a classified video URL.
    #[serde(default)]
    pub video_only: bool,
}

fn default_true() -> bool {
    true
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
            show_spike_halos: true,
            show_alerts: true,
            show_graticule: true,
            show_labels: true,
            focus_selection: false,
            heat_metric: HeatMetric::Attention,
            themes: Vec::new(),
            video_only: false,
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
    /// A typed exact start/end range (`timeline_panel`'s custom-range
    /// inputs), stored as a bucket count rather than a preset. Never a
    /// member of `CHOICES` — it only exists once the user has typed one.
    Custom(i64),
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
            // The exact typed range is already shown next to the combo
            // (`current_window()`'s "start → end" label), so this generic
            // fallback never needs to carry the bucket count itself.
            WindowLen::Custom(_) => "custom",
        }
    }

    pub fn buckets(self, total: i64) -> i64 {
        match self {
            WindowLen::H6 => 1,
            WindowLen::D1 => 4,
            WindowLen::D3 => 12,
            WindowLen::D7 => 28,
            WindowLen::All => total,
            WindowLen::Custom(n) => n,
        }
        .min(total.max(1))
    }
}

pub struct Timeline {
    pub len: WindowLen,
    pub start_bucket: i64,
    pub playing: bool,
    pub accum: f32,
    /// While `true`, every extent refresh and window-length change
    /// repositions the window to track wall-clock "now" (see
    /// `now_anchored_start_bucket`). Cleared the moment the user takes
    /// manual control (scrub, playback, or a typed custom range) so a
    /// background ingest tick can never yank them away from where they're
    /// looking; the timeline panel's "now" control turns it back on.
    pub auto_follow: bool,
    /// Typed custom-range text (`timeline_panel`'s "start"/"end" fields),
    /// `%Y-%m-%d %H:%M` UTC. Kept across frames so the user's partial input
    /// survives a redraw; applied on Enter, not on every keystroke.
    pub custom_start_input: String,
    pub custom_end_input: String,
    /// Set when the last apply attempt failed to parse; cleared on the next
    /// successful apply or edit. Shown next to the inputs.
    pub custom_range_error: Option<String>,
}

/// Discrete-event kind order for the timeline histogram stack (mirrors
/// `EventKind::ALL` minus `NewsAttention`, which is drawn as a line overlay
/// so it never mixes with the event stack — CLAUDE.md's attention/event
/// separation).
pub const HISTOGRAM_STACK_KINDS: [EventKind; 4] = [
    EventKind::Protest,
    EventKind::Conflict,
    EventKind::Disruption,
    EventKind::Other,
];

/// One column of the timeline histogram strip: discrete-event counts by
/// kind (indexed by [`HISTOGRAM_STACK_KINDS`]) plus the attention count.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistogramBucket {
    pub event_counts: [u32; 4],
    pub attention_count: u32,
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
    /// Batches waiting to be handed to the storage actor (one live ingest in
    /// flight at a time).
    ingest_queue: std::collections::VecDeque<Batch>,
    /// Live online mode (all live sources); drives the ingest worker.
    pub online: bool,
    /// Latest per-source live status lines for the UI (ordered by name).
    pub source_statuses: Vec<SourceStatus>,
    /// Events-table retention cap in days (`None` = keep everything). Applied to
    /// the storage actor; persisted in settings.
    pub retention_days: Option<u32>,
    pending_ingest: Option<Reply<IngestReport>>,
    pub ingest_report: Option<IngestReport>,
    pending_log: Option<Reply<(u64, Vec<IngestLogRow>)>>,
    pub ingest_log: Option<(u64, Vec<IngestLogRow>)>,
    pub show_log_window: bool,
    /// "How to read this map" (docs/VISUALIZATION.md V3 item 10). Opens once
    /// on first run and on `?` thereafter.
    pub show_how_to_read: bool,

    pending_extent: Option<Reply<Option<EpochWindow>>>,
    /// Bucket-aligned data extent `[start, end)`.
    pub extent: Option<EpochWindow>,

    pending_vocab: Option<Reply<Vec<(String, u32)>>>,
    /// Distinct themes with usage counts, most-used first (from the data).
    pub theme_vocab: Option<Vec<(String, u32)>>,

    pending_histogram: Option<Reply<Vec<TimelineHistogramPoint>>>,
    /// Raw full-extent histogram rows, kept so a late-arriving `extent`
    /// change can re-align the dense array without a fresh query.
    histogram_raw: Vec<TimelineHistogramPoint>,
    /// Dense, bucket-index-aligned histogram for the timeline strip.
    pub timeline_histogram: Vec<HistogramBucket>,

    pub timeline: Timeline,
    pub filters: Filters,
    last_saved_filters: Filters,
    dirty: bool,

    pending_buckets: Option<Reply<Vec<RegionBucket>>>,
    pending_points: Option<Reply<Vec<EventPoint>>>,
    pending_alerts: Option<Reply<Vec<storage::AlertCell>>>,
    /// NOAA alert cells in the current window, kept at storage resolution so
    /// the overlay can re-roll to a different H3 parent on zoom without a new
    /// query — the same contract `window_buckets` has for the heatmap.
    alert_cells: Vec<storage::AlertCell>,
    pub bucket_count: usize,
    /// Buckets of the current window, kept so the heatmap can re-aggregate
    /// at a different H3 rollup resolution when the zoom crosses a threshold
    /// without re-querying storage.
    window_buckets: Vec<RegionBucket>,
    /// H3 resolution the heatmap layer was last built at.
    heat_res: u8,
    /// Ranked spike regions in the current window (docs/VISUALIZATION.md V2
    /// item 6), each with its dense per-bucket series for the row sparkline.
    /// Both come from the cached `window_buckets` on every rebuild — this
    /// panel never issues a storage query.
    pub top_movers: Vec<(analytics::Mover, Vec<u32>)>,

    pub selected_cell: Option<u64>,
    pub selected_label: Option<String>,
    pending_detail: Option<Reply<RegionDetail>>,
    pub detail: Option<RegionDetail>,

    // --- region inspector, V2 item 7 (docs/VISUALIZATION.md) ---
    pending_history: Option<Reply<Vec<RegionHistoryPoint>>>,
    /// The selected cell's trailing bucket history, with the span it covers
    /// so absent buckets can be drawn as gaps rather than closed up.
    pub region_history: Vec<RegionHistoryPoint>,
    pub history_span: Option<EpochWindow>,
    pending_ledger: Option<Reply<RegionEventsPage>>,
    pub ledger: Option<RegionEventsPage>,
    /// Row offset of the ledger page being viewed. Reset whenever the
    /// selection or the window changes — a page number means nothing against
    /// a different result set.
    pub ledger_offset: usize,
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
        // Do not carry fixture-era theme selections into the live-only UI.
        // Subsequent live selections persist under this new key normally.
        let filters: Filters = settings.get("filters_live_v1")?.unwrap_or_default();
        // First run (or a first run after the text was revised): open the
        // reading guide before the user has drawn any conclusion from the map.
        let how_to_read_seen: bool = settings
            .get(crate::how_to_read::SEEN_KEY)?
            .unwrap_or(Some(false))
            .unwrap_or(false);

        // Retention: env override wins, else the saved setting, else unbounded.
        let retention_days: Option<u32> = match std::env::var("LES_RETENTION_DAYS") {
            Ok(s) => s.trim().parse::<u32>().ok().filter(|d| *d > 0),
            Err(_) => settings.get("retention_days")?.flatten(),
        };
        store.set_retention(retention_days);

        // Migrate the legacy mixed database in place: only fixture-attributed
        // rows/logs are removed, preserving every real record. Finish before
        // opening the window so synthetic data can never flash on screen or
        // survive an early close.
        let removed = store.purge_source(SourceId::Fixtures).wait()?;
        if removed > 0 {
            tracing::info!(removed, "legacy synthetic events removed");
        }
        let pending_extent = Some(store.time_extent());
        let pending_log = Some(store.ingest_log(20));
        let pending_vocab = Some(store.theme_vocab());
        let pending_histogram = Some(store.timeline_histogram());

        let ctx = cc.egui_ctx.clone();
        let (ingest_rx, ingest_handle) = ingest::spawn(move || ctx.request_repaint());
        let phase = Phase::Loading("loading live data…".into());

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
            source_statuses: Vec::new(),
            retention_days,
            pending_ingest: None,
            ingest_report: None,
            pending_log,
            ingest_log: None,
            show_log_window: false,
            show_how_to_read: !how_to_read_seen,
            pending_extent,
            extent: None,
            pending_vocab,
            theme_vocab: None,
            pending_histogram,
            histogram_raw: Vec::new(),
            timeline_histogram: Vec::new(),
            timeline: Timeline {
                len: WindowLen::D1,
                start_bucket: 0,
                playing: false,
                accum: 0.0,
                auto_follow: true,
                custom_start_input: String::new(),
                custom_end_input: String::new(),
                custom_range_error: None,
            },
            last_saved_filters: filters.clone(),
            filters,
            dirty: false,
            pending_buckets: None,
            pending_points: None,
            pending_alerts: None,
            alert_cells: Vec::new(),
            bucket_count: 0,
            window_buckets: Vec::new(),
            heat_res: core_types::H3_RESOLUTION,
            top_movers: Vec::new(),
            selected_cell: None,
            selected_label: None,
            pending_history: None,
            region_history: Vec::new(),
            history_span: None,
            pending_ledger: None,
            ledger: None,
            ledger_offset: 0,
            pending_detail: None,
            detail: None,
        };

        // Live updates are the desktop default. LES_ONLINE=0 remains a useful
        // explicit pause for testing cached real data without network access.
        let online = std::env::var("LES_ONLINE")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no"))
            .unwrap_or(true);
        app.set_online(online);
        Ok(app)
    }

    pub fn total_buckets(&self) -> i64 {
        self.extent
            .map(|(s, e)| ((e - s) / BUCKET_SECS).max(1))
            .unwrap_or(0)
    }

    /// Reposition the window to track wall-clock "now" and mark auto-follow
    /// on. Called at startup, after every extent refresh while still
    /// auto-following, on a window-length change while still auto-following,
    /// and by the explicit "now" control. No-op with no extent yet.
    pub fn sync_window_to_now(&mut self) {
        let Some(extent) = self.extent else { return };
        let len = self.timeline.len.buckets(self.total_buckets());
        self.timeline.start_bucket =
            now_anchored_start_bucket(extent, len, chrono::Utc::now().timestamp());
    }

    /// Apply the timeline panel's typed start/end inputs as the window.
    /// On success this takes the window off auto-follow (a typed range is
    /// an explicit "look here", not "keep me at now") and clears any prior
    /// error; on failure it leaves the window untouched and records the
    /// error for display next to the inputs.
    pub fn apply_custom_range(&mut self) {
        let Some(extent) = self.extent else { return };
        match parse_custom_range(
            &self.timeline.custom_start_input,
            &self.timeline.custom_end_input,
            extent,
        ) {
            Ok((start_bucket, len_buckets)) => {
                self.timeline.start_bucket = start_bucket;
                self.timeline.len = WindowLen::Custom(len_buckets);
                self.timeline.auto_follow = false;
                self.timeline.custom_range_error = None;
                self.mark_dirty();
            }
            Err(msg) => self.timeline.custom_range_error = Some(msg.to_string()),
        }
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
        // 1a. Drain all worker messages: queue live batches and apply status.
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
                        // Upsert this source's line; the app-level online flag
                        // is the aggregate (a credential-less source reporting
                        // offline must not clear it).
                        match self
                            .source_statuses
                            .iter_mut()
                            .find(|s| s.name == status.name)
                        {
                            Some(slot) => *slot = status,
                            None => {
                                self.source_statuses.push(status);
                                self.source_statuses.sort_by_key(|s| s.name);
                            }
                        }
                        self.online = self.source_statuses.iter().any(|s| s.online);
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
                    self.refresh_metadata();
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
                    if self.timeline.auto_follow {
                        self.sync_window_to_now();
                    } else {
                        let total = self.total_buckets();
                        let len = self.timeline.len.buckets(total);
                        self.timeline.start_bucket =
                            self.timeline.start_bucket.clamp(0, (total - len).max(0));
                    }
                    self.phase = Phase::Ready;
                    self.dirty = true;
                    self.rebuild_histogram();
                }
                Ok(None) => {
                    self.extent = None;
                    self.timeline.start_bucket = 0;
                    self.window_buckets.clear();
                    self.bucket_count = 0;
                    self.top_movers.clear();
                    self.map.heatmap = HeatmapLayer::empty();
                    self.map.markers = MarkerLayer::new(Vec::new());
                    self.map.spike_halos = HaloLayer::new(Vec::new());
                    self.map.marker_rows.clear();
                    self.histogram_raw.clear();
                    self.timeline_histogram.clear();
                    self.phase = Phase::Ready;
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

        if let Some(reply) = &self.pending_histogram
            && let Some(result) = reply.try_take()
        {
            self.pending_histogram = None;
            match result {
                Ok(points) => {
                    self.histogram_raw = points;
                    self.rebuild_histogram();
                }
                Err(e) => tracing::error!("timeline histogram query: {e}"),
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

        if let Some(reply) = &self.pending_alerts
            && let Some(result) = reply.try_take()
        {
            self.pending_alerts = None;
            match result {
                Ok(cells) => {
                    self.alert_cells = cells;
                    self.rebuild_alerts();
                }
                Err(e) => tracing::error!("alert cells query: {e}"),
            }
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
                    self.rebuild_halos();
                    self.rebuild_top_movers();
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
        if let Some(reply) = &self.pending_history
            && let Some(result) = reply.try_take()
        {
            self.pending_history = None;
            match result {
                Ok(points) => self.region_history = points,
                Err(e) => tracing::error!("region history query: {e}"),
            }
        }
        if let Some(reply) = &self.pending_ledger
            && let Some(result) = reply.try_take()
        {
            self.pending_ledger = None;
            match result {
                Ok(page) => self.ledger = Some(page),
                Err(e) => tracing::error!("region events query: {e}"),
            }
        }
    }

    /// Fire the two cell-scoped inspector queries (docs/VISUALIZATION.md V2
    /// item 7). Both go through `Reply<T>` like every other query — the UI
    /// thread polls them per frame and never blocks on storage.
    fn fire_region_queries(&mut self, cell: u64, window: EpochWindow) {
        self.pending_detail = Some(self.store.region_detail(cell, window));
        // History ends at the window end, so scrubbing back in time shows the
        // baseline as it stood then rather than as it stands now.
        let span_start =
            window.1 - i64::from(analytics::weights::BASELINE_WINDOW_DAYS) * SECS_PER_DAY;
        self.history_span = Some((span_start, window.1));
        self.pending_history = Some(self.store.region_history(cell, window.1));
        self.pending_ledger =
            Some(
                self.store
                    .region_events(cell, window, self.ledger_offset, LEDGER_PAGE_SIZE),
            );
    }

    /// Jump the ledger to a new page offset and re-query just that page.
    /// Record that the reading guide has been dismissed, so it stops opening
    /// on launch. A failure here only costs one extra showing next run.
    pub fn mark_how_to_read_seen(&mut self) {
        if let Err(e) = self.settings.set(crate::how_to_read::SEEN_KEY, &true) {
            tracing::warn!("saving how-to-read flag: {e}");
        }
    }

    pub fn set_ledger_offset(&mut self, offset: usize) {
        self.ledger_offset = offset;
        if let (Some(cell), Some(window)) = (self.selected_cell, self.current_window()) {
            self.pending_ledger =
                Some(
                    self.store
                        .region_events(cell, window, self.ledger_offset, LEDGER_PAGE_SIZE),
                );
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
            self.filters.video_only,
        ));
        self.pending_alerts = Some(self.store.alert_cells(window));
        if let Some(cell) = self.selected_cell {
            // The window moved under the existing page, so the old offset no
            // longer points at the same rows.
            self.ledger_offset = 0;
            self.fire_region_queries(cell, window);
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
        if self.filters.heat_metric == HeatMetric::Divergence {
            self.rebuild_divergence();
            return;
        }
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
                HeatMetric::Divergence => unreachable!("handled above"),
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

    /// Attention ↔ unrest divergence heat (docs/VISUALIZATION.md V2 item 5).
    ///
    /// The rollup to the display resolution happens *before* ranking, so the
    /// ranks describe the cells actually on screen — ranking res-3 cells and
    /// then painting res-1 parents would show a distribution the viewer
    /// cannot see. Same cached `window_buckets`, no extra storage query.
    fn rebuild_divergence(&mut self) {
        let mut per_cell: std::collections::HashMap<u64, analytics::CellComponents> =
            std::collections::HashMap::new();
        for b in &self.window_buckets {
            let Ok(cell) = geo_utils::cell_parent(b.h3_cell, self.heat_res) else {
                continue; // cells were validated at ingest
            };
            per_cell
                .entry(cell)
                .or_insert_with(|| analytics::CellComponents::new(cell))
                .absorb(b);
        }
        if per_cell.is_empty() {
            self.map.heatmap = HeatmapLayer::empty();
            return;
        }
        let components: Vec<analytics::CellComponents> = per_cell.into_values().collect();
        let cells = analytics::divergence_ranks(&components);
        self.map.heatmap = HeatmapLayer::from_divergence(&cells, &self.map.style);
    }

    /// Rank the window's cells for the top-movers panel and build each row's
    /// sparkline series — both from the cached `window_buckets`, so the panel
    /// costs one sort plus one scan per ranked row and no storage round-trip.
    fn rebuild_top_movers(&mut self) {
        let movers =
            analytics::top_movers(&self.window_buckets, analytics::weights::TOP_MOVERS_LIMIT);
        let Some(window) = self.current_window() else {
            self.top_movers = movers.into_iter().map(|m| (m, Vec::new())).collect();
            return;
        };
        self.top_movers = movers
            .into_iter()
            .map(|m| {
                let series = analytics::cell_series(&self.window_buckets, m.h3_cell, window);
                (m, series)
            })
            .collect();
    }

    /// Cells worth a spike halo, derived from the already-cached
    /// `window_buckets` — no extra storage round-trip (mirrors
    /// `rebuild_heatmap`, which shares the same dependency).
    fn rebuild_halos(&mut self) {
        let cells = analytics::spike_halo_cells(
            &self.window_buckets,
            analytics::weights::SPIKE_HALO_THRESHOLD,
            analytics::weights::SPIKE_HALO_MAX_CELLS,
        );
        self.map.spike_halos = HaloLayer::new(cells);
    }

    /// NOAA weather-alert overlay (docs/VISUALIZATION.md V3 item 8), rolled up
    /// to the heatmap's display resolution so alert cells register with the
    /// regions shaded underneath them.
    ///
    /// Peak severity wins a rollup, matching how `spike_halo_cells` and the
    /// divergence layer both summarize a parent cell: a coarse cell that
    /// contains a severe alert is a cell with a severe alert in it, and
    /// averaging would dilute exactly the case the layer exists to show.
    fn rebuild_alerts(&mut self) {
        let mut per_cell: std::collections::HashMap<u64, f32> = std::collections::HashMap::new();
        for a in &self.alert_cells {
            let Ok(cell) = geo_utils::cell_parent(a.h3_cell, self.heat_res) else {
                continue; // cells were validated at ingest
            };
            let slot = per_cell.entry(cell).or_insert(0.0);
            *slot = slot.max(a.severity);
        }
        let mut cells: Vec<(u64, f32)> = per_cell.into_iter().collect();
        // Most severe first, then by cell id so the truncation below is
        // deterministic across runs (a `HashMap` hands them over in any order).
        cells.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        cells.truncate(renderer::ALERT_MAX_CELLS);
        self.map.alerts = renderer::AlertLayer::new(&cells, &self.map.style);
    }

    fn rebuild_markers(&mut self, points: Vec<EventPoint>) {
        let article_norm = 81f32.ln(); // saturates at 80 articles
        // Recency fade only applies during playback — pausing always shows
        // full detail (docs/VISUALIZATION.md V1 item 4).
        let fade_window = self
            .timeline
            .playing
            .then(|| self.current_window())
            .flatten();
        let inputs: Vec<MarkerInput> = points
            .iter()
            .enumerate()
            .map(|(i, p)| MarkerInput {
                lon: p.lon,
                lat: p.lat,
                kind: p.kind,
                weight: ((p.article_count + 1) as f32).ln() / article_norm,
                severity: p.severity,
                alpha: fade_window.map_or(1.0, |(ws, we)| {
                    fade_alpha(we - p.ts_epoch_s, we - ws, FADE_FLOOR_ALPHA)
                }),
                glyph: renderer::MarkerGlyph::for_source(p.source),
                source_index: i,
            })
            .collect();
        // Point markers are the sparsest layer on the map and the easiest to
        // mistake for a rendering fault when a window happens to hold only
        // coarse-precision records. Log the count so "no markers" can be told
        // apart from "markers not drawing" without a rebuild.
        tracing::info!(
            markers = inputs.len(),
            rows = points.len(),
            "marker layer rebuilt"
        );
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
            if let Err(e) = self.settings.set("filters_live_v1", &self.filters) {
                tracing::warn!("saving filters: {e}");
            }
            self.last_saved_filters = self.filters.clone();
        }
    }

    fn refresh_metadata(&mut self) {
        self.pending_extent = Some(self.store.time_extent());
        self.pending_log = Some(self.store.ingest_log(20));
        self.pending_vocab = Some(self.store.theme_vocab());
        self.pending_histogram = Some(self.store.timeline_histogram());
    }

    /// Re-project `histogram_raw` into the dense, bucket-index-aligned
    /// `timeline_histogram` array. Full-extent, not window-scoped — refreshed
    /// only on ingest (`refresh_metadata`), never on scrub/window changes.
    fn rebuild_histogram(&mut self) {
        let Some((extent_start, _)) = self.extent else {
            self.timeline_histogram.clear();
            return;
        };
        let total = self.total_buckets().max(0) as usize;
        let mut dense = vec![HistogramBucket::default(); total];
        for p in &self.histogram_raw {
            let idx = (p.bucket_start - extent_start) / BUCKET_SECS;
            if idx < 0 || idx as usize >= dense.len() {
                continue; // stale row outside the current extent
            }
            let slot = &mut dense[idx as usize];
            if p.kind.is_attention() {
                slot.attention_count += p.count;
            } else if let Some(i) = HISTOGRAM_STACK_KINDS.iter().position(|&k| k == p.kind) {
                slot.event_counts[i] += p.count;
            }
        }
        self.timeline_histogram = dense;
    }

    /// Select a cell the user picked from a list rather than the map, and fly
    /// the viewport to it (docs/VISUALIZATION.md V2 item 6). Selection still
    /// happens if the cell has no derivable center, so the inspector works
    /// even when the map cannot move.
    pub fn select_and_fly(&mut self, cell: u64) {
        let center = geo_utils::cell_center_lonlat(cell).ok();
        self.select_cell(cell, center);
        if let Some((lon, lat)) = center {
            self.map.fly_to(lon, lat);
        }
    }

    pub fn select_cell(&mut self, cell: u64, lonlat: Option<(f64, f64)>) {
        self.selected_cell = Some(cell);
        self.detail = None;
        self.region_history.clear();
        self.ledger = None;
        self.ledger_offset = 0;
        self.selected_label = lonlat.and_then(|(lon, lat)| {
            self.countries
                .country_at(lon, lat)
                .map(|c| format!("{} ({})", c.name, c.iso_a3))
        });
        if let Some(window) = self.current_window() {
            self.fire_region_queries(cell, window);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_async();
        self.advance_playback(&ctx);

        // `?` reopens the reading guide. Ignored while a text field has focus
        // so typing a question mark into the theme filter doesn't fire it.
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Questionmark))
        {
            self.show_how_to_read = !self.show_how_to_read;
        }

        // Panel order matters in egui 0.35: sides first, central last.
        self.top_bar(ui);
        self.timeline_panel(ui);
        self.inspector_panel(ui);
        self.central_map(ui);
        self.log_window(&ctx);
        self.how_to_read_window(&ctx);

        // Zoom crossed a rollup threshold → re-aggregate the cached buckets
        // at the new display resolution (no storage round-trip).
        if matches!(self.phase, Phase::Ready) {
            let deg_per_px = self.map.viewport.as_ref().map_or(0.225, |v| v.deg_per_px);
            if Self::heat_resolution(deg_per_px) != self.heat_res {
                self.rebuild_heatmap();
                // The alert overlay shares the heatmap's display resolution so
                // its cells line up with the shaded regions underneath.
                self.rebuild_alerts();
            }
        }

        if self.dirty && matches!(self.phase, Phase::Ready) {
            self.dirty = false;
            self.fire_queries();
        }
        self.persist_settings();
    }
}

/// Recency-fade floor during playback (docs/VISUALIZATION.md V1 item 4:
/// "oldest ≈ 35%").
const FADE_FLOOR_ALPHA: f32 = 0.35;

/// Rows per page in the inspector's event ledger (docs/VISUALIZATION.md V2
/// item 7 — the ledger paginates; it never scrolls unbounded).
pub const LEDGER_PAGE_SIZE: usize = 25;

const SECS_PER_DAY: i64 = 86_400;

/// Playback recency-fade opacity: a point at the window end (`age_secs`
/// `<= 0`) is fully opaque; one at the window start (`age_secs >=
/// window_span_secs`) is `floor`; linear in between. Clamped so a point
/// racing a window change (outside `[0, window_span_secs]`) still yields a
/// sane alpha rather than an out-of-range one.
fn fade_alpha(age_secs: i64, window_span_secs: i64, floor: f32) -> f32 {
    if window_span_secs <= 0 {
        return 1.0;
    }
    let t = (age_secs as f32 / window_span_secs as f32).clamp(0.0, 1.0);
    1.0 - t * (1.0 - floor)
}

/// Position `start_bucket` so the window's right edge sits at wall-clock
/// "now" (bucket-aligned), not at the raw data extent's tail. The extent's
/// own max timestamp is the wrong anchor for "current": it can sit behind
/// "now" (fresh launch, before every source has reported — ACLED in
/// particular can hold a fixed months-old `LES_ACLED_WINDOW`) or ahead of it
/// (NOAA alerts are timestamped by `onset`, which can be future-dated for a
/// watch/warning issued ahead of the event). Either way the extent's tail is
/// not "now", so it is not used as the anchor here — only as the clamp range.
fn now_anchored_start_bucket(extent: EpochWindow, len_buckets: i64, now_epoch_s: i64) -> i64 {
    let (start, end) = extent;
    let total = ((end - start) / BUCKET_SECS).max(1);
    let max_start = (total - len_buckets).max(0);
    let now_bucket = (bucket_start_epoch(now_epoch_s) - start).div_euclid(BUCKET_SECS);
    (now_bucket + 1 - len_buckets).clamp(0, max_start)
}

/// Format expected from the timeline panel's typed custom-range inputs.
pub const CUSTOM_RANGE_FORMAT: &str = "%Y-%m-%d %H:%M";

/// Parse a typed `(start, end)` UTC pair (`CUSTOM_RANGE_FORMAT`) into a
/// bucket-aligned `(start_bucket, len_buckets)` pair against `extent`,
/// clamped into the extent — the rest of the app assumes `start_bucket`
/// stays within `[0, total)`, same invariant `now_anchored_start_bucket`
/// keeps. `Err` carries a short message for display next to the inputs.
fn parse_custom_range(
    start_s: &str,
    end_s: &str,
    extent: EpochWindow,
) -> Result<(i64, i64), &'static str> {
    let parse = |s: &str| {
        chrono::NaiveDateTime::parse_from_str(s.trim(), CUSTOM_RANGE_FORMAT)
            .map(|dt| dt.and_utc().timestamp())
    };
    let start_ts = parse(start_s).map_err(|_| "start: expected YYYY-MM-DD HH:MM")?;
    let end_ts = parse(end_s).map_err(|_| "end: expected YYYY-MM-DD HH:MM")?;
    if end_ts <= start_ts {
        return Err("end must be after start");
    }
    let (ext_start, ext_end) = extent;
    let total = ((ext_end - ext_start) / BUCKET_SECS).max(1);
    let start_bucket = (bucket_start_epoch(start_ts) - ext_start)
        .div_euclid(BUCKET_SECS)
        .clamp(0, total - 1);
    let end_bucket = ((bucket_start_epoch(end_ts - 1) - ext_start).div_euclid(BUCKET_SECS) + 1)
        .clamp(start_bucket + 1, total);
    Ok((start_bucket, end_bucket - start_bucket))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_alpha_is_full_at_window_end_and_floor_at_window_start() {
        assert_eq!(fade_alpha(0, 86_400, FADE_FLOOR_ALPHA), 1.0);
        assert!((fade_alpha(86_400, 86_400, FADE_FLOOR_ALPHA) - FADE_FLOOR_ALPHA).abs() < 1e-6);
    }

    #[test]
    fn fade_alpha_is_linear_at_the_midpoint() {
        let mid = fade_alpha(43_200, 86_400, FADE_FLOOR_ALPHA);
        assert!((mid - (1.0 + FADE_FLOOR_ALPHA) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn fade_alpha_clamps_outside_the_window() {
        assert_eq!(fade_alpha(-100, 86_400, FADE_FLOOR_ALPHA), 1.0);
        assert!((fade_alpha(200_000, 86_400, FADE_FLOOR_ALPHA) - FADE_FLOOR_ALPHA).abs() < 1e-6);
    }

    #[test]
    fn fade_alpha_degenerate_window_is_full_opacity() {
        assert_eq!(fade_alpha(0, 0, FADE_FLOOR_ALPHA), 1.0);
    }

    #[test]
    fn now_anchored_start_bucket_ends_the_window_at_nows_bucket() {
        let extent = (0, 10 * BUCKET_SECS); // 10 buckets: [0, 10)
        let now = 5 * BUCKET_SECS + 1_000; // inside bucket 5
        assert_eq!(now_anchored_start_bucket(extent, 4, now), 2);
    }

    /// The regression this function exists to fix: a NOAA `onset` timestamp
    /// (or any future-dated row) can stretch the extent's tail past "now".
    /// The window must still anchor at "now", not at that future tail.
    #[test]
    fn now_anchored_start_bucket_ignores_a_future_tail() {
        let extent = (0, 20 * BUCKET_SECS); // extent reaches 20 buckets out
        let now = 5 * BUCKET_SECS + 1_000; // "now" is only at bucket 5
        assert_eq!(now_anchored_start_bucket(extent, 4, now), 2);
    }

    /// The mirror case: "now" is newer than anything ingested yet (a fresh
    /// launch, or a source that hasn't reported this cycle). There is
    /// nothing to show for "now", so this falls back to the latest
    /// available data rather than pointing past the end of the extent.
    #[test]
    fn now_anchored_start_bucket_falls_back_to_the_tail_when_now_is_beyond_the_extent() {
        let extent = (0, 10 * BUCKET_SECS);
        let now = 20 * BUCKET_SECS;
        assert_eq!(now_anchored_start_bucket(extent, 4, now), 6); // total - len
    }

    #[test]
    fn now_anchored_start_bucket_clamps_to_zero_when_now_precedes_the_extent() {
        let extent = (10 * BUCKET_SECS, 20 * BUCKET_SECS);
        let now = 0;
        assert_eq!(now_anchored_start_bucket(extent, 4, now), 0);
    }

    #[test]
    fn parse_custom_range_bucket_aligns_a_valid_typed_range() {
        // Extent: epoch 0 .. 10 buckets. Typed range: bucket-aligned
        // 1970-01-01 06:00 .. 1970-01-01 18:00 = buckets [1, 3).
        let extent = (0, 10 * BUCKET_SECS);
        let (start_bucket, len) =
            parse_custom_range("1970-01-01 06:00", "1970-01-01 18:00", extent).unwrap();
        assert_eq!((start_bucket, len), (1, 2));
    }

    #[test]
    fn parse_custom_range_rejects_end_before_start() {
        let extent = (0, 10 * BUCKET_SECS);
        assert!(parse_custom_range("1970-01-01 18:00", "1970-01-01 06:00", extent).is_err());
    }

    #[test]
    fn parse_custom_range_rejects_unparseable_input() {
        let extent = (0, 10 * BUCKET_SECS);
        assert!(parse_custom_range("not a date", "1970-01-01 18:00", extent).is_err());
        assert!(parse_custom_range("1970-01-01 06:00", "also not a date", extent).is_err());
    }

    #[test]
    fn parse_custom_range_clamps_into_the_extent() {
        // Both ends fall far outside the 10-bucket extent; the result must
        // still land inside [0, total) so start_bucket's usual invariant
        // (see now_anchored_start_bucket) holds for a typed range too.
        let extent = (0, 10 * BUCKET_SECS);
        let (start_bucket, len) =
            parse_custom_range("1969-01-01 00:00", "1975-01-01 00:00", extent).unwrap();
        assert_eq!((start_bucket, len), (0, 10));
    }
}
