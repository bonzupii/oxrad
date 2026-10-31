# Network behavior

This document describes the network paths in the current implementation. It does not describe planned sources as if they exist.

## Hosts

| Host | Purpose | Current use |
|---|---|---|
| `api.weather.gov` | Radar-station discovery and NWS alerts in planned work | `GET /radar/stations` with `Accept: application/geo+json` |
| `nominatim.openstreetmap.org` | Place search | `GET /search` with `format=jsonv2`, a result limit, and a user agent |
| `api.open-meteo.com` | Global forecast | `GET /v1/forecast` for current, hourly, and daily data |
| `unidata-nexrad-level2.s3.amazonaws.com` | NEXRAD Level II archive | S3 list and object GET |

All requests identify the client as `oxrad/<CARGO_PKG_VERSION> (https://github.com/bonzupii/oxrad)`.

## Timeouts and body limits

| Path | Timeout | Body limit |
|---|---|---|
| JSON providers (`provider.rs`) | 10 seconds | 4 MiB |
| NEXRAD S3 (`network.rs`) | 30 seconds | 64 MiB |

The NEXRAD body limit is whole-response buffering. It is sufficient for an Archive II volume today and is a known memory limitation. Streaming acquisition is not implemented.

## Discovery and cache

The NWS station document is fetched at application start. A successful response is parsed, every feature must have a supported station type, and duplicate identifiers are rejected. The raw document and a save timestamp are written atomically to `~/.oxrad/stations.json`. If the cache path cannot be written, the live stations remain usable and the UI reports the cache warning. A successful feed is refreshed after six hours; a failed feed retries after fifteen minutes.

On startup, the cached document is read before the network request. A missing cache is not fatal: the station list shows that discovery is in progress. If live discovery fails and a cache is already loaded, the cached metadata remains visible with a notice. If no cached metadata exists, the failure is shown in the station panel. The current feed runs once per launch; automatic periodic station refresh is planned and not implemented.

The place cache is in memory only. Successful searches are cached for the session. Search requests are serialized with a one-request-per-second minimum interval.

## Forecast

Open-Meteo is requested with the selected place's latitude and longitude and `timezone=auto`. The response must contain `current`, `current_units`, `hourly`, `hourly_units`, `daily`, and `timezone`. Missing fields are typed shape errors, not defaults. A failed refresh keeps the last forecast for the same place and labels it as last received. A failed forecast for a different place cannot replace the current place's forecast.

The forecast request is one provider call. It does not use NWS gridpoints and is not limited to the United States.

## NEXRAD acquisition

The archive client lists each UTC day under `{year}/{month}/{day}/{site}/`, filters keys ending in `_V06` with a two-digit version, and selects the lexicographically newest key. Listing XML is parsed with `quick-xml`, including nested `Contents/Key`, whitespace, entities, `IsTruncated`, and `NextContinuationToken`. Pagination is capped at 10,000 pages.

The object is downloaded into memory and decoded in a worker. A 404 is reported as a stale listing (`VolumeGone`), not as an empty sky. A volume is accepted only when assembly sees volume-start and volume-end radials, complete sweep boundaries, and valid site coordinates.

## Retry behavior

Forecast retries use a 2 second base, exponential growth to 60 seconds, jitter, and `Retry-After` when a provider supplies it. NEXRAD fetch retries use the same base and cap. A deterministic decode failure retries once and then waits for a new volume or a manual retry. A changed forecast place supersedes the previous request; a stale answer is discarded by generation.

## Request cancellation

The forecast and place workers use generation numbers so a superseded answer cannot overwrite a newer selection. The radar pipeline stops when it is dropped. There is no transfer-level cancellation: a request already in flight completes or reaches its timeout. Bounded worker shutdown and streaming cancellation are not implemented.

## Privacy

Place queries are sent to Nominatim. Forecast coordinates are sent to Open-Meteo. Radar requests send only the site and time prefix. No account, API key, or device identifier is sent.
