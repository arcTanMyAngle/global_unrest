# Live Earth Signals engineering guide

This file is a concise working guide for contributors and coding agents. Read
README.md for product onboarding, docs/ARCHITECTURE.md for the implemented
topology, and docs/SAFETY_AND_PRIVACY.md before changing a data boundary.

## Current state

M1-M7 and visualization V1-V3 are shipped. The desktop is live-data-only and
uses GDELT, ACLED, NOAA/NWS, IODA, Bluesky aggregate chatter, Telegram
aggregate chatter, an opt-in Daily Events digest, and an on-demand Media page.
Fixtures remain deterministic tests and the worker/service smoke input; the
desktop does not load them.

Daily Events is Google Gemini-backed. It is a separate page, not a source: the
user selects a UTC day and explicitly requests generation. A generated result
is cached locally and has two required sections, media attention and event
data. New generation requires GEMINI_API_KEY and the gemini-live feature; a
cached result remains readable without either.

The Media page is also separate from sources and storage. A person explicitly
chooses one place, optional topic, and bounded time window to find public
video via GDELT, Bluesky, and, when configured, Telegram. Results are kept
only in app memory until the next search or exit; public social posts are
labelled unverified.

## Non-negotiable product rules

1. Keep media attention, discrete events, official alerts, aggregate chatter,
   generated prose, and transient media research separate. Never present a
   combined result as truth.
2. Do not add person-level tracking, search, profiling, or covert location
   features. Preserve the aggregate-before-storage chatter boundary.
3. Do not fabricate precision. Only city/exact records render as points;
   country/admin records shade regions.
4. Store metadata rather than article bodies. Never store ACLED notes.
5. Keep credentials in the environment or local gitignored files. Never log
   keys, sessions, or raw secret-bearing requests.
6. Daily Events must remain opt-in, bounded, labelled, and two-sectioned.
   ACLED and Bluesky/Telegram row-level data must stay out of its third-party
   request path.
7. The Media exception is narrow: an explicit place-scoped, time-bounded
   video query may temporarily show a public URL, bounded label, and
   outlet/handle/channel attribution. It must not poll, bulk collect, follow
   accounts, expose Telegram senders, persist results, feed chatter rollups,
   or extract media streams from watch pages.
8. Do not make the API public over ACLED-bearing worker aggregates. The API
   excludes ACLED event rows by default, but aggregates require worker-side
   policy too.

## Quality gates

Run these after implementation changes:

~~~sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p source-acled --features live
cargo test -p source-noaa --features live
cargo test -p source-ioda --features live
cargo test -p source-bluesky --features live
cargo test -p daily-digest --features live
cargo test -p media-search --features live
cargo deny check
~~~

Feature-wiring changes also need no-default-features coverage:

~~~sh
cargo test -p global-signal-desktop -p workers --no-default-features --features "acled-live,noaa-live,ioda-live,bluesky-live,telegram-live,global-signal-desktop/gemini-live,global-signal-desktop/media-live,global-signal-desktop/video-embed"
~~~

The CI workflow runs each source feature separately, the desktop-only
gemini-live/media-live/video-embed features, their full union, ACLED/NOAA/
IODA/Bluesky/Gemini/Media mock suites, Compose smoke coverage, and workspace
gates. UI-visible
changes need a live desktop run when feasible. A real cargo build of the
desktop is useful after dependency/linking changes because check and clippy do
not perform the final link.

## Runtime model

- The UI thread owns egui state and polls asynchronous storage replies.
- The storage actor owns the sole DuckDB connection. Do not share that
  connection across threads or processes.
- The ingest worker has its own current-thread Tokio runtime and sends
  normalized batches to the UI. The UI hands batches to storage.
- The Digest worker calls Google Gemini only after an explicit request and
  never opens storage itself.
- The Media worker has no cadence. It handles one user-directed lookup at a
  time, returns session-only hits to the UI, and never opens storage.
- The desktop deletes legacy fixture rows on startup and treats an empty live
  database as valid.

The desktop source cadences are: GDELT 15 minutes, NOAA 10 minutes, IODA and
Telegram 15 minutes, ACLED 12 hours, and Bluesky completed aggregate windows
every 5 minutes. Media search is never scheduled; LES_ONLINE pauses scheduled
ingest, not a person clicking Search.

## Services

