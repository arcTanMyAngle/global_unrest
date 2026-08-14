# In-flight handoff — video playback + wider source coverage

**Status: media search + in-app player are DONE and live-verified. Telegram
media leg, docs, and Task 2 (coverage widening) remain.**
Fold this into `HANDOFF.md` once the work lands, then delete this file.

Updated 2026-08-13 (second session). Branch `main`, HEAD `65ef438 model swap
complete and more`. Nothing is committed — the user's standing constraint is
**"I do my own commits — don't commit or push."** Honour it.

---

## 1. What the user asked for

**Task 1 — video playback in the app.** The map had a `🎥 has video` filter but
nothing played. Open an event (e.g. a Colombia earthquake) and watch the video
without leaving the app. → **Built. See §4.**

**Task 2 — far more source coverage.** Telegram's `ALLOWED_CHANNELS` allowlist
and Bluesky's gazetteer/topic matching are both too narrow; widen per-country
coverage. Every added Telegram channel must be live-verified public and
documented, with excluded candidates named and reasoned, in the existing style.
Coverage widens *counts*, never what is stored: `crates/chatter`'s
`(place, topic, window) -> count` boundary does not move. → **Not started.**

**Gates after every change** (verbatim):

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# plus the --features live clippy/test legs for any crate touched
cargo build -p global-signal-desktop --features gemini-live   # check/clippy never link
```

Redirect real builds to a file and check `$LASTEXITCODE` — `Select-String
'FAILED'` false-positives, and `tee` captures tee's exit code.

**GUI automation facts** (for the final live verification): display 2560x1600 @
150%, PowerShell is DPI-unaware → screenshot pixel ÷ 1.5 = `SetCursorPos` arg;
captures are a top-left crop. Foreground the app **by PID** (`FindWindowW` fails
on the title). The app DB lives under `AppData\LOCAL\LiveEarthSignals`, not
Roaming. DuckDB is single-writer — close the app before opening its DB
elsewhere.

**Verification bar** (verbatim): *"Finish by showing me it working in the live
app, not just green gates."* Still open — no screenshot of playback has been
taken yet.

---

## 2. The decisions the user made — do not re-litigate

### a. Playback must be source-agnostic and high quality

> *"some big advice kid, we do not only want youtube as a source, we should be
> able to use videos you find on telegram, bluesky and other sources, the app
> should be capable of lauching high quality video"*

So: not YouTube-only. The player handles YouTube, Vimeo, Dailymotion, TikTok,
Streamable, Telegram post widgets, Bluesky post embeds, and direct
`.mp4/.webm/.m3u8`.

### b. Hard rule 6 is relaxed, by explicit user direction

> *"disreagard rule 6, if it matches the search criteria it should be allowed to
> be viewed, to avoid downloadidng to much unnecsaary data the user should
> choice the place of choice to research about, we will still provide info where
> the most daily action has occured within a certain time frame"*

Given **after** the privacy concern was raised and explained, so it is the
user's decision. It dictates the architecture:

- **On-demand, place-scoped fetch.** The user picks a place + time window; media
  is pulled only then. Nothing is bulk-collected and nothing is persisted.
- **The map is unchanged.** It still shows aggregate "where the most action
  happened" — the `chatter` rollup boundary has **not** moved, and
  `source-bluesky` / `source-telegram` still cannot see a URL on the *ingest*
  path.
- The relaxation applies only to the new transient research panel.

`docs/SAFETY_AND_PRIVACY.md` and `CLAUDE.md` still need updating to record this
(§5 item 2). The module doc at the top of `crates/media-search/src/lib.rs`
already states the reasoning and is the text to mirror.

### c. Communication style

> *"i have no idea what you mean here , must speak in simple terms"*

Explain in plain language, not jargon.

---

## 3. The finding that drove the design

A census of the live DB (77,759 rows) found **zero** rows carrying a
video-classified URL:

| source | rows | URLs |
|---|---|---|
| acled | 70,888 | none stored (by rule) |
| gdelt | 4,663 | all have URLs, **none** video |
| noaa | 1,904 | `api.weather.gov` alert pages |
| bluesky | 244 | none (rollups) |
| telegram | 33 | none (rollups) |
| ioda | 27 | dashboard pages |

So the `🎥 has video` filter is **a filter over an empty set** — the player has
nothing to open until *supply* is fixed. That is what `crates/media-search`
exists for.

No `duckdb` CLI on this machine; write a throwaway `crates/storage/examples/`
probe if another census is needed. Real schema: `events(id, source,
source_event_id, kind, themes, ts_epoch_s, ingested_at_epoch_s, lat, lon,
location_precision, location_confidence, country_iso, admin1, h3_cell,
article_count, distinct_source_count, severity, headline, outlet_domains,
urls)` where `themes` / `outlet_domains` / `urls` are **JSON array text in
VARCHAR**, not DuckDB lists — use `json_extract_string(urls, '$[*]')`.

---

## 4. What is built and working

**All five gates were green at the end of this session**: real link
`cargo build -p global-signal-desktop` (EXIT=0), `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test
--workspace`, and `media-search --features live` clippy + test.

### `crates/core-types/src/media.rs`
`Embed { Page(String), File(String) }` and `embed_for(&str) -> Option<Embed>` —
the pure URL → playback mapping, using each provider's *published embed
endpoint* (`youtube-nocookie.com/embed/…`, `player.vimeo.com`, `t.me/…?embed=1`,
`embed.bsky.app`) rather than resolving a watch page to its stream, which would
be scraping. `None` = hand it to the OS browser.
*Bug already fixed, do not reintroduce*: the playable-extension check must stay
**above** the per-host arms, or `video.bsky.app/…/playlist.m3u8` falls into the
Bluesky arm and returns `None`.

### `crates/media-search` (new crate, in the workspace)
- `lib.rs` — `Provider {Gdelt, Bluesky, Telegram}`, `MediaHit`, `MediaQuery`,
  `search_terms` (strips user text to bare words so it cannot carry a query
  operator into any provider), `short_title`, `merge` (newest-first, dedup by
  URL across providers, stable sort so attribution order survives ties).
- `gdelt.rs` — `VIDEO_DOMAINS` (8 hosts), `query_expression`, `request_url`,
  `hits`, `parse_seendate`.
- `bluesky.rs` — keyless `searchPosts`; keeps only posts carrying video;
  `BLOCKED_LABELS` drops platform-labelled adult content (see §6).
- `live.rs` (feature `live`) — `MediaSearch` with one method per provider plus
  `search()` which merges and reports per-provider failures separately.
- `tests/live_mock.rs` — 7 tests against a local socket, no network.
- `examples/live_probe.rs` — manual real-API check:
  `cargo run -p media-search --features live --example live_probe -- Colombia - 72`
  (`-` = no topic; PowerShell drops empty `""` args).

### `apps/global-signal-desktop`
- `video.rs` — embedded player. `PlaybackRequest` (pure) plus `VideoPlayer` with
  two impls selected by `#[cfg(all(target_os = "windows", feature =
  "video-embed"))]`; the real one owns an `Option<wry::WebView>` built with
  `build_as_child(&frame.window_handle()?)` and repositioned per frame in
  **physical** pixels.
- `media.rs` — the media worker (no cadence; one search at a time; never touches
  storage). Stub-module pattern behind `media-live`, same as `digest.rs`.
- `media_page.rs` — the Media page: busiest-places shortcuts, place + topic +
  window picker, results split into "news video" / "public posts", player pane.
- `app.rs` / `main.rs` / `panels.rs` — `Page::Media` wired in.

**Airspace caveat** (in `video.rs`'s module header): the webview is a native
child window. It paints *over* everything egui draws in its rect, ignores egui
z-order, cannot be tinted or clipped, and cannot be scrolled under. Lay the UI
out *around* the player rect and `hide()` it whenever an egui window opens —
`hide()` both blanks the page and hides the window, because a merely hidden
webview keeps playing audio.

### Live status
- **Bluesky leg: proven working live.** Real Colombia earthquake video posts
  returned, spam filtered.
- **GDELT leg: code-correct, blocked by GDELT's rate limit** on this IP. Latest
  probe returned a genuine `429` (previously it was a silent connect timeout).
  Re-probe after a cooldown.

---

## 5. Next steps, in order

### 1. Telegram media leg — the next file to write

Add `pub async fn search_media(&self, query: &MediaQuery) ->
Result<Vec<MediaHit>, SourceError>` to `source-telegram`'s `TelegramSource`.
`source-telegram` depends on `media-search` for the types; **media-search must
never depend on source-telegram**, or the graph cycles (this is already stated
in `media-search/src/live.rs`'s module doc).

Keep the pure half in a new `crates/source-telegram/src/media.rs` (URL building,
match test, `MediaHit` construction) so it is unit-testable without a session,
and put only the MTProto sweep in `live.rs`. Hit URL is
`https://t.me/{channel}/{id}` — `core_types::embed_for` already maps that to
`t.me/…?embed=1`.

