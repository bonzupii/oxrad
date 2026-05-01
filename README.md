# oxrad

NEXRAD weather radar viewer.

![screenshot](screenshot.png)

Fetches the latest radar volume from the KPAH (Paducah, KY) site via the AWS public data feed and displays base reflectivity on a map using the standard NWS color scale. The map includes range rings at 50 km intervals and a latitude/longitude grid. Any data or network errors appear as messages in the status bar.

This is early development — site selection, zoom/pan, and additional radar products are not yet implemented. See [ROADMAP.md](ROADMAP.md) for what's planned.

## Building

```
cargo build --release
cargo run --release
```

Licensed under GPLv3. Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
