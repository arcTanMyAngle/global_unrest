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
//!
//! # One search at a time, three providers at once
//!
//! The worker used to await GDELT, then Bluesky, then Telegram in a row, so
//! the page showed nothing until the slowest of the three answered and a
//! single stalled provider spent the whole search budget. The legs hit three
//! unrelated hosts, so there was never a rate-limit argument for running them
//! in sequence — the "one search at a time" rule is about *searches*, not
//! about the legs within one.
//!
//! Each leg is now its own future under a [`FuturesUnordered`], and each
//! finishing leg is published on its own. Two consequences are worth naming
//! because they are the reason for the shape of this module:
//!
//! - **Results arrive out of order**, so every message carries a
//!   [`Generation`]. The UI stamps a search with a generation when it
//!   dispatches it and discards anything older, because a slow provider from
//!   the previous place can otherwise land in the new place's list.
//! - **Deadlines are per leg**, not per search. `media_search`'s HTTP client
//!   has a 30 s *total request* timeout, which cannot express "tell the user
//!   something within ten seconds", so the timing lives here.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use media_search::{MediaHit, MediaQuery, Provider};
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
    use core_types::SourceError;
    use media_search::{MediaHit, MediaQuery};

    pub struct MediaSearch;

    pub const BUILT: bool = false;

    pub fn make() -> Option<MediaSearch> {
        None
    }

    // The legs, not the combined `search()`: the worker drives one future per
    // provider, so the stub has to offer the same shape the live client does.
    impl MediaSearch {
        pub async fn gdelt(&self, _: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
            unreachable!("built without the media-live feature")
        }

        pub async fn bluesky(&self, _: &MediaQuery) -> Result<Vec<MediaHit>, SourceError> {
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

/// Which search a message belongs to.
///
/// Allocated by the UI at dispatch, not by the worker, so the page knows what
/// it is waiting for the instant the button is clicked rather than after the
/// first round trip. Anything older than the current generation is dropped on
/// arrival: a provider that answers after the person has moved on to another
/// place must not append to that place's results.
pub type Generation = u64;

/// How long the worker waits, and for what.
///
/// A struct rather than three constants because the tests need short ones —
/// a test that actually waited 45 s to prove the deadline fires would never
/// be run. [`Deadlines::DEFAULT`] holds the shipped numbers.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    /// When to tell the person that nothing has come back yet, naming the
    /// providers still outstanding. This is the number the acceptance
    /// criterion is written against: within it the page shows either a result
    /// or a provider-specific status.
    pub slow_notice: Duration,
    /// Per-leg budget. Reaching it fails that leg alone.
    pub provider: Duration,
    /// Whole-search budget. Reaching it cancels every outstanding leg by
    /// dropping its future and reports each as timed out.
    pub total: Duration,
}

impl Deadlines {
    pub const DEFAULT: Self = Self {
        slow_notice: Duration::from_secs(10),
        provider: Duration::from_secs(30),
        total: Duration::from_secs(45),
    };
}

impl Default for Deadlines {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Results from the worker back to the UI.
///
/// One search produces `Started`, then any number of per-provider messages in
/// completion order, then exactly one `Finished` — including when it is
/// superseded, rejected, or times out. The page relies on that invariant to
/// clear its spinner.
#[derive(Debug)]
pub enum MediaMsg {
    /// A search has begun against `providers`. Sent before any network call,
    /// so the page can name what it is waiting for.
    Started {
        generation: Generation,
        query: Box<MediaQuery>,
        providers: Vec<Provider>,
    },
    /// One provider answered. Its hits are merged into whatever has already
    /// arrived rather than replacing them.
    ProviderFinished {
        generation: Generation,
        provider: Provider,
        hits: Vec<MediaHit>,
    },
    /// One provider failed or ran out of time. Named rather than folded into
    /// an empty result list, so one rate-limited API reads as "news is
    /// unavailable right now" and not as "nothing happened here".
    ProviderFailed {
        generation: Generation,
        provider: Provider,
        problem: String,
    },
    /// [`Deadlines::slow_notice`] elapsed with these providers outstanding.
    StillWaiting {
        generation: Generation,
        providers: Vec<Provider>,
    },
    /// The query never reached a provider.
    Rejected {
        generation: Generation,
        problem: String,
    },
    /// No more messages for this generation.
    Finished { generation: Generation },
}

impl MediaMsg {
    pub fn generation(&self) -> Generation {
        match self {
            MediaMsg::Started { generation, .. }
            | MediaMsg::ProviderFinished { generation, .. }
            | MediaMsg::ProviderFailed { generation, .. }
            | MediaMsg::StillWaiting { generation, .. }
            | MediaMsg::Rejected { generation, .. }
            | MediaMsg::Finished { generation } => *generation,
        }
    }
}

enum Ctl {
    Search {
        generation: Generation,
        query: Box<MediaQuery>,
    },
}

/// UI-side handle. Dropping it stops the worker.
pub struct MediaHandle {
    ctl: tokio_mpsc::UnboundedSender<Ctl>,
    available: bool,
    next_generation: AtomicU64,
}

impl MediaHandle {
    pub fn available(&self) -> bool {
        self.available
    }

    /// Dispatch a search and return the generation it will report under.
    ///
    /// The caller must keep this and ignore messages from earlier ones — see
    /// [`MediaSession::apply`], which is where that happens in the app.
    pub fn search(&self, query: MediaQuery) -> Generation {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let _ = self.ctl.send(Ctl::Search {
            generation,
            query: Box::new(query),
        });
        generation
    }
}

/// Everything the media page knows about the search on screen.
///
/// Split out of `App` so the ordering rules — merge on arrival, discard
/// superseded generations, keep the played clip selected while later results
/// land around it — are testable without an egui context. Every field is
/// session-scoped: none of it reaches storage.
#[derive(Default)]
pub struct MediaSession {
    generation: Generation,
    /// The place text the current search was dispatched for, for status lines.
    place: String,
    pub hits: Vec<MediaHit>,
    /// Providers that failed on the current search.
    pub problems: Vec<String>,
    /// Providers not yet heard from.
    pub waiting: Vec<Provider>,
    pub searching: bool,
    /// The clip in the player, held **by URL rather than by index**: later
    /// results merge in by timestamp, which renumbers everything below them,
    /// and an index would silently start pointing at a different clip.
    selected_url: Option<String>,
    pub status: Option<String>,
    /// Whether the slow notice has fired for this search.
    slow: bool,
}

impl MediaSession {
    /// Called by the page the moment a search is dispatched, with the
    /// generation [`MediaHandle::search`] returned.
    ///
    /// The previous place's results are cleared here rather than left on
    /// screen until the first reply: a stale list under a new place's heading
    /// is worse than an empty one.
    pub fn begin(&mut self, generation: Generation, place: &str) {
        self.generation = generation;
        self.place = place.trim().to_string();
        self.hits.clear();
        self.problems.clear();
        self.waiting.clear();
        self.selected_url = None;
        self.searching = true;
        self.slow = false;
    }

    /// Note a failure that never reached the worker.
    pub fn reject(&mut self, problem: impl Into<String>) {
        self.searching = false;
        self.status = Some(problem.into());
    }

    /// Fold one worker message in. Returns `false` if it belonged to a
    /// superseded search and was discarded.
    pub fn apply(&mut self, msg: MediaMsg, window: &str) -> bool {
        if msg.generation() != self.generation {
            return false;
        }
        match msg {
            MediaMsg::Started {
                query, providers, ..
            } => {
                self.place = query.place.trim().to_string();
                self.waiting = providers;
                self.searching = true;
            }
            MediaMsg::ProviderFinished { provider, hits, .. } => {
                self.waiting.retain(|p| *p != provider);
                self.hits.extend(hits);
                // Re-merge on every arrival so the list is ordered by time
                // across providers rather than clumped by whoever answered
                // first, and so a clip both a news leg and a social leg found
                // appears once.
                self.hits = media_search::merge(std::mem::take(&mut self.hits));
                self.keep_selection();
            }
            MediaMsg::ProviderFailed {
                provider, problem, ..
            } => {
                self.waiting.retain(|p| *p != provider);
                self.problems
                    .push(format!("{}: {problem}", provider.label()));
            }
            MediaMsg::StillWaiting { providers, .. } => {
                self.waiting = providers;
                self.slow = true;
            }
            MediaMsg::Rejected { problem, .. } => {
                self.problems.push(problem);
            }
            MediaMsg::Finished { .. } => {
                self.searching = false;
                self.waiting.clear();
            }
        }
        self.status = Some(self.compose_status(window));
        true
    }

    /// Drop a selection whose clip is no longer in the list. The URL survives
    /// a re-merge, so a clip playing when a later provider answers keeps
    /// playing.
    fn keep_selection(&mut self) {
        if let Some(url) = &self.selected_url
            && !self.hits.iter().any(|h| &h.url == url)
        {
            self.selected_url = None;
        }
    }

    fn compose_status(&self, window: &str) -> String {
        let place = if self.place.is_empty() {
            "that place"
        } else {
            &self.place
        };
        let n = self.hits.len();
        let plural = if n == 1 { "" } else { "s" };
        if !self.searching {
            return if n == 0 {
                format!("no video found for {place} in the {window}")
            } else {
                format!("{n} result{plural} for {place} · {window}")
            };
        }
        let waiting: Vec<&str> = self.waiting.iter().map(|p| p.label()).collect();
        match (n, waiting.is_empty(), self.slow) {
            (_, true, _) => format!("searching {place} · {window}…"),
            (0, false, false) => format!("searching {place} · {window}…"),
            (0, false, true) => format!("still waiting on {} · {window}", waiting.join(", ")),
            (_, false, _) => format!(
                "{n} result{plural} so far · waiting on {}",
                waiting.join(", ")
            ),
        }
    }

    pub fn is_selected(&self, hit: &MediaHit) -> bool {
        self.selected_url.as_deref() == Some(hit.url.as_str())
    }

    pub fn select(&mut self, hit: &MediaHit) {
        self.selected_url = Some(hit.url.clone());
    }

    pub fn selected_hit(&self) -> Option<&MediaHit> {
        let url = self.selected_url.as_deref()?;
        self.hits.iter().find(|h| h.url == url)
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
            runtime.block_on(worker(
                search,
                telegram,
                tx_res,
                rx_ctl,
                wake,
                Deadlines::DEFAULT,
            ));
        })
        .expect("spawn media-search thread");

    (
        rx_res,
        MediaHandle {
            ctl: tx_ctl,
            available,
            next_generation: AtomicU64::new(1),
        },
    )
}

/// One provider's answer.
struct LegDone {
    provider: Provider,
    result: Result<Vec<MediaHit>, String>,
}

/// A boxed leg.
///
/// Boxed because the three legs are three different future types, and *local*
/// (no `Send` bound) because the worker runs on a current-thread runtime and
/// the futures borrow the client. That is also why this is a
/// [`FuturesUnordered`] rather than a `JoinSet`: it polls the futures in
/// place instead of requiring them to be `Send` and spawnable.
type Leg<'a> = Pin<Box<dyn Future<Output = LegDone> + 'a>>;

fn leg<'a, F, E>(provider: Provider, budget: Duration, fut: F) -> Leg<'a>
where
    F: Future<Output = Result<Vec<MediaHit>, E>> + 'a,
    E: std::fmt::Display + 'a,
{
    Box::pin(async move {
        let result = match tokio::time::timeout(budget, fut).await {
            Ok(Ok(hits)) => Ok(hits),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("no answer within {budget:?}")),
        };
        LegDone { provider, result }
    })
}

