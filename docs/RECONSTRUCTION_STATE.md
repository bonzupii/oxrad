# Reconstruction state

Campaign ledger for the reconstruction mission. One row per work item and per acceptance criterion. Read at the start of every pass; update before the pass ends.

States: not started, in progress, verified, blocked.

## Work items

| # | Work item | State | Evidence | Next action |
|---|---|---|---|---|
| 1 | Remove bundled datasets and prohibited assets | verified | Pass 1: no tracked file carries a dataset extension; no `include_bytes!` or `include_str!`; no `assets/`, `fixtures/`, or `data/` directory; no `build.rs` and no `include`/`exclude` keys in `Cargo.toml`; `src/basemap.rs` acquires OpenFreeMap tiles at runtime | none |
| 2 | Configuration, profiles, provider registry, cache, attribution foundations | in progress | Pass 2: `src/sources.rs` records 25 sources with coverage, licence, credit, cadence, cache permission and verification level; 17 pass the admission test, 8 are rejected until their terms are verified; `oxrad --sources` renders `docs/SOURCES.md`; RainViewer and Sunrise-Sunset credits now come from the registry. Configuration profiles, the settings surface and the provider manager are not built | Add profiles and a settings surface that reads this registry |
| 3 | OSM-derived basemap acquisition, styling, multi-scale cartography, persistent tile cache | in progress | `src/basemap.rs` and `src/basemap_tiles.rs` fetch and cache at runtime; multi-scale cartography and eviction unverified | Audit label density, zoom hierarchy, cache budget |
| 4 | Design system, shell, navigation, icons, themes, accessibility, layer manager | not started | | Audit `theme.rs`, `app.rs`, `layer_catalog.rs` |
| 5 | Separate Map and Radar workspaces | not started | | Map and Radar share `app.rs` presentation state |
| 6 | Radar gate geometry and scientific resampling | not started | | Audit `render.rs` topology and the sampling path |
| 7 | Radar source expansion (Level III, TDWR, international), mosaics | not started | | NEXRAD Level II only |
| 8 | Satellite, atmosphere, hydrology, seismic, climate, ocean, environment, space weather workspaces | not started | | Ten workspaces required; one shell exists |
| 9 | Weather-radio audio streaming | not started | | `src/radio.rs` produces text bulletins |
| 10 | Cross-workspace time, location, provenance, provider controls | not started | | Each subsystem tracks its own timestamps |
| 11 | GUI and scientific validation | blocked | `scripts/gui.sh` present; `.tools/root` absent, so no display, no window capture | Provision the harness under `.tools`, pinned |
| 12 | Documentation, including removal of environment-specific workflow | not started | `docs/HEADLESS.md` still carries the Xvfb procedure and an install instruction | Move it to the `headless-gui-testing` skill; make `AGENTS.md` instrument-neutral |

## Acceptance criteria

| # | Criterion (section 24) | State | Evidence |
|---|---|---|---|
| A1 | No datasets bundled in the repository or application | verified | see work item 1 |
| A2 | No prohibited dataset dependency in source, tests, packaging, or generated assets | verified | see work item 1 |
| A3 | External data acquired at runtime and cached per user under explicit policies | in progress | `cache.rs` and `basemap_tiles.rs` cache; per-provider policy unverified |
| A4 | OSM-derived tiles retrieved, rendered, attributed, cached, reused across restarts | not started | runtime acquisition exists; restart reuse unverified |
| A5 | Map supports independent visual styles and multi-scale cartography | not started | five style tokens exist in `basemap.rs` |
| A6 | Map and Radar are distinct workspaces | not started | |
| A7 | Layer manager controls real spatial and scientific visualizations | not started | |
| A8 | Configuration covers workspaces, providers, cache, themes, layers, processing, accessibility, profiles | not started | |
| A9 | Consistent professional design system and iconography | not started | |
| A10 | Dark, light, high contrast, and color-vision modes throughout | not started | |
| A11 | Radar gate geometry preserves measured footprints and angular gaps | not started | |
| A12 | Radar sampling and smoothing use scientifically defined methods | not started | |
| A13 | Multi-radar mosaics spatially coherent, quality-controlled, configurable, traceable | not started | |
| A14 | NEXRAD Level II, Level III, TDWR, and international sources have distinct paths | not started | |
| A15 | Every scientific domain has a complete data-to-visualization workflow | not started | |
| A16 | Weather radio streams actual online audio | not started | |
| A17 | Sunrise and sunset integrated with linked attribution and nullable events | not started | |
| A18 | Provider attribution and licensing systematic and visible | in progress | 25 sources carry licence and credit; 17 admit; the RainViewer and Sunrise-Sunset credits are read from the registry; basemap credit still comes from the OpenFreeMap style document; 15 admitted sources show no credit in the interface yet | Draw every credit from the registry and display it beside its layer |
| A19 | Persistent caches survive restarts and expose their state | not started | |
| A20 | Network behavior provider-aware and robust | not started | |
| A21 | Subsystems own state and lifecycle; root is not a god object | not started | `app.rs` is 4681 lines |
| A22 | Scientific calculations and transformations have meaningful automated tests | not started | 467 tests recorded in `docs/TESTING.md`; coverage unverified |
| A23 | Error paths, malformed data, missing metadata handled deliberately and visibly | not started | |
| A24 | GUI workflows run and visually inspected | blocked | no display; `.tools/root` absent |
| A25 | Settings and acquired data persist across restarts | not started | |
| A26 | Documentation reflects actual behavior, no stale AI document as authority | in progress | `docs/SOURCES.md` is generated from the registry and a test fails when it drifts; `docs/DATA_SOURCES.md` is marked a lead list; the remaining AI documents are unreconciled | Replace or delete each remaining document |
| A27 | Implementation committed and pushed to the managed development branch | verified | `238b1fb` and `849e2d3` pushed to `freebuff/9adade3717017f6ce0f6c608` |

