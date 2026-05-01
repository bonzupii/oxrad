# NEXRAD Rust Crate API Reference

Sourced from `cargo doc` HTML output in `target/doc/`. Units: km unless noted. Error types use `thiserror` with complete-sentence messages.

---

## Crate: `nexrad_data`

### Module: `nexrad_data::aws::realtime`

Functions are async (requires `tokio` runtime).

```rust
// Get the latest available volume index for a site.
pub async fn get_latest_volume(site: &str) -> Result<LatestVolumeResult>
```

```rust
// List chunks belonging to a volume. Returns chunk IDs sorted oldest→newest.
pub async fn list_chunks_in_volume(
    site: &str,
    volume: VolumeIndex,
    max_keys: usize,
) -> Result<Vec<ChunkIdentifier>>
```

```rust
// Download a single chunk. Returns (ChunkIdentifier, Chunk).
pub async fn download_chunk(
    site: &str,
    chunk_id: &ChunkIdentifier,
) -> Result<(ChunkIdentifier, Chunk)>
```

```rust
// Estimate when the next chunk will arrive for a given volume.
pub async fn estimate_next_chunk_time(
    site: &str,
    volume: VolumeIndex,
) -> Result<DateTime<Utc>>
```

#### `VolumeIndex`

```rust
pub struct VolumeIndex(usize);

impl VolumeIndex {
    pub fn new(index: usize) -> Self
    pub fn as_number(&self) -> usize
}
```

**Implements:** `Copy`, `Clone`, `PartialEq`, `Eq`, `Ord`, `PartialOrd`, `Hash`, `Debug`, `Display`.

#### `LatestVolumeResult`

```rust
pub struct LatestVolumeResult {
    pub volume: Option<VolumeIndex>,  // None if no volume available
    pub calls: usize,                  // Number of API calls made
}
```

#### `ChunkIdentifier`

```rust
pub struct ChunkIdentifier { /* opaque */ }
```

#### `Chunk` (enum)

```rust
pub enum Chunk {
    RDAStatusData(Vec<u8>),
    DigitalRadarData(Vec<u8>),
    ClutterFilterMap(Vec<u8>),
    VolumeCoveragePattern(Vec<u8>),
}
```

#### `ChunkType` (enum)

```rust
pub enum ChunkType {
    RDAStatusData,
    DigitalRadarData,
    ClutterFilterMap,
    VolumeCoveragePattern,
}
```

#### `NextChunk` (enum)

Indicates where to find the next expected chunk.

```rust
pub enum NextChunk {
    Sequence(ChunkIdentifier),  // next chunk is in same volume at this sequence
    Volume(VolumeIndex),         // next chunk is in a new volume
}
```

#### `poll_chunks` (async)

```rust
pub async fn poll_chunks(
    site: &str,
    stop: Receiver<()>,
    chunk_sender: Sender<Result<(ChunkIdentifier, Chunk), AWSError>>,
    stats_sender: Option<Sender<PollStats>>,
) -> Result<(), AWSError>
```

Polls AWS S3 for new chunks. Sends new chunks to `chunk_sender`. Stops when `stop` receives a message. Optionally sends `PollStats` to `stats_sender`.

---

### Module: `nexrad_data::volume`

#### `File`

Archive II volume file. Constructed from raw bytes.

```rust
pub struct File { /* ... */ }

impl File {
    pub fn new(data: Vec<u8>) -> Self
    pub fn data(&self) -> &Vec<u8>
    
    pub fn header(&self) -> Result<Header>   // decode Archive II file header
    pub fn records(&self) -> Vec<Record<'_>>  // LDM records in file
    
    // High-level: decodes entire file to nexrad_model Scan.
    // Returns nexrad_model::data::Scan with sweeps, radials, moment data.
    // This is the preferred entry point for getting reflectivity data.
    pub fn scan(&self) -> Result<nexrad_model::data::Scan>
}
```

#### `Header`

Archive II file header metadata.

```rust
pub struct Header { /* ... */ }

impl Header {
    pub fn deserialize<R: Read>(reader: &mut R) -> Result<Self>
    
    // Filename indicating data version, e.g. "AR2V0012." (pre-RDA Build 12.0)
    // or "AR2V0106." (Build 12.0+). Versions 02-07.
    pub fn tape_filename(&self) -> Option<String>
    
    // Sequential number 001-999, rolls over.
    pub fn extension_number(&self) -> Option<String>
    
    // Volume collection timestamp.
    pub fn date_time(&self) -> Option<DateTime<Utc>>
    
    // Four-letter radar identifier (e.g. "KPAH").
    pub fn icao_of_radar(&self) -> Option<String>
}
```

**Note:** `Header` does NOT contain site lat/lon. The lat/lon for a site must come from elsewhere (site lookup by ICAO, `nexrad_model::meta::Site` must be constructed manually).

#### `Record`

Single LDM record from an Archive II file.

```rust
pub struct Record<'a> { /* ... */ }

impl Record<'_> {
    pub fn new(data: Vec<u8>) -> Self
    pub fn from_slice(data: &[u8]) -> Self
    pub fn data(&self) -> &[u8]
    pub fn compressed(&self) -> bool
    
    // Decompress BZIP2-compressed data. Returns new Record with decompressed data.
    pub fn decompress(&self) -> Result<Record<'_>>
    
    // Decode LDM record into NEXRAD Level II messages.
    // Returns Vec<nexrad_decode::messages::Message>.
    pub fn messages(&self) -> Result<Vec<nexrad_decode::messages::Message>>
}
```

