# oxrad

Free desktop weather and radar workstation.

![Historical screenshot](screenshot.png)

The screenshot is historical and does not verify the current build.

oxrad is built for ordinary people and for storm spotters, chasers, meteorologists, and scientists. It has two separate jobs that share one window. A place search answers where the user is and what the weather will do there. A radar station answers what the radar is observing. The town and the station are separate identities throughout the data model and the interface.

The application is free software under GPLv3. It has no account, subscription, advertisement, or paid tier.

## What works today

- Ordinary place-name search through OpenStreetMap Nominatim, with international names and Unicode. Results show the resolved place and its nearest supported radar station. The place and station remain separate: changing the station never changes the forecast point.
- A global current, hourly, and daily forecast from Open-Meteo for the searched place. The panel shows current conditions, 24 hourly periods, seven daily periods, local timezone, units, provider, and source time. A failed refresh preserves the last good forecast and labels it as last received.
- Runtime radar-station discovery from the National Weather Service GeoJSON endpoint. No TSV or TOML site catalog ships with the application. Successful discovery is cached at `~/.oxrad/stations.json`; a later network failure keeps the cached metadata and reports that it is cached. A saved site is opened after discovery completes.
- Live NEXRAD Level II acquisition from the public Unidata AWS archive, bzip2 record framing, Type 31 parsing, complete volume assembly, and Archive II validation.
- Every complete elevation sweep in a volume is retained. The tilt selector offers only sweeps that carry the selected product and moves the selected sweep into the display without cloning it.
- All seven NEXRAD moments are decoded: base reflectivity (`DREF`), base velocity (`DVEL`), spectrum width (`DSW`), differential reflectivity (`DZDR`), correlation coefficient (`DRHO`), differential phase (`DPHI`), and specific differential phase (`DKDP`). A product is offered only when the source and the loaded volume carry it. Selecting velocity from a split-cut surveillance sweep moves to the lowest sweep that actually carries velocity.
- Product-specific color scales and legends, radial velocity units, readouts with the selected product's units and beam elevation, scan time and age, grid, range rings, station marker, and pointer latitude/longitude/range/azimuth.
- An offline Natural Earth 1:110m vector map with country fills, borders, labels, a graticule, a scale bar, coordinates, and runtime radar-site markers. The map pans by dragging, zooms under the pointer, and selects a radar site by clicking its marker. It has no tile account or network dependency.
- Map-view radar overlays. The selected station and up to three nearby stations load through independent pipelines and render as independent meshes at their true geographic positions, farthest first so the selected site is on top. Each layer has its own fetch, decode, render, and failure state. Layers whose scans differ from the selected station by more than six minutes are withheld and named as time-excluded in the map status, so a stale station cannot be mistaken for a simultaneous observation. Pan and zoom schedule a debounced re-query around the new centre, and the contributing site identifiers are shown on the map. This is a time-bounded station overlay, not a value-composited mosaic.
- A live global radar composite from RainViewer, covering more than 1,200 radars in over 150 countries. It renders at Web Mercator tile bounds, carries ten-minute frames for the previous two hours, and provides a timestamp scrubber and loop. RainViewer attribution is shown on the map. This is a global composite source, not a national Level II feed and not a substitute for the seven NEXRAD moments.
- Active National Weather Service warning polygons for the United States and its territories. The map draws each polygon by severity, reports the active count, names alerts whose geometry is unavailable instead of silently dropping them, and shows the most severe active event, area, urgency, issuance time, and expiration. Malformed alerts are counted as unreadable instead of discarding the whole collection. The layer can be hidden, refreshes every five minutes, and reports feed failures and retries. This is a United States alert feed, not a global warning system.
- Honest failure states for configuration, station discovery, place search, forecast, fetch, decode, and rendering. The last good radar frame stays visible with its age; a failed refresh does not become a current-data indicator.
- Saved town and radar restoration from `~/.oxrad/config.toml`, and `--site KPAH` for an explicit radar start. The town’s forecast point and the radar station are stored as separate identities.

## What is not implemented

