# Development

## Prerequisites

- Rust (pinned by `rust-toolchain.toml`; rustup installs it automatically).
- Windows: MSVC Build Tools (the bundled DuckDB C++ amalgamation compiles
  from source — the **first** build takes several minutes and is memory
  hungry; later builds hit the cache).
- Linux: a C/C++ toolchain (`build-essential`).
- The desktop is live-data-only and needs network access to ingest data.
  Committed fixtures remain a headless regression harness only.

## Common commands

```sh
# Run the live-only desktop (GDELT + NOAA + IODA, and ACLED when credentialed)
cargo run -p global-signal-desktop

# Regenerate synthetic fixtures (deterministic; commit the result)
cargo run -p source-fixtures --bin generate-fixtures

# Quality gates (run after every change; CI runs the same three)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Environment variables

| Variable | Purpose |
|---|---|
| `RUST_LOG` | tracing filter, e.g. `RUST_LOG=global_signal_desktop=debug`. |
| `WGPU_BACKEND` | Override the wgpu backend (`dx12`, `vulkan`, `gl`) if a driver misbehaves. |
| `LES_DATA_DIR` | Override the desktop data directory. |
| `LES_ONLINE` | Live updates default on; `0`/`false` starts with network polling paused. |
| `LES_RETENTION_DAYS` | Events retention cap in days (overrides the saved setting; `0`/unset = keep everything). |
| `LES_GDELT_DOC_ENDPOINT` / `LES_GDELT_EVENTS_URL` | Point the live loop at a local/mock server (testing; reproduces the network-down path). |
| `ACLED_EMAIL` / `ACLED_PASSWORD` | myACLED OAuth credentials (M5, feature `acled-live`; ACLED retired API keys). Never committed — shell or gitignored `.env` only; see `.env.example`. |
| `LES_ACLED_TOKEN_URL` / `LES_ACLED_ENDPOINT` | Point the ACLED adapter at a local/mock server (testing). |
| `LES_ACLED_WINDOW` | Fixed `YYYY-MM-DD\|YYYY-MM-DD` fetch window (inclusive) replacing the rolling 14-day lookback — for date-restricted ACLED tiers (e.g. accounts limited to events older than 12 months). |
| `LES_NOAA_ENDPOINT` | Point the NOAA alerts adapter at a local/mock server (testing). |
| `LES_IODA_ENDPOINT` | Point the IODA outage-events adapter at a local/mock server (testing). IODA itself is keyless — no credential env vars needed. |

## Where data lives

- Analytics DuckDB + settings SQLite: the per-user local data dir (for
  example `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data` on Windows).
- On startup, the desktop removes any legacy rows attributed to `fixtures`
  and rebuilds its aggregates before showing data.

## Dependency policy

- All shared dependency versions are pinned **once** in the workspace root
  `Cargo.toml`. Member crates say `dep.workspace = true`.
- eframe/egui and wgpu move in lockstep (eframe 0.35 = wgpu 29). egui
  upgrades happen in one dedicated PR, never as a side effect.
- `source-gdelt` (M3) uses **reqwest with rustls** (no OpenSSL/native-tls, so
  CI stays clean on Windows + Linux), `zip`/`flate2` with the pure-Rust
  miniz_oxide backend for the Events dumps, and `governor` for rate limiting.

## Build performance notes

- `[profile.dev.package."*"] opt-level = 2` keeps epaint/geo math fast in dev
  while workspace crates compile incrementally.
- If cold builds hurt, install `sccache` and set `RUSTC_WRAPPER=sccache`.

## Docker (M4+)

Backend services (`services/api`, `services/workers`) are stubs until M4;
`docker/` gains its compose file then. Docker on Windows means WSL2. The
desktop app is always a native binary, never containerized.
