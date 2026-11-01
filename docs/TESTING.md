# Testing

## Current suite

131 tests run under `cargo test`. Last recorded run in this repository: 131 passed, 0 failed.

| Area | Coverage |
|---|---|
| Projection | Direct and inverse geodesics against the roadmap reference table, eight azimuths at 75 km, antimeridian, polar, Mercator limits, rejection paths |
| Radar decode | Archive framing, Type 31 sizes, moment sentinels, missing radials, seam width, azimuth bisection, truncation |
| Render | Gate corner placement, ring geometry, pointer lookup, threshold classification, north orientation |
| Map | Pan, pointer-anchored zoom, triangulation, country labels, embedded dataset |
| Pipeline | Failure context, cache provenance ownership, retry channel closure |
| Display | Frame provenance, retry, rebuild |
| Cache | Round trip, digest mismatch, eviction order |
| Network | Listing parsing, malformed XML, 404 handling, live listing and retrieval |
| Stations | Parsing, malformed documents, cache fallback, live discovery |
| Place | Identity normalization, ambiguity, live search |
| Forecast | Request shape, unit strings, live forecasts inside and outside the United States |
| Warnings | Polygon parsing, multipolygon, malformed alert isolation, live feed |
| Global radar | Manifest validation, tile grid, live manifest and tile decode |
| History | Key parsing, playback advance and wrap |
| Retry | Backoff, `Retry-After` seconds and HTTP date, decode-failure policy |
| App | Warning selection retention, forecast retry, viewport station selection, map status bounds |

Several tests reach live services. A network failure fails the test. They are not replaced with fixtures.

## Gaps

| Gap | Consequence |
|---|---|
| No GUI assertions | Layout regressions are found only by inspecting screenshots |
| No performance test | No budget is enforced |
| No accessibility test | Contrast and focus regressions are invisible |
| No cache TTL or corruption-recovery test | New cache behavior is unverified |
| No provider adapter test for a second source | The provider boundary is unexercised |
| No layer composition test | Ordering and opacity are unverified |
| No unit conversion test | There is no unit system to test |
| No golden-image comparison | Palette and legend changes are unnoticed |

## Layers

| Layer | What it proves | Runs in |
|---|---|---|
| Unit | Pure functions and decode logic against reference values | Every change |
| Provider integration | A real payload parses into application types | Every change, network required |
| Rendering | Geometry and projection produce expected coordinates | Every change |
| GUI | States render correctly at fixed window sizes | Release candidate |
| Performance | Budgets hold under a stated workload | Release candidate |
| Accessibility | Contrast, focus, and keyboard reach | Release candidate |

## Rules

- A test asserts specified behavior, not execution.
- Error tests match the variant and its carried value.
- A failing test is fixed by correcting the implementation against the specification, or by correcting a wrong expectation with evidence.
- A test is never deleted, skipped, or weakened to obtain a green run.
- A new defect with a deterministic trigger receives a regression test.

## GUI verification

The headless workflow is recorded in [HEADLESS.md](HEADLESS.md). It runs the release binary against Xvfb with Mesa lavapipe and captures the real framebuffer. A screenshot proves what the application drew. It does not prove correctness of the data.

The state matrix in [UI_DESIGN.md](UI_DESIGN.md) defines what must be inspected. Each capture is inspected, defects are recorded, fixed, and recaptured.

Current GUI evidence covers the radar scene and the map scene at 1280x800, and the map at 1040x640. It does not cover light theme, collapsed panels, street-scale map, multi-layer composition, search interaction, or the empty, loading, and error states.

## Adding a source

A new provider adds a live integration test that fails when the source breaks, plus parser tests built from a captured payload. Captured payloads live in tests as text, never as binary blobs in the repository.
