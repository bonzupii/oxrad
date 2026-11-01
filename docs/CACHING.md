# Caching

## Current implementation

`cache.rs` is a directory-backed response cache.

| Property | Value |
|---|---|
| Location | `~/.oxrad/cache/responses/v1` |
| Key | Request URL |
| File name | SHA-256 of the key |
| Metadata | JSON sidecar with schema, key, fetch time, byte length, body digest |
| Integrity | Body length and SHA-256 checked on every read |
| Write | Temporary file plus rename, metadata written last |
| Entry limit | 256 |
| Byte limit | 128 MiB |
| Body limit | 32 MiB |
| Eviction | Oldest fetch time first, on prune |
| Corruption | Entry removed on detection, live request attempted, warning surfaced |

Cached bodies in use today: NEXRAD archive volumes, RainViewer manifests, RainViewer tiles.

In-memory caches: RainViewer manifest for 90 seconds, up to 120 decoded tiles, place searches for the session.

Other persisted state: `~/.oxrad/stations.json` holds the last station document and its save time. `~/.oxrad/config.toml` holds user preferences.

## Defects

| Defect | Consequence |
|---|---|
| No TTL or freshness field | A cached body is treated as valid forever; food safety relies on the caller |
| No conditional requests | Every miss refetches a full body |
| No stale-while-revalidate | A stale entry cannot serve while a refresh runs |
| No per-class policy | Radar volumes, tiles, forecasts, and alerts share one set of limits |
| No in-flight deduplication | Two panels requesting the same URL issue two requests |
| Prune only on RainViewer refresh | NEXRAD-only sessions can exceed the intended budget |
| No cache metrics | Hit rate and byte use are unmeasured |

## Target design

One store, one policy table, explicit classes.

```rust
struct CacheKey { class: CacheClass, identity: String }

struct CachePolicy {
    ttl: Duration,
    stale_while_revalidate: bool,
    max_body: usize,
    retain_offline: bool,
}
```

| Class | TTL | Offline retention | Notes |
|---|---|---|---|
| `RadarVolume` | 24 hours | Yes, up to byte budget | Immutable once published |
| `RadarListing` | 2 minutes | No | Small, changes every volume |
| `RadarStationList` | 6 hours | Yes | Required for offline start |
| `Forecast` | 30 minutes | Yes, labelled stale | Coordinates in the key |
| `PlaceSearch` | 7 days | Yes | Query in the key |
| `Alerts` | 2 minutes | Yes, labelled with age | Never shown as current |
| `RadarTile` | 10 minutes | Yes, bounded | Immutable per frame time |
| `BasemapTile` | 30 days | Yes | Versioned by style |
| `SourceMetadata` | 24 hours | Yes | Licence and attribution travel with it |

Rules:

- A read returns `Fresh`, `Stale`, or `Miss`. The caller sees which.
- A stale entry is served only when its class allows it, and the interface labels it.
- Conditional requests use `ETag` and `Last-Modified` where the operator supplies them. A `304` refreshes the timestamp without rewriting the body.
- Bodies are never rewritten to satisfy a presentation preference.
- Credentials are never cached.
- Eviction is per class, so a large radar archive cannot evict the station directory.

## Ownership

The store is a value owned by the thread that uses it. It holds a root path and no shared state. Concurrent writers are safe because each write is atomic and keyed. Readers verify integrity, so a partially written entry is a miss.

## Failure and offline

| Condition | Behavior |
|---|---|
| Cache directory unavailable | Request proceeds live; the interface reports that caching is disabled |
| Corrupt entry | Entry removed, live request attempted, warning shown |
| Digest mismatch | Same as corrupt |
| Offline with cache | Cached data renders with age and an offline marker |
| Offline without cache | Failure state names the source and the missing data |
| Quota exceeded | Oldest entries of the same class removed before the write |

No failure silently converts to a plausible value. A cache miss that cannot reach the network is an error, not empty data.

## Testing

Required tests: cold miss, warm hit, stale within policy, stale outside policy, corruption, interrupted write, eviction order, per-class isolation, offline read, and concurrent writes to one key.

Current tests cover round trip, digest mismatch, and eviction order. The remaining cases are not covered.
