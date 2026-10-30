# oxrad

Storm-chaser-grade weather radar viewer.

![Historical screenshot](screenshot.png)

The screenshot is historical and does not verify the current build.

oxrad is a desktop weather and radar application for everyday use, storm chasers, emergency managers, and weather spotters. Milestone 1 shows live KPAH (Paducah, KY) Level II base reflectivity with a latitude/longitude grid, 50 km range rings, a dBZ legend, the scan time and its age, and a pointer readout of the gate under the cursor. The maintainer has not yet completed the interactive visual review of this build; do not rely on it for weather or safety decisions.

NEXRAD is the first radar source. [ROADMAP.md](ROADMAP.md) specifies worldwide weather and radar coverage, source catalogs, forecasts, and the milestone plan.

## Running

```
cargo run --release
```

The application takes no arguments. It restores the last site from `~/.oxrad/config.toml`; with no saved site it opens the site picker over an empty map and the pipeline stays idle until a site is chosen. The picker lists every WSR-88D site in the bundled National Weather Service catalog and filters on identifier or place name.

`--site KPAH` overrides the saved site for diagnostics, and `--help` prints the usage.

Linux needs libxkbcommon, an X11 or Wayland client library, and a Vulkan loader with a working driver, as required by winit and wgpu.

## Data source

Volumes come from the public NEXRAD Level II archive at `unidata-nexrad-level2.s3.amazonaws.com`. Requests carry `oxrad/<version>` as their user agent and send nothing else. The volume listing is paginated to exhaustion and ordered by timestamp, and a volume is accepted only when its radials include the volume start and volume end records.

## Tests

```
cargo test
```

Two tests use the network, because the data path they cover is real data. One retrieves the pinned archive documented in [tests/fixtures/README.md](tests/fixtures/README.md) and verifies its size and SHA-256 before decoding it. The other lists and retrieves the current KPAH volume from the live feed. Both fail when the feed is unreachable. No radar archive is stored in this repository.

## Development

[ROADMAP.md](ROADMAP.md) is the architecture and development roadmap. [STYLE.md](STYLE.md) is the code and prose style guide. [AGENTS.md](AGENTS.md) holds the rules that bind changes to this repository.

Both gates must pass:

```
cargo clippy -- -D warnings
cargo test
```

Licensed under GPLv3. Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
