# Session handoff — Live Earth Signals

Last session: 2026-08-12. **M0–M6 complete; V1 visualization batch shipped;
IODA and Bluesky live sources shipped.** This session: verified IODA live in
the running app (the previous session's open item), then built the **Bluesky
Jetstream** aggregate-chatter source — new `crates/chatter` (the shared
aggregate-before-storage machinery) and `crates/source-bluesky` (the first
*streaming* source), wired into both binaries behind `bluesky-live`.

**Next session: public Telegram channels**, reusing `crates/chatter`
unchanged. It is blocked on **two user decisions** (which channels; bot
token vs. personal MTProto credentials) — see "Next session: Telegram"
below; ask those before writing code. After Telegram: V2 visualization
(docs/VISUALIZATION.md), interleaved with M7 service hardening.

**Before touching `chatter` or `source-bluesky`, read
[docs/SAFETY_AND_PRIVACY.md](docs/SAFETY_AND_PRIVACY.md) hard rule 6.** The
aggregate-only shape of these sources is a deliberate hold-the-line
decision (see "Why aggregate-only"), not a default to relax.

Read this file, then [CLAUDE.md](CLAUDE.md).

## Where things stand

| | |
|---|---|
| Repo | `live-earth-signals/` — the user's **public repo** `github.com/arcTanMyAngle/global_unrest`. **`origin/main` is behind: the 2 IODA commits, the previous handoff commit, and this session's Bluesky commits are all local-only.** The user was asked this session and chose **not** to push yet — ask again rather than pushing unprompted. CI: `check` (fmt/clippy/test × Windows+Ubuntu), `feature-matrix` (now each of `acled-live`/`noaa-live`/`ioda-live`/`bluesky-live` solo plus all four × Ubuntu), `acled-live-mock`, `compose-smoke`, `cargo-deny`. |
| Commits | Clean PR-sized commits through M6, then 4 for V1, 2 for IODA, then this session's Bluesky set (gazetteer, `chatter`, `source-bluesky`, wiring, docs) — `git log --oneline` |
| Tests | `cargo test --workspace` green; E2E pipeline test green; clippy `-D warnings` clean on default **and** the 4-way feature matrix (`acled-live,noaa-live,ioda-live,bluesky-live`), plus `bluesky-live` solo and `--no-default-features` |
| Version | Workspace `0.6.0` (milestone-tied: `0.<M>.0`); not bumped for V1, IODA, or Bluesky — versioning is milestone-tied, not batch-tied |
| Credentials | `.env` (gitignored) holds `ACLED_EMAIL`/`ACLED_PASSWORD`; `.env.example` is the committed template. IODA and Bluesky are both keyless — nothing to configure. |
| Brief / plan | `../prompt_1.md`; [docs/PLAN.md](docs/PLAN.md) (M0–M5 ✅); [docs/ROADMAP.md](docs/ROADMAP.md) (M6 ✅ except branch protection; V1 ✅; IODA ✅ pulled forward from M8; M7/V2/M8-remainder next) |
| **GUI live-visual verification** | **Done for V1** (real live data, screenshots — see below). **IODA: log-verified this session** (see "IODA verification" below) — a real fetch cycle completed in the running app. **Bluesky: verified at the data level, not in the GUI** — the real client was run against the live firehose via the `live_probe` example (5,918 posts scanned → 16 matched → 15 rollups → 15 normalized events), but the desktop app was not launched with `bluesky-live` on and no chatter event has been *seen on the map*. That's the natural first check next session. Note it takes ≥5 minutes of runtime before the first flush publishes anything, and chatter events are mostly Country precision, so they shade regions rather than appearing as markers. No screenshots were taken this session: the user was actively at the machine, and the run skill's landmine #8 says stop sending synthetic input in that case. |

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

## IODA — what shipped (2026-08-11, 2 commits: code, docs)

New optional live source, `crates/source-ioda`, feature `ioda-live`
(keyless, desktop default) — Internet Outage Detection and Analysis
(Georgia Tech Internet Intelligence Research Lab): near-real-time
internet-outage events, country precision. User-requested mid-session as
the first of three "real-time signal ahead of mainstream media" sources
(IODA → Bluesky Jetstream → Telegram, in that order); pulled forward from
`docs/ROADMAP.md`'s M8 stretch-layers bucket rather than invented as a new
milestone number.

**API was verified live, not guessed.** `curl` against the real endpoint
succeeded in this session's transcript; the exact query params and response
shape came from reading `InetIntel/ioda-api`'s actual PHP controller source
(`src/Controller/OutagesController.php`) on GitHub, not docs-scraping (IODA's
own docs pages are a JS SPA that returns an empty shell to a plain fetch).
Base: `https://api.ioda.inetintel.cc.gatech.edu/v2/`. Endpoint:
`GET /outages/events?entityType=country&from=<unix>&until=<unix>&format=codf`
— keyless, no stated rate limit. Response `data[]` items:
`{"location":"country/US","start":<unix>,"duration":<secs>,"method":"median","datasource":"ping-slash24","score":753.19,"location_name":"United States","overlaps_window":false}`.

Design decisions worth knowing about:
- **Country geocoding reuses real geometry, not a hand-typed table.**
  `geo_utils::CountryIndex` (already bundling Natural Earth's
  `ne_110m_admin_0_countries.geojson` for the basemap/click-lookup) gained
  `iso_a2: String` on `CountryInfo` and a new
  `centroid_by_iso_a2(code) -> Option<(&CountryInfo, (f64, f64))>`, backed
  by a real `geo::Centroid`-computed centroid per country, precomputed once
  at load. Rejected hand-typing ~100+ country centroids from memory as too
  error-prone for this project's "never guess a coordinate" rule. An IODA
  code not in Natural Earth's ~177 countries fails normalization into
  `ingest_log` rather than guessing.
- **Severity is log-scaled from an unbounded score.** IODA's `score` has no
  fixed range (observed live: ~700 for a brief blip to ~233,000 for a total
  national blackout). `source_ioda::severity_from_score` squashes it onto
  `[0,1]` via named constants `weights::IODA_SCORE_FLOOR` (100.0) /
  `IODA_SCORE_CEIL` (100,000.0) — the first *continuous* severity
  normalization in this codebase (NOAA's is a 4-value categorical match on
  a bounded NWS enum). These anchors are a judgment call from one session's
  worth of live samples, not a calibrated statistical fit — revisit if
  real usage shows most events pinned at the floor or ceiling.
- **Country precision, so never a point marker.** Same precision-rendering
  contract as everything else coarser than City — IODA events shade H3
  cells in the heatmap and count in the region inspector, but the map will
  never show an IODA diamond marker. This is correct, not a bug — worth
  remembering before "fixing" it.
- **`source_event_id`** is a composite key (`{country}-{start}-{datasource}-
  {method}`) since IODA's `codf` format has no explicit event id.

Wiring is a straight copy of `source-noaa`'s pattern (keyless cfg-stub in
both `ingest.rs` and `services/workers/src/main.rs`, `live_cycle` polling
every 15 min with a 6 h lookback — IODA's own server-side `extendWindow`,
14 days by default, plus dedup-by-id cover the rest). Full docs pass done
per `CONTRIBUTING.md`'s new-source checklist (see `git log` for the docs
commit) — `docs/DATA_MODEL.md` has the fullest technical writeup if you need
more detail than this.

