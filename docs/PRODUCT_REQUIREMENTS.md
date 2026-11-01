# Product requirements

## Product

A free desktop workstation for weather, radar, and environmental observation, usable without training and deep enough for operational meteorology. It runs on Linux, macOS, and Windows through `eframe` and `wgpu`.

The product has one window with one geographic workspace. Radar, forecasts, warnings, satellite, and environmental observations are layers on that workspace. A user can read the weather for a town without knowing a station identifier, and can inspect a specific gate, tilt, and product without leaving the same window.

## Users

| Persona | Task | Required capability |
|---|---|---|
| Ordinary user | "Will it rain here today?" | Search a place, read current and hourly forecast, see nearby radar |
| Storm spotter | "What is approaching my position?" | Radar loop, velocity, warnings, distance and bearing to a feature |
| Storm chaser | "Which road do I take?" | Road detail, storm motion, live position, range and bearing readout |
| Meteorologist | "What does this volume show?" | Products, tilts, cross sections, derived products, provenance |
| Scientist | "Can I trust this value?" | Units, thresholds, missing-data semantics, source and time metadata |
| Climatologist | "How does this compare over time?" | Archive access, explicit time selection, no silent interpolation |

## Capability levels

The application exposes three levels over one state model. A level controls which controls are visible, never which data exists.

| Level | Visible | Hidden |
|---|---|---|
| Basic | Place search, forecast, radar image, product and tilt, warnings, playback | Thresholds, palettes, interpolation, compositing, source selection |
| Standard | Basic plus layer panel, units, map style, legend controls | Per-layer diagnostics, raw readouts, cache state |
| Expert | Everything, including thresholds, interpolation, compositing policy, provenance, diagnostics | Nothing |

The level is a persisted preference. Switching level never discards configured state.

## Requirements

Each requirement is testable. A requirement marked implemented was observed in the running binary. Others are not implemented regardless of what code exists.

### Interaction and search

| ID | Requirement | State |
|---|---|---|
| UX-1 | Search returns results while the user types, without a submit action | Implemented |
| UX-2 | Partial names match: `Chicag` returns Chicago | Implemented |
| UX-3 | Transposed or misspelled names match: `cihcago` returns Chicago | Implemented |
| UX-4 | Exact name matches rank above partial matches | Implemented |
| UX-5 | Results nearer the current map centre rank above distant matches | Not implemented |
| UX-6 | Each result shows country, region, and coordinates | Implemented |
| UX-7 | Obsolete searches are cancelled and their results discarded | Implemented |
| UX-8 | A place selection and a radar station selection are independent | Implemented |
| UX-9 | Every control has a keyboard path and a visible focus state | Not implemented |

Live search is debounced and superseded queries are discarded. Relevance is ordered by name match only; proximity to the map centre is not yet part of ranking.

### Map and geography

| ID | Requirement | State |
|---|---|---|
| MAP-1 | Global, continental, regional, local, and street scales render distinct content | Not implemented |
| MAP-2 | Country and coastline geometry renders from a real dataset | Implemented |
| MAP-3 | Settlements and labels render with collision avoidance | Implemented |
| MAP-4 | Roads and streets render at street scale | Not implemented |
| MAP-5 | Terrain or elevation renders as a selectable layer | Not implemented |
| MAP-6 | Hydrography renders as a selectable layer | Partially implemented |
| MAP-7 | Pan and zoom keep the point under the pointer fixed | Implemented |
| MAP-8 | Coordinates are readable at every zoom level | Implemented |
| MAP-9 | Range rings, azimuth spokes, and the station marker render in map view | Implemented |
| MAP-10 | Radar geometry and the basemap share one projection and stay aligned under zoom | Partially implemented |
| MAP-11 | Distance, bearing, and area measurement work in map view | Partially implemented |
| MAP-12 | A legend states what each visible layer encodes | Partially implemented |

Only two-point distance and bearing are implemented. Area measurement is not. Rivers and admin-1 boundaries are embedded vector layers; they are not yet individually selectable.

### Radar observation

| ID | Requirement | State |
|---|---|---|
| RAD-1 | Every NEXRAD Level II moment decodes with its measured geometry | Implemented |
| RAD-2 | A product is offered only where the loaded volume carries it | Implemented |
| RAD-3 | Below-threshold, range-folded, and measured gates stay distinct | Implemented |
| RAD-4 | Rendering offers native, nearest, bilinear, and smoothed modes | Not implemented |
| RAD-5 | Gate values are readable under the pointer with units and provenance | Implemented |
| RAD-6 | Multiple stations render in one view with a stated time relationship | Partially implemented |
| RAD-7 | Composite values are composed from overlapping stations by a stated rule | Not implemented |
| RAD-8 | Regional, national, and user-defined mosaics are selectable | Not implemented |
| RAD-9 | Radar history lists recent volumes and replays them | Implemented |
| RAD-10 | A missing scan in a sequence is reported, never interpolated | Partially implemented |
| RAD-11 | Derived products (storm-relative velocity, VIL, echo tops) are computed from source data | Not implemented |

