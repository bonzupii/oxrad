// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//! The open sources this application may use.
//!
//! Every provider is recorded once, with the licence and credit line that
//! appear in the interface, the coverage it actually publishes, its refresh
//! cadence and whether it permits a persistent cache. Admission is a
//! structural test: a source with a key, an unverified licence, no credit
//! line, no coverage or no cache permission cannot pass, and the inventory
//! printed by `oxrad --sources` says which ones fail and why.

use std::error::Error;
use std::fmt::{self, Display, Formatter, Write as _};

/// The stable identifier a source is recorded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceId(&'static str);

impl SourceId {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The Earth-system domain a source feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Basemap,
    Forecast,
    Observation,
    Warnings,
    Radar,
    Satellite,
    Seismicity,
    Hydrology,
    Climate,
    Ocean,
    AirQuality,
    Fire,
    SpaceWeather,
    Volcano,
    Cyclone,
    Terrain,
    Geocoding,
    Solar,
}

impl Domain {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Basemap => "Basemap",
            Self::Forecast => "Forecast",
            Self::Observation => "Observation",
            Self::Warnings => "Warnings",
            Self::Radar => "Radar",
            Self::Satellite => "Satellite",
            Self::Seismicity => "Seismicity",
            Self::Hydrology => "Hydrology",
            Self::Climate => "Climate",
            Self::Ocean => "Ocean",
            Self::AirQuality => "Air quality",
            Self::Fire => "Fire",
            Self::SpaceWeather => "Space weather",
            Self::Volcano => "Volcano",
            Self::Cyclone => "Cyclone",
            Self::Terrain => "Terrain",
            Self::Geocoding => "Geocoding",
            Self::Solar => "Solar",
        }
    }
}

/// Where a source publishes open data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Global,
    /// ISO 3166-1 alpha-2 codes, sorted and unique.
    Countries(&'static [&'static str]),
    /// A documented extent that is neither global nor a country set.
    Extent(&'static str),
}

impl Coverage {
    fn label(self) -> String {
        match self {
            Self::Global => String::from("Global"),
            Self::Countries(codes) => codes.join(", "),
            Self::Extent(extent) => String::from(extent),
        }
    }

    const fn is_empty(self) -> bool {
        match self {
            Self::Global => false,
            Self::Countries(codes) => codes.is_empty(),
            Self::Extent(extent) => extent.is_empty(),
        }
    }
}

/// The wire format the endpoint returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    GeoJson,
    Xml,
    Tiles,
    VectorTiles,
    Binary,
}

impl Format {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::GeoJson => "GeoJSON",
            Self::Xml => "XML",
            Self::Tiles => "Raster tiles",
            Self::VectorTiles => "Vector tiles",
            Self::Binary => "Binary",
        }
    }
}

/// The terms the provider publishes its data under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Licence {
    pub name: &'static str,
    pub url: &'static str,
}

/// How often the provider publishes new data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Seconds(u32),
    ProviderDefined,
}

impl Refresh {
    fn label(self) -> String {
        match self {
            Self::Seconds(seconds) => format!("{seconds} s"),
            Self::ProviderDefined => String::from("provider-defined"),
        }
    }
}

/// Whether the provider's terms permit a persistent local cache of responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePermission {
    Permitted,
    Unstated,
}

impl CachePermission {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Permitted => "permitted",
            Self::Unstated => "unstated",
        }
    }
}

/// How far a source has been carried into the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Found in a catalogue, never fetched.
    Documented,
    /// One keyless request answered with the expected content type.
    Probed,
    /// Our code turned a response into typed data, with a test.
    Parsed,
    /// Parsed, licensed, credited and displayed in the application.
    Admitted,
}

impl Level {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Documented => "documented",
            Self::Probed => "probed",
            Self::Parsed => "parsed",
            Self::Admitted => "admitted",
        }
    }

    /// Only a source our code has parsed may reach the interface.
    pub const fn is_publishable(self) -> bool {
        matches!(self, Self::Parsed | Self::Admitted)
    }
}

