# oxrad: Architecture and Development Roadmap

## Purpose

oxrad is a storm-chaser-grade weather radar viewer. Its primary users are storm chasers, emergency managers, and weather spotters making real-time decisions during severe weather events. A display error, a frozen screen, or a corrupted bin placement is a safety failure, not a UI inconvenience. Every design decision in this document is made with that constraint as the first priority.

oxrad started because the viewers available are slow, crash-prone, and freeze the display on settings changes. oxrad is the fast, stable, responsive one. The bar for done is a viewer good enough for anyone to use anywhere in the world where a national weather service publishes open radar data. oxrad is open software under GPLv3, and stays that way: weather preparedness belongs to everyone.

---

## Scope

A desktop viewer for Windows, macOS, and Linux that renders weather radar from any source whose data is openly published and whose format oxrad decodes, and carries that radar forward into nowcasting and forecasting. A source is a catalog entry (see Radar Sources). NEXRAD (United States) is the first source. ODIM_H5 volumes, the format used by European national services, are the second, with Germany's DWD as the first target. Adding a country is adding a catalog entry and, if the format is new, a decoder.

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

The KPAH test origin below is a test input declared in the test fixtures. It does not appear in any rendering path. All values were computed independently of the implementation with GeographicLib (geodesics) and PROJ (Mercator), and cross-checked against closed-form expressions.

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

| Concern | Choice | Basis |
|---|---|---|
| NEXRAD Level II parsing | `nexrad-decode` | Parses decompressed Archive II records against NOAA ICD 2620002AA and performs no I/O. MIT. |
| NEXRAD fetch | oxrad's fetch worker | `nexrad-data` bundles Archive II decompression with AWS S3 access. Its network client would sit outside the fetch worker and outside the Network Policy table. |
| Archive II decompression | selected in Milestone 5 | Streaming, pure Rust, no C build step. |
| Image output | oxrad's GPU renderer | `nexrad-render` produces `image` buffers. oxrad draws bins from geometry. |
| HDF5 container for ODIM_H5 and GLM | `hdf5-pure` | Pure Rust with no build script. Reads superblocks v0 to v3, object headers v1 and v2, contiguous, compact, and chunked storage, compound types, and variable-length strings. Version 0.x, MIT. Accepted on fixtures in Milestone 18. |
| ODIM_H5 radar decoding | oxrad module over `hdf5-pure` | No Rust ODIM_H5 radar decoder turned up in a crate search. |
| Geodesics | `geographiclib-rs` | Direct and inverse geodesic solutions after Karney. The crate describes itself as a subset of GeographicLib, and direct and inverse are the subset in use. Accepted when it reproduces the Reference Values within tolerance. |
| Vector tile decode and style evaluation | selected in Milestone 2 | Pure Rust, MVT decode, MapLibre style expressions, fractional zoom. |
| HTTP, windowing and GPU, TOML, notifications | selected in the milestone that first needs each | Each runs under the Network Policy and the dependency record. |

---

## Data Model

### NEXRAD Level II

Level II is full-resolution dual-polarization volume scan data. It is the primary data source for storm chasing in the United States.

**Volume assembly.** A real-time volume on the AWS feed consists of approximately 55 chunks delivered over several seconds. A complete volume requires paginating the chunk list to exhaustion, sorting chunks by sequence number where the API permits, and verifying the presence of start and end sentinel chunks before decoding. Decoding a partial volume and presenting it as complete is not permitted.

**Sweep selection.** A volume contains multiple elevation sweeps. The application selects sweeps by elevation number, not by list index. The lowest available elevation angle is the default. `first()` is not an acceptable sweep selector.

**Gate geometry.** For each moment and each radial, the following values are read directly from the radial header:
- Range to first gate center (converted from meters to kilometers)
- Gate interval (converted from meters to kilometers)
- Azimuth center and spacing

No fallback constants are permitted. If a radial does not supply valid geometry, it is skipped and counted in a diagnostic counter visible in the UI.

**Gate value handling.** `BelowThreshold` gates are mapped to the minimum valid value for that moment (-32 dBZ for reflectivity). `RangeFolded` gates are mapped to a distinct NaN sentinel that renders as a visually distinct color. Both are kept in the bin array to preserve index-to-range alignment. A skipped gate shifts all subsequent gates inward, which is a safety defect.

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

User preferences persisted across sessions in TOML format. The configuration file is written only when settings change. If the file is absent, malformed, or contains unrecognized keys, the application starts with defaults and attempts to rewrite a clean config. The home location is stored as latitude and longitude in plain text. Defaults that depend on the operating system locale are derived once on first run and then written explicitly.

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

