# oxrad: Architecture and Development Roadmap

## Purpose

oxrad is a storm-chaser-grade weather radar viewer. Its primary users are storm chasers, emergency managers, and weather spotters making real-time decisions during severe weather events. A display error, a frozen screen, or a corrupted bin placement is a safety failure, not a UI inconvenience. Every design decision in this document is made with that constraint as the first priority.

oxrad started because the viewers available are slow, crash-prone, and freeze the display on settings changes. oxrad is the fast, stable, responsive one. The bar for done is a viewer good enough for anyone to use anywhere in the world where a national weather service publishes open radar data. oxrad is open software under GPLv3, and stays that way: weather preparedness belongs to everyone.

---

## Scope

A free, open-source desktop weather and radar application for Windows, macOS, and Linux. It shows current weather, hourly and daily forecasts, radar, and severe-weather information for locations the user chooses. It renders radar from sources whose data is openly published and whose format oxrad decodes, and carries that radar forward into nowcasting. A source is a catalog entry (see Radar Sources). NEXRAD (United States) is the first radar source. ODIM_H5 volumes, the format used by European national services, are the second, with Germany's DWD as the first target. Adding a country is adding a catalog entry and, if the format is new, a decoder. No subscription or paid feature is required; a source that requires one is outside scope.

---

## The Product

The application answers both everyday and urgent weather questions: what it is like now, what is expected later, where weather is, and when it may arrive. Weather and radar are first-class views for the same selected location. The weather view gives a readable current summary and hourly and daily forecasts; the radar view gives a map, recent scans, and hazards. Switching views preserves the location. Forecasts and radar-derived nowcasts remain distinct, with their source and time visible.

The radar view is a storm-spotter and meteorology-grade workspace, not a simplified backdrop for the weather view. It exposes full-resolution radar products and available elevation angles, precise measurements and units, scan age, warnings, geographic context, and the diagnostic information needed to interpret missing or degraded data. Product and tilt selection, animation, and radar inspection remain directly reachable during a live update. Ordinary weather summaries do not replace measured radar products or conceal their limitations. Severe-weather workflows and everyday forecasts are both part of the finished product.

The application opens on the radar map at the home location or last site, showing the most recent volume. With no saved location, the site picker opens over the map. A weather-view control opens current conditions and the forecast for the selected location; search, home, location, and a map pin select that location. The map carries the radar layer, place labels, the color legend with units, the frame time, and the user's position when available. Stepping through recent scans and playing them is a first-class control. A glance answers what is falling, where it is relative to the user and named places, which way it is moving, and how old the picture is. A second step answers the value at a point, the product and tilt, and the forecast for a point.

Every displayed measurement carries its place, time, and units. A transient failure names the operation, the site, and the cause, and offers a retry. The last good volume stays visible with its age labeled until a new one arrives. The application never presents stale data as current, an empty map as clear weather, or below-threshold gates as precipitation.

The map fills the window. A left rail holds the site picker, product selector, tilt selector, and layer toggles. A bottom bar holds the time controls, the frame timestamp, and the status line. The legend sits in a map corner. The forecast panel docks to the right and collapses. A narrow window collapses the rails to icon buttons that open over the map. Every control has a keyboard path and a visible focus state.

The application is used outdoors, one-handed, in sunlight, on a poor connection. Controls are reachable without a menu dive, targets are large enough for a gloved finger or a trackpad, text contrast is at least 4.5:1, and nothing blocks the map during a live update.

A feature that exists but cannot be reached in that loop is not delivered.

---

## Design Principles

**No hardcoded geophysical constants in rendering paths.** Gate geometry, azimuth spacing, site coordinates, and elevation angle are all read from the data. If the data does not supply a value, the bin or radial is rejected and counted, not substituted with a default.

**All floating-point math is computationally verified.** No formula in this codebase is derived from memory or inferred from training data. Every coordinate transform, projection, and range calculation is sourced from primary references (Karney 2013, Snyder USGS PP-1395, Doviak & Zrnić, NEXRAD ICD, OSM slippy map spec) and verified by independent numerical computation before being locked into the codebase. If a formula is in dispute, it is tested against known reference values before it is used in production.

**Errors are always visible.** A failure the user cannot see is worse than a crash. Every network error, parse failure, and data anomaly surfaces as a labeled UI state. The application never presents silence as success.

**The UI thread never blocks.** All I/O, decoding, and computation run on background workers. The UI remains responsive and interactive regardless of what the data pipeline is doing. Settings changes take effect without freezing the display.

**No unauthorized decisions.** The codebase does not silently normalize, substitute, default, or approximate. When a value is unavailable, the affected feature is disabled with a visible reason.

**Source-neutral core.** Decoders convert every source into the same `RadarVolume`. Rendering, hover, legends, animation, mosaic, and nowcasting see only that type.

**Absence has a name.** Where an empty display could mean nothing is there, the feed is stale, or the viewport is outside coverage, each case is a separate visible state.

---

## Coordinate Mathematics

All coordinate math is locked to verified implementations. Every value cited here appears in Reference Values below, which is the source of truth for the absolute-value tests.

**Geographic placement.** Bin corners are placed with the geodesic direct problem on the WGS84 ellipsoid (Karney 2013, *Algorithms for geodesics*): given the site origin, an azimuth, and a ground range, it returns latitude and longitude. The geodesic inverse converts a cursor position back to ground range and azimuth from the site, for hover and overlay intersection.

**Ground range from slant range.** Slant range from the radar is not ground range on the map. Beam height and ground range follow the standard 4/3 effective-radius model (Doviak & Zrnić). With `θ` the sweep elevation angle, `r` the slant range, and `ae` the effective radius:

- `h = sqrt(r² + ae² + 2·r·ae·sin θ) − ae`
- `s = ae · asin(r·cos θ / (ae + h))`

`h` is beam height above the antenna and `s` is ground range. The slant-to-ground difference is 18.7 m at 100 km at 0.5° and 487 m at 100 km at 5°, and reaches 1.47 km at 230 km at 5°.

**Web Mercator tile coordinates, forward and inverse.** Used for vector tile fetching and screen positioning (EPSG:3857). Latitude is clamped to ±85.0511287798066° before computation.

**Beam geometry.** Each range bin is a quadrilateral defined by its near and far ground range edges and its left and right azimuth edges. All four corners are placed independently with the geodesic direct problem. Gate near and far edges are derived from the gate center (as reported in the radial header) offset by half the gate interval in each direction.

**Earth constants.** Constants are named for what they are and are not interchangeable:

- **WGS84 ellipsoid** (a = 6378137 m, 1/f = 298.257223563): geographic placement and surface distance.
- **Mean Earth radius** (6371.0 km): the radius of the standard beam propagation model.
- **Effective radius** `ae` = 4/3 × 6371.0 km = 8494.6667 km: beam height and slant-to-ground conversion. It is computed from the mean radius and stored unrounded.

### Reference Values

The KPAH origin below is the site's published coordinate, used as a test input only; no rendering path depends on it. All values were computed independently of the implementation with GeographicLib (geodesics) and PROJ (Mercator), and cross-checked against closed-form expressions.

**Geodesic direct, WGS84, KPAH test origin 37.06833°N 88.77194°W, ground range 75 km**

| Azimuth | Latitude | Longitude |
|---|---|---|
| 0° | 37.744095 | −88.771940 |
| 45° | 37.544662 | −88.171837 |
| 90° | 37.065332 | −87.928621 |
| 135° | 36.588961 | −88.179319 |
| 180° | 36.392487 | −88.771940 |
| 225° | 36.588961 | −89.364561 |
| 270° | 37.065332 | −89.615259 |
| 315° | 37.544662 | −89.372043 |

Tolerance for absolute tests: 5×10⁻⁷ degrees. Round-trip tolerance (direct then inverse, eight azimuths at 75 km): 1 mm in distance, 10⁻⁹ degrees in azimuth.

**Slant to ground range and beam height, `ae` = 8494.666667 km, antenna height zero**

