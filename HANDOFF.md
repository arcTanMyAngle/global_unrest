# Session handoff — Live Earth Signals

Last session: 2026-08-10. **M0–M6 complete; V1 visualization batch shipped.**
M6 (repo hygiene, CI depth, releases) shipped everything in
[docs/ROADMAP.md](docs/ROADMAP.md) except branch protection on `main`, which
needs a human with an authenticated `gh`/GitHub session (this machine's `gh`
is installed but not logged in) — see "Loose ends" below. This session
implemented all four **V1** items from
[docs/VISUALIZATION.md](docs/VISUALIZATION.md) (timeline histogram, spike
halos, severity markers + tooltip, recency fade) plus a user-requested
"has video" marker filter — see "V1 — what shipped" below.

**Next session: V2** (attention↔unrest divergence layer, top-movers panel,
region history sparkline + event ledger) per
**[docs/VISUALIZATION.md](docs/VISUALIZATION.md)** — the user's explicit
direction remains *original, detailed* views, never copies of provider
dashboards. M7 service-hardening items can interleave. Read this file, then
[CLAUDE.md](CLAUDE.md), then those two docs.

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — pushed to the user's **public repo** `github.com/arcTanMyAngle/global_unrest` (HTTPS origin, GCM-cached auth; the sibling `../global_unrest/` folder is an empty clone shell). CI is live on push: `check` (fmt/clippy/test × Windows+Ubuntu), `feature-matrix` (M5 features × Ubuntu), `acled-live-mock`, `compose-smoke`, `cargo-deny`. **Not yet pushed to origin** — 4 local V1 commits on `main` this session, see "Loose ends." |
| Commits | Clean PR-sized commits through M6, plus 4 more this session for V1 (`git log --oneline`) |
| Tests | `cargo test --workspace` green (including new histogram/halo/severity/video-filter/fade tests); E2E pipeline test green; clippy `-D warnings` clean; `cargo deny check` clean (new `core-types → url` dependency edge checked) |
| Version | Workspace `0.6.0` (milestone-tied: `0.<M>.0`); all crates `publish = false` (internal-only, never meant for crates.io). Not bumped for V1 — versioning is milestone-tied, not batch-tied. |
| Credentials | `.env` (gitignored) holds `ACLED_EMAIL`/`ACLED_PASSWORD`; `.env.example` is the committed template |
| Brief / plan | `../prompt_1.md`; [docs/PLAN.md](docs/PLAN.md) (M0–M5 ✅); [docs/ROADMAP.md](docs/ROADMAP.md) (M6 ✅ except branch protection; V1 ✅; M7/V2/M8 next) |

## V1 — what shipped (2026-08-10, 4 PR-sized commits, see `git log`)

Per [docs/VISUALIZATION.md](docs/VISUALIZATION.md)'s V1 batch, in order:

1. **Timeline histogram strip** — the bare time-window `egui::Slider` is
   replaced by a custom-painted widget (new
   `apps/global-signal-desktop/src/timeline_strip.rs`): stacked kind-colored
   bars for discrete events, a thin attention-count line on its own scale
   (never mixed into the stack), a translucent window brush, and a
   draggable/clickable playhead. New `storage::timeline_histogram()`
   aggregates `(bucket_start, kind) -> count` directly against `events` for
   the full extent (no `region_buckets` rollup exists at that grain);
   fetched once on ingest/extent refresh, not on scrub. Visually verified:
   click-to-scrub moves the playhead and updates the window label instantly.
2. **Spike halos** — pulsing rings on H3 cells whose `spike_score` clears a
   new named threshold (`analytics::weights::SPIKE_HALO_THRESHOLD = 0.8`,
   capped to `SPIKE_HALO_MAX_CELLS = 40`); cold-start cells excluded (no
   baseline, no anomaly claim). New pure `analytics::spike_halo_cells` (unit
   tested) derives the cell list from the already-cached `window_buckets` —
   no new storage query. New `renderer::HaloLayer` draws plain per-frame
   epaint circle strokes (not a `GeoMesh` — the doc calls for this
   explicitly; the cell list is small and the pulse animates every frame),
   following the basemap border-stroke precedent. New top-bar toggle
   (`Filters::show_spike_halos`, default on) and a legend entry. Visually
   verified: rings render on real live ACLED/GDELT hotspots and the toggle
   cleanly shows/hides them.
