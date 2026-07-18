# Roadmap — from M5 to a professional release

M0–M5 of [PLAN.md](PLAN.md) are complete (statuses there). This document is
the forward plan, user-approved 2026-07-16. The visualization work has its
own detailed design doc: **[VISUALIZATION.md](VISUALIZATION.md)** — that is
the next session's focus (batch V1), with the M6 hygiene items free to
interleave.

## Standing loose ends

| | Status |
|---|---|
| ACLED live data | **Fully live-verified 2026-07-17** (institutional account): 17,560 real events → normalize (0 failures) → snapshot → api. The account is date-restricted to events older than 12 months, so use `LES_ACLED_WINDOW` for ingest windows. |
| `docker compose up` | **Closed 2026-07-18** — CI's `compose-smoke` job builds both images and runs the stack with `LES_ONLINE=0`, asserting `/health` reports events > 0. Still never run interactively on the dev machine (no local docker CLI), but the stack is now exercised on every push/PR. |
| Branch protection on `main` | **Not done** — `gh` is installed but not authenticated on this machine, so it can't be scripted here. Manual step (once, via GitHub → repo Settings → Branches, or `gh auth login` then `gh api repos/arcTanMyAngle/global_unrest/branches/main/protection -X PUT ...`): require the `check` and `feature-matrix` jobs to pass before merge. |

## M6 — Repo hygiene, CI depth, releases ✅ (2026-07-18, except branch protection — see above)

Repo is live and public: `github.com/arcTanMyAngle/global_unrest` (pushed
2026-07-17, CI active on push).

1. ✅ CI matrix: `feature-matrix` job covers the M5 feature combinations
   (`acled-live`, `noaa-live`, both) on the desktop app + worker binary;
   `acled-live-mock` job runs `cargo test -p source-acled --features live`.
2. ✅ **Compose smoke job** (ubuntu): builds both Docker images, runs the
   stack with `LES_ONLINE=0` (fixtures only), asserts `/health` → 200 with
   `snapshot.events > 0` — closes the M4 verification gap without local
   Docker. `docker-compose.yml`'s `LES_ONLINE` is now shell-overridable
   (`${LES_ONLINE:-1}`) to make this possible.
3. ✅ Supply chain: `cargo-deny` job (`deny.toml`) — advisories (with two
   documented, justified `ignore`s: quick-xml's DoS-class CVEs are
   build-time-only via `wayland-scanner`'s bundled protocol XML, never
   attacker input; `ttf-parser` is unmaintained with no upstream fix,
   reached only through the Linux Wayland clipboard's font fallback) +
   license allowlist (added `BSL-1.0`/`OFL-1.1`/`Ubuntu-font-1.0`/
   `CDLA-Permissive-2.0` after running the tool for real). Dependabot
   config (`.github/dependabot.yml`, cargo + actions, weekly, `wgpu`
   excluded since it's locked to `eframe`).
4. ✅ Releases: `.github/workflows/release.yml`, tag-driven (`v*`) — desktop
   binaries for Windows/Linux/macOS attached to GitHub Releases, worker/api
   images to GHCR. `CHANGELOG.md` (Keep-a-Changelog; retroactive 0.1.0–0.5.0
   per milestone, 0.6.0 = this M6 work). Version bump policy: workspace
   version is milestone-tied (`0.<milestone>.0`, all crates `publish =
   false` — none of this is meant for crates.io); bumped to 0.6.0.
5. ✅ Portfolio README: a real screenshot (offline fixture mode, `assets/
   screenshots/map-overview.png`, captured via the run skill), a mermaid
   architecture diagram, CI/license/rust-version badges, an "Ethics & data
   policy" section summarizing SAFETY_AND_PRIVACY.md. `CONTRIBUTING.md`.
   ⏳ Branch protection on `main` — manual, see the loose-ends table above.

## V1–V3 — Visualization batches

See [VISUALIZATION.md](VISUALIZATION.md). Summary: V1 timeline histogram +
spike halos + severity markers + recency fade; V2 attention↔unrest
divergence layer, top-movers panel, region sparkline + event ledger; V3
per-source layer identity + legend, basemap orientation polish, "how to
read this map" overlay. Honest-visualization principles and perf guardrails
are defined there and are binding.

## M7 — Service hardening (api/worker)

- axum middleware: request timeout, concurrency cap, per-IP rate limit
  (`tower-governor`), CORS, compression, `tower-http` tracing; graceful
  shutdown.
- Snapshot version as `ETag`/`If-None-Match` on all endpoints; `/events`
  pagination.
- OpenAPI via `utoipa` served at `/openapi.json` (docs/API.md becomes
  machine-checked).
- Prometheus `/metrics`; snapshot-age staleness surfaced in `/health`.
- Integration suite against a committed fixture snapshot.
- **Policy:** ACLED-bearing snapshots are never served publicly
  (SAFETY_AND_PRIVACY.md); any hosted demo runs GDELT + fixtures only.

## M8 — Stretch layers & desktop platform polish

- walkers 0.56 slippy-tile basemap: its own design pass first —
  Web-Mercator vs equirectangular projection decision, OSM tile-policy row
  in SAFETY, online-only and clearly toggled.
- CelesTrak satellites layer (keyless; `sgp4` propagation; a moving-point
  layer class) — the literal "what's overhead" stretch.
- AIS ship positions (aisstream.io websocket, free key) only if wanted —
  high-volume streaming needs its own thinning design.
- Settings UI (sources on/off + status; credentials stay env-only, never in
  the settings DB), About panel with full attributions.
- criterion benches wired into CI as regression checks; profiling pass
  toward 1M-event retention.

## Standing quality bar (unchanged, every session)

`cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D
warnings` (plus the feature matrix when ingest/source code changes) ·
`cargo test --workspace` · `cargo test -p source-acled --features live` ·
PR-sized commits · fixtures stay the permanent offline regression base ·
HANDOFF.md updated at session end.
