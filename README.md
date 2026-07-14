# Live Earth Signals

A desktop-first, Rust-based geospatial dashboard that visualizes global
news-attention and unrest/event signals over time. Civic-data research and
visualization only: public or properly authorized sources, aggregate-level
signals, transparent (non-ML) scoring, and a hard separation between "media
attention" and "verified event data."

**Milestones 1–2 complete: runs 100% offline** from committed synthetic
fixtures — no network, no API keys. Live GDELT ingestion is M3.

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

## Commands

```sh
cargo test --workspace                          # all tests (headless)
cargo run -p source-fixtures --bin generate-fixtures   # regenerate fixtures
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
```

## Documentation

| Doc | Contents |
|---|---|
| [HANDOFF.md](HANDOFF.md) | Session handoff: current status, M2 task list, known quirks |
| [docs/PLAN.md](docs/PLAN.md) | The approved project plan, with milestone status |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate map, threading model, rendering strategy, single-writer rule |
| [docs/DATA_MODEL.md](docs/DATA_MODEL.md) | `GeoTemporalEvent`, buckets, DuckDB schema, fixtures |
| [docs/SCORING.md](docs/SCORING.md) | Transparent scoring formulas, baseline/spike design (M2) |
| [docs/SAFETY_AND_PRIVACY.md](docs/SAFETY_AND_PRIVACY.md) | Hard rules, licensing, biases, retention |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Setup, env vars, build notes |

## Roadmap

- **M1 ✅** offline fixture pipeline: ingest → DuckDB → map/timeline/inspector
- **M2 ✅** scoring depth: score components, 28-day median baselines, spike
  detection with cold-start badges, theme filters, Parquet export
- **M3** live GDELT ingestion (DOC API + 15-min dumps), optional OSM tile layer
- **M4** Dockerized services (axum API + ingest worker, Parquet handoff)
- **M5** ACLED adapter (authorized access only), optional NOAA/AIS/CelesTrak layers

## Data & attribution

- All current data is **synthetic** fixture data; outlet names use reserved
  `.example` domains and headlines are tagged `[synthetic]`.
- Basemap: [Natural Earth](https://www.naturalearthdata.com/) 1:110m
  countries (public domain).
- GDELT (M3) is used with attribution per its terms; ACLED (M5) only with
  registered authorization.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
