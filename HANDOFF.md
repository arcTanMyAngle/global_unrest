# Session handoff — Live Earth Signals

Last session: 2026-07-17. **M0–M5 code-complete.** M5 shipped ACLED
(feature `acled-live`, myACLED OAuth — **ACLED retired API keys**) and NOAA
active alerts (feature `noaa-live`, keyless, **live-verified**). Two loose
ends: (1) the ACLED *live* smoke run is blocked on working myACLED
credentials — the real OAuth endpoint answered `400 invalid_grant "user
credentials were incorrect"` for the pair in `.env`, so the user must
register/verify the account or fix the password; (2) M4's
`docker compose up` remains unverified locally (no docker CLI) — plan is to
close it with a CI compose smoke test. Next: the **professional-level
roadmap** (M6 public GitHub repo + CI live + releases; see the plan file
`~/.claude/plans/continue-to-m5-then-streamed-mochi.md`, Part B). Read this
file, then [CLAUDE.md](CLAUDE.md), then skim [docs/PLAN.md](docs/PLAN.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — git, **no remote yet**; user approved a **public GitHub repo** (M6) |
| Commits | Clean PR-sized commits through M5 (`git log --oneline`) |
| Tests | `cargo test --workspace` green; also `cargo test -p source-acled --features live` (mock server); clippy `-D warnings` clean across the feature matrix |
| Credentials | `.env` (gitignored) holds `ACLED_EMAIL`/`ACLED_PASSWORD` — currently **rejected by ACLED** (`invalid_grant`); `.env.example` is the committed template |
| Brief / plan | `../prompt_1.md`; [docs/PLAN.md](docs/PLAN.md) (M0–M5 ✅); roadmap Part B in the plan file above |

## Milestone 5 — what shipped (PR-sized commits)

1. **`source-acled` live adapter** — ACLED's 2025+ access model: OAuth
   password grant (`https://acleddata.com/oauth/token`, `client_id=acled`,
   `scope=authenticated`) → 24 h bearer + 14 d refresh, cached in memory,
   401 → one re-auth; windowed paged reads
   (`event_date=a|b`+`event_date_where=BETWEEN`, `limit`≤5000, `page` loop
   capped at `MAX_PAGES`); 429 → `SourceError::RateLimited` for the shared
   `sched::Backoff`. Pure `normalize_event` (always compiled): fixture-
   compatible kind/precision/severity mappings, numeric `iso` → alpha-3 via
   a full ISO 3166-1 table (`iso3.rs`; unknown codes → empty `country_iso`,
   e.g. Kosovo=0), **`notes` never read** (metadata-only + no
   redistribution). Mock-server tests cover auth/caching/re-auth/paging/429
   (`tests/live_mock.rs`, hand-rolled tokio TCP server).
2. **Wiring** — both ingest loops (desktop `ingest.rs`, `services/workers`)
   gained the source behind `acled-live` with a cfg-free stub when off
   (`make()` → `None`; `source-acled` not compiled). Own cadence: 12 h poll,
   14-day lookback, backoff capped at 1 h. Desktop status is now
   **per-source** (`SourceStatus.name`, upserted `Vec` in the app, aggregate
   `online` = any) with per-source attribution lines; missing creds show as
   "off — set ACLED_EMAIL / ACLED_PASSWORD".
3. **`source-noaa`** — keyless api.weather.gov active alerts (feature
   `noaa-live`, `LES_NOAA_ENDPOINT` override): `Disruption` at the polygon
   centroid, Admin1 precision (shading only), NWS severity scale → 0..1,
   `country_iso="USA"`, admin1 from the UGC state prefix. **Zone-scoped
   alerts (no polygon) yield zero events by design** — never guess
   coordinates. 10-min cadence via a shared generic `live_cycle` (ACLED uses
   it too; GDELT keeps its bespoke two-feed cycle). New
   `RawRecord::NoaaAlertJson` + `SourceId::Noaa` in core-types.
4. **Docs** — SAFETY licensing rows (ACLED OAuth/no-redistribution/
   corrections caveat; NOAA public-domain/US-only), README attribution,
   CLAUDE.md, PLAN.md status, DEVELOPMENT.md env table, this file.

### How M5 was verified

- Gates: fmt, clippy (`-D warnings`) on the default build **and** every
  feature combination of `acled-live`/`noaa-live` on both binaries, full
  workspace tests, plus `cargo test -p source-acled --features live`.
- **NOAA live** (real feed, 2026-07-17): worker with `noaa-live` fetched 612
  active alerts → 122 polygon events ingested, 0 failures, snapshot
  published (zone-only alerts correctly yielded nothing).
- **ACLED**: mock-server suite green; against the **real** endpoints the
  OAuth exchange is well-formed but the server rejects the credentials
  (`invalid_grant`) — rerun the recipe below once the account works.

```sh
# ACLED live smoke (once credentials are valid; bash syntax):
set -a; . ./.env; set +a
RUST_LOG=info LES_ONLINE=1 \
  LES_WORKER_DATA_DIR=<scratch>/data LES_PUBLISH_DIR=<scratch>/publish \
  LES_FIXTURES_DIR=./fixtures \
  cargo run -p workers --features acled-live,noaa-live
# expect: "acled token acquired", "acled fetched", "live cycle ingested origin=acled",
# snapshot published; then curl the api over that publish dir.
```

## Next up — professional-level roadmap (user-approved)

Part B of the plan file (`continue-to-m5-then-streamed-mochi.md`). Summary:

- **M6 — public repo + CI live**: `gh repo create` (public), branch
  protection; cargo-deny + Dependabot; **CI compose smoke test** (build both
  Docker images on ubuntu, worker fixtures-publish → api `/health` — closes
  the M4 docker gap without local Docker); GHCR images on tags; tag-driven
  release workflow (Win/Linux/macOS desktop binaries); CHANGELOG
  (0.5.0 at M5); portfolio README (screenshots via the run skill, diagram,
  badges); CONTRIBUTING.md. CI should also cover the M5 feature matrix.
- **M7 — service hardening**: axum middleware (timeouts, concurrency cap,
  per-IP rate limit, CORS, compression, trace layer, graceful shutdown),
  snapshot-version ETag, `/events` pagination, OpenAPI via utoipa,
  Prometheus `/metrics`, snapshot-age alerting in `/health`, integration
  suite over a committed fixture snapshot. **Never serve ACLED-bearing
  snapshots publicly** (SAFETY).
- **M8 — desktop polish + stretch**: walkers slippy-tile basemap (own
  design pass: Web-Mercator vs equirectangular + OSM tile policy), settings
  UI (creds stay env-only), About panel attributions, CelesTrak satellites
  (sgp4) as the thematic stretch, AIS (aisstream.io key) only if wanted,
  criterion benches in CI.

## Landmines and quirks (learned the hard way)

- **ACLED auth (M5)**: no API keys anymore — OAuth password grant with
  `client_id=acled`, `scope=authenticated`; refresh grant on expiry; the
  token endpoint's `error_description` is surfaced in errors (never the
  credentials). A `400 invalid_grant` means the account/password is wrong,
  not the request. ACLED **corrections reuse event ids** — dedup-by-id means
  revisions are not re-applied (accepted, documented).