## IODA verification (2026-08-12) — the previous session's open item, closed

Launched the desktop app headlessly with logs and watched a real IODA cycle
complete:

```
INFO source_ioda::live: ioda outage events fetched records=7
INFO global_signal_desktop::ingest: live cycle ok records=6 origin="ioda"
INFO storage: ingest complete inserted=6 duplicates=0 failures=1 pruned=0
```

The `failures=1` is **the designed path, not a bug**, and it's worth knowing
why before someone "fixes" it. The same 6-hour window fetched by hand
returned 7 events, one of them `country/VG` (British Virgin Islands).
Natural Earth's 1:110m set has ~177 countries and does not include VG, so
normalization fails into `ingest_log` rather than inventing a coordinate —
exactly the documented behaviour. Expect a steady trickle of these for small
territories. If that ever becomes annoying, the fix is bundling `ne_50m`
rather than hand-typing a fallback table.

## Bluesky — what shipped (2026-08-12)

New `crates/chatter` + `crates/source-bluesky`, feature `bluesky-live`
(keyless, desktop default), wired into both binaries. Second of the three
user-prioritized real-time sources. Full technical writeup:
`docs/DATA_MODEL.md` § "Chatter normalization"; policy in
`docs/SAFETY_AND_PRIVACY.md` hard rule 6.

