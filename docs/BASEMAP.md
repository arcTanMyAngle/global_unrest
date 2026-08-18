# Slippy-tile basemap — design record

**Status: design only. Nothing here is implemented.** This document settles the
policy questions that have kept the tile basemap deferred since M3
([ROADMAP.md](ROADMAP.md) M8, [VISUALIZATION.md](VISUALIZATION.md) V3 item 9).
Implementation is phased at the end and is not authorized by this document
existing.

Provider terms below were read on **2026-08-17** and are cited by URL. Terms
change; re-read every cited policy before the first line of network code, and
update this file in the same change if any of them moved.

## Decisions at a glance

| Question | Decision |
|---|---|
| Projection | Keep equirectangular. Consume tiles that are **already** EPSG:4326, so there is no reprojection step. |
| Provider | NASA GIBS WMTS `epsg4326`, keyless, static shaded-relief layer. No provider is a hard dependency: the vector basemap remains the map. |
| Web-Mercator (XYZ) tiles | Not in the initial scope. Possible later via a row-subdivided warp mesh (phase 4), never by reprojecting the renderer. |
| Offline | Tiles composite **over** the existing vector basemap, which is always painted. A missing tile reveals the vector map. No blank world, no checkerboard, no half-drawn world. |
| Cache | `ProjectDirs::cache_dir()/tiles/…`, bounded and LRU-evicted. **Never** in `data_dir`, never in DuckDB, never in a snapshot or export. |
| Toggle | Off by default, in the map's layer toggles, with the network cost stated at the toggle and in Settings. `LES_ONLINE=0` silences it. |
| Rendering | New `renderer::TileLayer` on the existing `MeshCache` / `affine_key` pattern; a bounded number of textured quads per frame. Fetch, disk I/O, and JPEG decode all on a dedicated worker thread. |
| Precision | Imagery is scrimmed and label-free, the zoom cap does not move, and the layer adds no records. The point-vs-region contract is untouched. |

---

## 1. Projection

### What the renderer actually does today

`geo_utils::MapViewport` is equirectangular (plate carrée) and its projection
is **affine in lon/lat**: `x = a·lon + b`, `y = c·lat + d`
([geo-utils/src/lib.rs](../crates/geo-utils/src/lib.rs), `Affine`). Every
renderer layer is built on that one fact. `GeoMesh` stores vertices in lon/lat
and `GeoMesh::to_mesh` turns a viewport change into one mul-add per vertex;
`MeshCache` keyed by `affine_key` means an idle map re-tessellates nothing;
`GraticuleLayer` draws meridians as exactly-vertical and parallels as
exactly-horizontal screen lines *because* the projection is affine
([VISUALIZATION.md](VISUALIZATION.md) V3). Zoom runs from
`MAX_DEG_PER_PX = 1.0` to `MIN_DEG_PER_PX = 0.002` (≈ 200 m/px at the equator).

### Can XYZ/WMTS tiles be composited under it without reprojection?

It depends entirely on the tile's own CRS, and the two common cases are not
close to each other:

- **EPSG:4326 tiles (plate carrée, WMTS `WorldCRS84Quad`-style grids).** Yes,
  for free. Such a tile's footprint is a lon/lat rectangle, and the current
  affine maps a lon/lat rectangle to an axis-aligned screen rectangle. One
  textured quad per tile, four vertices, exact — no resampling, no warp, no
  per-frame geometry beyond a bounded handful of quads. This is the same
  arithmetic the existing layers already do.
- **EPSG:3857 tiles (Web Mercator — the XYZ default, OSM/Carto/Mapbox/Esri).**
  No. Mercator's `x` is linear in longitude, so horizontally a 3857 tile still
  lands on an axis-aligned screen span, but `y = ln(tan(π/4 + φ/2))` is
  nonlinear in latitude. Drawing such a tile as a single quad stretches the
  image wrongly inside the tile — visibly so at low zoom, where one tile spans
  tens of degrees.

So the projection question decides "cheap or a rewrite", and the answer is:
**cheap, if we choose a provider that serves 4326.** One does, keylessly (§2).

### The honest options for Mercator-only tiles, if one is ever required

