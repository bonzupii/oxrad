# oxrad: product architecture and roadmap

## Product

> DESKTOP WEATHER AND RADAR WORKSTATION USABLE BY GRANNY, STORM SPOTTERS, CHASERS, METEOROLOGISTS, SCIENTISTS.

> FULL FUCKING FEATURED FREE DESKTOP WEATHER AND RADAR WORKSTATION USABLE BY GRANNY, STORM SPOTTERS, CHASERS, METEOROLOGISTS, SCIENTISTS. PROFESSIONAL GRADE. KILLS THE FUCKING MOAT HELD BY SUBSCRIPTION BASED SERVICES.

> AND IT'S NOT JUST FOR US. NOT JUST US AND GERMANY. FOR, FUCKING, EVERYONE. EVERYONE.

oxrad is a long-lived desktop product. A test, a screenshot, or one decoded volume is verification evidence, not the product. The product is the working application and the coverage it actually provides.

The ordinary path and the expert path are equally required. A person must be able to search a town by name, read its weather, and find radar without knowing a station identifier. A meteorologist must be able to select products, tilts, times, stations, layers, and measurements without the application hiding what the source actually provides.

## Current state

This section describes the repository as it exists, not the target.

| Area | State | Evidence or limitation |
|---|---|---|
| Global place search | Implemented | OpenStreetMap Nominatim, Unicode queries, six results, in-memory session cache |
| Global forecast | Implemented | Open-Meteo current, hourly, daily; retry, stale labeling, place-scoped supersession |
| NEXRAD station discovery | Implemented | Runtime NWS GeoJSON; disk cache; six-hour refresh; offline cache fallback |
| Station identity | Implemented | `Place` and `Station` are separate; changing one does not change the other |
| NEXRAD Level II acquisition | Implemented | S3 listing, pagination, bzip2 records, volume-start/end validation |
| NEXRAD decoding | Implemented | Type 31 framing and all seven moment blocks, with typed structural rejection |
| Products | Implemented | DREF, DVEL, DSW, DZDR, DRHO, DPHI, DKDP |
| Tilts | Implemented | Complete sweeps retained; tilt list filtered by selected product |
| Geographic rendering | Implemented | WGS84 gate corners, Web Mercator, product palette, rings, grid, pointer readout |
| Global radar ingestion | Partial | RainViewer global reflectivity composite with two-hour history; no national Level II adapters outside the United States |
| TDWR ingestion | Not implemented | TDWR stations are excluded from the selectable feed |
| Viewport model | Partial | The map has Mercator pan, zoom, a global radar tile, and independently placed station meshes; the radar scene remains a centered single-site disc and no common-time values are composed |
| Multi-station composition | Partial | Map viewport overlays the selected station and up to three nearby stations within a six-minute scan window; no resampling, composite values, or gap fill |
| Time history and looping | Partial | RainViewer composite has 13 ten-minute frames with scrub and loop; NEXRAD offers on-demand review, looping, and a drag scrubber over the eight newest volumes, without a preloaded sequence or frame cache |
| Warnings | Partial | NWS active alerts for the United States and territories, drawn by severity with area, urgency, issuance, expiration, count, unreadable-alert count, a selectable severity-sorted list, visible-map filtering, polygon focus, a selected-alert panel with headline, description, and instructions, and feed-failure state; no international alert feeds |
| Map base and labels | Implemented | Offline Natural Earth 1:110m countries, borders, labels, graticule, scale, and site markers; no local detail yet |
| Measurement tools | Partial | Pointer latitude, longitude, range, azimuth, and gate value readout; two-point map measurement with WGS84 distance and initial bearing |
| Network cache | Partial | RainViewer manifest and tile bodies use a bounded disk cache with SHA-256 verification, atomic writes, corruption removal, and offline fallback; no HTTP validators, provider JSON, NEXRAD volumes, or forecast caching |
| Comparison panes | Not implemented | One scene |
| Colorblind palette options | Not implemented | One product palette per product |
| Independent reviews | Not completed | No separate reviewer process has been run |

The application therefore does not yet meet the global, multi-station, full-workstation completion bar. It must not be described as if it does.

## Non-negotiable behavior

- Every visible control has a real state transition and a failure state.
- A town and a radar station are distinct in state, requests, caching, and labels.
- A missing product is disabled or absent. It is never drawn under another product's name or filled with a default.
- Below-threshold, range-folded, and measured gates remain distinct. An absent moment block is represented by an absent product.
- A failed refresh preserves the last good volume with its age. It does not become a current-data indicator.
- A late result for a superseded place, site, product, tilt, or generation is discarded.
- Shutdown must not wait for a full poll interval. Worker channels have one owner and a defined stop condition.
- The application starts without an onboarding wall. A saved site opens after discovery. No setup step requires an ICAO code.