**Verified against the real thing, twice.** The endpoint and exact message
schema came from a live socket capture *before* any Rust was written (the
same discipline IODA got), and then the finished client was run against the
live firehose via the committed `live_probe` example:

```
cargo run -p source-bluesky --features live --example live_probe -- 120
scanned 5918 posts, matched 16 (0.270%)  ->  15 rollups, 15 events
```

Results were plausible (Colombia+earthquake, Ukraine+strike, Athens+flood,
Chicago+flood). **0.27% is the honest hit rate** — worth remembering before
anyone assumes the source is broken because a quiet window produces nothing.

### Design decisions worth knowing about

- **A stream behind a poll interface.** `SignalSource` is fetch-a-window
  shaped, and Bluesky is not. Rather than adding a new source shape, the
  socket task counts continuously into a shared accumulator and `fetch()`
  drains it, so `ingest.rs`'s select loop needed no new structure — just
  another arm identical to NOAA/IODA's.
- **Only *completed* windows drain.** This one is subtle and was a real bug
  caught during wiring. `source_event_id` is `{place}-{topic}-{window_start}`,
  so a mid-window drain publishes a partial count under an id that the
  window's remainder would later collide with — and dedup-by-id would
  silently drop those posts. `drain_completed(now)` leaves the in-progress
  window accumulating. Do not "simplify" this back to a plain drain.
- **`time_us`, not `createdAt`.** The record's `createdAt` is written by the
  posting client and can be backdated, wrong, or in the future; `time_us` is
  the firehose's own ordering clock.
- **No cursor on reconnect.** Jetstream can replay from a `time_us` cursor,
  but replayed posts would be counted twice and inflate the aggregates. A
  gap while disconnected undercounts instead — the honest direction to fail.
- **Place *and* topic both required.** This is the main false-positive
  defence, not a filter refinement: a place name alone matches recipes and
  given names. One place and one topic per post (leftmost, longest) so a
  widely-shared multi-country post can't inflate several aggregates.
- **Named ambiguity list.** `AMBIGUOUS_TOKENS` drops `male` (Malé, which
  Natural Earth itself ASCII-folds into the English word), plus `chad`,
  `jordan`, `georgia`. "us" is deliberately *not* a United States alias.
  Countries beat cities on token collisions ("Panama").
- **Chatter is attention, never an event.** Rollups are
  `EventKind::NewsAttention` with the post count in `article_count`, so they
  feed the attention component and never the unrest one.
  `location_confidence` is 0.5, saying in the number the UI already shows
  that keyword matching is crude.

### Landmine found here: rustls needs an explicit crypto provider

`tokio-tungstenite`'s rustls feature pulls rustls but selects **no** crypto
provider, and rustls 0.23 panics on the first handshake if it can't infer
one. `source-bluesky`'s dependency graph has no `reqwest` to supply it, so
`live.rs` installs `ring` explicitly. This is the nasty kind of bug: feature
unification means it would have *appeared* to work inside the desktop binary
(which links reqwest) while failing in any standalone example or test.
Caught by running the probe, not by reading code.

## Next session: Telegram (aggregate-only, reusing `chatter`)

The third real-time source. `crates/chatter` was built to be reused
unchanged — a Telegram source needs only its own message-fetching path plus
`ChatterAccumulator::observe` + `drain_completed`, then the same cfg-stub
wiring `bluesky` uses in `ingest.rs` and `services/workers/src/main.rs`.

