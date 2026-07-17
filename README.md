# Live Earth Signals

A desktop-first, Rust-based geospatial dashboard that visualizes global
news-attention and unrest/event signals over time. Civic-data research and
visualization only: public or properly authorized sources, aggregate-level
signals, transparent (non-ML) scoring, and a hard separation between "media
attention" and "verified event data."

**Milestones 1–5 complete.** Runs 100% offline from committed synthetic
fixtures by default (no network, no credentials). Optional live sources:
**GDELT** (M3, keyless), **ACLED** (M5, feature-gated, authorized myACLED
account) and **NOAA/NWS active alerts** (M5, feature-gated, keyless) — all
rate-limited, attributed, and degrading gracefully offline.

## Quickstart

```sh
# From this directory (first build compiles bundled DuckDB — several minutes)
cargo run -p global-signal-desktop
```

You get a dark world map with:

- **Heatmap** — H3 cells shaded by media attention, event count, or source
  diversity (log scale; toggle in the top bar). Cells roll up to coarser H3
  parents at world zoom.
- **Event markers** — protests/conflicts/disruptions as colored diamonds.
  Only city/exact-precision records render as points; country/admin
  centroids shade regions instead of faking hotspots.
- **Time slider** — replay 35 days of data in 6-hour buckets (▶ loops).
- **Region inspector** — click anywhere: counts by kind, attention vs.
  events (always separate), **score components as separate bars**
  (attention / unrest / spike-vs-baseline / combined, per
  [docs/SCORING.md](docs/SCORING.md)), low-confidence badges (baseline cold
  start, coarse geocoding), top themes, outlet diversity, headline metadata.
- **Filters** — event kinds, themes (vocabulary from the data), minimum
  location confidence, layer toggles.
- **Parquet export** — one click writes the session as date-partitioned
  Parquet (the M4 service handoff layout).
- **Ingest log** — malformed records are logged and surfaced, never
  silently dropped.

Pan by dragging, zoom with the scroll wheel, `reset view` in the top bar.

### Live mode

Tick **live** in the top bar to add live data on top of the fixtures
(fixtures always remain the offline base). GDELT is always available when
live (keyless): the DOC 2.0 API (media attention, geocoded to source
country) plus the 15-minute Events dumps (discrete CAMEO events),
rate-limited and politely backed off. `↻` forces an immediate fetch; the
inspector's **Live source** panels show per-source state and, if the
network drops, a *degraded — showing cached data* badge (last-known data
stays on screen). Cap the events table with the **retention** menu
(≥ 30 days keeps the 28-day baselines warm). Env knobs: `LES_ONLINE=1`
(auto-start live), `LES_RETENTION_DAYS`.

### M5 live sources (opt-in cargo features)

```sh
# NOAA/NWS active weather alerts (keyless, US coverage):
cargo run -p global-signal-desktop --features noaa-live

# ACLED (requires a Research-tier myACLED account; see below) + NOAA:
#   put credentials in .env (copy .env.example), load them into the shell,
#   then:
cargo run -p global-signal-desktop --features acled-live,noaa-live
```

ACLED credentials are `ACLED_EMAIL` / `ACLED_PASSWORD` env vars (OAuth —
ACLED retired API keys). Note: ACLED grants **API** access only to
Research/Partner/Enterprise-tier myACLED accounts (institutional email);
Open-tier accounts authenticate but receive `403 Access denied` on data
reads. Without credentials the ACLED status line simply reports itself off.

The M4 services take the same features: `cargo run -p workers --features
acled-live,noaa-live` ingests live and publishes Parquet snapshots that
`cargo run -p api` serves (see [docs/API.md](docs/API.md)).

## Commands

```sh
cargo test --workspace                          # all tests (headless)
cargo run -p source-fixtures --bin generate-fixtures   # regenerate fixtures
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
```

## Documentation

| Doc | Contents |
|---|---|
| [HANDOFF.md](HANDOFF.md) | Session handoff: current status, next task list, known quirks |
| [docs/PLAN.md](docs/PLAN.md) | The approved project plan, with milestone status |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Forward plan: M6 repo/CI/releases, M7 service hardening, M8 stretch layers |
| [docs/VISUALIZATION.md](docs/VISUALIZATION.md) | Visualization design plan (V1–V3): timeline, anomaly halos, divergence view, ledger |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate map, threading model, rendering strategy, single-writer rule |
| [docs/DATA_MODEL.md](docs/DATA_MODEL.md) | `GeoTemporalEvent`, buckets, DuckDB schema, fixtures |
| [docs/SCORING.md](docs/SCORING.md) | Transparent scoring formulas, baseline/spike design (M2) |
| [docs/SAFETY_AND_PRIVACY.md](docs/SAFETY_AND_PRIVACY.md) | Hard rules, licensing, biases, retention |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Setup, env vars, build notes |

## Roadmap

- **M1 ✅** offline fixture pipeline: ingest → DuckDB → map/timeline/inspector
- **M2 ✅** scoring depth: score components, 28-day median baselines, spike
  detection with cold-start badges, theme filters, Parquet export
- **M3 ✅** live GDELT ingestion (DOC 2.0 API + 15-min Events dumps),
  rate-limited fetch loop, retention, dedup, graceful degradation. Optional
  OSM slippy-tile layer deferred (stretch)
- **M4 ✅** Dockerized services (axum API + ingest worker, Parquet handoff)
- **M5 ✅** ACLED adapter (feature `acled-live`, authorized OAuth access only)
  and NOAA/NWS active-alerts layer (feature `noaa-live`, keyless). AIS /
  CelesTrak remain backlog stretch layers.

## Data & attribution

- Offline (default): all data is **synthetic** fixture data; outlet names use
  reserved `.example` domains and headlines are tagged `[synthetic]`.
- Live mode: data is from the **[GDELT Project](https://www.gdeltproject.org/)**,
  used **with attribution** per its terms (keyless, no redistribution of raw
  dumps). GDELT DOC attention is geocoded only to the *source country* and is
  always shown at country precision — an imperfect, coverage-biased proxy.
- Basemap: [Natural Earth](https://www.naturalearthdata.com/) 1:110m
  countries (public domain).
- ACLED (feature `acled-live`): data from the **Armed Conflict Location &
  Event Data Project (ACLED)**, [acleddata.com](https://acleddata.com) —
  authorized access only (free myACLED account; OAuth credentials via
  `ACLED_EMAIL`/`ACLED_PASSWORD` env vars). Used with attribution; raw ACLED
  data (including event narratives) is never stored or redistributed.
- NOAA (feature `noaa-live`): **NOAA/NWS active weather alerts**
  ([api.weather.gov](https://www.weather.gov/documentation/services-web-api)),
  US-government public domain; US coverage only.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