/// One provider, recorded once.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub id: SourceId,
    pub name: &'static str,
    pub domain: Domain,
    pub coverage: Coverage,
    /// The human page a reader can visit.
    pub home: &'static str,
    /// The machine endpoint the application requests.
    pub endpoint: &'static str,
    pub format: Format,
    pub licence: Option<Licence>,
    /// The credit line the interface must show verbatim.
    pub credit: &'static str,
    pub refresh: Refresh,
    pub cache: CachePermission,
    pub level: Level,
}

impl Source {
    /// Whether this source may be used at all.
    pub fn admits(&self) -> Result<(), AdmissionError> {
        if self.endpoint.is_empty() {
            return Err(AdmissionError::NoEndpoint { id: self.id });
        }
        if !self.endpoint.starts_with("https://") {
            return Err(AdmissionError::Insecure {
                id: self.id,
                endpoint: self.endpoint,
            });
        }
        if let Some(marker) = credential_marker(self.endpoint) {
            return Err(AdmissionError::Credential {
                id: self.id,
                marker,
            });
        }
        if self.licence.is_none() {
            return Err(AdmissionError::NoLicence { id: self.id });
        }
        if self.credit.is_empty() {
            return Err(AdmissionError::NoCredit { id: self.id });
        }
        if self.coverage.is_empty() {
            return Err(AdmissionError::NoCoverage { id: self.id });
        }
        if self.cache == CachePermission::Unstated {
            return Err(AdmissionError::CacheUnstated { id: self.id });
        }
        Ok(())
    }

    fn admission_label(&self) -> String {
        match self.admits() {
            Err(error) => format!("rejected: {error}"),
            Ok(()) if !self.level.is_publishable() => {
                format!("parsed required, currently {}", self.level.label())
            }
            Ok(()) => String::from("admitted"),
        }
    }
}

/// A query parameter that would turn a public endpoint into a keyed one.
const CREDENTIAL_MARKERS: &[&str] = &[
    "api_key=",
    "apikey=",
    "access_token=",
    "client_secret=",
    "token=",
    "key=",
];

fn credential_marker(endpoint: &str) -> Option<&'static str> {
    CREDENTIAL_MARKERS
        .iter()
        .copied()
        .find(|marker| endpoint.contains(*marker))
}

/// Why a source cannot be admitted.
#[derive(Debug)]
pub enum AdmissionError {
    NoEndpoint {
        id: SourceId,
    },
    Insecure {
        id: SourceId,
        endpoint: &'static str,
    },
    Credential {
        id: SourceId,
        marker: &'static str,
    },
    NoLicence {
        id: SourceId,
    },
    NoCredit {
        id: SourceId,
    },
    NoCoverage {
        id: SourceId,
    },
    CacheUnstated {
        id: SourceId,
    },
}

impl Display for AdmissionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEndpoint { id } => {
                write!(formatter, "{} has no endpoint", id.as_str())
            }
            Self::Insecure { id, endpoint } => write!(
                formatter,
                "{} requests {endpoint}, which is not HTTPS",
                id.as_str()
            ),
            Self::Credential { id, marker } => write!(
                formatter,
                "{} carries {marker} in its endpoint, so it is not keyless",
                id.as_str()
            ),
            Self::NoLicence { id } => write!(
                formatter,
                "{} has no verified licence and cannot ship",
                id.as_str()
            ),
            Self::NoCredit { id } => {
                write!(formatter, "{} has no credit line", id.as_str())
            }
            Self::NoCoverage { id } => {
                write!(formatter, "{} states no coverage", id.as_str())
            }
            Self::CacheUnstated { id } => write!(
                formatter,
                "{} does not permit a persistent cache",
                id.as_str()
            ),
        }
    }
}

impl Error for AdmissionError {}

const CC_BY: Licence = Licence {
    name: "CC BY 4.0",
    url: "https://creativecommons.org/licenses/by/4.0/",
};

