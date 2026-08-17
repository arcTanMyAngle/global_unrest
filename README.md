# Live Earth Signals

### See the signal. Question the story. Stay connected to the world.

[![CI](https://github.com/arcTanMyAngle/global_unrest/actions/workflows/ci.yml/badge.svg)](https://github.com/arcTanMyAngle/global_unrest/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.96](https://img.shields.io/badge/rust-1.96-orange.svg)](rust-toolchain.toml)

Live Earth Signals is a desktop-first Rust map for inspecting public-interest
signals without treating attention as truth. It keeps media attention,
structured event data, official alerts, and aggregate chatter visibly
separate; preserves provenance; and avoids person-level tracking.

The current build includes the completed M1-M7 work and visualization batches
V1-V3. The desktop ingests live GDELT, NOAA/NWS, IODA, Bluesky aggregate
chatter, optional ACLED, and optional Telegram aggregate chatter. Synthetic
fixtures are never loaded by the desktop; they remain regression data and the
fixtures-only service smoke-test path.

## Read the evidence, not a single score

- **Media attention** says that outlets or public feeds are covering
  something. It does not confirm the underlying claim.
- **Provider event data** is a normalized report from GDELT Events or an
  authorized provider such as ACLED. It can be corrected and is not
  infallible ground truth.
- **Official alerts** are notices from the issuing agency, within that
  agency's coverage and update cycle.
- **Aggregate chatter** is a count of public posts or channel messages that
  matched a place and topic. The app stores no post text, author identity,
  message identifier, or message URL.

This is a situational-awareness aid, not a substitute for official emergency
instructions or a promise that an area is safe.

## Quickstart

1. Copy [.env.example](.env.example) to .env. Credentials are optional for
   the keyless map sources.
2. Run the desktop:

   ~~~sh
   cargo run -p global-signal-desktop
   ~~~

The first build compiles bundled DuckDB and can take several minutes. Live
updates start by default; set LES_ONLINE=0 to start with network polling
paused. An empty database waits for live records rather than falling back to
synthetic data.

## What is available today

| Capability | Status | What it does |
|---|---|---|
| Map and analysis | Available | Heat modes for attention, events, source diversity, and attention-vs-unrest divergence; a six-hour timeline with replay, now-follow, and typed UTC ranges. |
| Evidence inspection | Available | Per-region score components, source links and video candidates, source-shaped markers, legend, top movers, sparklines, and a paged event ledger. |
| NOAA alert layer | Available | A US NWS weather-alert overlay separated visually from unrest signals, with graticule, country labels, and an in-app reading guide. |
| GDELT | Live | Global news metadata and CAMEO event records. Coverage is not confirmation. |
| ACLED | Live with authorized credentials | Curated conflict and civic-event records. Account access and available dates vary by tier. |
| NOAA/NWS | Live | Active US and territory alerts with usable polygon geometry. |
| IODA | Live | Country-precision internet-outage severity. It shades regions and never creates a point marker. |
| Bluesky | Live, aggregate-only | Keyless Jetstream chatter counts in completed five-minute windows. |
| Telegram | Live with setup | Aggregate-only public-channel chatter from a curated allowlist, using a local MTProto session. |
| Daily Events | Opt-in | A model-written, two-section digest for one UTC day of stored data. |
| Media research and playback | On demand | Public video lookup for one selected place and time window; results are transient and can play in-app on Windows or open in a browser. |
| Services API | Available | Dockerized worker/API snapshots with conditional GET, pagination, OpenAPI, metrics, and health staleness reporting. |
| On-scene publishing | Planned | Consent-based field channels with publisher safety controls and explicit evidence states. |

### Source cadence and limits

| Source | Normal cadence | Important limit |
|---|---:|---|
| GDELT | 15 minutes | Upstream DOC and Events feeds can be partial, delayed, or unavailable. |
| NOAA/NWS | 10 minutes | US and territories only; zone-only alerts are not mapped. |
| IODA | 15 minutes | Country precision only; never rendered as a point. |
| ACLED | 12 hours | Requires authorized credentials; licensing and account date restrictions apply. |
| Bluesky | 5 minutes | A continuous stream is published only as completed aggregate windows. |
| Telegram | 15 minutes | Requires app credentials and a pre-created local session; reads only the curated public-channel allowlist. |

The app keeps source event time, ingest time, and six-hour analysis buckets
separate. A frequent fetch does not mean every underlying report is current or
independently confirmed.

## Optional credentials, Daily Events, and media research

ACLED uses ACLED_EMAIL and ACLED_PASSWORD from an authorized myACLED account.
Some accounts are date-restricted; use
LES_ACLED_WINDOW=YYYY-MM-DD|YYYY-MM-DD for a fixed inclusive historical
window.

Telegram requires a dedicated account's TELEGRAM_API_ID and
TELEGRAM_API_HASH, then a one-time local session setup:

~~~sh
cargo run -p source-telegram --features live --example login_setup
~~~

To create a Daily Events digest, set GEMINI_API_KEY in .env or the process
environment, open **daily events**, choose a UTC day with stored data, and
click **generate digest**. Nothing is generated automatically. A digest is
cached locally per day and can be regenerated explicitly.

The page always keeps **media attention** and **event data** in separate
sections with their record counts, model, and generation time. A bounded set
of aggregate counts and permitted record metadata is sent to Google Gemini for a
requested digest; ACLED and Bluesky/Telegram row-level data are withheld and
remain counts-only. Read the exact boundary in
[Safety and privacy](docs/SAFETY_AND_PRIVACY.md#third-party-processing-google-gemini-api).

The **media** page is a separate, user-directed research action. Pick a place,
an optional topic, and a bounded time window; only then does the app search
GDELT, public Bluesky posts, and the configured Telegram allowlist for video.
Nothing is fetched on a timer or written to the database. News videos and
unverified public posts are labelled separately. On Windows, supported
provider embeds can play inside the app; every result retains a browser
fallback.

## Services API

For the worker/API stack, Docker Compose is the normal entry point:

~~~sh
docker compose up
~~~

Once the worker has published a snapshot, the API is available at
http://localhost:8080. See [docs/API.md](docs/API.md) for /health, /meta,
/buckets, /events, /metrics, and /openapi.json.

The worker owns its DuckDB database and publishes immutable Parquet snapshots.
The API reads only those snapshots; it never opens the worker database. The
worker loads fixtures at startup for its service/test path, then can ingest
GDELT and any enabled live-source features. Do not expose ACLED-bearing
snapshots publicly.

## Architecture

~~~mermaid
flowchart LR
    subgraph Sources["Live runtime sources"]
        GDELT["GDELT"]
        ACLED["ACLED (authorized)"]
        NOAA["NOAA/NWS"]
        IODA["IODA"]
        BSKY["Bluesky aggregate chatter"]
        TG["Telegram aggregate chatter"]
    end

    subgraph Desktop["Desktop app"]
        INGEST["live ingest"]
        STORE[("DuckDB storage actor")]
        MAP["map, timeline, inspector"]
        FACTS["daily facts"]
        DIGEST["Daily Events page"]
    end

    Sources --> INGEST --> STORE --> MAP
    STORE --> FACTS
    FACTS -->|"explicit Generate click"| GEMINI["Google Gemini API"]
    GEMINI --> DIGEST
    DIGEST -->|"local cache"| STORE

    QUERY["place + topic + time window"]
    QUERY --> SEARCH["on-demand media search"]
    SEARCH --> MEDIA["media page + player"]

    FIX["Fixtures: tests and service smoke only"]
    FIX -.-> WORKER
    subgraph Services["Worker and read-only API"]
        WORKER["workers: DuckDB + snapshot publisher"]
        SNAP[("immutable Parquet snapshots")]
        API["api: Axum + read_parquet"]
    end
    WORKER --> SNAP --> API
~~~

The full crate map, threading model, data boundaries, and service handoff are
in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Commands and CI

~~~sh
# Formatting, linting, and headless tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Fixture maintenance and service stack
cargo run -p source-fixtures --bin generate-fixtures
docker compose up
~~~

The complete gate list — including the per-source mock suites, cargo-deny, and
no-default-features coverage — is in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#common-commands).

CI runs the workspace gates on Windows and Linux; source-feature checks for
ACLED, NOAA, IODA, Bluesky, Telegram, and the desktop-only gemini-live,
media-live, and video-embed features; the full feature union; Daily Events and
media-search mock suites; Docker Compose smoke coverage; and cargo-deny.
Tag-driven releases build desktop binaries and publish worker/API images.

## Documentation

| Doc | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Runtime topology, crate map, threading, and snapshot handoff. |
| [docs/API.md](docs/API.md) | Services API, snapshot contract, middleware, and endpoints. |
| [docs/DATA_MODEL.md](docs/DATA_MODEL.md) | Domain types, DuckDB schema, Daily Events cache, transient media research, and fixtures. |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Setup, environment variables, test commands, and service operations. |
| [docs/SCORING.md](docs/SCORING.md) | Transparent scoring and baseline design. |
| [docs/VISUALIZATION.md](docs/VISUALIZATION.md) | Shipped V1-V3 visualization decisions and guardrails. |
| [docs/SAFETY_AND_PRIVACY.md](docs/SAFETY_AND_PRIVACY.md) | Hard safety rules, licensing, bias, retention, and Daily Events/media boundaries. |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Milestone record, open operational items, and the remaining M8/M9 direction. |
| [docs/ENGINEERING_NOTES.md](docs/ENGINEERING_NOTES.md) | Build/linking traps, source behavior that looks like a bug, and verification discipline. |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Contribution workflow and verification requirements. |
| [CHANGELOG.md](CHANGELOG.md) | Milestone-tied release history. |

## Status and direction

Completed: M1-M7, visualization V1-V3, and the IODA, Bluesky, Telegram,
Daily Events, and on-demand media-research layers.

Next: remaining M8 platform/source polish and safety-gated M9 voluntary
on-scene publishing. See [docs/ROADMAP.md](docs/ROADMAP.md) for the current
plan.

## Safety, data, and attribution

- The project is not a covert-surveillance or targeting system. A contributor
  choosing to publish is supported; locating, profiling, or following a
  person without consent is prohibited.
- Media attention and structured events remain separate components. Aggregate
  chatter is never treated as an on-the-ground observation. The Media page is
  a narrow exception for user-requested, transient public video research; it
  does not alter the aggregate ingest/storage boundary.
- The desktop stores source metadata, not article bodies. ACLED narratives are
  not stored or redistributed; ACLED data must not be publicly served.
- GDELT is used with attribution. Natural Earth country data is public domain.
  NOAA/NWS is US government public-domain data. IODA, Bluesky, and Telegram
  use the constraints documented in [Safety and privacy](docs/SAFETY_AND_PRIVACY.md).
- Daily Events is labelled generated text, not a news report. Its output helps
  readers inspect stored signals; it does not assess importance, attribute
  cause, or forecast events.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
