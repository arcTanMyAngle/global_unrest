# GDELT GEO 2.0 / GKG 2.1 — A2 spike finding

Status: **complete**. This closes M9 item A2 and, with it, M9.

The question A2 was asked to settle: should GDELT geography enter the product
as **(a)** GEO 2.0 as its own explicitly-aggregate signal, or **(b)** GKG 2.1
for real article-location-domain linkage? A window-level pseudo-join between
DOC articles and GEO aggregates was ruled out before the spike and is not
revisited here.

Raw evidence is committed under
[`crates/source-gdelt/tests/data/spike-a2/`](../crates/source-gdelt/tests/data/spike-a2/),
with per-file provenance in that directory's `README.md`. Everything below is
measured from those captures unless marked as an estimate. Captures were taken
2026-08-19 between 04:00Z and 15:35Z.

---

## Decision

**Option (b): GKG 2.1.**

Option (a) is not implementable — **GEO 2.0 is gone**. It returns `404 Not
Found` for every mode, format, and scheme tried, from two independent networks,
while sibling endpoints on the same host and path prefix serve normally. There
is no live endpoint to build an aggregate signal on, so (a) is not a design
trade-off we get to make; it is off the table.

GKG 2.1 is available, complete, and cheap to parse, and it supplies the thing
DOC cannot: a per-article link between **an outlet domain**, **the places the
article names**, and **a headline**. It is also served from Google Cloud
Storage as immutable 15-minute files, which is a materially more reliable
transport than `api.gdeltproject.org` (see the DOC 429 finding below).

The decision comes with three constraints that are part of the decision, not
follow-up polish:

1. **GKG is `MediaAttention`, never `RecordedEvent`.** Its locations are places
   an article *mentions*. That is `LocationRole::MentionedPlace`. A GKG row is
   not evidence that anything happened at that place.
2. **Precision is per-mention, not per-article.** Only 30.7% of location
   mentions are city-or-finer. Those may render as points; the 52.1% country
   and 17.2% admin mentions must shade regions. Carrying one precision per
   article would fabricate precision and violate product rule 3.
3. **No theme-to-location attribution.** GKG exposes no edge between a theme and
   a location; both merely carry character offsets into the article. Attributing
   a theme to a place by offset proximity is ambiguous (measured below) and
   would be exactly the kind of pseudo-join M9 forbids, only at paragraph scale
   instead of window scale. Themes stay **document-level**.

What this does **not** decide: how much GKG to ingest, where it is ingested, and
what is filtered. At 470 MB/day for English alone this cannot be a desktop
15-minute cadence in its raw form. That is the first M9.1 question, not an A2
conclusion.

---

## GEO 2.0 is a hard 404

| Request | Result |
|---|---|
| `api/v2/geo/geo?query=protest&format=GeoJSON&mode=PointData&timespan=1d` | `404` |
| same, `mode=ADM1` | `404` |
| same, `format=CSV&mode=Country` | `404` |
| same over plain `http://` | `404` |
| bare `api/v2/geo/geo` | `404` |
| `api/v2/geo`, `api/v2/geo/geo.php`, `api/v1/geo/geo` | `404` |
| `PointData` with a browser UA + `Referer` | `404` |
| **control:** `api/v2/tv/tv` | **`200 OK`** |
| **control:** `api/v2/doc/doc` | serves (200 when not load-shed) |

The 404s carry `Server: GDELT Server` and Apache's stock 404 body — this is the
origin answering, not a CDN or a DNS failure. `summary/summary` and
`context/context` also serve. The 404 reproduced from a second, independent
network path. No retirement announcement was found on the GDELT blog or docs;
the endpoint is simply no longer there, and the documentation still describes it.

Full capture with response headers: `spike-a2/geo_probe.txt`.

**Consequence:** any future design that assumed GEO — including the plan's
"GEO+DOC over 56 windows" costing — is void.

---

## GKG 2.1 measured

Reference window `20260819041500` (English stream): 781 rows, 9,753,591 bytes
uncompressed, 3,154,021 compressed.

### Mentions vs articles vs domains

| Unit | Count in one 15-minute window |
|---|---|
| Articles (rows, = distinct `GKGRECORDID`, = distinct URLs) | **781** |
| Distinct outlet domains | **173** |
| Location *mentions* (raw `V2ENHANCEDLOCATIONS` entries) | **6,827** |
| Distinct (article, location) pairs | **2,109** |
| Distinct (domain, location) pairs | **1,639** |
| Distinct `FEATUREID`s | **676** |

