//! The network path (feature `live`): MTProto over a real Telegram account
//! session, polled on a fixed cadence like NOAA/IODA (not streamed — unlike
//! Bluesky, Telegram has no keyless public firehose).
//!
//! **Login is a one-time, out-of-band step.** Telegram's account login needs
//! a phone number and an SMS/app code, which cannot be automated from a
//! long-lived worker/desktop process. `examples/login_setup.rs` is a small
//! interactive tool: run it once, and it saves a local SQLite session file.
//! Every subsequent run of the real source just opens that file — no further
//! interaction. If the file is missing or not yet authorized, [`fetch`]
//! returns a clear error naming the setup command rather than trying to
//! prompt for input from inside a GUI app or headless worker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chatter::ChatterAccumulator;
use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use grammers_client::Client;
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;
use tokio::sync::OnceCell;

use crate::{ALLOWED_CHANNELS, ChannelSweep};

/// Don't ingest a channel's entire history the first time it's swept — just
/// the most recent handful, enough to prime the per-channel high-water mark.
const FIRST_SWEEP_LIMIT: usize = 30;

/// Bound on how many new messages one poll pulls per channel. At this
/// source's poll cadence a channel would need to be extremely active to hit
/// this; if it does, the remainder is picked up next cycle — undercounting,
/// not overcounting, is the safe direction to bound in.
const PER_CYCLE_LIMIT: usize = 200;

struct Conn {
    client: Client,
    /// Keeps the connection-driving task alive for as long as the source
    /// lives; never polled directly again after being spawned.
    _runner: tokio::task::JoinHandle<()>,
}

/// Live Telegram adapter: MTProto over a curated public-channel allowlist
/// ([`crate::ALLOWED_CHANNELS`]).
pub struct TelegramSource {
    api_id: i32,
    session_path: String,
    conn: OnceCell<Conn>,
    accumulator: Mutex<ChatterAccumulator>,
    /// Highest message id already processed per channel, so a poll only
    /// walks messages newer than what was already counted. Deliberately
    /// **not** persisted to disk: on restart each channel is swept from
    /// scratch (bounded to [`FIRST_SWEEP_LIMIT`]), but any chatter window
    /// that already published re-derives the same `source_event_id` and is
    /// discarded by storage's dedup-by-id (the same corrections-reuse-ids
    /// behavior ACLED relies on) — safe, just occasionally redundant work,
    /// never double counted.
    last_seen: Mutex<HashMap<String, i32>>,
}

impl TelegramSource {
    /// Build from `TELEGRAM_API_ID` and `LES_TELEGRAM_SESSION_FILE`. Neither
    /// env var present means the source isn't configured — `Ok(None)`, same
    /// as ACLED's credential-gated pattern, not an error. `TELEGRAM_API_HASH`
    /// is deliberately not read here: it's needed only for the interactive
    /// login in `examples/login_setup.rs`, never for polling an
    /// already-authorized session.
    ///
    /// This does not touch the network or open the session file yet — that
    /// happens lazily on the first [`SignalSource::fetch`], inside an async
    /// context (opening a grammers session is itself async). If the session
    /// turns out not to be logged in, that surfaces as a `fetch` error
    /// naming the setup command, not here.
    pub fn from_env() -> Result<Option<Self>, SourceError> {
        let (Ok(api_id_raw), Ok(session_path)) = (
            std::env::var("TELEGRAM_API_ID"),
            std::env::var("LES_TELEGRAM_SESSION_FILE"),
        ) else {
            return Ok(None);
        };
        if api_id_raw.trim().is_empty() || session_path.trim().is_empty() {
            return Ok(None);
        }
        let api_id = api_id_raw
            .trim()
            .parse::<i32>()
            .map_err(|e| SourceError::Other(format!("TELEGRAM_API_ID must be an integer: {e}")))?;
        let accumulator = ChatterAccumulator::from_bundled(chatter::DEFAULT_WINDOW_SECS)
            .map_err(|e| SourceError::Other(format!("building chatter matcher: {e}")))?;
        Ok(Some(Self {
            api_id,
            session_path,
            conn: OnceCell::new(),
            accumulator: Mutex::new(accumulator),
            last_seen: Mutex::new(HashMap::new()),
        }))
    }