3. **Severity-weighted markers + richer tooltip** — marker size now
   interpolates with `severity` (0..1, e.g. ACLED fatality-derived) when
   present, falling back to the existing article-count-derived sizing
   otherwise. Hover tooltip gained severity, precision (new
   `LocationPrecision::label()`), source, and a video badge alongside
   kind/timestamp. `storage::EventPoint` gained `severity`/`source`.
4. **Has-video marker filter** (user-requested mid-session, folded into #3's
   vertical slice since it touches the same `EventPoint`/query pipeline) —
   a new top-bar "🎥 has video" toggle filters markers to those whose record
   carries a URL classified as video. The classifier already existed
   duplicated in the region inspector's source-link list
   (`youtube.com`/`vimeo.com`/etc. hosts + direct video file extensions);
   moved it to `core_types::is_video_url` as the single shared
   implementation (new `core-types` dep on `url`, already workspace-pinned)
   used by both the new `storage::query_points(video_only)` filter and the
   existing inspector code — no behavior change there, just de-duplication.
5. **Recency fade during playback** — while playing, marker opacity decays
   with age inside the current window (newest ≈ opaque, oldest ≈ 35%,
   linear in between) via a new pure `fade_alpha` (unit tested) and a
   `MarkerInput::alpha` field consumed with the same `gamma_multiply` idiom
   `heatmap.rs` already uses. Pausing always shows full detail — the fade
   is playback-only, and costs nothing extra per frame since playback
   already re-fires the points query on every bucket step. Visually
   verified: pressing play advances the window and playhead smoothly with
   no crash; opacity decay itself is covered by the renderer unit test
   (`alpha_fades_marker_opacity`) rather than a live capture (hard to time
   a screenshot against a 0.4s-per-step animation).

### GUI verification note (this session)

