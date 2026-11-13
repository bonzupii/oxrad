# Network behavior

Every provider request passes through `Acquire`. The request engine owns identity, cache lookup, pacing, retries, response validation and provenance. Domain modules pass a registry operation and values; they do not open sockets, own a cache or sleep on a retry.

## Transport policy

Transfer limits come from the registry wire format. They bound each phase and the silence between reads. There is no total deadline on a response, so a large archive can finish over a slow link while a stalled socket still fails.

| Format class | Resolve | Connect | Response headers | Read or write silence |
|---|---|---|---|---|
| JSON, GeoJSON, XML, tiles | 20 s | 20 s | 30 s | 30 s |
| Binary archives | 30 s | 30 s | 60 s | 60 s |

Documents are capped at 4 MiB and binary responses at 64 MiB. A response over the cap is rejected rather than truncated.

## Retry

A transient transport, body-read, 429 or 5xx failure is retried up to four attempts. The wait starts at 4 seconds and doubles to 32 seconds, with a deterministic per-request spread of up to 500 ms. A valid `Retry-After` header replaces the computed wait and is capped at 60 seconds. A 4xx response other than 429, a malformed body and a decode failure are not retried. Deterministic answers use the same policy in tests.

## Cache and provenance

The engine stores successful bodies in the per-user cache with a SHA-256 digest and metadata. The registry refresh cadence selects the lifetime unless an endpoint carries an override. Every served value names its source, operation, station, product, scan time, URL, cache state, attempt count and byte count. `--render-registry`, the status row and `--fetch-source` expose the same records.

HTTP validators, conditional requests and range resumption are not implemented. A body is downloaded whole before it is committed. Transfer cancellation is not implemented; a superseded generation is discarded after it answers.

## Shared queue

Parsing and live probes run on one shared job queue. The queue owns no cache, retry clock or socket. Dropping a readout drops its answer channel and the job queue continues.
