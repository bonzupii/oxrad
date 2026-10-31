# NEXRAD Rust Crate API Reference

Reference notes for `nexrad-decode` and `nexrad-model`. Check signatures against the versions in `Cargo.lock` before use. Dependency-specific types stay inside the NEXRAD adapter. [ROADMAP.md](ROADMAP.md) specifies application data contracts; [STYLE.md](STYLE.md) specifies error and code conventions.

The locked `nexrad-decode` version does not validate Archive II file framing or completeness. Its [`decode_messages`](https://docs.rs/crate/nexrad-decode/0.1.2/source/src/messages.rs) treats a failed next-header read as the end of the sequence, including a truncated header. The caller must validate record boundaries and decode until the exact end of the decompressed record, reporting an incomplete header as an error.

---

## Crate: `nexrad_decode`

### Module: `nexrad_decode::messages`

#### `Message` (top-level wrapper)

Decoded NEXRAD Level II message (any type).

```rust
pub struct Message { /* ... */ }

impl Message {
    pub fn header(&self) -> &MessageHeader
    pub fn contents(&self) -> &MessageContents
    pub fn into_contents(self) -> MessageContents  // consume, own contents
}
```

#### `MessageHeader`

```rust
pub struct MessageHeader {
    pub segment_size: u16,       // half-words for fixed-length messages; special value for segmented/variable-length
    pub redundant_channel: u8,
    pub message_type: u8,
    pub sequence_number: u16,
    pub date: u16,               // days since 1970-01-01
    pub time: u32,               // milliseconds past midnight GMT
    pub segment_count: u16,
    pub segment_number: u16,
}

impl MessageHeader {
    pub fn segment_size(&self) -> Option<Information>
    pub fn rda_redundant_channel(&self) -> RedundantChannel
    pub fn message_type(&self) -> MessageType
    pub fn date_time(&self) -> Option<DateTime<Utc>>
    pub fn segmented(&self) -> bool
    pub fn segment_count(&self) -> Option<u16>
    pub fn segment_number(&self) -> Option<u16>
    pub fn message_size_bytes(&self) -> u32
    pub fn message_size(&self) -> Information
}
```

#### `MessageType` (enum)

```rust
pub enum MessageType {
    RDAStatusData,       // type 2
    DigitalRadarData,    // type 31  <-- main radar data
    ClutterFilterMap,    // type 15
    VolumeCoveragePattern, // type 5
    Other(u16),
}
```

#### `MessageContents` (enum)

```rust
pub enum MessageContents {
    RDAStatusData(Box<rda_status_data::Message>),
    DigitalRadarData(Box<digital_radar_data::Message>),  // <-- main path
    ClutterFilterMap(Box<clutter_filter_map::Message>),
    VolumeCoveragePattern(Box<volume_coverage_pattern::Message>),
    Other,
}
```

**Pattern to access digital radar data:**

```rust
let msg: Message = /* from the decoder */;
// or via MessageContents:
if let MessageContents::DigitalRadarData(inner) = msg.contents() {
    // inner is &digital_radar_data::Message
}
```

---

### Module: `nexrad_decode::messages::digital_radar_data`

The Message type that contains all radar moment data (reflectivity, velocity, etc.).

#### `Message`

```rust
pub struct Message {
    pub header: digital_radar_data::Header,
    pub volume_data_block: Option<VolumeDataBlock>,
    pub elevation_data_block: Option<ElevationDataBlock>,
    pub radial_data_block: Option<RadialDataBlock>,
    pub reflectivity_data_block: Option<GenericDataBlock>,     // <-- what we want
    pub velocity_data_block: Option<GenericDataBlock>,
    pub spectrum_width_data_block: Option<GenericDataBlock>,
    pub differential_reflectivity_data_block: Option<GenericDataBlock>,
    pub differential_phase_data_block: Option<GenericDataBlock>,
    pub correlation_coefficient_data_block: Option<GenericDataBlock>,
    pub specific_diff_phase_data_block: Option<GenericDataBlock>,
}

impl Message {
    // Convert to common model Radial. Uses reflectivity block if present.
    pub fn radial(&self) -> Result<nexrad_model::data::Radial>
    pub fn into_radial(self) -> Result<nexrad_model::data::Radial>
}
```

#### `Header` (digital radar data header — radial-level metadata)

```rust
pub struct Header {
    pub radar_identifier: [u8; 4],       // ICAO 4-byte identifier
    pub time: u32,                        // Integer4 — ms past midnight GMT
    pub date: u16,                        // Integer2 — days since 1970-01-01
    pub azimuth_number: u16,              // Integer2 — radial number within elevation (up to 720, 0.5° increments)
    pub azimuth_angle: f32,               // Real4 — degrees
    pub compression_indicator: u8,        // Code1 — 0=Uncompressed, 1=BZIP2, 2=ZLIB, 3=Future
    pub spare: u8,
    pub radial_length: u16,               // Integer2 — bytes
    pub azimuth_resolution_spacing: u8,   // Code1 — 1=0.5°, 2=1.0°
    pub radial_status: u8,                // Code1 — 0=ElevationStart, 1=Intermediate, 2=ElevationEnd, 3=VolumeScanStart, 4=VolumeScanEnd, 5=ElevationStartVCPFinal
    pub elevation_number: u8,             // Integer1 — ordinal within volume scan
    pub cut_sector_number: u8,            // Integer1 — 0=continuous surveillance only
    pub elevation_angle: f32,             // Real4 — degrees
    pub radial_spot_blanking_status: u8,  // Code1 — 0=None, 1=Radial, 2=Elevation, 4=Volume
    pub azimuth_indexing_mode: u8,        // ScaledInteger1 — 0=No indexing, 1-100=0.01 to 1.00 degrees
    pub data_block_count: u16,            // Integer2 — 4 to 10
}

impl Header {
    pub fn radar_identifier(&self) -> String
    pub fn date_time(&self) -> Option<DateTime<Utc>>
    pub fn compression_indicator(&self) -> CompressionIndicator
    pub fn azimuth_angle(&self) -> Angle           // uom; raw * 1.0 degree
    pub fn radial_length(&self) -> Information     // uom; bytes
    pub fn elevation_angle(&self) -> Angle         // uom; raw * 1.0 degree
    pub fn azimuth_resolution_spacing(&self) -> Angle  // uom; raw * 0.5 degree
    pub fn radial_status(&self) -> RadialStatus    // always available, not feature-gated
    pub fn radial_spot_blanking_status(&self) -> SpotBlankingStatus
    pub fn azimuth_indexing_mode(&self) -> Option<Angle>  // uom; 0=None, else raw * 0.01 degree
}
```

**RadialStatus values:** 0→ElevationStart, 1→IntermediateRadialData, 2→ElevationEnd, 3→VolumeScanStart, 4→VolumeScanEnd, 5→ElevationStartVCPFinal. Reject unknown codes rather than treating them as code 5. Azimuth resolution spacing is commanded spacing, not a guarantee of actual separation between adjacent measured azimuths.

#### `VolumeDataBlock`

Present only on radials where `radial_status == ElevationStart` (0 or 3).

```rust
pub struct VolumeDataBlock {
    pub data_block_id: DataBlockId,
    pub lrtup: u16,                                    // Integer2 — block size in bytes
    pub major_version_number: u8,                      // Integer1
    pub minor_version_number: u8,                      // Integer1
    pub latitude: f32,                                 // Real4 — degrees
    pub longitude: f32,                                // Real4 — degrees
    pub site_height: i16,                              // SInteger2 — meters above sea level
    pub feedhorn_height: u16,                          // Integer2 — meters above ground
    pub calibration_constant: f32,                     // Real4 — dB
    pub horizontal_shv_tx_power: f32,                  // Real4 — kW
    pub vertical_shv_tx_power: f32,                    // Real4 — kW
    pub system_differential_reflectivity: f32,         // Real4 — dB
    pub initial_system_differential_phase: f32,        // Real4 — degrees
    pub volume_coverage_pattern_number: u16,           // Integer2
    pub processing_status: u16,                        // Integer2 — 0=RxRNoise, 1=CBT
    pub zdr_bias_estimate_weighted_mean: u16,          // Integer2 — dB
    pub spare: [u8; 6],
}

impl VolumeDataBlock {
    pub fn lrtup(&self) -> Information
    pub fn latitude(&self) -> Angle
    pub fn longitude(&self) -> Angle
    pub fn site_height(&self) -> Length
    pub fn feedhorn_height(&self) -> Length
    pub fn horizontal_shv_tx_power(&self) -> Power
    pub fn vertical_shv_tx_power(&self) -> Power
    pub fn initial_system_differential_phase(&self) -> Angle
    pub fn volume_coverage_pattern(&self) -> u16
    pub fn processing_status(&self) -> ProcessingStatus
}
```

**Critical:** `latitude` and `longitude` are raw `f32` degrees — the site coordinates come from here. No uom needed; just cast: `latitude as f64`, `longitude as f64`.

#### `ElevationDataBlock` (digital_radar_data variant)

```rust
pub struct ElevationDataBlock {
    pub data_block_id: DataBlockId,
    pub lrtup: u16,                    // Integer2 — block size in bytes
    pub atmos: i16,                    // ScaledSInteger2 — atmospheric attenuation, dB/km (raw i16, no accessor scale)
    pub calibration_constant: f32,     // Real4 — dB, scaling constant for this elevation
}

impl ElevationDataBlock {
    pub fn lrtup(&self) -> Information
}
```

#### `RadialDataBlock`

```rust
pub struct RadialDataBlock {
    pub data_block_id: DataBlockId,
    pub lrtup: u16,                                    // Integer2 — block size in bytes
    pub unambiguous_range: u16,                        // ScaledInteger2 — km (raw u16, no scale)
    pub horizontal_channel_noise_level: f32,           // Real4 — dBm
    pub vertical_channel_noise_level: f32,             // Real4 — dBm
    pub nyquist_velocity: u16,                         // ScaledInteger2 — raw * 0.01 = m/s
    pub radial_flags: u16,                             // Integer2 — RPG processing flags
    pub horizontal_channel_calibration_constant: f32,  // Real4 — dBZ
    pub vertical_channel_calibration_constant: f32,    // Real4 — dBZ
}

impl RadialDataBlock {
    pub fn lrtup(&self) -> Information
    pub fn unambiguous_range(&self) -> Length
    pub fn nyquist_velocity(&self) -> Velocity
}
```

**nyquist_velocity:** raw `u16` × 0.01 = m/s.

#### `GenericDataBlock`

A moment data block (e.g. reflectivity, velocity). Contains header with gate metadata + raw data.

```rust
pub struct GenericDataBlock {
    pub header: GenericDataBlockHeader,
    pub encoded_data: Vec<u8>,
}

impl GenericDataBlock {
    pub fn encoded_values(&self) -> &[u8]
    
    // Decodes raw values using header.scale/offset into Vec<ScaledMomentValue>.
    // Special values: `BelowThreshold`, `RangeFolded`.
    pub fn decoded_values(&self) -> Vec<ScaledMomentValue>
    
    // Converts to nexrad_model::data::MomentData.
    pub fn moment_data(&self) -> MomentData   // clones underlying data
    pub fn into_moment_data(self) -> MomentData  // no clone
}
```

#### `GenericDataBlockHeader`

**The critical type for gate/range metadata.**

```rust
pub struct GenericDataBlockHeader {
    pub data_block_id: DataBlockId,
    pub reserved: u32,
    pub number_of_data_moment_gates: u16,    // 0 to 1840
    pub data_moment_range: u16,              // Range to CENTER of first gate, in 0.001-scaled km
    pub data_moment_range_sample_interval: u16,  // Gate width in 0.001-scaled km
    pub tover: u16,
    pub snr_threshold: u16,
    pub control_flags: u8,
    pub data_word_size: u8,                  // 8 or 16 bits
    pub scale: f32,
    pub offset: f32,
}

impl GenericDataBlockHeader {
    // Returns uom Length. To get km: length.get::<uom::si::f64::Kilometer>()
    pub fn data_moment_range(&self) -> Length
    pub fn data_moment_range_sample_interval(&self) -> Length
    pub fn control_flags(&self) -> ControlFlags
    pub fn moment_size(&self) -> Information
}
```

**Important:**
- `data_moment_range` is range to **center** of first gate, in km × 1000.
  - To get km: `data_moment_range as f64 / 1000.0`
  - For gate `i` (0-indexed): `range_i = data_moment_range / 1000.0 + i * data_moment_range_sample_interval / 1000.0`
- `data_moment_range_sample_interval` is gate width in km × 1000.
  - To get km: `data_moment_range_sample_interval as f64 / 1000.0`
- The physical measurement is `(raw - offset) / scale`; raw values 0 and 1 represent below-threshold and range-folded, respectively. Validate the word size, finite positive scale, finite offset, and expected encoded length before decoding. Version 0.1.2's `decoded_values()` treats zero scale as unscaled measurements and iterates bytes rather than 16-bit words, so it must not be used unconditionally. Retain each gate's position and category. See the [locked crate source](https://docs.rs/crate/nexrad-decode/0.1.2/source/src/messages/digital_radar_data/generic_data_block.rs).

#### `DataBlockId`

```rust
pub struct DataBlockId {
    pub data_moment_type: DataMomentPointerType,
    pub data_name: [u8; 3],
}

pub enum DataMomentPointerType {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    DifferentialPhase,
    CorrelationCoefficient,
    SpecificDifferentialPhase,
}
```

#### `ScaledMomentValue` (enum)

```rust
pub enum ScaledMomentValue {
    Value(f32),         // decoded floating-point value
    BelowThreshold,     // below signal threshold
    RangeFolded,        // range folded
}
```

---

## Crate: `nexrad_model`

### Module: `nexrad_model::data`

#### `Scan`

```rust
pub struct Scan { /* ... */ }

impl Scan {
    pub fn sweeps(&self) -> &[Sweep]
}
```

#### `Sweep`

```rust
pub struct Sweep { /* ... */ }

impl Sweep {
    pub fn radials(&self) -> &[Radial]
}
```

#### `Radial`

```rust
pub struct Radial { /* ... */ }

impl Radial {
    pub fn azimuth_angle_degrees(&self) -> f32
    pub fn azimuth_spacing_degrees(&self) -> f32
    pub fn elevation_angle_degrees(&self) -> f32
    
    // Returns None if this radial has no reflectivity data.
    pub fn reflectivity(&self) -> Option<&MomentData>
    pub fn velocity(&self) -> Option<&MomentData>
}
```

#### `MomentData`

```rust
pub struct MomentData { /* ... */ }

impl MomentData {
    // Construct from decoded values from a GenericDataBlock.
    pub fn from_fixed_point(scale: f32, offset: f32, values: Vec<u8>) -> Self
    
    // Returns raw Vec<u8> values. Use values() for decoded floats.
    pub fn raw(&self) -> &Vec<u8>
    
    // Returns Vec<MomentValue> with decoded floating-point values.
    pub fn values(&self) -> Vec<MomentValue>
}
```

#### `MomentValue` (enum)

```rust
pub enum MomentValue {
    Value(f32),
    BelowThreshold,
    RangeFolded,
}
```

**Note:** `MomentData` does not expose `data_moment_range` or gate width. Gate metadata is in the decode-level `GenericDataBlockHeader`.

### Module: `nexrad_model::meta`

#### `Site`

```rust
pub struct Site { /* ... */ }

impl Site {
    // Construct with all metadata.
    pub fn new(
        identifier: [u8; 4],
        latitude: f32,          // degrees
        longitude: f32,         // degrees
        height_meters: i16,
        feedhorn_height_meters: u16,
    ) -> Self
    
    pub fn identifier(&self) -> &[u8; 4]
    pub fn identifier_string(&self) -> String   // e.g. "KPAH"
    pub fn latitude(&self) -> f32
    pub fn longitude(&self) -> f32
    pub fn height_meters(&self) -> i16
    pub fn feedhorn_height_meters(&self) -> u16
}
```

**Note:** `Site` must be constructed manually. However, site coordinates are available from `VolumeDataBlock` in the decoded data — no external source needed.

---

## Site Coordinates

**Source:** `VolumeDataBlock.latitude` and `VolumeDataBlock.longitude` (f32, degrees).

Found on the first `DigitalRadarData` message where `header.radial_status == 0` (ElevationStart) or `header.radial_status == 3` (VolumeScanStart). Cast to f64 for projection calls: `volume_data_block.latitude as f64`, `volume_data_block.longitude as f64`.

Rendering uses the volume header coordinates. Runtime-discovered station metadata supplies coordinates for site selection and for the catalog-versus-header diagnostic. There is no bundled site catalog.

---

## Error Types

### `nexrad_decode::result::Error` (from `nexrad_decode` crate)

Decode errors. Used as source in higher-level errors.

```rust
pub enum Error {
    FileError(std::io::Error),              // File read failure
    DeserializationError(bincode::Error),   // Bincode decode failure
    DecodingError(String),                  // Binary data decoding failure
    MessageMissingDateError,                 // Message has no date field
}
```

### `nexrad_model::result::Error` (from `nexrad_model` crate)

Model construction errors.

```rust
pub enum Error {
    ElevationMismatchError,  // Elevation angle mismatch between radials in a sweep
}
```

---

## Units

| Quantity | Raw Unit | To km / degrees |
|----------|----------|-----------------|
| Gate range (in GenericDataBlockHeader) | u16, 0.001-scaled km | ÷ 1000.0 |
| Gate width (in GenericDataBlockHeader) | u16, 0.001-scaled km | ÷ 1000.0 |
| Azimuth angle (in digital_radar_data::Header) | f32 | degrees |
| Elevation angle (in digital_radar_data::Header) | f32 | degrees |
| Site latitude (in Site) | f32 | degrees |
| Site longitude (in Site) | f32 | degrees |
| Header time | u32 | ms past midnight, combine with date for `DateTime<Utc>` |

The `uom` feature enables typed units via the `uom` crate. When using `data_moment_range()` / `data_moment_range_sample_interval()` methods, values are returned as `Length` types. Conversion: `length.get::<uom::si::f64::Kilometer>()`.