/// What the worker should do once a search returns.
enum Next {
    /// Wait for the next request.
    Idle,
    /// A request arrived mid-search and superseded it.
    Queued(Ctl),
    /// The UI is gone.
    Shutdown,
}

async fn worker(
    search: api::MediaSearch,
    telegram: Option<telegram::TelegramSource>,
    tx: mpsc::Sender<MediaMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
    deadlines: Deadlines,
) {
    let mut queued: Option<Ctl> = None;
    loop {
        let mut ctl = match queued.take() {
            Some(ctl) => ctl,
            None => match rx_ctl.recv().await {
                Some(ctl) => ctl,
                None => return,
            },
        };
        // Impatient clicking queues several searches; only the last one is
        // worth a request. Skipping the others here means they never open a
        // socket, rather than opening one and discarding the answer.
        while let Ok(next) = rx_ctl.try_recv() {
            ctl = next;
        }
        let Ctl::Search { generation, query } = ctl;
        match run_search(
            &search,
            telegram.as_ref(),
            &tx,
            &mut rx_ctl,
            &wake,
            deadlines,
            generation,
            query,
        )
        .await
        {
            Next::Idle => {}
            Next::Queued(ctl) => queued = Some(ctl),
            Next::Shutdown => return,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_search(
    search: &api::MediaSearch,
    telegram: Option<&telegram::TelegramSource>,
    tx: &mpsc::Sender<MediaMsg>,
    rx_ctl: &mut tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: &impl Fn(),
    deadlines: Deadlines,
    generation: Generation,
    query: Box<MediaQuery>,
) -> Next {
    // `false` means the UI is gone, which ends the worker rather than the
    // search: there is nobody left to show a result to.
    let send = |msg: MediaMsg| -> bool {
        if tx.send(msg).is_err() {
            return false;
        }
        wake();
        true
    };

    let mut waiting = vec![Provider::Gdelt, Provider::Bluesky];
    if telegram.is_some() {
        waiting.push(Provider::Telegram);
    }
    let valid = query.is_valid();
    if !send(MediaMsg::Started {
        generation,
        query: query.clone(),
        providers: if valid { waiting.clone() } else { Vec::new() },
    }) {
        return Next::Shutdown;
    }
    if !valid {
        // The page guards this too; the worker guards it as well so no
        // caller can spend a request on a query no provider would accept.
        let rejected = send(MediaMsg::Rejected {
            generation,
            problem: "a media search needs a place and a time window".to_string(),
        }) && send(MediaMsg::Finished { generation });
        return if rejected { Next::Idle } else { Next::Shutdown };
    }

    let mut legs: FuturesUnordered<Leg<'_>> = FuturesUnordered::new();
    legs.push(leg(
        Provider::Gdelt,
        deadlines.provider,
        search.gdelt(&query),
    ));
    legs.push(leg(
        Provider::Bluesky,
        deadlines.provider,
        search.bluesky(&query),
    ));
    if let Some(telegram) = telegram {
        legs.push(leg(
            Provider::Telegram,
            deadlines.provider,
            telegram.search_media(&query),
        ));
    }

    let slow = tokio::time::sleep(deadlines.slow_notice);
    let total = tokio::time::sleep(deadlines.total);
    tokio::pin!(slow, total);
    let mut slow_fired = false;
    // A closed control channel is not a reason to abandon a search in
    // progress: `recv()` on a dropped sender is ready forever, so without
    // this the supersession branch would win every race and end the search
    // before a single leg was polled.
    let mut ctl_open = true;

    while !waiting.is_empty() {
        tokio::select! {
            // Biased so a leg that is ready is always published before the
            // loop considers giving up on it or moving to another search.
            biased;

            Some(done) = legs.next() => {
                waiting.retain(|p| *p != done.provider);
                let delivered = match done.result {
                    Ok(hits) => send(MediaMsg::ProviderFinished {
                        generation,
                        provider: done.provider,
                        hits,
                    }),
                    Err(problem) => send(MediaMsg::ProviderFailed {
                        generation,
                        provider: done.provider,
                        problem,
                    }),
                };
                if !delivered {
                    return Next::Shutdown;
                }
            }

            _ = &mut slow, if !slow_fired => {
                slow_fired = true;
                if !send(MediaMsg::StillWaiting {
                    generation,
                    providers: waiting.clone(),
                }) {
                    return Next::Shutdown;
                }
            }

            _ = &mut total => {
                // Dropping `legs` below is the cancellation: an in-flight
                // request is abandoned rather than left running against a
                // search nobody is waiting for any more.
                for provider in std::mem::take(&mut waiting) {
                    if !send(MediaMsg::ProviderFailed {
                        generation,
                        provider,
                        problem: format!("search deadline reached ({:?})", deadlines.total),
                    }) {
                        return Next::Shutdown;
                    }
                }
            }

            ctl = rx_ctl.recv(), if ctl_open => {
                match ctl {
                    // A new search supersedes this one; its legs are dropped
                    // unfinished. `Finished` still goes out, so the page is
                    // never left believing this generation is in flight.
                    Some(ctl) => {
                        if !send(MediaMsg::Finished { generation }) {
                            return Next::Shutdown;
                        }
                        return Next::Queued(ctl);
                    }
                    // The handle is gone. Let this search finish and report -
                    // the results are already paid for - and let the outer
                    // loop end the worker.
                    None => ctl_open = false,
                }
            }
        }
    }

    if send(MediaMsg::Finished { generation }) {
        Next::Idle
    } else {
        Next::Shutdown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, TimeZone, Utc};

    fn query() -> Box<MediaQuery> {
        Box::new(MediaQuery {
            place: "Paris".into(),
            topic: String::new(),
            start: Utc::now() - ChronoDuration::hours(24),
            end: Utc::now(),
            limit: 20,
        })
    }

    fn hit(url: &str, provider: Provider, secs: i64) -> MediaHit {
        MediaHit {
            url: url.into(),
            title: "t".into(),
            origin: "o".into(),
            provider,
            ts_utc: Utc.timestamp_opt(secs, 0).unwrap(),
        }
    }

    /// A session mid-search, generation 7, waiting on both keyless legs.
    fn session() -> MediaSession {
        let mut s = MediaSession::default();
        s.begin(7, "Paris");
        s.apply(
            MediaMsg::Started {
                generation: 7,
                query: query(),
                providers: vec![Provider::Gdelt, Provider::Bluesky],
            },
            "last 24h",
        );
        s
    }

    #[test]
    fn shipped_deadlines_are_the_numbers_the_acceptance_criterion_names() {
        assert_eq!(Deadlines::DEFAULT.slow_notice, Duration::from_secs(10));
        assert_eq!(Deadlines::DEFAULT.total, Duration::from_secs(45));
        assert!(
            Deadlines::DEFAULT.provider < Deadlines::DEFAULT.total,
            "a per-leg budget at or above the whole-search one can never fire"
        );
    }

    #[test]
    fn a_superseded_generation_is_discarded_rather_than_appended() {
        let mut s = session();
        // The previous place's slow provider finally answers.
        let applied = s.apply(
            MediaMsg::ProviderFinished {
                generation: 6,
                provider: Provider::Gdelt,
                hits: vec![hit("https://youtu.be/old", Provider::Gdelt, 10)],
            },
            "last 24h",
        );
        assert!(!applied);
        assert!(s.hits.is_empty(), "{:?}", s.hits);
        assert!(s.searching, "an old message must not clear the spinner");
    }

    #[test]
    fn later_results_merge_in_by_time_instead_of_trailing_the_first_provider() {
        let mut s = session();
        s.apply(
            MediaMsg::ProviderFinished {
                generation: 7,
                provider: Provider::Gdelt,
                hits: vec![hit("https://youtu.be/older", Provider::Gdelt, 10)],
            },
            "last 24h",
        );
        s.apply(
            MediaMsg::ProviderFinished {
                generation: 7,
                provider: Provider::Bluesky,
                hits: vec![hit("https://bsky.app/newer", Provider::Bluesky, 99)],
            },
            "last 24h",
        );
        let urls: Vec<&str> = s.hits.iter().map(|h| h.url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://bsky.app/newer", "https://youtu.be/older"]
        );
        assert!(s.waiting.is_empty());
    }

    #[test]
    fn the_clip_in_the_player_survives_a_later_provider_answering() {
        let mut s = session();
        let playing = hit("https://youtu.be/playing", Provider::Gdelt, 10);
        s.apply(
            MediaMsg::ProviderFinished {
                generation: 7,
                provider: Provider::Gdelt,
                hits: vec![playing.clone()],
            },
            "last 24h",
        );
        s.select(&playing);
        // Three newer posts arrive and sort above it, so what was index 0 is
        // now index 3. An index-based selection would have moved the player.
        s.apply(
            MediaMsg::ProviderFinished {
                generation: 7,
                provider: Provider::Bluesky,
                hits: vec![
                    hit("https://bsky.app/a", Provider::Bluesky, 90),
                    hit("https://bsky.app/b", Provider::Bluesky, 80),
                    hit("https://bsky.app/c", Provider::Bluesky, 70),
                ],
            },
            "last 24h",
        );
        assert_eq!(s.hits.len(), 4);
        assert_eq!(
            s.selected_hit().map(|h| h.url.as_str()),
            Some(playing.url.as_str()),
            "the player must not jump to another clip when results arrive"
        );
    }

    #[test]
    fn a_failed_provider_is_named_and_does_not_empty_the_list() {
        let mut s = session();
        s.apply(
            MediaMsg::ProviderFinished {
                generation: 7,
                provider: Provider::Gdelt,
                hits: vec![hit("https://youtu.be/a", Provider::Gdelt, 10)],
            },
            "last 24h",
        );
        s.apply(
            MediaMsg::ProviderFailed {
                generation: 7,
                provider: Provider::Bluesky,
                problem: "429 rate limited".into(),
            },
            "last 24h",
        );
        s.apply(MediaMsg::Finished { generation: 7 }, "last 24h");
        assert_eq!(
            s.hits.len(),
            1,
            "one leg failing must not discard the other"
        );
        assert_eq!(s.problems, vec!["bluesky: 429 rate limited"]);
        assert!(!s.searching);
        assert_eq!(s.status.as_deref(), Some("1 result for Paris · last 24h"));
    }

    #[test]
    fn the_status_names_outstanding_providers_while_the_search_is_still_running() {
        let mut s = session();
        s.apply(
            MediaMsg::StillWaiting {
                generation: 7,
                providers: vec![Provider::Bluesky],
            },
            "last 24h",
        );
        assert_eq!(
            s.status.as_deref(),
            Some("still waiting on bluesky · last 24h")
        );
        assert!(s.searching);
    }

    #[test]
    fn an_empty_finished_search_says_so_rather_than_staying_on_the_spinner() {
        let mut s = session();
        s.apply(MediaMsg::Finished { generation: 7 }, "last 6h");
        assert!(!s.searching);
        assert!(s.waiting.is_empty());
        assert_eq!(
            s.status.as_deref(),
            Some("no video found for Paris in the last 6h")
        );
    }

    /// The orchestration itself, against local mock servers. Gated on the
    /// feature that supplies the real HTTP client: without it the legs are
    /// `unreachable!()` stubs and there is nothing to schedule.
    #[cfg(feature = "media-live")]
    mod worker_tests {
        use super::*;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        /// A local HTTP server that answers every connection with `body`
        /// after `delay`, counting connections. A `delay` longer than any
        /// deadline under test is how a provider "never answers".
        async fn serve(body: &'static str, delay: Duration) -> (String, Arc<AtomicUsize>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let counter = requests.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        // Read the request before answering. Closing a socket
                        // that still has unread inbound data sends an RST on
                        // Windows, and the client loses the response it had
                        // already been handed -- "an existing connection was
                        // forcibly closed by the remote host", os error 10054,
                        // which arrives as a provider failure and hides
                        // whatever the test was actually about.
                        let mut seen = Vec::new();
                        loop {
                            if stream.readable().await.is_err() {
                                return;
                            }
                            let mut chunk = [0u8; 1024];
                            match stream.try_read(&mut chunk) {
                                Ok(0) => break,
                                Ok(n) => {
                                    seen.extend_from_slice(&chunk[..n]);
                                    if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                                Err(_) => return,
                            }
                        }
                        tokio::time::sleep(delay).await;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let mut sent = 0;
                        while sent < response.len() {
                            if stream.writable().await.is_err() {
                                return;
                            }
                            match stream.try_write(&response.as_bytes()[sent..]) {
                                Ok(n) => sent += n,
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                                Err(_) => return,
                            }
                        }
                    });
                }
            });
            (format!("http://{addr}/x"), requests)
        }

        /// A GDELT DOC reply carrying one video article.
        const GDELT_BODY: &str = r#"{"articles":[{"url":"https://www.youtube.com/watch?v=aaaaaaaaaaa","title":"Flood footage","domain":"youtube.com","seendate":"20260101T000000Z"}]}"#;

        /// A Bluesky reply carrying one video post.
        const BSKY_BODY: &str = r#"{"posts":[{"uri":"at://did:plc:zzz/app.bsky.feed.post/abc","cid":"c","author":{"handle":"someone.bsky.social"},"record":{"text":"clip","createdAt":"2026-01-01T00:00:00Z"},"embed":{"$type":"app.bsky.embed.video#view","playlist":"https://video.bsky.app/x/playlist.m3u8"}}]}"#;

        /// Short deadlines so the timeout paths are testable in milliseconds
        /// rather than in the shipped 10/30/45 seconds.
        fn short() -> Deadlines {
            Deadlines {
                slow_notice: Duration::from_millis(80),
                provider: Duration::from_millis(400),
                total: Duration::from_millis(600),
            }
        }

        /// Run one search to completion against two mock endpoints.
        fn run(
            gdelt: Duration,
            bluesky: Duration,
            deadlines: Deadlines,
        ) -> (Vec<MediaMsg>, Duration) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let (gdelt_url, _) = serve(GDELT_BODY, gdelt).await;
                let (bsky_url, _) = serve(BSKY_BODY, bluesky).await;
                let search = media_search::MediaSearch::new()
                    .unwrap()
                    .with_gdelt_endpoint(gdelt_url)
                    .with_bluesky_endpoint(bsky_url);
                let (tx, rx) = mpsc::channel();
                let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();
                tx_ctl
                    .send(Ctl::Search {
                        generation: 1,
                        query: query(),
                    })
                    .unwrap();
                drop(tx_ctl);
                let started = std::time::Instant::now();
                worker(search, None, tx, rx_ctl, || {}, deadlines).await;
                (rx.into_iter().collect(), started.elapsed())
            })
        }

        fn finished(msgs: &[MediaMsg]) -> Vec<Provider> {
            msgs.iter()
                .filter_map(|m| match m {
                    MediaMsg::ProviderFinished { provider, .. } => Some(*provider),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn the_legs_run_at_the_same_time_rather_than_one_after_the_other() {
            let delay = Duration::from_millis(300);
            let (msgs, elapsed) = run(delay, delay, Deadlines::DEFAULT);
            // Sequentially this is 600ms plus overhead. The margin is wide
            // because CI machines are slow, but it cannot stay under 600ms
            // with the legs serialized.
            assert!(
                elapsed < Duration::from_millis(550),
                "two 300ms legs took {elapsed:?}"
            );
            assert_eq!(finished(&msgs).len(), 2, "{msgs:?}");
        }

        #[test]
        fn a_fast_provider_is_published_without_waiting_for_a_slow_one() {
            let (msgs, _) = run(
                Duration::from_millis(10),
                Duration::from_millis(300),
                Deadlines::DEFAULT,
            );
            assert_eq!(
                finished(&msgs),
                vec![Provider::Gdelt, Provider::Bluesky],
                "results must be published in completion order: {msgs:?}"
            );
            let first = msgs
                .iter()
                .position(|m| matches!(m, MediaMsg::ProviderFinished { .. }))
                .unwrap();
            assert!(
                first < msgs.len() - 1,
                "the fast leg was published only at the very end: {msgs:?}"
            );
        }

        #[test]
        fn a_provider_that_never_answers_is_named_and_the_search_still_finishes() {
            let (msgs, _) = run(Duration::from_millis(10), Duration::from_secs(30), short());
            let failed: Vec<&Provider> = msgs
                .iter()
                .filter_map(|m| match m {
                    MediaMsg::ProviderFailed { provider, .. } => Some(provider),
                    _ => None,
                })
                .collect();
            assert_eq!(failed, vec![&Provider::Bluesky], "{msgs:?}");
            assert!(
                msgs.iter().any(|m| matches!(
                    m,
                    MediaMsg::ProviderFinished { provider, hits, .. }
                        if *provider == Provider::Gdelt && !hits.is_empty()
                )),
                "a working leg must survive another leg failing: {msgs:?}"
            );
            assert!(matches!(msgs.last(), Some(MediaMsg::Finished { .. })));
        }

        #[test]
        fn nothing_back_by_the_slow_mark_still_names_what_it_is_waiting_on() {
            let (msgs, _) = run(
                Duration::from_millis(250),
                Duration::from_millis(250),
                short(),
            );
            let waiting = msgs
                .iter()
                .find_map(|m| match m {
                    MediaMsg::StillWaiting { providers, .. } => Some(providers.clone()),
                    _ => None,
                })
                .expect("a slow search must say what it is waiting for");
            assert_eq!(waiting, vec![Provider::Gdelt, Provider::Bluesky]);
            // It is a notice, not a verdict: both legs still complete.
            assert_eq!(finished(&msgs).len(), 2, "{msgs:?}");
        }

        #[test]
        fn every_search_ends_with_exactly_one_finished() {
            for (g, b) in [
                (Duration::from_millis(10), Duration::from_millis(10)),
                (Duration::from_millis(10), Duration::from_secs(30)),
                (Duration::from_secs(30), Duration::from_secs(30)),
            ] {
                let (msgs, _) = run(g, b, short());
                assert_eq!(
                    msgs.iter()
                        .filter(|m| matches!(m, MediaMsg::Finished { .. }))
                        .count(),
                    1,
                    "{msgs:?}"
                );
                assert!(matches!(msgs.last(), Some(MediaMsg::Finished { .. })));
            }
        }

        #[test]
        fn the_whole_search_deadline_cancels_the_legs_still_running() {
            let deadlines = Deadlines {
                slow_notice: Duration::from_millis(50),
                // Deliberately longer than `total`, so the only thing that can
                // end this search is the whole-search deadline.
                provider: Duration::from_secs(30),
                total: Duration::from_millis(200),
            };
            let (msgs, elapsed) = run(Duration::from_secs(30), Duration::from_secs(30), deadlines);
            assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
            let failed: Vec<Provider> = msgs
                .iter()
                .filter_map(|m| match m {
                    MediaMsg::ProviderFailed { provider, .. } => Some(*provider),
                    _ => None,
                })
                .collect();
            assert_eq!(failed, vec![Provider::Gdelt, Provider::Bluesky], "{msgs:?}");
            assert!(
                msgs.iter().any(|m| matches!(
                    m,
                    MediaMsg::ProviderFailed { problem, .. } if problem.contains("deadline")
                )),
                "{msgs:?}"
            );
        }

        #[test]
        fn a_second_search_supersedes_the_first_which_never_reports_results() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let msgs: Vec<MediaMsg> = runtime.block_on(async move {
                let (gdelt_url, _) = serve(GDELT_BODY, Duration::from_millis(250)).await;
                let (bsky_url, _) = serve(BSKY_BODY, Duration::from_millis(250)).await;
                let search = media_search::MediaSearch::new()
                    .unwrap()
                    .with_gdelt_endpoint(gdelt_url)
                    .with_bluesky_endpoint(bsky_url);
                let (tx, rx) = mpsc::channel();
                let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();
                tx_ctl
                    .send(Ctl::Search {
                        generation: 1,
                        query: query(),
                    })
                    .unwrap();
                let sender = tx_ctl.clone();
                tokio::spawn(async move {
                    // Late enough that generation 1 is genuinely in flight.
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let _ = sender.send(Ctl::Search {
                        generation: 2,
                        query: query(),
                    });
                });
                drop(tx_ctl);
                worker(search, None, tx, rx_ctl, || {}, Deadlines::DEFAULT).await;
                rx.into_iter().collect()
            });

            assert!(
                msgs.iter()
                    .any(|m| matches!(m, MediaMsg::Started { generation, .. } if *generation == 1))
            );
            assert!(
                !msgs.iter().any(|m| matches!(
                    m,
                    MediaMsg::ProviderFinished { generation, .. } if *generation == 1
                )),
                "a superseded search must not append to the new one: {msgs:?}"
            );
            assert!(
                msgs.iter()
                    .any(|m| matches!(m, MediaMsg::Finished { generation } if *generation == 1)),
                "a superseded search must still close out, or the page spins: {msgs:?}"
            );
            assert_eq!(
                msgs.iter()
                    .filter(|m| matches!(
                        m,
                        MediaMsg::ProviderFinished { generation, .. } if *generation == 2
                    ))
                    .count(),
                2,
                "{msgs:?}"
            );
        }

        #[test]
        fn queued_clicks_collapse_to_the_last_one_so_only_it_opens_a_socket() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (msgs, requests): (Vec<MediaMsg>, usize) = runtime.block_on(async move {
                let (gdelt_url, gdelt_requests) = serve(GDELT_BODY, Duration::from_millis(5)).await;
                let (bsky_url, _) = serve(BSKY_BODY, Duration::from_millis(5)).await;
                let search = media_search::MediaSearch::new()
                    .unwrap()
                    .with_gdelt_endpoint(gdelt_url)
                    .with_bluesky_endpoint(bsky_url);
                let (tx, rx) = mpsc::channel();
                let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();
                for generation in 1..=4 {
                    tx_ctl
                        .send(Ctl::Search {
                            generation,
                            query: query(),
                        })
                        .unwrap();
                }
                drop(tx_ctl);
                worker(search, None, tx, rx_ctl, || {}, Deadlines::DEFAULT).await;
                let msgs: Vec<MediaMsg> = rx.into_iter().collect();
                let requests = gdelt_requests.load(Ordering::SeqCst);
                (msgs, requests)
            });
            assert_eq!(requests, 1, "four queued clicks made {requests} requests");
            let started: Vec<Generation> = msgs
                .iter()
                .filter_map(|m| match m {
                    MediaMsg::Started { generation, .. } => Some(*generation),
                    _ => None,
                })
                .collect();
            assert_eq!(started, vec![4], "{msgs:?}");
        }

        #[test]
        fn a_query_with_no_place_is_refused_without_a_request() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (msgs, requests): (Vec<MediaMsg>, usize) = runtime.block_on(async move {
                let (gdelt_url, gdelt_requests) = serve(GDELT_BODY, Duration::from_millis(5)).await;
                let (bsky_url, _) = serve(BSKY_BODY, Duration::from_millis(5)).await;
                let search = media_search::MediaSearch::new()
                    .unwrap()
                    .with_gdelt_endpoint(gdelt_url)
                    .with_bluesky_endpoint(bsky_url);
                let (tx, rx) = mpsc::channel();
                let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();
                let mut bad = query();
                bad.place = "   ".into();
                tx_ctl
                    .send(Ctl::Search {
                        generation: 1,
                        query: bad,
                    })
                    .unwrap();
                drop(tx_ctl);
                worker(search, None, tx, rx_ctl, || {}, Deadlines::DEFAULT).await;
                let msgs: Vec<MediaMsg> = rx.into_iter().collect();
                let requests = gdelt_requests.load(Ordering::SeqCst);
                (msgs, requests)
            });
            assert_eq!(requests, 0, "an invalid query must not reach a provider");
            assert!(
                msgs.iter().any(|m| matches!(m, MediaMsg::Rejected { .. })),
                "{msgs:?}"
            );
            assert!(matches!(msgs.last(), Some(MediaMsg::Finished { .. })));
        }
    }
}
