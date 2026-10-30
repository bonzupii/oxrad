# oxrad

Storm-chaser-grade weather radar viewer.

![Historical screenshot](screenshot.png)

The screenshot is historical and does not verify the current build.

oxrad is a desktop weather and radar application for everyday use, storm chasers, emergency managers, and weather spotters. Milestone 1 targets live KPAH (Paducah, KY) Level II base reflectivity with a latitude/longitude grid, 50 km range rings, a legend, scan time, and pointer readout. The current display and live-data path have not completed runtime acceptance; do not rely on this build for weather or safety decisions.

NEXRAD is the first radar source. [ROADMAP.md](ROADMAP.md) specifies worldwide weather and radar coverage, source catalogs, forecasts, and the milestone plan.

## Building

```
cargo build --release
cargo run --release
```

Linux needs libxkbcommon, an X11 or Wayland client library, and a Vulkan loader with a working driver, as required by winit and wgpu.

## Development

[ROADMAP.md](ROADMAP.md) is the architecture and development roadmap. [STYLE.md](STYLE.md) is the code and prose style guide. [AGENTS.md](AGENTS.md) holds the rules that bind changes to this repository.

Both gates must pass:

```
cargo clippy -- -D warnings
cargo test
```

Licensed under GPLv3. Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