**Ask the user these two questions before writing code** (they were flagged
as decisions in the previous handoff and are still open):

1. **Which specific channels** — a small explicit curated allowlist (known
   conflict/OSINT-monitoring channels), not open crawling. Reading public
   channels with an explicit allowlist is a materially different posture
   from scraping, but it's still a real list someone has to choose.
2. **Credential path** — a bot token (can only read channels it has been
   added to as admin) vs. a personal account's MTProto API id/hash (can read
   any public channel's history, but ties ingestion to a real Telegram
   account). Don't default to the personal-account path; it's the more
   invasive of the two.

Note Telegram differs from Bluesky in one way that matters: it is
**poll-based**, not streaming, so it does *not* need the spawn-a-socket
pattern — it can use `live_cycle` directly like NOAA/IODA, with the
accumulator filled during `fetch` rather than by a background task.

### Why aggregate-only (read this before writing any code here)

Early in this session the user asked to add these two sources **and** to
drop the project's "no person-level identification/tracking/targeting"
rule (CLAUDE.md's hard rules), reasoning that only they would be using it
for now. That was declined, not just noted as a preference to revisit:
Bluesky posts and Telegram channel messages tied to real-time unrest events
are frequently posted *by* the protesters, journalists, and dissidents in
those events, often somewhere being identified as such is genuinely
dangerous. A tool that geolocates and tracks individuals against
unrest/conflict data is the shape of thing that has historically been used
to identify and target exactly those people — and that risk doesn't scale
down just because only one person is looking at it today; the capability
and any data collected outlive the current single-user framing. The user
accepted the alternative: both sources are **aggregate chatter-volume
signals**, the same shape as GDELT's article-count attention, not
individual-post tracking. This is a hold-the-line constraint for whoever
picks this up next, not a suggestion to re-litigate:

- **Never store an individual post/message**, its author handle/DID/user
  id, or its literal text, even transiently in the database. Match against
  a keyword+place-token list as text passes through, increment an in-memory
  counter, discard the source text/author immediately.
- Flush periodically into `NewsAttention`-kind `GeoTemporalEvent`s (chatter
  volume is an attention signal, same class as GDELT DOC article counts) —
  `article_count` = the matched-post count for that window, `headline` a
  generic string like `"Social chatter spike: <keyword>"`, never real
  post/message content, no per-post URLs.
- Place attribution is crude keyword string-matching against a small
  curated country/major-city token list (reuse `geo_utils::CountryIndex`
  from the IODA work above for country centroids) — **never** NLP-based
  location inference from post content. If nothing matches, the post
  contributes to no aggregate at all (never guessed).

### Architectural gap both sources share: this codebase has no aggregate-before-storage pattern yet

Every existing source (GDELT, ACLED, NOAA, IODA) stores one
`GeoTemporalEvent` per raw record and lets `storage::score_buckets`
aggregate later. Bluesky/Telegram need the opposite: aggregate first
(in-memory, ephemeral), store only the periodic rollup. Worth designing
this once as a small shared piece (e.g. a `ChatterAccumulator` type with
`record_match(place_token, keyword, ts) `/`flush() -> Vec<GeoTemporalEvent>`)
that both sources use, rather than duplicating the accumulation logic.
Where it should live is an open question — a new small crate, or a module
in each source crate — worth 10 minutes of thought before writing code, not
a given.

### Bluesky Jetstream

Public WebSocket firehose (keyless), **not** a poll-based REST endpoint —
the first *streaming* source in this codebase. Public Jetstream instances
exist at `wss://jetstream2.us-east.bsky.network/subscribe` and similar
(multiple regions; verify current endpoints live, don't trust this from
memory next session — same "read the real thing" discipline used for IODA
this session), filterable server-side to the `app.bsky.feed.post` collection
via a query param. Needs a long-lived WebSocket task — `tokio-tungstenite`
or similar is a **new** dependency, not something already in the workspace;
`sched::request_limiter`/`Backoff` (built for poll-based sources) don't
apply here, this needs its own reconnect/backoff logic for a dropped
socket. Verify the real message schema live before coding against it, the
same way this session verified IODA's actual JSON shape via `curl` rather
than trusting documentation.

### Telegram public channels

Needs two decisions from the user before implementation, not defaults to
assume:
1. **Which specific channels** — a small explicit curated allowlist (e.g.
   known conflict/OSINT-monitoring channels), not open crawling. Reading
   public channels via Telegram's own API with an explicit allowlist is a
   materially different posture from scraping, but it's still a real list
   someone has to choose.
2. **Credential path** — a bot token (can only read channels it's been
   added to as admin) vs. a personal account's MTProto API id/hash (can
   read any public channel's history, but ties ingestion to a real
   Telegram account). The user should explicitly pick one; don't default to
   the personal-account path without asking, since it's the more invasive
   of the two.

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
- **Nothing is pushed to origin** — the 2 IODA commits, the previous
  handoff commit, and all of this session's Bluesky commits are local-only.
  The user was asked directly this session and chose "not yet", so **ask
  before pushing**; don't treat it as a pending chore to clear. When it
  happens, CI runs the same gates verified locally, now including the
  4-way feature matrix.
