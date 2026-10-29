# oxrad

Storm-chaser-grade weather radar viewer.

![screenshot](screenshot.png)

oxrad renders weather radar for storm chasers, emergency managers, and weather spotters. It currently fetches the latest Level II volume for KPAH (Paducah, KY) from the AWS public data feed and draws base reflectivity with the standard NWS color scale, a latitude/longitude grid, and 50 km range rings. Data and network errors appear in the status bar.

NEXRAD is the first source. [ROADMAP.md](ROADMAP.md) covers the source catalog, additional products, and the milestone plan.

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
