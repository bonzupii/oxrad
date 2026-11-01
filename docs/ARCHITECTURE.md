# Architecture

## Current structure

The binary is one crate. `main.rs` parses arguments and constructs the application. Modules own one concern each.

| Module | Lines | Responsibility |
|---|---|---|
| `app` | 4254 | Window, panels, interaction state, all workflow orchestration |
| `radar` | 1915 | Archive II framing, Type 31 decode, volume and sweep assembly |
| `render` | 933 | Gate corner geometry, overlay geometry, screen transform |
| `global_radar` | 869 | RainViewer manifest, tiles, playback |
| `error` | 808 | Application error enums |
| `map` | 718 | Natural Earth basemap, projections, station markers |
| `place` | 687 | Place identity, search, nearest station |
| `pipeline` | 636 | Fetch and decode workers for one site |
| `forecast` | 627 | Open-Meteo forecast |
| `stations` | 624 | NWS station discovery and cache |
| `warnings` | 533 | NWS active alerts |
| `cache` | 508 | SHA-256 verified disk cache |
| `projection` | 468 | WGS84 geodesics, slant range, Mercator |
| `network` | 451 | NEXRAD S3 listing and retrieval |
| `config` | 359 | `~/.oxrad/config.toml` |
| `retry` | 327 | Backoff and `Retry-After` |
| `colormap` | 308 | Product palettes |
| `history` | 307 | Recent-volume listing and fetch |
| `gpu` | 268 | wgpu pipeline and buffers |
| `display` | 257 | Displayed frame and geometry worker |
| `provider` | 253 | HTTPS JSON access |
| `test_support` | 172 | Test error type |
| `product` | 137 | Product identity and units |
| `overlay` | 134 | Grid, rings, legend drawing |
| `time` | 112 | Timestamp formatting |
| `main` | 107 | Argument parsing, window construction |
| `paths` | 56 | File locations |

## Ownership

One UI thread owns all display state. Workers own their inputs, produce an owned result, and send it through `std::sync::mpsc`. No shared mutable state exists.

```
UI thread (OxradApp)
  StationFeed ....... discovery worker  -> owned Station list
  PlaceSearch ....... search worker     -> owned Place list
  ForecastFeed ...... forecast worker   -> owned Forecast
  Pipeline .......... fetch worker      -> owned RadarVolume
                      decode worker
  WarningFeed ....... alert worker      -> owned Warning list
  GlobalRadarFeed ... manifest and tile workers
  Display ........... geometry worker   -> owned RadarGeometry
  HistoryFeed ....... listing and volume workers
```

Supersession uses generation counters. A result whose generation no longer matches is discarded. A dropped pipeline closes its retry channel.

## Data flow for one radar frame

```
S3 listing -> newest complete key
           -> cache lookup -> verified body or live GET
           -> bzip2 record framing
           -> Type 31 radials -> complete sweeps -> RadarVolume
           -> geometry worker: WGS84 gate corners in site-relative km
           -> wgpu mesh upload
           -> UI draw with the product palette
```

Provenance travels with the volume. `RetrievedVolume` carries `ArchiveOrigin` and, for history frames, the archive key, so a label always describes the frame on screen rather than the last request.

## Measured structural defects

These are defects against `STYLE.md`, not preferences.

| Defect | Evidence | Consequence |
|---|---|---|
| `app.rs` is a god module | 4254 lines; `OxradApp` holds 60 fields; 30 methods on one impl | Every feature interacts with every other; no unit can be tested alone |
| No layer abstraction | Warnings, global radar, and viewport radars each carry their own visibility, state, and draw path | A fourth data type needs a fourth parallel implementation |
| No provider abstraction | NEXRAD, RainViewer, NWS alerts, Open-Meteo, and Nominatim each reach the UI through separate ad-hoc state | No international source can be added without new UI branches |
| No unit model | Units arrive as provider strings; radar uses product units | No conversion, no consistency, no preference |
| No design tokens | Colours are module constants in `app.rs`; sizes are literals at call sites | No theme, no scale, no consistency |
| No time model | Each subsystem tracks its own timestamps | Layers cannot be aligned to one observation time |
| Rendering assumes one mesh per scene | `gpu.rs` draws one mesh | Composition requires a new renderer |