### Hover Cursor

Displays at all times: latitude, longitude, ground range from the active site, azimuth from the active site, and the product value of the nearest bin with units. If the cursor is not over a bin, the product value shows "no data." If no volume is loaded, only lat/lon is shown. In mosaic mode the cursor also shows which site supplied the value and the age of that site's scan.

### Sweep Elevation Selector

Shows all available elevation angles in the current volume. The user steps through sweeps or selects by elevation number. The lowest elevation is the default. Each sweep entry shows its elevation angle in degrees and its scan time offset.

### Animation

Loops the last 2 to 3 complete volume scans for the active site and product. Controls: Play/Pause, step forward, step backward, configurable frame interval. The scan timestamp for the currently displayed frame is shown persistently. Controls are disabled with a visible reason when fewer than 2 complete scans are available. On startup, the volume cache under `~/.oxrad/volumes/` pre-populates the animation buffer while a fresh fetch runs in the background. The frame interval is persisted to `config.toml`. In mosaic mode animation is disabled with the visible reason "Mosaic holds the latest volume per site".

### Color Scale Legend

A persistent legend panel showing the color ramp, value range, and units for the active product and palette. It updates immediately on product, palette, or unit change. Meteorologically significant thresholds are physical values with fixed positions on the ramp: 35 dBZ and 50 dBZ for reflectivity and ±30 m/s for velocity. Their labels show the converted value in the active unit to one decimal place (±30 m/s reads ±58.3 kt or ±67.1 mph). Additional ticks sit at round values in the active unit.

### Units and Time

Display preferences live in `config.toml`: kilometers or miles for ground range, beam height, and range-ring labels; meters per second, knots, or miles per hour for velocity; local time or UTC; 12-hour or 24-hour clock. Internally all values are SI and all times are UTC. Preferences apply wherever a value is displayed: the hover cursor, legend ticks, scan timestamps, sweep time offsets, range rings, alert effective and expiry times, site-health age, and lightning flash age. Every displayed time carries its zone label.

### Colorblind-Safe Palettes

Palette selection offers color-vision-deficiency-safe ramps alongside the NWS-convention scales. The legend renders the active palette, and the selection persists in `config.toml`.

Verification is a test in the repository. Each palette is simulated under deuteranopia, protanopia, and tritanopia at full severity with the Machado, Oliveira & Fernandes (2009) matrices and measured with CIEDE2000. Adjacent color bins differ by ΔE00 of at least 6 under all three simulations. Sequential products (Z, SW, ZDR, KDP, CC) have monotonic lightness along the ramp. The velocity ramp uses a cool/warm pair so that inbound and outbound read at a glance: the strongest inbound and strongest outbound colors differ by ΔE00 of at least 40 under all three simulations, and the zero-velocity bin is a light neutral distinct from both.

### NWS Weather Alerts and Regional Alert Sources

Active weather alerts from the NWS public API (`api.weather.gov`) are displayed as geographic overlays on the map. Alert polygons are drawn with color coding by alert type (tornado warning, severe thunderstorm warning, tornado watch, severe thunderstorm watch, special weather statement, etc.) following NWS standard alert colors. Alerts are polled on a configurable interval (default 60 seconds) while the application is running. Alert details (headline, effective time, expiration time, issuing office, and full description) are accessible by clicking an alert polygon. Expired alerts are removed from the display immediately. If the NWS API is unreachable, the overlay is disabled with a visible message and polling continues with exponential backoff while the rest of the application operates normally. Alert data is live or absent and is not cached to disk.

Many watches and some warnings arrive with null geometry and a list of affected zones. For these the alert worker fetches each affected zone's geometry, from `~/.oxrad/zones/` when cached, and the alert's area is the union of those zones. An alert whose zone geometry cannot be resolved appears in an alerts panel with the label "area unavailable" and counts in the alert count.

Alert sources for other regions are catalog entries that point at a Common Alerting Protocol (CAP) feed, mapped onto the same alert type, color, and detail fields. A region without an alert source shows the alert overlay disabled with the reason "no alert source for this region".

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

Point forecasts for a location the user chooses: the home location, a placed pin, or a clicked map point. The forecast panel shows the next hours and days of temperature, precipitation amount, precipitation probability where the service provides it, wind speed and direction, and cloud cover, in the user's unit and time preferences, with the issue time and the service name.

