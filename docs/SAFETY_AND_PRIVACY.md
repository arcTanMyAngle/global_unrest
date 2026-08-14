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
   blended score is presented as a factual claim.
4. **Do not invent precision.** Only city/exact records become point markers.
   Country/admin records shade regions. A centroid is never described as an
   observed location.
5. **Credentials stay local.** Keys and sessions come from environment
   variables or local, gitignored files. They are never committed, logged, or
   included in URLs.
6. **Social chatter is aggregate-only.** Bluesky and Telegram normalize
   completed place/topic windows into counts before storage. Post or message
   text, author identity, handles, identifiers, and URLs are not stored,
   logged, or exposed by the source adapters.
7. **Generated text is labelled, bounded, and never the record.** Daily
   Events is an opt-in interpretation aid. Its response schema has two
   separate fields, media attention and event data, rather than a combined
   significance judgement. Each section displays its record count, model, and
   generation time. Generated prose is not a news report, forecast, severity
   rating, event source, or map caption.

## Third-party processing (Anthropic API)

Daily Events is the only feature that sends stored signals to a third party.
The user must select a UTC day and explicitly click Generate digest. No digest
is generated on startup, on a timer, or in the background.

### What leaves the device

- Bounded daily aggregates: record counts, article counts, distinct-outlet
  counts, and country-level totals.
- A bounded sample of permitted GDELT, NOAA, and IODA metadata, such as
  headlines/outlet domains or event source, label, kind, and severity.
- Counts for ACLED and chatter sources when relevant, but never their
  row-level data.

The request is capped by named limits in the daily-digest crate so it cannot
become a bulk export. Article bodies, ACLED rows, post/message text, author
identity, handles, user IDs, and URLs are not part of the digest facts type
and cannot be sent through this path.

### Credential and cache behavior

Anthropic access uses ANTHROPIC_API_KEY in an HTTP header, not a query string.
The application reports a missing key without echoing its value. Treat every
generation as a metered third-party API call and review the terms of the
Anthropic account in use before enabling it.

One local cache row exists per UTC day. A requested regeneration explicitly
replaces that row; reopening a cached day makes no API request. The cache
stores generated prose, model, generation time, and its two record counts, not
the source rows that were considered.

## Source licensing and handling

| Source | Handling rule |
|---|---|
| GDELT | Public/keyless data used with attribution. Store metadata, not article bodies; treat coverage as a biased proxy. |
| ACLED | Requires authorized access. Do not store notes or redistribute raw ACLED data. Public worker/API deployments must not ingest or serve it. |
| NOAA/NWS active alerts | US government public-domain alerts. US and territory coverage only; alerts without usable geometry do not get guessed coordinates. |
| IODA | Keyless outage events from Georgia Tech's Internet Intelligence Research Lab. Country precision only; use as aggregate network signal, never person-level data. |
| Bluesky Jetstream | Keyless public stream processed only into aggregate chatter windows. A disconnect may undercount; it never justifies backfilling person-level content. |
| Telegram public channels | A dedicated account reads only the curated public-channel allowlist using a local MTProto session. Aggregate before storage and do not crawl beyond that scope. |
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
  location or a discrete event.
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
5. Does it give a viewer more control over a contributor's disclosure than the
   contributor has?

If any answer is yes, redesign the feature or do not ship it.

## Daily Events review record

Daily Events was reviewed as the first generative and third-party-processing
layer in the project. It may describe what the sources recorded, but it may
not assess importance, attribute cause, forecast, rank places, or produce a
single significance score. The two-section schema, bounded facts, withheld
source rows, explicit user action, and local cache are enforcement points, not
mere prompt wording.
