//! Media-search worker: a long-lived thread with a current-thread tokio
//! runtime that answers one place-scoped media query at a time.
//!
//! Like [`crate::digest`] and unlike [`crate::ingest`], this worker has **no
//! cadence**. Nothing is fetched until a person types a place and presses
//! search. That is the whole design: results are transient, scoped to one
//! place and one window, and never written to the database — see
//! `media_search`'s module docs and docs/SAFETY_AND_PRIVACY.md's "On-demand
//! media lookup" section.
//!
//! The worker never touches storage. It receives a query, returns hits, and
//! the page holds them until the next search replaces them.

use std::sync::mpsc;

use media_search::{MediaHit, MediaQuery};
use tokio::sync::mpsc as tokio_mpsc;

/// Feature-gated network handle — the same stub-module pattern the live
/// sources use, so the worker body stays free of `cfg` arms. With the feature
/// off `make()` yields `None` and the page says so instead of spinning.
#[cfg(feature = "media-live")]
mod api {
    pub use media_search::MediaSearch;

    pub const BUILT: bool = true;

    pub fn make() -> Option<MediaSearch> {
        match MediaSearch::new() {
            Ok(search) => Some(search),
            Err(e) => {
                tracing::warn!("media search unavailable: {e}");
                None
            }
        }
    }
}
#[cfg(not(feature = "media-live"))]
mod api {
    use media_search::{MediaHit, MediaQuery};

    pub struct MediaSearch;

    pub const BUILT: bool = false;

    pub fn make() -> Option<MediaSearch> {
        None
    }

    impl MediaSearch {
        pub async fn search(&self, _: &MediaQuery) -> (Vec<MediaHit>, Vec<String>) {
            unreachable!("built without the media-live feature")
        }
    }
}

/// The Telegram leg, gated separately because it rides the credentialed
/// MTProto session rather than a keyless HTTP API — same stub-module shape as
/// [`crate::ingest`]'s sources. It is optional in a stronger sense than the
/// others: with no session file configured the rest of the search still runs.
///
/// It lives in `source-telegram` and not in `media_search` because the search
/// needs that session; making `media-search` depend on `source-telegram` would
/// cycle the crate graph.
#[cfg(feature = "telegram-live")]
mod telegram {
    pub use source_telegram::TelegramSource;

    /// Read-only on the session file: `ingest`'s poller owns the same file,
    /// and two writers would overwrite each other's cached peers.
    pub fn make() -> Option<TelegramSource> {
        match TelegramSource::from_env() {
            Ok(source) => source.map(TelegramSource::read_only),
            Err(e) => {
                tracing::warn!("telegram media search unavailable: {e}");
                None
            }
        }
    }
}
#[cfg(not(feature = "telegram-live"))]
mod telegram {
    use core_types::SourceError;
    use media_search::{MediaHit, MediaQuery};

    pub struct TelegramSource;

    pub fn make() -> Option<TelegramSource> {
        None
    }

    impl TelegramSource {
        pub async fn search_media(&self, _: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
            unreachable!("built without the telegram-live feature")
        }
    }
}

/// Why searching is unavailable, in the words the page shows.
pub fn unavailable_reason() -> &'static str {
    if api::BUILT {
        "The media search could not start its HTTP client — see the log."
    } else {
        "This build has the `media-live` feature off, so it cannot search for media."
    }
}

/// Results from the worker back to the UI.
pub enum MediaMsg {
    /// A search finished. `problems` lists providers that failed, so one
    /// rate-limited API reads as "news is unavailable right now" rather than
    /// as "nothing happened here".
    Results {
        query: Box<MediaQuery>,
        hits: Vec<MediaHit>,
        problems: Vec<String>,
    },
}

enum Ctl {
    Search(Box<MediaQuery>),
}

/// UI-side handle. Dropping it stops the worker.
pub struct MediaHandle {
    ctl: tokio_mpsc::UnboundedSender<Ctl>,
    available: bool,
}

impl MediaHandle {
    pub fn available(&self) -> bool {
        self.available
    }

    pub fn search(&self, query: MediaQuery) {
        let _ = self.ctl.send(Ctl::Search(Box::new(query)));
    }
}

/// Spawn the media worker. `wake` (a repaint request) fires after every
/// message so the UI polls promptly.
pub fn spawn(wake: impl Fn() + Send + 'static) -> (mpsc::Receiver<MediaMsg>, MediaHandle) {
    let (tx_res, rx_res) = mpsc::channel();
    let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();

    let search = api::make();
    let available = search.is_some();
    // Absent when there's no Telegram login configured. That is not a reason
    // to disable searching — the keyless legs carry it.
    let telegram = telegram::make();

    std::thread::Builder::new()
        .name("media-search".into())
        .spawn(move || {
            let Some(search) = search else {
                // Drop the receiver so a stray request fails fast rather than
                // leaving the page on a spinner forever.
                return;
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("media tokio runtime: {e}");
                    return;
                }
            };
            runtime.block_on(worker(search, telegram, tx_res, rx_ctl, wake));
        })
        .expect("spawn media-search thread");

    (
        rx_res,
        MediaHandle {
            ctl: tx_ctl,
            available,
        },
    )
}

async fn worker(
    search: api::MediaSearch,
    telegram: Option<telegram::TelegramSource>,
    tx: mpsc::Sender<MediaMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
) {
    while let Some(Ctl::Search(query)) = rx_ctl.recv().await {
        // One search at a time, in arrival order. These are user-initiated
        // requests against public rate-limited APIs; concurrency would only
        // buy a 429.
        let (mut hits, mut problems) = search.search(&query).await;
        if let Some(telegram) = telegram.as_ref() {
            match telegram.search_media(&query).await {
                Ok(found) => hits.extend(found),
                Err(e) => problems.push(format!("telegram: {e}")),
            }
            // Merge again so the Telegram hits interleave by time rather than
            // trailing the keyless ones. No extra cap: `search_media` already
            // holds itself to `query.limit`, which is what each keyless
            // provider contributes too.
            hits = media_search::merge(hits);
        }
        if tx
            .send(MediaMsg::Results {
                query,
                hits,
                problems,
            })
            .is_err()
        {
            return;
        }
        wake();
    }
}
