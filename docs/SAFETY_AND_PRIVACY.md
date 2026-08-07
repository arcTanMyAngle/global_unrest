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
   profiling, or persistent location tracking. Actor data from structured
   sources remains limited to coarse source-taxonomy labels.
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

## Source licensing

| Source | Terms we rely on | Notes |
|---|---|---|
| GDELT | Free for use with attribution | Keyless public API/dumps; attribute in README and UI About. |
| ACLED | Registered authorization required (myACLED account) | Included in desktop defaults; OAuth password grant—ACLED retired API keys; credentials via `ACLED_EMAIL`/`ACLED_PASSWORD` env vars only. **No redistribution of raw ACLED data**: the `notes` narrative is never stored, only structural metadata, and worker snapshots containing ACLED rows are for local/authorized use—never served publicly. Attribution: "Armed Conflict Location & Event Data Project (ACLED); acleddata.com" (UI status panel + README). ACLED *corrections* reuse event ids and are not re-applied by dedup (documented limitation). |
| NOAA/NWS active alerts | US-government public domain | Included in desktop defaults; keyless; descriptive `User-Agent` per api.weather.gov policy. **US + territories coverage only**—a documented coverage bias, not a global weather layer. Zone-scoped alerts without polygon geometry yield no events (we never guess coordinates). |
| Natural Earth | Public domain | Attributed anyway (basemap credit). |
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