## Architecture

### Identity

`Place` is the searched town or geographic point and is the only input to the forecast. `Station` is a radar source with its own coordinates, capabilities, kind, status, and provenance. `SiteId` is a validated four-character NEXRAD identifier. `nearest_station` maps a place to a station for radar; it does not rewrite the place.

### Source adapters

Each source owns its discovery, metadata, products, protocol, and format. The generic UI sees `Station`, `RadarProduct`, `RadarVolume`, and typed failures. NEXRAD discovery reads NWS GeoJSON and its acquisition reads the Unidata Archive II bucket. Forecast discovery is Open-Meteo. A new radar source is an adapter, not a conditional in the UI.

The current adapter set is deliberately narrow. Advertising an unbuilt adapter is prohibited.

### Radar volume

An Archive II volume is validated at the byte boundary, decompressed per record, and framed into 28-byte message headers plus Type 31 radials. The Type 29/31 size field excludes 12 bytes of that header, so a valid Type 31 message at the pinned archive starts at 0 and the next starts at 9,956 bytes. Every message must end within its record.

A radial is accepted only with a known status, finite azimuth and elevation, a defined resolution code, and nonempty moments. A zero data-block pointer means that moment block is absent. Nonzero pointers must lie beyond the pointer table and cannot overlap. Radials are emitted in measured-azimuth order; duplicate or out-of-order radials are skipped and counted.

A complete sweep has a start, an end, and at least two radials. Complete sweeps are retained in `RadarVolume::sweeps`; the display sweep is moved, not cloned. Tilt selection only chooses a sweep that carries the selected product.

### Work and ownership

```
UI thread
  ├─ StationFeed   (one discovery worker)
  ├─ PlaceSearch   (one search worker)
  ├─ ForecastFeed  (one forecast worker)
  ├─ Pipeline      (one fetcher and one decoder per active site)
  ├─ WarningFeed   (one active-alert worker)
  └─ Display       (one geometry worker)

worker → owned message → UI thread
UI thread → cancel/generation state → worker
```

No worker writes UI state directly. Superseded forecasts and searches are rejected by place/generation. A pipeline result belongs to the site that started it. Dropping a pipeline closes its retry channel; the fetcher checks for closure in short slices so it does not sit in a 30-second wait.

### Rendering

Geometry is built once per volume and product off the UI thread. Gate corners use WGS84 direct geodesics after slant-to-ground conversion. Offsets are site-relative Web Mercator kilometres. Each uploaded mesh owns its uniform buffer and bind group, so a later mesh cannot overwrite an earlier mesh's scale. A product-wide empty result still has the sweep's geographic extent so the interface can draw a truthful "no measured echo" state.

The current renderer draws one mesh. The multi-mesh path is a required architectural next step, not a completed feature.

### Local state

`~/.oxrad/config.toml` stores the selected site. `~/.oxrad/stations.json` stores the last parsed NWS station document and save time. The station cache is written atomically. No downloaded volumes, screenshots, or test logs are stored in the repository.

## Coverage

| Source | Region | Products | Status |
|---|---|---|---|
| NEXRAD Level II via Unidata S3 | United States WSR-88D | DREF, DVEL, DSW, DZDR, DRHO, DPHI, DKDP | Implemented |
| NWS radar stations | United States metadata | WSR-88D and TDWR metadata | Implemented; TDWR selection excluded |
| NWS active alerts | United States and territories | Warning polygons, severity, area, expiration | Implemented; no viewport filter or full alert text |
| Open-Meteo | Global | Current, hourly, daily forecast | Implemented |
| OpenStreetMap Nominatim | Global | Place search | Implemented |
| DWD, ECCC, BoM, JMA, CMA, IMD, Roshydromet, SMN, MetService | National | Unknown until each interface is verified | Planned |
| OPERA / ODIM_H5 | Europe and participating networks | Unknown until decoded | Planned |

The absence of a working international radar decoder is a product limitation. It is not hidden behind a source list.

## Coordinate reference

KPAH test origin: 37.06833 N, 88.77194 W. Geodesic direct distance: 75 km. Tolerance: 5e-7 degrees.

