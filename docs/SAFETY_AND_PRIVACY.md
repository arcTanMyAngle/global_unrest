# Safety and privacy

Live Earth Signals is a situational-awareness tool, not a surveillance,
targeting, or ground-truth system. Its maps describe what particular sources
recorded and how those records relate; they do not locate people, prove a
claim, or establish that an area is safe.

## Hard rules

1. **No covert tracking or targeting.** Do not build features that locate,
   profile, follow, identify, or operationally target a person without their
   consent. Voluntary publishing by a contributor is a separate future
   capability and must remain under the publisher's control.
2. **Metadata, not article bodies.** The app stores normalized source
   metadata such as headlines, URLs, outlet domains, timestamps, and
   aggregate counts. It does not store article bodies. ACLED narratives are
   not stored.
3. **Attention is not truth.** Media attention, provider event data, official
   alerts, and aggregate chatter remain separate evidence classes. No single
   blended score is presented as a factual claim. This is enforced in the
   schema, not by convention: every record carries a `SignalFamily`, each
   family has its own volume unit, and which components a family may enter is
   a checked matrix ([SIGNAL_MODEL.md](SIGNAL_MODEL.md)).
4. **Do not invent precision.** Only city/exact records become point markers.
   Country/admin records shade regions. A centroid is never described as an
   observed location. Every record also carries a `LocationRole` saying what
   its coordinates *are* — an event site, a place merely mentioned, a
   publisher's origin, or a reporting jurisdiction — and a publisher-origin
   record is never drawn as though something happened there.
5. **Credentials stay local.** Keys and sessions come from environment
   variables or local, gitignored files. They are never committed, logged, or
   included in URLs.
6. **Social chatter ingestion is aggregate-only.** Bluesky and Telegram
   normalize completed place/topic windows into counts before storage. The
   ingest path does not store or log post/message text, author identity,
   handles, identifiers, or URLs, and its normalized records never expose
   them. The narrowly scoped, user-directed Media lookup exception is defined
   below; it does not change the ingest, storage, map, or API boundary.
7. **Generated text is labelled, bounded, and never the record.** Daily
   Events is an opt-in interpretation aid. Its response schema has two
   separate fields, media attention and event data, rather than a combined
   significance judgement. Each section displays its record count, model, and
   generation time. Generated prose is not a news report, forecast, severity
   rating, event source, or map caption.

## Third-party processing (Google Gemini API)

Daily Events is the only feature that sends stored signals to a third party.
The user must select a UTC day and explicitly click Generate digest. No digest
is generated on startup, on a timer, or in the background.

### What leaves the device

- Bounded daily aggregates: record counts, article counts, distinct-outlet
  counts, and country-level totals.
- A bounded sample of permitted GDELT, NOAA, and IODA metadata, such as
  headlines/outlet domains or event source, label, kind, and severity.
- Counts for ACLED when relevant, but never its row-level data.

Aggregate chatter does not leave the device through this path **at all**: the
digest's fact queries select `family = 'media_attention'` for the attention
section and `family IN ('recorded_event', 'official_alert')` for the event
section, so chatter is excluded by the query rather than by a rule someone
must remember. Chatter is shown only in the app's own UI.

The request is capped by named limits in the daily-digest crate so it cannot
become a bulk export. Article bodies, ACLED rows, post/message text, author
identity, handles, user IDs, and URLs are not part of the digest facts type
and cannot be sent through this path.

### Credential and cache behavior

Google Gemini access uses GEMINI_API_KEY in the x-goog-api-key HTTP header,
not a query string. The application reports a missing key without echoing its
value. Treat every generation as a metered third-party API call and review the
applicable Google API terms, quota, and billing for the account in use before
enabling it.

One local cache row exists per UTC day. A requested regeneration explicitly
replaces that row; reopening a cached day makes no API request. The cache
stores generated prose, model, generation time, and its two record counts, not
the source rows that were considered.

## On-demand media lookup

The Media page is a deliberate, limited exception to hard rule 6. It supports
researching public video after a user explicitly asks for one named place, an
optional topic, and one bounded time window. It is not a SignalSource and does
not broaden the map's aggregate-data model.

The exception is constrained as follows:

- **No background collection.** Nothing polls, prefetches, follows accounts,
  or searches a place until the user clicks Search. There is no
  everywhere/everything query, and the UI offers only 24-hour, 3-day, 7-day,
  and 30-day windows with a 25-result per-provider cap.
- **Public video only.** GDELT is queried for video-hosting results; Bluesky
  public posts and configured Telegram public-channel posts must carry video.
  Social results are visually marked as unverified public posts, not event
  evidence.
- **Minimal display fields.** A temporary hit may include its public URL, a
  bounded one-line title/caption, timestamp, and public outlet, Bluesky
  handle, or Telegram channel attribution. The Telegram lookup never reads or
  exposes a message sender.
- **No persistence or reuse.** Hits are held in process memory only until the
  next search replaces them or the app exits. They are not written to DuckDB,
  Parquet, a cache, logs, the services API, aggregate chatter rollups, or
  Daily Events facts.
- **No stream extraction.** The player uses a provider's published embed or a
  direct public media URL. It does not turn watch pages into stream URLs or
  scrape protected player data. In-app playback is available only where the
  Windows WebView2 implementation and the URL support it; a browser link
  remains available everywhere.

