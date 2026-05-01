# oxrad — Architecture and Development Roadmap

## Purpose

oxrad is a storm-chaser-grade NEXRAD weather radar viewer. Its primary users are storm chasers, emergency managers, and weather spotters making real-time decisions during severe weather events. A display error, a frozen screen, or a corrupted bin placement is not a UI inconvenience — it is a safety failure. Every design decision in this document is made with that constraint as the first priority.

---

## Design Principles

**No hardcoded geophysical constants in rendering paths.** Gate geometry, azimuth spacing, site coordinates, and elevation angle are all read from the data. If the data does not supply a value, the bin or radial is rejected and counted — not substituted with a default.

**All floating-point math is computationally verified.** No formula in this codebase is derived from memory or inferred from training data. Every coordinate transform, projection, and range calculation is sourced from primary references (Snyder USGS PP-1395, NEXRAD ICD, OSM slippy map spec) and verified by independent numerical computation before being locked into the codebase. If a formula is in dispute, it is tested against known reference values before it is used in production.

**Errors are always visible.** A failure the user cannot see is worse than a crash. Every network error, parse failure, and data anomaly surfaces as a labeled UI state. The application never presents silence as success.

**The UI thread never blocks.** All I/O, decoding, and computation run on background workers. The UI remains responsive and interactive regardless of what the data pipeline is doing.

**No unauthorized decisions.** The codebase does not silently normalize, substitute, default, or approximate. When a value is unavailable, the affected feature is disabled with a visible reason.

---

## Coordinate Mathematics

All coordinate math is locked to verified implementations.

**Azimuthal equidistant — forward and inverse.** Used to project NEXRAD sweep geometry onto geographic coordinates and to convert screen positions back to lat/lon. The forward transform takes slant range (converted to ground range using elevation angle), azimuth, and site origin. The inverse is used for hover cursor display and overlay intersection.

**Ground range from slant range.** Slant range from the radar is not ground range on the map. The conversion uses the sweep elevation angle and the effective Earth radius (`4/3 × R` for standard atmosphere refraction). This correction is small at 0.5° elevation but exceeds 500 m at 5° — meaningful for accurate bin placement.

**Web Mercator tile coordinates — forward and inverse.** Used for OSM tile fetching and positioning. Latitude must be clamped to the Web Mercator valid range before computation.

**Beam geometry.** Each range bin is a quadrilateral defined by its near and far ground range edges and its left and right azimuth edges. All four corners are projected independently using the forward transform. Gate near and far edges are derived from the gate center (as reported in the radial header) offset by half the gate interval in each direction.

**Earth constants.** Two radius values are maintained separately and are not interchangeable:
- **Authalic radius** (6371.0 km) — lat/lon transforms and surface distance.
- **Effective radius** (8494.7 km, 4/3 × authalic) — beam height calculations and slant-to-ground range conversion under standard atmosphere refraction.

All unit test reference values are derived from independent numerical computation, not from the implementation under test.

---

## Data Model

### NEXRAD Level II

Level II is full-resolution dual-polarization volume scan data. It is the primary data source for storm chasing.

**Volume assembly.** A real-time volume on the AWS feed consists of approximately 55 chunks delivered over several seconds. A complete volume requires paginating the chunk list to exhaustion, sorting chunks by sequence number where the API permits, and verifying the presence of start and end sentinel chunks before decoding. Decoding a partial volume and presenting it as complete is not permitted.

**Sweep selection.** A volume contains multiple elevation sweeps. The application selects sweeps by elevation number, not by list index. The lowest available elevation angle is the default. `first()` is not an acceptable sweep selector.

**Gate geometry.** For each moment and each radial, the following values are read directly from the radial header:
- Range to first gate center (converted from meters to kilometers)
- Gate interval (converted from meters to kilometers)
- Azimuth center and spacing

No fallback constants are permitted. If a radial does not supply valid geometry, it is skipped and counted in a diagnostic counter visible in the UI.

**Gate value handling.** `BelowThreshold` gates are mapped to the minimum valid value for that moment (-32 dBZ for reflectivity). `RangeFolded` gates are mapped to a distinct NaN sentinel that renders as a visually distinct color. Both are kept in the bin array to preserve index-to-range alignment. A skipped gate shifts all subsequent gates inward — this is a safety defect.

**Site coordinates.** Radar site latitude and longitude are read from the volume header and stored in `RadarVolume`. No geographic constant is hardcoded in the rendering path.

### NEXRAD Level III

