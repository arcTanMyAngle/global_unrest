# CLAUDE.md — Live Earth Signals

Desktop-first Rust geospatial dashboard visualizing global news-attention
and unrest/event signals. Civic-data research/visualization only.
**M0–M6 complete 2026-07-18; V1 visualization batch complete 2026-08-10;
IODA (internet-outage) live source added 2026-08-11; Bluesky Jetstream and
Telegram aggregate-chatter sources added 2026-08-12; V2 and V3 visualization
batches complete 2026-08-12** — M5 (ACLED + NOAA)
fully live-verified; M6 shipped repo hygiene (CI feature matrix, `docker
compose` smoke test, cargo-deny, Dependabot, tag-driven releases,
CHANGELOG, portfolio README, CONTRIBUTING.md); V1 shipped the timeline
histogram, spike halos, severity markers, and recency fade
(docs/VISUALIZATION.md); IODA added a fourth optional live source
(`ioda-live`, keyless, country-precision internet-outage severity signal).
Telegram is the third real-time chatter source (after Bluesky), reading a
small live-verified public-channel allowlist over MTProto — credential-gated
like ACLED, and needs a one-time interactive login
(`crates/source-telegram/examples/login_setup.rs`) before it activates.
Branch protection on `main` is the one M6 item left, and it's a manual
GitHub-settings step (no authenticated `gh`/API access from this machine)
— see HANDOFF.md. V3 shipped per-source marker identity (shape = source,
color = kind), a NOAA weather-alert overlay, a full painted legend, an
offline graticule/labels/border-hierarchy orientation pass, and the
"how to read this map" overlay. **M7 service hardening complete 2026-08-12;
the Daily Events AI digest shipped 2026-08-13** — `crates/daily-digest` plus
a dedicated desktop page, one model-written summary per UTC calendar day,
cached locally, credential-gated on `GEMINI_API_KEY` (moved off the paid
Anthropic API to Google Gemini's free tier 2026-08-13 — a provider swap only;
the two-section separation, cadence, caching, and page are unchanged). It is
the project's only *interpretive* surface and its only outbound flow of stored
records — and the free tier **may train on what is sent**, so read
docs/SAFETY_AND_PRIVACY.md's hard rule 7 and its "Third-party processing"
section before changing anything there. See
[HANDOFF.md](HANDOFF.md) for status and the next task list,
and [docs/PLAN.md](docs/PLAN.md) for the approved plan.

## Commands

```sh
cargo run -p global-signal-desktop                     # run live-only desktop (live sources default on)
cargo test --workspace                                 # all tests, headless, no GPU
cargo fmt --all --check                                # gate 1
cargo clippy --workspace --all-targets -- -D warnings  # gate 2
cargo run -p source-fixtures --bin generate-fixtures   # regenerate fixtures (deterministic; commit result)
cargo test -p global-signal-desktop --test pipeline    # E2E acceptance test
cargo run -p workers                                   # M4 ingest worker (publishes Parquet snapshots)
cargo run -p api                                       # M4 read API (needs LES_PUBLISH_DIR)
docker compose up                                      # M4 worker + api stack (WSL2 on Windows)
cargo test -p source-acled --features live             # M5 ACLED mock-server tests
cargo run -p global-signal-desktop                     # ACLED + NOAA + IODA + Bluesky + Telegram are desktop defaults
cargo run -p workers --features acled-live,noaa-live,ioda-live,bluesky-live,telegram-live  # worker with all live sources
cargo run -p source-bluesky --features live --example live_probe -- 60  # manual live firehose check (aggregate output only)
cargo run -p source-telegram --features live --example login_setup      # one-time interactive Telegram login
cargo test -p daily-digest --features live             # Daily Events Gemini mock-server tests
cargo deny check                                       # M6 gate: advisories + license allowlist
```

Live sources are cargo features on both binaries: `acled-live` (needs
`ACLED_EMAIL`/`ACLED_PASSWORD` — myACLED OAuth; ACLED retired API keys),
`noaa-live` (keyless), `ioda-live` (keyless), `bluesky-live` (keyless), and
`telegram-live` (needs `TELEGRAM_API_ID`/`TELEGRAM_API_HASH` plus a one-time
interactive `login_setup` run — see `crates/source-telegram`). All five are
desktop default features; the worker keeps them opt-in. Clippy the feature
matrix when touching ingest loops.

