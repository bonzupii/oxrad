# oxrad roadmap

[STYLE.md](STYLE.md) governs code and prose. The authoritative statement of what
is implemented, tested and still missing is
[docs/RECONSTRUCTION_STATE.md](docs/RECONSTRUCTION_STATE.md). This file names
the remaining work; it does not mark a phase complete.

## Product direction

One desktop workstation for weather, radar and environmental observation:
global where open data exists, usable by ordinary people, storm spotters,
chasers, meteorologists and scientists. Free, without an account or paid tier.

Every displayed layer keeps its source, station or grid identity, units, time,
quality, freshness, provenance and attribution. A failure is visible. A
missing value stays missing. A stale value is labeled stale.

## Remaining work

### Shared workstation

- Complete the layer model: groups, ordering, opacity, style and legends.
- Finish linked panes and shared selection across map, radar, profile, chart
  and section inspectors.
- Extend timeline and animation controls across the layers that carry time.
- Complete the provider and cache manager, including budget controls.
- Persist themes, units, projections, layouts, layers, providers, cache policy
  and radar settings as one typed configuration.

### Radar

- Add TDWR and the international adapters: DWD, OPERA and ODIM_H5.
- Complete quality control, gate resampling, cross-sections, storm analysis and
  mosaicking.
- Verify the remaining radar products and geometry against independent
  reference values.

### Atmosphere and environment

- Add soundings, hodographs, model fields and warnings beyond the current
  coverage.
- Complete satellite channels, georeferencing and animation.
- Complete hydrology, seismology, climate, ocean and cryosphere workflows.
- Complete air quality, wildfire, snow, volcano, cyclone, lightning and space
  weather inspectors.

### Engineering

- Make resumable partial bodies durable across process restarts.
- Parse provider cache expiry where the provider supplies it.
- Enforce the cache budget after writes, not only when pruning is requested.
- Add reference tests for every scientific transform and decoder boundary not
  yet covered.
- Measure rendering and pan/zoom performance against a recorded workload.
- Inspect the full graphical state matrix, including cancellation, refreshing,
  stale, unavailable and error states.

## Non-negotiable behavior

- Every visible control has a state transition and a visible failure state.
- A late result for a superseded place, site, product or generation is
  discarded.
- A failed refresh preserves last-known-good data with its age and failure.
- A layer beyond its time tolerance is withheld and named.
- Below-threshold, range-folded and measured radar gates remain distinct.
- No setup wall: a saved site opens after discovery, without an ICAO code.
- No fabricated coordinates, measurements, timestamps or units.

## Verification

The gates are recorded in [docs/TESTING.md](docs/TESTING.md) and the current
results in [docs/RECONSTRUCTION_STATE.md](docs/RECONSTRUCTION_STATE.md):

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --message-format=short -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

A feature is complete only when its behavior runs in the binary, its failure
paths are tested, and its interface states have been inspected.