The worker owns a separate DuckDB database, loads fixture data at startup, and
publishes immutable versioned Parquet snapshots. The API opens an in-memory
DuckDB connection per request and reads only those snapshots. It never opens a
worker DuckDB file. Media hits and Daily Events cache rows are desktop-only
and never appear in a snapshot or API response.

Use Docker Compose for the normal service stack. The worker binary does not
load .env itself; manual worker/API runs need their relevant process
environment variables. The API requires the same LES_PUBLISH_DIR as the
worker and supports LES_API_BIND plus private-only LES_API_ALLOW_ACLED.

## Workspace map

| Package | Responsibility |
|---|---|
| core-types | Pure domain types, source contracts, and safe video/embed classification. |
| geo-utils | H3, projection, antimeridian, country/city indexes. |
| source-fixtures | Deterministic fixtures and generator. |
| source-gdelt, source-acled, source-noaa, source-ioda | Normalization and live adapters for their respective sources. |
| chatter, source-bluesky, source-telegram | Aggregate-only social-chatter ingest pipeline; Telegram also has the bounded, on-demand media leg. |
| media-search | Pure query/result types plus feature-gated GDELT/Bluesky public-video lookup. Not a SignalSource and has no storage. |
| analytics | Bucket scoring, baselines, spikes, divergence. |
| storage | DuckDB actor, migrations, queries, exports, snapshots. |
| daily-digest | Bounded day facts, schema, parsing, optional Google Gemini transport. |
| renderer | Cached egui layers, glyphs, alerts, graticule, labels. |
| global-signal-desktop | Desktop application/UI, including Daily Events and Media. |
| workers, api | Snapshot publisher and read-only HTTP API. |

## Dependency and implementation guardrails

- Pin shared versions in the workspace Cargo.toml; do not declare divergent
  member versions.
- Keep eframe/egui and wgpu in lockstep. eframe 0.36 uses wgpu 30.
- Preserve the pure-Rust/rustls dependency posture where it is intentional.
- The Telegram stack deliberately disables the default grammers session
  storage to avoid linking a second static SQLite implementation. Do not
  re-enable it without testing a real desktop link.
- The Windows player uses wry/WebView2 only for a provider's published embed
  or direct public media file. Non-Windows and unsupported URLs must keep an
  honest browser fallback. Two facts here were paid for in live debugging and
  must not be undone: the player page is served through a wry custom protocol
  so it has a real `http://lesplay.localhost` origin (navigating the webview
  straight at an embed gives it an opaque origin and YouTube refuses with
  "Error 153"), and Bluesky post pages get no embed at all, because
  `embed.bsky.app` is a post card whose play button links back to bsky.app
  rather than playing.
- Keep renderer work cached. Do not add per-frame geometry tessellation,
  unbounded overlay loops, or UI queries that block a frame.

## Model routing

`.claude/MODEL_ROUTING.md` records which model each kind of task in this repo
belongs to. Read it at the start of a work session: if the task you were given
does not match the model you are running, say so before starting and name the
model it belongs to.

## Documentation discipline

Update docs in the same change as code:

- README.md for user-visible behavior and setup.
- docs/DATA_MODEL.md for types, migrations, storage, or transient-media
  boundaries.
- docs/ARCHITECTURE.md for runtime/process ownership.
- docs/API.md for routes or middleware.
- docs/SAFETY_AND_PRIVACY.md for source terms, privacy, third-party
  processing, or the Media exception.
- docs/DEVELOPMENT.md and CONTRIBUTING.md for commands, features, CI, and
  environment variables.
- docs/ROADMAP.md for milestone status, open operational items, and planned
  direction.
- docs/ENGINEERING_NOTES.md for a build/tooling trap or source quirk that cost
  real debugging time.
- CHANGELOG.md for user-visible work under Unreleased.

Read docs/ENGINEERING_NOTES.md before a dependency, linking, or live-source
change; it records failures that repeat otherwise. Do not reintroduce a dated
session journal — CHANGELOG.md and `git log` carry history, ROADMAP.md carries
open items, and ENGINEERING_NOTES.md carries durable lessons.

## Agent skills

### Issue tracker

Issues live as GitHub issues in `arcTanMyAngle/global_unrest`, driven by the
`gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles use their default label strings (`needs-triage`,
`needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: one root `CONTEXT.md` plus `docs/adr/`, both created lazily.
See `docs/agents/domain.md`.