---

## Crate: `nexrad_decode`

### Module: `nexrad_decode::messages`

#### `Message` (top-level wrapper)

Decoded NEXRAD Level II message (any type). The one you get from `record.messages()`.

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
let msg: Message = /* from record.messages() */;
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

**RadialStatus values:** 0→ElevationStart, 1→IntermediateRadialData, 2→ElevationEnd, 3→VolumeScanStart, 4→VolumeScanEnd, other→ElevationStartVCPFinal.

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
- Scale/offset for dBZ: `dbz = raw * scale + offset`

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

**Note:** `MomentData` does NOT expose `data_moment_range` or gate width. That metadata is only available in the decode-level `GenericDataBlockHeader`. If you use `File::scan()` and need gate metadata, you must extract it from `record.messages()` → `MessageContents::DigitalRadarData` → `reflectivity_data_block.header`.

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

## High-Level Workflow

### Path 1: Deprecated — Use `File::scan()`

```rust
let file = nexrad_data::volume::File::new(all_data);
let scan = file.scan()?;

// First sweep, first radial:
let sweep = &scan.sweeps()[0];
let radial = &sweep.radials()[0];
let moment = match radial.reflectivity() {
    Some(m) => m,
    None => { /* handle missing data */ return; }
};

// Values are decoded dBZ:
for mv in moment.values() {
    match mv {
        MomentValue::Value(dbz) => { /* plot */ }
        _ => { /* skip */ }
    }
}
```

**Problem:** `moment` has no gate range or width. You can't correctly position bins on screen. Also skips `BelowThreshold`/`RangeFolded` gates, breaking index-to-range alignment. **Do not use this path.**

### Path 2: Correct — Use `Record::messages()`

```rust
let file = nexrad_data::volume::File::new(all_data);
for record in file.records() {
    let messages = record.messages()?;
    for message in messages {
        let contents = message.contents();
        if let MessageContents::DigitalRadarData(inner) = contents {
            if let Some(ref block) = inner.reflectivity_data_block {
                let header = &block.header;
                
                // Gate metadata (0.001-scaled km → divide by 1000):
                let first_gate_km = header.data_moment_range as f32 / 1000.0;
                let gate_width_km = header.data_moment_range_sample_interval as f32 / 1000.0;
                let num_gates = header.number_of_data_moment_gates as usize;
                
                // For gate i:
                let range_km = first_gate_km + i as f32 * gate_width_km;
                
            // Azimuth from radial header (f32, degrees):
            let az_deg = inner.header.azimuth_angle;
                
                // dBZ from decoded values:
                let decoded = block.decoded_values();
                for (i, val) in decoded.into_iter().enumerate() {
                    match val {
                        ScaledMomentValue::Value(dbz) => { /* plot at range_km + i*gate_width_km */ }
                        _ => { }
                    }
                }
            }
        }
    }
}
```

### Path 3: Deprecated — Mixed `File::scan()` + `Record::messages()`

Discarded. Path 2 is the sole correct approach. Using `File::scan()` at all loses gate metadata and breaks index alignment.

---

## Site Coordinates

**Source:** `VolumeDataBlock.latitude` and `VolumeDataBlock.longitude` (f32, degrees).

Found on the first `DigitalRadarData` message where `header.radial_status == 0` (ElevationStart) or `header.radial_status == 3` (VolumeScanStart). Cast to f64 for projection calls: `volume_data_block.latitude as f64`, `volume_data_block.longitude as f64`.

No external lookup or hardcoded constants needed.

---

## Error Types

### `nexrad_data::result::Error` (from `nexrad_data` crate)

Used as `source` in `RadarError::NetworkFetch`, `RadarError::Decode`, etc.

```rust
pub enum Error {
    FileError(std::io::Error),              // File I/O failure
    DeserializationError(bincode::Error),  // Bincode decode failure
    UncompressedDataError,                  // Data unexpectedly uncompressed
    AWS(aws::AWSError),                     // AWS/S3 operations
    Decode(decode::Error),                  // Level II message decode
    Model(model::Error),                    // Model construction
    CompressedDataError,                    // Data unexpectedly compressed
    MissingCoveragePattern,                 // Volume coverage pattern not found
    DecompressionError(bzip2::mem::Error), // BZIP2 decompression
}
```

### `nexrad_data::result::aws::AWSError`

AWS/S3-specific errors. 14 variants:

```rust
pub enum AWSError {
    TruncatedListObjectsResponse,                          // S3 response truncated mid-list
    DateTimeError(String),                                 // Failed to parse datetime
    InvalidSiteIdentifier(String),                        // Site ICAO invalid (not 4 chars)
    UnrecognizedChunkFormat,                              // Unknown chunk format in S3
    S3ListObjectsError(reqwest::Error),                   // Listing S3 objects failed
    S3GetObjectRequestError(reqwest::Error),              // S3 GET request construction failed
    S3GetObjectError(Option<String>),                     // S3 GET failed (body or message)
    S3ObjectNotFoundError,                                // S3 key not found (404)
    S3StreamingError(reqwest::Error),                     // Streaming S3 body failed
    LatestVolumeNotFound,                                 // No volume found for site
    ExpectedChunkNotFound,                                // Expected chunk missing from volume
    PollingAsyncError,                                     // Async polling channel error
    FailedToDetermineNextChunk,                           // Could not estimate next chunk time
    S3ListObjectsDecodingError,                            // Failed to decode S3 list response
}
```

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
