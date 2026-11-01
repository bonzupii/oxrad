# Real-data test inputs

No radar archive is stored in this repository. The tests retrieve real volumes from NOAA's public NEXRAD distribution at test time, so the bytes stay out of the tree and out of published history.

## Pinned historical archive

`KTWX20261031_003109_V06` is an unaltered KTWX Level II archive volume. It is retrieved from the Unidata AWS archive during the test run:

- URL: `https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/31/KTWX/KTWX20261031_003109_V06`
- Size: 12,584,711 bytes
- SHA-256: `d69fbeb10f2211515cdc82b7b11defd48f98dd48f79b1c27c6d3eb72f2db3d0b`
- Volume start time in the file name: 2026-10-31 00:31:09 UTC

`src/radar.rs` retrieves the object, checks its length and digest against the values above, then runs the application path from Archive II records through decompression, Level II decoding, sweep assembly, and render geometry. The test fails on a network failure or a digest mismatch. It never substitutes synthetic bytes.

The pinned object is historical data. It is never presented as live radar.

## Current live source

`src/network.rs` separately exercises the live source: it paginates the bucket listing for KTWX to exhaustion, orders the listed keys, retrieves the newest volume, and decodes it through the same path. Completeness comes from the volume start and end radials inside the archive, which the decoder requires before it returns a sweep.

A failure to reach the feed fails that test. It is an environment blocker, not a reason to substitute synthetic input.

## Geocoder document

`photon_kansas.json` holds features in Photon's documented GeoJSON response shape. It drives the geocoder parse, rank, and station-resolution tests through the same code path the live provider uses, so the test suite never depends on the public instance being reachable.

NOAA NEXRAD data are public domain; redistribution does not imply NOAA endorsement.