| Elevation | Slant (km) | Ground (km) | Height (km) | Slant − ground (m) |
|---|---|---|---|---|
| 0.5° | 50 | 49.99495 | 0.58346 | 5.05 |
| 0.5° | 100 | 99.98130 | 1.46113 | 18.70 |
| 0.5° | 230 | 229.88078 | 5.11928 | 119.22 |
| 5.0° | 50 | 49.78363 | 4.50374 | 216.37 |
| 5.0° | 100 | 99.51281 | 9.29909 | 487.19 |
| 5.0° | 230 | 228.53021 | 23.12805 | 1469.79 |
| 19.5° | 50 | 47.03917 | 16.82084 | 2960.83 |
| 19.5° | 100 | 93.89135 | 33.90164 | 6108.65 |
| 19.5° | 230 | 214.81976 | 79.51712 | 15180.24 |

Tolerance: 1×10⁻⁵ km.

**Placement error of a sphere (R = 6371.0 km) against the WGS84 geodesic, worst case over azimuths in 5° steps**

| Ground range | Worst error |
|---|---|
| 75 km | 175.5 m |
| 230 km | 538.7 m |
| 460 km | 1080.0 m |

**Web Mercator, KPAH test origin as above**

| Zoom | Tile x | Tile y |
|---|---|---|
| 4 | 4.054580 | 6.223877 |
| 10 | 259.493148 | 398.328116 |

At zoom 1, (0°, 0°) is tile (1.0, 1.0). The north-west corner of tile 0/0/0 is (85.0511287798066°, −180°). Tolerance: 10⁻⁶ tile units.

---

## Radar Sources

A source is a catalog entry plus a decoder. Catalog entries are TOML files bundled with the application and reviewed like code. They are fixed at build time. Each entry declares:

| Field | Content |
|---|---|
| Identity | Source id, kind (`radar`, `alert`, `forecast`, or `lightning`), region, display name |
| Format | `NexradLevel2`, `NexradLevel3`, or `OdimH5Volume` for radar entries |
| Endpoints | Host names and listing scheme. Every host has a row in the Network Policy table, and a source ships with its row. |
| Sites | Id, name, latitude, longitude, and the precision of those coordinates |
| Candidate radius | Distance used to choose which sites to fetch for a viewport. Rendering extent comes from the decoded data. |
| Attribution | License and attribution text for the attribution panel |
| Alert source | The id of the region's alert entry, where one exists |

**Catalog coordinates and header coordinates.** The catalog supplies coordinates for selection, nearest-site search, and mosaic candidacy, which all happen before any volume exists. Rendering uses the coordinates in the volume header. For every decoded volume the distance between the two is computed and shown in diagnostics, in yellow when it exceeds the precision the catalog declares for that site.

**Canonical products.** Decoders map source moments onto the canonical product set: Z, V, SW, ZDR, CC, KDP. For ODIM_H5 the mapping is DBZH to Z, VRADH to V, WRADH to SW, ZDR to ZDR, RHOHV to CC, and KDP to KDP. A canonical product the source does not publish is disabled with the visible reason "not provided by this source". ODIM `undetect` takes the `BelowThreshold` handling and `nodata` takes a distinct missing sentinel. Gain, offset, `nodata`, and `undetect` are read from each dataset's own attributes.

**Source-specific geometry.** Wavelength band, gate spacing, maximum range, and scan strategy are read from the data like everything else. The core carries no S-band or NEXRAD numbers.

---

## Dependencies

Each crate is chosen per concern and confined behind the one module that uses it, so replacing a crate changes that module only. The NEXRAD crates decode a single country's format, so they sit inside the NEXRAD decoder module and their model types stop at its boundary. Every added dependency is recorded in `docs/DEPENDENCIES.md` with its license, whether it performs network I/O, and a census of its `unsafe` code.

Versions live in `Cargo.toml` and `Cargo.lock` only. This document names the crate and the capability required.

| Concern | Choice | Basis |
|---|---|---|
| NEXRAD Level II parsing | `nexrad-decode` | Parses decompressed Archive II records against NOAA ICD 2620002AA and performs no I/O. MIT. |
| NEXRAD fetch | oxrad's fetch worker | `nexrad-data` bundles Archive II decompression with AWS S3 access. Its network client would sit outside the fetch worker and outside the Network Policy table. |
| Archive II decompression | selected in Milestone 1 | Streaming, pure Rust, no C build step. |
| Image output | oxrad's GPU renderer | `nexrad-render` produces `image` buffers. oxrad draws bins from geometry. |
| HDF5 container for ODIM_H5 and GLM | `hdf5-pure` | Pure Rust with no build script. Reads superblocks v0 to v3, object headers v1 and v2, contiguous, compact, and chunked storage, compound types, and variable-length strings. MIT. Accepted on fixtures in Milestone 18. |
| ODIM_H5 radar decoding | oxrad module over `hdf5-pure` | No Rust ODIM_H5 radar decoder turned up in a crate search. |
| Geodesics | `geographiclib-rs` | Direct and inverse geodesic solutions after Karney. The crate describes itself as a subset of GeographicLib, and direct and inverse are the subset in use. Accepted when it reproduces the Reference Values within tolerance. |
| Vector tile decode and style evaluation | selected in Milestone 2 | Pure Rust, MVT decode, MapLibre style expressions, fractional zoom. |
| HTTP, windowing and GPU, TOML, notifications | selected in the milestone that first needs each | Each runs under the Network Policy and the dependency record. |

---

## Data Model

### NEXRAD Level II

Level II is full-resolution dual-polarization volume scan data. It is the primary data source for storm chasing in the United States.

**Volume assembly.** A real-time volume on the AWS feed consists of approximately 55 chunks delivered over several seconds. A complete volume requires paginating the chunk list to exhaustion, ordering chunks by the sequence numbers exposed by the feed, and verifying the presence of start and end sentinel chunks before decoding. If the feed does not expose enough ordering or boundary information to establish completeness, the volume is rejected as incomplete and the feed limitation is reported. A partial or ambiguously ordered volume is never presented as complete. Decoding a partial volume and presenting it as complete is not permitted.

**Sweep selection.** A volume contains multiple elevation sweeps. The application selects sweeps by elevation number, not by list index. The lowest available elevation angle is the default. `first()` is not an acceptable sweep selector.

**Gate geometry.** For each moment and each radial, the following values are read directly from the radial header:
- Range to first gate center (converted from meters to kilometers)
- Gate interval (converted from meters to kilometers)
- Azimuth center and spacing

No fallback constants are permitted. If a radial does not supply valid geometry, it is skipped and counted in a diagnostic counter visible in the UI.

**Gate value handling.** `BelowThreshold` and `RangeFolded` are distinct from measured values. The decoded representation retains each gate's original range position and value category; omitting a gate from a display mesh must not shift subsequent gates inward. Range-folded gates use a distinct visible color. The show-below-threshold option displays those gates at the moment's minimum valid value (-32 dBZ for reflectivity), without treating them as measured echo.

Below-threshold gates are hidden in the default view and are never drawn as echo. The `Show below-threshold bins` configuration field reveals them, and the hidden count is reported in diagnostics. Range-folded gates draw in the distinct sentinel color.

**Site coordinates and height.** Radar site latitude, longitude, and antenna height are read from the volume header and stored in `RadarVolume`. No geographic constant is hardcoded in the rendering path.

### NEXRAD Level III

Level III products are derived, lower-resolution products distributed via NOAA's public HTTPS infrastructure. The data level toggle switches the fetch pipeline and updates the product selector to reflect only products available at the active level.

### ODIM_H5 volumes

Polar volumes in the OPERA ODIM_H5 format decode into the same `RadarVolume`. Sweeps, elevation angles, range start and scale, azimuth geometry, and time stamps come from the file's own `where` and `what` groups.

---

## Local Storage: `~/.oxrad`

All persistent application data lives under `~/.oxrad`. The directory is created on first run if it does not exist. Every subdirectory and file path is defined in a single `paths` module, and no path strings appear elsewhere in the codebase.