Level III products are derived, lower-resolution products distributed via NOAA's public HTTPS infrastructure. The data level toggle switches the fetch pipeline and updates the product selector to reflect only products available at the active level.

---

## Local Storage — `~/.oxrad`

All persistent application data lives under `~/.oxrad`. The directory is created on first run if it does not exist. Every subdirectory and file path is defined in a single `paths` module — no path strings appear elsewhere in the codebase.

### Tile cache — `~/.oxrad/tiles/`

OSM raster tiles cached as individual files named by `{z}/{x}/{y}.png`. Tiles are fetched on demand and written to disk immediately. On subsequent requests, the cached file is used without a network request if it exists and is not expired. Cache expiry follows the `max-age` directive from the OSM tile server response headers, defaulting to 24 hours if the header is absent. Cache entries older than 7 days are purged on startup. If the cache directory is unavailable, the application falls back to memory-only caching for the session and surfaces a warning in the UI.

### NEXRAD volume cache — `~/.oxrad/volumes/`

Decoded radar volumes cached by site and timestamp, named `{SITE}_{YYYYMMDD_HHMMSS}.bin` in a compact serialized format. The cache is used to populate the animation buffer on startup without a network fetch — if cached volumes exist for the active site, they are loaded immediately while a fresh fetch runs in the background. Cache entries older than 48 hours are purged on startup. The volume cache is never used as a substitute for a fresh fetch; it is supplementary. Cached volumes that fail to deserialize are deleted and logged.

### Shapefile cache — `~/.oxrad/shapefiles/`

Census TIGER/Line shapefiles downloaded on first run and stored permanently. Shapefiles are not re-downloaded unless explicitly triggered by the user or if the cached files fail to parse. Download progress and checksum verification results are surfaced in the UI.

### Error log — `~/.oxrad/errors.log`

All errors that are surfaced to the UI are also written to the error log with a timestamp, the active site, and the full error chain. The log is append-only, capped at 10 MB by rotating to `errors.log.1` when the limit is reached. The log exists for post-incident review — it is not a substitute for UI visibility. Errors that do not reach the UI must not appear only in the log.

### User configuration — `~/.oxrad/config.toml`

User preferences persisted across sessions in TOML format. The configuration file is written only when settings change. If the file is absent, malformed, or contains unrecognized keys, the application starts with defaults and attempts to rewrite a clean config — it does not fail to start. All configuration fields have documented defaults. The following settings are persisted:
- Last active site (ICAO code)
- Last active data level (Level II / Level III)
- Last active radar product
- Last active elevation index
- Map center position (lat, lon) and zoom level
- Overlay visibility state (county boundaries, highways) individually
- Animation frame interval (milliseconds)
- Window size and position
- Below-threshold bin visibility toggle
- Color scale opacity for radar overlay

---

## Application Architecture

### Threading Model

Strict one-way data flow:

```
fetch worker → channel → decode worker → channel → UI thread
tile worker → channel ──────────────────────────→ UI thread
alert worker → channel ──────────────────────────→ UI thread
```

The fetch worker handles all NEXRAD network I/O. The decode worker handles chunk assembly, volume validation, and moment extraction. The tile worker handles OSM tile fetching and cache reads. The alert worker handles NWS API polling. The UI thread owns all display state exclusively. No shared mutable state crosses thread boundaries.

### Pipeline State Machine

The radar data pipeline has explicit states the UI reflects directly:

- **Idle** — no site selected or fetch not yet started
- **Fetching** — chunk list and download in progress, with chunk count visible
- **Assembling** — chunks downloaded, awaiting volume completion sentinels
- **Decoding** — volume being decoded
- **Ready** — volume decoded, sweeps available for display
- **Error** — fetch or decode failed, specific error message visible
- **Stale** — displayed data is from a previous fetch cycle, new fetch in progress

Transitions between states are explicit. The UI never infers state from the presence or absence of data.

### Worker Resilience

A worker thread that panics must be caught at the thread boundary, reported to the UI as an error state with a descriptive message, and must not crash the application. Worker panics are expected operating conditions, not unrecoverable failures.

---

## Feature Specification

### Site Selection

A searchable dropdown containing all operational US WSR-88D ICAO codes. Selecting a site cancels any in-progress fetch, clears the current radar display, sets the pipeline to Fetching, and begins loading the latest volume. Stale data from the previous site is never displayed without a visible stale label. When the new volume is ready, the display updates atomically. The last active site is persisted to `config.toml` and restored on startup.

### Data Level Toggle

