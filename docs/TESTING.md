# Testing

## Current suite

The last recorded run of `cargo test --all-targets` was 544 passed, 0 failed.
The authoritative result, commit and gate list are in
[RECONSTRUCTION_STATE.md](RECONSTRUCTION_STATE.md).

Provider tests build controlled responses in code. Cache and transfer tests use
temporary directories and loopback TCP servers. No provider data is bundled,
recorded or replayed.

| Area | Coverage |
|---|---|
| Projection | Direct and inverse geodesics against the reference table, eight azimuths at 75 km, antimeridian, polar, Mercator limits and rejection paths |
| Radar decode | Archive framing, Type 31 sizes, moment sentinels, missing radials, seam width, azimuth bisection and truncation |
| Render | Gate corners, rings, pointer lookup, threshold classification and north orientation |
| Map | Pan, pointer-anchored zoom, vector-tile decoding, addressing, range rings, graticule and tile failure states |
| Pipeline | Failure context, cache provenance ownership and retry channel closure |
| Display | Frame provenance, retry and rebuild |
| Cache | Digest and length verification, atomic replacement, validators, schema rejection, entry and byte budgets, oldest-first pruning, truncation and replacement |
| Network | Conditional requests, 304 without a second body, 200 validator replacement, range resume with checksum, no-range restart, stalled reads, prompt cancellation, cancellation without a cache entry, listing parsing and URL encoding |
| Stations | Generated NWS-document parsing, malformed documents and cache persistence |
| Place | Identity normalization, ambiguity and generated search responses |
| Forecast | Request shape, units and generated forecasts for national and international points |
| Warnings | Generated polygons and multipolygons, malformed-alert isolation |
| Global radar | Manifest validation and tile-grid geometry |
| History | Key parsing, playback advance and wrap |
| Retry | Backoff, `Retry-After` seconds and HTTP date, decode-failure policy |
| App | Warning selection retention, forecast retry, viewport station selection and map status bounds |
| Units | Metric and imperial conversion, provider temperature scale, configuration round trip and rejection |

The transport tests are the current evidence for the network requirements:
`engine::tests::a_304_revalidation_returns_the_cached_body_without_a_second_transfer`,
`engine::tests::a_dropped_binary_body_resumes_with_a_range_and_verifies_the_whole_file`,
`engine::tests::an_origin_without_range_support_restarts_cleanly`,
`engine::tests::cancelling_a_stalled_body_interrupts_the_read_and_leaves_no_cache_entry`
and `engine::tests::a_silent_body_is_stalled_not_cancelled_or_failed`.

## Gaps

| Gap | Consequence |
|---|---|
| No GUI assertions for cancellation or refreshing | The controls compile and their logic is tested, but the displayed transitions were not observed in a captured frame |
| No performance test | No rendering or transfer budget is enforced |
| No accessibility test | Contrast, focus and keyboard reach regressions are invisible |
| No cross-process resume test | Partial bodies do not survive a process restart |
| No HTTP `Cache-Control` test | Provider header expiry is not parsed |
| No automatic post-write prune test | The cache budget is enforced only by `--prune-cache` |
| No provider adapter test for the international radar formats | Those adapters are not implemented |
| Incomplete unit coverage | Distance and temperature convert; other quantities still use provider units |
| No golden-image comparison | Palette and legend changes are unnoticed |

## Layers

| Layer | What it proves | Runs in |
|---|---|---|
| Unit | Pure functions and decoders against reference values | Every change |
| Provider integration | A controlled provider response parses into application types | Every change |
| Loopback transport | Revalidation, range, integrity, stall and cancel behavior against real sockets | Every change |
| Rendering | Geometry and projection produce expected coordinates | Every change |
| GUI | States render at fixed window sizes | Release candidate |
| Performance | Budgets hold under a stated workload | Release candidate |
| Accessibility | Contrast, focus and keyboard reach | Release candidate |

## Rules

- A test asserts specified behavior, not execution.
- Error tests match the variant and its carried value.
- A failing test is fixed by correcting the implementation against the
  specification, or by correcting a wrong expectation with evidence.
- A test is never deleted, skipped or weakened to obtain a green run.
- A new defect with a deterministic trigger receives a regression test.
- A green suite is necessary and not sufficient; running behavior is checked
  separately.

## GUI verification

The graphical procedure is in [HEADLESS.md](HEADLESS.md). It runs the release
binary against Xvfb with Mesa lavapipe and captures the real framebuffer. A
screenshot proves what the application drew, not that the data is correct.

`scripts/gui.sh <scenario>` runs the launch, input, capture and non-empty-frame
checks. Current scenarios include radar, map, measure, pan, zoom, small,
registry and the environmental layers. The cancellation and refreshing states
are attempted separately; see [RECONSTRUCTION_STATE.md](RECONSTRUCTION_STATE.md)
for whether a frame was captured.

## Adding a source

A new provider adds request-construction and parser tests with controlled
responses generated in code. Provider availability is verified separately from
the deterministic suite. The registry entry records coverage, format, cadence,
licence, rate limit and cache permission before the adapter is admitted.