Ran the live app (real ACLED/GDELT/NOAA data, not fixtures — this is the
live-only desktop) via the `.claude/skills/run/SKILL.md` recipe and
confirmed: the histogram strip renders with real per-kind bars and an
attention line; click-to-scrub moves the playhead and updates the window
label; spike halos render on real hotspots and the top-bar toggle
shows/hides them cleanly; the "has video" checkbox is present and wired;
playback advances without crashing. **New landmine**: `SetProcessDPIAware()`
must be called in **every** PowerShell process that does screen capture, not
just the one that maximizes the window — the run skill's existing note (#2)
already says this, but it's easy to miss that this includes throwaway
follow-up screenshot calls, not just the first one. A first screenshot
attempt without it produced a virtualized/scaled 1707×1067 capture that
visually looked like the bottom timeline panel was completely missing (it
wasn't — the capture was just clipped/scaled by DPI virtualization); a
second capture with `SetProcessDPIAware()` called in that same process
(`PrimaryScreen.Bounds` correctly reporting the full 2560×1600) showed
everything, including the timeline strip.

## Milestone 6 — what shipped (PR-sized commits, see `git log`)

1. **CI depth** — `feature-matrix` job (Ubuntu only; the feature code isn't
   OS-specific) clippies + tests `global-signal-desktop`+`workers` across
   `acled-live`/`noaa-live`/both; `acled-live-mock` job runs
   `source-acled`'s mock-OAuth suite standalone.
2. **`compose-smoke` CI job** — builds both service Docker images, runs the
   stack with `LES_ONLINE=0` (`docker-compose.yml`'s worker env is now
   `${LES_ONLINE:-1}`, shell-overridable), polls `/health`, asserts
   `snapshot.events > 0` via `jq`. Closes the M4 verification gap that's
   been open since 2026-07-16 — first real exercise of `docker compose up`,
   just not on this machine (still no local docker CLI).
3. **`cargo-deny`** (`deny.toml` + CI job) — installed the tool locally to
   validate for real rather than guessing. Two rounds of real findings
   fixed:
   - License allowlist was missing `BSL-1.0` (clipboard-win/error-code via
     arboard), `OFL-1.1`+`Ubuntu-font-1.0` (egui's bundled default fonts),
     `CDLA-Permissive-2.0` (webpki-roots) — all legitimately permissive,
     added after `cargo deny check` named them.
   - `[bans] wildcards = "deny"` flagged every internal workspace path
     dependency (no version req) as unbounded. Fix: `[workspace.package]
     publish = false` + `publish.workspace = true` on all 12 members (none
     of these are meant for crates.io anyway) + `allow-wildcard-paths =
     true` — that combination is what cargo-deny actually checks for
     ("does not apply to public crates").
   - Two RUSTSEC advisories are explicitly `ignore`d with reasoning in
     `deny.toml`, not silently allowed: quick-xml's DoS-class CVEs
     (RUSTSEC-2026-0194/0195) reach us only via `wayland-scanner`, which
     parses quick-xml at **build time** against its own bundled trusted
     protocol XML — never attacker input; `ttf-parser` unmaintained
     (RUSTSEC-2026-0192, "no safe upgrade available" per its own advisory)
     is reached only through the Linux Wayland clipboard's font fallback
     (`ab_glyph` → `sctk-adwaita`). Both are transitive through
     `eframe`/`winit`; fixing either means bumping winit's Wayland backend
     stack, out of scope for this pass — re-check next `eframe` bump.
4. **Dependabot** (`.github/dependabot.yml`) — cargo + github-actions,
   weekly, grouped; `wgpu` excluded from auto-bumps (locked to `eframe`,
   CLAUDE.md).
5. **Releases** (`.github/workflows/release.yml`, tag-driven on `v*`) —
   desktop binaries (Windows/Linux/macOS) zipped/tarred with `fixtures/`
   alongside and attached to GitHub Releases; worker/api images built and
   pushed to `ghcr.io/arcTanMyAngle/global-unrest-{workers,api}` on the
   same tag. Not yet exercised (no tag pushed) — first `git tag v0.6.0&&
   git push --tags` will be the real test.
6. **`CHANGELOG.md`** — Keep-a-Changelog format, retroactive milestone
   entries 0.1.0 (M1) through 0.6.0 (this M6), dated from `git log`.
   Workspace version bumped 0.1.0 → 0.6.0 to match.
7. **Portfolio README** — CI/license/rust-version badges; a mermaid
   architecture diagram (sources → core → storage → desktop/services); a
   real screenshot (`assets/screenshots/map-overview.png`, offline fixture
   mode, captured via the run skill this session — see the GUI-verification
   note below); an "Ethics & data policy" section; M6 roadmap line;
   `CONTRIBUTING.md`/`CHANGELOG.md` doc-table rows.
8. **`CONTRIBUTING.md`** — PR workflow, quality-gate commands (including
   the new feature-matrix and `cargo-deny` ones), feature-gating rules for
   new live sources, visualization-originality rule.

### GUI verification note (screenshot capture)

Launched the app headlessly, foregrounded/maximized it (DPI-aware Win32
recipe, `.claude/skills/run/SKILL.md`), and captured one clean screenshot
of the map view (now `assets/screenshots/map-overview.png`). Attempted a
second click-through screenshot of the region inspector; the *second*
screenshot came back showing the user's own VS Code/Claude Code window
instead of the app — focus had been stolen back between the click and the
capture. Per the established rule (landmine #8 in the run skill: "if
foreground keeps getting stolen, the user is actively at the machine —
stop sending input immediately"), synthetic input was stopped immediately
and the app process was killed. One good screenshot was enough for the
README; no second attempt was made this session.

### Loose ends

- **Branch protection on `main`** — the only unfinished M6 item. `gh` is
  installed on this machine but not authenticated
  (`gh auth login` needed first), so it can't be scripted here. Once
  authenticated: `gh api repos/arcTanMyAngle/global_unrest/branches/main/
  protection -X PUT --input -` with a JSON body requiring the `check` (both
  OS legs) and `feature-matrix` status contexts, or do it via GitHub →
  Settings → Branches in the browser.
- **Release workflow untested** — `.github/workflows/release.yml` is
  written and YAML-validated but has never actually run (no tag pushed
  yet). First real exercise: `git tag v0.6.0 && git push origin v0.6.0`
  (confirm with the user before pushing a tag/triggering a public release
  and GHCR image push).
- **`compose-smoke` untested locally** — validated the YAML and the logic
  by hand (no local docker CLI, unchanged from prior sessions); first real
  run will be on CI's next push.
- **V1 commits not pushed to origin** — 4 new commits on `main` this
  session (histogram, halos, severity+tooltip+video-filter, fade) sit
  ahead of `origin/main`; not pushed yet (wasn't asked to). `git push`
  when ready — CI will run the same gates verified locally this session.
- **README screenshot not refreshed for V1** — `assets/screenshots/
  map-overview.png` still shows the pre-V1 map (bare slider, no halos).
  VISUALIZATION.md's own guardrail says shipped views should get a
  screenshot; deferred this session (verification screenshots were taken
  ad hoc into the scratchpad, not committed) — worth doing next session or
  on request.

## Next up — professional-level roadmap (user-approved)

Canonical version: **[docs/ROADMAP.md](docs/ROADMAP.md)** (+
[docs/VISUALIZATION.md](docs/VISUALIZATION.md) for the V1–V3 view batches,
which take priority per the user). Summary:

- **V1–V3 visualization batches**: timeline histogram + spike halos +
  severity markers + recency fade (V1) ✅ shipped this session (see "V1 —
  what shipped" above). **Next session's focus: V2** — attention↔unrest
  divergence layer + top-movers + region sparkline + event ledger; then V3
  — per-source layer identity/legend + basemap orientation polish + "how
  to read this map" overlay. Honest-visualization principles and perf
  guardrails in VISUALIZATION.md are binding; never copy a provider's
  dashboard (ACLED etc.) — build original detail on this app's own visual
  language.
- **M7 — service hardening**: axum middleware (timeouts, concurrency cap,
  per-IP rate limit, CORS, compression, trace layer, graceful shutdown),
  snapshot-version ETag, `/events` pagination, OpenAPI via utoipa,
  Prometheus `/metrics`, snapshot-age alerting in `/health`, integration
  suite over a committed fixture snapshot. **Never serve ACLED-bearing
  snapshots publicly** (SAFETY).
- **M8 — desktop polish + stretch**: walkers basemap + CelesTrak satellites
  (sgp4) as the thematic stretch, AIS (aisstream.io key) only if wanted,
  settings UI (creds stay env-only), About panel attributions, criterion
  benches in CI.

## Landmines and quirks (learned the hard way)

- **cargo-deny (M6)**: internal workspace path deps need `publish = false`
  (workspace-level, inherited via `publish.workspace = true` per crate) +
  `[bans] allow-wildcard-paths = true` together, or every path dependency
  is flagged as an unbounded wildcard — `allow-wildcard-paths` alone only
  exempts crates already marked non-publishable. License allowlists need
  running the tool for real (`cargo install cargo-deny`, ~minutes cold);
  guessing the SPDX ids from memory missed `BSL-1.0`/`OFL-1.1`/
  `Ubuntu-font-1.0`/`CDLA-Permissive-2.0` this session. `[graph] targets`
  matters — Wayland/Linux-only transitive deps (and their advisories) only
  show up if `x86_64-unknown-linux-gnu` is in the target list; this repo
  ships to all three OSes so all three are listed.
- **docker-compose env overrides**: a hardcoded `KEY: "value"` in
  `environment:` can't be shell-overridden; use `KEY: "${KEY:-default}"`
  if CI (or anyone) needs to flip a flag like `LES_ONLINE` without editing
  the file.
- **ACLED auth (M5)**: no API keys anymore — OAuth password grant with
  `client_id=acled`, `scope=authenticated`; refresh grant on expiry; the
  token endpoint's `error_description` is surfaced in errors (never the
  credentials). A `400 invalid_grant` means the account/password is wrong,
  not the request. ACLED **corrections reuse event ids** — dedup-by-id means
  revisions are not re-applied (accepted, documented).
- **NOAA alerts**: most alerts are zone-scoped with `geometry: null` —
  normalization returns `Ok(vec![])` for them (not an error, not a guess).
  US coverage only. api.weather.gov wants a descriptive User-Agent.
- **Feature stubs**: both binaries wrap ACLED/NOAA in tiny cfg modules
  (`make() -> Option<Source>`) so the select loops stay cfg-free. Clippy the
  matrix: default, `acled-live`, `noaa-live`, both — CI now does this
  automatically (`feature-matrix` job).
- **reqwest has no `json` feature here** (lean rustls pin): use
  `.text()` + `serde_json::from_str`, like source-gdelt.
- **egui 0.35 API**: `App::ui(&mut self, ui, frame)`; unified
  `egui::Panel::top/bottom/right(id)`; menu close is `ui.close()`.
  eframe 0.35 rides **wgpu 29** — do not bump wgpu independently (also why
  Dependabot excludes `wgpu` from auto-bump PRs).
- **duckdb crate** `1.10504.0` = DuckDB 1.5.4. Connection `!Sync` — one
  thread (storage actor); the api opens throwaway in-memory conns inside
  `spawn_blocking`. No ALTER TABLE ADD non-null columns.
- **Single-writer rule (M4)**: worker owns its `.duckdb`; api reads only
  Parquet snapshots via the atomically-flipped `LATEST` pointer.
- **M3/M4 deps**: reqwest 0.12 rustls `default-features=false`; `zip` 6
  needs `deflate-flate2` + direct `flate2`; `governor` 0.10; axum 0.8 (api
  only); Docker builder needs `cmake`.
- **GDELT DOC has no per-article coordinates** — source-country precision
  only; FIPS≠ISO traps (AU/AS, CH/SZ, CI); Events keeps CAMEO roots 14–20.
- Desktop app data: `%LOCALAPPDATA%\LiveEarthSignals\live-earth-signals\data`;
  worker uses `…-worker`. First cold build compiles DuckDB C++ (minutes).
- **GUI verification on this machine**: `.claude/skills/run/SKILL.md`;
  focus-stealing prevention applies — if another app keeps taking
  foreground, the user is at the machine; stop sending input (this
  happened again this session — see the GUI verification note above).
- **DPI-unaware screenshot = looks like content is missing, not just
  scaled (V1)**: every PowerShell tool call is a fresh process, so
  `SetProcessDPIAware()` must be (re-)called in the *same* process that
  calls `CopyFromScreen`/`Screen.PrimaryScreen.Bounds` — not just the one
  that maximized the window. Skipping it silently returns a
  DPI-virtualized 1707×1067 capture (on this machine's 2560×1600 @150%)
  that looked exactly like the bottom timeline panel had vanished; it
  hadn't — the capture was just clipped. Always sanity-check
  `Screen.PrimaryScreen.Bounds` equals the real physical resolution before
  trusting a "missing UI" observation.
- **Custom egui widgets that replace a stock one (V1 timeline strip)**:
  `ui.allocate_painter(size, Sense::click_and_drag())` + `response.dragged()
  ||response.clicked()` + `response.interact_pointer_pos()` is the whole
  recipe for a draggable/clickable custom strip — no new architecture
  needed beyond what `map_view.rs`'s pan/zoom handling and
  `draw_cell_outline`'s per-frame `Shape` painting already established.

## Quality gates (run after every step; CI runs the same, plus more)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p source-acled --features live   # M5 mock-server suite
cargo deny check                             # M6: advisories + licenses (needs `cargo install cargo-deny`)
```

If you touched the desktop app, `services/workers`, or any `source-*`
crate, also run the M5 feature matrix (CI's `feature-matrix` job does this
automatically, but it's fast enough to run locally too):

```sh
cargo clippy -p global-signal-desktop -p workers --features acled-live,noaa-live --all-targets -- -D warnings
cargo test -p global-signal-desktop -p workers --features acled-live,noaa-live
```
