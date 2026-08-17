//! The network path (feature `live`): MTProto over a real Telegram account
//! session, polled on a fixed cadence like NOAA/IODA (not streamed — unlike
//! Bluesky, Telegram has no keyless public firehose).
//!
//! **Login is a one-time, out-of-band step.** Telegram's account login needs
//! a phone number and an SMS/app code, which cannot be automated from a
//! long-lived worker/desktop process. `examples/login_setup.rs` is a small
//! interactive tool: run it once, and it saves a local session file (see
//! [`crate::file_session`] for why that file is JSON and not SQLite).
//! Every subsequent run of the real source just opens that file — no further
//! interaction. If the file is missing or not yet authorized, [`fetch`]
//! returns a clear error naming the setup command rather than trying to
//! prompt for input from inside a GUI app or headless worker.
//!
//! **This module is deliberately thin.** Everything that decides *what* to
//! sweep, what to keep, and what to do when a channel fails lives in ungated
//! [`crate::ChannelOrchestrator`] / [`crate::search_all`], behind the
//! [`ChannelReader`] seam, where it is testable without a session. What is
//! left here is resolve, iterate, map — the part no fake could honestly
//! stand in for anyway.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use core_types::{
    GeoTemporalEvent, NormalizeError, RawRecord, SignalSource, SourceError, SourceFilters,
    SourceId, TimeWindow,
};
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_client::session::types::PeerRef;
use grammers_client::tl::enums::MessagesFilter;
use grammers_mtsender::SenderPool;
use media_search::{MediaHit, MediaQuery};
use tokio::sync::OnceCell;

use crate::file_session::FileSession;
use crate::media::ChannelVideo;
use crate::{ChannelOrchestrator, ChannelReader, media};

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
    ingest: ChannelOrchestrator,
    /// Open the session file without writing it back. See
    /// [`TelegramSource::read_only`].
    read_only: bool,
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
        Ok(Some(Self {
            api_id,
            session_path,
            conn: OnceCell::new(),
            ingest: ChannelOrchestrator::from_bundled(chatter::DEFAULT_WINDOW_SECS)?,
            read_only: false,
        }))
    }

    /// Use the login on disk without writing back to it.
    ///
    /// For the **second** client on one session file — the desktop runs this
    /// source twice: once polling chatter, once answering
    /// [`TelegramSource::search_media`]. Two writers would take turns
    /// overwriting each other's cached peers and pay for it in
    /// `resolve_username` flood waits, so the media instance reads only. See
    /// [`FileSession::load_read_only`] for why that costs nothing.
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Open (or reuse) the MTProto connection. On failure the cell stays
    /// uninitialized, so the next `fetch` retries rather than being stuck.
    async fn ensure_conn(&self) -> Result<&Client, SourceError> {
        let conn =
            self.conn
                .get_or_try_init(|| async {
                    let session = if self.read_only {
                        FileSession::load_read_only(&self.session_path)
                    } else {
                        FileSession::load(&self.session_path)
                    }
                    .map_err(|e| {
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

    /// On-demand video lookup across the allowlist — the user-directed
    /// exception to this crate's aggregate-only rule. The bounds, and why
    /// they hold, are documented on [`crate::search_all`]; this only supplies
    /// the connection.
    pub async fn search_media(&self, query: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
        // `search_all` rejects these too, but checking before `ensure_conn`
        // keeps an unusable query from opening a session for nothing.
        if !query.is_valid() || media::query_text(&query.place, &query.topic).is_none() {
            return Ok(Vec::new());
        }
        let client = self.ensure_conn().await?;
        crate::search_all(&GrammersReader { client }, query).await
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
        self.ingest.sweep_all(&GrammersReader { client }).await;
        Ok(self.ingest.drain_completed(chrono::Utc::now()))
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

/// The grammers half of the seam.
struct GrammersReader<'a> {
    client: &'a Client,
}

impl GrammersReader<'_> {
    /// Resolve a public username to something addressable.
    ///
    /// `Ok(None)` means the channel is not there — renamed, deleted, or never
    /// public. That is absence, not failure, and the caller treats it as
    /// such; only a real error gets counted against a channel.
    async fn peer(&self, channel: &str) -> Result<Option<PeerRef>, SourceError> {
        let peer = self
            .client
            .resolve_username(channel)
            .await
            .map_err(|e| SourceError::Other(format!("resolving @{channel}: {e}")))?;
        let Some(peer) = peer else {
            tracing::warn!(channel, "telegram channel not found; skipping");
            return Ok(None);
        };
        let peer_ref = peer
            .to_ref()
            .await
            .map_err(|e| SourceError::Other(format!("addressing @{channel}: {e}")))?;
        if peer_ref.is_none() {
            tracing::warn!(
                channel,
                "telegram channel has no addressable peer; skipping"
            );
        }
        Ok(peer_ref)
    }
}

impl ChannelReader for GrammersReader<'_> {
    async fn sweep_history(
        &self,
        channel: &str,
        after: Option<i32>,
        limit: usize,
        on_message: &mut dyn FnMut(i32, &str, DateTime<Utc>),
    ) -> Result<(), SourceError> {
        let Some(peer_ref) = self.peer(channel).await? else {
            return Ok(());
        };
        let mut iter = self.client.iter_messages(peer_ref);
        iter = match after {
            Some(id) => iter.offset_id(id).reverse(true).limit(limit),
            None => iter.limit(limit),
        };
        loop {
            match iter.next().await {
                // Borrowed, folded into the accumulator, and gone — nothing
                // here accumulates message text.
                Ok(Some(msg)) => on_message(msg.id(), msg.text(), msg.date()),
                Ok(None) => return Ok(()),
                Err(e) => return Err(SourceError::Other(format!("reading @{channel}: {e}"))),
            }
        }
    }

    async fn search_videos(
        &self,
        channel: &str,
        text: &str,
        query: &MediaQuery,
    ) -> Result<Vec<ChannelVideo>, SourceError> {
        let Some(peer_ref) = self.peer(channel).await? else {
            return Ok(Vec::new());
        };

        // `min_date`/`max_date` want a fixed-offset timestamp; the query
        // carries UTC, so this is a representation change, not a shift.
        let mut iter = self
            .client
            .search_messages(peer_ref)
            .query(text)
            .filter(MessagesFilter::InputMessagesFilterVideo)
            .min_date(&query.start.fixed_offset())
            .max_date(&query.end.fixed_offset())
            .limit(media::PER_CHANNEL_LIMIT);

        let mut found = Vec::new();
        loop {
            let msg = match iter.next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => return Err(SourceError::Other(format!("searching @{channel}: {e}"))),
            };
            // Only the message id, its own caption, and its date are read —
            // never `sender()`.
            let document = match msg.media() {
                Some(Media::Document(doc)) => Some((
                    doc.mime_type().map(str::to_owned),
                    doc.name().map(str::to_owned),
                )),
                _ => None,
            };
            let has_document = document.is_some();
            let (mime_type, file_name) = document.unwrap_or((None, None));
            found.push(ChannelVideo {
                id: msg.id(),
                caption: msg.text().to_owned(),
                date: msg.date(),
                mime_type,
                file_name,
                has_document,
            });
        }
        Ok(found)
    }
}