A two-position toggle between Level II and Level III. Switching levels cancels any in-progress fetch, reloads data for the current site at the new level, and updates the product selector to show only products available at the new level. Products unavailable at the active level are hidden or visibly disabled.

### Radar Products

Product availability depends on data level and the specific volume scan.

**Dual-polarization suite (Level II primary):** base reflectivity (Z), base velocity (V), spectrum width (SW), differential reflectivity (ZDR), correlation coefficient (CC), specific differential phase (KDP).

**Level III products** are enumerated from the actual Level III product catalog at runtime, not hardcoded.

Each product has a color scale legend that updates when the active product changes. The legend shows the value range, units, and labeled tick marks at meteorologically significant thresholds. Color scales follow NWS operational conventions sourced from the WSR-88D product palette files.

A product unavailable in the current volume or at the current data level is disabled and visually distinct — it does not produce an error when in the disabled state.

### Map Base

OSM raster tiles with mouse-wheel zoom and click-drag pan. Radar overlay is reprojected correctly at every zoom level without distortion. Tiles are disk-cached under `~/.oxrad/tiles/`. On tile load failure the application falls back to a blank background with a coordinate grid — no broken placeholders. The map center and zoom level are persisted to `config.toml`.

### Overlays

County/parish boundaries and major highways from Census TIGER/Line shapefiles, cached under `~/.oxrad/shapefiles/`. Both overlays are enabled by default and individually toggleable. Visibility state is persisted to `config.toml`. If shapefiles are unavailable, the overlay toggle is disabled with a visible reason and the application continues normally.

### Hover Cursor

Displays at all times: latitude, longitude, ground range from the active site, azimuth from the active site, and the product value of the nearest bin with units. If the cursor is not over a bin, the product value shows "no data." If no volume is loaded, only lat/lon is shown.

### Sweep Elevation Selector

Shows all available elevation angles in the current volume. The user steps through sweeps or selects by elevation number. The lowest elevation is the default. Each sweep entry shows its elevation angle in degrees and its scan time offset.

### Animation

Loops the last 2–3 complete volume scans for the active site and product. Controls: Play/Pause, step forward, step backward, configurable frame interval. The scan timestamp for the currently displayed frame is shown persistently. Controls are disabled with a visible reason when fewer than 2 complete scans are available. On startup, the volume cache under `~/.oxrad/volumes/` is used to pre-populate the animation buffer while a fresh fetch runs in the background. Frame interval is persisted to `config.toml`.

### Color Scale Legend

A persistent legend panel showing the color ramp, value range, and units for the active product. Updates immediately on product change. Tick marks at meteorologically significant thresholds (e.g., 35 dBZ and 50 dBZ for reflectivity, ±30 m/s for velocity).

### NWS Weather Alerts

Active weather alerts from the NWS public API (`api.weather.gov`) are displayed as geographic overlays on the map. Alert polygons are drawn with color coding by alert type (tornado warning, severe thunderstorm warning, tornado watch, severe thunderstorm watch, special weather statement, etc.) following NWS standard alert colors. Alerts are polled on a configurable interval (default 60 seconds) while the application is running. Alert details — headline, effective time, expiration time, issuing office, and full description — are accessible by clicking an alert polygon. Expired alerts are removed from the display immediately. If the NWS API is unreachable, the overlay is disabled with a visible message and polling continues with exponential backoff — the application continues operating normally. Alert data is not cached to disk; it is always live or absent.

### Status and Diagnostics

A persistent status bar showing: current site and data level, volume scan timestamp, pipeline state, skipped radial and bin counts (non-zero counts highlighted in yellow), tile load failures, active alert count, and any active error condition. Non-zero skipped counts indicate data the user is not seeing and must always be visible.

---

## Network Policy

The application makes outbound connections to exactly these destinations:

1. AWS NEXRAD S3 bucket (HTTPS) — Level II data
2. NOAA public HTTPS distribution — Level III data
3. `tile.openstreetmap.org` (HTTPS) — map tiles
4. Census Bureau public HTTPS endpoint — shapefile download, first run only
5. `api.weather.gov` (HTTPS) — NWS active weather alerts

No other outbound connections under any circumstances. Zero telemetry. Zero analytics. No third-party SDKs that initiate their own network activity.

---

## Build and Portability

Single `cargo build` on Windows, macOS, and Linux. No platform-specific scripts. No build steps outside Cargo. Required system libraries for Linux are documented in `README.md`.

---

## Development Milestones

Each milestone must pass all gates before the next begins: `cargo clippy -- -D warnings` at zero warnings, `cargo test` fully passing. No milestone is complete with failing tests or outstanding warnings.