Caches are bounded by size as well as age. At the cap, the least recently used entries are evicted. The caps are configuration fields.

### Tile cache: `~/.oxrad/tiles/`

Vector tiles cached as individual files named by `{z}/{x}/{y}.mvt`. Tiles are fetched on demand and written to disk immediately. On subsequent requests the cached file is used without a network request if it exists and has not expired. Cache expiry follows the `max-age` directive from the tile host response headers, defaulting to 7 days if the header is absent. Entries older than 30 days are purged on startup. Total size is capped at 1 GiB by default. A PMTiles archive may serve as an offline tile source in place of the network fetch. If the cache directory is unavailable, the application falls back to memory-only caching for the session and surfaces a warning in the UI.

### Glyph cache: `~/.oxrad/glyphs/`

Signed-distance-field glyph ranges for map labels, fetched from the tile host and cached by font stack and range. Entries expire after 30 days. Sprites are bundled with the application.

### NEXRAD volume cache: `~/.oxrad/volumes/`

Decoded radar volumes cached by site and timestamp, named `{SITE}_{YYYYMMDD_HHMMSS}.bin` in a compact serialized format. The cache is used to populate the animation buffer on startup without a network fetch: if cached volumes exist for the active site, they are loaded immediately while a fresh fetch runs in the background. Cache entries older than 48 hours are purged on startup, and total size is capped at 2 GiB by default. Mosaic mode writes the latest volume per site and replaces the previous one, so a mosaic of many sites holds no history. The volume cache is supplementary and is never a substitute for a fresh fetch. Cached volumes that fail to deserialize are deleted and logged.

### Zone geometry cache: `~/.oxrad/zones/`

Forecast-zone and county-zone polygons used to resolve alerts that carry no polygon of their own. Zone boundaries are reference geography and change rarely. Entries expire after 30 days.

### Shapefile cache: `~/.oxrad/shapefiles/`

Census TIGER/Line shapefiles, downloaded the first time a United States source is active and stored permanently. Shapefiles are re-downloaded only when the user triggers it or the cached files fail to parse. Download progress and checksum verification results are surfaced in the UI.

### Error log: `~/.oxrad/errors.log`

All errors that are surfaced to the UI are also written to the error log with a timestamp, the active site, and the full error chain. The log is append-only, capped at 10 MB by rotating to `errors.log.1` when the limit is reached. The log exists for post-incident review and does not replace UI visibility. Errors that do not reach the UI do not appear only in the log. Place-search text and home-location coordinates are excluded from the log, and a failed search is logged as its status.

### User configuration: `~/.oxrad/config.toml`

User preferences persisted across sessions in TOML format. The configuration file is written only when settings change. If the file is absent, the application starts with the documented defaults and creates a config when settings are saved. If it is malformed or contains unrecognized keys, the application reports the issue and uses the documented defaults for affected settings for this session; it does not overwrite the existing file without the user's explicit action. The home location is stored as latitude and longitude in plain text. Defaults that depend on the operating system locale are derived once on first run and then written explicitly.

| Field | Default |
|---|---|
| Active source and site | none; first run shows the site picker |
| Data level | Level II |
| Product | Z |
| Elevation | lowest available, stored as elevation number |
| Map center and zoom | world view until a site is selected, then that site's location |
| Base map style | `muted` (`muted` or `dark`) |
| Overlays: boundaries, roads, alerts, lightning | on, on, on, off |
| Animation frame interval | 500 ms |
| Initial window size | 1280×800 default, a hint for floating window managers; tiling managers ignore it |
| Show below-threshold bins | off |
| Radar overlay opacity | 0.85 |
| Palette | `nws` |
| Mosaic mode | off |
| Mosaic site concurrency | 6 |
| Distance unit | OS locale: miles for the United States, kilometers elsewhere |
| Velocity unit | OS locale: knots for the United States, meters per second elsewhere |
| Time zone display | local |
| Clock | OS locale |
| UI language | OS locale |
| Home location | unset |
| Location-warning alert types | tornado warning, severe thunderstorm warning, flash flood warning, tornado watch, severe thunderstorm watch |
| Alert poll interval | 60 s |
| Lightning flash lifetime | 15 min |
| Nowcast | off |
| Nowcast lead times | 15, 30, 60 min |
| Nowcast cell threshold | 35 dBZ |
| Forecast units | follow the distance, velocity, and time preferences |
| Tile cache cap | 1 GiB |
| Volume cache cap | 2 GiB |

---

## Application Architecture

### Threading Model

Strict one-way data flow. Single-site mode is a pool of one.

```
per site (bounded pool, size = mosaic concurrency):
  fetch worker → channel → decode worker → channel ─┐
                                                    ├→ compositor worker → channel → UI thread
tile worker (tiles, glyphs) → channel ──────────────────────────────────→ UI thread
alert worker (alerts, zones) → channel ─────────────────────────────────→ UI thread
lightning worker → channel ─────────────────────────────────────────────→ UI thread
geocode worker → channel ───────────────────────────────────────────────→ UI thread
compositor worker → channel → nowcast worker → channel ─────────────────→ UI thread
forecast worker → channel ──────────────────────────────────────────────→ UI thread
```

Each site has its own fetch worker, decode worker, and pipeline state. The compositor worker is the only consumer of decoded volumes. It owns them, selects the sweep and product, resolves overlap between sites, and sends the UI thread an owned frame holding render geometry and a lookup structure for hover. With one site the compositor passes that site's sweep through unchanged. The nowcast worker receives reflectivity grids from the compositor, keeps a short history of them in memory, and sends the UI thread owned motion and extrapolation results. The UI thread owns all display state exclusively, and no shared mutable state crosses thread boundaries.

### Pipeline State Machine

Each site runs its own pipeline with explicit states the UI reflects directly:

- **Idle**: no site selected or fetch not yet started
- **Fetching**: chunk list and download in progress, with chunk count visible
- **Assembling**: chunks downloaded, awaiting volume completion sentinels
- **Decoding**: volume being decoded
- **Ready**: volume decoded, sweeps available for display
- **Refreshing**: the displayed volume is from the previous fetch cycle and a new fetch is in progress
- **Error**: fetch or decode failed, specific error message visible

Transitions between states are explicit, and the UI never infers state from the presence or absence of data. In mosaic mode the status bar shows the count of sites in each state, and each site's state is available on hover.

Pipeline state describes the application's work. Site health describes whether the radar is producing data (see Site Health). The two use separate names.

### Worker Resilience

A worker thread that panics is caught at the thread boundary, reported to the UI as an error state with a descriptive message, and does not crash the application. Worker panics are expected operating conditions, not unrecoverable failures. A panic in one site's workers affects only that site.

### Retry Policy

Transient failures retry with exponential backoff: a 2 s base, doubling to a 60 s cap, with jitter. A selected site retries indefinitely. A decode failure retries once, then stays in Error until the next volume or a manual retry, because the same bytes produce the same failure. HTTP 429 and 503 honor `Retry-After`. A 404 for a volume that has left the feed is a stale listing: the worker re-lists and fetches the current volume. The error message stays visible with the next retry time and is cleared only by a success.

### Interaction Budgets

Latency is part of correctness. A readout that lags the pointer describes a place the pointer has left. A frozen map presents an old frame as the current situation.

Reference machine: four-core x86-64 at 2.5 GHz, integrated GPU, 1920×1080, release build.

| Interaction | Budget |
|---|---|
| Pointer readout update | within one frame of the pointer event, at every zoom level |
| Static radar frame | 60 fps at the reference resolution with the radar layer, grid, and rings drawn |
| Pan and zoom | 60 fps with radar, labels, boundaries, roads, alerts, and lightning drawn |
| Product, tilt, or frame change | 100 ms when the data is in memory, with no rebuild of per-volume geometry |
| First radar frame from a cached volume | 500 ms after the UI receives the volume |
| Window resize | 100 ms |