    /// Open (or reuse) the MTProto connection. On failure the cell stays
    /// uninitialized, so the next `fetch` retries rather than being stuck.
    async fn ensure_conn(&self) -> Result<&Client, SourceError> {
        let conn =
            self.conn
                .get_or_try_init(|| async {
                    let session = SqliteSession::open(&self.session_path).await.map_err(|e| {
                        SourceError::Other(format!(
                            "opening telegram session file `{}`: {e}",
                            self.session_path
                        ))
                    })?;
                    let session = Arc::new(session);
                    let SenderPool { runner, handle, .. } =
                        SenderPool::new(Arc::clone(&session), self.api_id);
                    let client = Client::new(handle);
                    let runner_task = tokio::spawn(runner.run());
                    let authorized = client.is_authorized().await.map_err(|e| {
                        SourceError::Other(format!("checking telegram session: {e}"))
                    })?;
                    if !authorized {
                        return Err(SourceError::Other(format!(
                            "telegram session `{}` is not logged in — run `cargo run -p \
                         source-telegram --features live --example login_setup` once to create it",
                            self.session_path
                        )));
                    }
                    Ok(Conn {
                        client,
                        _runner: runner_task,
                    })
                })
                .await?;
        Ok(&conn.client)
    }

    fn lock_last_seen(&self) -> MutexGuard<'_, HashMap<String, i32>> {
        self.last_seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_accumulator(&self) -> MutexGuard<'_, ChatterAccumulator> {
        self.accumulator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sweep one channel: pull messages newer than its high-water mark (or,
    /// on first contact, just the most recent [`FIRST_SWEEP_LIMIT`]), feed
    /// matching text into the accumulator, and advance the mark. Failures
    /// are logged and swallowed here rather than propagated — one
    /// unreachable or renamed channel must not degrade the other seven.
    async fn sweep_channel(&self, client: &Client, name: &str) {
        let peer = match client.resolve_username(name).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                tracing::warn!(channel = name, "telegram channel not found; skipping");
                return;
            }
            Err(e) => {
                tracing::warn!(channel = name, error = %e, "telegram resolve_username failed");
                return;
            }
        };
        let peer_ref = match peer.to_ref().await {
            Ok(Some(p)) => p,
            Ok(None) => {
                tracing::warn!(
                    channel = name,
                    "telegram channel has no addressable peer; skipping"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(channel = name, error = %e, "telegram peer resolution failed");
                return;
            }
        };

        let last_id = self.lock_last_seen().get(name).copied();
        let mut iter = client.iter_messages(peer_ref);
        iter = match last_id {
            Some(id) => iter.offset_id(id).reverse(true).limit(PER_CYCLE_LIMIT),
            None => iter.limit(FIRST_SWEEP_LIMIT),
        };

        let mut sweep = ChannelSweep::new(last_id);
        loop {
            let msg = match iter.next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(channel = name, error = %e, "telegram iter_messages failed mid-sweep");
                    break;
                }
            };
            sweep.observe(
                &mut self.lock_accumulator(),
                msg.id(),
                msg.text(),
                msg.date(),
            );
        }
        if let Some(newest) = sweep.finish() {
            self.lock_last_seen().insert(name.to_owned(), newest);
        }
        tracing::info!(
            channel = name,
            scanned = sweep.scanned(),
            "telegram channel swept"
        );
    }
}

impl SignalSource for TelegramSource {
    fn id(&self) -> SourceId {
        SourceId::Telegram
    }

    /// Sweep every allowlisted channel, then drain whatever chatter windows
    /// completed. `window` is ignored, like Bluesky: each channel's own
    /// high-water mark already bounds the sweep to new messages, so there's
    /// no separate query window to honor.
    async fn fetch(
        &self,
        _window: TimeWindow,
        _filters: &SourceFilters,
    ) -> Result<Vec<RawRecord>, SourceError> {
        let client = self.ensure_conn().await?;
        for name in ALLOWED_CHANNELS {
            self.sweep_channel(client, name).await;
        }
        let rollups = self.lock_accumulator().drain_completed(chrono::Utc::now());
        tracing::info!(rollups = rollups.len(), "telegram chatter rollups drained");
        Ok(rollups.into_iter().map(RawRecord::ChatterRollup).collect())
    }

    fn normalize(&self, raw: &RawRecord) -> Result<Vec<GeoTemporalEvent>, NormalizeError> {
        match raw {
            RawRecord::ChatterRollup(rollup) => {
                chatter::normalize_rollup(rollup, SourceId::Telegram)
            }
            other => Err(NormalizeError::InvalidValue {
                field: "record",
                detail: format!("telegram source received a foreign record: {other:?}"),
            }),
        }
    }
}