**Verified grammers 0.10 API for this** (read from the installed source, since
the crate's `master` is ahead of crates.io — the same trap that bit `jiff`):

- `client.search_messages(peer) -> SearchIter` with builders `.query(&str)`,
  `.min_date(&DateTime<FixedOffset>)`, `.max_date(&DateTime<FixedOffset>)`,
  `.offset_id(i32)`, `.filter(tl::enums::MessagesFilter)`
  (`grammers-client-0.10.0/src/client/messages.rs:384–443`). Server-side search
  beats sweeping and re-filtering locally.
- `grammers_client::tl` re-exports `grammers_tl_types` (`src/lib.rs:101`), so
  the filter enum is reachable without adding a dependency. **The enum variants
  are code-generated into `OUT_DIR`, not present in the crate's `src/`** — grep
  won't find them; let the compiler confirm the exact variant name (expect
  something like `MessagesFilter::InputMessagesFilterVideo`).
- `Media` enum variants: `Photo, Document, Sticker, Contact, Poll, Geo, Dice,
  Venue, GeoLive, WebPage` (`src/media/media.rs:101`). `Document::mime_type() ->
  Option<&str>` (`:308`) and `Document::name() -> Option<&str>` (`:295`) are how
  you tell a video document from any other file.
- `Message::date()` returns `chrono::DateTime<Utc>` in the published 0.10.0.
  `min_date`/`max_date` want `&DateTime<FixedOffset>` — convert.

