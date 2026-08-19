# Signal model

The contract that keeps different kinds of observation from being summed into
one number. Product rule 1 says media attention, discrete events, official
alerts, aggregate chatter, generated prose, and transient media research stay
separate; this page is the machine-checkable form of that rule.

Written before the M9 implementation, deliberately: adding an enum variant is
not separating the signals. Separation is real only when units, scoring,
geography, storage, digest behaviour, and UI all agree. Everything below is
enforced in code or tested, and where it is not yet, that is said explicitly.

## Why the previous shape failed

`EventKind::is_discrete_event()` was literally `!is_attention()`, and analytics
branched on it:

```rust
if ev.kind.is_attention() { /* attention_count += 1 */ } else { /* event_count += 1 */ }
```

A two-valued taxonomy has no room for a third thing. Any new variant fell into
the `else` and became a discrete event — contributing event count, recency,
severity, and precision to the unrest score. A zero kind weight would not have
saved it, because `event_count_weight` counts records, not weights.

The trailing baseline compounded it: it was built from
`event_count + attention_count`, so anything landing in either counter entered
the spike score and therefore `combined_score`. That is how Bluesky and Telegram
post volume — normalized as `EventKind::NewsAttention` — became "media
attention" in Daily Events, with zero outlet domains and no headlines.

## Families

`SignalFamily` is the top-level axis: **what kind of observation is this?**
`EventKind` is demoted to the within-family subtype. The pair is validated at
normalization by `SignalFamily::permits(EventKind)`; a pair outside this table
is a normalization error, so the stored column cannot drift.

| Family | Valid kinds | Volume unit | Unrest | Generic spike | `combined_score` | Digest |
|---|---|---|:--:|:--:|:--:|---|
| `MediaAttention` | `NewsAttention` | articles | no | yes | yes (attention term) | Attention section |
| `Chatter` | `Chatter` | posts | no | **no** | **no** | **excluded** |
| `RecordedEvent` | `Protest`, `Conflict`, `Disruption`, `Other` | records | **yes** | yes | yes (unrest term) | Event section |
| `OfficialAlert` | `Alert` | alerts | **no** | no | no | Event section, labelled official |
| `Measurement` | `Measurement` | samples | no | no | no | excluded |

### Chatter enters nothing generic

Chatter gets its own family spike (`family_baselines`, which silence detection
needs) and touches **none** of: `unrest_score`, the generic spike baseline,
`combined_score`, `article_count`, `distinct_outlets`, `source_count`. Anything
less and the headline number is contaminated by post volume despite the family
label — the exact failure this contract exists to prevent. Tested directly, not
inferred from weights.

### OfficialAlert leaves unrest

NOAA previously normalized to `Disruption` and scored as civil unrest. It is now
its own family and does not enter `unrest_score`. This is a **real behaviour
change**, not a free declaration: US-heavy unrest scores drop visibly, and
before/after numbers on a live database are part of shipping it. A weather
warning is a jurisdiction announcing a hazard, not an occurrence of unrest.

Alerts still appear in the Daily Events *event* section, labelled as official —
the digest stays two-sectioned (product rule 6).

### Measurement is declared and unused

Declared in the matrix and in validation so the enum is complete and long-form
storage needs no migration to accept it. **No adapter, no source, no renderer
lane, no UI.** Reserving a variant must not become building a lane for a source
we are not authorized to use.

### Open question, recorded not hidden

Whether `MediaAttention` should keep feeding the *generic* spike or move to a
per-family spike like chatter. It stays generic here because that is current
behaviour and the attention-vs-unrest divergence mode depends on it. Revisit
after the GDELT geography question is settled.

## Units

Volume is family-relative. The unit is implied by the family per the matrix
above; there is no free-text unit column.

- `GeoTemporalEvent.article_count` becomes **`volume_count`**. IODA, NOAA, and
  chatter stop claiming articles they do not have.
- `RegionBucket.article_count`, `source_count`, and `distinct_outlets` are
  **attention-only by construction** — they are populated from `MediaAttention`
  records and nothing else.
- Chatter writes **no synthetic `headline`**. The UI composes its label from the
  rollup's own place, topic, and count at render time. The stored row claims
  nothing it cannot support.

Cross-family volume is never summed. Two families' counts may be shown side by
side; they may not be added.

## Location roles

Coordinates on a record do not all mean the same thing:

```rust
pub enum LocationRole {
    EventSite,             // where the thing happened
    MentionedPlace,        // a place the coverage refers to
    PublisherOrigin,       // where the outlet is, not where the story is
    ReportingJurisdiction, // the authority whose area the record covers
}
```

