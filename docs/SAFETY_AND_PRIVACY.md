# Safety and privacy

This is a civic-data research, visualization, and future voluntary field-
publishing project. It aggregates public or properly authorized signals about
media attention and reported events. It may support real-time channels that an
on-scene journalist or contributor deliberately publishes. It is **not** a
covert surveillance, involuntary tracking, or targeting tool.

## Hard rules

1. **Aggregate by default; publishing requires consent.** Existing source
   signals are keyed to regions (H3 cells, countries) and times, people.
   A future field channel represents a publisher's explicit choice to share;
   it does authorize face recognition, involuntary identity search,
   profiling, or persistent location tracking.
2. **Minimize content and location.** Existing feeds store headlines, URLs,
   and outlet domains—not full article text—unless the source license allows
   it. Future channel media is stored or relayed only under an explicit
   publisher agreement. Exact capture coordinates are stripped or withheld by
   default, and the publisher controls precision, delay, retention, and access.
3. **Public or authorized sources only.** No scraping of restricted sources;
   no bypassing paywalls, authentication, rate limits, or anti-bot systems.
   Rate limits are enforced client-side per adapter.
4. **No guaranteed-ground-truth label.** Media attention is an imperfect,
   biased proxy, provider records may be corrected, and authenticated
   eyewitnesses can still be mistaken or impersonated. The UI separates
   evidence classes and exposes provenance, corroboration, disputes, and
   corrections instead of making an absolute truth claim.
   The current UI separates "media attention" from "event data," shows score
   components individually, and badges low-confidence values. The combined
   number is never shown without its parts.
5. **Secrets stay out of git.** API keys live in environment variables or
   `.env` files covered by `.gitignore`.
6. **Streaming/social sources are aggregate-only, by construction.** Posts
   and channel messages about live unrest are frequently written by the
   protesters, journalists, and dissidents inside those events, for whom
   being identified can be dangerous — and a tool that geolocates
   individuals against unrest data is the shape of thing historically used
   to find exactly those people. So for Bluesky, Telegram (and any future
   social source): **never store an individual post or message**, its
   author handle/DID/user id, its text, or its URL — not in the database,
   not in a log, not transiently. Text is matched as it streams or is
   polled past, an in-memory counter is incremented, and the text is
   dropped inside the same call. Only a `(place, topic, time window) ->
   count` rollup is ever persisted. Place attribution is crude keyword
   matching against a real gazetteer, **never** NLP location inference from
   content; a post that matches nothing contributes to no aggregate rather
   than being placed somewhere plausible. The `chatter` crate is the
   enforcement point: its `observe` takes only text and a timestamp, so
   author identity cannot be passed in even by mistake, and its only output
   type is a count. This constraint is not a default to revisit when
   convenient — it is the condition under which these sources are allowed
   to exist here at all.
   For Telegram specifically, this also shapes *which* channels are
   readable at all: reading a public channel's history via a real account
   (MTProto) is the only mechanism that works without that channel owner's
   cooperation (a bot token can only read channels its own owner explicitly
   added it to), and channel selection is a small **curated allowlist**
   (`source-telegram::ALLOWED_CHANNELS`), not open crawling — every entry
   was live-verified (real, active, on-topic) before being added, and
   channels found during research but excluded (a combatant's own channel,
   several self-described partisan/"alternative narrative" accounts, one
   channel dead since 2018) are documented by name and reason right next to
   the allowlist, specifically so nobody re-adds one without knowing why it
   was passed over.

## Source licensing