`gemini-live` (needs `GEMINI_API_KEY`) is a **desktop-only** feature
and does not follow that shape: `daily-digest` is not optional, because the
page, the cache table, and the day picker are built from its types either
way — the feature gates only the network half, so a build without it still
reads every cached digest. In CI's feature matrix it is therefore
package-qualified (`global-signal-desktop/gemini-live`); the other
entries leave it off, which is what compiles the stub `api` module in
`apps/global-signal-desktop/src/digest.rs`.

M4 services env: worker reads `LES_WORKER_DATA_DIR` (its own DuckDB),
`LES_PUBLISH_DIR` (snapshot root), `LES_FIXTURES_DIR`, `LES_RETENTION_DAYS`,
`LES_PUBLISH_KEEP_LAST`, `LES_ONLINE` (defaults **on**; `0` = fixtures only).
api reads `LES_PUBLISH_DIR` + `LES_API_BIND`. Never point the api at a
`.duckdb` file or share the worker's DB — Parquet snapshots are the only
handoff (docs/API.md).

Run all three gates after every change. First cold build compiles bundled
DuckDB C++ (several minutes) — never `cargo clean` casually.

## Hard project rules (from the brief; non-negotiable)

- Public/authorized data sources only; no scraping restricted sources, no
  bypassing paywalls/auth/rate limits. Live APIs land only in their
  milestone (GDELT M3; ACLED + NOAA M5; IODA 2026-08-11; Bluesky and
  Telegram 2026-08-12 — all feature-gated, credentials via env vars only
  where credentials exist).
  ACLED data is never redistributed — `notes` never stored, ACLED-bearing
  snapshots never served publicly.
- No person-level identification/tracking/targeting features. Aggregate
  signals only (H3 cells, countries). For **streaming/social sources**
  (Bluesky, Telegram) this is enforced by construction: never
  store a post/message, author handle/DID/user id, its text, or its URL —
  not in the DB, not in a log, not transiently. Match text as it streams,
  increment a counter, drop the text in the same call; persist only the
  `(place, topic, window) -> count` rollup. Place attribution is keyword
  matching against a real gazetteer, never NLP location inference. This
  was a deliberate hold-the-line decision, not a default — read
  docs/SAFETY_AND_PRIVACY.md hard rule 6 before changing anything here.
- Store headline/URL/outlet-domain **metadata only**, never article bodies.
- "Media attention" and "event data" are computed and displayed
  **separately**; score components are always shown, never only the
  combined number. Media attention ≠ ground truth.
- One milestone at a time; synthetic fixtures remain a permanent headless
  regression harness but must never enter the desktop runtime database/map.
- API keys in env vars only; `.gitignore` covers `.env` and databases.

## Architecture in 30 seconds

Cargo workspace, edition 2024, all dep versions pinned in the **root**
`Cargo.toml` (members use `dep.workspace = true`).

- `crates/core-types` — domain types, `SignalSource` trait, shared constants
  (`H3_RESOLUTION = 3`, `BUCKET_SECS = 6h`, FNV-1a event ids). No I/O.
- `crates/geo-utils` — equirectangular viewport (affine in lon/lat), H3
  assignment (range-validates before h3o), antimeridian-normalized
  boundaries, country point-in-polygon. egui-free.
- `crates/source-fixtures` — fixture reader + deterministic generator
  (SplitMix64, fixed anchor 2026-07-01). Normalization is fallible **per
  record**; failures go to `ingest_log`, never dropped.
- `crates/analytics` — pure functions; `score_buckets` is the single
  scoring/aggregation implementation storage persists (no SQL twin);
  `scoring.rs`/`baseline.rs` hold the M2 component functions + medians;
  every constant is named in `analytics::weights`.
- `crates/storage` — DuckDB behind a dedicated **actor thread** (the
  connection is `!Sync`); versioned `.sql` migrations in `migrations/`;
  `Reply<T>` handles polled by the UI per frame; rusqlite settings store.
  DuckDB is **single-writer per file** — M4 hands off via Parquet.
