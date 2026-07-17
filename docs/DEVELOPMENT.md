# Development

## Prerequisites

- Rust (pinned by `rust-toolchain.toml`; rustup installs it automatically).
- Windows: MSVC Build Tools (the bundled DuckDB C++ amalgamation compiles
  from source — the **first** build takes several minutes and is memory
  hungry; later builds hit the cache).
- Linux: a C/C++ toolchain (`build-essential`).
- No network is required to run the app — offline fixture mode is the
  default and permanent regression path.

## Common commands

```sh
# Run the desktop app (offline, fixtures)
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
| `LES_DATA_DIR` / `LES_FIXTURES_DIR` | Override the data dir / fixtures dir. |
| `LES_ONLINE` | `1`/`true` auto-starts live GDELT mode (headless verification/automation). |
| `LES_RETENTION_DAYS` | Events retention cap in days (overrides the saved setting; `0`/unset = keep everything). |
| `LES_GDELT_DOC_ENDPOINT` / `LES_GDELT_EVENTS_URL` | Point the live loop at a local/mock server (testing; reproduces the network-down path). |
| `ACLED_EMAIL` / `ACLED_PASSWORD` | myACLED OAuth credentials (M5, feature `acled-live`; ACLED retired API keys). Never committed — shell or gitignored `.env` only; see `.env.example`. |
| `LES_ACLED_TOKEN_URL` / `LES_ACLED_ENDPOINT` | Point the ACLED adapter at a local/mock server (testing). |
| `LES_NOAA_ENDPOINT` | Point the NOAA alerts adapter at a local/mock server (testing). |

## Where data lives

- Analytics DuckDB + settings SQLite: the per-user data dir
  (`%APPDATA%\live-earth-signals` on Windows, XDG dirs on Linux).
- Delete that directory to reset; the app re-ingests fixtures on next start.

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
