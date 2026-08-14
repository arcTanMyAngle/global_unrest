# Safety and privacy

This is a civic-data research, visualization, and future voluntary field-
publishing project. It aggregates public or properly authorized signals about
media attention and reported events. It may support real-time channels that an
on-scene journalist or contributor deliberately publishes. It is **not** a
covert surveillance, involuntary tracking, or targeting tool.

## Hard rules

1. **Aggregate by default; publishing requires consent.** Existing source
   signals are keyed to regions (H3 cells, countries) and times, not people.
   A future field channel represents a publisher's explicit choice to share;
   it does not authorize face recognition, involuntary identity search,
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
   matching against a real gazetteer, never NLP location inference from
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
7. **Generated text is labelled, bounded, and never the record.** The Daily
   Events page (`crates/daily-digest`) is the project's only *interpretive*
   surface: a language model writes prose about a day of stored signals. It
   is additive commentary on the data, never a substitute for it, and three
   things are enforced in code rather than asked of the prompt. (a) The
   response schema has exactly two properties — `media_attention` and
   `event_data` — with `additionalProperties: false`, so a blended
   significance judgement is not a shape the answer can take (hard rule 4,
   at the schema level). The provider enforces it server-side by constrained
   decoding, and it must be sent in the request field that actually does so
   (`generationConfig.responseJsonSchema`, **not** `responseSchema`, whose
   OpenAPI-3.0 subset has no `additionalProperties` and would drop the
   keyword silently); verified against the live API with a prompt explicitly
   ordering a third, blended field — the answer still came back with exactly
   two. (b) Row-level text is forwarded only for sources
   whose terms allow it (`row_level_permitted`); ACLED and the chatter
   sources reach the model as counts only. (c) Each section is displayed
   under its own heading with the number of records it was written from, above
   a provenance line naming the model and generation time. The page says in
   its own words — not the model's — that the summary adds no facts and is
   not a news report. A digest is regenerable and disposable; the stored
   records are the artifact.

## Third-party processing (Google Gemini API)

The daily digest is the only feature that sends stored signals *out* of the
machine, so it is worth being exact about what leaves and what does not.

**The free tier may train on what is sent.** Google's terms for the unpaid
tier of the Gemini API state that submitted prompts and generated responses
are used to improve their products, and that human reviewers may read them.
This is not a footnote to skim past — it is the reason the "what is sent"
list below is exhaustive and capped, and the reason nothing person-level can
reach it. Everything in that list is public metadata this project is already
permitted to republish (GDELT/NOAA/IODA fields and our own derived counts),
so training on it discloses nothing that is not already public. If that ever
stops being true of a field, the fix is to withhold the field, not to hope
the provider does not read it. Anyone who needs the data not to be trained on
should use a paid tier (where Google states it is not used for training) or
turn the feature off — with no key, the page reads the local cache and makes
no requests.

- **What is sent**: one request per generated day, containing the day's
  aggregate counts (records, articles, distinct outlet domains, per-country
  totals) plus a bounded sample of row-level fields — headline titles and
  outlet domains, event kind/source/label/severity — drawn only from GDELT,
  NOAA, and IODA. All of it is public metadata this project is permitted to
  republish, and all of it is capped (`MAX_PLACES`, `MAX_HEADLINES`,
  `MAX_NOTABLE`) so the request cannot grow into a bulk export.
- **What is never sent**: article bodies (never stored in the first place),
  ACLED rows, and anything person-level. The `DigestFacts` type has no field
  that can carry an author, handle, user id, or post/message text; the
  chatter sources discarded that text upstream before it was ever counted.
  A test asserts the withheld sources appear in the rendered prompt as
  counts and never as rows.
- **When**: only on an explicit click, once per UTC day, cached locally in
  DuckDB afterwards. Nothing is generated in the background, on startup, or
  on a timer. With no `GEMINI_API_KEY` (or with the `gemini-live`
  feature off) the page still reads every previously generated digest and
  makes no requests at all.