### Milestone 1 — Core render loop ✓

Window opens. Single hardcoded site (KPAH). Base reflectivity rendered on a plain background with lat/lon grid and range rings. All errors surfaced as visible UI messages. Coordinate math unit-tested against independently verified reference values. Gate geometry read from radial headers. Site coordinates read from volume header. `BelowThreshold` and `RangeFolded` handled correctly.

### Milestone 2 — OSM tile mapping

Replace the plain background with OSM raster tiles. Mouse-wheel zoom and click-drag pan. Radar overlay reprojected correctly at all zoom levels without distortion. Tile disk cache under `~/.oxrad/tiles/` with expiry and startup purge. Rate limiting on tile fetches. Graceful fallback to coordinate grid on tile failure. OSM tile forward/inverse transforms unit-tested against known reference values.

### Milestone 3 — Local storage and configuration

Establish the `~/.oxrad` directory structure. Implement `paths` module as the single source of all file paths. User configuration in `~/.oxrad/config.toml` with all fields from the configuration spec. Persist and restore: last site, last product, map center and zoom, overlay visibility, animation frame interval, window geometry. Error log at `~/.oxrad/errors.log` with rotation. Configuration load failures fall back to defaults and rewrite a clean file — they do not prevent startup.

### Milestone 4 — Site selection

Searchable dropdown of all US WSR-88D ICAO codes. Site switch cancels in-progress fetch, clears display, begins new fetch. Stale data labeled during transition. Last active site persisted to config. Verify site coordinates are always read from volume header with no hardcoded fallback.

### Milestone 5 — Gate geometry and full data fidelity

Slant-to-ground range conversion using elevation angle and effective Earth radius. Verify all gate geometry (first gate center, gate interval, azimuth spacing) is read from radial headers with no fallback constants. Volume assembly paginated to exhaustion. Start/end sentinel verification before decode. Sweep selector by elevation angle, not list index. Skipped radial and bin counts verified in UI.

### Milestone 6 — Product switching and color legends

Full dual-pol product suite: Z, V, SW, ZDR, CC, KDP. Product selector updates to reflect availability for the active sweep and data level. Disabled products are visually distinct and non-interactive. Color scale legend panel with meteorological threshold tick marks. NWS-convention color scales sourced from WSR-88D palette files.

### Milestone 7 — Data level toggle (Level III)

Level II / Level III toggle. Level III fetch pipeline. Product catalog enumerated from Level III data at runtime. Product selector reflects active level. Switching levels cancels in-progress fetch and reloads.

### Milestone 8 — NEXRAD volume cache and animation

Volume cache under `~/.oxrad/volumes/` with 48-hour expiry and startup purge. Animation buffer pre-populated from cache on startup while fresh fetch runs in background. Animation controls: play/pause, step forward, step backward, configurable frame interval. Scan timestamp display. Controls disabled with visible reason when fewer than 2 scans available. Frame interval persisted to config.

### Milestone 9 — Shapefiles and overlays

County/parish boundary and highway overlays from Census TIGER/Line data. First-run download from Census Bureau HTTPS with progress display and integrity verification. Cached under `~/.oxrad/shapefiles/`. Per-overlay toggles. Overlay visibility persisted to config. Graceful degradation when shapefiles unavailable.

### Milestone 10 — Hover cursor and diagnostics

Full hover display: lat, lon, ground range, azimuth, product value with units. "No data" when cursor is off-bin. Diagnostic status bar showing all skipped counts, tile failures, pipeline state, and alert count. Scan timestamp persistent in UI.

### Milestone 11 — NWS weather alerts

Alert overlay from `api.weather.gov`. Alert polygons color-coded by alert type following NWS standard colors. Configurable poll interval with exponential backoff on failure. Alert detail view on polygon click: headline, times, issuing office, full description. Expired alerts removed immediately. Alert API failure disables overlay with visible message, does not affect other functionality. Alert data is never cached to disk.

### Milestone 12 — Resilience hardening and performance

Comprehensive review of all failure modes: worker panic recovery verified through the full feature stack, all pipeline states reachable and correctly labeled, all error paths confirmed to reach the UI. Performance profiling at 60 fps target during pan and zoom with full overlay stack, animation, and alert polling active. Cache integrity: verify corrupt cache entries in all three caches are detected, deleted, and reported without preventing startup.

---

## Out of Scope

- Distribution packaging (.deb, .rpm, .nix, .appimage)
- Telemetry or analytics of any kind
- Any network connection not listed in the Network Policy section
- Mobile or web targets
