# Dependencies

This is the direct dependency record required by `STYLE.md`. It records what each crate is for and where network access enters the program. A complete transitive `unsafe` census has not been performed; that is an open verification item, not a claim that none exists.

| Crate | Version | Concern | Network I/O | License | Notes |
|---|---|---|---|---|---|
| `bzip2-rs` | 0.1.2 | Archive II bzip2 decompression | No | MIT/Apache-2.0 | Pure Rust decompressor used by `radar.rs` |
| `eframe` | 0.34.1 | Native window, egui UI, wgpu integration | No | MIT/Apache-2.0 | Pulls winit, wgpu, egui, and their transitive dependencies |
| `geographiclib-rs` | 0.2.7 | WGS84 direct and inverse geodesics | No | MIT | Used by `projection.rs`; adapted from GeographicLib |
| `image` | 0.25.10 | PNG decode for global radar tiles | Yes, through `ureq` | MIT/Apache-2.0 | Direct dependency with only the PNG feature; confined to `global_radar.rs` |
| `nexrad-decode` | 0.1.2 | NEXRAD Level II message and moment parsing | No | MIT | Confined to `radar.rs`; framing, bzip2, and validation remain oxrad's responsibility |
| `quick-xml` | 0.41.0 | S3 listing XML parsing | No | MIT/Apache-2.0 | Direct dependency; input comes from `network.rs`; parses whitespace, entities, and pagination fields |
| `serde_json` | 1.0 | JSON parsing for station, place, forecast, and warning providers; cache metadata | No | MIT/Apache-2.0 | Provider documents are parsed into oxrad's own types at the adapter boundary |
| `sha2` | 0.11.1 | Stable cache keys and body-integrity verification | No | MIT/Apache-2.0 | Direct dependency used by `cache.rs`; also verifies the pinned archive in tests |
| `ureq` | 3.5.0 | HTTPS client | Yes | MIT/Apache-2.0 | Confined to `provider.rs`, `network.rs`, and `global_radar.rs`; timeouts and body limits are set in `provider.rs` or `network.rs` |

## Data assets

`assets/ne_110m_admin_0_countries.geojson` is Natural Earth 1:110m Admin 0 Countries, public domain. It is compiled into the binary with `include_str!` and parsed with `serde_json`; it is not a tile service and has no network or attribution requirement. Its provenance and licence are recorded in `assets/README.md`.

## Dependency boundaries

- `nexrad-decode` types do not cross out of `radar.rs`. The application receives `RadarVolume`, `RadarSweep`, `RadialBins`, and `GateValue`.
- `geographiclib-rs` is used only in `projection.rs`.
- `serde_json` is used at JSON provider boundaries. The station, forecast, and warning modules convert into application types immediately; warning geometry is reduced to validated latitude/longitude rings.
- `sha2` is used by `cache.rs` for URL-derived file names and body digests, and by the pinned-archive acceptance test.
- `ureq` is used by `provider.rs`, `network.rs`, and `global_radar.rs`; the last reuses the bounded NEXRAD HTTP helper.
- `image` is used only to decode RainViewer PNG tiles into RGBA pixels before egui texture upload.
- `eframe` is the window and GPU boundary. `gpu.rs` owns the wgpu pipeline and buffers; `app.rs` owns UI state.

## Not present

- `thiserror` is not used. Error types implement `Display` and `Error` by hand.
- No HDF5 reader or ODIM decoder is present. `image` is present for the RainViewer PNG composite; it is not a mapping engine or a national radar decoder.
- No runtime plugin or dynamic decoder loading exists. Additional radar sources require compiled adapters.