Render geometry for a volume is built once, off the UI thread, and reused for every frame it is displayed. A frame draws cached GPU buffers. It does not rebuild the mesh, re-project bins, or allocate per-bin data.

Each budget is measured in the milestone that introduces the interaction, with the layers that exist at that milestone, and again in Milestone 24 with the full stack. A milestone that misses a budget is not complete.

---

## Feature Specification

### Site Selection

A searchable list of every site in every enabled source's catalog, searchable by id, name, and place name. On first run, with no location and no saved site, the application shows the picker over a map of all site locations, and the pipeline stays Idle with the visible reason "No site selected". A site is chosen only when location is granted or a site is saved. Selecting a site cancels any in-progress fetch, clears the current radar display, sets the pipeline to Fetching, and begins loading the latest volume. Data from the previous site carries a visible "Previous site" label until the new volume is ready, and the display then updates atomically. The last active site is persisted to `config.toml` and restored on startup.

### Place Search and Nearest Site

A place search box and a use-my-location control sit alongside the site picker. The query goes to the geocoder when the user presses Enter. Requests run one at a time at no more than one per second, and results are cached in memory for the session. A site id, a site name, or a `lat, lon` pair typed into the box resolves locally without a request.

A resolved place selects the nearest operational site across all enabled catalogs by geodesic distance from the catalog coordinates, and the result list shows the distance and lets the user pick another. The location control uses the operating system's location service where available and a placed map pin otherwise. Location is read when the user invokes the control, or on first run when the user grants it. On first run with location granted, the nearest site is selected and shown as selected. Switching follows Site Selection.

### Data Level Toggle

A two-position toggle between Level II and Level III, shown for sources that publish both. Switching levels cancels any in-progress fetch, reloads data for the current site at the new level, and updates the product selector to show only products available at the new level. Products unavailable at the active level are hidden or visibly disabled.

### Radar Products

Product availability depends on source, data level, and the specific volume scan.

**Dual-polarization suite:** base reflectivity (Z), base velocity (V), spectrum width (SW), differential reflectivity (ZDR), correlation coefficient (CC), specific differential phase (KDP).

**Level III products** are enumerated from the actual Level III product catalog at runtime, not hardcoded.

Each product has a color scale legend that updates when the active product changes. The legend shows the value range, units, and labeled tick marks at meteorologically significant thresholds. The default `nws` palette follows NWS operational conventions sourced from the WSR-88D product palette files.

A product unavailable in the current volume, at the current data level, or from the current source is disabled and visually distinct, and produces no error in the disabled state.

### Map Base

Vector tiles render client-side from a bundled MapLibre-style stylesheet written against the OpenMapTiles schema, with mouse-wheel zoom and click-drag pan. The tile host is OpenFreeMap (planet coverage, no account or key), listed as row 3 of the Network Policy table, and glyph ranges come from the same host. A muted style keeps the radar overlay legible and a dark style serves night use, both selectable in the application. Fractional zoom renders sharply at every zoom level, and the radar overlay is reprojected correctly at every zoom level without distortion. Tiles are disk-cached under `~/.oxrad/tiles/`, or read from a PMTiles archive when one is configured. On tile load failure the application falls back to a blank background with a coordinate grid. The map center and zoom level are persisted to `config.toml`.

### Map Labels

Place, road, and boundary labels render from glyph ranges in the style's font stacks with collision-based placement. Label text follows the UI language where the tiles carry a name in that language and falls back to the local name. Labels draw above the radar layer.

### Attribution

An attribution panel, reachable from the main window and shown in a corner of the map, lists the attribution for the map data (OpenStreetMap contributors), the tile host and schema, and every active source and data feed, taken from the catalog entries.

### Overlays

County/parish boundaries and major highways draw above the radar and toggle individually. Everywhere they come from the boundary and transportation layers of the vector tiles. When a United States source is active, the Census TIGER/Line shapefiles under `~/.oxrad/shapefiles/` provide county/parish boundaries and highways at higher fidelity. Both overlays are enabled by default, and their visibility is persisted to `config.toml`. If a data source for an overlay is unavailable, that overlay's toggle is disabled with a visible reason and the application continues normally.

### Hover Cursor and Measurement

The cursor reports latitude, longitude, ground range and azimuth from the active site, and the product value of the gate under it with units. If the cursor is not over a gate, the product value shows "no data." If no volume is loaded, only lat/lon is shown. In mosaic mode the cursor also shows which site supplied the value and the age of that site's scan. The selected sweep's elevation and calculated beam height above the antenna are available with the readout, labeled as an estimate rather than surface conditions.

A measurement tool places two map points and shows their geodesic distance and initial bearing in the active units. The points remain visible until cleared; the tool works without a radar volume and has keyboard access.

### Location and Follow Mode

When location permission is granted, the map marks the user's reported position and its accuracy when the operating system supplies it. Follow mode keeps the position in view as it changes. Panning the map suspends follow mode visibly; the user can resume it. An unavailable or stale location has a visible reason and is not displayed as a current fix. Location updates stay on the device except when the user explicitly requests a forecast or search for that position.

### Sweep Elevation Selector

Shows all available elevation angles in the current volume. The user steps through sweeps or selects by elevation number. The lowest elevation is the default. Each sweep entry shows its elevation angle in degrees and its scan time offset.

### Comparison Views

The radar workspace can show multiple synchronized panes so a user can compare available products or elevations for the selected site. Each pane identifies its site, product, elevation, scan time, and legend. Linked panes share map movement and time selection; a missing product or scan is labeled rather than replaced by a different one. Comparison controls work without a subscription.

### Animation

Loops a user-selectable history of complete volume scans for the active site and product, subject to the available feed and cache. Controls: Play/Pause, step forward, step backward, history length, and frame interval. A historical time selector opens available archived scans, labels archive time separately from live data, and provides an explicit return-to-live control. Show the available time span and distinguish missing scans from clear weather. The scan timestamp for the currently displayed frame is shown persistently. Controls are disabled with a visible reason when fewer than 2 complete scans are available. On startup, the volume cache under `~/.oxrad/volumes/` pre-populates the animation buffer while a fresh fetch runs in the background. The frame interval is persisted to `config.toml`. In mosaic mode animation is disabled with the visible reason "Mosaic holds the latest volume per site".

### Color Scale Legend

A persistent legend panel showing the color ramp, value range, and units for the active product and palette. It updates immediately on product, palette, or unit change. Meteorologically significant thresholds are physical values with fixed positions on the ramp: 35 dBZ and 50 dBZ for reflectivity and ±30 m/s for velocity. Their labels show the converted value in the active unit to one decimal place (±30 m/s reads ±58.3 kt or ±67.1 mph). Additional ticks sit at round values in the active unit.

### Units and Time

Display preferences live in `config.toml`: kilometers or miles for ground range, beam height, and range-ring labels; meters per second, knots, or miles per hour for velocity; local time or UTC; 12-hour or 24-hour clock. Internally all values are SI and all times are UTC. Preferences apply wherever a value is displayed: the hover cursor, legend ticks, scan timestamps, sweep time offsets, range rings, alert effective and expiry times, site-health age, and lightning flash age. Every displayed time carries its zone label.

### Colorblind-Safe Palettes

Palette selection offers color-vision-deficiency-safe ramps alongside the NWS-convention scales. The legend renders the active palette, and the selection persists in `config.toml`.

Verification is a test in the repository. Each palette is simulated under deuteranopia, protanopia, and tritanopia at full severity with the Machado, Oliveira & Fernandes (2009) matrices and measured with CIEDE2000. Adjacent color bins differ by ΔE00 of at least 6 under all three simulations. Sequential products (Z, SW, ZDR, KDP, CC) have monotonic lightness along the ramp. The velocity ramp uses a cool/warm pair so that inbound and outbound read at a glance: the strongest inbound and strongest outbound colors differ by ΔE00 of at least 40 under all three simulations, and the zero-velocity bin is a light neutral distinct from both.

### NWS Weather Alerts and Regional Alert Sources

