# Safety and privacy

This is a civic-data research and visualization project. It aggregates
public, properly authorized signals about media attention and reported
events. It is **not** a surveillance, targeting, or operational tool, and
features that would push it that way are out of scope by design.

## Hard rules

1. **Aggregate-level only.** No person-level identification, tracking,
   profiling, or search. Signals are keyed to regions (H3 cells, countries)
   and times, never to individuals. Actor information, if ever stored, is
   limited to coarse categorical labels from the source taxonomy.
2. **Metadata, not bodies.** We store headlines, URLs, and outlet domains —
   never full article text — unless a source's license explicitly allows it.
3. **Public or authorized sources only.** No scraping of restricted sources;
   no bypassing paywalls, authentication, rate limits, or anti-bot systems.
   Rate limits are enforced client-side per adapter.
4. **Attention is not truth.** Media attention is an imperfect, biased proxy.
   The UI separates "media attention" from "event data," shows score
   components individually, and badges low-confidence values. The combined
   number is never shown without its parts.
5. **Secrets stay out of git.** API keys live in environment variables or
   `.env` files covered by `.gitignore`.

## Source licensing

| Source | Terms we rely on | Notes |
|---|---|---|
| GDELT | Free for use with attribution | Keyless public API/dumps; attribute in README and UI About. |
| ACLED | Registered authorization required | Feature-gated (`live` in `source-acled`), compiled out by default; no redistribution of raw ACLED data; key via `ACLED_API_KEY`. |
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
  default keep-all offline). A window ≥ the 28-day baseline keeps recent
  baselines warm. We store only normalized event metadata — never raw GDELT
  dumps or article bodies.
- Derived metrics and fixtures: kept indefinitely (they contain no personal
  data).
- Session Parquet exports are local files created explicitly by the user.

## Misuse review

New features are checked against: does it enable identifying, targeting,
harassing, or tracking individuals or small groups? Does it provide
operational/tactical guidance? If yes, it does not ship. This document is the
place to record any judgment calls.