- **Dependabot PRs are open on origin** — `git fetch` this session showed
  two new remote branches (`dependabot/cargo/...`,
  `dependabot/github_actions/...`). Nobody has looked at them. Note the
  standing rule that `wgpu` must not be bumped independently of `eframe`.
- **Bluesky not yet seen rendering on the map** — see the GUI row above.
  Data-level verification is done; the visual check is the natural first
  task next session (allow ≥5 min for the first flush).
- **No Bluesky mock-server test** — `source-acled` has one
  (`--features live`), and the equivalent for Bluesky would be a local
  WebSocket server driving `run_once`. `LES_BLUESKY_ENDPOINT` already
  exists to point the client at one; the parsing/counting path is covered
  by unit tests, but the socket/reconnect path is only covered by the
  manual probe. Worth adding when the streaming path next changes.
- **README screenshot not refreshed for V1, IODA, or Bluesky** —
  `assets/screenshots/map-overview.png` still shows the pre-V1 map (bare
  slider, no halos). VISUALIZATION.md's guardrail says shipped views should
  get a screenshot; deferred again — worth doing next session or on request.

## Next up — professional-level roadmap (user-approved)

Canonical version: **[docs/ROADMAP.md](docs/ROADMAP.md)** (+
[docs/VISUALIZATION.md](docs/VISUALIZATION.md) for the V1–V3 view batches,
which take priority per the user). Summary:

- **Real-time signal sources (user-prioritized)**: IODA ✅, Bluesky ✅,
  **Telegram next** — see "Next session: Telegram" above (aggregate-only,
  reuses `crates/chatter`, blocked on two user decisions).
