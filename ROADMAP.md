# oxrad: product architecture and roadmap

## Product

A free desktop workstation for weather, radar, and environmental observation, usable by ordinary people and by storm spotters, chasers, meteorologists, and scientists. Global, not national. Free, without account or paid tier.

[STYLE.md](STYLE.md) governs code and prose. [AGENTS.md](AGENTS.md) binds changes. [docs/PRODUCT_REQUIREMENTS.md](docs/PRODUCT_REQUIREMENTS.md) defines requirements. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) defines structure.

## Verified state

Only behavior observed in the running binary is listed as verified.

| Area | State | Evidence or limitation |
|---|---|---|
| NEXRAD Level II acquisition | Verified | S3 listing, record framing, volume assembly, pinned archive test |
| NEXRAD decoding | Verified | Type 31 radials, seven moment blocks, typed structural rejection |
| Products and tilts | Verified | DREF, DVEL, DSW, DZDR, DRHO, DPHI, DKDP; sweeps filtered by product |
| Radar rendering | Verified | WGS84 gate corners, product palette, rings, grid, pointer readout |
| Radar history | Verified | Eight recent volumes, loop, scrub, UTC label, cached-volume label |
| Global place search | Verified | Photon live results; partial and transposed names, ranked; proximity bias absent |
| Global forecast | Verified | Open-Meteo current, hourly, daily, retry, stale labeling |
| Station discovery | Verified | Runtime NWS directory, disk cache, six-hour refresh |
| Map basemap | Verified | Natural Earth 1:110m countries, borders, labels, graticule, scale |
| Map radar geometry | Verified | Geodesic 50 km rings, eight spokes, unit-aware labels, 1/5/15 degree adaptive graticule |
| Map station overlays | Verified | Selected plus three nearby stations within a six-minute window |
| Map measurement | Partially verified | Two-point geodesic distance and bearing; no area |
| Global radar composite | Verified | RainViewer manifest, tiles, scrub, loop, attribution |
| Warnings | Partially verified | NWS polygons, list, detail, viewport filter; United States only |
| Response cache | Verified | SHA-256 verification, atomic write, eviction, corruption removal |
| Themes | Implemented | Dark and light palettes, persisted preference; two map controls still draw dark in light mode |
| Units | Partial | Metric and imperial for distance and temperature; speed, pressure, precipitation, and elevation unconverted |
| Layer model | Absent | Three independent toggles; no ordering, opacity, or grouping |
| International radar | Absent | RainViewer composite only |
| Performance measurement | Absent | No baseline has been taken |

## Defect register

These are established defects. They are requirements, not questions.

| ID | Defect | Requirement |
|---|---|---|
| D-01 | Layout is poorly proportioned and cramped | UI-1, UI-4 |
| D-02 | Control and text alignment is inconsistent | UI-1 |
| D-03 | Search placeholder alignment is wrong | UI-1 |
| D-04 | Typography is too small in metadata, forecast, and status | UI-3 |
| D-05 | Secondary text contrast is insufficient | A11Y-1 |
| D-06 | Controls lack a consistent visual language | UI-1 |
| D-07 | Panels look assembled rather than designed | UI-1, UI-4 |
| D-08 | Application reads as unfinished | UI-2, UI-6 |
| D-09 | Local and global radar rendering are inconsistent | RAD-4 |
| D-10 | Vector map is visually impoverished | MAP-1, MAP-3 |
| D-11 | Zoom reveals no street, road, or city detail | MAP-4 |
| D-12 | Radar rings and grid are misaligned | MAP-10 |
| D-13 | Radar geometry is absent in map view | MAP-9 |
| D-14 | No usable map legend | MAP-12 |
| D-15 | Unit switching is absent | UNIT-1, UNIT-2 |
| D-16 | Colourblind accessibility is absent | A11Y-5, UI-6 |
| D-17 | Performance is severely degraded | PERF-1 to PERF-5 |
| D-18 | Advanced controls are inadequate | ARCH, UI |
| D-19 | Data layering is insufficient | ENV-1 |
| D-20 | Global coverage is inconsistent | DATA-3, DATA-4 |
| D-21 | Search requires exact names | UX-1 to UX-5 |
| D-22 | Subsystems are not integrated | ARCH |
| D-23 | Two-point measurement is partial | MAP-11 |

## Structural defects

