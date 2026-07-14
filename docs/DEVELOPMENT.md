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
| `ACLED_API_KEY` | M5 only; never committed. Lives in your shell or a `.env` (gitignored). |

## Where data lives

- Analytics DuckDB + settings SQLite: the per-user data dir
  (`%APPDATA%\live-earth-signals` on Windows, XDG dirs on Linux).
- Delete that directory to reset; the app re-ingests fixtures on next start.

## Dependency policy

- All shared dependency versions are pinned **once** in the workspace root
  `Cargo.toml`. Member crates say `dep.workspace = true`.
- eframe/egui and wgpu move in lockstep (eframe 0.35 = wgpu 29). egui
  upgrades happen in one dedicated PR, never as a side effect.
- `source-gdelt` (reqwest etc.) stays dependency-light until M3 so M1 builds
  fast.

## Build performance notes

- `[profile.dev.package."*"] opt-level = 2` keeps epaint/geo math fast in dev
  while workspace crates compile incrementally.
- If cold builds hurt, install `sccache` and set `RUSTC_WRAPPER=sccache`.

## Docker (M4+)

Backend services (`services/api`, `services/workers`) are stubs until M4;
`docker/` gains its compose file then. Docker on Windows means WSL2. The
desktop app is always a native binary, never containerized.