### Sources and coverage

| ID | Requirement | State |
|---|---|---|
| DATA-1 | Station discovery happens at runtime from an official source | Implemented |
| DATA-2 | Discovered metadata is cached with provenance and age | Implemented |
| DATA-3 | A radar source outside the United States is integrated | Not implemented |
| DATA-4 | Every source reports coverage, products, cadence, and licence | Partially implemented |
| DATA-5 | A source that cannot be integrated has its blocker recorded | Partially implemented |
| DATA-6 | The interface states where coverage is absent | Not implemented |

### Environmental layers

| ID | Requirement | State |
|---|---|---|
| ENV-1 | Layers share one registration, ordering, and opacity model | Not implemented |
| ENV-2 | Each layer exposes availability, freshness, and failure state | Not implemented |
| ENV-3 | Lightning, satellite, air quality, seismic, and hydrology layers exist | Not implemented |
| ENV-4 | A layer that is hidden performs no fetch or render work | Not implemented |

Warning and global radar visibility flags exist. They are not a layer model.

### Units and time

| ID | Requirement | State |
|---|---|---|
| UNIT-1 | Units are selected once and apply to every panel, legend, and readout | Partially implemented |
| UNIT-2 | Temperature, distance, speed, precipitation, pressure, elevation, and visibility convert correctly | Partially implemented |
| UNIT-3 | Stored values stay canonical; conversion happens at presentation | Partially implemented |
| UNIT-4 | UTC and local time are both available and labelled | Partially implemented |
| UNIT-5 | Decimal precision and coordinate format are configurable | Not implemented |

Forecast units are whatever the provider returns. Radar readouts use product units. No preference system exists.

### Presentation

| ID | Requirement | State |
|---|---|---|
| UI-1 | Every colour and dimension comes from a named token | Partially implemented |
| UI-2 | A light theme and a dark theme both render the whole interface | Partially implemented |
| UI-3 | Body text is at least 13 points at default scale | Partially implemented |
| UI-4 | Every panel collapses and restores without losing state | Not implemented |
| UI-5 | Layout holds at 1040x640 and 2560x1440 | Partially implemented |
| UI-6 | A colourblind-safe palette set is selectable per product | Not implemented |

### Accessibility

| ID | Requirement | State |
|---|---|---|
| A11Y-1 | Text meets 4.5:1 contrast against its background | Partially implemented |
| A11Y-2 | Focus order is logical and focus is always visible | Not implemented |
| A11Y-3 | Every control has an accessible name | Not implemented |
| A11Y-4 | Interface scale is adjustable | Not implemented |
| A11Y-5 | Qualitative meaning survives without colour | Not implemented |

### Performance

| ID | Requirement | State |
|---|---|---|
| PERF-1 | Startup to first painted frame is under 2 seconds on reference hardware | Unmeasured |
| PERF-2 | Pan and zoom sustain 60 frames per second at 2560x1440 | Unmeasured |
| PERF-3 | A warm-cache radar frame appears within 500 ms of selection | Unmeasured |
| PERF-4 | Idle repaint does no work when nothing is animating | Unmeasured |
| PERF-5 | Resident memory stays under 1.5 GB with four radar layers and warnings | Unmeasured |

No measurement exists for PERF-1 through PERF-5. The targets are requirements, not results.

### Resilience

| ID | Requirement | State |
|---|---|---|
| REL-1 | A failed refresh keeps the last good frame, labelled with its age | Implemented |
| REL-2 | A stale frame is never presented as current | Implemented |
| REL-3 | Offline start shows cached data with an offline marker | Partially implemented |
| REL-4 | A corrupt cache entry is detected and removed | Implemented |
| REL-5 | An in-flight request is cancelled when superseded | Not implemented |

## Deferred

An Android companion that streams device position over Bluetooth or Wi-Fi is out of scope. The desktop keeps location selection behind one module so a future position source can be added without changes to map or radar code.

## Acceptance

A requirement is complete when its behavior is observed in the running binary and covered by a test where the behavior is deterministic. Documentation, type definitions, and empty controls do not satisfy a requirement.