Active weather alerts from the NWS public API (`api.weather.gov`) are displayed as geographic overlays on the map. Alert polygons are drawn with color coding by alert type (tornado warning, severe thunderstorm warning, tornado watch, severe thunderstorm watch, special weather statement, etc.) following NWS standard alert colors. Alerts are polled on a configurable interval (default 60 seconds) while the application is running. Alert details (headline, effective time, expiration time, issuing office, and full description) are accessible by clicking an alert polygon. Expired alerts are removed from the display immediately. If the NWS API is unreachable, the overlay reports the error, preserves any last good unexpired alerts with their age and stale state visible, and retries with exponential backoff while the rest of the application operates normally. Without a last good alert set, the overlay is unavailable with a visible reason. Alerts are not cached to disk; expired alerts are removed immediately.

Many watches and some warnings arrive with null geometry and a list of affected zones. For these the alert worker fetches each affected zone's geometry, from `~/.oxrad/zones/` when cached, and the alert's area is the union of those zones. An alert whose zone geometry cannot be resolved appears in an alerts panel with the label "area unavailable" and counts in the alert count.

Alert sources for other regions are catalog entries that point at a Common Alerting Protocol (CAP) feed, mapped onto the same alert type, color, and detail fields. A region without an alert source shows the alert overlay disabled with the reason "no alert source for this region".

### Storm Reports and Tracks

Where an openly accessible source supplies them, show local storm reports and official storm-track products as separately toggleable overlays. Each report identifies the event, location, observed time, source, and available details. Each track identifies its source and issue and valid times. A report or track is not an official warning, and a derived nowcast is labeled separately. When a source is unavailable, unsupported in the region, or stale, show that state rather than implying that no reports exist. User-submitted reports are optional, explicit actions and are sent only to a supported openly accessible destination with confirmation of the data being sent.

### Location Warnings

A home location stored in `config.toml`, set from place search, the location control, or a placed map pin. On every alert poll the application tests the home location against the area of every active alert, using polygons and resolved zone geometry, on this machine. When the home location falls inside an alert of a type the user enabled, the application raises a system notification with the headline and effective time and marks the home location on the map.

An alert and its updates form one chain through the alert's references. The user is notified once per chain, and again when the chain's event type changes or its severity increases (watch to warning, severe thunderstorm to tornado). A routine continuation raises no new notification. Alerts already active over the home location at startup each notify once. If the operating system blocks notifications, a persistent in-app banner in the alert type's color appears and the status bar shows "Notifications blocked".

Warnings work while oxrad is running. The status bar shows "Warnings: on" with the poll age whenever location warnings are active, and the settings panel states this limit where the feature is enabled.

### Mosaic

Mosaic mode composites the active product across every site whose coverage intersects the viewport. Candidates are the catalog sites within the catalog's candidate radius of the viewport, up to the concurrency limit, and sites beyond the limit are listed in the status bar as not fetched. Once a site's volume decodes, its footprint comes from the data. Each site contributes its lowest-elevation sweep that carries the active product, selected by elevation number as in single-site mode.

Every map position takes its value from the single site whose beam center is lowest there, with nearer range breaking ties. This rule holds for every product and does not blend values. Doppler velocity is radial to each radar, so the mosaic offers velocity as a single-source view with site seams drawn and the label "radial velocities are site-relative".

Sites scan at different times. Hover reports the supplying site and the age of its scan, the status bar shows the oldest and newest scan time in view, and a site whose health is Silent draws dimmed with a hatch. The compositor holds the latest volume per site. Selecting a site from the picker leaves mosaic mode for that site.

### Site Health

Site health is its own status bar state, separate from the pipeline state and from fetch errors. It is derived from the feed.

Cadence is the median of the last five intervals between volume start times observed at that site, from the feed listing and the cache, and it needs at least three intervals. Each source's catalog entry declares how its listing maps to volumes and start times: for NEXRAD, the chunk listing; for ODIM_H5 services that publish one file per moment, the timestamp in the file name of the reflectivity moment's files, one per volume.

A site is Live when the age of the newest volume in the feed listing is at most twice the cadence, and Silent when the age exceeds twice the cadence, with the age shown. A silent site keeps retrying on the normal fetch schedule. With fewer than three intervals observed, health reads "Cadence unknown" with the age shown. When the listing itself cannot be fetched, the pipeline shows an Error and health shows "Unknown", so a network failure does not read as a dead radar. The scan mode (clear-air or precipitation) read from the volume header is shown as information next to the health state and does not set the thresholds.

### Lightning (GLM)

Total-lightning flashes from the GOES Geostationary Lightning Mapper, fetched as Level 2 flash data from the public GOES bucket, render as a toggleable overlay. Lightning satellites and their buckets are catalog entries of kind `lightning`, in the same catalog as radar sources. The coverage footprint of each satellite comes from the geolocation metadata in its own files. Where two satellites cover the same position, flashes come from the one nearer its subpoint, which prevents double-counting.

Flash markers fade with age and expire after the configured lifetime. The overlay shows flash age and the time of the newest file used. GLM files arrive minutes behind real time, and the age display makes that delay visible. The overlay is always in exactly one of five states: Live (newest file within its expected cadence), Delayed (older, with the file time), No data (feed unreachable, shown as an error), Outside coverage (viewport outside every covered footprint), and No flashes in view (covered, current, none present). Outside the Americas the overlay reports Outside coverage. The toggle persists in `config.toml`.

### Nowcasting

Nowcasting carries the radar forward in time from the radar data alone. It extrapolates what is on the map and does not model storm growth, decay, initiation, or dissipation, and the overlay label says so permanently.

The input is the compositor's reflectivity grids: the single site's lowest sweep, or the mosaic composite. The nowcast worker keeps the last four grids in memory, independent of the volume cache, so it works in mosaic mode. A motion field is estimated on a regular grid in map space by block-matching successive grids with cross-correlation (Rinehart & Garvey 1978, TREC). Blocks without enough echo to correlate stay empty. Cells are contiguous regions at or above the cell threshold (35 dBZ by default, matching the legend threshold) and are tracked from frame to frame by centroid and area (Dixon & Wiener 1993, TITAN). Each tracked cell carries its own velocity vector and track history.

The latest reflectivity grid is advected along the motion field to each configured lead time (15, 30, and 60 minutes by default) and drawn as a ghost layer with a lead-time selector, with cell tracks drawn as vectors. For a set home location the nowcast reports an estimated arrival time range for each tracked cell whose path crosses it, taken from the spread of that cell's velocity vectors across the last frame pairs. An "approaching storm" alert type is available for location warnings and defaults to off. The display carries the age of the newest input grid, the number of grids used, the lead time, and the "extrapolation only" label.

Nowcasting is disabled with a visible reason when fewer than three grids are available, when the source site's health is Silent, or when the time gaps between grids differ from one another by more than the cadence the health state reports. Verification is two tests with expected values derived independently of the implementation: a synthetic reflectivity field translated by a known vector must be recovered by the block-matching within the stated tolerance, and a synthetic cell moving at a known velocity must produce the known arrival time. The toggle, lead times, and cell threshold persist in `config.toml`.

### Forecasting

The weather view and forecast panel serve a location the user chooses by search, home, current position, a placed pin, or a clicked map point. Show a plain-language condition and temperature together, such as "Sunny, 70 °F," when supplied. Identify observed current conditions by observation time and source; a forecast for the present hour is labeled as a forecast, never as an observation. Show hourly and daily forecasts with condition, temperature, precipitation probability and amount, wind speed and direction, and cloud cover when supplied. Daily rows identify the day, high and low where supplied, and the period to which each probability applies; for example, "Tomorrow: 70% chance of rain" only when the provider supplies that probability for that period. Show the forecast issue time, applicable time zone, and provider. Use the user's temperature, distance, wind, precipitation, and time preferences consistently. Missing values are labeled unavailable and never inferred as zero, clear sky, or a provider-supported probability.