The first two rows are the architectural cause of most reported interface defects.

## Target architecture

### Core concepts

| Concept | Owns |
|---|---|
| `Workspace` | Map view, selected place, selected station, active layers, time cursor |
| `Layer` | Visibility, opacity, order, source, time, legend, availability, failure |
| `Source` | Discovery, products, coverage, cadence, licence, retrieval |
| `Product` | Identity, units, range, palette, thresholds |
| `Preferences` | Units, theme, level, precision, locale |
| `Provenance` | Source, retrieval time, observation time, cache origin, licence |

### Layer model

```
LayerId -> LayerSpec { source, kind, time policy, render policy }
        -> LayerState { availability, freshness, failure, last value }
        -> LayerRender { draw order, opacity, textures or meshes }
```

Kinds are a closed enum: `RadarVolume`, `RadarComposite`, `Alerts`, `ForecastOverlay`, `Satellite`, `PointObservations`, `Basemap`, `Geometry`. A new kind is an enum variant plus a renderer, not a new panel.

Ordering, opacity, visibility, and time policy live in one place. Panels read that state; they do not own copies.

### Source boundary

Traits are permitted here because independent implementations are required by the product.

```rust
trait RadarSource {
    fn coverage(&self) -> Coverage;
    fn products(&self) -> &[ProductCapability];
    fn list_recent(&self, site: &SiteRef) -> Result<Vec<ScanRef>, SourceError>;
    fn fetch(&self, scan: &ScanRef) -> Result<RetrievedVolume, SourceError>;
}

trait TileSource { .. }
trait AlertSource { .. }
trait PointSource { .. }
trait GeocodingSource { .. }
```

Each trait has at least two implementations before it is introduced. A single implementation stays a concrete type. Source-specific types stay inside the adapter module, as `nexrad-decode` types do today.

Selection uses `Box<dyn RadarSource>` held by the workspace. Ownership stays single; no `Arc`.

### Geographic foundation

One projection module serves the basemap, radar geometry, warnings, and overlays. Map view state is a centre and a zoom in the shared Web Mercator model. Radar geometry is generated in site-relative kilometres and transformed by the same view.

Tile and vector data enter through `TileSource`. Basemap detail is level-of-detail driven, so street-scale content is requested only when the viewport needs it.

### Time

`TimeCursor` holds the workspace observation time. A layer declares its tolerance. A layer beyond tolerance is drawn as stale with its age, or withheld and named, never silently included.

### Units

`Measurement` carries a canonical value and a quantity kind. Formatting converts at the presentation boundary using `Preferences`. Stored values never change.

### Preferences and persistence

`~/.oxrad/config.toml` holds station, place, product, level, units, theme, panel sizes, and layer configuration. Unknown keys are reported, not ignored.

### Rendering

The renderer accepts a list of meshes and textured layers with explicit draw order. Each mesh keeps its own uniform buffer. Layer uploads are incremental: a layer whose data is unchanged is not re-uploaded.

## Migration order

1. Extract workspace state and panel state from `OxradApp` into `workspace.rs`, `sidebar.rs`, and `status_bar.rs`. Behavior unchanged.
2. Introduce `Preferences` with theme and units, and route existing strings through it.
3. Introduce the `Layer` model and convert warnings, global radar, and viewport radars to it.
4. Introduce `RadarSource` and move NEXRAD behind it. Add DWD as the second implementation.
5. Replace the single-mesh renderer with ordered layer drawing.
6. Add the basemap level-of-detail path.

Each step keeps the binary working and the test suite passing. A step is committed only after its behavior is observed.

## Boundaries preserved

Safe Rust only. No shared ownership. No interior mutability. Worker channels remain the only cross-thread mechanism. Every fallible call is handled. Errors reach the interface.
