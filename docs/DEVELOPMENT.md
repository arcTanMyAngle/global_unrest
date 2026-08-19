# Development

## Prerequisites

- Rust is pinned by rust-toolchain.toml; rustup installs the selected toolchain.
- On Windows, install MSVC Build Tools. Bundled DuckDB compiles from source,
  so the first build is CPU- and memory-intensive.
- On Linux, install a C/C++ toolchain such as build-essential.
- The desktop is live-data-only. It needs network access to ingest records;
  committed fixtures are a headless regression harness and a service smoke
  fixture, not a desktop fallback.

## Run the desktop

Copy .env.example to .env if you need credentialed sources or Daily Events,
then run:

~~~sh
cargo run -p global-signal-desktop
~~~

The desktop enables GDELT, ACLED, NOAA, IODA, Bluesky, Telegram, the Daily
Events Gemini path, on-demand media search, and the Windows player path by
default. Credentials still control whether ACLED, Telegram, and Gemini
generation are available. GDELT, NOAA, IODA, Bluesky, and the keyless media
search legs are available without credentials.

Live updates begin automatically. Set LES_ONLINE=0 or LES_ONLINE=false to
start with scheduled ingest polling paused; cached real data remains visible.
An explicit Media-page search is still user-directed and can run. The desktop
never loads synthetic fixtures.

## Common commands

~~~sh
# Format, lint, and test the workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Live source/transport paths against local mock servers
cargo test -p source-acled --features live
cargo test -p source-noaa --features live
cargo test -p source-ioda --features live
cargo test -p source-bluesky --features live
cargo test -p daily-digest --features live
cargo test -p media-search --features live

# Deterministic fixture maintenance
cargo run -p source-fixtures --bin generate-fixtures

# Dependency advisories and licenses
cargo deny check
~~~

These are the canonical quality gates; every change must keep them green.
Other documents link here rather than restating the list.

Feature-wiring changes also need no-default-features coverage:

~~~sh
cargo test -p global-signal-desktop -p workers --no-default-features --features "acled-live,noaa-live,ioda-live,bluesky-live,telegram-live,global-signal-desktop/gemini-live,global-signal-desktop/media-live,global-signal-desktop/video-embed"
~~~