Forecast services are catalog entries. Inside the United States the service is `api.weather.gov`, and everywhere else it is MET Norway's `api.met.no` locationforecast, which covers the globe and needs no key. Each carries its attribution into the attribution panel. A request sends the forecast point's coordinates to the service, and the Network Policy table lists this. Responses are held in memory and honor the service's `Expires` and `Last-Modified` headers, and nothing is cached to disk.

Requests time out after 10 s. A transient failure retries with exponential backoff up to 60 s while the panel is visible, and the panel shows the retry state. HTTP 429 and 503 honor `Retry-After`. A 4xx response other than 429 is not retried and appears as unavailable with its status. One request per point is in flight; moving the panel to a new point cancels the old request.

The weather view presents current conditions first, then hourly rows for at least the next 24 hours and daily rows for at least the next 7 days when the source supplies them. The forecast panel presents the same data for its map point. If a source supplies a shorter horizon or no observation, show the available horizon or observation as unavailable; do not fabricate a forecast, probability, daily high, or low. Identify the selected location in both views and mark a map point on the map. The issue time and service name stay visible in every ready state. Available forecast map layers show the quantity, units, valid time, issue time, and source; an unavailable layer is disabled with a reason, and an observation, forecast, or nowcast is never labeled as another.

The weather view and panel distinguish loading, ready (with issue time), stale (with age), unavailable (with the specific error), and "no forecast service for this location". The last good forecast remains visible and labeled stale on a refresh failure. Observation availability and age are independent of forecast availability and age. With nowcasting on, the panel shows the nowcast arrival range next to the model forecast in a separately labeled block.

### Localization

All user-facing strings live in translation catalogs. Numbers, dates, and times format by locale. The initial catalog is English, and adding a language is adding a catalog file. The UI language is a `config.toml` field that defaults to the OS locale.

### Status and Diagnostics

A persistent status bar showing: current site and data level, volume scan timestamp, pipeline state, site health, skipped radial and bin counts (non-zero counts highlighted in yellow), tile load failures, active alert count, location-warning state, lightning state when the overlay is on, the catalog-versus-header coordinate distance, and any active error condition. Non-zero skipped counts indicate data the user is not seeing and are always visible.

---

## Network Policy

The application makes outbound connections to exactly these destinations. Every request carries a User-Agent of the form `oxrad/<version> (<project URL>)` and nothing that identifies the user.

| # | Destination | Purpose | When | Sent beyond the request |
|---|---|---|---|---|
| 1 | AWS NEXRAD S3 bucket (HTTPS) | Level II data | United States source active | nothing |
| 2 | NOAA public HTTPS distribution | Level III data | United States source, Level III | nothing |
| 3 | OpenFreeMap tile host (HTTPS) | vector tiles and glyph ranges | map visible, no PMTiles archive | tile coordinates |
| 4 | Census Bureau public HTTPS endpoint | TIGER/Line shapefiles | first time a United States source is active | nothing |
| 5 | `api.weather.gov` (HTTPS) | alerts, zone geometry, United States forecasts | alert overlay on, or a forecast requested, United States | the coordinates of the forecast point, for forecasts |
| 6 | GOES public bucket (HTTPS) | GLM Level 2 flash data | lightning overlay on | nothing |
| 7 | OpenStreetMap Nominatim (HTTPS) | place search | when the user submits a search | the typed search text |
| 8 | DWD open data (HTTPS) | ODIM_H5 volumes | German source active | nothing |
| 9 | MET Norway `api.met.no` (HTTPS) | forecasts outside the United States | a forecast requested | the coordinates of the forecast point |

Further sources add rows to this table in the same change that adds their catalog entries. No other outbound connections exist. Zero telemetry. Zero analytics. No third-party SDKs that initiate their own network activity.

---

## Build and Portability

Single `cargo build` on Windows, macOS, and Linux. No platform-specific scripts. No build steps outside Cargo. Required system libraries for Linux are documented in `README.md`.

HDF5 files (ODIM_H5 radar volumes and GLM NetCDF-4 files) are read with `hdf5-pure`, which has no C dependency and no build script. The reader is accepted when it reproduces `h5dump` output exactly on a real GLM Level 2 file and a real ODIM_H5 volume, both kept as test fixtures with their provenance.

Release binaries for Windows, macOS, and Linux are produced by `cargo build --release` in continuous integration.

## Specification Boundaries

This document owns product scope, externally observable behavior, system architecture, data contracts, runtime guarantees, source integration, operational failure behavior, and development milestones.

[STYLE.md](STYLE.md) owns code organization and implementation style: Rust idioms, ownership expression, naming, comments, tests, lint configuration, and prose style. The style guide does not redefine product behavior or grant exceptions to architectural requirements. This document does not duplicate stylistic rules. A change to a style convention belongs in the style guide; a change to a product guarantee or system behavior belongs here.

The two documents are complementary. Their scopes are distinct, not hierarchical. Neither document grants permission to weaken a requirement owned by the other. If an implementation detail appears to conflict with a product or safety requirement, the implementation must satisfy the product or safety requirement. If the documents appear to conflict, identify the exact statements and resolve their meaning from the intended behavior; do not select the easier interpretation, silently omit functionality, or invent an exception.

## Product Coverage and Users

oxrad is a full-featured desktop weather and radar application, not a United States-only NEXRAD viewer. It serves everyday users who want a clear desktop weather experience, weather enthusiasts, storm spotters, local meteorologists, regional and nationwide meteorological operations, emergency managers, and first responders involved in response and rescue.

The product includes the capabilities users reasonably expect from a desktop weather widget, a general-purpose weather application such as MyRadar, and a professional radar workstation such as RadarScope. These are capability references, not dependencies or permission to copy proprietary implementation or presentation.

The finished application includes:
- Current conditions, hourly forecasts, daily forecasts, severe-weather information, and forecast maps.
- Interactive radar maps, full-resolution source products, elevation selection, animation, historical scans where public data permits, comparison views, measurements, overlays, and multi-site mosaics.
- Radar-derived nowcasting, storm-cell identification and tracking, motion estimation, extrapolated precipitation fields, and estimated arrival information.
- Alerts, watches, warning polygons, storm reports, lightning where a public feed exists, location-based warnings, and diagnostic status.
- A usable everyday interface and a detailed operational interface. Complexity is progressively exposed; ordinary users are not forced to interpret professional diagnostics, and professional users are not deprived of the underlying data.

The application is usable worldwide. Radar, forecast, alert, and lightning coverage varies by public data availability, licensing, service policy, and implemented decoder/provider support. Regional coverage gaps must be represented explicitly. A missing provider or unsupported format is a coverage limitation, not evidence of clear weather and not a reason to narrow the product to the United States. Source adapters and catalogs are extensible across countries and regions. NEXRAD and ODIM_H5/DWD are initial radar integrations, not the final geographic boundary. Forecast integration begins with the United States and MET Norway providers and expands through provider adapters to public forecast services covering additional regions.

The product must run responsively on ordinary consumer desktop and laptop hardware, including older machines. Heavy numerical weather prediction, climate modelling, data assimilation, and simulation workloads requiring workstation-class hardware or large local compute resources are outside scope. Forecast presentation consumes provider forecasts; nowcasting performs bounded, efficient analysis of recent radar observations. Performance must degrade through explicit reduced capabilities, not an unresponsive interface or silent loss of correctness.

## Runtime Error and Recovery Contract

Failure handling is a system behavior, not merely an implementation convention.

Every failure is represented, classified, surfaced, and accounted for. No network failure, decode failure, malformed record, missing field, cache failure, worker panic, provider outage, or data-quality anomaly may disappear silently. Logs supplement visible status; they never replace it.

A failure must not terminate the whole application merely because one operation, source, site, layer, or worker failed. The application isolates independent work and continues unaffected functionality. A failed radar site must not stop other sites, forecasts, alerts, map tiles, or the UI. A failed optional overlay must not disable the radar display. A failed forecast provider must not stop radar or cached forecasts. A worker panic is contained at its worker boundary, reported, and isolated to the affected work unit.

