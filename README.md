# oxrad

A free desktop Earth-science workstation in Rust: radar, weather, satellite,
hydrology, seismicity, climate, ocean, air quality, wildfire, space weather and
related open feeds in one map and inspection surface.

oxrad runs on Linux, macOS and Windows through `eframe` and `wgpu`. It has no
account, subscription, advertisement or paid tier. It is licensed under GPLv3.

## Status

This is a pre-release build. The checked behavior, known gaps and the evidence
for each area are recorded in
[docs/RECONSTRUCTION_STATE.md](docs/RECONSTRUCTION_STATE.md). That ledger is the
authority for what is implemented; this README describes how to build and use
the application and does not claim completeness.

Do not use this build for safety decisions. Coverage, warnings and geographic
detail are incomplete.

The implemented data path keeps provenance and freshness visible. A request is
reported as `live`, `cached`, `refreshed`, `stale`, `refreshing` or
`unavailable`; a failed refresh preserves a whole cached body as `stale` rather
than presenting it as current. Invalid or missing measurements are rejected
rather than replaced with invented values.

## Build

```sh
cargo build --release --locked
```

Linux needs an X11 or Wayland client library, `libxkbcommon`, and a Vulkan
loader and driver.

## Run

```sh
cargo run --release
oxrad --site KTWX
```

`--site` starts with that NEXRAD station instead of the saved one. `--help`
prints the available commands, including the source inventory and registry
probes.

The window has a left rail, a map or radar workspace, a right-side forecast and
detail panel, and a status bar. The status bar carries station, product, time,
age, cache state, controls and provenance. The rail exposes place search,
station selection, radar product and tilt, history, warnings and the
environmental layers. Map and radar views share navigation, selection and
provenance.

## Network and cache

All provider requests pass through one acquisition engine. It owns cache
lookup, request pacing, retries, response validation and provenance. Timeouts
are phase and payload aware: there is no fixed total deadline that aborts a
large archive on a slow link.

Cached responses retain `ETag` and `Last-Modified` validators. A stale entry is
revalidated conditionally; a 304 reuses the cached body and refreshes its age
without transferring it again. A 200 replaces the body, digest and validators
through one metadata commit. Bodies and metadata are integrity-checked before
use.

An interrupted binary transfer keeps the bytes already received. When the
origin advertises range support, the retry resumes with a `Range` request and
the assembled body is checked against its declared length and any supplied
SHA-256 digest. An origin without range support is restarted cleanly. Partial
bytes live in memory only; resumption across process restarts is not
implemented.

The reader can cancel an active transfer. Cancellation shuts down the socket
agent carrying that transfer, so a read blocked on a silent origin returns
promptly. A cancelled transfer is distinct from a stalled transfer and from a
failed transport. Cancellation never commits bytes to the cache. See
[docs/NETWORK.md](docs/NETWORK.md) for the exact policies.

The cache is stored under `~/.oxrad/cache/responses`. Its budget is enforced by
`--prune-cache`; it is not pruned after every write. HTTP `Cache-Control`
expiry is not parsed; registry cadence and endpoint overrides own lifetime.

## Data sources

The registry contains only free, public sources that permit display, persistent
caching and attribution. It records coverage, products, formats, cadence,
licence, rate limits and cache terms. The generated inventory is
[docs/SOURCES.md](docs/SOURCES.md); the admission terms and current request
routes are in [docs/NETWORK.md](docs/NETWORK.md).

The core sources include Unidata NEXRAD Level II, the National Weather Service,
Open-Meteo, OpenFreeMap and OpenStreetMap contributors, USGS, NASA GIBS,
RainViewer, Sunrise-Sunset.org, and the public feeds listed by the registry.
Attribution and licence text are shown with the data and in exported
provenance. Provider errors are visible; missing coverage is named rather than
filled with another provider's value.

Requests identify the client as
`oxrad/<version> (https://github.com/bonzupii/oxrad)`. Place queries and
forecast coordinates are not written to logs.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --message-format=short -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

Tests use generated and loopback responses. They do not bundle provider data.
The current test count, the exact checks that ran and the remaining gaps are
recorded in [docs/RECONSTRUCTION_STATE.md](docs/RECONSTRUCTION_STATE.md).
Graphical checks run the release binary under Xvfb; see
[docs/HEADLESS.md](docs/HEADLESS.md).

## Documents

| Document | Contents |
|---|---|
| [docs/RECONSTRUCTION_STATE.md](docs/RECONSTRUCTION_STATE.md) | Authoritative implementation state, tests, gates and gaps |
| [docs/NETWORK.md](docs/NETWORK.md) | Transport, retry, cache, revalidation, resume, cancellation |
| [docs/SOURCES.md](docs/SOURCES.md) | Generated provider inventory |
| [docs/TESTING.md](docs/TESTING.md) | Test layers and coverage gaps |
| [docs/HEADLESS.md](docs/HEADLESS.md) | Graphical smoke procedure and limits |
| [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) | Direct dependencies |
| [STYLE.md](STYLE.md) | Code and prose conventions |
| [AGENTS.md](AGENTS.md) | Engineering constraints |

Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>. GPLv3.