`cargo check` and `cargo clippy` do not link. After a dependency or linking
change, build the real desktop binary — see
[ENGINEERING_NOTES.md](ENGINEERING_NOTES.md#build-and-linking).

CI also runs the `analytics` criterion benches as a compile-and-smoke gate,
not a performance gate — there is no stable perf baseline or GPU on the
runner, so it fails only if the bench harness stops compiling or a bench
panics, never on timing:

~~~sh
cargo bench -p analytics -- --quick
~~~

`crates/analytics/Cargo.toml` sets `[lib] bench = false` so this
package-level form works; running `--bench scoring` directly hits the lib's
own empty libtest bench target first, which rejects criterion's `--quick`
flag.

## Retention profiling harness

`apps/global-signal-desktop/tests/retention_profile.rs` is the two-axis
timing harness behind the M8 retention work. Like `chatter::observe_cost` it
is `#[ignore]`d — it is a measurement, not a gate, and CI never times it:

~~~sh
cargo test -p global-signal-desktop --release --test retention_profile -- --ignored --nocapture
~~~

Release mode is required; debug numbers are noise. It reports two axes:

- **fixture-generator day axis** — every `events_*.json` under `fixtures/`
  and `fixtures/generated/`, run through the real ingest path. Generate the
  multiples first, since only the 35-day file is committed:
  `cargo run --release -p source-fixtures --bin generate_fixtures -- --out <dir>/events_350d.json --days 350`.
- **online-rate axis** — synthesized events at a realistic online volume
  spread over ~1,500 res-3 cells, which the fixture generator's 23 fixed
  spots cannot exercise.

| Variable | Purpose |
|---|---|
| LES_PROFILE_FIXTURES | Directory of `events_*.json` for the fixture axis. Defaults to `fixtures/`. |
| LES_PROFILE_DAYS | Comma-separated day counts for the online axis. Default `1,2,4,10`. |
| LES_PROFILE_PER_DAY | Events per day for the online axis. Default `100000`. |

The `empty` column is the one to watch when changing ingest: it is a tick
that brings nothing new, so whatever it costs is paid on every cadence tick
regardless of batch size.

## Desktop environment variables

The desktop loads .env during startup. Process environment variables take
precedence; credentials and session files must never be committed.

| Variable | Purpose |
|---|---|
| RUST_LOG | Tracing filter, for example global_signal_desktop=debug. |
| WGPU_BACKEND | Override the wgpu backend (dx12, vulkan, or gl) when a driver misbehaves. |
| LES_DATA_DIR | Override the desktop data directory. |
| LES_ONLINE | Live updates default on; 0, false, or no starts with polling paused. |
| LES_RETENTION_DAYS | Events retention cap in days, enforced to the UTC day (a lower bound, see DATA_MODEL.md). 0 or unset keeps all retained records. |
| LES_GDELT_DOC_ENDPOINT / LES_GDELT_EVENTS_URL | Point scheduled GDELT ingest at a local/mock endpoint. They do not configure Media search. |
| ACLED_EMAIL / ACLED_PASSWORD | Authorized myACLED OAuth credentials. ACLED no longer uses API keys. |
| LES_ACLED_TOKEN_URL / LES_ACLED_ENDPOINT | Local/mock ACLED OAuth or data endpoint. |
| LES_ACLED_WINDOW | Inclusive fixed window, YYYY-MM-DD\|YYYY-MM-DD, for date-restricted ACLED accounts. |
| LES_NOAA_ENDPOINT | Local/mock NOAA alerts endpoint. |
| LES_IODA_ENDPOINT | Local/mock IODA endpoint. IODA is keyless. |
| LES_BLUESKY_ENDPOINT | Pin scheduled Bluesky Jetstream ingest to a local/mock or chosen endpoint. It does not configure Media search. |
| LES_BLUESKY_WINDOW_SECS | Aggregate chatter window in seconds; default 300. Only completed windows are stored. |
| TELEGRAM_API_ID / TELEGRAM_API_HASH | MTProto app credentials for a dedicated account. The hash is used only by the interactive session setup tool. |
| LES_TELEGRAM_SESSION_FILE | Local JSON session generated by the Telegram login setup example. Treat it as a credential. |
| GEMINI_API_KEY | Enables explicit Daily Events generation. No request is made until the user clicks Generate digest. |
| LES_GEMINI_ENDPOINT | Local/mock Google Generative Language API endpoint override. |

Create a Telegram session once before enabling the source:

~~~sh
cargo run -p source-telegram --features live --example login_setup
~~~

## Media research and playback

The Media page is enabled by the media-live feature and makes no background
requests. A person selects a place, optional topic, and one of the bounded
time windows; the app then looks for public video through GDELT, Bluesky, and
the configured Telegram allowlist. Results are held only in the current UI
session and are never written to DuckDB, logs, or a cache.

The Windows-only video-embed feature uses WebView2 through wry to render a
provider's published embed page in the app. Linux/macOS and builds without the
feature retain the result list and browser fallback but report honestly that
they cannot embed playback. Do not extract stream URLs from a watch page.

## Local data

- Analytics records and the Daily Events cache are stored in the per-user data
  directory, for example
  %LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data on Windows.
- The storage actor owns the DuckDB connection. The settings database is a
  separate local SQLite file.
- On startup, the desktop removes legacy fixture rows and rebuilds derived
  aggregates before rendering live data.
- **A database written before the signal-family migration is upgraded in
  place on first open.** Migration `0004_signal_families` reclassifies every
  stored record, so the first launch after it rebuilds all derived rows and
  drops any cached Daily Events prose, which then has to be regenerated. That
  is a one-time cost per database and is expected, not a fault. There is no
  downgrade path: an older build will not read a v4 store.

### Spike evidence fixtures

`crates/source-gdelt/tests/data/spike-a2/` holds raw upstream captures backing
the A2 finding in [GDELT_GEO_GKG.md](GDELT_GEO_GKG.md). They are **evidence,
not test inputs**: no test reads them, nothing in the workspace depends on
them, and they are deliberately unmodified — response headers, tab delimiters
and all. Do not reformat them, and do not delete them as unused; their whole
value is being byte-for-byte what the service returned on 2026-08-19. That
directory's `README.md` carries per-file provenance and checksums.

## Services

The service stack is implemented and runs separately from the desktop:

~~~sh
docker compose up
~~~

Compose starts a worker that owns its DuckDB database and publishes Parquet
snapshots, then an API on http://localhost:8080 that reads only those
snapshots. The Compose worker starts with fixture data and GDELT; the optional
worker source features are opt-in at build time. For a no-network smoke run,
set LES_ONLINE=0 in the shell before invoking Compose.

The worker binary does not load .env itself. For a manual service run, supply
credentials and feature flags through the process/container environment; set
LES_PUBLISH_DIR to the same directory for workers and api, and mount it
read-only in api. The API also honors LES_API_BIND and, for a private
authorized deployment only, LES_API_ALLOW_ACLED=1. Read
[API.md](API.md) before enabling the latter.

## Feature coverage

CI tests each source feature by itself, the complete source union, and the
desktop-only gemini-live, media-live, and video-embed features. When changing
feature wiring, mirror the workflow's no-default-features posture:

~~~sh
cargo test -p global-signal-desktop -p workers --no-default-features --features "acled-live,noaa-live,ioda-live,bluesky-live,telegram-live,global-signal-desktop/gemini-live,global-signal-desktop/media-live,global-signal-desktop/video-embed"
~~~

The exact per-feature matrix and mock suites live in .github/workflows/ci.yml.

## Dependency and build policy

- Shared dependency versions are pinned once in the workspace Cargo.toml.
  Member crates use workspace dependencies.
- eframe/egui and wgpu move in lockstep. eframe 0.36 uses wgpu 30; do not
  bump wgpu independently.
- reqwest uses rustls, and the GDELT ZIP path uses the pure-Rust miniz_oxide
  backend, keeping CI free of OpenSSL and system zlib requirements.
- The development profile optimizes dependencies to keep map rendering and
  geospatial math responsive while workspace crates retain fast incremental
  builds. If cold builds are painful, use sccache through RUSTC_WRAPPER.
- Dependabot keeps migrations out of routine-patch PRs
  (`.github/dependabot.yml`). Cargo updates arrive as `egui-stack`
  (eframe/egui/epaint/wgpu, which move together), `archive` (zip/flate2),
  `benchmarks` (criterion), and a `cargo-dependencies` catch-all; Actions
  updates as `release-actions` and `ci-actions`. Add a new group rather than
  widening the catch-all whenever an upgrade would need its own review — and
  for an `egui-stack` PR, merging requires a real desktop build plus a live
  run, because clippy accepts a renderer that draws nothing (see
  [ENGINEERING_NOTES.md](ENGINEERING_NOTES.md#build-and-linking)).
