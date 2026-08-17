# M8 session prompts

Copy one block per session. Routing follows [MODEL_ROUTING.md](MODEL_ROUTING.md);
the model is named in each heading because it must be selected **before** the
session opens. Run them in the order given — S1 and S2 are independent, S4
depends on S3, S6 closes out whatever landed.

| # | Work | Model | Depends on |
|---|---|---|---|
| S1 | Attribution + source-state inventory (data only, no UI) | Sonnet | — |
| S2 | Benches in CI | Sonnet | — |
| S3 | Slippy-tile basemap **design doc** | Opus | — |
| S4 | Settings and About UI | Opus | S1 |
| S5 | Chatter segmentation for unsegmented scripts | Opus | — |
| S6 | Profiling pass and retention increase | Opus | S2 |
| S7 | M8 close-out: ROADMAP, CHANGELOG, README | Sonnet | all |

Deliberately **not** in M8: CelesTrak satellites and AIS. ROADMAP gates both
behind a thinning/precision/disclosure design that does not exist yet, and
neither is required to call M8 done. Do not let a session start pulling on
them.

---

## S1 — Attribution and source-state inventory (Sonnet)

> Build the data layer for the Settings/About screen. **Data only — write no
> UI in this session**; the egui panel is a separate Opus session and will
> consume what you produce.
>
> Every source already carries its terms, licence, and attribution
> requirements in prose across `README.md`, `docs/SAFETY_AND_PRIVACY.md`, and
> the individual `crates/source-*` module docs. Nothing collects them.
> Produce a single in-repo table that does.
>
> Add to `crates/core-types` a `SourceAttribution` struct and a `const` (or
> `fn`) table covering every `SourceId` variant plus the non-source
> third-party legs (Google Gemini for Daily Events, and the Media page's
> GDELT/Bluesky/Telegram legs). Per entry, at minimum: display name,
> upstream/homepage URL, licence or terms label, the exact attribution string
> the UI must show verbatim if one is required, whether credentials are
> needed, and which env vars configure it. `SourceId` is already exhaustive,
> so make the table exhaustive over it and add a test that fails if a new
> variant is added without an entry.
>
> Rules that constrain this:
> - Credential **values** never appear in this table, only variable *names*.
>   Product rule 5 — credentials live in the environment, never in the
>   settings database, never in a log line.
> - Where a source's terms require verbatim attribution text, copy it
>   verbatim and comment where it came from. Do not paraphrase a licence.
> - Cite each entry's source in a comment (`README` section,
>   SAFETY_AND_PRIVACY heading, or the crate doc) so the next reader can
>   check it without re-researching.
>
> Also add a plain accessor for whether a source is *configured* (env vars
> present) as distinct from *enabled at compile time* (feature flag) — the UI
> needs to tell "you didn't set the key" apart from "this build can't do it".
> Read how `TelegramSource::from_env` and the ACLED adapter already
> distinguish those two and follow the same shape rather than inventing a
> third.
>
> Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
> -- -D warnings`, `cargo test --workspace` (baseline 364 passing). Docs:
> `docs/DATA_MODEL.md` for the new type. Don't commit or push without asking.

---

## S2 — Criterion benches in CI (Sonnet)

> `.github/workflows/ci.yml` has `check`, `feature-matrix`, six live-mock
> jobs, `compose-smoke`, and `cargo-deny` — no bench job. `analytics` has
> `benches/scoring.rs` on criterion 0.8 with saved baselines in
> `target/criterion`. Add a bench job.
>
> Scope it as a **compile-and-smoke gate, not a performance gate**: CI has no
> stable performance floor and no GPU, so a wall-clock regression threshold
> would flake and get ignored. The job should build the benches and run them
> at minimum sample count so a broken harness or a bench that no longer
> compiles fails the build. `cargo bench -p analytics -- --quick` is the
> shape; note that `analytics` sets `[lib] bench = false` specifically so the
> package-level command works, so do not add `--bench scoring` back in.
>
> Match the existing jobs' conventions exactly — same runner, same cache
> action and key shape, same `RUSTFLAGS`/`CARGO_TERM_COLOR` handling,
> SHA-pinned third-party actions with a version comment, same
> least-privilege `permissions` block. Read the neighbouring jobs first;
> anything you invent that they don't do is probably wrong.
>
> Do not touch `release.yml`. Do not change bench code or thresholds.
>
> Gates: the workflow must be valid YAML and the command you wire in must be
> one you have actually run locally — run it and paste the real timings.
> Docs: `docs/DEVELOPMENT.md` command list, `CHANGELOG.md` under Unreleased.
> Don't commit or push without asking.

---

## S3 — Slippy-tile basemap design pass (Opus)

> Produce `docs/BASEMAP.md`: the design for a slippy-tile basemap under the
> existing map. **Design only — no implementation this session.** This item
> has been deferred continuously since M3 because the policy questions were
> never settled, so settling them *is* the deliverable.
>
> The map today renders Natural Earth geometry through cached egui layers in
> `crates/renderer`, with `crates/geo-utils` owning projection, antimeridian
> handling, and H3. Read both before proposing anything, plus
> `docs/VISUALIZATION.md` for the layer-identity and orientation decisions
> already made.
>
> The document must answer, each with the reasoning and the rejected
> alternatives:
> - **Projection.** What the current renderer actually uses, whether XYZ/WMTS
>   tiles can be composited under it without a reprojection step, and if not,
>   what the honest options are. This decides whether the rest is cheap or a
>   rewrite.
> - **Provider policy.** Concrete candidates with their real terms: attribution
>   requirements, API-key requirements, rate limits, and whether bulk/offline
>   caching is permitted. A provider whose terms forbid the caching this
>   project needs is disqualified, not a maybe. Name at least one that is
>   viable with no key.
> - **Offline and failure behavior.** The desktop is live-data-only but must
>   stay usable with no network. Tiles missing must degrade to today's vector
>   basemap, never to a blank or half-drawn world.
> - **Cache design.** On-disk location, bound, eviction, and what happens when
>   the bound is hit mid-pan. Tiles are third-party cached bytes, not project
>   data — say explicitly whether they belong anywhere near the DuckDB store
>   (they do not) and where they go instead.
> - **The user toggle.** Default state, where it lives, and what the user is
>   told about the network traffic it starts.
> - **Rendering integration.** How tiles fit the cached-layer model without
>   per-frame tessellation or an unbounded overlay loop, and how tile loading
>   stays off the UI thread. Cite the actual cache/threading types you would
>   hook into.
> - **Precision.** A basemap must not make a country-precision shaded region
>   read as a located point. State how the visual hierarchy holds.
>
> End with a scoped implementation plan in phases, each independently
> shippable, and an explicit "do not build this if…" list.
>
> Docs: `docs/BASEMAP.md` is the deliverable; link it from
> `docs/VISUALIZATION.md` and flip the ROADMAP M8 bullet from "design pass
> before implementation" to a pointer at the finished design. Don't commit or
> push without asking.

---

## S4 — Settings and About UI (Opus)

> Build the Settings and About screen in `apps/global-signal-desktop`,
> consuming the `SourceAttribution` table added to `core-types` in a previous
> session (read it first — do not rebuild it, and if it is missing, stop and
> say so rather than inlining a second copy).
>
> Two surfaces, and they are different things:
> - **Settings** — per-source state: compiled in or not, configured or not,
>   last successful fetch, last error, next scheduled poll, and the cadence.
>   Read-only status is the floor; a user-visible enable/disable toggle per
>   source is in scope if it can be done without disturbing the ingest
>   worker's ownership model.
> - **About** — full attributions rendered verbatim where the terms require
>   it, the project licence, version, and links out.
>
> Hard constraints, in the order they matter:
> 1. **No credential ever reaches the settings database or the screen.**
>    Product rule 5. Show "configured" / "not configured" and the env var
>    *name*; never a value, never a masked prefix, never a length.
> 2. **The UI thread owns egui state and must not block a frame.** Source
>    status comes from state the UI already polls, or from an async storage
>    reply — not a synchronous query and not a network call. If the status you
>    want isn't already flowing to the UI, extend the existing channel; do not
>    add a second one.
> 3. **Cached rendering stays cached.** No per-frame allocation of the
>    attribution list, no unbounded loop.
> 4. Persisted preferences go through `crates/storage`'s existing settings
>    path (`crates/storage/src/settings.rs`), with a migration if the schema
>    changes. Follow the migration ledger already there.
>
> Match the existing panel conventions in `panels.rs`, `how_to_read.rs`, and
> `style.rs` rather than introducing a new layout idiom.
>
> **This is egui work, which compiles clean and draws nothing.** A green
> `cargo test` is not evidence here. Run the desktop for real, look at both
> screens, and report what you saw — including with a source deliberately
> unconfigured, so the "not configured" path is exercised rather than assumed.
>
> Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
> -- -D warnings`, `cargo test --workspace`, a real `cargo build -p
> global-signal-desktop` (check and clippy do not link), and a live run. Docs:
> `README.md`, `docs/ARCHITECTURE.md` if ownership moves,
> `docs/DATA_MODEL.md` for any settings migration, `CHANGELOG.md`. Don't
> commit or push without asking.