- `crates/renderer` — egui **layer library**, not a wgpu engine: geometry
  tessellated once in lon/lat (`GeoMesh`), screen meshes rebuilt only on
  viewport change (affine mul-add per vertex), world-copy offsets for ±180°.
  Never add per-frame path tessellation. V3 added `glyph.rs`
  (`MarkerGlyph` — marker **shape encodes the source**, color still encodes
  `EventKind`; the unit polygons are equal-**area** so shape never leaks into
  the severity-size channel), `alerts.rs` (`AlertLayer` — NOAA weather alerts
  as a cool severity tint inside a dashed outline whose dash length comes from
  each ring's *screen* perimeter, giving a fixed dash count at any zoom), and
  `graticule.rs` (meridians/parallels; affine projection makes each one a
  single screen-aligned segment, spacing adapts to zoom, only in-viewport
  lines are generated). `BasemapLayer::paint` takes an `emphasis` ISO-A3 for
  the border hierarchy and resolves codes exactly as `geo_utils::CountryIndex`
  does — a test pins the agreement, because a mismatch fails silently.
- `crates/source-gdelt` — M3 live GDELT: `doc` (DOC 2.0 artlist JSON →
  country-precision attention), `events` (15-min Events CSV-zip dumps → CAMEO
  discrete events), `country` (name/FIPS → ISO-A3 + centroid), `sched`
  (governor rate limiter + backoff + cadence/backfill). Keyless; parse/
  normalize pure and offline golden-tested, only `fetch*` touch the network.
- `apps/global-signal-desktop` — eframe 0.35 shell; state machine in
  `app.rs`, map widget in `map_view.rs`, panels in `panels.rs`, custom-painted
  widgets in `timeline_strip.rs` (V1) and `sparkline.rs` (V2). V3 added
  `style.rs` (UI constants + **painted** legend swatches — egui's bundled
  fonts have no `◆`/`●`/`■` glyphs, so those rendered as missing-glyph boxes;
  swatches now draw from `MarkerGlyph::unit_corners` so the legend cannot
  drift from the map) and `how_to_read.rs` (the first-run / `?`-key reading
  guide; its copy is structured data because `RichText` renders markdown
  markup literally). `MapView::show` takes a `MapInputs` struct, not a row of
  positional bools. `ingest.rs`
  is a long-lived, live-only GDELT/ACLED/NOAA worker. `App.page` (`Page::Map`
  / `Page::DailyEvents`) switches whole pages rather than adding a panel:
  `daily_events.rs` draws the digest page (day picker + the two headed
  sections), `digest.rs` is its worker — same stub-module pattern as the
  live sources, but with **no cadence of its own**, since generating spends a
  metered API call and ships stored records to a third party — only an
  explicit click fires one. The worker never
  touches storage; the UI thread owns it, as everywhere else. Startup purges legacy
  `source=fixtures` rows and treats an empty database as valid.
  UI thread never blocks on storage; it ingests worker batches (dedup makes
  re-fetch idempotent). `MapView::fly_to` is the only thing in the map that
  requests repaints, and it is bounded — it settles and stops.
- `services/workers` — M4 ingest worker binary: owns its own DuckDB, ingests
  fixtures + live GDELT (same `source-gdelt` loop as the desktop), and calls
  `StorageHandle::publish_snapshot` after every cycle.
- `services/api` — M4 axum read API over the worker's published Parquet
  snapshots (`/health` `/meta` `/buckets` `/events`); ephemeral in-memory
  DuckDB `read_parquet` per request, never a `.duckdb` file (docs/API.md).
- `crates/source-acled` — M5 live ACLED: OAuth password/refresh grants
  (`live.rs`, feature `live`), paged windowed reads, pure `normalize_event`
  (never stores `notes`), full ISO-3166 numeric→alpha3 table (`iso3.rs`).
  Mock-server tests: `cargo test -p source-acled --features live`.
- `crates/source-noaa` — M5 NOAA/NWS active alerts (keyless, feature
  `live`): polygon alerts → `Disruption` at polygon centroid, Admin1
  precision; zone-only alerts yield zero events by design (never guess
  coordinates). US coverage only.
- `crates/source-ioda` — IODA internet-outage events (keyless, feature
  `live`, added 2026-08-11): country-precision `Disruption` events from
  `/outages/events`; `severity_from_score` log-scales IODA's unbounded
  `score` onto [0,1] (`weights::IODA_SCORE_FLOOR`/`IODA_SCORE_CEIL`);
  country centroid via `geo_utils::CountryIndex::centroid_by_iso_a2`
  (bundled Natural Earth data, never a hand-typed coordinate table).
- `crates/chatter` — aggregate-before-storage machinery for streaming
  social sources (added 2026-08-12). Place/topic word-window matching over
  bundled Natural Earth gazetteers, an in-memory `ChatterAccumulator`, and
  `normalize_rollup`. **This crate is a privacy boundary**: `observe` takes
  only `(&str, ts)` so author identity cannot be passed in, and the only
  output is a `(place, topic, window) -> count` rollup. Never add an API
  here that accepts or returns post text or identity — SAFETY_AND_PRIVACY
  hard rule 6, and read it before touching this crate.
- `crates/source-bluesky` — Bluesky Jetstream (keyless, feature `live`,
  added 2026-08-12). The only **streaming** source: a long-lived WebSocket
  task counts into a shared accumulator and `fetch` drains **completed
  windows only** (a half-counted window would claim its `source_event_id`
  and lose the remainder to dedup-by-id). Uses the message's `time_us`, not
  client-supplied `createdAt`; no cursor on reconnect (replay would
  double-count — undercounting is the honest failure). `live_probe` example
  checks it against the real firehose, printing aggregates only.
- `crates/source-telegram` — Telegram public channels (credential-gated,
  feature `live`, added 2026-08-12). MTProto via `grammers-client` (pure
  Rust, no TDLib/C++), the only mechanism that can read a third-party public
  channel's history without that channel's owner cooperating (a bot token
  cannot — Telegram only delivers channel messages to a bot the channel's
  own admin added). Poll-based like NOAA/IODA, not streamed: each cycle
  sweeps `ALLOWED_CHANNELS` (a small, live-verified, curated allowlist —
  excluded candidates documented by name and reason alongside it) and
  advances a per-channel in-memory high-water mark. Login is a one-time
  interactive step (`examples/login_setup.rs`, phone number + SMS code)
  that saves a local JSON session file; the real source only ever opens
  that file, never logs in itself — a missing/unauthorized session surfaces
  as a `fetch` error naming the setup command. Reuses `chatter` unchanged.
- `crates/daily-digest` — the Daily Events digest (added 2026-08-13).
  Deliberately **not** named `source-*`: that prefix means "implements
  `SignalSource`, yields `GeoTemporalEvent`s", and this crate ingests
  nothing — it reads what storage already holds and writes prose about it.
  Everything except `live.rs` (feature `live`) is pure and offline-tested:
  `DigestFacts` (what storage fills in), `render_facts`, `request_body`,
  `parse_response`, `DayKey` (UTC calendar day ↔ storage key ↔ epoch
  window). Three rules are structural, not prompt-dependent:
  `output_schema()` has exactly two properties with
  `additionalProperties: false` so attention and event data cannot blend;
  `row_level_permitted(SourceId)` withholds ACLED and chatter row text from
  the request (they contribute counts only — applied in `crates/storage`,
  the single place row content is selected); and `DigestFacts` has no field
  that could carry an author, handle, or message text. `tests/live_mock.rs`
  runs the whole live path against a local socket — no key needed.
  Gemini gotchas (provider swapped 2026-08-13): the schema must be sent as
  `generationConfig.responseJsonSchema` — **not** `responseSchema`, which
  takes the OpenAPI-3.0 subset, has no `additionalProperties`, and would drop
  the separation wall silently; `responseMimeType: "application/json"` is what
  engages constrained decoding. The model id travels in the **URL**
  (`…/models/{MODEL}:generateContent`), never the body; unknown
  `generationConfig` keys are a 400 naming the field, so typos fail loudly. A
  bad key is an ordinary **400 `INVALID_ARGUMENT`**, not a 401/403 — the
  credential hint comes from `error.details[].reason == "API_KEY_INVALID"`.
  429s usually carry no `Retry-After`; the delay is a `RetryInfo` detail
  (`"41s"`). Thinking is on by default and `thinkingLevel: "low"` measurably
  zeroes `thoughtsTokenCount`; thought parts are flagged `thought: true` while
  the *answer* part carries a `thoughtSignature`, so `parse_response` filters
  on the flag, not on some thinking-shaped field. Blocks arrive as **HTTP
  200** in two shapes: `promptFeedback.blockReason` with no candidates, or a
  candidate whose `finishReason` is not `STOP` — check both before indexing
  `parts[0]`. Model ids expire: `gemini-2.5-flash` now 404s with "no longer
  available to new users", so a 404 on generate is the first thing to suspect.

Precision rendering contract: only City/Exact records render as point
markers; Country/Admin1 shade regions (enforced in the storage query).

## Version gotchas (verified against installed sources)

- egui 0.35: `App::ui(&mut self, ui, frame)` root-Ui trait; unified
  `egui::Panel::top/bottom/right`; `smooth_scroll_delta()`; `Frame::NONE`;
  `rect_stroke` needs `StrokeKind`. eframe 0.35 = wgpu 29 (do not bump wgpu).
- geojson 1.0: struct variants + `Position` newtype.
- duckdb `1.10504.0` = DuckDB 1.5.4 (`1.MMmmpp.x` scheme).
- M3 deps: reqwest 0.12 (**rustls-tls, no default TLS/http2** — keeps CI
  OpenSSL-free); `zip` 6 with **`deflate-flate2` + a direct `flate2` dep** so
  the DEFLATE backend (miniz_oxide) is actually selected; `governor` 0.10
  (`FakeRelativeClock` for deterministic limiter tests). tokio gained `net`
  for the worker's IO driver.
- M4 deps: `axum` 0.8 (services/api only). The api uses `spawn_blocking` for
  every DuckDB call (the connection is `!Sync` and blocking); each request
  opens a throwaway in-memory connection — no shared connection, no cache.
  Docker builds need `cmake` in the builder image (bundled DuckDB C++).
- Bluesky deps: `tokio-tungstenite` 0.30 (`default-features = false`,
  features `connect` + `rustls-tls-webpki-roots` — same rustls stack as
  reqwest, no OpenSSL) plus `futures-util`. **Also a direct `rustls` dep
  with the `ring` feature**: tokio-tungstenite's rustls feature pulls
  rustls but selects *no* crypto provider, and rustls 0.23 panics on the
  first handshake if it cannot infer one. `source-bluesky`'s graph has no
  reqwest to supply it, so `live.rs` installs `ring` explicitly rather than
  relying on cross-crate feature unification (which silently works in the
  desktop binary and fails in a standalone example/test).
- Telegram deps: `grammers-client`/`grammers-session`/`grammers-mtsender`
  0.10 (pure Rust MTProto, no TDLib/C++). **`grammers-session` is pinned
  `default-features = false` in the root manifest** — its default
  `sqlite-storage` pulls `libsql-ffi`, a *second* vendored static SQLite
  next to `rusqlite`/`libsqlite3-sys`, and linking both into one binary
  fails with duplicate `sqlite3_*` symbols (LNK2005). `cargo check`/`clippy`
  never link, so only a real `cargo build` catches that class of bug. The
  dropped storage is replaced by `source-telegram`'s own
  `file_session::FileSession` (JSON file; `SessionData` has public fields
  but no serde derives, hence the mirror struct). Its `serde` feature is on,
  which is what adds `serde_with`. `Message::date()` returns
  plain `chrono::DateTime<Utc>` in the published 0.10.0, **not**
  `jiff::Timestamp`: the crate's `master` branch had already migrated to
  `jiff` when this was researched, one release ahead of what crates.io
  actually serves. Caught by the compiler, not by re-reading source — a live
  reminder for the next bullet.
- reqwest is pinned `default-features = false, features = ["rustls-tls"]`
  workspace-wide, which **excludes the `json` feature** — `RequestBuilder::
  json()` and `Response::json()` do not exist here. `daily-digest::live`
  therefore builds bodies with `serde_json::to_vec` + `.body(...)` and reads
  them with `.text()` + `serde_json::from_str`. Adding the `json` feature to
  get one convenience method would pull it into every source crate; don't.
- When an API surprises you, read the crate source in
  `~/.cargo/registry/src/index.crates.io-*/<crate>/` before guessing — and
  prefer it over a repo's `master` branch, which can be ahead of what Cargo
  actually resolved (see the grammers/jiff note above).

## Conventions

- Small PR-sized commits; commit after each step once gates pass.
- Tests colocated in each crate; hand-computed golden tests for anything
  the brief calls "transparent" (scoring).
- Comments state constraints the code can't show (threading, contracts,
  version locks) — not narration.
- All synthetic content stays obviously synthetic: `[synthetic]` headline
  prefix, `.example` outlet domains. Never imitate real publications.
