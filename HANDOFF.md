# Session handoff — Live Earth Signals

Last session: 2026-07-18. **M0–M6 complete.** M6 (repo hygiene, CI depth,
releases) shipped everything in [docs/ROADMAP.md](docs/ROADMAP.md) except
branch protection on `main`, which needs a human with an authenticated
`gh`/GitHub session (this machine's `gh` is installed but not logged in) —
see "Loose ends" below.

**Next session: visualization batch V1** (timeline histogram, spike halos,
severity markers, recency fade) per
**[docs/VISUALIZATION.md](docs/VISUALIZATION.md)** — the user's explicit
direction is *original, detailed* views, never copies of provider
dashboards. M7 service-hardening items can interleave. Read this file, then
[CLAUDE.md](CLAUDE.md), then those two docs.

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — pushed to the user's **public repo** `github.com/arcTanMyAngle/global_unrest` (HTTPS origin, GCM-cached auth; the sibling `../global_unrest/` folder is an empty clone shell). CI is live on push: `check` (fmt/clippy/test × Windows+Ubuntu), `feature-matrix` (M5 features × Ubuntu), `acled-live-mock`, `compose-smoke`, `cargo-deny`. |
| Commits | Clean PR-sized commits through M6 (`git log --oneline`) |
| Tests | `cargo test --workspace` green; `cargo test -p source-acled --features live` green; clippy `-D warnings` clean on default **and** the full M5 feature matrix (verified locally this session, not just added to CI) |
| Version | Workspace `0.6.0` (milestone-tied: `0.<M>.0`); all crates `publish = false` (internal-only, never meant for crates.io) |
| Credentials | `.env` (gitignored) holds `ACLED_EMAIL`/`ACLED_PASSWORD`; `.env.example` is the committed template |
| Brief / plan | `../prompt_1.md`; [docs/PLAN.md](docs/PLAN.md) (M0–M5 ✅); [docs/ROADMAP.md](docs/ROADMAP.md) (M6 ✅ except branch protection; M7/M8 next) |

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

## Next up — professional-level roadmap (user-approved)

Canonical version: **[docs/ROADMAP.md](docs/ROADMAP.md)** (+
[docs/VISUALIZATION.md](docs/VISUALIZATION.md) for the V1–V3 view batches,
which take priority per the user). Summary:

- **V1–V3 visualization batches** (next session's focus): timeline
  histogram + spike halos + severity markers + recency fade (V1);
  attention↔unrest divergence layer + top-movers + region sparkline +
  event ledger (V2); per-source layer identity/legend + basemap
  orientation polish + "how to read this map" overlay (V3). Honest-
  visualization principles and perf guardrails in VISUALIZATION.md are
  binding; never copy a provider's dashboard (ACLED etc.) — build original
  detail on this app's own visual language.
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