These four counts are the whole reason (b) is worth doing: DOC gives you 781
articles each pinned to its *publisher's* country and nothing else. GKG gives
you 1,639 real domain-to-place edges over 676 distinct places from the same
15 minutes.

Note the 6,827 to 2,109 collapse: **4,718 mentions are the same place repeated
within the same article** at different character offsets. Any volume metric
must dedupe per article first, or a single article naming one city 40 times
will outweigh 40 articles naming it once. Articles carry a mean of 8.74
locations, max 128.

Domain concentration is mild: median 3 articles per domain, max 31
(`bignewsnetwork.com`).

### Actual precision

Of 6,827 location mentions:

| `V2ENHANCEDLOCATIONS` type | Count | Share | Renders as |
|---|---|---|---|
| 1 COUNTRY | 3,555 | 52.1% | region shading |
| 2 USSTATE | 1,090 | 16.0% | region shading |
| 5 WORLDSTATE | 85 | 1.2% | region shading |
| 3 USCITY | 922 | 13.5% | point |
| 4 WORLDCITY | 1,175 | 17.2% | point |

**Rolled up: 30.7% city-or-finer, 17.2% admin, 52.1% country.**

Every type carries lat/lon, including COUNTRY — which is a trap. A country-type
mention's coordinates are a country centroid, not a location. Storing that
coordinate without its `LocationPrecision` and rendering it as a point would
put a dot in the middle of the Sahara and call it news. The type field is the
authority on precision; the coordinate is not.

166 of 781 rows (21.3%) have **no** location field at all. Geography is
available for 78.7% of articles, not all of them.

### Headline availability

**100%** — all 781 rows carry a `<PAGE_TITLE>` inside `V2EXTRASXML`. This is
the field DOC's `artlist` gives directly and the reason a GKG-derived signal
can show a real headline rather than a bare domain.

GKG ships no article body, so rule 4 (metadata, not bodies) is satisfied by the
format itself, provided we extract the title and leave the rest of
`V2EXTRASXML` alone.

### Theme and location semantics

`V2ENHANCEDTHEMES` is `theme,charoffset` pairs; `V2ENHANCEDLOCATIONS` carries a
character offset in its 9th subfield. **There is no explicit theme-to-location
edge.** The only way to link them is offset proximity, and that is ambiguous:

| Measure over all located mentions in the window | Value |
|---|---|
| Character distance to the *nearest* theme | median 37, p90 173, mean 100 |
| Distinct themes within +/-100 characters of a location | median 2, p90 7, max 23 |
| Locations with **zero** themes within +/-100 characters | 20.5% |

A median of 2 and a p90 of 7 candidate themes per location means proximity
attribution is a coin flip at best. The window is dense — 32,618 theme
occurrences across 781 articles, a mean of 41.8 per article — so themes are
everywhere and nearness means little.

**Therefore themes are document-level.** "This article carries theme X and
mentions place Y" is supportable. "Theme X happened at place Y" is not.

### Idempotency keys

| Property | Result |
|---|---|
| `GKGRECORDID` unique within a window | 781/781 |
| URL overlap, adjacent windows (`0400` vs `0415`) | **0** of 895 / 781 |
| URL overlap, same slot 24h apart | **0** of 781 / 681 |
| Re-download of the same file, byte-identical | yes (SHA-256 match) |
| `ETag` | MD5 of the object |
| Transport | Google Cloud Storage (`x-goog-*` headers), `Accept-Ranges: bytes` |

Windows are disjoint by construction, so `GKGRECORDID` is a sound primary key
and the 15-minute filename is a sound coverage-ledger unit. Files are immutable
once published — the ETag lets a ledger verify a window without re-parsing it.
This is a much cleaner idempotency story than DOC, where overlapping
`startdatetime`/`enddatetime` windows return overlapping article sets.

### Volume and processing time

Sampled at 3-hour spacing across a full day (8 slots per stream):

| Stream | Mean per 15-min file | Per day | Per 7 days |
|---|---|---|---|
| English | 5.13 MB | **~470 MB** | ~3.2 GB |
| Translation | 8.89 MB | **~814 MB** | ~5.6 GB |
| Combined | 14.0 MB | ~1.28 GB | **~8.8 GB** |