1. **Warp mesh (recommended if it comes up; phase 4).** Subdivide each tile
   vertically into *K* rows. Row boundaries are placed at exact latitudes
   computed by inverse-Mercator, and the texture V coordinate is interpolated
   linearly inside a row. Cost is `K+1` vertices per tile column instead of 2,
   still a bounded per-frame constant, still no tessellation of *data*
   geometry. *K* is chosen by measured screen error, not by taste: a tile's
   latitude span is `180°/2^z`, so the within-tile distortion collapses as zoom
   deepens — *K* is large only for the top two or three levels and can be 1–2
   below that. The acceptance test is a unit test asserting max screen error
   < 0.5 px across the zoom ladder and up to |φ| = 80°.
2. **Reproject the renderer to Web Mercator.** *Rejected.* It is technically
   available — Mercator is also affine, in (lon, mercY) rather than (lon, lat)
   — so `Affine` would survive, but everything that feeds it changes: H3 cell
   boundaries need per-vertex `mercY` at tessellation time, the graticule's
   parallel ladder becomes non-uniform, `unproject` needs an inverse, the poles
   clip at ±85.05°, and hit-testing, fly-to easing, and label placement all
   move. Worse than the work: Mercator inflates high-latitude area, so an H3
   cell in northern Russia would paint several times the visual weight of an
   equal cell in the Sahel. This app's whole visual argument is that a shaded
   region is an honest statement of coarse precision; a projection that makes
   the coarse regions *louder* the further from the equator they are is a
   quiet lie. Not worth a basemap.
3. **Server-side reprojection (a WMS with `CRS=EPSG:4326`).** Available from
   some providers and correct, but it converts every pan into a bespoke
   server-rendered request — no shared tile grid, no cacheability, and it is
   exactly the usage pattern free providers ask you not to generate. Rejected
   as a default; acceptable only against a self-hosted server.

### Why not the `walkers` crate

[VISUALIZATION.md](VISUALIZATION.md) V3 item 9 named `walkers` as the likely
route. Rejected: `walkers` owns its own Web-Mercator viewport, its own tile
cache, and its own plugin/overlay model. Adopting it means handing over
`MapViewport` — the type every layer, the fly-to animation, the hit-testing,
and the heatmap rollup are written against — and inheriting the Mercator
distortion above. It remains worth reading as prior art for its HTTP/tile-cache
shape; it is not worth adopting as the map.

---

## 2. Provider policy

Requirements this project imposes, in order:

1. **No API key in the binary.** A key shipped in a distributed desktop build
   is a published key, and product rule 5 keeps credentials in the environment.
   A provider is only acceptable keyless, or with a key the *user* supplies via
   an environment variable — never one of ours.
2. **On-disk caching must be permitted.** The desktop must stay usable on a
   flaky connection; a provider whose terms forbid persisting tiles is
   disqualified, not a maybe.
3. **Attribution must be renderable verbatim.** Whatever the terms require goes
   on the map surface and into the About screen (S4's `SourceAttribution`
   table is the right home for the string).
4. **4326 preferred**, per §1.