- **Credential**: `GEMINI_API_KEY`, env var only, like every other keyed
  source here (hard rule 5). It travels in the `x-goog-api-key` header, never
  in the `?key=` query parameter this API also accepts — query strings are the
  part of a URL that ends up in logs and proxies. Error messages name the
  variable and never echo its value — also a test.

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
  `attention_score` and `unrest_score` are separate components. The desktop's
  **attention ↔ unrest divergence layer** (docs/VISUALIZATION.md V2 item 5)
  makes this bias itself visible, and its legend says plainly that the map is
  a picture of our coverage rather than of the world. It deliberately claims
  *nothing* for a cell where one channel has no records at all — an absence
  can be under-reporting or can be a gap in what these sources index, and the
  data cannot tell those apart.
- **Geocoding bias**: sources frequently geocode to country/admin centroids.
  The precision rendering contract (see DATA_MODEL.md) prevents centroid
  records from appearing as false point hotspots. In particular, GDELT **DOC**
  attention is geocoded only to the *source country* (the publisher's country,
  not the event's), so it is always emitted at country precision and shades
  regions only — never a point. GDELT **Events** rows carry real coordinates
  and render per the contract.
- **Event taxonomies differ** between sources; `EventKind` is a coarse
  mapping, and per-source provenance is always preserved.
- **Generated summaries inherit every bias above, then add their own.** A
  digest describes what these sources happened to record; a quiet section
  means quiet *data*, not a quiet day. The model is instructed to use only
  the given facts and to say so when a day is thin, but instructions are not
  guarantees — a summary can still emphasize, flatten, or read causation into
  a coincidence. Two days' digests are also not strictly comparable: they are
  separate generations, not a measured series. Treat the prose as a reading
  aid over the counts, which are shown beside it precisely so the reader can
  check it.

## Data retention

- Events table (M3+): a configurable retention window prunes events older than
  *N* days from the newest event on each ingest (UI menu / `LES_RETENTION_DAYS`;
  default keep-all). A window ≥ the 28-day baseline keeps recent
  baselines warm. We store only normalized event metadata — never raw GDELT
  dumps or article bodies.
- Derived metrics may be kept indefinitely. Synthetic fixtures are test-only
  and never enter the desktop database.
- Session Parquet exports are local files created explicitly by the user.
- Daily digests are cached in a local `daily_digest` table, one row per UTC
  day, keyed by day and overwritten on regenerate. Rows hold generated prose
  plus the two record counts it was written from — no source rows, so a
  digest cannot become a back door around ACLED non-redistribution or around
  the chatter aggregate-only rule. Deleting a row costs nothing but a
  regeneration.
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

### Daily Events digest (2026-08-13)

Reviewed because it is the first *generative* layer here — everything before
it moved records around, this one writes about them — and the first feature
that sends stored signals to a third party.

- *Does it enable identifying, targeting, harassing, or locating someone?*
  No. Its inputs are country-level counts and public article/alert metadata;
  its output is prose about countries and datasets. The prompt forbids naming
  or describing individuals, and — more to the point — the facts type has no
  person-level field for it to name one from. The failure mode it could have
  had, "summarize who was posting from where," is unreachable because the
  chatter sources never kept that to summarize.
- *Does it weaken an existing rule?* Two were load-bearing here and both were
  moved into code rather than into the prompt: the attention/event split
  became the response schema, and ACLED non-redistribution became
  `row_level_permitted`, applied in `crates/storage` — the single place row
  content is selected for the request. A prompt-only version of either would
  have been a rule with no enforcement.
- *What is genuinely new risk?* Fluent prose is more persuasive than a
  choropleth, so an error here travels further than a mis-shaded cell. The
  mitigations are labelling (model + timestamp above the text, "not a news
  report" in the page's own voice), the record counts printed beside each
  section, and the fact that the map remains the primary surface — the digest
  is a separate page the user opts into, never a caption under the data.
- *Judgment call recorded*: the digest is allowed to describe **what the
  sources recorded**, not to assess importance, attribute cause, or forecast.
  If a future change asks the model for a ranking, a severity judgement, or a
  single "how significant was this day" figure, that is a different feature
  and needs a fresh review — the two-field schema is the wall, so widening
  the schema is the thing to catch.