- **NOAA alerts**: most alerts are zone-scoped with `geometry: null` —
  normalization returns `Ok(vec![])` for them (not an error, not a guess).
  US coverage only. api.weather.gov wants a descriptive User-Agent.
- **Feature stubs**: both binaries wrap ACLED/NOAA in tiny cfg modules
  (`make() -> Option<Source>`) so the select loops stay cfg-free. Clippy the
  matrix: default, `acled-live`, `noaa-live`, both.
- **reqwest has no `json` feature here** (lean rustls pin): use
  `.text()` + `serde_json::from_str`, like source-gdelt.
- **egui 0.35 API**: `App::ui(&mut self, ui, frame)`; unified
  `egui::Panel::top/bottom/right(id)`; menu close is `ui.close()`.
  eframe 0.35 rides **wgpu 29** — do not bump wgpu independently.
- **duckdb crate** `1.10504.0` = DuckDB 1.5.4. Connection `!Sync` — one
  thread (storage actor); the api opens throwaway in-memory conns inside
  `spawn_blocking`. No ALTER TABLE ADD non-null columns.
- **Single-writer rule (M4)**: worker owns its `.duckdb`; api reads only
  Parquet snapshots via the atomically-flipped `LATEST` pointer.
- **M3/M4 deps**: reqwest 0.12 rustls `default-features=false`; `zip` 6
  needs `deflate-flate2` + direct `flate2`; `governor` 0.10; axum 0.8 (api
  only); Docker builder needs `cmake`.
- **GDELT DOC has no per-article coordinates** — source-country precision
  only; FIPS≠ISO traps (AU/AS, CH/SZ, CI); Events keeps CAMEO roots 14–20.
- Desktop app data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`;
  worker uses `…-worker`. First cold build compiles DuckDB C++ (minutes).
- **GUI verification on this machine**: `.claude/skills/run/SKILL.md`;
  focus-stealing prevention applies — if another app keeps taking
  foreground, the user is at the machine; stop sending input.

## Quality gates (run after every step; CI runs the same)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p source-acled --features live   # M5 mock-server suite
```
