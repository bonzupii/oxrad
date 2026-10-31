# Network behavior

This document describes the network paths in the current implementation. It does not describe planned sources as if they exist.

## Hosts

| Host | Purpose | Current use |
|---|---|---|
| `api.weather.gov` | Radar-station discovery and active warnings | `GET /radar/stations` and `GET /alerts/active?status=actual&message_type=alert`, both with `Accept: application/geo+json` |
| `nominatim.openstreetmap.org` | Place search | `GET /search` with `format=jsonv2`, a result limit, and a user agent |
| `api.open-meteo.com` | Global forecast | `GET /v1/forecast` for current, hourly, and daily data |
| `unidata-nexrad-level2.s3.amazonaws.com` | NEXRAD Level II archive | S3 list and object GET |
| `api.rainviewer.com` | Global radar manifest | `GET /public/weather-maps.json` |
| `tilecache.rainviewer.com` | Global radar image tiles | 512-pixel PNG tile GETs for each manifest frame |

All requests identify the client as `oxrad/<CARGO_PKG_VERSION> (https://github.com/bonzupii/oxrad)`.

## Timeouts and body limits

| Path | Timeout | Body limit |
|---|---|---|
| JSON providers (`provider.rs`) | 10 seconds | 4 MiB |
| NEXRAD S3 and RainViewer tiles (`network.rs`, `global_radar.rs`) | 30 seconds | 64 MiB |

The NEXRAD body limit is whole-response buffering. It is sufficient for an Archive II volume today and is a known memory limitation. Streaming acquisition is not implemented.

## Discovery and cache

The NWS station document is fetched at application start. A successful response is parsed, every feature must have a supported station type, and duplicate identifiers are rejected. The raw document and a save timestamp are written atomically to `~/.oxrad/stations.json`. If the cache path cannot be written, the live stations remain usable and the UI reports the cache warning. A successful feed is refreshed after six hours; a failed feed retries after fifteen minutes.

On startup, the cached document is read before the network request. A missing cache is not fatal: the station list shows that discovery is in progress. If live discovery fails and a cache is already loaded, the cached metadata remains visible with a notice. If no cached metadata exists, the failure is shown in the station panel. The current feed runs once per launch; automatic periodic station refresh is planned and not implemented.

The place cache is in memory only. Successful searches are cached for the session. Search requests are serialized with a one-request-per-second minimum interval.

## Forecast

Open-Meteo is requested with the selected place's latitude and longitude and `timezone=auto`. The response must contain `current`, `current_units`, `hourly`, `hourly_units`, `daily`, and `timezone`. Missing fields are typed shape errors, not defaults. A failed refresh keeps the last forecast for the same place and labels it as last received. A failed forecast for a different place cannot replace the current place's forecast.

The forecast request is one provider call. It does not use NWS gridpoints and is not limited to the United States.

## Active warnings

The active-alert feed is requested without coordinates; it returns the national collection of actual, non-test alerts. Each feature must have a nonempty identifier, event name, and valid polygon or multipolygon geometry, or be retained with no geometry and counted as such. Geometry coordinates are longitude then latitude and must fall in valid WGS84 ranges. Severity comes from the provider's `severity` field; an absent or unknown value is shown as unknown rather than assigned a severity.

The feed is parsed in a worker and superseded generations are discarded. A successful collection is refreshed after five minutes; a failed request retries after two minutes and is shown in the map status and notice. The layer defaults to shown only within the Map scene; the Radar scene does not draw warning polygons. The application does not filter the feed to the viewport and does not expose issuance, urgency, or full alert text yet. Coverage is the United States and its territories, not the rest of the world.

## NEXRAD acquisition

The archive client lists each UTC day under `{year}/{month}/{day}/{site}/`, filters keys ending in `_V06` with a two-digit version, and selects the lexicographically newest key. Listing XML is parsed with `quick-xml`, including nested `Contents/Key`, whitespace, entities, `IsTruncated`, and `NextContinuationToken`. Pagination is capped at 10,000 pages.

The object is downloaded into memory and decoded in a worker. A 404 is reported as a stale listing (`VolumeGone`), not as an empty sky. A volume is accepted only when assembly sees volume-start and volume-end radials, complete sweep boundaries, and valid site coordinates.

## Global radar composite

RainViewer supplies a manifest of 10-minute reflectivity frames for the previous two hours. The manifest is accepted only from an HTTPS host and only with validated frame paths and times. The manifest is cached in the worker for 90 seconds; decoded PNG bytes are cached for up to 52 tiles. Each viewport request selects the best Web Mercator zoom up to level 7 and fetches the tile containing the map centre.

The viewport is covered by a bounded grid of at most eight Web Mercator tiles at the highest zoom that fits the budget. Tile bounds travel with each frame so the renderer places every tile independently, including views that cross the antimeridian. Uncovered areas remain uncovered only where RainViewer itself has no radar coverage or the source supplied no echo.

A new viewport request supersedes the previous generation; stale manifest, frame, and error messages are discarded. A source failure is reported in the UI and retried after 60 seconds. RainViewer requires visible attribution in the application, which the map shows as "Weather data by RainViewer".

## Retry behavior

Forecast retries use a 2 second base, exponential growth to 60 seconds, jitter, and `Retry-After` when a provider supplies it. NEXRAD fetch retries use the same base and cap. A deterministic decode failure retries once and then waits for a new volume or a manual retry. A changed forecast place supersedes the previous request; a stale answer is discarded by generation.

## Request cancellation

The forecast, place, global-radar, and active-warning workers use generation numbers so a superseded answer cannot overwrite a newer selection. The radar pipeline stops when it is dropped. There is no transfer-level cancellation: a request already in flight completes or reaches its timeout. Bounded worker shutdown and streaming cancellation are not implemented.

## Privacy

Place queries are sent to Nominatim. Forecast coordinates are sent to Open-Meteo. Global radar requests send the map-centre tile coordinates to RainViewer. NEXRAD requests send only the site and time prefix. Active-warning requests send no coordinates or user identity and receive the national United States collection. No account, API key, or device identifier is sent.
