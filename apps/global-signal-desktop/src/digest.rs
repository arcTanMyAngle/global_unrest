//! Daily Events worker: a long-lived thread with a current-thread tokio
//! runtime that turns a day's facts into a written digest via the Gemini
//! `generateContent` API.
//!
//! Unlike [`crate::ingest`] this worker has no cadence of its own — it only
//! acts when the user asks for a day. Every generation sends stored records to
//! a third party and draws on a metered quota, so nothing here is automatic:
//! the page reads the cache, and a request only leaves the machine when the
//! user clicks generate for a day with no cached row.
//!
//! The worker never touches storage (the UI thread owns it, as everywhere
//! else): it receives the facts, returns the written digest, and the app
//! caches it.

use std::sync::mpsc;

use daily_digest::{DayKey, DigestError, DigestFacts};
use tokio::sync::mpsc as tokio_mpsc;

/// Feature-gated Gemini handle — the same stub-module pattern the live
/// sources use, so the worker body stays free of `cfg` arms. With the feature
/// off `make()` is always `Ok(None)`, which the page reports exactly like a
/// missing API key: the cache still reads, generation is unavailable.
#[cfg(feature = "gemini-live")]
mod api {
    pub use daily_digest::GeminiDigester;

    /// Built with the network half; a missing digester means a missing key.
    pub const BUILT: bool = true;

    pub fn make() -> Result<Option<GeminiDigester>, daily_digest::DigestError> {
        GeminiDigester::from_env()
    }
}
#[cfg(not(feature = "gemini-live"))]
mod api {
    use daily_digest::{DayDigest, DigestError, DigestFacts};

    pub struct GeminiDigester;

    pub const BUILT: bool = false;

    pub fn make() -> Result<Option<GeminiDigester>, DigestError> {
        Ok(None)
    }

    impl GeminiDigester {
        pub async fn generate(&self, _: &DigestFacts, _: i64) -> Result<DayDigest, DigestError> {
            unreachable!("built without the gemini-live feature")
        }
    }
}

/// Why generation is unavailable, in the words the page shows.
pub fn unavailable_reason() -> &'static str {
    if api::BUILT {
        "Set GEMINI_API_KEY (in the environment or a .env file) to write new \
         digests. Days already generated stay readable without it."
    } else {
        "This build has the `gemini-live` feature off, so it can only read \
         previously generated digests."
    }
}

/// Results from the worker back to the UI.
pub enum DigestMsg {
    /// A day was written. The app caches it and shows it.
    Written(Box<daily_digest::DayDigest>),
    /// Generation failed for this day; the message is already user-facing
    /// (never carries the API key — see `daily_digest::live`).
    Failed { day: DayKey, message: String },
}

/// Commands from the UI to the worker.
enum Ctl {
    Generate(Box<DigestFacts>),
}

/// UI-side handle. Dropping it stops the worker.
pub struct DigestHandle {
    ctl: tokio_mpsc::UnboundedSender<Ctl>,
    /// Whether this process can generate at all: the feature is on *and* a
    /// key was present at startup. Checked once so the page can disable the
    /// button instead of offering a call that is certain to fail.
    available: bool,
}

impl DigestHandle {
    pub fn available(&self) -> bool {
        self.available
    }

    /// Ask for one day to be written. Facts are built by the storage actor on
    /// the UI side, so the worker never queries the database.
    pub fn generate(&self, facts: DigestFacts) {
        let _ = self.ctl.send(Ctl::Generate(Box::new(facts)));
    }
}

/// Spawn the digest worker. `wake` (a repaint request) fires after every
/// message so the UI polls promptly.
pub fn spawn(wake: impl Fn() + Send + 'static) -> (mpsc::Receiver<DigestMsg>, DigestHandle) {
    let (tx_res, rx_res) = mpsc::channel();
    let (tx_ctl, rx_ctl) = tokio_mpsc::unbounded_channel();

    // Resolve the credential once, on the UI thread, before the worker
    // starts: the page needs to know *now* whether to offer the button, and
    // a key that appears later still needs a restart to reach the process
    // environment anyway.
    let digester = match api::make() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("digest credentials: {e}");
            None
        }
    };
    let available = digester.is_some();

    std::thread::Builder::new()
        .name("digest".into())
        .spawn(move || {
            let Some(digester) = digester else {
                // Nothing to serve. Drop the receiver so a stray request
                // fails fast rather than hanging the page on a spinner.
                return;
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("digest tokio runtime: {e}");
                    return;
                }
            };
            runtime.block_on(worker(digester, tx_res, rx_ctl, wake));
        })
        .expect("spawn digest thread");

    (
        rx_res,
        DigestHandle {
            ctl: tx_ctl,
            available,
        },
    )
}

async fn worker(
    digester: api::GeminiDigester,
    tx: mpsc::Sender<DigestMsg>,
    mut rx_ctl: tokio_mpsc::UnboundedReceiver<Ctl>,
    wake: impl Fn(),
) {
    while let Some(Ctl::Generate(facts)) = rx_ctl.recv().await {
        let day = facts.day_utc;
        // One request at a time, in arrival order: this is a user-initiated,
        // once-per-day call against a quota, so there is nothing to gain from
        // concurrency and a rate limit to lose by it.
        let msg = match digester
            .generate(&facts, chrono::Utc::now().timestamp())
            .await
        {
            Ok(digest) => DigestMsg::Written(Box::new(digest)),
            Err(e) => DigestMsg::Failed {
                day,
                message: user_message(&e),
            },
        };
        if tx.send(msg).is_err() {
            return;
        }
        wake();
    }
}

/// Turn a `DigestError` into something worth showing on the page. The
/// `Display` text is already safe to show (never echoes the key); this only
/// adds what the user can *do* about the ones with an obvious next step.
fn user_message(error: &DigestError) -> String {
    match error {
        DigestError::MissingKey => unavailable_reason().to_owned(),
        DigestError::RateLimited { retry_after_secs } => match retry_after_secs {
            Some(s) => format!("Rate limited by the API — try again in {s}s."),
            None => "Rate limited by the API — try again shortly.".to_owned(),
        },
        DigestError::NoData(day) => {
            format!("No records stored for {day} — nothing to summarize.")
        }
        other => other.to_string(),
    }
}
