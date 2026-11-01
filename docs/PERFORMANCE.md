# Performance

## Position

No performance measurement has been taken for this application. Every number in this document is a target, or a property of the code read from source. Nothing here is a result.

The interface is reported as severely degraded. That report is treated as a defect until measurement shows otherwise.

## Targets

| Metric | Target | Workload |
|---|---|---|
| Cold start to first painted frame | under 2 s | Release build, warm OS page cache |
| Warm start | under 800 ms | Configuration and station cache present |
| First radar frame, warm cache | under 500 ms | One volume, reflectivity |
| First radar frame, cold fetch | under 4 s | One volume over a 20 Mbit link |
| Pan and zoom | 60 fps sustained | 2560x1440, four radar layers plus warnings |
| Playback | 10 fps minimum | Eight-frame history loop |
| Idle CPU | under 2 percent | Static view, nothing animating |
| Resident memory | under 1.5 GB | Four radar layers, one hour of use |
| Search to first result | under 400 ms | Typing, debounced |

## Known costs from source

| Cost | Location | Reason |
|---|---|---|
| Whole-body buffering | `network.rs` | NEXRAD volumes are read into memory before decode; limit is 64 MiB |
| Volume decode in one worker | `pipeline.rs` | One decode blocks the next frame for that site |
| Per-frame texture upload | `global_radar.rs` | Each selected frame uploads its tiles |
| Mesh rebuild on product or tilt change | `display.rs` | Geometry is rebuilt rather than reinterpreted |
| Repaint scheduling | `app.rs` | Repaint intervals are requested during playback and polling |
| Basemap geometry drawn every frame | `map.rs` | Country rings are triangulated once and drawn as shapes |

These are read from the code. Their cost is not measured.

## Measurement method

1. Build release with symbols: `cargo build --release`.
2. Record wall time from process start to first frame using a timestamp printed at first paint.
3. Sample CPU and resident memory with `pidstat` and `ps` at one-second intervals during a scripted interaction.
4. Record frame intervals with an in-application histogram exposed in expert diagnostics.
5. Capture network byte counts per source over one session.
6. Record cache hit and miss counts per class.

Tools required: `pidstat`, `perf`, and a display server. None of these has been installed in this environment yet.

## Workload

A measurement names its workload. The standard workload is:

- KTWX, reflectivity, lowest tilt, live.
- Three nearby stations in map view.
- NWS warnings on.
- Global composite on at regional zoom.
- Ten pan and zoom operations across two zoom levels.
- One history playback cycle of eight frames.
- Search for one place and select it.

A single-layer empty-map measurement is not a valid baseline for interaction budgets.

## Regression policy

A change that makes a measured metric slower by more than 10 percent requires either a fix or a recorded justification with the new number. A change that disables work, lowers fidelity, or drops frames is not a performance improvement.

## Reporting

Each measurement records: build hash, hardware, display resolution, workload, metric, and result. A claim without those fields is unproven and is labelled as such.