| Source | Terms we rely on | Notes |
|---|---|---|
| GDELT | Free for use with attribution | Keyless public API/dumps; attribute in README and UI About. |
| ACLED | Registered authorization required (myACLED account) | Included in desktop defaults; OAuth password grant—ACLED retired API keys; credentials via `ACLED_EMAIL`/`ACLED_PASSWORD` env vars only. **No redistribution of raw ACLED data**: the `notes` narrative is never stored, only structural metadata, and worker snapshots containing ACLED rows are for local/authorized use—never served publicly. Attribution: "Armed Conflict Location & Event Data Project (ACLED); acleddata.com" (UI status panel + README). ACLED *corrections* reuse event ids and are not re-applied by dedup (documented limitation). |
| NOAA/NWS active alerts | US-government public domain | Included in desktop defaults; keyless; descriptive `User-Agent` per api.weather.gov policy. **US + territories coverage only**—a documented coverage bias, not a global weather layer. Zone-scoped alerts without polygon geometry yield no events (we never guess coordinates). |
| IODA (Internet Outage Detection and Analysis) | Public API, © Georgia Tech Research Corporation | Included in desktop defaults; keyless, no stated rate limit (polled politely regardless). Country-precision only—no finer geometry is available, so events shade regions and never render as point markers. Aggregate network telemetry only, no person-level data of any kind. |
| Bluesky Jetstream | Public firehose of public posts, keyless | Included in desktop defaults. **Aggregate chatter volume only** (hard rule 6): counts of posts mentioning a known place and a known topic in a 5-minute window. No post text, author DID/handle, post id, or URL is stored anywhere, and the source exposes no API that returns them. Server-side filtered to `app.bsky.feed.post`; no cursor on reconnect, so a disconnect undercounts rather than double-counts. Chatter is a **media-attention** signal, never a discrete event record, and place matching is keyword-based with a documented false-positive rate. |
| Telegram (public channels) | Public channel content, read via a dedicated account's own MTProto session (not a bot token — Telegram's Bot API cannot read channels it wasn't added to) | Included in desktop defaults, but inert until a one-time interactive login is run (`crates/source-telegram/examples/login_setup.rs`); credential-gated like ACLED. **Aggregate chatter volume only** (hard rule 6), same shape as Bluesky. Reads only a small **curated allowlist** of 8 channels (`source-telegram::ALLOWED_CHANNELS`), each live-verified before inclusion — never open crawling, never a channel the account wasn't already free to read as a public channel. Excluded candidates are documented by name and reason alongside the allowlist. |
| Natural Earth | Public domain | Attributed anyway (basemap credit); supplies both the country polygons and the 1:110m populated-places gazetteer used for chatter place matching. |
| OSM tiles (M3+, optional) | OSM tile usage policy | Documented before the tile layer lands; offline mode never touches them. |
| Fixtures | Fully synthetic | Reserved `.example` outlet domains; imitates schemas, not publications. |

## Known biases (documented, surfaced in UI)

- **Coverage bias**: media density varies enormously by language and region;
  attention scores skew toward well-covered places. This is why
  `attention_score` and `unrest_score` are separate components.
- **Geocoding bias**: sources frequently geocode to country/admin centroids.
  The precision rendering contract (see DATA_MODEL.md) prevents centroid
  records from appearing as false point hotspots. In particular, GDELT **DOC**
  attention is geocoded only to the *source country* (the publisher's country,
  not the event's), so it is always emitted at country precision and shades
  regions only — never a point. GDELT **Events** rows carry real coordinates
  and render per the contract.
- **Event taxonomies differ** between sources; `EventKind` is a coarse
  mapping, and per-source provenance is always preserved.

## Data retention

- Events table (M3+): a configurable retention window prunes events older than
  *N* days from the newest event on each ingest (UI menu / `LES_RETENTION_DAYS`;
  default keep-all). A window ≥ the 28-day baseline keeps recent
  baselines warm. We store only normalized event metadata — never raw GDELT
  dumps or article bodies.
- Derived metrics may be kept indefinitely. Synthetic fixtures are test-only
  and never enter the desktop database.
- Session Parquet exports are local files created explicitly by the user.
- Future field-channel retention must be explicit, visible to the publisher,
  and independently configurable from normalized event retention. Deleting a
  channel must also revoke ordinary playback access to its retained media.

## Misuse review

New features are checked against: does this enable identifying, targeting,
harassing, or locating someone without their consent? Could exact location,
true-live timing, bystanders, or retained media endanger the publisher or
others? Does the design give viewers more disclosure power than the publisher?
If yes, it does not ship in that form. Voluntary journalism and field
publishing are in scope; covert tracking and tactical targeting are not. This
document is the place to record judgment calls.
