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

A transient transport, body-read, stalled-read, 429 or 5xx failure is retried up to four attempts. The wait starts at 4 seconds and doubles to 32 seconds, with a deterministic per-request spread of up to 500 ms. A valid `Retry-After` header replaces the computed wait and is capped at 60 seconds. A 4xx response other than 429, a malformed body and a decode failure are not retried. Deterministic answers use the same policy in tests.

## Cache and revalidation

The cache stores a body, its SHA-256 digest, its length, its fetch time and the `ETag` and `Last-Modified` validators the origin supplied. The body lives in a digest-named file. That file is written first. The metadata file is the single commit point, and the previous body is removed only after the metadata rename. A body is read only when its metadata is valid, its length matches and its SHA-256 digest matches. A truncated or changed body is rejected and removed, then fetched again.

A cache entry younger than the registry refresh cadence, or an endpoint's override, is served as `cached` without a request. An older entry with validators sends `If-None-Match` and `If-Modified-Since`. A 304 returns the stored body without transferring it again, updates the fetch time and any returned validators, and reports `refreshed`. A 200 replaces the body, validators and digest, and reports `live`. An entry without validators is fetched whole. If revalidation or a refetch exhausts its retries on a transient failure, a whole cached body is served as `stale`; the failure is still recorded. The reader-facing labels are `live`, `cached`, `refreshed`, `stale`, `refreshing` and `unavailable`.

The cache schema is 2. Entries written by schema 1 are refused rather than guessed at, and the engine refetches them.

## Large transfers

A binary retry keeps the bytes already read. When the origin advertises `Accept-Ranges: bytes`, the retry sends `Range: bytes=<offset>-`. A 206 response is accepted only when its `Content-Range` starts at the requested offset and, if a total was already known, declares the same total. An origin that answers 200 instead replaces the partial bytes and starts a clean transfer. An origin without range support is retried from zero.

The whole body is verified before it is stored: its length against `Content-Length` or `Content-Range`, and its SHA-256 against `X-Checksum-SHA256`, `X-Amz-Checksum-SHA256`, `Content-Digest` or `Repr-Digest` when the origin supplies one. An archive or image response with neither a declared length nor a checksum is refused, because a short read could not be detected. Partial bytes live in memory only; no partially written or truncated body can satisfy a later request. Resumption does not survive a process restart.

## Cancellation and progress

A transfer carries a reader-owned cancellation flag and the agent carrying its socket. The UI shows `Cancel transfer` while a transfer is active. Cancelling sets the flag and calls `Agent::shutdown()` on that transfer's agent, which closes the socket and unblocks a read waiting for the next byte. The engine checks the flag before every attempt, between body reads, during backoff, and after a response arrives but before it commits anything. A cancelled transfer returns a distinct cancelled result and never stores bytes.

A read that exceeds the phase silence limit without a cancellation is a stall, reported as `stalled`; a transport error is `failed`. A stall remains retryable, and a whole cached body may still be served as `stale` when retries are exhausted. Name resolution and the TCP connect phase are bounded by their own limits and are not interrupted by `Agent::shutdown()`.

Progress snapshots report the active subject, cache state, bytes received, known total and resumed-from offset. A resumed binary reports the offset it started from rather than restarting the display at zero. A revalidation reports `refreshing` while it is in flight.

## Shared queue

Parsing and live probes run on one shared job queue. The queue owns no cache, retry clock or socket. Dropping a readout drops its answer channel and the job queue continues.
