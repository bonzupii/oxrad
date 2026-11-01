# Data sources

## Status vocabulary

| Status | Meaning |
|---|---|
| Verified | Requested, decoded, and rendered in the running binary |
| Implemented, unverified | Code exists; the current binary has not been observed against the live source |
| Researched | Endpoint, format, and licence identified; no adapter exists |
| Candidate | Known public service; interface and licence not yet confirmed |
| Blocked | A specific technical or legal obstacle is documented |

A source is never listed as available because an organisation publishes something somewhere. It is listed when this application can retrieve, decode, and display it.

## Integrated sources

| Source | Operator | Coverage | Products | Access | Cadence | Licence | Status |
|---|---|---|---|---|---|---|---|
| NEXRAD Level II archive | NOAA, distributed by Unidata | United States WSR-88D | Radial moments: reflectivity, velocity, spectrum width, ZDR, correlation coefficient, differential phase, specific differential phase | S3 list and GET over HTTPS, no key | Volume every 4 to 6 minutes | United States Government work, public domain | Verified |
| Radar station directory | National Weather Service | United States and territories | Station identity, coordinates, type, capability text | `api.weather.gov/radar/stations`, GeoJSON | Continuous | United States Government work, public domain | Verified |
| Active alerts | National Weather Service | United States and territories | Warning polygons, event, severity, urgency, area, headline, description, instruction, expiry | `api.weather.gov/alerts/active`, GeoJSON | Continuous | United States Government work, public domain | Verified |
| Global forecast | Open-Meteo | Global | Current, hourly, daily | `api.open-meteo.com/v1/forecast` | Hourly model updates | CC BY 4.0, attribution required | Verified |
| Place search | OpenStreetMap contributors, Photon | Global | Place name, region, country, coordinates | `photon.komoot.io/api/`, GeoJSON | Live, fair use | ODbL 1.0 for data, attribution required | Verified |
| Global radar composite | RainViewer | 1,200 or more radars in 150 or more countries | Reflectivity composite tiles, two hours of ten-minute frames | `api.rainviewer.com` manifest, `tilecache.rainviewer.com` tiles | 10 minutes | Free personal and community terms, visible attribution required | Verified |

RainViewer is a processed composite. It is not Level II and cannot supply moment-level products. Attribution is displayed in map view.

Photon is used for live type-ahead place search. It supports partial and many misspelled queries; a single adjacent transposition is retried locally when the first answer has no close match. Its public instance is offered under fair use without an availability guarantee, so search errors are surfaced rather than hidden.

## Researched, not implemented

### Deutscher Wetterdienst

| Field | Finding |
|---|---|
| Portal | `https://opendata.dwd.de/weather/radar/` |
| National composite | `weather/radar/composite/rv/`, one tar file per five-minute slot, RADOLAN binary with ASCII header |
| Site products | `weather/radar/sites/`, including `dx/`, `px/`, `sweep_pcp_z/`, `sweep_vol_z/` and dual-polarization sweep directories |
| Format | RADOLAN binary composite; BUFR and ODIM_H5 for site sweeps |
| Projection | Polar stereographic on a sphere of 6370.04 km, origin at the pole, central meridian 10 degrees east |
| Grid | 900x900 and 1100x900 at 1 km, 1500x1400 extended European grid |
| Cadence | Five minutes, files appear about three minutes after nominal time |
| Unit caution | RV is a precipitation-rate product in the RADOLAN family; dBZ-native composites are RX, EX, and WX |
| Licence | Reported as CC BY 4.0 with attribution to DWD; the DWD licence page was not retrieved directly |
| Blocker | No HDF5, BUFR, or RADOLAN decoder exists in this repository |

The projection and header conventions are documented from the wradlib implementation of the DWD specification. An RV file has not been decoded here. Any adapter must verify grid dimensions, word size, and unit from a live file before claiming support.

### Other radar networks

Each entry requires the same verification before an adapter is written. None has an adapter.

| Network | Region | Expected format | Status |
|---|---|---|---|
| Environment and Climate Change Canada | Canada | Raw radar and composite products | Candidate |
| Met Office | United Kingdom | Composite imagery and raw products | Candidate |
| OPERA, via EUMETNET | Europe | ODIM_H5 | Candidate |
| Japan Meteorological Agency | Japan | Composite and site products | Candidate |
| Bureau of Meteorology | Australia | Site and composite products | Candidate |
| China Meteorological Administration | China | Site and mosaic products | Candidate |
| India Meteorological Department | India | Composite imagery | Candidate |
| Roshydromet | Russia | Composite imagery | Candidate |
| Servicio Meteorologico Nacional | Mexico | Composite imagery | Candidate |
| MetService | New Zealand | Composite imagery | Candidate |

Coverage claims for these networks stay out of the interface until an adapter works.

## Candidate environmental sources

| Domain | Service | Status |
|---|---|---|
| Earthquakes | USGS earthquake feeds | Candidate |
| Wildfire hotspots | NASA FIRMS | Candidate |
| Satellite imagery | EUMETSAT, NOAA nowCOAST and GOES products | Candidate |
| Air quality | OpenAQ | Candidate |
| River levels | National hydrological services | Candidate |
| Lightning | Regional detection networks | Candidate |

These are research targets. None has an adapter, and none appears as a layer.

## Admission protocol

A source is admitted in this order. Skipping a step leaves it listed as researched.

1. Retrieve the endpoint and record the exact URL, method, and required headers.
2. Read the licence and attribution terms from the operator, not from a third party summary.
3. Decode a real payload into application types.
4. Measure update cadence and publication latency over at least one hour.
5. Record coverage as geometry where the source provides it.
6. Record products with units, ranges, and quantization.
7. Define failure behavior: timeout, 404, malformed body, rate limit.
8. Add a live integration test that fails when the source breaks.
9. Map the source to `RadarSource`, `TileSource`, `AlertSource`, or `PointSource`.
10. State coverage in the interface, including gaps.

## Rejected and blocked

| Source | Reason |
|---|---|
| Radar products requiring an API key or paid tier | The product is free software with no account |
| Scraped consumer web maps | Terms prohibit automated access; geometry is not authoritative |
| Aggregators without a stated licence | Provenance cannot be preserved |
| Static station lists shipped with the binary | Station identity must come from a runtime directory |

## Attribution requirements

| Source | Required text |
|---|---|
| RainViewer | Weather data by RainViewer |
| OpenStreetMap | Data copyright OpenStreetMap contributors |
| Open-Meteo | Weather data by Open-Meteo, CC BY 4.0 |
| DWD, when integrated | Deutscher Wetterdienst, CC BY 4.0, with a statement of modification |
| National Weather Service | NOAA National Weather Service |

Attribution is displayed where the data appears, not only in an about dialog.