Forecast services are catalog entries. Inside the United States the service is `api.weather.gov`, and everywhere else it is MET Norway's `api.met.no` locationforecast, which covers the globe and needs no key. Each carries its attribution into the attribution panel. A request sends the forecast point's coordinates to the service, and the Network Policy table lists this. Responses are held in memory and honor the service's `Expires` and `Last-Modified` headers, and nothing is cached to disk.

The panel is in one of five states: loading, ready (with the issue time), stale (issue time older than the service's own update interval, with its age), unavailable (with the specific error), and "no forecast service for this location". With nowcasting on, the panel shows the nowcast arrival range for the point next to the model forecast, in a separately labeled block.

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

## Code Style

These rules bind all code in the project. Lints are a gate, and a green lint run says only that the mechanical surface is clean. Wrong-spec tests, unexercised generality, subsystem bypasses, safety arguments, and narrative comments need reading.

**Types first.** Bad states are unrepresentable: enums for exclusive states, newtypes for validated values, typestate for protocol ordering. Runtime checks live at real boundaries: parsing, IO, FFI. A defensive check for an invariant the type already guarantees is a bodge.

**Safe Rust.** Edition 2024. New `unsafe` needs explicit approval first, with the location, the measured gain, the safety argument, and the cost of the safe alternative. Approved `unsafe` lives in minimal leaf functions that contain no safe logic, and every site carries its safety argument. Miri runs on `unsafe`-adjacent tests where feasible.

**Ownership.** Resolution order: shared borrow, mutable borrow, move, and shared ownership only with explicit permission. A clone added to satisfy the borrow checker means the ownership model is wrong, so the code is restructured until the clone disappears or its necessity is documented and load-bearing. `Arc<Mutex<T>>` and `Rc<RefCell<T>>` are not default architecture.

**Errors.** Library code carries typed `thiserror` enums whose variants carry the invalid value as a field wherever possible. `anyhow` appears at the UI boundary only. `Box<dyn Error>` on a public API, a single `Msg(String)` variant, and `.map_err(|e| e.to_string())` erase the failure modes callers need to match on. Every error reaches the user as a human-readable message.

**Panics.** No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, or `unreachable!` in production code, and no indexing or slicing on computed indices without a proven bound. An `expect` with a proof is legitimate, and its message states the invariant. Tests may fail loudly.

**Fallbacks.** No `unwrap_or_default`, no `unwrap_or(0)` or `unwrap_or("")`, no `let _ =` on fallible operations, no catch-all arms that swallow a case. A fallback names the condition it handles, is visible to the caller, and is pinned by a test. If the caller cannot tell from outside whether the main path or the fallback ran, the fallback is the defect.

**Abstraction.** Generality is exercised, not anticipated. One implementor is inlined or becomes an enum. Closed sets dispatch through `match` on an enum, never through `dyn Trait`. A struct literal replaces a builder, and generic parameters appear only where instantiated at more than one type.

**One computation per value.** Every value with more than one consumer has exactly one computation site. Duplicated derivations are fixed by extraction. A feature built beside a canonical capability, with its own storage, IO, or error surface, is routed through that capability, and the capability is extended where it lacks something.

**Small units.** Each new behavior gets a unit whose reason to change is that behavior. If the last five features all landed in the same unit for different reasons, the unit is factored. Functions past 100 lines, 7 or more arguments, and cognitive complexity past 15 are factoring tasks.

**Effects visible in types.** Mutation, IO, time, randomness, and global state stay out of code that presents as pure computation. `static` holds compiled-once values and atomics, each named at the site. Interior mutability serves genuinely single-threaded shared state with the sharing reason stated. Other state is threaded through parameters, returned, or held in a struct the caller can see.

**Strings at system boundaries.** File contents, user input, display text, and serialized forms are text. Internal values get types at birth: paths are `PathBuf` or `&Path`, fixed key sets are enums, modes and commands are enums the dispatcher matches on, identifiers are newtypes. Values are parsed once and carried as their type.

**Performance.** Borrows over allocations in hot paths, `&str` for borrowed data, `Cow` for conditional ownership, iterators end to end. Measure the hot path, remove the allocation, measure again.

**Dead weight.** A stub is implemented, deleted, or documented as unimplemented in user-facing docs and unreachable from the UI. Dead code and unused public API are deleted. A feature counts as working when the binary has been exercised and the code path does what its name promises.

**Unnecessary complexity.** A layer with no client is deleted. Complexity that earns its place names its client in a mechanistic comment: the second implementor, the measured hot path, or the invariant it maintains.

**Comments.** One sentence, mechanistic, about the machine: memory layout, data flow, algorithm, or a verifiable invariant. The only other comments are permission requests.

**Structure and naming.** One module per concern, named files, no `mod.rs`. `main.rs` holds argument parsing and app construction only. Imports run `std`, then external crates, then internal modules, one blank line between groups. Types are named after what they are, with no `Manager`, `Service`, `Controller`, `Factory`, `Provider`, `Helper`, `Util`, or `Handler` in type names. Newtypes carry a validated constructor returning `Result`.

**Tests.** Tests return `Result` and propagate with `?`. Assertions state the expected value, the actual value, and the tolerance. Error-path tests match the specific variant and the value it carries. A test asserts the behavior the spec requires, and code and test are fixed as a pair. No exemption lists, skip filters, or `#[ignore]`d tests. A deliberate sabotage of the code must turn a test red. Round-trip tests accompany absolute reference value tests and never replace them. Coverage floor: coordinate transforms against the reference values in this document, round-trip at eight azimuths at 75 km from the KPAH test origin, every error variant with its carried value, tile transforms at two zoom levels, colormap keypoints and out-of-range paths.

**Lints.** `#[allow]` is not used. The lint gate is `cargo clippy -- -D warnings` over the `[lints]` tables in `Cargo.toml` and the thresholds in `clippy.toml` (cognitive complexity 15, 7 arguments, 100 lines). Floating point is not denied: the math in this document is float math. Every float computation with a known answer carries absolute reference-value tests within a stated tolerance. No `==` or `!=` on computed floats. The NaN `RangeFolded` sentinel is detected with `is_nan()` only. Non-finite values are rejected or counted at decode boundaries, except the designed sentinel the `RangeFolded` mapping itself produces. Domain clamps where rounded inputs can leave a function's domain are stated and tested.

---

## Development Milestones

Each milestone must pass all gates before the next begins: `cargo clippy -- -D warnings` at zero warnings and `cargo test` fully passing. Each milestone's gate also covers the failure modes it introduces: the network failing, malformed input, and a panic in each worker it adds. No milestone is complete with failing tests or outstanding warnings.

### Milestone 1: Core render loop ✓

Window opens. Single hardcoded site (KPAH). Base reflectivity rendered on a plain background with lat/lon grid and range rings. All errors surfaced as visible UI messages. Coordinate math unit-tested against independently verified reference values. Gate geometry read from radial headers. Site coordinates read from volume header. `BelowThreshold` and `RangeFolded` handled correctly.

### Milestone 2: Vector tile pipeline and base map

Tile fetch from the tile host, MVT decode, and client-side rendering of fills and lines from the bundled stylesheet, in muted and dark styles. The tile and style crate is selected here against the criteria in Dependencies. Mouse-wheel zoom and click-drag pan, sharp at fractional zoom levels. Radar overlay reprojected correctly at all zoom levels without distortion. Disk cache under `~/.oxrad/tiles/` with expiry, size cap, and startup purge, with a PMTiles archive as an offline source. Fetch concurrency and rate limits follow the host's terms, recorded in `docs/NETWORK.md`, which this milestone creates. Graceful fallback to coordinate grid on tile failure. Attribution panel. Tile forward/inverse transforms unit-tested against the reference values.

### Milestone 3: Local storage and configuration

Establish the `~/.oxrad` directory structure. Implement the `paths` module as the single source of all file paths. User configuration in `~/.oxrad/config.toml` with every field and default in the configuration table, including fields for features that land later. Persist and restore last site, product, map center and zoom, overlay visibility, animation frame interval, and window size as an initial-size hint. Window position is never persisted or set; placement is the window manager's job. Error log at `~/.oxrad/errors.log` with rotation. Cache size caps and eviction. `docs/DEPENDENCIES.md` created. Configuration load failures fall back to defaults and rewrite a clean file.

### Milestone 4: Radar source catalog and site selection

Catalog format and loader, and the NEXRAD catalog. Searchable site picker. First-run picker state with no site selected. Site switch cancels in-progress fetch, clears display, begins new fetch, and labels previous-site data. Last active site persisted. Catalog-versus-header coordinate distance computed and surfaced, with rendering driven by the header.

### Milestone 5: Gate geometry and full data fidelity

Geodesic placement on WGS84 through `geographiclib-rs`, and slant-to-ground conversion with the effective radius, both against the reference tables. All gate geometry (first gate center, gate interval, azimuth spacing) read from radial headers with no fallback constants. NEXRAD decoder module over `nexrad-decode`, with the Archive II decompression crate selected here. Volume assembly paginated to exhaustion. Start/end sentinel verification before decode. Sweep selector by elevation number, not list index. Skipped radial and bin counts verified in UI.

### Milestone 6: Products, legends, units, and palettes

Full dual-pol product suite: Z, V, SW, ZDR, CC, KDP. Product selector reflects availability for the active sweep, level, and source, and disabled products are visually distinct and non-interactive. Unit and time types (distance, velocity, time zone, clock) and the palette type exist from this milestone, so every later display consumes them. The `nws` palette sourced from WSR-88D palette files. Legend with physical-threshold ticks labeled in the active unit. Unit and palette preferences live in settings and persist.

### Milestone 7: Data level toggle (Level III)

Level II / Level III toggle. Level III fetch pipeline. Product catalog enumerated from Level III data at runtime. Product selector reflects active level. Switching levels cancels in-progress fetch and reloads.

### Milestone 8: NEXRAD volume cache and animation

Volume cache under `~/.oxrad/volumes/` with 48-hour expiry, size cap, and startup purge. Animation buffer pre-populated from cache on startup while a fresh fetch runs in the background. Animation controls: play/pause, step forward, step backward, configurable frame interval. Scan timestamp display. Controls disabled with a visible reason when fewer than 2 scans are available. Frame interval persisted to config.

### Milestone 9: Boundary and road overlays

Boundary and road layers from the vector tiles. Census TIGER/Line overlays for United States sources, downloaded on first use of such a source with progress display and integrity verification, cached under `~/.oxrad/shapefiles/`. Per-overlay toggles. Overlay visibility persisted. Graceful degradation when a data source is unavailable.

### Milestone 10: Hover cursor and diagnostics

Full hover display: lat, lon, ground range, azimuth, and product value with units, all in the active display units. "No data" when the cursor is off-bin. Diagnostic status bar showing all skipped counts, tile failures, pipeline state, the coordinate-distance diagnostic, and alert count. Scan timestamp persistent in UI.

### Milestone 11: Map labels

Glyph range fetch and cache, label layout with collision placement, language-aware label names with local-name fallback. Labels above the radar layer.

### Milestone 12: NWS weather alerts

Alert overlay from `api.weather.gov`. Alert polygons color-coded by alert type following NWS standard colors. Zone-geometry resolution and cache for alerts without polygons, with the "area unavailable" state. Configurable poll interval with exponential backoff on failure. Alert detail view on click: headline, times in the active time preference, issuing office, full description. Expired alerts removed immediately. Alert API failure disables the overlay with a visible message and leaves other functionality unaffected. Alert data is not cached to disk.

### Milestone 13: Site health

Cadence from observed volume intervals. States Live, Silent, Cadence unknown, and Unknown on an unreachable feed, derived from the feed listing and distinct from pipeline state. Age shown. Scan mode shown as information. Silent sites keep the normal retry schedule.

### Milestone 14: Place search and nearest site

Geocoder worker with submit-only search, one request in flight, one request per second, session result cache, and identifying User-Agent. Local resolution of site ids, names, and `lat, lon`. Operating-system location control with map-pin fallback. Nearest-site selection with distance shown. First-run behavior from Site Selection. Query text excluded from the error log.

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

Forecast worker and panel for home, pin, and clicked points. United States and MET Norway catalog entries with attribution and network-policy rows. Memory-only caching that honors response headers. Loading, ready, stale, unavailable, and no-service states. Nowcast arrival range shown beside the forecast in a separately labeled block. Forecast-point coordinates excluded from the error log.

### Milestone 23: Localization

Translation catalogs for all strings. Locale-aware number, date, and time formatting. UI language field. A second language shipped to exercise the mechanism.

### Milestone 24: Resilience hardening and performance

Comprehensive review of all failure modes across the full stack: worker panic recovery for every worker, all pipeline states and site-health states reachable and correctly labeled, all error paths confirmed to reach the UI. Malformed-input tests on every decoder. Performance profiling at 60 fps during pan and zoom with the full overlay stack, a mosaic at the concurrency limit, animation, alert polling, and lightning active. Cache integrity: corrupt entries in every cache are detected, deleted, and reported without preventing startup.

### Milestone 25: Release binaries

`cargo build --release` artifacts for Windows, macOS, and Linux from continuous integration, with the `README.md` system-library documentation verified on a clean machine for each.
