# Dependencies

This is the direct dependency record required by `STYLE.md`. It records what each crate is for and where network access enters the program. A complete transitive `unsafe` census has not been performed; that is an open verification item, not a claim that none exists.

| Crate | Version | Concern | Network I/O | License | Notes |
|---|---|---|---|---|---|
| `bzip2-rs` | 0.1.2 | Archive II bzip2 decompression | No | MIT/Apache-2.0 | Pure Rust decompressor used by `radar.rs` |
| `eframe` | 0.34.1 | Native window, egui UI, wgpu integration | No | MIT/Apache-2.0 | Pulls winit, wgpu, egui, and their transitive dependencies |
| `geographiclib-rs` | 0.2.7 | WGS84 direct and inverse geodesics | No | MIT | Used by `projection.rs`; adapted from GeographicLib |
| `nexrad-decode` | 0.1.2 | NEXRAD Level II message and moment parsing | No | MIT | Confined to `radar.rs`; framing, bzip2, and validation remain oxrad's responsibility |
| `quick-xml` | 0.41.0 | S3 listing XML parsing | No | MIT/Apache-2.0 | Direct dependency; input comes from `network.rs`; parses whitespace, entities, and pagination fields |
| `serde_json` | 1.0 | JSON parsing for station, place, and forecast providers | No | MIT/Apache-2.0 | Provider documents are parsed into oxrad's own types at the adapter boundary |
| `ureq` | 3.5.0 | HTTPS client | Yes | MIT/Apache-2.0 | Confined to `provider.rs` and `network.rs`; timeouts and body limits are set there |

## Development dependency

| Crate | Version | Concern | Network I/O | License |
|---|---|---|---|---|
| `sha2` | 0.11.1 | Pinned-archive SHA-256 verification | No | MIT/Apache-2.0 |

## Dependency boundaries

- `nexrad-decode` types do not cross out of `radar.rs`. The application receives `RadarVolume`, `RadarSweep`, `RadialBins`, and `GateValue`.
- `geographiclib-rs` is used only in `projection.rs`.
- `serde_json` is used at JSON provider boundaries. The station and forecast modules convert into application types immediately.
- `ureq` is used only by `provider.rs` and `network.rs`.
- `eframe` is the window and GPU boundary. `gpu.rs` owns the wgpu pipeline and buffers; `app.rs` owns UI state.

## Not present

- `thiserror` is not used. Error types implement `Display` and `Error` by hand.
- No image library, mapping tile renderer, HDF5 reader, or ODIM decoder is present. The roadmap lists those as planned work.
- No runtime plugin or dynamic decoder loading exists. Additional radar sources require compiled adapters.