---

## S5 — Chatter segmentation for unsegmented scripts (Opus)

> `crates/chatter` matches place and topic tokens for aggregate chatter
> rollups (`place.rs`, `topic.rs`). It cannot see topics in scripts that do
> not delimit words — Burmese, Thai, Khmer, Lao, Japanese, Chinese.
>
> **Read
> [docs/ENGINEERING_NOTES.md](../docs/ENGINEERING_NOTES.md#correction-to-the-chatter-backlog-burmese-topic-tokens-will-not-work)
> before starting.** A previous session recorded the correction: adding
> keywords in these scripts does not work, because the matcher's tokenization
> never produces the tokens to match against. The real task is a segmentation
> strategy — syllable-level matching, or substring matching restricted to
> script runs. If your first instinct is to extend a keyword list, you have
> misread the problem.
>
> Deliver in this order:
> 1. **A written strategy** before code: which approach per script family,
>    what it costs per message at ingest rates, its false-positive profile,
>    and what it deliberately will not catch. Substring matching in an
>    unsegmented script produces cross-word false hits — say how that is
>    bounded, and whether an inflated count is acceptable in a rollup that is
>    presented as attention volume.
> 2. **The implementation**, pure and unit-tested, with real sample strings
>    per script in the tests.
> 3. **Wiring** into `place.rs`/`topic.rs` without changing the
>    `(place, topic, window) -> count` output contract.
>
> Non-negotiable: this touches the aggregate-before-storage boundary. Message
> text is observed and dropped in the same call — segmentation must run
> inside that call and must not buffer, cache, or return any message text.
> The rollup gains no new field. Product rule 2, and the `source-telegram`
> orchestration suite has a boundary test that must keep passing.
>
> Also state plainly whether any dependency you add is pure Rust, since the
> workspace's rustls/pure-Rust posture is deliberate, and whether a
> dictionary/model file would need bundling (a data file is a licensing and
> repo-size decision, not a detail).
>
> Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
> -- -D warnings`, `cargo test --workspace`, `cargo test -p source-bluesky
> --features live`, `cargo deny check` if a dependency is added. Docs:
> `docs/DATA_MODEL.md` (chatter matching), `docs/SAFETY_AND_PRIVACY.md` if
> coverage claims change, ENGINEERING_NOTES if the strategy corrects the
> recorded one again, `CHANGELOG.md`. Don't commit or push without asking.

---

## S6 — Profiling pass and retention increase (Opus)

> ROADMAP's M8 line is "criterion benchmarks in CI **and a profiling pass
> toward higher retention**". The benches-in-CI half is done separately; this
> session is the profiling half. The goal is a defensible answer to "how far
> back can the desktop hold data before something degrades", and then moving
> that limit.
>
> Measure before changing anything. The interesting paths are the DuckDB
> query layer behind the timeline and heatmap, the H3 bucket aggregation in
> `crates/analytics`, and the renderer's cached layers at high bucket counts.
> Report real numbers at the current retention and at 2×, 4×, and 10× the
> data volume — generated through the fixture generator, which exists for
> exactly this.
>
> Then find the actual ceiling and say which of these it is, with evidence:
> query time, memory, frame time, or storage size. Do not optimize before
> that sentence can be written.
>
> Constraints on any change you then make:
> - The storage actor owns the sole DuckDB connection. A faster query does
>   not get to open a second one.
> - No UI query may block a frame, and renderer work stays cached — no
>   per-frame tessellation, no unbounded overlay loop.
> - Retention is user-visible behavior: if the retention default moves, that
>   is a README and CHANGELOG change, not a silent constant edit.
> - An index or a materialized rollup is a schema change and needs a
>   migration in the ledger.
>
> This is cross-crate work touching storage threading, analytics, and the
> renderer at once — if the profiling says the fix belongs in exactly one
> crate and is mechanical, say so and stop rather than expanding scope.
>
> Gates: full workspace gates, plus the perf smoke test, plus a real desktop
> run at the new retention. Docs: `README.md` if the default moves,
> `docs/DATA_MODEL.md` for a migration, `docs/ARCHITECTURE.md` if ownership
> shifts, ENGINEERING_NOTES for anything the profiling turned up that would
> cost the next person time, `CHANGELOG.md`. Don't commit or push without
> asking.

---

## S7 — M8 close-out (Sonnet)

> Close out M8 in the docs. Every implementation session is finished; this is
> a documentation-only pass and should touch no `.rs` file.
>
> 1. `docs/ROADMAP.md`: move M8 into the Shipped table with a one-line
>    summary matching the format of the existing rows, and delete the M8
>    section's bullets that are now done. Items that were **not** built —
>    CelesTrak satellites and AIS — do not silently vanish: leave them stated
>    as gated on a thinning/precision/disclosure design that has not been
>    written, and say so in the same voice the file already uses for deferred
>    work. If a bullet only partly landed, say which part.
> 2. `CHANGELOG.md`: fold the Unreleased entries into a milestone section
>    consistent with the `## [0.7.0] — M7` heading style, and check every
>    entry against `git log` for the milestone so nothing shipped is
>    undocumented and nothing documented is unshipped.
> 3. `README.md`: user-visible M8 behavior — the Settings/About screens and
>    any retention or chatter-coverage change.
> 4. Verify every internal doc link that the M8 work added or moved actually
>    resolves.
>
> Do not restate implementation detail that belongs in the crate docs, and do
> not create a session journal — CHANGELOG and ROADMAP carry this. Don't
> commit or push without asking.