An incorrect decoder, calculation, or data association is a correctness defect: reject affected results, diagnose and fix the cause, and verify against the source format and independent references. Error reporting and retry do not make incorrect results trustworthy. Format-defined sentinel categories remain distinct valid data. Recovery is attempted when it is safe and meaningful:
- Transient network and service failures retry under the retry policy, honoring `Retry-After`, timeouts, cancellation, and bounded backoff.
- A decode failure for identical immutable bytes is not retried indefinitely. The failed volume is quarantined or skipped, the error is surfaced, and the next available volume is attempted. A manual retry remains available where it can change the outcome.
- Corrupt cache entries are identified, removed or quarantined, reported, and refetched when possible. Cache corruption must not prevent startup.
- Malformed records are rejected individually when record boundaries and format semantics make isolation safe. Valid records and other independent products continue. If corruption invalidates the enclosing volume or makes geometry/data association unreliable, reject that volume rather than render misleading data.
- Missing optional values disable only the dependent measurement or feature and state exactly what is unavailable. Required geometry is never fabricated.
- A failed refresh preserves the last known-good result with its age and stale state visible. It is never relabeled current.
- Worker restart is attempted only when worker state can be safely reconstructed. Repeated deterministic failures are surfaced and rate-limited rather than hidden in an endless restart loop.
- Cancellation and supersession are normal outcomes when the user changes site, product, level, or forecast point. Superseded work is cancelled and cannot overwrite newer state.

Errors have distinct states for loading, ready, stale, unavailable, failed, and not provided where applicable. The UI identifies the operation, source/site/location, affected feature, cause, and next retry or recovery action. Error messages are actionable and do not expose secrets or sensitive location/search content. Diagnostic counters include rejected records, skipped radials/bins, missing products, stale inputs, retries, provider failures, and worker restarts.

The UI thread remains responsive during failures and recovery. A fatal process exit is reserved for conditions in which continued execution cannot preserve memory safety or trustworthy application state. It is not a generic error-handling strategy. Since unsafe Rust is prohibited, memory-safety faults are not handled by introducing unsafe recovery mechanisms.

No required feature may be removed, hidden, stubbed, or marked complete as a workaround for a build, lint, test, performance, or integration failure. A failing quality gate is a defect to fix while preserving the feature. Feature scope changes require an explicit product-scope decision; implementation difficulty is not such a decision.

## Delivery and Completion Contract

A milestone is complete only when all of its specified behavior is implemented, reachable through the application, exercised in the running binary, and verified by its acceptance tests. Compilation, Clippy, and unit tests are necessary gates, not substitutes for feature completion.

A required feature is never an acceptable stub. There are no placeholder implementations, empty handlers, fake data paths, TODO-based feature stand-ins, dead UI controls, or documentation-only substitutions for required behavior. If a feature is scheduled for a later milestone, it is not represented as an available feature in the current application. If its UI is present because the milestone requires it, the control must perform the specified behavior or expose a truthful unavailable state caused by a real dependency/coverage limitation, not an unfinished implementation.

A build or lint failure never authorizes deleting the feature that exposed it. Fix the implementation, its types, tests, and integration while preserving the specified capability. A milestone cannot pass by reducing the acceptance criteria, removing a failing test, disabling a lint, hiding a control, replacing real data with fabricated data, or changing a required feature into a placeholder.

Each milestone records the implemented behavior, tests run, runtime observations, and measured performance. Unimplemented acceptance criteria remain open work and block milestone completion.

---

## Development Milestones

Each milestone must pass all gates before the next begins: `cargo clippy -- -D warnings` at zero warnings and `cargo test` fully passing. Each milestone's gate also covers the failure modes it introduces: the network failing, malformed input, and a panic in each worker it adds. No milestone is complete with failing tests or outstanding warnings.

Passing the gates is necessary, not sufficient. The milestone's acceptance criteria are observed in the running binary, and its interaction budgets are measured. A display defect the tests cannot see is a failed milestone.

### Milestone 1: Core render loop

The application starts with `cargo run` and no arguments. It restores the last selected site from `~/.oxrad/config.toml`; with no saved site it shows the site picker with the pipeline Idle and the reason "No site selected", and acquisition begins only for a site the user selects. The site list is the bundled National Weather Service catalog, and selecting a site cancels the previous site's work before the new fetch starts.

Window opens on the latest complete live KPAH Level II volume. Discovery, fetching, completeness validation, Archive II decompression, decoding, and refresh are implemented in this milestone. Two real-data tests cover the source: a pinned, provenance-recorded KPAH archive is retrieved from the AWS feed at test time after its size and SHA-256 are verified, and the current live listing is paginated and decoded. Neither is presented as live radar. Base reflectivity draws on a plain background with a lat/lon grid and 50 km range rings. For reflectivity, only gates that carry echo are drawn: `BelowThreshold` gates are excluded from the default view and counted in diagnostics, and `RangeFolded` gates use the distinct sentinel color. The color legend shows the reflectivity scale and dBZ. The scan time is visible. A pointer readout shows the gate value with units, ground range, and azimuth, and tracks the pointer within the interaction budget. Render geometry is built once per volume and reused across frames.

Gate geometry and elevation are read from radial headers and site coordinates from the volume header. Each gate's slant-range edges are converted to ground range before display, and the geographic placement follows the WGS84 geodesic. Coordinate math is unit-tested against the independently verified reference values. Every fetch and decode error surfaces as a message naming the operation, the site, and the cause. A failed fetch leaves the last good volume visible with its age and retries on the schedule in Retry Policy.

The milestone is complete when `cargo clippy -- -D warnings` and `cargo test` pass, the binary has been run, every behavior above has been observed, and the pointer-readout and frame-time measurements are recorded. Tests and lints alone do not satisfy this milestone.

### Milestone 2: Vector tile pipeline and base map

Tile fetch from the tile host, MVT decode, and client-side rendering of fills and lines from the bundled stylesheet, in muted and dark styles. The tile and style crate is selected here against the criteria in Dependencies. Mouse-wheel zoom and click-drag pan, sharp at fractional zoom levels. Radar overlay reprojected correctly at all zoom levels without distortion. Disk cache under `~/.oxrad/tiles/` with expiry, size cap, and startup purge, with a PMTiles archive as an offline source. Fetch concurrency and rate limits follow the host's terms, recorded in `docs/NETWORK.md`, which this milestone creates. Graceful fallback to coordinate grid on tile failure. Attribution panel. Tile forward/inverse transforms unit-tested against the reference values.

### Milestone 3: Local storage and configuration

Establish the `~/.oxrad` directory structure. Implement the `paths` module as the single source of all file paths. User configuration in `~/.oxrad/config.toml` with every field and default in the configuration table, including fields for features that land later. Persist and restore last site, product, map center and zoom, overlay visibility, animation frame interval, and window size as an initial-size hint. Window position is never persisted or set; placement is the window manager's job. Error log at `~/.oxrad/errors.log` with rotation. Cache size caps and eviction. `docs/DEPENDENCIES.md` created. Configuration load failures are visible; existing malformed or unrecognized configuration is not overwritten without the user's explicit action.

### Milestone 4: Radar source catalog and site selection

Catalog format and loader, and the NEXRAD catalog. Searchable site picker. First-run picker state with no site selected. Site switch cancels in-progress fetch, clears display, begins new fetch, and labels previous-site data. Last active site persisted. Catalog-versus-header coordinate distance computed and surfaced, with rendering driven by the header.

### Milestone 5: Gate geometry and full data fidelity

Extend the Milestone 1 geodesic placement and slant-to-ground conversion to every available sweep and product, with the reference tables as the acceptance tests. All gate geometry (first gate center, gate interval, azimuth spacing) is read from radial headers with no fallback constants. Extend the NEXRAD decoder module over `nexrad-decode` and the Archive II decompression selected in Milestone 1. Apply the complete volume assembly contract, including pagination to exhaustion and start/end sentinel verification before decode, to all supported Level II retrieval paths. Sweep selector by elevation number, not list index. Skipped radial and bin counts verified in UI.

