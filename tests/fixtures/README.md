# Real-data test inputs

No radar archive is stored in this repository. The tests retrieve real volumes from NOAA's public NEXRAD distribution at test time, so the bytes stay out of the tree and out of published history.

## Pinned historical archive

`KPAH20261030_023529_V06` is an unaltered KPAH Level II archive volume. It is retrieved from the Unidata AWS archive during the test run:

- URL: `https://unidata-nexrad-level2.s3.amazonaws.com/2026/10/30/KPAH/KPAH20261030_023529_V06`
- Size: 13,738,335 bytes
- SHA-256: `8fb521c151f5022e1b8e771c2312b8b52fcd640849cac997be9e964c203264ae`
- Volume start time in the file name: 2026-10-30 02:35:29 UTC

`src/radar.rs` retrieves the object, checks its length and digest against the values above, then runs the application path from Archive II records through decompression, Level II decoding, sweep assembly, and render geometry. The test fails on a network failure or a digest mismatch. It never substitutes synthetic bytes.

The pinned object is historical data. It is never presented as live radar.

## Current live source

`src/network.rs` separately exercises the live source: it paginates the bucket listing for KPAH to exhaustion, orders the listed keys, retrieves the newest volume, and decodes it through the same path. Completeness comes from the volume start and end radials inside the archive, which the decoder requires before it returns a sweep.

A failure to reach the feed fails that test. It is an environment blocker, not a reason to substitute synthetic input.

NOAA NEXRAD data are public domain; redistribution does not imply NOAA endorsement.
