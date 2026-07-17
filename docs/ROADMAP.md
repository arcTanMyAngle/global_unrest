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
| `docker compose up` | Written, never run locally (no docker CLI, user chose not to install). Close via the M6 CI compose smoke test. |

## M6 — Repo hygiene, CI depth, releases

Repo is live and public: `github.com/arcTanMyAngle/global_unrest` (pushed
2026-07-17, CI active on push).

1. CI matrix: add the M5 feature combinations (`acled-live`, `noaa-live`,
   both) to clippy/test jobs, plus `cargo test -p source-acled --features
   live` (mock suite).
2. **Compose smoke job** (ubuntu): build both Docker images, run the stack
   with `LES_ONLINE=0` (fixtures only), assert api `/health` → 200 with
   events > 0 — closes the M4 verification gap without local Docker.
3. Supply chain: `cargo-deny` (advisories + MIT/Apache-2.0-compatible
   license allowlist) as a CI job; Dependabot config (cargo + actions).
4. Releases: tag-driven workflow building desktop binaries for
   Windows/Linux/macOS attached to GitHub Releases; images to GHCR on tags;
   `CHANGELOG.md` (Keep-a-Changelog; 0.5.0 = M5); version bump policy tied
   to milestones.
5. Portfolio README: screenshots of the shipped views (run-skill recipe),
   architecture diagram, CI/license badges, quickstart, attribution +
   ethics section. `CONTRIBUTING.md`. Branch protection on `main`.

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