| Candidate | CRS | Key | Caching | Attribution | Verdict |
|---|---|---|---|---|---|
| **NASA GIBS** WMTS ([docs](https://nasa-gibs.github.io/gibs-api-docs/access-basics/)) | 4326 **and** 3857, 3413, 3031 | None | NASA open-data posture; no prohibition on client caching | Acknowledgment requested (below) | **Primary** |
| OSM standard tiles ([policy](https://operations.osmfoundation.org/policies/tiles/)) | 3857 only | None | Local cache *required*; **bulk download, pre-seeding, tile archives, and "download for offline" features prohibited** | "© OpenStreetMap contributors" | **Rejected** |
| Carto / Stadia / Mapbox / MapTiler / Thunderforest | 3857 | Account or key | Restricted by contract, tier-dependent | Required | **Rejected as default**; only ever with a user-supplied key |
| Esri World Imagery | 3857 | None for basemap use, but terms bind | Terms restrict caching and derivative use | Required | **Rejected** |
| EOX Sentinel-2 cloudless | 4326 available | None | Permissive-ish | Required, and the free layer's licence is non-commercial — **verify before relying on it** | Second choice, unverified |
| Natural Earth II raster, tiled locally and bundled | 4326 by construction | n/a | n/a — no network at all | Public domain, none required | **Keep as the zero-network fallback** |

### NASA GIBS — why it wins

- **It serves EPSG:4326 natively**, which removes the entire reprojection
  question. RESTful WMTS:
  `https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/{Layer}/default/{Time}/{TileMatrixSet}/{z}/{y}/{x}.{ext}`.
- **No key.** Nothing to ship, nothing to leak.
- **Resolution ladder fits our zoom range.** The published EPSG:4326 tile
  matrix sets top out at 0.5625°/px at level 0 and halve per level; the `250m`
  set's finest level is 0.002197°/px, within ~10% of the app's
  `MIN_DEG_PER_PX = 0.002`. The app therefore never zooms meaningfully past the
  imagery, and never needs a level the provider does not publish.
  **Do not assume the tile pixel size or the level-0 matrix dimensions** — the
  prose docs do not state them and the resolution table alone does not close
  the arithmetic. Read `TileWidth`, `MatrixWidth`, `MatrixHeight`, and
  `TopLeftCorner` out of GetCapabilities and write the tile-index math against
  those values, with a test that a known lon/lat lands in the expected tile.
- **Attribution.** NASA requests, verbatim: *"We acknowledge the use of imagery
  provided by services from NASA's Global Imagery Browse Services (GIBS), part
  of NASA's Earth Science Data and Information System (ESDIS)."* That string
  goes in the About screen; the map surface carries the short form
  ("Imagery: NASA GIBS").
- **Layer choice is an honesty decision, not an aesthetic one.** Use a
  **static** layer — shaded relief with bathymetry — not a dated true-colour
  layer. A true-colour mosaic stamped with today's date, sitting under today's
  events, invites the reading that the imagery *shows* the event. It does not.
  If a dated layer is ever offered, the imagery date must be rendered in the
  legend next to the toggle. Confirm the exact layer identifier and its
  available tile matrix set in GetCapabilities before wiring it.

### Why OSM standard tiles are disqualified

Not because of attribution — that is trivial — but because the policy and this
product contradict each other. The policy requires local caching yet prohibits
"pre-seeding large areas or multiple zoom levels", "building tile archives for
distribution", and "any 'download for offline' features", and directs
applications that need offline capability to self-host or use a provider that
permits it. A live-data desktop that must remain usable with no network is that
application. It is also 3857-only, so it would cost the warp path (§1) on top.
Interactive browse-only use would arguably be within the policy, but designing
an offline-capable map on a provider that forbids offline use is building on a
rule we are already planning to lean against.

---

## 3. Offline and failure behavior

The rule is one sentence: **the vector basemap is always painted, and tiles are
composited on top of it.**

That single ordering decision answers every failure mode without any new state:

- No network, no cache → zero tiles draw → the map is exactly today's map.
- Partial cache → the covered area shows imagery, the rest shows the vector
  land fill in the same colours as always. Not a gap, not a checkerboard; the
  map is continuous everywhere and merely more detailed in places.
- Provider outage, DNS failure, TLS failure, 429 → the fetcher backs off and
  the map degrades to the case above. Failures are counted and surfaced in the
  Settings source-state list as a tile-layer status line, never as a modal.
- Mid-pan cache eviction → see §4; eviction may not touch a tile in the current
  visible set, so the visible world cannot lose pixels it is drawing.

The cost of always painting the vector fill under the tiles is one extra cached
mesh draw — the mesh is already built and cached (`BasemapLayer`'s `MeshCache`),
so this is a draw call, not a rebuild. Paying it buys the removal of an entire
category of failure state, an "is the imagery loaded enough" heuristic, and any
possibility of a blank world.

Consequences for the layer order in `MapView::show`:

```
background → graticule → vector basemap fill → TILES → tile scrim
           → vector basemap borders → heat → alerts → markers → halos
           → focus dim → labels → selection outline
```

Country borders move **above** the tile layer — imagery would otherwise bury
them, and the border hierarchy is a V3 encoding, not decoration. That is a
signature change on `BasemapLayer::paint` (fills and borders become separately
callable), not a rewrite.

---

## 4. Cache design

**Tiles are third-party cached bytes, not project data.** They do not go in
`data_dir`, they do not go in DuckDB, and they never appear in an export, a
Parquet snapshot, or an API response. Putting them in the store would drag them
into retention pruning, the export path, and the snapshot contract — three
places that must carry only records this project collected and can attribute.
The storage actor is not involved in this feature at all.

- **Location:** `directories::ProjectDirs::cache_dir()` — deliberately *not*
  `data_local_dir()`, which is where `signals.duckdb` and `settings.sqlite`
  live ([app.rs](../apps/global-signal-desktop/src/app.rs)). Layout
  `tiles/<provider>/<layer>/<matrixset>/<z>/<x>/<y>.<ext>`, so a provider or
  layer change is a directory, and "clear the cache" is a directory removal.
  Overridable with `LES_TILE_CACHE_DIR` for the same reasons `LES_DATA_DIR`
  exists.
- **Bound:** default **256 MiB**, user-adjustable in Settings, enforced as a
  byte total. The index (path, size, last-access) is built by one directory
  scan on the tile worker when the layer is first enabled — never on the UI
  thread — and maintained in memory thereafter.
- **Eviction:** LRU by last access, batched, and run on the tile worker only
  after a write pushes the total over the bound. Two hard rules: evict down to
  a low-water mark (~85% of the bound) so eviction is not re-triggered by every
  subsequent tile, and **never evict a tile in the current visible set** — the
  worker holds the last visible-tile list, so a mid-pan overflow evicts the
  furthest-back tiles, not the ones under the cursor. Without the low-water
  mark and the visible-set exclusion, a pan across a full cache degenerates
  into evict-refetch thrash on the network *and* the disk.
- **Freshness:** honour `Cache-Control`/`Expires`, with a 30-day floor for a
  static imagery layer. Serve stale immediately and revalidate in the
  background: a stale tile is always better than a hole, and this keeps pans
  smooth on a slow link.
- **Clearing:** a "Clear tile cache" action in Settings, showing the current
  size. The user started this traffic and this disk usage with a toggle; they
  get an unambiguous way to end both.
- **Texture memory is the other bound, and it is the tighter one.** A 512×512
  RGBA texture is 1 MiB resident. A naive "keep every decoded tile" policy
  reaches hundreds of MiB of VRAM within a few minutes of panning. Cap resident
  `TextureHandle`s at **96** (~96 MiB) in an LRU keyed by tile id, and cap
  texture *uploads* at ~4 per frame so a burst of arrivals cannot spike frame
  time. State both numbers as named constants, not literals at the call site.

---

## 5. The user toggle

- **Default: off.** The desktop is live-data-only but it is not chatty by
  default about anything the user did not ask for, and this toggle starts
  recurring traffic to a third party as a side effect of *panning* — which is
  not a per-action consent the way the Media page's Search button is. Off is
  the honest default.
- **Where:** with the existing layer toggles in the map's top bar (next to
  graticule, labels, alerts, halos) — it is a layer, and it belongs with the
  layers. Settings carries the durable half: provider, cache size and clear
  button, and the tile-fetch status/error line.
- **What the user is told,** at the toggle and again in Settings, in one plain
  sentence: turning this on downloads map imagery from NASA's Global Imagery
  Browse Services as you pan and zoom, and caches it on this computer; the app
  sends the map area you are looking at and nothing else. No records, no
  queries, no identifiers.
- **Persistence:** through the existing settings path
  (`crates/storage/src/settings.rs`) alongside the other layer toggles, with a
  migration if the schema moves.
- **`LES_ONLINE=0` silences tile traffic**, the same way it pauses scheduled
  ingest. Panning is not an explicit per-action request, so it does not get the
  Media page's exemption. The toggle may stay on; cached tiles keep drawing;
  nothing is fetched.
- **No fetching while the window is unfocused**, and no speculative prefetch
  beyond one ring of tiles around the viewport.
- **A unique `User-Agent`** naming the app and its repository on every tile
  request. OSM's policy requires it; it is good manners everywhere else, and it
  is what lets a provider talk to us instead of blocking us.

---

## 6. Rendering integration

New `renderer::TileLayer`, following the shape every other layer already has.

**Per-frame cost is a bounded constant, and no data geometry is touched.**
A 4326 tile is one quad: four vertices, `WHITE_UV` replaced by a real UV
rectangle, and `epaint::Mesh::texture_id` set to that tile's texture. Because
`epaint::Mesh` carries a single texture id, this is one `Mesh` per visible
tile rather than one merged mesh — which is why `TileLayer` produces
`Vec<Mesh>` directly instead of going through `GeoMesh` (which has no UVs and
assumes one draw). Visible tiles are capped at **48**; if the level selection
would exceed the cap, drop to the next coarser level rather than skipping
tiles, so the world is never partially covered by a rendering decision (as
opposed to a network one). The whole set is cached in a `MeshCache` keyed by
`affine_key(aff) ^ tileset_generation`, so an idle map rebuilds nothing —
identical to `HeatmapLayer` and `BasemapLayer`.

**Level selection:** from `aff`, take the viewport's `deg_per_px` and choose
the finest published level whose resolution is no finer than the viewport's,
so we never upload more pixels than the screen can show. Zoom is clamped by
`MIN_DEG_PER_PX`, so the level is always inside the provider's ladder.

**World copies:** reuse `visible_world_offsets` exactly as the other layers
do. A tile column index wraps modulo the level's matrix width, so the wrapped
copy at ±360° reuses the *same* texture — the antimeridian costs no extra
network traffic and no extra VRAM.

**Threading:** a `tiles` worker in the desktop app with the same shape as
[`media.rs`](../apps/global-signal-desktop/src/media.rs) and
[`digest.rs`](../apps/global-signal-desktop/src/digest.rs) — a dedicated thread
with a current-thread Tokio runtime, an `std::sync::mpsc` reply channel, the
`wake: impl Fn()` repaint callback, and the `#[cfg(feature = …)]` stub-module
pattern so the worker body carries no `cfg` arms. It never opens storage, like
the other two. The UI sends the visible tile list (a *replacement*, not a
queue, so a fast pan cancels stale work instead of accumulating it); the worker
does cache lookup, HTTP, and JPEG decode to `egui::ColorImage`, capped at 2
concurrent requests. The UI thread's only work is draining the channel and
calling `ctx.load_texture` — bounded per frame as above. **No filesystem read,
no decode, and no network call ever happens on the UI thread**, which is the
same rule the storage `Reply<T>` pattern enforces for queries.

**New dependency:** a JPEG decoder. `image` with default features off and only
`jpeg` enabled, or `zune-jpeg` directly. Both are pure Rust, which keeps the
deliberate rustls/pure-Rust posture intact; `cargo deny check` and a licence
read are part of phase 2, and the choice belongs in
[ENGINEERING_NOTES.md](ENGINEERING_NOTES.md) if it costs anything. `reqwest` is
already a workspace dependency with `rustls-tls`; nothing new there.

**Feature gating:** `tiles-live` on the desktop, joining the existing
desktop-only features in the CI feature matrix and the no-default-features
union command in [CLAUDE.md](../CLAUDE.md). Without it the toggle is present
and disabled with an honest reason, exactly as the Media page behaves without
`media-live`.

**Acceptance:** the existing perf smoke budget holds unchanged (≥ 30 fps at
10k events), plus a new bounded-cost test asserting the visible-tile count
never exceeds the cap across the zoom ladder and that the mesh cache does not
rebuild on an idle frame.

---

## 7. Precision — how the visual hierarchy holds

The tile layer adds **no records**, so the point-vs-region contract itself is
untouched: only city/exact records still become markers, and coarser records
still shade regions. The risk is perceptual, and it is real — photographic
imagery reads as survey-grade truth, and a country-precision hexagon floating
over a crisp satellite image invites exactly the over-reading
[SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md) exists to prevent.

Four things hold the hierarchy:

1. **A scrim.** A translucent dark rect (`MapStyle::tile_scrim`, ≈ 45% of the
   background colour) is painted over the tile layer and under every data
   layer. The imagery becomes terrain context — coastlines, relief, desert vs
   forest — and stops competing with the heat ramp and the marker palette for
   the same attention. The existing ramp separation tests
   (`alert_ramp_is_monotone_and_never_collides_with_the_heat_ramps`) keep
   guarding the data palettes; the scrim is what keeps the *basemap* out of
   that argument.
2. **No labels, no roads.** The chosen GIBS imagery layer carries no place
   names or street geometry. This is a substantive advantage over an OSM raster
   layer, where street-level labels beside a country-precision shaded region
   are the false-precision failure in a single screenshot.
3. **The zoom cap does not move.** `MIN_DEG_PER_PX` stays at 0.002 (≈ 200 m/px).
   The map cannot be zoomed to a block where a shaded country region would look
   absurd, and the imagery ladder does not out-resolve the cap anyway (§2).
4. **A static, undated layer** (§2), so the imagery cannot be read as a
   photograph *of* the event. If that ever changes, the date renders in the
   legend.

Additions to the "How to read this map" overlay
([how_to_read.rs](../apps/global-signal-desktop/src/how_to_read.rs)), in its
limits section, of equal weight to the rest: the basemap is imagery for
orientation only; it is not evidence about any record's location, and it does
not make a shaded region more precise than the region it shades.

---

## 8. Implementation plan

Each phase is independently shippable and independently revertible. A phase
that does not earn its keep is where this stops.

**Phase 1 — compositing, no network.** `renderer::TileLayer`, level selection,
world-copy wrapping, the scrim, the layer reorder (borders above tiles), the
toggle wired to a small set of tiles loaded from a local directory. Proves the
visual hierarchy, the vector-under-tiles degradation, and the frame budget with
zero network code and zero new terms to honour. Tests: tile-index math against
GetCapabilities values, visible-tile cap, cache non-rebuild on idle.

**Phase 2 — live fetch, session-only.** The `tiles` worker behind `tiles-live`,
HTTP with the app `User-Agent`, JPEG decode, the resident-texture LRU and the
per-frame upload cap. No disk cache yet: quitting forgets everything. Ships a
working online basemap, and every failure path already degrades correctly
because of phase 1.

**Phase 3 — disk cache and Settings.** The cache directory, the byte bound,
LRU eviction with the low-water mark and visible-set exclusion, stale-while-
revalidate, the size display and clear button, the tile status line, the About
attribution entry, and the `SourceAttribution` row.

**Phase 4 — Web-Mercator warp (only on demand).** The row-subdivided warp mesh
and its error test, needed only if a 3857-only provider ever becomes necessary.
Do not build it speculatively.

Documentation lands with each phase, not after:
[VISUALIZATION.md](VISUALIZATION.md) (the layer and its hierarchy),
[SAFETY_AND_PRIVACY.md](SAFETY_AND_PRIVACY.md) (a third-party network leg the
user opts into, with what is and is not sent), [README.md](../README.md) (the
toggle and its traffic), [ARCHITECTURE.md](ARCHITECTURE.md) (the new worker),
[DEVELOPMENT.md](DEVELOPMENT.md) (the feature flag and env vars), and
[CHANGELOG.md](../CHANGELOG.md).

## 9. Do not build this if…

- …the provider's terms at implementation time forbid persisting tiles to disk.
  The offline requirement is not negotiable; the basemap is.
- …it requires an API key shipped inside the binary. A user-supplied key in an
  environment variable is the only acceptable form.
- …it requires reprojecting the renderer to Web Mercator. That trades an
  honest equal-area-ish presentation of coarse regions for a prettier
  backdrop, and this project does not make that trade.
- …tiles cannot be composited without touching the storage actor, the DuckDB
  file, or the snapshot/export path.
- …the visible-tile count, texture residency, or per-frame upload count cannot
  be bounded by a constant. An unbounded overlay loop is the one thing the
  rendering contract has never allowed.
- …any part of it needs a network call, a filesystem read, or an image decode
  on the UI thread.
- …it cannot ship off by default, or cannot be silenced by `LES_ONLINE=0`.
- …it needs a non-pure-Rust dependency, or a bundled data file whose licence
  and repository-size cost has not been decided explicitly.