const US_GOVERNMENT: Licence = Licence {
    name: "US Government work, public domain",
    url: "https://www.usa.gov/government-copyright",
};

const OSM_COPYRIGHT: Licence = Licence {
    name: "OpenStreetMap data, ODbL 1.0",
    url: "https://www.openstreetmap.org/copyright",
};

/// OpenFreeMap vector tiles and styles.
pub const OPENFREEMAP: Source = Source {
    id: SourceId::new("openfreemap"),
    name: "OpenFreeMap",
    domain: Domain::Basemap,
    coverage: Coverage::Global,
    home: "https://openfreemap.org",
    endpoint: "https://tiles.openfreemap.org",
    format: Format::VectorTiles,
    licence: Some(OSM_COPYRIGHT),
    credit: "OpenFreeMap, OpenMapTiles, OpenStreetMap contributors",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Admitted,
};

/// Open-Meteo numerical forecast.
pub const OPEN_METEO_FORECAST: Source = Source {
    id: SourceId::new("open-meteo-forecast"),
    name: "Open-Meteo forecast",
    domain: Domain::Forecast,
    coverage: Coverage::Global,
    home: "https://open-meteo.com",
    endpoint: "https://api.open-meteo.com/v1/forecast",
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Weather data by Open-Meteo.com",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Open-Meteo ERA5 archive.
pub const OPEN_METEO_ARCHIVE: Source = Source {
    id: SourceId::new("open-meteo-archive"),
    name: "Open-Meteo archive, ERA5",
    domain: Domain::Climate,
    coverage: Coverage::Global,
    home: "https://open-meteo.com/en/docs/historical-weather-api",
    endpoint: "https://archive-api.open-meteo.com/v1/archive",
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Weather data by Open-Meteo.com, ERA5 from ECMWF",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Open-Meteo CAMS air quality.
pub const OPEN_METEO_AIR: Source = Source {
    id: SourceId::new("open-meteo-air-quality"),
    name: "Open-Meteo air quality, CAMS",
    domain: Domain::AirQuality,
    coverage: Coverage::Global,
    home: "https://open-meteo.com/en/docs/air-quality-api",
    endpoint: "https://air-quality-api.open-meteo.com/v1/air-quality",
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Air quality data by Open-Meteo.com, CAMS",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Open-Meteo marine wave and current fields.
pub const OPEN_METEO_MARINE: Source = Source {
    id: SourceId::new("open-meteo-marine"),
    name: "Open-Meteo marine",
    domain: Domain::Ocean,
    coverage: Coverage::Global,
    home: "https://open-meteo.com/en/docs/marine-weather-api",
    endpoint: "https://marine-api.open-meteo.com/v1/marine",
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Marine data by Open-Meteo.com",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// The global reflectivity composite.
pub const RAINVIEWER: Source = Source {
    id: SourceId::new("rainviewer"),
    name: "RainViewer composite",
    domain: Domain::Radar,
    coverage: Coverage::Global,
    home: "https://www.rainviewer.com",
    endpoint: "https://api.rainviewer.com",
    format: Format::Tiles,
    licence: Some(Licence {
        name: "RainViewer free terms, attribution required",
        url: "https://www.rainviewer.com/api.html",
    }),
    credit: "Weather data by RainViewer",
    refresh: Refresh::Seconds(600),
    cache: CachePermission::Permitted,
    level: Level::Admitted,
};

/// The United States NEXRAD Level II archive.
pub const NEXRAD_LEVEL_TWO: Source = Source {
    id: SourceId::new("nexrad-level-2"),
    name: "NEXRAD Level II",
    domain: Domain::Radar,
    coverage: Coverage::Countries(&["US"]),
    home: "https://www.ncei.noaa.gov/products/radar/next-generation-weather-radar",
    endpoint: "https://unidata-nexrad-level2.s3.amazonaws.com",
    format: Format::Binary,
    licence: Some(US_GOVERNMENT),
    credit: "NOAA NEXRAD, distributed by Unidata",
    refresh: Refresh::Seconds(300),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// United States station metadata and active alerts.
pub const NWS_API: Source = Source {
    id: SourceId::new("nws-api"),
    name: "National Weather Service API",
    domain: Domain::Warnings,
    coverage: Coverage::Countries(&["US"]),
    home: "https://www.weather.gov",
    endpoint: "https://api.weather.gov",
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "National Weather Service",
    refresh: Refresh::Seconds(60),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Global earthquakes.
pub const USGS_EARTHQUAKES: Source = Source {
    id: SourceId::new("usgs-earthquakes"),
    name: "USGS earthquakes",
    domain: Domain::Seismicity,
    coverage: Coverage::Global,
    home: "https://earthquake.usgs.gov",
    endpoint: "https://earthquake.usgs.gov",
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "USGS Earthquake Hazards Program",
    refresh: Refresh::Seconds(60),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// United States river gauges.
pub const USGS_WATER: Source = Source {
    id: SourceId::new("usgs-water"),
    name: "USGS National Water Information System",
    domain: Domain::Hydrology,
    coverage: Coverage::Countries(&["US"]),
    home: "https://waterdata.usgs.gov",
    endpoint: "https://waterservices.usgs.gov",
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "USGS National Water Information System",
    refresh: Refresh::Seconds(900),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// NASA Global Imagery Browse Services.
pub const NASA_GIBS: Source = Source {
    id: SourceId::new("nasa-gibs"),
    name: "NASA GIBS",
    domain: Domain::Satellite,
    coverage: Coverage::Global,
    home: "https://www.earthdata.nasa.gov",
    endpoint: "https://gibs.earthdata.nasa.gov",
    format: Format::Tiles,
    licence: Some(Licence {
        name: "NASA open data, no key required",
        url: "https://nasa-gibs.github.io/gibs-api-docs/",
    }),
    credit: "NASA EOSDIS GIBS",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// NOAA space weather products.
pub const NOAA_SWPC: Source = Source {
    id: SourceId::new("noaa-swpc"),
    name: "NOAA Space Weather Prediction Center",
    domain: Domain::SpaceWeather,
    coverage: Coverage::Global,
    home: "https://www.swpc.noaa.gov",
    endpoint: "https://services.swpc.noaa.gov",
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "NOAA Space Weather Prediction Center",
    refresh: Refresh::Seconds(600),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Sunrise, sunset and solar noon.
pub const SUNRISE_SUNSET: Source = Source {
    id: SourceId::new("sunrise-sunset"),
    name: "Sunrise-Sunset.org",
    domain: Domain::Solar,
    coverage: Coverage::Global,
    home: "https://sunrise-sunset.org",
    endpoint: "https://api.sunrise-sunset.org/v2",
    format: Format::Json,
    licence: Some(Licence {
        name: "Free API, linked attribution required",
        url: "https://sunrise-sunset.org/api",
    }),
    credit: "Sun data by Sunrise-Sunset.org",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// OpenStreetMap place search.
pub const PHOTON: Source = Source {
    id: SourceId::new("photon"),
    name: "Photon place search",
    domain: Domain::Geocoding,
    coverage: Coverage::Global,
    home: "https://photon.komoot.io",
    endpoint: "https://photon.komoot.io",
    format: Format::GeoJson,
    licence: Some(Licence {
        name: "OpenStreetMap data, ODbL 1.0, fair use",
        url: "https://www.openstreetmap.org/copyright",
    }),
    credit: "Place search by Photon, data from OpenStreetMap contributors",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Global disaster alerts, including tropical cyclones.
pub const GDACS: Source = Source {
    id: SourceId::new("gdacs"),
    name: "GDACS disaster alerts",
    domain: Domain::Cyclone,
    coverage: Coverage::Global,
    home: "https://www.gdacs.org",
    endpoint: "https://www.gdacs.org",
    format: Format::GeoJson,
    licence: Some(Licence {
        name: "Free and open, attribution requested",
        url: "https://www.gdacs.org",
    }),
    credit: "GDACS, European Commission Joint Research Centre",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Worldwide aviation surface reports.
pub const AVIATION_WEATHER_METAR: Source = Source {
    id: SourceId::new("aviationweather-metar"),
    name: "Aviation Weather Center METAR",
    domain: Domain::Observation,
    coverage: Coverage::Extent("Worldwide where METAR is reported"),
    home: "https://aviationweather.gov",
    endpoint: "https://aviationweather.gov",
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "Aviation Weather Center METAR",
    refresh: Refresh::Seconds(600),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// United States wildfire perimeters.
pub const NIFC_WFIGS: Source = Source {
    id: SourceId::new("nifc-wfigs"),
    name: "NIFC WFIGS perimeters",
    domain: Domain::Fire,
    coverage: Coverage::Countries(&["US"]),
    home: "https://www.nifc.gov",
    endpoint: "https://services3.arcgis.com",
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "NIFC WFIGS interagency perimeters",
    refresh: Refresh::Seconds(1800),
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Arctic and Antarctic sea-ice extent.
pub const NSIDC_SEA_ICE: Source = Source {
    id: SourceId::new("nsidc-sea-ice"),
    name: "NSIDC Sea Ice Index",
    domain: Domain::Ocean,
    coverage: Coverage::Global,
    home: "https://nsidc.org/data/g02135",
    endpoint: "https://noaadata.apps.nsidc.org",
    format: Format::Binary,
    licence: Some(US_GOVERNMENT),
    credit: "NSIDC Sea Ice Index, National Snow and Ice Data Center",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// United States volcano activity notices.
pub const USGS_VOLCANO: Source = Source {
    id: SourceId::new("usgs-volcano"),
    name: "USGS Volcano Hazards Program",
    domain: Domain::Volcano,
    coverage: Coverage::Extent("United States volcanoes and territories"),
    home: "https://volcanoes.usgs.gov",
    endpoint: "https://volcanoes.usgs.gov",
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "USGS Volcano Hazards Program",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

// Candidates below have not had their terms verified against the provider.
// `admits` rejects every one of them until a licence is recorded, so none can
// reach the interface by accident.

/// German national radar and forecast open data.
pub const DWD_OPEN_DATA: Source = Source {
    id: SourceId::new("dwd-open-data"),
    name: "Deutscher Wetterdienst open data",
    domain: Domain::Radar,
    coverage: Coverage::Countries(&["DE"]),
    home: "https://www.dwd.de",
    endpoint: "https://opendata.dwd.de",
    format: Format::Binary,
    licence: None,
    credit: "Deutscher Wetterdienst",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// Canadian radar and weather layers.
pub const ECCC_GEOMET: Source = Source {
    id: SourceId::new("eccc-geomet"),
    name: "Environment and Climate Change Canada GeoMet",
    domain: Domain::Radar,
    coverage: Coverage::Countries(&["CA"]),
    home: "https://eccc-msc.github.io/open-data/",
    endpoint: "https://api.weather.gc.ca",
    format: Format::GeoJson,
    licence: None,
    credit: "Environment and Climate Change Canada",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    // Probed 2026-11-13: the collections listing answered 200 application/json.
    level: Level::Probed,
};

/// Australian national radar.
pub const BOM_RADAR: Source = Source {
    id: SourceId::new("bom-radar"),
    name: "Australian Bureau of Meteorology radar",
    domain: Domain::Radar,
    coverage: Coverage::Countries(&["AU"]),
    home: "https://www.bom.gov.au",
    endpoint: "https://www.bom.gov.au",
    format: Format::Binary,
    licence: None,
    credit: "Australian Bureau of Meteorology",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// European warning aggregation.
pub const METEOALARM: Source = Source {
    id: SourceId::new("meteoalarm"),
    name: "MeteoAlarm",
    domain: Domain::Warnings,
    coverage: Coverage::Extent("Europe"),
    home: "https://www.meteoalarm.org",
    endpoint: "https://www.meteoalarm.org",
    format: Format::Xml,
    licence: None,
    credit: "MeteoAlarm, EUMETNET",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// European-Mediterranean earthquakes.
pub const EMSC: Source = Source {
    id: SourceId::new("emsc"),
    name: "European-Mediterranean Seismological Centre",
    domain: Domain::Seismicity,
    coverage: Coverage::Global,
    home: "https://www.seismicportal.eu",
    endpoint: "https://www.seismicportal.eu",
    format: Format::GeoJson,
    licence: None,
    credit: "European-Mediterranean Seismological Centre",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// Norwegian global forecast.
pub const MET_NORWAY: Source = Source {
    id: SourceId::new("met-norway"),
    name: "Norwegian Meteorological Institute",
    domain: Domain::Forecast,
    coverage: Coverage::Global,
    home: "https://www.met.no",
    endpoint: "https://api.met.no",
    format: Format::Json,
    licence: None,
    credit: "Norwegian Meteorological Institute",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    // Probed 2026-11-13: the compact forecast answered 200 application/json.
    level: Level::Probed,
};

/// Global elevation.
pub const COPERNICUS_DEM: Source = Source {
    id: SourceId::new("copernicus-dem"),
    name: "Copernicus DEM",
    domain: Domain::Terrain,
    coverage: Coverage::Global,
    home: "https://dataspace.copernicus.eu",
    endpoint: "https://copernicus-dem-30m.s3.amazonaws.com",
    format: Format::Binary,
    licence: None,
    credit: "Copernicus DEM, European Space Agency",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// Global volcano activity.
pub const SMITHSONIAN_GVP: Source = Source {
    id: SourceId::new("smithsonian-gvp"),
    name: "Smithsonian Global Volcanism Program",
    domain: Domain::Volcano,
    coverage: Coverage::Global,
    home: "https://volcano.si.edu",
    endpoint: "https://volcano.si.edu",
    format: Format::Xml,
    licence: None,
    credit: "Smithsonian Global Volcanism Program",
    refresh: Refresh::ProviderDefined,
    cache: CachePermission::Permitted,
    level: Level::Documented,
};

/// Every recorded source, admitted or not.
pub const SOURCES: &[Source] = &[
    OPENFREEMAP,
    OPEN_METEO_FORECAST,
    OPEN_METEO_ARCHIVE,
    OPEN_METEO_AIR,
    OPEN_METEO_MARINE,
    RAINVIEWER,
    NEXRAD_LEVEL_TWO,
    NWS_API,
    USGS_EARTHQUAKES,
    USGS_WATER,
    NASA_GIBS,
    NOAA_SWPC,
    SUNRISE_SUNSET,
    PHOTON,
    GDACS,
    AVIATION_WEATHER_METAR,
    NIFC_WFIGS,
    NSIDC_SEA_ICE,
    USGS_VOLCANO,
    DWD_OPEN_DATA,
    ECCC_GEOMET,
    BOM_RADAR,
    METEOALARM,
    EMSC,
    MET_NORWAY,
    COPERNICUS_DEM,
    SMITHSONIAN_GVP,
];

/// Looks up a source by its registry identifier.
pub fn find(id: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.id.as_str() == id)
}

/// Renders the source inventory from the registry.
///
/// The checked-in table is this output, never hand-written, so a new provider
/// cannot appear in the documentation without appearing in the code.
pub fn inventory_markdown() -> Result<String, fmt::Error> {
    let mut out = String::new();
    out.push_str(
        "# Open sources\n\n\
         Generated by `oxrad --sources` from `src/sources.rs`. A source carries \
         the licence, credit line, coverage, cadence and cache permission it is \
         recorded with; a source that fails admission says why.\n\n\
         Levels: documented (found, never fetched), probed (one keyless request \
         answered), parsed (our code produced typed data, with a test), admitted \
         (parsed, licensed, credited and displayed).\n\n",
    );
    out.push_str(
        "| id | name | domain | coverage | format | endpoint | licence | credit | refresh | cache | level | admission |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for source in SOURCES {
        writeln!(
            &mut out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            source.id.as_str(),
            source.name,
            source.domain.label(),
            source.coverage.label(),
            source.format.label(),
            source.endpoint,
            source.licence.map_or("unverified", |licence| licence.name),
            source.credit,
            source.refresh.label(),
            source.cache.label(),
            source.level.label(),
            source.admission_label(),
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn country_codes_are_valid(codes: &[&str]) -> bool {
        let shape = codes
            .iter()
            .all(|code| code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_uppercase()));
        let order = codes
            .iter()
            .zip(codes.iter().skip(1))
            .all(|(first, second)| first < second);
        shape && order
    }

    #[test]
    fn every_identifier_is_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for source in SOURCES {
            assert!(
                !seen.contains(&source.id.as_str()),
                "duplicate source id {}",
                source.id.as_str()
            );
            seen.push(source.id.as_str());
        }
    }

    #[test]
    fn country_codes_are_sorted_and_uppercase() {
        for source in SOURCES {
            if let Coverage::Countries(codes) = source.coverage {
                assert!(
                    country_codes_are_valid(codes),
                    "{} has malformed coverage {codes:?}",
                    source.id.as_str()
                );
            }
        }
    }

    #[test]
    fn every_admitted_source_passes_the_test() {
        let mut admitted = 0;
        for source in SOURCES {
            if source.admits().is_ok() {
                admitted += 1;
                assert!(
                    source.level.is_publishable(),
                    "{} admits but was never parsed",
                    source.id.as_str()
                );
            }
        }
        assert!(admitted >= 15, "only {admitted} sources admit");
    }

    #[test]
    fn a_keyed_endpoint_is_rejected() {
        let keyed = Source {
            endpoint: "https://example.test/data?api_key=secret",
            licence: Some(CC_BY),
            ..OPEN_METEO_FORECAST
        };
        assert!(matches!(
            keyed.admits(),
            Err(AdmissionError::Credential {
                marker: "api_key=",
                ..
            })
        ));
    }

    #[test]
    fn an_unlicensed_source_is_rejected() {
        assert!(matches!(
            DWD_OPEN_DATA.admits(),
            Err(AdmissionError::NoLicence { .. })
        ));
    }

    #[test]
    fn an_insecure_endpoint_is_rejected() {
        let insecure = Source {
            endpoint: "http://example.test/data",
            ..OPEN_METEO_FORECAST
        };
        assert!(matches!(
            insecure.admits(),
            Err(AdmissionError::Insecure { .. })
        ));
    }

    #[test]
    fn a_documented_source_cannot_be_published() {
        assert!(!Level::Documented.is_publishable());
        assert!(Level::Parsed.is_publishable());
        assert!(RAINVIEWER.level.is_publishable());
    }

    #[test]
    fn the_inventory_names_every_source() -> Result<(), fmt::Error> {
        let inventory = inventory_markdown()?;
        for source in SOURCES {
            assert!(
                inventory.contains(source.id.as_str()),
                "the inventory omits {}",
                source.id.as_str()
            );
            assert!(
                inventory.contains(source.credit),
                "the inventory omits the credit for {}",
                source.id.as_str()
            );
        }
        Ok(())
    }

    #[test]
    fn the_checked_in_inventory_is_the_rendered_one() {
        let rendered = inventory_markdown();
        assert!(rendered.is_ok(), "the inventory could not be rendered");
        if let Ok(text) = rendered {
            assert_eq!(
                include_str!("../docs/SOURCES.md"),
                text,
                "run `oxrad --sources > docs/SOURCES.md`"
            );
        }
    }

    #[test]
    fn the_sun_credit_is_the_registry_entry() {
        assert_eq!(SUNRISE_SUNSET.credit, "Sun data by Sunrise-Sunset.org");
        assert_eq!(SUNRISE_SUNSET.home, "https://sunrise-sunset.org");
    }
}