All figures are *compressed* transfer bytes. Uncompressed, the reference English
window is 3.1x its zip.

Volume swings with the news day: English ranges 2.55–7.84 MB per file across the
sampled day, translation 4.77–14.28 MB, and the two peak at different hours.

Parsing is not the bottleneck: **~0.07 s** to parse the 781-row reference file
with Python's `csv` module, i.e. roughly 7 seconds of CPU for a full day's 96
English files. Download dominates by two orders of magnitude.

The translation stream is a separate set of files with `-T`-suffixed record ids
and a `V2.1TRANSLATIONINFO` field carrying `srclc:<lang>`. Zero `-T` ids appear
in the English stream, so the two are cleanly separable and translation is an
opt-in cost, not an unavoidable one.

### Historical addressability

`20150218224500` serves (`200`, 10.9 MB); `20140101000000` is `404`. GKG 2.1
15-minute files therefore reach back to **2015-02-18**, addressed directly by
timestamp with no query API in the path. Backfill is a file fetch, not a search.

---

## DOC 2.0 findings (incidental, but they change earlier assumptions)

These fell out of the spike and correct constraints previously recorded as
settled.

### Explicit historical windows *do* work

The plan recorded that arbitrary explicit windows were unsupported. They are
not. `startdatetime`/`enddatetime` 60 days back and ~2 years back both return
real articles whose `seendate` falls inside the requested window
(`doc_artlist_historical_60d.json`, `doc_artlist_historical_2y.json`). The 7-day
`TIMESPAN` ceiling applies to the relative-window parameter, not to explicit
ones.

### DOC silently truncates a wide window to one 15-minute slot

`DEFAULT_QUERY` over a **one-hour** window at `maxrecords=250` returns 250
articles that **all** carry `seendate` `20260818T011500Z` — a single 15-minute
slot (`doc_artlist_1h_truncated.json`). The response contains no truncation
flag. A caller that does not inspect `seendate` will believe it has an hour of
coverage and actually have 15 minutes of it.

Worse, `20260818T011500Z` is **outside** the requested window
(`20260818000000`–`20260818010000`). DOC's `enddatetime` leaked by 15 minutes.
So the returned set is neither complete for the window nor confined to it.

### DOC 429s are load shedding, not rate limiting

Eight requests spaced **20 seconds** apart — four times more conservative than
the documented one-per-five-seconds — returned **6 x 429 and 2 x 200,
interleaved**. A single cold request after ~11 hours of idle also 429'd, while
a request one minute after a 429 burst succeeded.

The 429 body is plain text (not JSON) and recites the 5-second limit regardless
of actual request spacing, so it cannot be used to infer what we did wrong.

**Consequence:** any DOC backfill cost model expressed as "N requests x 5 s" is
wrong. Throughput is not ours to control. A DOC backfill needs retry with
backoff, a coverage ledger recording which windows actually landed, and an
acceptance that a run may not complete in bounded time. This is a strong
independent argument for GKG's static-file transport.

### `country.rs` drops 9.6% of DOC records

`resolve()` covers 52 canonical country names and a small alias set, and by
design does not guess. In a real DOC sample **9.6% of records failed
normalization** and landed in `ingest_log` — and because normalization fails for
the whole record, the article is lost entirely, not merely left ungeocoded.

Under decision (b) this becomes less central: GKG, not DOC, becomes the spatial
record, and DOC's publisher-country geocoding stops being the geography that
matters. But it does not become moot while DOC ingest exists, because the
failure discards articles rather than degrading their precision. A3 is
downgraded from "expand the country index" to "stop discarding the record when
the country is unknown".

---

## What this closes and what it opens

Closed: A2, and with it M9.

Opened, for M9.1 to sequence:

- **Ingest shape for GKG.** 470 MB/day English is not a desktop cadence in raw
  form. Filtering (themes? locations? domains?), placement (worker vs desktop),
  and retention are undecided.
- **A4 coverage ledger** now has a natural unit for GKG — the 15-minute file
  name plus its ETag — and a sharper justification for DOC, given the 429
  behaviour.
- **A3** is re-scoped as described above.
- The `V1LOCATIONS`/`V2ENHANCEDLOCATIONS` `FEATUREID` namespace needs a
  resolution story before locations become stable keys; 676 distinct ids
  appeared in a single window.