| Defect | Evidence |
|---|---|
| `app.rs` god module | 4254 lines, 60 fields, 30 methods |
| No provider abstraction | Five sources reach the UI through separate state |
| No layer abstraction | Each overlay owns visibility and draw path |
| No unit model | No conversion, no preference, no canonical value type |
| No token layer | Colours and sizes are literals at call sites |
| Single-mesh renderer | Composition needs a new renderer |

## Non-negotiable behavior

- Every visible control has a real state transition and a failure state.
- A town and a radar station are distinct in state, requests, caching, and labels.
- A missing product is absent. It is never drawn under another product's name.
- Below-threshold, range-folded, and measured gates remain distinct.
- A failed refresh preserves the last good volume with its age. It never becomes a current-data indicator.
- A late result for a superseded place, site, product, tilt, or generation is discarded.
- A layer beyond its time tolerance is withheld and named.
- Shutdown does not wait for a poll interval.
- No onboarding wall. A saved site opens after discovery. No setup step requires an ICAO code.

## Architecture

Current module boundaries, ownership, data flow, and measured structural defects are recorded in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The target contracts are recorded there as well.

The concurrency model stays: one UI thread owns all display state; workers own their data and send owned results; no shared mutable state.

## Phases

A phase is complete only when its behavior runs in the binary, its failure paths are tested, and its interface states are inspected.

| Phase | Scope | State |
|---|---|---|
| 1 | Documentation and repository audit | In progress: this document set |
| 2 | Architectural remediation: workspace, layer, source, units, preferences | Not started |
| 3 | Visual redesign: tokens, layout, typography, themes, disclosure | Not started |
| 4 | Map and geographic infrastructure: detail, roads, terrain, geometry | Not started |
| 5 | Radar visualization: interpolation, compositing, mosaics, controls | Not started |
| 6 | International parity: DWD first, then other national adapters | Not started |
| 7 | Environmental layers: seismic, wildfire, satellite, air quality, hydrology | Not started |
| 8 | Performance and caching: measure, then optimize; unified cache policy | Not started |
| 9 | Accessibility, localization, information radio | Not started |
| 10 | Integration, acceptance, full visual QA, release | Not started |

### Phase detail

**Phase 1.** Documentation set covering product, architecture, interface, sources, caching, testing, and performance. No feature code.

**Phase 2.** Extract workspace and panel state from `app.rs`. Add `Preferences`. Add the layer model and convert the three existing overlays. Add `RadarSource` with NEXRAD as the first implementation. Behavior unchanged at each step.

**Phase 3.** Token module, type scale, panel collapse, focus rings, light theme, level selector. Rebuild panels on the token layer.

**Phase 4.** Level-of-detail basemap with settlements, roads, and hydrography. Shared projection for radar geometry. Map legend. Range rings and spokes in map view.

**Phase 5.** Interpolation modes with measurement provenance preserved. Time-exact composition with a stated overlap rule. Mosaic selection. Derived products computed from source data.

**Phase 6.** DWD first, because the composite format and projection are documented and the licence permits redistribution with attribution. Then each national source in admission order from [docs/DATA_SOURCES.md](docs/DATA_SOURCES.md).

**Phase 7.** Environmental layers through the same layer model. Each layer carries source, units, legend, freshness, and availability.

**Phase 8.** Measure against [docs/PERFORMANCE.md](docs/PERFORMANCE.md). Then optimize the measured bottleneck. Unify cache classes and policy.

**Phase 9.** Colourblind palettes, interface scale, keyboard reach, accessible names, localization, information radio.

**Phase 10.** Full state matrix inspection at four window sizes and two themes. Complete gates. Remote delivery.

## Deferred

An Android companion that streams device position. The desktop keeps location selection behind one module so a position source can be added later without changes to map or radar code.

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

## Gates

```
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo test
cargo build
cargo build --release --locked
```

`cargo test` includes live network tests. GUI behavior is exercised under Xvfb with software Vulkan per [docs/HEADLESS.md](docs/HEADLESS.md). A screenshot is evidence of what was drawn, not proof of correctness.

## Completion

The product is complete when every requirement in [docs/PRODUCT_REQUIREMENTS.md](docs/PRODUCT_REQUIREMENTS.md) is observed in the running binary, every claimed source has a working adapter, every failure is representable, the full visual QA matrix has been inspected, and the performance targets have been measured.
