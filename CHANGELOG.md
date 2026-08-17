# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
milestone-tied (`0.<milestone>.0`) per [docs/ROADMAP.md](docs/ROADMAP.md),
not strict [SemVer](https://semver.org/) — this is a portfolio/research
project with no published crate API to stabilize against.

## [Unreleased]

### Changed

- Upgraded eframe/egui 0.35 → 0.36.1, which moves wgpu 29 → 30 transitively
  along with egui's text stack (harfrust, skrifa, glifo). No workspace source
  changes were needed; `Context::egui_wants_keyboard_input`, `Painter::galley`
  and the cached-galley country labels all carried over unchanged. Verified by
  a real desktop link and a live run, not just `cargo check` — see
  `docs/ENGINEERING_NOTES.md`.
- Dependabot no longer bundles migrations with routine patches. Cargo updates
  are split into `egui-stack` (eframe/egui/epaint/wgpu, version-locked and
  migration-sized), `archive` (zip/flate2), `benchmarks` (criterion), and a
  `cargo-dependencies` catch-all; GitHub Actions updates are split into
  `release-actions` (GHCR push, artifact up/download, provenance, release
  body — none of which a pull request's own CI exercises) and `ci-actions`.
  The inert `ignore: wgpu` entry is gone: grouping wgpu with eframe enforces
  the lockstep rule that the ignore only approximated.
- Documentation consolidation. `HANDOFF.md` and `docs/PLAN.md` are removed;
  their durable content moved to a new `docs/ENGINEERING_NOTES.md` (build and
  linking traps, source behavior that looks like a bug, verification
  discipline) and to `docs/ROADMAP.md` (milestone record, standing risks, and
  a new "Open operational items" section covering release-workflow defects,
  branch protection, and pending dependency upgrades). The quality-gate
  command list is now canonical in `docs/DEVELOPMENT.md`; README and
  CONTRIBUTING link to it instead of restating a drifted copy.
- `workspace.package.version` bumped from 0.6.0 to 0.7.0.

### Fixed

- `release.yml`: added a `validate-tag` job that rejects a non-semver `v*`
  tag and checks it against `workspace.package.version`, a matching
  CHANGELOG heading, and ancestry from `main`; fixed the release body to
  reference the real unprefixed image tags; pointed the CHANGELOG link at
  the tagged ref instead of moving `main`; SHA-pinned every third-party
  action; added a top-level read-only `permissions: contents: read` with
  jobs escalating only what they use; made `ghcr-images` wait on
  `desktop-binaries` so GHCR publishing can't outrun a failing platform
  build; and added a `.sha256` checksum plus a build provenance attestation
  per desktop archive, with `provenance: true` on the GHCR image builds.

- Media player: a Bluesky post's own page (not `embed.bsky.app`) is now the
  hit URL and never claimed as an in-app embed. `embed.bsky.app` renders a
  post card whose play button is a link back to `bsky.app` rather than a
  player, so the previous mapping opened a dead click inside the webview;
  affected native-video hits now open in the OS browser, where playback
  works.

### Added

- Daily Events: a separate, opt-in desktop page that writes a model-generated
  digest for a selected UTC day only after an explicit user action. Digests
  have separate media-attention and event-data sections, display their record
  counts/model/generation time, and cache locally by day.
- daily-digest: bounded fact construction and an optional Google Gemini
  generateContent transport, plus a local mock-server suite. ACLED and
  Bluesky/Telegram row-level data are withheld from third-party processing;
  only permitted aggregate counts reach the digest path.
- Storage migration 0003 for the local daily_digest cache.
- Chatter coverage widening: country-name aliases, selected unambiguous
  demonyms, and small city-exonym mappings now resolve only through bundled
  geography; the topic table also covers additional hazard, violence,
  displacement, health, and crime terms. The output remains aggregate
  place/topic/window counts, with no raw social content stored.
- Media page: an explicit, place-scoped public-video lookup with optional
  topic and 24-hour, 3-day, 7-day, or 30-day windows. Results are temporary,
  split between news video and unverified public posts, and never enter map
  ingest, DuckDB, Parquet snapshots, the API, a cache, or Daily Events.
- media-search: feature-gated, on-demand GDELT and Bluesky video lookup,
  with the configured Telegram allowlist as a read-only third leg. A Windows
  WebView2 player can use supported providers' published embeds; every
  platform and unsupported link retains a browser fallback.
- M7 service hardening: request middleware, snapshot ETags and conditional
  GET, events pagination, OpenAPI, Prometheus metrics, health staleness, and
  committed integration snapshot coverage.
- V2/V3 map work: attention-vs-unrest divergence, top movers, regional
  sparklines, paged event ledger, source-shaped markers, NOAA alert overlay,
  legend, graticule/country labels, focus dimming, reading guide, now-follow,
  and typed UTC ranges.

- `source-bluesky`: a new optional live source (feature `bluesky-live`,
  keyless, desktop default) for the Bluesky Jetstream firehose — the first
  **streaming** source in the workspace. It publishes **aggregate chatter
  volume only**: counts of posts mentioning both a known place and a known
  topic in a 5-minute window, as a media-attention signal alongside GDELT's
  article counts. The ingest path stores and logs no post text, author
  identity, post id, or URL, and exposes none in normalized source data
  (docs/SAFETY_AND_PRIVACY.md hard rule 6). A local mock-Jetstream-server
  suite (`tests/live_mock.rs`) drives the real `spawn_stream`/`fetch` path
  over a WebSocket, covering scanned/matched counts, malformed-frame
  tolerance, and the completed-vs-pending window boundary — no real network
  or keys needed.
- `chatter`: the shared aggregate-before-storage machinery both Bluesky and
  Telegram use — gazetteer place matching, a
  fixed topic keyword table, an in-memory accumulator, and `ChatterRollup`
  normalization. Requires a place *and* a topic in the same post; a
  pre-widening live sample matched 0.27% of 5,918 posts.
- `source-telegram`: a new optional live source (feature `telegram-live`,
  credential-gated, desktop default) for public Telegram channels — the
  second aggregate-chatter source. MTProto (`grammers-client`, pure Rust) is
  the only mechanism that can read a third-party public channel's history
  without that channel's owner cooperating; poll-based (not streamed) like
  NOAA/IODA, sweeping a small live-verified curated allowlist of 11 channels
  every 15 minutes. Login is a one-time interactive step
  (`examples/login_setup.rs`) that saves a local session file; the source
  itself only ever opens it. Its ingest path has the same **aggregate chatter
  volume only** guarantee as Bluesky; the separate, explicit Media lookup is
  the documented transient public-video exception.
- `geo-utils`: bundles Natural Earth's 1:110m populated-places gazetteer
  (243 major cities) behind a new `CityIndex`, plus
  `CountryIndex::iter_with_centroid`.

- The region inspector now exposes real source URLs for the selected area,
  identifies direct/known-host video candidates, and offers an opt-in external
  YouTube search using the area and event context. Nothing is fetched or
  opened until the user clicks, and search results are labeled unverified.
- V1 visualization batch (docs/VISUALIZATION.md): a timeline histogram strip
  (stacked per-kind bars, an attention-count line on its own scale, a
  draggable playhead) replaces the bare time-window slider; pulsing spike
  halos on cells whose spike score clears a named threshold; marker size now
  interpolates with severity when a source provides one; marker opacity
  fades with age during playback (full detail while paused). Also added a
  "has video" marker filter, sharing a new `core_types::is_video_url`
  classifier with the region inspector's existing source-link list.
- `source-ioda`: a new optional live source (feature `ioda-live`, keyless,
  desktop default) for IODA (Internet Outage Detection and Analysis,
  Georgia Tech) — near-real-time internet-outage events, country precision,
  severity log-scaled from IODA's unbounded anomaly score. Country
  centroids resolve via a new `geo_utils::CountryIndex::centroid_by_iso_a2`
  (real Natural Earth geometry, never a hand-typed coordinate table).

### Changed

- The desktop default feature set now includes ACLED, NOAA, IODA, Bluesky,
  Telegram, Gemini-backed Daily Events, on-demand media search, and the
  Windows video-embed path. Credentials still gate ACLED, Telegram, and
  digest generation at runtime.

- Desktop runtime is live-data-only: live polling defaults on, ACLED/NOAA are
  default features, `.env` is loaded automatically, and synthetic fixtures are
  retained solely as test assets.
- Startup removes legacy `source=fixtures` rows and rebuilds derived buckets
  while preserving live records; an empty database now waits for live data.
- Source status distinguishes partial GDELT success, abbreviates request
  errors, reports last-ingest inserts/duplicates accurately, resets fixture-era
  theme filters, and shows unavailable confidence as N/A.
- End-user documentation now distinguishes shipped live-source capabilities
  from the planned voluntary on-scene channel system, documents source refresh
  cadence and evidence classes, and removes the stale synthetic-default claim.
- Safety policy now explicitly supports consent-based field publishing while
  requiring publisher-controlled location, delay, provenance, corroboration,
  and correction states; it continues to prohibit covert tracking and
  targeting.

## [0.6.0] — 2026-07-18 — M6: repo hygiene, CI depth, releases

### Added

- CI: feature-matrix job covering `acled-live`/`noaa-live`/both on the
  desktop app and worker binary, plus a dedicated job for
  `source-acled`'s mock-OAuth-server suite (`--features live`).
- CI: `docker compose` smoke-test job — builds both service images, runs
  the stack fixtures-only (`LES_ONLINE=0`), and asserts `/health` reports
  a published snapshot with events > 0. Closes the M4 verification gap
  (no local Docker on the dev machine).
- CI: `cargo-deny` job (security advisories + license allowlist).
- Dependabot config for the `cargo` and `github-actions` ecosystems
  (weekly, grouped; `wgpu` excluded from automated bumps since it's
  version-locked to `eframe`).
- Tag-driven release workflow: desktop binaries for Windows/Linux/macOS
  attached to GitHub Releases, worker/api images pushed to GHCR.
- `CHANGELOG.md`, `CONTRIBUTING.md`.
- Portfolio README: badges, architecture diagram, ethics/attribution
  section.

### Changed

- `docker-compose.yml`: the worker's `LES_ONLINE` is now shell-overridable
  (`${LES_ONLINE:-1}`) so CI can force fixtures-only mode without editing
  the compose file.

## [0.5.0] — 2026-07-16 — M5: ACLED + NOAA live sources

### Added

- `source-acled`: live ACLED adapter behind the `acled-live` feature —
  myACLED OAuth password/refresh grant (API keys retired in 2025), paged
  windowed reads, pure `normalize_event` with a full ISO-3166 numeric →
  alpha-3 table, `LES_ACLED_WINDOW` override for date-restricted accounts.
  Never stores ACLED `notes` (no redistribution of raw data).
- `source-noaa`: live NOAA/NWS active-alerts adapter behind the
  `noaa-live` feature (keyless) — polygon alerts become `Disruption`
  events at the polygon centroid; zone-only alerts (no geometry) yield
  zero events by design.
- Both ingest loops (desktop, `services/workers`) wired to the new
  sources; desktop status indicator became per-source.
- Live-verified end-to-end: NOAA against the real feed (612 alerts → 122
  events); ACLED against a mock OAuth server plus 17,560 real events via
  an authorized institutional account.

## [0.4.0] — 2026-07-16 — M4: Dockerized services

### Added

- `storage`: versioned Parquet snapshot publish (atomic `LATEST` pointer).
- `services/workers`: ingest worker binary — owns its own DuckDB, ingests
  fixtures + live GDELT, publishes snapshots every cycle.
- `services/api`: read-only axum API over published Parquet
  (`/health`, `/meta`, `/buckets`, `/events`) — never opens a `.duckdb`
  file (DuckDB is single-writer-per-file; Parquet is the only handoff).
- `docker-compose.yml` + per-service Dockerfiles.

## [0.3.0] — 2026-07-14 — M3: live GDELT ingestion

### Added

- `source-gdelt`: DOC 2.0 JSON attention client, Events 2.0 CSV-zip dump
  path, country/FIPS → ISO-A3 resolution, rate limiting + backoff +
  fetch-cadence scheduling.
- Desktop live mode: online toggle, incremental ingest loop, retention
  cap, per-source status indicator with graceful degradation (cached data
  shown on network loss).

## [0.2.0] — 2026-07-14 — M2: transparent scoring

### Added

- `analytics`: score components (attention/unrest/spike-vs-baseline),
  28-day trailing-median baselines with cold-start badges.
- Inspector: per-component score bars (never a bare combined number),
  theme filters, source-diversity heat metric, heatmap rollup at world
  zoom.
- Parquet session export (the layout M4's snapshot handoff later reused).
- criterion scoring benches.

## [0.1.0] — 2026-07-13 — M1: offline fixture pipeline

### Added

- Cargo workspace scaffold (`core-types`, `geo-utils`, `source-fixtures`,
  `analytics`, `storage`, `renderer`), CI, dual MIT/Apache-2.0 licensing.
- Deterministic 35-day synthetic fixture generator.
- DuckDB storage actor thread (the connection is `!Sync`).
- eframe desktop shell: cached-mesh basemap/heatmap/marker layers, time
  slider, region inspector, E2E pipeline test.

[Unreleased]: https://github.com/arcTanMyAngle/global_unrest/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/arcTanMyAngle/global_unrest/releases/tag/v0.6.0
[0.5.0]: https://github.com/arcTanMyAngle/global_unrest/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/arcTanMyAngle/global_unrest/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/arcTanMyAngle/global_unrest/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/arcTanMyAngle/global_unrest/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/arcTanMyAngle/global_unrest/releases/tag/v0.1.0
