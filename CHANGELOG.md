# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
milestone-tied (`0.<milestone>.0`) per [docs/ROADMAP.md](docs/ROADMAP.md),
not strict [SemVer](https://semver.org/) — this is a portfolio/research
project with no published crate API to stabilize against.

## [Unreleased]

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