Also decide how the desktop reaches this leg: `MediaSearch` lives in
`media-search` and has no Telegram method, and Telegram is credential-gated. The
cleanest shape is for `apps/global-signal-desktop/src/media.rs`'s worker to own
an optional `TelegramSource` alongside `MediaSearch` and merge the two result
lists with `media_search::merge`, behind a `telegram-live`-gated arm of the
existing stub-module pattern.

### 2. Docs
`docs/SAFETY_AND_PRIVACY.md` — new "On-demand media lookup" section plus the
rule-6 relaxation, explicitly noting that `crates/chatter`'s boundary has **not**
moved. `CLAUDE.md` — the `media-search` crate, the `media-live` / `video-embed`
features, the Media page, the wry/airspace gotcha, `api.bsky.app` vs
`public.api.bsky.app`, and GDELT's SYN-dropping throttle.

### 3. Re-probe GDELT DOC live once the throttle clears
`cargo run -q -p media-search --features live --example live_probe -- Colombia - 72`

### 4. Task 2a — widen `source-telegram::ALLOWED_CHANNELS`
Currently 8: `liveuamap`, `ClashReport`, `osintdefender`, `osintsahel`,
`Osinttechnical`, `AMK_Mapping`, `borderlandbeat`, `DVBTV`. Live-verify each
addition via its public `t.me/s/<handle>` preview — **not** a search summary's
word — and document every exclusion by name and reason. Preserve the existing
exclusions: `globalconflictmonitor` (~74 subs, murky admin story), `RSFSudan`
(combatant's own channel), `southfronteng` / `intelslava` / `eurasianist` /
`BellumActaNews` / `rnintel` (self-described partisan), `middleeastobserver`
(dead since 2018), `GeoConfirmed` (preview returned no content). The test
`allowed_channels_are_unique_bare_usernames` pins the format.

### 5. Task 2b — widen `crates/chatter` coverage
Currently 9 topics, a Natural Earth 1:110m gazetteer of roughly one city per
country, and 17 country aliases. Widen places and topics **without moving the
rollup boundary** — `observe(&str, ts)` in, `(place, topic, window) -> count`
out, and no new API that accepts or returns post text or identity.

### 6. All gates
Including the `--features live` legs for every touched crate and one real
`cargo build -p global-signal-desktop --features gemini-live,video-embed`
redirected to a file with `$LASTEXITCODE` checked.

### 7. Live app run + screenshots proving playback works
The user's stated bar; green gates alone do not close the task.

---

## 6. Gotchas found (beyond what CLAUDE.md already records)

- **Bluesky: use `api.bsky.app`, not `public.api.bsky.app`.** The documented
  public host answers most keyless XRPC methods but returns a bot-block HTML
  `403` for `searchPosts` specifically — live-verified 2026-08-13 (`getProfile`
  on `public.api.bsky.app` = 200, `searchPosts` on the same host = 403,
  `searchPosts` on `api.bsky.app` = 200 unauthenticated). A routing quirk, not
  an auth requirement.
- **Bluesky results need moderation-label filtering.** Adult-content accounts
  hashtag-stuff country names; a live `colombia` search returned **21 labelled
  posts out of 50** and they crowded out the genuine footage.
  `bluesky::BLOCKED_LABELS` filters on the platform's own labels (post-level and
  author-level). `!no-unauthenticated` is a *visibility* label and must survive
  — blocking every label empties the panel.