GDELT DOC resolves `sourcecountry` — the publisher — so DOC rows are
`PublisherOrigin`. Until the GDELT geography work lands, **`PublisherOrigin`
records are excluded from the spatial attention layer** and Daily Events calls
them publisher-origin coverage. A map that shades the publisher's country and
calls it attention for that place is wrong, and labelling it honestly is
cheaper than leaving it wrong while a spike runs.

The M9 A2 spike has since chosen what replaces it: **GKG 2.1
`V2ENHANCEDLOCATIONS`**, entering as `MediaAttention` with
`LocationRole::MentionedPlace`. Two constraints from that finding bind this
contract — precision is carried **per mention**, not per article (only 30.7% of
mentions are city-or-finer, and country-type mentions ship centroid
coordinates that must never render as points), and themes stay
**document-level**, because GKG exposes no theme-to-location edge and inferring
one from character offsets is ambiguous. See
[GDELT_GEO_GKG.md](GDELT_GEO_GKG.md). The implementation is M9.1; the
quarantine above stays in force until it lands.

NOAA alerts are `ReportingJurisdiction`. ACLED, GDELT Events, and IODA are
`EventSite`. Chatter rollups are `MentionedPlace` — a place named in posts, not
a location taken from any person.

## Storage

`region_buckets` and `baselines` are **derived**: fully rebuilt from `events`
after every ingest. Migrations recreate rather than alter them (DuckDB cannot
add NOT NULL columns to an existing table — see
`crates/storage/migrations/0002_scores.sql`), so changing their shape costs a
DROP, a CREATE, and a rebuild.

`region_buckets` keeps the existing attention/unrest/spike/combined scores.
Per-family counts and baselines are **long-form**:

```text
family_buckets(h3_cell, bucket_start, family, record_count, volume_count)
family_baselines(h3_cell, tod_bucket, family, baseline, sample_days)
```

Long-form rather than one column per family, for two reasons: a sixth family
must not require a schema migration — which was the whole argument for
declaring families now — and `family_baselines` is exactly the shape silence
detection needs (a deficit is per family against that family's own baseline).

This settles the bucket and baseline representation **before** the performance
work on incremental baselines, so we do not tune a representation we are about
to replace.

`events` gains a NOT NULL `family` column, which DuckDB cannot add in place:
the migration builds a shadow table inside one transaction, backfills with
validation, swaps, and drops. Migration does not itself trigger a bucket
rebuild (that runs on ingest or purge), so it sets a marker that forces the
rebuild before any query is served.

## Digest membership

Daily Events remains **two sections**. The fix for chatter appearing as media
attention is not a third section — it is the attention section counting only
`MediaAttention`.

| Family | Digest |
|---|---|
| `MediaAttention` | Attention section |
| `RecordedEvent` | Event section |
| `OfficialAlert` | Event section, labelled official |
| `Chatter` | Excluded — app UI only |
| `Measurement` | Excluded |

Because chatter no longer reaches the digest at all, no chatter-specific
withholding rule is needed on the third-party request path.

## Privacy boundary, unchanged

Families are a classification axis, not a loosening. `ChatterRollup` remains
count-only: no post text, author handles, DIDs, user ids, post ids, or URLs.
Channel *class* (see below) is a property of a channel, not a person, and lives
in the accumulator key rather than in any per-message record.

The events schema carries no person-identifying columns, and no article bodies
or ACLED notes are stored.

## Channel class

Chatter sources differ in what their volume means. A monitoring channel's
posting rate tracks events; a combatant channel's tracks messaging. Summing
them produces a number that means neither.

Class is therefore part of the accumulator key — `(place, topic, class, window)`
— and part of the derived rollup event id, because class-specific rollups for
the same place/topic/window would otherwise collide in storage. Adding class to
`ChatterRollup` alone would be too late: the counts are already summed by then.

The default is `Unspecified`, never `Monitor`. Defaulting Bluesky, Nostr, or
pre-migration Telegram rows to `Monitor` would fabricate provenance the source
never asserted.

Configured channel `region` is **channel provenance** — where the outlet is
based — and never the geolocation of a post.

## What this constrains

- Adding a source means choosing its family and location role explicitly; there
  is no default.
- Adding a family means adding a matrix row, its validation, and its scoring
  membership. It does not mean a storage migration.
- No code path may branch on "attention or else". Matches over `SignalFamily`
  are exhaustive with no catch-all arm, so a new family fails to compile
  wherever a decision is required.

See also: DATA_MODEL.md (record and storage shapes), SCORING.md (component
formulas), SAFETY_AND_PRIVACY.md (the chatter boundary and source terms).