| Azimuth | Latitude | Longitude |
|---|---|---|
| 0 | 37.744095 | -88.771940 |
| 45 | 37.544662 | -88.171837 |
| 90 | 37.065332 | -87.928621 |
| 135 | 36.588961 | -88.179319 |
| 180 | 36.392487 | -88.771940 |
| 225 | 36.588961 | -89.364561 |
| 270 | 37.065332 | -89.615259 |
| 315 | 37.544662 | -89.372043 |

Slant-to-ground uses the 4/3 effective-radius model with a 6371.0 km mean radius. Web Mercator clamps latitude to 85.0511287798066 degrees. Longitude is normalized to [-180, 180), keeping -180 as the western edge. Antimeridian, Mercator-limit, zero-range, near-coincident, and antipodal cases have tests.

## Roadmap

The phases are ordered by dependency. A phase is complete only when its behavior runs in the binary and its failure paths are tested. None of the planned phases is complete.

### Phase 0: data correctness and product model (implemented)

Archive framing, complete sweep assembly, all seven moments, product capability model, tilt filtering, gate categories, projection edge cases, network parsing, and the pinned KPAH acceptance test.

### Phase 1: ordinary place workflow (implemented)

Place search, international names, global forecast, stale handling, place/station separation, and nearest-station lookup.

### Phase 2: runtime station discovery (implemented)

NWS discovery, typed parsing, disk cache, atomic cache writes, offline fallback, and scheduled refresh. Add source adapters for other countries from this boundary.

### Phase 3: geographic viewport and multi-station foundation (in progress)

The offline vector map, pointer-anchored Mercator zoom, drag pan, station markers, and click selection are implemented. Remaining work is one shared geographic viewport for the radar layer, viewport station filtering, coverage intersection, a multi-station selection model, and a persistent station list. This must precede composite products because a composite without a shared viewport is only a fixed disc.

### Phase 4: multi-station acquisition and composition (in progress)

Bounded independent pipelines and per-mesh transforms render the selected station plus up to three nearby stations in the map viewport, with debounced re-query after pan or zoom and contributing-site labels. Layers outside a six-minute window of the selected scan are withheld and named as time-excluded. Remaining work is exact-time resampling, deterministic per-product overlap values, gap disclosure, and tests for overlap order and viewport changes during loading.

### Phase 5: time history and professional inspection (in progress)

The RainViewer global composite has a 13-frame, two-hour scrubber and loop. NEXRAD has an on-demand list of the eight newest complete volumes from the last three days; selecting one pauses the live pipeline, fetches and decodes that volume alone, and reports its UTC scan time. Play history advances and loops through the listed scans, and a drag scrubber fetches the chosen frame on demand. Two-point geodesic measurement is implemented on the map. Remaining work is a preloaded NEXRAD sequence and frame cache, scan comparison, product comparison panes, and storm motion where the data supports it.

### Phase 6: authoritative context (in progress)

NWS active-alert polygons, severity colors, area, urgency, issuance, expiration, layer toggle, visible-map filtering, refresh, failure reporting, and a selected-alert panel with the full headline, description, and instructions are implemented for the United States and territories. One malformed alert is counted and does not discard the valid collection. Remaining work is authoritative alert adapters for other regions. Observation, forecast, and warning time remain distinct concepts in the interface.

### Phase 7: international radar adapters (in progress)

The RainViewer global composite is integrated as one global reflectivity source. Implement and verify each authoritative national source independently. The order is driven by accessible interfaces and formats, not by country prestige. DWD and other ODIM_H5 feeds require a tested HDF5/ODIM decoder before they can be listed as supported. Every adapter must expose its actual products, coverage, update cadence, and limitations.

### Phase 8: resilience, accessibility, and release

Complete cancellation at the transfer boundary, caching for NEXRAD volumes and every provider response, conditional revalidation, reduced idle repaint, colorblind-safe palettes, light and high-contrast themes, resize behavior, release packaging, and measured interaction budgets. Run the two independent full-system reviews required by the product directive. No commit is made before both reviews are clean.

## Gates

```
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo test
cargo build
cargo build --release --locked
```

`cargo test` includes live discovery, live forecast, live active warnings, live listing, and the pinned archive download, size, SHA-256, and decode. A network failure fails those tests. GUI behavior is exercised separately under Xvfb with software Vulkan; screenshots are evidence, not correctness proof.

## Completion

The product is complete only when the working application supports the ordinary and expert workflows above, every claimed source is backed by real acquisition and decoding, every failure is representable, and two genuinely independent full-system reviews find zero discrepancies. The current repository does not meet that contract.
