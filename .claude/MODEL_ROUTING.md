# Model routing

Which model to hand a task to in this repo, and why.

**This file cannot select a model.** The model is chosen before a session
starts, so by the time an agent reads this the choice is already made. It has
two jobs: it is the checklist a human consults when opening a session, and it
is a self-check for the agent — **if the task you were given does not match
the model you are running, say so before doing the work**, and name which
model it belongs to. A misroute is cheap to fix in the first message and
expensive to fix after a bad renderer migration is committed.

## The test

The question is not "is this hard?" — it is **"if this is done wrong, does
anything catch it?"**

This workspace has strong automated gates: 352 tests, `-D warnings` clippy,
per-source mock suites, a feature matrix in CI. Where those gates bite, a
cheaper model is fine, because a mistake surfaces immediately. Route to Opus
where the gates are structurally blind.

### Opus — the gates cannot catch a mistake here

- **Anything that compiles clean and fails at runtime.** egui/eframe and
  renderer work is the canonical case: UI code can build, pass every test,
  and draw nothing. Same for `cargo check`/`clippy` on dependency and linking
  changes, which do not link at all — see
  [../docs/ENGINEERING_NOTES.md](../docs/ENGINEERING_NOTES.md) "Build and
  linking".
- **Feature-flag and cfg-wiring changes.** A wrong flag produces a green
  build and a broken binary in a configuration CI happens not to cover.
- **Anything touching a non-negotiable product rule** (CLAUDE.md): the
  aggregate-before-storage chatter boundary, the ACLED exclusion, the Gemini
  row-level withholding, the point-vs-region precision contract. A wrong call
  here is a product-rule violation, not a bug — no test will call it wrong,
  and it is the kind of mistake that is embarrassing rather than annoying.
- **Cross-crate reasoning**: storage-actor threading, the snapshot contract
  between `workers` and `api`, ingest cadence and dedup interactions.
- **Work with no spec, where the design is the deliverable.**

### Sonnet — the work is specified and the gates will catch a slip

- A defect list that already names symptoms and fixes (the `release.yml`
  hardening was exactly this).
- Mechanical or repetitive edits against an established pattern in-repo.
- Documentation, CHANGELOG, workflow YAML, and config.
- Single-crate changes where an existing test would fail if it went wrong.
- Dependency bumps that are *not* migrations.

### Haiku

Fine for one-shot lookups — find a symbol, read a config value, summarize a
file. Not for edits.

## Worked examples

| Task | Model | Why |
|---|---|---|
| eframe/egui 0.35 → 0.36, wgpu 29 → 30 | Opus | Renderer migration; compiles clean and renders nothing. Needed a real link and a live run. |
| `release.yml` hardening (8 enumerated defects) | Sonnet | Every defect was named with its symptom. Bounded YAML editing. |
| CHANGELOG close-out, CODEOWNERS, dead link refs | Sonnet | Mechanical, verifiable by reading. |
| `zip` 6 → 8 in the GDELT dump path | Opus | Moves the reader API *and* the DEFLATE backend feature names — a wrong flag compiles and fails on real dumps. |
| `criterion` 0.7 → 0.8 | Sonnet | Harness migration confined to benches; `cargo bench --no-run` catches it. |
| Slippy-tile basemap (M8) | Opus | No spec; projection/provider/offline policy is the work. |
| Chatter segmentation for unsegmented scripts | Opus | Touches the chatter boundary and needs a real strategy, not a keyword list. |
| Adding a mock server for `source-telegram` | Opus to scope, Sonnet to fill in | Scoping an MTProto mock is design; writing tests against a settled shape is not. |

## Splitting a session

Prefer two sessions over one mixed session. When handing a cheaper model a
task that sits next to a migration-sized one, say so explicitly — *"skip the
X items, they are handled separately"* — or a capable model will notice the
adjacent work and start pulling on it.

Always end a work prompt with **"Don't commit or push without asking."**
Commits are done by hand here.