- National Level II or Level III adapters outside the United States. The RainViewer composite provides global reflectivity, but there is no DWD, ECCC, BoM, JMA, CMA, IMD, Roshydromet, SMN, MetService, OPERA, or ODIM_H5 decoder yet. The station list excludes TDWR because no TDWR Level II acquisition path exists.
- A value-composited multi-station mosaic. The map viewport overlays the selected station and up to three nearby stations only when their scans fall within six minutes of the selected scan, but it does not resample to one exact time, fill gaps, or resolve overlapping gates into a composite value.
- NEXRAD scan history, looping, and a scrubber. The RainViewer global composite has its own two-hour history and loop, but the Level II path still shows only the newest completed volume. Side-by-side panes, storm tracks, storm motion, local state and city detail beyond the 1:110m country map, distance measurements, and location-permission handling are also absent.
- Warning detail beyond the national feed summary. The warning layer does not yet filter alerts to the viewport, expose issuance and urgency fields, or open a selected alert's full text.
- A first-run location picker. There is no onboarding overlay, and none is planned. The application opens with the saved site when one exists and normal station and place controls in the left rail.
- Persistent caching outside the RainViewer global composite. NEXRAD listings and volumes, forecast responses, place searches, station documents, and warning polygons still rely on live requests or their existing separate caches; HTTP validators and conditional revalidation are not implemented.
- Colorblind-safe palette alternatives and a light theme. The current interface is dark-only.
- The required two independent full-system clean reviews. No separate reviewer process was available in this environment.

Do not use this build for safety decisions. Radar coverage, warnings, and geographic products are incomplete.

## Running

```
cargo run --release
```

Linux needs an X11 or Wayland client library, `libxkbcommon`, and a Vulkan loader and driver for winit and wgpu. `--help` prints the command-line usage. `--site KPAH` starts with that NEXRAD site instead of the saved one.

The application stores configuration and station metadata under `~/.oxrad`. Downloaded RainViewer manifests and tiles are kept in a bounded, integrity-checked cache under `~/.oxrad/cache`, so previously viewed global radar frames can be reopened when their source is unreachable.

## Data sources

| Source | Use | Coverage | Status |
|---|---|---|---|
| Unidata NEXRAD Level II S3 archive | Radar volumes | United States WSR-88D | Implemented |
| RainViewer Weather Maps API | Global reflectivity composite and two-hour history | 1,200+ radars in 150+ countries | Implemented; free personal/community terms, visible attribution |
| NWS `api.weather.gov/radar/stations` | Runtime station discovery | United States WSR-88D and TDWR metadata | Implemented; TDWR excluded from selection |
| NWS `api.weather.gov/alerts/active` | Active warning polygons and metadata | United States and territories | Implemented; severity, area, and expiration shown |
| Open-Meteo forecast API | Current, hourly, daily forecast | Global | Implemented |
| OpenStreetMap Nominatim | Place search | Global | Implemented |

Live station discovery is refreshed every six hours. A failed refresh keeps the cached list and retries after fifteen minutes. A failed discovery with no cache is reported in the station panel.

Requests carry `oxrad/<version> (https://github.com/bonzupii/oxrad)` as the user agent. Place-search text and forecast coordinates are not written to logs.

## Development

`ROADMAP.md` describes the target architecture, implemented state, and remaining work. `STYLE.md` is the code and prose guide. `AGENTS.md` binds changes to the product and engineering constraints. `API.md` records the NEXRAD crate interface. `docs/DEPENDENCIES.md` records direct dependencies and their purpose. `docs/NETWORK.md` records every network path and its limits.

Gates:

```
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo test
cargo build
cargo build --release --locked
```

`cargo test` currently runs 124 tests. Ten use the network because the paths they cover are real services:

- the pinned KPAH archive download, size check, SHA-256 check, and full decode;
- the live KPAH listing and retrieval;
- the missing-volume 404 path;
- live place search;
- live Open-Meteo forecasts for a United States place and a place outside the United States;
- live NWS station discovery and the cache round trip;
- live active-warning parsing;
- the live RainViewer manifest and a real 512-pixel global tile decode.

A network failure fails those tests; they are not replaced with synthetic data. No radar archive is stored in the repository.

Licensed under GPLv3. Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>.