Any widening of the query scope, result fields, provider access, retention, or
playback mechanism requires a new privacy and terms review.

## Source licensing and handling

| Source | Handling rule |
|---|---|
| GDELT | Public/keyless data used with attribution. Ingest stores metadata, not article bodies; treat coverage as a biased proxy. The Media page may make an explicit, bounded GDELT video lookup and keeps the resulting link transient. |
| ACLED | Requires authorized access. Do not store notes or redistribute raw ACLED data. Public worker/API deployments must not ingest or serve it. |
| NOAA/NWS active alerts | US government public-domain alerts. US and territory coverage only; alerts without usable geometry do not get guessed coordinates. |
| IODA | Keyless outage events from Georgia Tech's Internet Intelligence Research Lab. Country precision only; use as aggregate network signal, never person-level data. |
| Bluesky Jetstream | The ingest stream is processed only into aggregate chatter windows. The Media page may explicitly show a public video post's bounded label, URL, and visible handle transiently; it does not feed those fields back into ingest or storage. |
| Telegram public channels | A dedicated account reads only the curated public-channel allowlist using a local MTProto session. Ingest aggregates before storage, keyed by channel class so classes are never summed together; a class describes a *channel's* provenance and never a person, and defaults to `unspecified` rather than claiming one. The explicit Media lookup uses the same allowlist and a read-only session, returning only temporary public video links, bounded labels, and channel attribution. |
| Published video embeds | Use provider-published embeds or direct public media files only. Do not resolve a watch page into an underlying stream. |
| Natural Earth | Public-domain basemap and gazetteer data, attributed in the application and README. |
| OSM tiles | Not implemented. Any M8 tile layer needs a provider-policy, attribution, offline behavior, and user-control review before it lands. |
| Fixtures | Fully synthetic test/service-smoke data using reserved example domains. Never loaded by the desktop. |

## Known limits and biases

- **Coverage bias:** media density varies by language, geography, and
  publisher. Attention can reflect what is indexed rather than what happened.
  The divergence layer visualizes this mismatch but cannot distinguish
  under-reporting from a source coverage gap.
- **Geocoding bias:** GDELT DOC attention is geocoded to a source country,
  which is not evidence that a publisher is at the event. Other feeds can
  also supply only country/admin precision.
- **Taxonomy mismatch:** event kinds and severities are source-specific
  normalizations, not a universal taxonomy.
- **Aggregate chatter matching:** place/topic keyword matching can miss or
  misclassify posts. It is media-attention data, not a claim about a person's
  location or a discrete event. Coverage is uneven by language: most of the
  vocabulary is English, and the native-script tables for Burmese, Thai, Lao,
  Khmer, Japanese, and Chinese carry endonyms and common topic words only, so
  a post in one of those scripts about a place elsewhere in the world does
  not count. In those scripts, where words are not space-separated, matching
  is substring-based within a script run and can occasionally match across a
  word boundary — that inflates a count, and cannot invent a place, since
  coordinates always come from the bundled gazetteer.
- **Media lookup:** a public video or post is a lead, not verification. Search
  results can be missing, removed, mislabelled, geographically ambiguous, or
  misleading; public social posts are explicitly marked unverified.
- **Generated prose:** a fluent model summary can sound more certain than its
  inputs. The page displays counts and keeps its two evidence sections
  separate so readers can inspect the underlying signal class.

## Retention

- The analytics store can prune events older than the configured
  LES_RETENTION_DAYS window. A window at least as long as the 28-day baseline
  period keeps recent baselines meaningful; unset keeps retained records.
- Derived buckets and local settings are local application data.
- Session Parquet exports are explicitly created local files.
- Daily Events cache rows are local, one per UTC day, and are replaceable by
  explicit regeneration.
- Media lookup hits are session-memory data, replaced by the next search and
  discarded when the application exits.
- Fixture data is a permanent deterministic regression harness, not runtime
  desktop data.

## Future voluntary channels

If on-scene channels are built, the contributor controls whether exact
location and true-live timing are shared. Required foundations include
authentication and provenance, approximate location by default, optional
delay, emergency cutoff, expiration, metadata stripping, evidence states,
moderation, anti-impersonation controls, bystander privacy, and a correction
path. Emergency and local safety guidance remain authoritative.

## Review checklist

Before shipping a source, visual layer, API route, or interpretation feature,
ask:

1. Does it make a person easier to identify, locate, target, or harass?
2. Does it imply more geographical or temporal precision than the source
   supplies?
3. Does it blend media attention, events, alerts, or chatter into a claim the
   data cannot support?
4. Does it store, transmit, or expose content beyond the declared source
   terms and privacy boundary?
5. Does it add a lookup, result field, retention behavior, or player access
   path beyond the bounded Media exception?
6. Does it give a viewer more control over a contributor's disclosure than the
   contributor has?

If any answer is yes, redesign the feature or do not ship it.

## Daily Events review record

Daily Events was reviewed as the first generative and third-party-processing
layer in the project. It may describe what the sources recorded, but it may
not assess importance, attribute cause, forecast, rank places, or produce a
single significance score. The two-section schema, bounded facts, withheld
source rows, explicit user action, and local cache are enforcement points, not
mere prompt wording.
