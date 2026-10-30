# oxrad

Storm-chaser-grade weather radar viewer.

![Historical screenshot](screenshot.png)

The screenshot is historical and does not verify the current build.

oxrad is a desktop weather and radar application for everyday use, storm chasers, emergency managers, and weather spotters. It restores the last radar station, finds the nearest WSR-88D station to a searched United States town, and shows live Level II base reflectivity with a latitude/longitude grid, 50 km range rings, a dBZ legend, the scan time and its age, and a pointer readout of the gate under the cursor. The forecast panel requests the National Weather Service forecast for the town's exact coordinates. Radar and forecast remain separate: the radar station and the forecast point are labeled independently.

Forecasts use the National Weather Service and cover the United States. Places outside that coverage are identified as outside forecast service. This build is a single-station Milestone 1 display, not the full overlay stack. Do not rely on it for weather or safety decisions.

NEXRAD is the first radar source. [ROADMAP.md](ROADMAP.md) specifies worldwide weather and radar coverage, source catalogs, forecasts, and the milestone plan.

## Running

```
cargo run --release
```

The application restores the last radar station from `~/.oxrad/config.toml`. A town search sets the forecast location for the session. With no saved station it opens the station list in the left rail and the pipeline stays idle until a station is chosen. The list contains every WSR-88D site in the bundled National Weather Service catalog and filters on identifier or place name. The search field submits a town or place query to Nominatim; the result list shows each place and its nearest radar station.

`--site KPAH` overrides the saved station, and `--help` prints the usage.

Linux needs libxkbcommon, an X11 or Wayland client library, and a Vulkan loader with a working driver, as required by winit and wgpu.

## Data source

Volumes come from the public NEXRAD Level II archive at `unidata-nexrad-level2.s3.amazonaws.com`. Requests carry `oxrad/<version>` as their user agent and send nothing else. The volume listing is paginated to exhaustion and ordered by timestamp, and a volume is accepted only when its radials include the volume start and volume end records.

Town search uses Nominatim at `nominatim.openstreetmap.org` with an identifying `oxrad/<version>` user agent. Searches are limited to one request per second, only the current request is allowed in flight, and successful results are cached for the session. Search results are © OpenStreetMap contributors. Forecast requests go to `api.weather.gov/points/{lat},{lon}`, then to the grid forecast URL that response supplies. Query text and forecast coordinates are not written to the error log.

## Tests

```
cargo test
```

Five tests use the network, because the paths they cover use real services. One retrieves the pinned archive documented in [tests/fixtures/README.md](tests/fixtures/README.md) and verifies its size and SHA-256 before decoding it. One lists and retrieves the current KPAH volume from the live feed. One confirms that a missing volume is reported as a stale listing. One resolves a town through the live Nominatim service. One requests the live National Weather Service forecast for a United States point. Each fails when its service is unreachable. No radar archive is stored in this repository.

## Development

[ROADMAP.md](ROADMAP.md) is the architecture and development roadmap. [STYLE.md](STYLE.md) is the code and prose style guide. [AGENTS.md](AGENTS.md) holds the rules that bind changes to this repository.

Both gates must pass:

```
cargo clippy -- -D warnings
cargo test
```

Licensed under GPLv3. Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
