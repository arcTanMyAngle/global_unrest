# Contributing

This started as a solo milestone-driven build (see
[docs/PLAN.md](docs/PLAN.md) and [HANDOFF.md](HANDOFF.md) for the full
history), but the workflow below applies to anyone sending a PR.

## Before you start

- Read [CLAUDE.md](CLAUDE.md) for the architecture map, hard project rules,
  and version gotchas — most of it applies regardless of who or what is
  writing the code.
- Check [docs/ROADMAP.md](docs/ROADMAP.md) and open issues so work doesn't
  collide with an in-flight milestone.
- Non-negotiable rules from the project brief (also in CLAUDE.md): public/
  authorized data sources only, no person-level tracking, metadata-only
  storage (never article bodies), media attention and event data always
  shown as separate components, credentials via env vars only.

## Workflow

1. One focused change per PR — small, reviewable, milestone- or
   issue-scoped. Match the existing PR-sized-commit style
   (`git log --oneline` shows the pattern: one commit per logical step).
2. Run the quality gates locally before pushing (CI runs the same checks
   plus the M5 feature matrix, the `docker compose` smoke test, and
   `cargo-deny`):

   ```sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo test -p source-acled --features live   # if you touched ingest/source code
   ```

   If you touched the desktop app or `services/workers`, also clippy/test
   the M5 feature combinations:

   ```sh
   cargo clippy -p global-signal-desktop -p workers --features acled-live,noaa-live --all-targets -- -D warnings
   cargo test -p global-signal-desktop -p workers --features acled-live,noaa-live
   ```
3. Regenerate fixtures only if you changed the fixture generator, and
   commit the regenerated output — fixtures are the permanent offline
   regression base (`cargo run -p source-fixtures --bin generate-fixtures`).
4. Update the relevant doc alongside the code: `docs/DATA_MODEL.md` for
   schema changes, `docs/SAFETY_AND_PRIVACY.md` for a new data source or
   licensing term, `docs/API.md` for API surface changes, `CHANGELOG.md`
   under `[Unreleased]` for anything user-visible.
5. New live data sources must be feature-gated (off by default, like
   `acled-live`/`noaa-live`), degrade gracefully offline, and add a
   licensing row to `docs/SAFETY_AND_PRIVACY.md` before the PR lands.
6. New visualizations: build on the app's own visual language — never
   copy a data provider's dashboard, charts, or branding (see
   `docs/VISUALIZATION.md`).

## Reporting issues

Bug reports and feature requests are welcome via GitHub Issues. For
anything touching a live data source's terms of use or a privacy concern,
say so explicitly in the issue — those get read against
`docs/SAFETY_AND_PRIVACY.md` first.

## License

By contributing, you agree your contributions are dual-licensed under MIT
OR Apache-2.0, matching the rest of the project (`LICENSE-MIT`,
`LICENSE-APACHE`).
