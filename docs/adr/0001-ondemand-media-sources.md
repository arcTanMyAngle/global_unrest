# ADR-0001: On-demand media source expansion

**Status:** Proposed — needs a privacy/terms review before any code.
**Scope:** the Media page's on-demand lookup (CLAUDE.md product rule 7,
docs/SAFETY_AND_PRIVACY.md "On-demand media lookup").

## Context

The Media page answers one question, only when a person asks it: *"show me
footage published about this place in this time window."* Today that question
is served by three legs, all on-demand, all transient, none persisted:

- **GDELT** DOC 2.0, domain-restricted to video hosts — news outlets' mirrored
  video.
- **Bluesky** `searchPosts` — public posts that carry video.
- **Telegram** public-channel allowlist, read-only session — on-the-ground
  channel footage (only when a session is configured).

The ask is to make that lookup show *what is actually going on in the
specified region* — war, crime, natural disasters — rather than thin or
irrelevant results. The first pass shipped inside the existing boundary: topic
topic chips (war/crime/protest/flood/earthquake/wildfire/storm) and a
labelled external YouTube search link. This note scopes the second pass: new
**sources**, which is where the Media exception ends and a review begins.

The exception is narrow and must survive untouched:

- on-demand only — no polling, no prefetch, no account following;
- place-scoped, time-bounded, per-provider capped;
- public **video** only;
- minimal display fields (public URL, bounded one-line label, timestamp, and
  outlet/handle/channel attribution);
- transient — never persisted, never fed back into ingest or storage;
- no stream extraction — published embeds or direct public media files only.

## Decision drivers

From the review checklist in docs/SAFETY_AND_PRIVACY.md:

1. Does it make a person easier to identify, locate, target, or harass?
2. Does it imply more precision than the source supplies?
3. Does it blend attention/events/alerts/chatter into an unsupported claim?
4. Does it store, transmit, or expose content beyond declared source terms?
5. Does it add a lookup, field, retention, or playback path beyond the
   bounded Media exception?
6. Does it give a viewer more control over a contributor's disclosure than
   the contributor has?

A source is in scope only if the answer to all six is no, or the design is
changed until it is.

## The key constraint: the Media page needs video, not articles

A recurring theme below: the Media page surfaces **video**, and most feed
sources publish **article links**. An RSS item's link is the article page,
which does not classify as video (`core_types::is_video_url`) and cannot be
verified to contain footage without fetching it — which the exception forbids.
So "add Liveuamap RSS" does not improve the Media page by itself; it improves
**ingest** (aggregate chatter). The sources that genuinely widen the *video*
page are the ones whose items are posts or video links directly.

## Candidate sources and disposition

### 1. Telegram classified packs — ACCEPT (both ingest and Media)

The strongest on-the-ground video source already in the exception. Expanding
it is the highest-value, lowest-risk change.

- A structured TOML catalog, not a handle list: each entry carries mandatory
  `class` (`resistance/militia`, `junta/pro-military`, `thai-deep-south`,
  `narco/crime`, `state`, `neutral`) and `region` provenance. Entries without
  explicit provenance are rejected, not defaulted (already ROADMAP policy).
- `region` is **channel provenance**, never a post's geolocation — a post is
  not placed by its channel's region.
- Ingest: `Partisan`/`Combatant`/`State` volume goes to its own claims lane
  via the chatter accumulator key, out of the neutral aggregate.
- Media: the existing read-only Telegram leg reads the same catalog; results
  stay public video links + bounded label + channel attribution, and never
  expose a sender.
- Class is shown as a label where a channel's provenance differs from
  neutral, so "resistance channel" is never silently presented as neutral.

### 2. Liveuamap — REJECT as a Media feed, ACCEPT via its Telegram channel

- Liveuamap regional **feeds** are article/incident links, not video: they do
  not qualify under "public video only" and therefore cannot feed the Media
  page directly.
- The `liveuamap` Telegram channel is already on the allowlist and posts
  footage; it is covered by item 1 (add it as a classified-pack row if not
  already present).
- If a machine-readable public feed is confirmed and its terms permit reuse,
  it becomes a `source-feeds` config row for **ingest only** (item 3).

### 3. `crates/source-feeds` (RSS/Atom/JSON adapter) — ACCEPT for ingest only

- One generic adapter with the feed list as configuration
  (`url, shape, region, topic, class, cadence`), reusing
  `chatter::PlaceMatcher` and `TOPICS`.
- Feeds the chatter accumulator; never the Media page (article links are not
  video).
- Each endpoint is verified machine-readable and terms-permitting before it
  lands; a feed with no such permission is not scraped.

### 4. Nostr + fediverse video posts — ACCEPT behind an experimental flag

- A new Media leg with the same shape as Bluesky: place-scoped, time-bounded,
  capped, transient, public video only (native media or link cards to video
  hosts).
- Nostr: verify event signatures before showing anything (sybil/spam risk is
  reflected in confidence, not by dropping the source).
- Mastodon/fediverse: public timelines may need auth, may be disabled per
  instance, and cap at ~40 statuses per call — "keyless" is
  instance-dependent, and a relay/instance list is **sampled coverage**, not a
  firehose, so the UI labels it as such.
- The same legs can feed ingest through `ChatterAccumulator` with bounded
  id/URI deduplication *before* counting (one Nostr event appears on many
  relays; one post on many instances).

## Config shape

- **Telegram packs:** TOML catalog (`id, handle, class, region, cadence`),
  consumed by both the ingest poller and the read-only Media leg. No handle
  list without class/region.
- **source-feeds:** TOML/JSON feed rows (`url, shape, region, topic, class,
  cadence`), validated for shape and terms at load.
- **Nostr/Mastodon:** relay/instance lists with the sampled-coverage caveat,
  behind an experimental feature flag.

## Boundary compliance

| Source | On-demand | Video only | Transient | Attribution | New privacy surface |
|---|---|---|---|---|---|
| Telegram packs | yes (read-only leg) | yes | yes | channel, never sender | channel class/region catalog |
| Liveuamap feed | n/a (ingest) | — | ingest aggregates | outlet | terms review for the feed |
| source-feeds | n/a (ingest) | — | ingest aggregates | feed/outlet | terms review per feed |
| Nostr/fediverse | yes (Media leg) + ingest | yes | yes | handle + relay label | relay/instance list |

No item in this note adds persistence, background collection, sender
exposure, or watch-page stream extraction to the Media page.

## Open questions for review

1. Per-channel and per-feed **terms**: which allow reuse, and which are
   ingest-only vs Media-page-eligible?
2. **Labelling** of partisan/combatant channels in the Media results list —
   the class must be visible, not buried in a tooltip.
3. Whether the Nostr/Mastodon Media leg earns its complexity, or whether
   those sources land as **ingest-only** first and the Media page stays on
   GDELT + Bluesky + Telegram.
4. Proxy routing for the new legs (`LES_SOCKS5_PROXY`, fail-closed) — the
   transport note in ROADMAP applies to every new network leg.

## Docs to update when any of this is implemented

- docs/SAFETY_AND_PRIVACY.md — new source handling rows + terms.
- docs/ROADMAP.md — move the relevant items from "planned" to "done".
- docs/DEVELOPMENT.md — feature flags and environment variables.
- README.md — user-visible source list and labelling.
- CHANGELOG.md — under Unreleased.
