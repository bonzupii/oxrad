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
| Basemap | Natural Earth | Global | Countries, settlements, admin-1 boundaries, rivers, major highways | Embedded GeoJSON | Static release | Public domain | Verified |
| Earthquakes | USGS Earthquake Hazards Program | Global | Epicentre, magnitude and scale, depth, origin time, review status, tsunami flag, PAGER alert level, felt reports, event page | `earthquake.usgs.gov/earthquakes/feed/v1.0/summary/`, GeoJSON | Minute-scale, feeds summarise the past hour, day, week | United States Government work, public domain | Verified |
| Satellite imagery | NASA EOSDIS GIBS | Global, Web Mercator, to level 9 for true colour and the vegetation index, level 8 for snow cover, level 7 for the temperature and chlorophyll layers, level 6 for the rest | MODIS Terra corrected reflectance, cloud-top temperature and height, aerosol optical depth, land-surface and sea-surface temperature, ocean chlorophyll, snow cover, eight-day NDVI, and GPM IMERG precipitation rate | `gibs.earthdata.nasa.gov/wmts/epsg3857/best/wmts.cgi`, WMTS KVP, PNG and JPEG tiles | Provider composites; the layer offers six days of archive and falls back a day when a composite is not built yet | NASA open data, no key required | Verified: tiles fetched for every layer; true colour, cloud-top temperature, and NDVI drawn in the application |
| Geomagnetic index | NOAA Space Weather Prediction Center | Global | Three-hour planetary K index, two days observed and estimated plus two days predicted, with the NOAA storm scale per value | `services.swpc.noaa.gov/products/noaa-planetary-k-index-forecast.json`, JSON | Three hours, refreshed every ten minutes | United States Government work, public domain | Verified: 81 values read, 17 of them predicted |
| Solar wind | NOAA Space Weather Prediction Center | Sun-Earth line at L1 | Proton speed and the interplanetary magnetic field magnitude and Bz | `products/summary/solar-wind-speed.json` and `products/summary/solar-wind-mag-field.json`, JSON | One minute | United States Government work, public domain | Verified: speed and field both answered with current values |
| Air quality | Open-Meteo, CAMS modelled concentrations | Global, roughly 11 km grid for the global CAMS product | United States and European air-quality indices, PM2.5, PM10, ozone, nitrogen dioxide, sulphur dioxide, carbon monoxide, hourly | `air-quality-api.open-meteo.com/v1/air-quality`, JSON | Hourly, past day and two forecast days in the layer | CC BY 4.0, attribution required | Verified: current readings, units, and 72 hourly values at 39.0, -96.2 |
| Climate reanalysis | Open-Meteo archive, ERA5 | Global, about 30 km grid | Daily mean temperature and precipitation from 1991 to the archive's last day, folded by the layer into monthly means, a 1991–2020 baseline, and departures | `archive-api.open-meteo.com/v1/archive`, JSON | Daily, about a week behind the calendar | CC BY 4.0, attribution required | Verified: 13,084 days in one request at 39.0, -96.2, with October departs computed against a 30-year baseline |
| Wildfire perimeters | NIFC WFIGS, interagency perimeters | United States | Incident name, mapped acres, containment, point of origin state, mapping time, and the perimeter polygon | `services3.arcgis.com/T4QMspbfLg3qTGWY/…/WFIGS_Interagency_Perimeters_Current/query`, GeoJSON over ArcGIS REST | Continuous, refreshed by the layer every 30 minutes | United States Government work, public domain | Verified: 82 perimeters and 769,498 mapped acres at the current minute |
| Storm environment | Open-Meteo forecast | Global | Convective available potential energy, convective inhibition, lifted index, freezing level, and surface, 850 hPa and 500 hPa winds, hourly | `api.open-meteo.com/v1/forecast`, JSON | Hourly model steps, two days per request | CC BY 4.0, attribution required | Verified: 48 hours of fields at 39.0, -96.2, including both wind levels the shear needs |
| Sea state | Open-Meteo marine, MeteoFrance MFWAM and SMOC with ECMWF WAM | Global ocean, about 8 km for the French models | Significant wave height, wave direction and period, swell height, sea-surface temperature, surface current speed and direction, hourly | `marine-api.open-meteo.com/v1/marine`, JSON | Hourly, three days per request, wave models updated every 6–12 hours | CC BY 4.0, attribution required | Verified: 72 hours of waves, swell, temperature, and current at 19.0, 175.6 |
| Surface observations | Aviation Weather Center, METAR | Worldwide where aviation reports exist | Station identity, position, elevation, observation time, temperature, dew point, wind and gusts, visibility, altimeter, cloud cover, flight category, raw report | `aviationweather.gov/api/data/metar` with a bounding box, JSON | Hourly and on change; the layer asks for the past two hours and refreshes every ten minutes | United States Government work, public domain | Verified: 103 stations with temperature, wind, visibility and category inside one Kansas box |
| Tropical cyclones | GDACS global disaster alert service (European Commission Joint Research Centre, with JTWC and NOAA sources) | Global | Cyclone name, episode dates, alert level, maximum wind, affected country, issuing source, last reported position | `www.gdacs.org/gdacsapi/api/events/geteventlist/SEARCH?eventlist=TC&fromdate=MM/DD/YYYY&todate=MM/DD/YYYY`, GeoJSON | Updated when an issuing centre posts an advisory | Free and open, attribution requested | Verified: four cyclones in the last thirty days (ARNAB-26 in India at red and 222 km/h, CHAMPI-26 at red and 296 km/h, SIMON-26 and ISAIAS-26 at orange), each with a published position; none in progress at the time of the check, so the live-storm path is exercised by the same code but was not observed with an active storm |
| River gauges | USGS National Water Information System, instantaneous values | United States and its territories | Gage height and discharge, with the agency's qualifiers and provisional mark, per gauge | `waterservices.usgs.gov/nwis/iv/?format=json&bBox=…&parameterCd=00065,00060`, JSON (WaterML-JSON) | Every fifteen minutes, provisional until approved | Public domain, US government work | Verified: 74 gauges in a two-degree box over Kansas, 645 readings per gauge over seven days; the service rejects any box above about twenty-five square degrees and answers a small box in twenty to forty-five seconds on a busy day; it writes `-999999` where a gauge has no measurement |
| Volcanic activity | USGS Volcano Hazards Program, HANS public API | The volcanoes the USGS and its partner observatories issue notices for (Alaska, the Cascades, Hawaiʻi, Yellowstone, and the observatories in the same system) | Volcano name, observatory, alert level, aviation colour code, notice time, plus latitude, longitude, elevation, region and the synopsis of the latest notice | `volcanoes.usgs.gov/hans-public/api/volcano/getElevatedVolcanoes`, then `.../getVolcano/{vnum}` and `.../notice/getNotice/{identifier}`, JSON | A daily update from each observatory, with notices whenever the level changes | Public domain, US government work | Verified: four raised volcanoes (Great Sitkin at orange/watch, Shishaldin, Kilauea and Ahyi Seamount at yellow), each with a published position |
| Land surface | Open-Meteo (best-match forecast model) | Global | Volumetric soil water at 0–10 cm and 10–40 cm, soil temperature at 6 cm, reference evapotranspiration, vapour-pressure deficit, shortwave and direct-normal irradiance, ultraviolet index | `api.open-meteo.com/v1/forecast?hourly=soil_moisture_0_to_10cm,soil_moisture_10_to_40cm,soil_temperature_6cm,et0_fao_evapotranspiration,vapour_pressure_deficit,shortwave_radiation,direct_normal_irradiance,uv_index`, JSON | Hourly, one past day and two forecast days; model runs update several times a day | Free for non-commercial use, attribution required | Verified: 72 hours for a Kansas point with an m³/m³ soil-moisture unit, 0.224 at the top layer and 0.242 below it, and a 332 W/m² solar peak; the seven-centimetre soil-moisture variable returns a placeholder unit and nulls, so the ten-centimetre layer is used instead |
| Sea ice | National Snow and Ice Data Center, Sea Ice Index v4 (G02135) | Both polar hemispheres | Ice extent and ice area, in millions of square kilometres, for one calendar month in every year since 1978, with the source dataset named per row | `noaadata.apps.nsidc.org/NOAA/G02135/{north,south}/monthly/data/{N,S}_MM_extent_v4.0.csv`, comma-separated text | Monthly, published a few weeks after the month ends | Free and open, attribution required | Verified: 48 years of November records in both hemispheres, with a 1981–2010 baseline computed from the same files |
| Vertical profile | Open-Meteo forecast, pressure-level fields | Global model grid | Temperature, relative humidity, geopotential height, wind speed and direction on 1000, 975, 925, 850, 800, 700, 600, 500, 400, 300, 250, and 200 hPa, with two-metre temperature, humidity, dew point, surface pressure, and ten-metre wind | `api.open-meteo.com/v1/forecast` with every pressure-level field, `wind_speed_unit=ms`, one forecast day, JSON | Hourly model steps, refreshed by the layer every 30 minutes or after the map moves 30 km | CC BY 4.0, attribution required | Verified: twelve levels and a 5848 m 500 hPa height at 37.2, -97.5, with the derived cloud base, freezing level, bulk shear, helicity, lapse rate, and precipitable water computed from them |
| Snowpack | Open-Meteo forecast, snow model | Global model grid | Snow depth and snowfall, hourly, with the grid elevation and the provider's units for both | `api.open-meteo.com/v1/forecast` with `hourly=snow_depth,snowfall`, a day of past hours and three forecast days, JSON | Hourly model steps, refreshed by the layer every 30 minutes or after the map moves 20 km | CC BY 4.0, attribution required | Verified: 96 hours of depth and snowfall with a 2380 m grid elevation at 46.5, 10.0 in the Alps |
| River discharge | Open-Meteo flood service (GloFAS) | Global river network grid | Daily river discharge in cubic metres per second, deterministic run plus ensemble members | `flood-api.open-meteo.com/v1/flood`, JSON | Daily, three weeks of past days and seven forecast days in the layer | CC BY 4.0, attribution required | Verified: 28-day record and 50 ensemble members read at 39.0, -96.2 |

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
| Wildfire hotspots | NASA FIRMS | Candidate |
| Satellite imagery | NASA EUMETSAT and NOAA products | Candidate |
| Air quality | OpenAQ | Candidate |
| River gauge levels | National hydrological services | Candidate: a gauge reading needs a national service per country, and the discharge layer uses a global model instead |
| Lightning | Regional detection networks | Candidate |

These are research targets. None has an adapter, and none appears as a layer.

The USGS feeds are admitted: the layer requests `all_hour`, `2.5_day`, or `4.5_week`, keeps each event's own magnitude scale and review status, and counts events the provider listed without a usable magnitude, position, or time instead of plotting them at an invented value. The magnitude floor is part of the feed, so the interface states the floor beside the count: a quiet hour under a floor is not a quiet planet.

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
