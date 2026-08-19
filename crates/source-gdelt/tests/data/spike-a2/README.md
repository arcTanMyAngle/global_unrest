# A2 spike fixtures — GDELT GEO 2.0 / GKG 2.1

Raw, unmodified evidence captured for the M9 A2 spike. These files back the
written finding in `docs/GDELT_GEO_GKG.md`. They are **evidence, not test
inputs**: no parser reads them yet, and nothing in the workspace depends on
them. Do not reformat, re-indent, or "clean" them — their value is that they
are byte-for-byte what the upstream service returned.

All captures were taken 2026-08-19 between 04:00Z and 15:35Z from a single
residential IP with `User-Agent: live-earth-signals/0.8.0`.

## Files

| File | Bytes | What it proves |
|---|---|---|
| `geo_probe.txt` | 5738 | GEO 2.0 is a hard 404; DOC 429s are load-shedding, not rate-driven |
| `gkg_sample.csv` | 120117 | GKG 2.1 record shape across all five location types |
| `gkg_translation_sample.csv` | 39766 | Translation-stream record shape and `V2.1TRANSLATIONINFO` |
| `gkg_lastupdate.txt` | 319 | The 15-minute file pointer used for discovery |
| `doc_artlist_1h_truncated.json` | 125250 | DOC silently truncates a 1h window to one 15-minute slot at the 250 cap |
| `doc_artlist_historical_60d.json` | 2762 | DOC explicit historical windows work at 60 days back |
| `doc_artlist_historical_2y.json` | 2425 | DOC explicit historical windows work at 2 years back |

## Provenance

### `geo_probe.txt`

A recorded `curl` session with full response headers. Contains, in order:

1. Five GEO 2.0 requests, all `404 Not Found` from `Server: GDELT Server`:
   - `https://api.gdeltproject.org/api/v2/geo/geo?query=protest&format=GeoJSON&mode=PointData&timespan=1d`
   - same with `mode=ADM1`
   - same with `format=CSV&mode=Country`
   - same as the first but over plain `http://`
   - the bare endpoint `https://api.gdeltproject.org/api/v2/geo/geo`
2. A DOC control (`429` at the time of capture).
3. A `tv/tv` control returning `200 OK` — this is the **valid sibling control**
   proving the host and `/api/v2/` path prefix serve normally while `geo/geo`
   does not.
4. A second DOC control after backoff, and the 429 cooldown/intermittency runs
   described below.

Additional GEO paths probed during the spike but not retained in the capture
(all 404): `api/v2/geo`, `api/v2/geo/geo.php`, `api/v1/geo/geo`, and the
`PointData` mode with a browser `User-Agent` plus a `Referer` header. The 404
was also reproduced over a second, independent network path.

### `gkg_sample.csv`

Eight rows lifted verbatim (no field edits, tab delimiters intact) from
`http://data.gdeltproject.org/gdeltv2/20260819041500.gkg.csv.zip`
(781 rows, 9,753,591 bytes uncompressed). Rows were selected to cover:

- one row per `V2ENHANCEDLOCATIONS` location type 1–5
  (COUNTRY, USSTATE, USCITY, WORLDCITY, WORLDSTATE), each with ≤ 12 locations
  so the row stays readable;
- one row with **no** location field at all (166 of 781 rows are like this);
- the densest row in the window (128 location mentions);
- record 0 of the file, for a stable anchor.

### `gkg_translation_sample.csv`

Three rows lifted verbatim from
`http://data.gdeltproject.org/gdeltv2/20260819041500.translation.gkg.csv.zip`,
covering Bengali, Greek, and Gujarati source languages so the
`V2.1TRANSLATIONINFO` `srclc:` subfield and the `-T` record-id suffix are both
visible.

### `gkg_lastupdate.txt`

`http://data.gdeltproject.org/gdeltv2/lastupdate.txt` as served at
`20260819041500`. Three lines: export, mentions, gkg — each `size hash url`.

### `doc_artlist_1h_truncated.json`

```
GET https://api.gdeltproject.org/api/v2/doc/doc
  query=(protest OR unrest OR flood OR earthquake OR wildfire OR election OR
         strike OR narcotics OR trafficking OR overdose)
  mode=artlist  format=json  maxrecords=250  sort=datedesc
  startdatetime=20260818000000  enddatetime=20260818010000
```

This is `DEFAULT_QUERY` from `crates/source-gdelt/src/lib.rs` over a **one-hour**
window. All 250 returned articles carry `seendate` `20260818T011500Z` — a single
15-minute slot, and one *outside* the requested window. See the finding for what
that means.

### `doc_artlist_historical_60d.json` / `doc_artlist_historical_2y.json`

The same query at `maxrecords=5`, with explicit `startdatetime`/`enddatetime`
60 days and ~2 years before capture. Both return real articles with `seendate`
inside the requested window, which is what settles the "do explicit historical
windows work at all" question.

## Checksums

Regenerate with `sha256sum *` in this directory.

```
e4c95e76d513985e  doc_artlist_1h_truncated.json
849d2f56e92244c2  doc_artlist_historical_2y.json
18949b3a77664b9f  doc_artlist_historical_60d.json
8061db0529fa241b  geo_probe.txt
5f07b8dc69fb5b67  gkg_lastupdate.txt
308a0485728eaef9  gkg_sample.csv
2ce069dfb27a41a3  gkg_translation_sample.csv
```