### Milestone 6: Products, legends, units, and palettes

Full dual-pol product suite: Z, V, SW, ZDR, CC, KDP. Product selector reflects availability for the active sweep, level, and source, and disabled products are visually distinct and non-interactive. Unit and time types (distance, velocity, time zone, clock) and the palette type exist from this milestone, so every later display consumes them. The `nws` palette sourced from WSR-88D palette files. Legend with physical-threshold ticks labeled in the active unit. Unit and palette preferences live in settings and persist.

### Milestone 7: Data level toggle (Level III)

Level II / Level III toggle. Level III fetch pipeline. Product catalog enumerated from Level III data at runtime. Product selector reflects active level. Switching levels cancels in-progress fetch and reloads. Synchronized comparison panes identify the product, elevation, scan time, and unavailable data for each pane.

### Milestone 8: NEXRAD volume cache and animation

Volume cache under `~/.oxrad/volumes/` with 48-hour expiry, size cap, and startup purge. Animation buffer pre-populated from cache on startup while a fresh fetch runs in the background. Animation controls: play/pause, step forward, step backward, selectable available history, and configurable frame interval. Historical scan selection and explicit return to live. Scan timestamp display. Controls disabled with a visible reason when fewer than 2 scans are available. Frame interval persisted to config.

### Milestone 9: Boundary and road overlays

Boundary and road layers from the vector tiles. Census TIGER/Line overlays for United States sources, downloaded on first use of such a source with progress display and integrity verification, cached under `~/.oxrad/shapefiles/`. Per-overlay toggles. Overlay visibility persisted. When a data source is unavailable, the affected overlay reports its unavailable state and reason; all unrelated layers and application functions continue.

### Milestone 10: Hover cursor and diagnostics

Full hover display: lat, lon, ground range, azimuth, and product value with units, all in the active display units. "No data" when the cursor is off-bin. Diagnostic status bar showing all skipped counts, tile failures, pipeline state, the coordinate-distance diagnostic, and alert count. Scan timestamp persistent in UI.

### Milestone 11: Map labels

Glyph range fetch and cache, label layout with collision placement, language-aware label names with local-name fallback. Labels above the radar layer.

### Milestone 12: NWS weather alerts

Alert overlay from `api.weather.gov`. Alert polygons color-coded by alert type following NWS standard colors. Zone-geometry resolution and cache for alerts without polygons, with the "area unavailable" state. Configurable poll interval with exponential backoff on failure. Alert detail view on click: headline, times in the active time preference, issuing office, full description. Expired alerts removed immediately. Alert API failure marks alerts unavailable or stale with a visible message, preserves the last good alert set with its age, retries under the retry policy, and leaves other functionality unaffected. Alert data is not cached to disk. Separately toggleable local storm report and official track overlays show their source, observation or issue time, details, and unavailable or stale state. Optional report submission requires an explicit user action and confirmation.

### Milestone 13: Site health

Cadence from observed volume intervals. States Live, Silent, Cadence unknown, and Unknown on an unreachable feed, derived from the feed listing and distinct from pipeline state. Age shown. Scan mode shown as information. Silent sites keep the normal retry schedule.

### Milestone 14: Place search and nearest site

Geocoder worker with submit-only search, one request in flight, one request per second, session result cache, and identifying User-Agent. Local resolution of site ids, names, and `lat, lon`. Operating-system location control with map-pin fallback, current-position accuracy and age, and follow mode with visible suspension on pan. A two-point geodesic distance and bearing tool works independently of radar availability. Nearest-site selection with distance shown. First-run behavior from Site Selection. Query text excluded from the error log.

### Milestone 15: Location warnings

Home location in `config.toml`. Local point-in-area check against polygons and resolved zone geometry on every poll. System notification with alert-type preferences, chain-based deduplication, upgrade re-notification, and startup behavior. Blocked-notification banner. "Warnings: on" status and the running-only statement. Home marker on the map.

### Milestone 16: Colorblind-safe palettes

Color-vision-deficiency-safe ramps added to the palette type from Milestone 6. Machado simulation and CIEDE2000 criteria implemented as tests, including the velocity polarity criterion. Legend renders the active palette, and the selection persists.

### Milestone 17: Multi-site pipeline and mosaic

Per-site pipelines in a bounded pool, one compositor worker, and per-site state aggregation in the status bar. Mosaic compositing with the lowest-beam overlap rule, site seams, per-site age on hover, and dimmed hatched drawing of silent sites. Velocity labeled site-relative. Latest-volume-per-site retention. Animation disabled in mosaic mode with its reason. Panic isolation per site.

### Milestone 18: HDF5 integration

`hdf5-pure` behind one module with typed errors. Accepted when it reproduces `h5dump` output exactly on the GLM and ODIM_H5 fixtures. Malformed-input tests: truncated files, corrupt headers, oversized length fields.

### Milestone 19: Lightning overlay (GLM)

GLM Level 2 fetch from the public GOES bucket using the Milestone 18 module. Satellite selection and coverage footprints from file metadata, with nearer-subpoint resolution of overlap. Toggleable overlay with age-fading markers, expiry, and flash age. The five overlay states and the last-file time. Visibility persists.

### Milestone 20: ODIM_H5 radar source

ODIM_H5 decoder to `RadarVolume` with the quantity mapping and `undetect`/`nodata` handling. DWD catalog entry and network-policy row. Volume assembly from the per-moment files the service publishes. Attribution and license text from the catalog. Rendering, hover, legend, animation, health, and mosaic run on the unchanged code.

### Milestone 21: Nowcasting

Nowcast worker fed by the compositor with an in-memory history of four reflectivity grids. TREC-style block-matching motion field, TITAN-style cell identification and tracking, advection to the configured lead times, ghost overlay and cell tracks with lead-time selector. Arrival range for the home location. The "extrapolation only" label, input age, and the disabled states with reasons. Synthetic-field and known-velocity verification tests. Settings persist. Optional "approaching storm" alert type.

### Milestone 22: Forecasting

Weather view and forecast worker and panel for searched locations, home, current position, pins, and clicked points. United States and MET Norway are the initial provider catalog entries, with attribution and network-policy rows. The forecast-provider interface and catalog support additional public regional and national providers without changing the weather view or forecast model. Add providers to expand worldwide coverage; never treat the initial provider set as a geographic product limit. Show observed current conditions when available, or label the current-hour forecast as such; show plain-language condition, temperature, hourly forecasts for 24 hours, and daily forecasts for 7 days when supplied, with precipitation probability and amount, wind, cloud cover, units, source, valid periods, and issue or observation time. Label unavailable values and shorter horizons without inventing them. Weather and radar views share the selected location, and switching preserves it. Available forecast map layers identify quantity, units, source, issue time, and valid time; unavailable layers state why. Memory-only caching honors `Expires` and `Last-Modified`. Request timeout, retry with exponential backoff, `Retry-After` handling, and cancellation when the point changes. Loading, ready, stale, unavailable, and no-service states preserve and label the last good forecast on refresh failure. Nowcast arrival range appears beside the forecast in a separately labeled block. Forecast-point coordinates are excluded from the error log.

### Milestone 23: Localization

Translation catalogs for all strings. Locale-aware number, date, and time formatting. UI language field. A second language shipped to exercise the mechanism.

### Milestone 24: Resilience hardening and performance

Comprehensive review of all failure modes across the full stack: worker panic recovery for every worker, all pipeline states and site-health states reachable and correctly labeled, all error paths confirmed to reach the UI. Malformed-input tests on every decoder. Every interaction budget measured on the reference machine with the full overlay stack, a mosaic at the concurrency limit, animation, alert polling, and lightning active; the measurements are recorded with the method and hardware. A budget missed here sends the work back to the milestone that introduced the interaction. Cache integrity: corrupt entries in every cache are detected, deleted, and reported without preventing startup.

### Milestone 25: Release binaries

`cargo build --release` artifacts for Windows, macOS, and Linux from continuous integration, with the `README.md` system-library documentation verified on a clean machine for each.