## Pass log

### Pass 1

- Verified work item 1 by inspection: tracked file extensions, `include_bytes!`/`include_str!`, asset directories, packaging keys, embedded geometry constants. The only large constants are colormap ramps in `colormap.rs`, pressure levels in `sounding.rs`, and ring steps in `map.rs`, all algorithmic rather than external data. Nothing was removed because nothing prohibited is present.
- Baseline gates: `cargo check --all-targets` green in 22.57s, `cargo fmt --all -- --check` green, `cargo test --all-targets` 467 passed and 0 failed, `cargo build` green.
- `cargo clippy -- -D warnings` failed at HEAD on three `map(...).unwrap_or_default()` sites in `src/gauge.rs` and `src/radar.rs`. Replaced with `map_or_default`, which returns the same value without the defaulting call STYLE.md prohibits. The gate exits 0.
- Recorded gate gap: `cargo clippy --all-targets` reports 280 errors, all `panic_in_result_fn` from `assert_eq!` inside tests that return `Result`. The documented gate excludes test targets, so they never block a run. Next action: choose between tests that return `Result` with assertions and tests that do not return `Result`, then extend the gate to test targets.
- Release build: `cargo build --release --locked` green in 120s, binary at `target/release/oxrad`.
- Committed `238b1fb` and `849e2d3` and pushed to `freebuff/9adade3717017f6ce0f6c608`. The `dev` branch was not modified.

### Pass 2

- Built the source registry as the first foundation because every domain draws endpoints, coverage, credit and cache policy from it. `src/sources.rs` holds 25 providers as constants: id, name, domain, coverage, home, endpoint, format, licence, credit line, refresh cadence, cache permission and verification level. No provider is selected by string at a call site.
- Admission is structural, not documented. `admits` rejects a non-HTTPS endpoint, a credential marker in the URL, a missing licence, a missing credit line, empty coverage and an unstated cache permission. Tests prove each rejection and prove that every admitting source has reached at least `parsed`.
- 17 of 25 sources admit. The 8 that do not are German, Canadian, Australian, European and global candidates whose terms are unverified; their licence is `None`, so they cannot reach the interface by accident. Canada and Norway were probed: `api.weather.gc.ca/collections` and `api.met.no` both answered 200 `application/json` to one identified request on 2026-11-13.
- Quiet failure found in the old attribution: the RainViewer credit and the Sunrise-Sunset link were constants in their modules, so a provider change could not be seen from one place. Both now read the registry entry.
- `oxrad --sources` renders the inventory, `docs/SOURCES.md` is that output, and a test compares the checked-in file against the render, so the documentation cannot drift from the code the way the AI documents did.
- Gates: `cargo fmt --all` clean, `cargo clippy -- -D warnings` exit 0, `cargo test --all-targets` 477 passed and 0 failed, `cargo build --release --locked` exit 0, `cargo check --all-targets` exit 0.
- Not done: configuration profiles, the settings surface and the provider manager. Work item 2 stays in progress.

## Known gate gaps

| Gap | Detail | Next action |
|---|---|---|
| Test targets are not linted | `cargo clippy --all-targets` fails on 280 assertion-in-`Result` errors that the documented gate never reaches | Align `STYLE.md` with one test convention, then lint test targets |
| GUI evidence absent | No display and no `.tools/root`, so no window can be opened or captured | Provision the harness under `.tools` with pinned versions |
| Basemap credit outside the registry | `walkers` supplies the OpenFreeMap attribution from the style document, so it bypasses `src/sources.rs` | Read the credit from the registry in `draw_attribution` |