- **V1–V3 visualization batches**: timeline histogram + spike halos +
  severity markers + recency fade (V1) ✅ shipped this session (see "V1 —
  what shipped" above). **V2 next** (after the two social sources above) —
  attention↔unrest divergence layer + top-movers + region sparkline + event
  ledger; then V3 — per-source layer identity/legend + basemap orientation
  polish + "how to read this map" overlay. Honest-visualization principles
  and perf guardrails in VISUALIZATION.md are binding; never copy a
  provider's dashboard (ACLED etc.) — build original detail on this app's
  own visual language.
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

- **rustls 0.23 needs an explicit crypto provider (Bluesky)**: see the
  Bluesky section above. The trap is that cross-crate feature unification
  hides it — the desktop binary links `reqwest`, which enables `ring`, so
  the socket would work there while any standalone example/test in
  `source-bluesky` panics on the first handshake. Install the provider
  explicitly in the crate that needs it.
- **Aggregate-before-storage sources and dedup-by-id (Bluesky)**: when a
  source derives `source_event_id` from a time window, it must publish that
  window **once, complete**. Publishing a partial window claims the id, and
  storage's dedup-by-id then silently discards the remainder. Any future
  source of this shape (Telegram) inherits the hazard.
- **Verifying a streaming API**: a plain `curl` can't check a WebSocket, but
  .NET's `ClientWebSocket` from PowerShell can, and it took ~15 lines to
  capture the real Jetstream message schema before writing any Rust. Use it
  rather than trusting a documented schema — the same rule IODA established.

- **Researching a new live API (IODA)**: the provider's own docs pages were
  a JS SPA — `WebFetch` got an empty shell every time, no matter the URL.
  What worked: find the actual server-side implementation repo (`gh
  api`/`curl` against the GitHub API for repo contents when `gh` isn't
  authenticated — unauthenticated GitHub API calls work fine for public
  repos, just rate-limited) and read the real controller/route source for
  exact param names and response shape, then confirm with one live `curl`
  before writing any Rust against it. Don't trust a WebSearch summary's
  paraphrase of an API shape — verify against the source or a live call.
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

## Token management for the next session (learned here, repo-specific)

This repo is large and its files are long; most waste comes from reading
more than needed and from polling slow builds. What worked:

**Map before you read.** Never open a big source file to find one type.
Get a line-number map first, then read only that range:

```powershell
Select-String -Path crates\core-types\src\lib.rs -Pattern '^pub (struct|enum|fn|const|trait)|^impl ' |
  ForEach-Object { "{0,5}: {1}" -f $_.LineNumber, $_.Line }
```

**Avoid wide `Grep -A/-C` on core files.** A `Grep` with `-A 42` across
`core-types` this session returned 23.7 KB and got spilled to a file —
strictly worse than the map-then-`Read` pattern above. Keep context windows
to `-C 3` unless you know the match count is small.

**Never poll a `cargo` build.** Cold/feature-matrix builds here run 5–15
minutes (bundled DuckDB C++, eframe). Start them with
`run_in_background: true` and *wait for the completion notification* — each
manual status check costs a round trip and returns nothing useful. Batch the
whole gate set into one background command that echoes `$LASTEXITCODE` after
each step, then read the exit codes once.

**Scope gates while iterating, run the full set once.** `cargo clippy -p
<crate>` during development; the workspace-wide clippy and the feature
matrix only before committing. A full-workspace clippy after every edit is
the single biggest time/token sink in this repo.

**Commit messages via `-F <file>`.** PowerShell parses `git commit -m @'`
as splatting and mangles the here-string (it failed that way this session).
`Write` the message to the scratchpad and `git commit -F` it — one attempt,
no retry loop, and long structured messages stay intact.

**Verify live APIs directly, not through a summarizing tool.** One
`Invoke-RestMethod` (or `ClientWebSocket`) returns the exact shape in a few
lines; `WebFetch` costs a model call and paraphrases. Both IODA and Bluesky
were pinned down this way.

**Read only `HANDOFF.md` + `CLAUDE.md` to start.** They are maintained to
make re-reading the crates unnecessary; if something in them is stale, fix
it there rather than compensating by reading more code.

**On offloading to another model** (the user has Gemini and a `gemini`/
`codex` CLI on this machine): there is no browser tool in this harness, so
`gemini.google.com` cannot be driven directly — the installed `gemini` CLI
is the deterministic equivalent. It is worth it for self-contained research
with a compact answer (API schemas, "what changed in crate X"). It is *not*
worth it for editing this codebase: the conventions here (privacy rules,
comment style, named-constant discipline, precision contract) take more
context to convey than the edit saves.

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
cargo clippy -p global-signal-desktop -p workers --features acled-live,noaa-live,ioda-live,bluesky-live --all-targets -- -D warnings
cargo test -p global-signal-desktop -p workers --features acled-live,noaa-live,ioda-live,bluesky-live
# and at least one solo-feature leg, since the desktop enables all by default:
cargo clippy -p global-signal-desktop -p workers --no-default-features --features bluesky-live --all-targets -- -D warnings
```

Manual live check for the streaming source (not part of CI; prints
aggregate counts only, never post text):

```sh
cargo run -p source-bluesky --features live --example live_probe -- 60
```
