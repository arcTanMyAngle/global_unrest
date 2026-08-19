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
2. Run the quality gates before pushing. The canonical list — workspace
   format/lint/test, the per-source and Daily Events/media mock suites,
   cargo-deny, and no-default-features feature-wiring coverage — is in
   [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#common-commands). Run the mock
   suite for any credentialed or on-demand network path you touched, and the
   no-default-features leg for any feature-wiring change.

3. CI runs each source feature separately, the three desktop-only features,
   and the complete union. Keep .github/workflows/ci.yml in sync if the
   feature surface changes.

4. Regenerate fixtures only when changing the fixture generator, and commit
   the deterministic result:

   ~~~sh
   cargo run -p source-fixtures --bin generate-fixtures
   ~~~

## Documentation requirements

Update the relevant documentation in the same pull request:

| Change | Update |
|---|---|
| User-visible capability or setup | README.md and CHANGELOG.md under Unreleased |
| Domain type, migration, local cache, or transient media data | docs/DATA_MODEL.md |
| Signal family, volume unit, location role, or score/digest/layer membership | docs/SIGNAL_MODEL.md (read it *before* changing one) |
| Source, license, privacy boundary, third-party processing, or media-query scope | docs/SAFETY_AND_PRIVACY.md |
| HTTP route, response, or API behavior | docs/API.md |
| Runtime topology, crate ownership, or handoff | docs/ARCHITECTURE.md |
| Commands, environment variables, CI, or Docker | docs/DEVELOPMENT.md |
| Visualization encoding or performance contract | docs/VISUALIZATION.md |
| Milestone status, open operational item, or planned direction | docs/ROADMAP.md |
| A build/tooling trap or source quirk that cost real debugging time | docs/ENGINEERING_NOTES.md |

## Source and safety rules

- New live-source code should be feature-gated at the crate/worker level and
  degrade clearly when credentials or connectivity are unavailable. The
  desktop's default feature set is intentional and must be updated
  consciously, with matching README and CI changes.
- Add attribution, licensing, retention, privacy, and precision decisions to
  SAFETY_AND_PRIVACY.md before adding a source.
- A country/admin-only source must shade a region, never render as a guessed
  point.
- Social-source ingestion must aggregate before storage. Do not introduce post
  text, author identity, message identifiers, or URLs into stored rows, logs,
  or APIs.
- The Media page is the documented exception for a user-directed,
  place-scoped, time-bounded public-video lookup. Do not widen its result
  fields, provider access, retention, or playback behavior without updating
  the safety review and tests; never turn it into a background collector.
- Daily Events changes require review of its two-section schema and
  third-party processing boundary. Do not turn generated prose into an event,
  a severity score, a forecast, or a map caption.

## Reporting issues

Report bugs and feature requests through GitHub Issues. Explicitly flag any
question involving live-source terms, privacy, exact location, public hosting,
or media lookup scope so it can be reviewed against the safety policy.

## License

By contributing, you agree that your contribution is dual-licensed under MIT
or Apache-2.0, matching LICENSE-MIT and LICENSE-APACHE.
