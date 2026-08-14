# Contributing

Live Earth Signals is a milestone-driven Rust project. Contributions should be
small, reviewable, and grounded in the project's evidence, privacy, and
precision rules.

## Before you start

- Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
  [docs/SAFETY_AND_PRIVACY.md](docs/SAFETY_AND_PRIVACY.md).
- Check [docs/ROADMAP.md](docs/ROADMAP.md) and open issues before starting
  work that may overlap another change.
- Keep these non-negotiable constraints intact: use public or authorized
  sources only; do not add person-level tracking; store metadata rather than
  article bodies; preserve attention/event separation; and keep credentials
  outside the repository.

## Workflow

1. Keep one focused concern per pull request. Avoid mixing source behavior,
   rendering redesign, and documentation overhaul in a single change unless
   they are inseparable.
2. Run the workspace gates before pushing:

   ~~~sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo deny check
   ~~~

3. Run the focused mock suite when changing a credentialed network path:

   ~~~sh
   cargo test -p source-acled --features live
   cargo test -p daily-digest --features live
   ~~~

4. When changing desktop or worker feature wiring, use no-default-features
   coverage in addition to the normal desktop build:

   ~~~sh
   cargo test -p global-signal-desktop -p workers --no-default-features --features "acled-live,noaa-live,ioda-live,bluesky-live,telegram-live,global-signal-desktop/anthropic-live"
   ~~~

   CI also runs each source feature separately. Keep
   .github/workflows/ci.yml in sync if the feature surface changes.

5. Regenerate fixtures only when changing the fixture generator, and commit
   the deterministic result:

   ~~~sh
   cargo run -p source-fixtures --bin generate-fixtures
   ~~~

## Documentation requirements

Update the relevant documentation in the same pull request:

| Change | Update |
|---|---|
| User-visible capability or setup | README.md and CHANGELOG.md under Unreleased |
| Domain type, migration, or local cache | docs/DATA_MODEL.md |
| Source, license, privacy boundary, or third-party processing | docs/SAFETY_AND_PRIVACY.md |
| HTTP route, response, or API behavior | docs/API.md |
| Runtime topology, crate ownership, or handoff | docs/ARCHITECTURE.md |
| Commands, environment variables, CI, or Docker | docs/DEVELOPMENT.md |
| Visualization encoding or performance contract | docs/VISUALIZATION.md |

The historical PLAN.md does not replace current documentation. Preserve it as
a dated planning record; update README, ROADMAP, and the implementation docs
for current behavior.

## Source and safety rules

- New live-source code should be feature-gated at the crate/worker level and
  degrade clearly when credentials or connectivity are unavailable. The
  desktop's default feature set is intentional and must be updated
  consciously, with matching README and CI changes.
- Add attribution, licensing, retention, privacy, and precision decisions to
  SAFETY_AND_PRIVACY.md before adding a source.
- A country/admin-only source must shade a region, never render as a guessed
  point.
- Social sources must aggregate before storage. Do not introduce post text,
  author identity, message identifiers, or URLs into stored rows, logs, or
  APIs.
- Daily Events changes require review of its two-section schema and
  third-party processing boundary. Do not turn generated prose into an event,
  a severity score, a forecast, or a map caption.

## Reporting issues

Report bugs and feature requests through GitHub Issues. Explicitly flag any
question involving live-source terms, privacy, exact location, or public
hosting so it can be reviewed against the safety policy.

## License

By contributing, you agree that your contribution is dual-licensed under MIT
or Apache-2.0, matching LICENSE-MIT and LICENSE-APACHE.