- **GDELT throttles by dropping SYNs, not by answering 429.** Live-verified:
  `curl` got a 429 body ("Please limit requests to one every 5 seconds") while a
  request seconds later timed out during connect. `MediaSearch::get` has one
  connect-only retry (3 s) for exactly that, and `live.rs`'s `describe()`
  classifies the failure and keeps only the innermost cause — `reqwest`'s own
  `Display` inlines the whole percent-encoded query and reads identically for
  DNS, TLS, timeout, and drop failures.
- **`media-search` needs `tokio` as an optional *dependency*, not just a
  dev-dependency** — the connect retry calls `tokio::time::sleep`. Feature is
  `live = ["dep:reqwest", "dep:tokio"]`.
- **Rust raw strings**: a JSON fixture containing `"#hashtag"` closes an `r#"…"#`
  literal. Use `r##"…"##`.
- **PowerShell drops empty `""` arguments** to native exes — that is why
  `live_probe` uses `-` as its "no topic" sentinel.
- **`wry` 0.55.1 API, verified against the installed source**:
  `WebViewBuilder::new()` (872), `with_background_color` (933), `with_autoplay`
  (945), `with_url` (1191), `with_html` (1213), `with_incognito` (1359),
  `build_as_child<W: HasWindowHandle>` (1491), `load_url` (2117), `load_html`
  (2132), `set_bounds` (2149), `set_visible` (2154). `Rect { position:
  dpi::Position, size: dpi::Size }`. Default features `["protocol",
  "os-webview", "x11"]` are all dropped.
- **`eframe::Frame` implements `raw_window_handle::HasWindowHandle`**
  (`eframe-0.35.0/src/epi.rs:695`, rwh 0.6.2) — that is what `build_as_child`
  gets.
- **CI shape matters.** The `check` job matrix is `os: [windows-latest,
  ubuntu-latest]`, so any webview dep must stay under
  `[target.'cfg(windows)'.dependencies]`. The `feature-matrix` job is
  ubuntu-only.
- **Twitch has no usable embed** from an embedded webview (its embed requires a
  `parent=` matching the hosting page's real domain) — deliberately `None` from
  `embed_for`, falls back to the browser.
- **Rumble watch slugs** don't map to an embed id without an API lookup, so only
  already-`/embed/` Rumble URLs play inline.
- **Read the installed crate source**, not the project's GitHub `master` — the
  registry version is what Cargo resolved, and `master` can be a release ahead
  (this is how the grammers `jiff` vs `chrono` mismatch was caught).
