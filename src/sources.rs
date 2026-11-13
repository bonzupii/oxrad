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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// One named request the registry can produce.
#[derive(Debug, Clone, Copy)]
pub struct Endpoint {
    /// The stable operation name a module asks for.
    pub operation: &'static str,
    /// What the response holds, in the reader's terms.
    pub subject: &'static str,
    /// The complete URL, with `{name}` query values and `{raw:name}` path
    /// values. The registry holds every literal, separator and route.
    pub template: &'static str,
    /// The Accept header this operation needs.
    pub accept: &'static str,
    /// Whether the host is supplied by a manifest value rather than a literal.
    /// The module that supplies it validates the scheme before the request.
    pub host_from_value: bool,
    /// An operation-specific cache lifetime, when it differs from the source's
    /// refresh cadence. The engine owns using it.
    pub ttl_seconds: Option<u64>,
}

impl Endpoint {
    pub const fn new(
        operation: &'static str,
        subject: &'static str,
        template: &'static str,
        accept: &'static str,
    ) -> Self {
        Self {
            operation,
            subject,
            template,
            accept,
            host_from_value: false,
            ttl_seconds: None,
        }
    }

    /// Sets an operation-specific cache lifetime in seconds.
    pub const fn ttl(mut self, seconds: u64) -> Self {
        self.ttl_seconds = Some(seconds);
        self
    }

    pub const fn with_host_value(
        operation: &'static str,
        subject: &'static str,
        template: &'static str,
        accept: &'static str,
    ) -> Self {
        Self {
            operation,
            subject,
            template,
            accept,
            host_from_value: true,
            ttl_seconds: None,
        }
    }

    /// Produces the request URL from this operation's template.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError`] when the template is malformed, a value is
    /// missing or empty, or a supplied value has no placeholder.
    pub fn build(&self, values: &[(&str, &str)]) -> Result<String, EndpointError> {
        let mut url = String::with_capacity(self.template.len() + 64);
        let mut used: Vec<&str> = Vec::new();
        let mut rest = self.template;
        while let Some(open) = rest.find('{') {
            url.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                return Err(EndpointError::Unclosed {
                    operation: self.operation,
                });
            };
            let key = &after[..close];
            let (name, raw) = key
                .strip_prefix("raw:")
                .map_or((key, false), |name| (name, true));
            let Some((_, value)) = values.iter().find(|(candidate, _)| *candidate == name) else {
                return Err(EndpointError::MissingValue {
                    operation: self.operation,
                    variable: String::from(name),
                });
            };
            if value.is_empty() {
                return Err(EndpointError::EmptyValue {
                    operation: self.operation,
                    variable: String::from(name),
                });
            }
            used.push(name);
            if raw {
                if value.contains('{') || value.contains('}') {
                    return Err(EndpointError::InvalidValue {
                        operation: self.operation,
                        variable: String::from(name),
                    });
                }
                url.push_str(value);
            } else {
                url.push_str(&crate::text::encode_component(value));
            }
            rest = &after[close + 1..];
        }
        url.push_str(rest);
        if let Some((name, _)) = values
            .iter()
            .find(|(candidate, _)| !used.contains(candidate))
        {
            return Err(EndpointError::UnusedValue {
                operation: self.operation,
                variable: String::from(*name),
            });
        }
        Ok(url)
    }

    /// Whether the operation needs no caller-supplied values.
    pub fn is_static(&self) -> bool {
        !self.template.contains('{')
    }
}

/// Why a registry endpoint could not produce a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    UnknownOperation {
        source: SourceId,
        operation: &'static str,
    },
    Unclosed {
        operation: &'static str,
    },
    MissingValue {
        operation: &'static str,
        variable: String,
    },
    EmptyValue {
        operation: &'static str,
        variable: String,
    },
    InvalidValue {
        operation: &'static str,
        variable: String,
    },
    UnusedValue {
        operation: &'static str,
        variable: String,
    },
}

impl Display for EndpointError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation { source, operation } => write!(
                formatter,
                "{} has no operation named {operation}",
                source.as_str()
            ),
            Self::Unclosed { operation } => {
                write!(formatter, "{operation} has an unclosed placeholder")
            }
            Self::MissingValue {
                operation,
                variable,
            } => write!(formatter, "{operation} needs a value for {variable}"),
            Self::EmptyValue {
                operation,
                variable,
            } => write!(formatter, "{operation} received an empty {variable}"),
            Self::InvalidValue {
                operation,
                variable,
            } => write!(formatter, "{operation} received an invalid {variable}"),
            Self::UnusedValue {
                operation,
                variable,
            } => write!(formatter, "{operation} received an unused {variable}"),
        }
    }
}

impl Error for EndpointError {}

/// One provider, recorded once.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub id: SourceId,
    pub name: &'static str,
    pub domain: Domain,
    pub coverage: Coverage,
    /// The human page a reader can visit.
    pub home: &'static str,
    /// Every named request this source publishes.
    pub endpoints: &'static [Endpoint],
    pub format: Format,
    pub licence: Option<Licence>,
    /// The credit line the interface must show verbatim.
    pub credit: &'static str,
    pub refresh: Refresh,
    /// The smallest humane interval between two requests to this source, in
    /// seconds. The request engine enforces it for every caller.
    pub rate_limit_seconds: u32,
    pub cache: CachePermission,
    pub level: Level,
}

impl Source {
    /// Looks up one named operation.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownOperation`] when the name is not recorded.
    pub fn operation(&self, operation: &'static str) -> Result<&'static Endpoint, EndpointError> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.operation == operation)
            .ok_or(EndpointError::UnknownOperation {
                source: self.id,
                operation,
            })
    }

    /// The first operation whose URL needs no values, for a headless probe.
    pub fn static_operation(&self) -> Option<&'static Endpoint> {
        self.endpoints.iter().find(|endpoint| endpoint.is_static())
    }

    /// Whether this source may be used at all.
    pub fn admits(&self) -> Result<(), AdmissionError> {
        if self.endpoints.is_empty() {
            return Err(AdmissionError::NoEndpoint { id: self.id });
        }
        for endpoint in self.endpoints {
            if endpoint.operation.is_empty()
                || endpoint.subject.is_empty()
                || endpoint.accept.is_empty()
            {
                return Err(AdmissionError::EmptyEndpoint { id: self.id });
            }
            let secure = endpoint.template.starts_with(HTTPS_SCHEME)
                || (endpoint.host_from_value && endpoint.template.starts_with("{raw:host}"));
            if !secure {
                return Err(AdmissionError::Insecure {
                    id: self.id,
                    endpoint: endpoint.template,
                });
            }
            if let Some(marker) = credential_marker(endpoint.template) {
                return Err(AdmissionError::Credential {
                    id: self.id,
                    marker,
                });
            }
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
    for marker in CREDENTIAL_MARKERS {
        let mut offset = 0;
        while let Some(found) = endpoint[offset..].find(marker) {
            let absolute = offset + found;
            let boundary =
                absolute == 0 || matches!(endpoint.as_bytes().get(absolute - 1), Some(b'?' | b'&'));
            if boundary {
                return Some(marker);
            }
            offset = absolute + marker.len();
        }
    }
    None
}

/// Why a source cannot be admitted.
#[derive(Debug)]
pub enum AdmissionError {
    NoEndpoint {
        id: SourceId,
    },
    EmptyEndpoint {
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
            Self::EmptyEndpoint { id } => {
                write!(formatter, "{} has an incomplete endpoint", id.as_str())
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

/// The transport scheme every registry endpoint carries.
pub const HTTPS_SCHEME: &str = "https://";

/// The honest User-Agent the engine sends with every request.
pub const USER_AGENT: &str = concat!(
    "oxrad/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/bonzupii/oxrad)"
);

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
/// The basemap tile template, kept beside the endpoint that declares it so the
/// tile grid can build a provider URL without composing one itself.
pub const BASEMAP_TILE_TEMPLATE: &str =
    "https://tiles.openfreemap.org/planet/latest/{zoom}/{x}/{y}.pbf";

/// Builds a basemap tile URL from the registry template.
pub fn basemap_tile_url(zoom: u8, x: u32, y: u32) -> String {
    BASEMAP_TILE_TEMPLATE
        .replace("{zoom}", &zoom.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string())
}

pub const OPENFREEMAP: Source = Source {
    id: SourceId::new("openfreemap"),
    name: "OpenFreeMap",
    domain: Domain::Basemap,
    coverage: Coverage::Global,
    home: "https://openfreemap.org",
    endpoints: &[
        Endpoint::new(
            "style",
            "basemap style",
            "https://tiles.openfreemap.org/styles/{style}",
            "application/json",
        ),
        Endpoint::new(
            "tile",
            "basemap vector tile",
            BASEMAP_TILE_TEMPLATE,
            "application/vnd.mapbox-vector-tile",
        )
        .ttl(86_400),
    ],
    format: Format::VectorTiles,
    licence: Some(OSM_COPYRIGHT),
    credit: "OpenFreeMap, OpenMapTiles, OpenStreetMap contributors",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "forecast",
            "point forecast",
            "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m,wind_direction_10m&hourly=temperature_2m,precipitation_probability,weather_code,wind_speed_10m&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm&timezone=auto&forecast_days={forecast_days}",
            "application/json",
        ),
        Endpoint::new(
            "environment",
            "convective environment",
            "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&hourly=cape,convective_inhibition,lifted_index,freezing_level_height,wind_speed_10m,wind_direction_10m,wind_speed_850hPa,wind_direction_850hPa,wind_speed_500hPa,wind_direction_500hPa&forecast_days={forecast_days}&timezone=UTC",
            "application/json",
        ),
        Endpoint::new(
            "land",
            "land surface",
            "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&hourly=soil_moisture_0_to_10cm,soil_moisture_10_to_40cm,soil_temperature_6cm,et0_fao_evapotranspiration,vapour_pressure_deficit,shortwave_radiation,direct_normal_irradiance,uv_index&past_days={past_days}&forecast_days={forecast_days}&timezone=UTC",
            "application/json",
        ),
        Endpoint::new(
            "snow",
            "snow model",
            "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&hourly=snow_depth,snowfall&past_days={past_days}&forecast_days={forecast_days}&timezone=UTC",
            "application/json",
        ),
        Endpoint::new(
            "sounding",
            "pressure-level profile",
            "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&hourly={hourly}&wind_speed_unit=ms&forecast_days=1&timezone=UTC",
            "application/json",
        ),
    ],
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Weather data by Open-Meteo.com",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "climate",
        "ERA5 climate record",
        "https://archive-api.open-meteo.com/v1/archive?latitude={latitude}&longitude={longitude}&start_date={start_date}&end_date={end_date}&daily=temperature_2m_mean,precipitation_sum&timezone=UTC",
        "application/json",
    )],
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Weather data by Open-Meteo.com, ERA5 from ECMWF",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "quality",
        "CAMS air quality",
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={latitude}&longitude={longitude}&current=us_aqi,european_aqi,pm2_5,pm10,ozone,nitrogen_dioxide,sulphur_dioxide,carbon_monoxide&hourly=pm2_5,us_aqi&timezone=UTC&past_days={past_days}&forecast_days={forecast_days}",
        "application/json",
    )],
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Air quality data by Open-Meteo.com, CAMS",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "ocean",
        "marine surface",
        "https://marine-api.open-meteo.com/v1/marine?latitude={latitude}&longitude={longitude}&hourly=wave_height,wave_direction,wave_period,swell_wave_height,sea_surface_temperature,ocean_current_velocity,ocean_current_direction&forecast_days={forecast_days}&timezone=UTC&cell_selection=sea",
        "application/json",
    )],
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "Marine data by Open-Meteo.com",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
    cache: CachePermission::Permitted,
    level: Level::Parsed,
};

/// Open-Meteo GloFAS river discharge.
pub const OPEN_METEO_FLOOD: Source = Source {
    id: SourceId::new("open-meteo-flood"),
    name: "Open-Meteo river discharge, GloFAS",
    domain: Domain::Hydrology,
    coverage: Coverage::Global,
    home: "https://open-meteo.com/en/docs/flood-api",
    endpoints: &[
        Endpoint::new(
            "hydro",
            "GloFAS discharge",
            "https://flood-api.open-meteo.com/v1/flood?latitude={latitude}&longitude={longitude}&daily=river_discharge&past_days={past_days}&forecast_days={forecast_days}",
            "application/json",
        ),
        Endpoint::new(
            "hydro-ensemble",
            "GloFAS ensemble discharge",
            "https://flood-api.open-meteo.com/v1/flood?latitude={latitude}&longitude={longitude}&daily=river_discharge&past_days={past_days}&forecast_days={forecast_days}&ensemble=true",
            "application/json",
        ),
    ],
    format: Format::Json,
    licence: Some(CC_BY),
    credit: "River discharge by Open-Meteo.com, GloFAS",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "manifest",
            "global radar manifest",
            "https://api.rainviewer.com/public/weather-maps.json",
            "application/json",
        ),
        Endpoint::with_host_value(
            "tile",
            "global radar tile",
            "{raw:host}{raw:path}/{size}/{zoom}/{x}/{y}/{scheme}/1_1.png",
            "image/png",
        )
        .ttl(3_600),
    ],
    format: Format::Tiles,
    licence: Some(Licence {
        name: "RainViewer free terms, attribution required",
        url: "https://www.rainviewer.com/api.html",
    }),
    credit: "Weather data by RainViewer",
    refresh: Refresh::Seconds(600),
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "list",
            "Level II archive listing",
            "https://unidata-nexrad-level2.s3.amazonaws.com?list-type=2&prefix={raw:prefix}",
            "application/xml",
        )
        .ttl(60),
        Endpoint::new(
            "list-page",
            "Level II archive listing page",
            "https://unidata-nexrad-level2.s3.amazonaws.com?list-type=2&prefix={raw:prefix}&continuation-token={continuation}",
            "application/xml",
        )
        .ttl(60),
        Endpoint::new(
            "object",
            "Level II archive object",
            "https://unidata-nexrad-level2.s3.amazonaws.com/{raw:key}",
            "application/octet-stream",
        )
        .ttl(31_536_000),
    ],
    format: Format::Binary,
    licence: Some(US_GOVERNMENT),
    credit: "NOAA NEXRAD, distributed by Unidata",
    refresh: Refresh::Seconds(300),
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "stations",
            "radar station catalog",
            "https://api.weather.gov/radar/stations",
            "application/geo+json",
        ),
        Endpoint::new(
            "alerts",
            "active warnings",
            "https://api.weather.gov/alerts/active?status=actual&message_type=alert",
            "application/geo+json",
        ),
    ],
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "National Weather Service",
    refresh: Refresh::Seconds(60),
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "feed",
        "earthquake feed",
        "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/{window}.geojson",
        "application/geo+json",
    )],
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "USGS Earthquake Hazards Program",
    refresh: Refresh::Seconds(60),
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "box",
            "river gauges in a box",
            "https://waterservices.usgs.gov/nwis/iv/?format=json&bBox={bbox}&parameterCd=00065,00060&siteStatus=all",
            "application/json",
        ),
        Endpoint::new(
            "history",
            "river gauge history",
            "https://waterservices.usgs.gov/nwis/iv/?format=json&sites={site}&parameterCd=00065,00060&period=P7D",
            "application/json",
        ),
    ],
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "USGS National Water Information System",
    refresh: Refresh::Seconds(900),
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "tile",
        "NASA GIBS tile",
        "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/wmts.cgi?SERVICE=WMTS&REQUEST=GetTile&VERSION=1.0.0&LAYER={layer}&STYLE=default&TILEMATRIXSET=GoogleMapsCompatible_Level{level}&TILEMATRIX={zoom}&TILEROW={y}&TILECOL={x}&FORMAT={raw:format}&TIME={raw:time}",
        "image/*",
    )
    .ttl(86_400)],
    format: Format::Tiles,
    licence: Some(Licence {
        name: "NASA open data, no key required",
        url: "https://nasa-gibs.github.io/gibs-api-docs/",
    }),
    credit: "NASA EOSDIS GIBS",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "kp",
            "planetary K index",
            "https://services.swpc.noaa.gov/products/noaa-planetary-k-index-forecast.json",
            "application/json",
        ),
        Endpoint::new(
            "wind",
            "solar wind speed",
            "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json",
            "application/json",
        ),
        Endpoint::new(
            "magnetic-field",
            "solar wind magnetic field",
            "https://services.swpc.noaa.gov/products/summary/solar-wind-mag-field.json",
            "application/json",
        ),
    ],
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "NOAA Space Weather Prediction Center",
    refresh: Refresh::Seconds(600),
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "day",
        "sunrise and sunset",
        "https://api.sunrise-sunset.org/v2?lat={latitude}&lng={longitude}",
        "application/json",
    )],
    format: Format::Json,
    licence: Some(Licence {
        name: "Free API, linked attribution required",
        url: "https://sunrise-sunset.org/api",
    }),
    credit: "Sun data by Sunrise-Sunset.org",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "search",
        "place search",
        "https://photon.komoot.io/api/?q={query}&limit={limit}",
        "application/json",
    )],
    format: Format::GeoJson,
    licence: Some(Licence {
        name: "OpenStreetMap data, ODbL 1.0, fair use",
        url: "https://www.openstreetmap.org/copyright",
    }),
    credit: "Place search by Photon, data from OpenStreetMap contributors",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "search",
        "tropical cyclones",
        "https://www.gdacs.org/gdacsapi/api/events/geteventlist/SEARCH?eventlist=TC&fromdate={raw:from_date}&todate={raw:to_date}",
        "application/json",
    )],
    format: Format::GeoJson,
    licence: Some(Licence {
        name: "Free and open, attribution requested",
        url: "https://www.gdacs.org",
    }),
    credit: "GDACS, European Commission Joint Research Centre",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "box",
        "surface observations",
        "https://aviationweather.gov/api/data/metar?bbox={bbox}&format=json&hours={hours}",
        "application/json",
    )],
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "Aviation Weather Center METAR",
    refresh: Refresh::Seconds(600),
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "perimeters",
        "current wildfire perimeters",
        "https://services3.arcgis.com/T4QMspbfLg3qTGWY/arcgis/rest/services/WFIGS_Interagency_Perimeters_Current/FeatureServer/0/query?where=1%3D1&outFields=poly_IncidentName,poly_GISAcres,attr_PercentContained,attr_IncidentTypeCategory,attr_POOState,poly_PolygonDateTime&outSR=4326&geometryPrecision=4&maxAllowableOffset=0.01&f=geojson",
        "application/json",
    )],
    format: Format::GeoJson,
    licence: Some(US_GOVERNMENT),
    credit: "NIFC WFIGS interagency perimeters",
    refresh: Refresh::Seconds(1800),
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "month",
        "sea-ice extent",
        "https://noaadata.apps.nsidc.org/NOAA/G02135/{hemisphere}/monthly/data/{code}_{month}_extent_v4.0.csv",
        "text/csv",
    )],
    format: Format::Binary,
    licence: Some(US_GOVERNMENT),
    credit: "NSIDC Sea Ice Index, National Snow and Ice Data Center",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[
        Endpoint::new(
            "list",
            "raised volcanoes",
            "https://volcanoes.usgs.gov/hans-public/api/volcano/getElevatedVolcanoes",
            "application/json",
        ),
        Endpoint::new(
            "volcano",
            "volcano record",
            "https://volcanoes.usgs.gov/hans-public/api/volcano/getVolcano/{vnum}",
            "application/json",
        ),
        Endpoint::new(
            "notice",
            "volcano notice",
            "https://volcanoes.usgs.gov/hans-public/api/notice/getNotice/{notice}",
            "application/json",
        ),
    ],
    format: Format::Json,
    licence: Some(US_GOVERNMENT),
    credit: "USGS Volcano Hazards Program",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "DWD candidate site",
        "https://opendata.dwd.de",
        "*/*",
    )],
    format: Format::Binary,
    licence: None,
    credit: "Deutscher Wetterdienst",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "ECCC candidate site",
        "https://api.weather.gc.ca",
        "*/*",
    )],
    format: Format::GeoJson,
    licence: None,
    credit: "Environment and Climate Change Canada",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "BOM candidate site",
        "https://www.bom.gov.au",
        "*/*",
    )],
    format: Format::Binary,
    licence: None,
    credit: "Australian Bureau of Meteorology",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "MeteoAlarm candidate site",
        "https://www.meteoalarm.org",
        "*/*",
    )],
    format: Format::Xml,
    licence: None,
    credit: "MeteoAlarm, EUMETNET",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "EMSC candidate site",
        "https://www.seismicportal.eu",
        "*/*",
    )],
    format: Format::GeoJson,
    licence: None,
    credit: "European-Mediterranean Seismological Centre",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "MET Norway candidate site",
        "https://api.met.no",
        "*/*",
    )],
    format: Format::Json,
    licence: None,
    credit: "Norwegian Meteorological Institute",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "Copernicus DEM candidate site",
        "https://copernicus-dem-30m.s3.amazonaws.com",
        "*/*",
    )],
    format: Format::Binary,
    licence: None,
    credit: "Copernicus DEM, European Space Agency",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    endpoints: &[Endpoint::new(
        "site",
        "Smithsonian candidate site",
        "https://volcano.si.edu",
        "*/*",
    )],
    format: Format::Xml,
    licence: None,
    credit: "Smithsonian Global Volcanism Program",
    refresh: Refresh::ProviderDefined,
    rate_limit_seconds: 1,
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
    OPEN_METEO_FLOOD,
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

/// The attribution link the basemap credit points at, owned by the licence.
pub fn openfreemap_attribution_url() -> &'static str {
    OPENFREEMAP
        .licence
        .map_or(OPENFREEMAP.home, |licence| licence.url)
}

/// Looks up a source by its registry identifier.
pub fn find(id: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.id.as_str() == id)
}

fn endpoint_list(source: &Source) -> String {
    source
        .endpoints
        .iter()
        .map(|endpoint| {
            endpoint.ttl_seconds.map_or_else(
                || format!("{}: {}", endpoint.operation, endpoint.template),
                |seconds| {
                    format!(
                        "{}: {} ({} s)",
                        endpoint.operation, endpoint.template, seconds
                    )
                },
            )
        })
        .collect::<Vec<String>>()
        .join("<br>")
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
        "| id | name | domain | coverage | format | operations | licence | credit | refresh | rate | cache | level | admission |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for source in SOURCES {
        writeln!(
            &mut out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            source.id.as_str(),
            source.name,
            source.domain.label(),
            source.coverage.label(),
            source.format.label(),
            endpoint_list(source),
            source.licence.map_or("unverified", |licence| licence.name),
            source.credit,
            source.refresh.label(),
            source.rate_limit_seconds,
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

    const KEYED_ENDPOINTS: &[Endpoint] = &[Endpoint::new(
        "read",
        "test read",
        "https://example.test/data?api_key=secret",
        "application/json",
    )];
    const INSECURE_ENDPOINTS: &[Endpoint] = &[Endpoint::new(
        "read",
        "test read",
        "http://example.test/data",
        "application/json",
    )];

    #[test]
    fn a_keyed_endpoint_is_rejected() {
        let keyed = Source {
            endpoints: KEYED_ENDPOINTS,
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
            endpoints: INSECURE_ENDPOINTS,
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

    fn sample_value(name: &str) -> Option<&'static str> {
        Some(match name {
            "latitude" => "39.025",
            "longitude" => "-96.225",
            "forecast_days" => "2",
            "past_days" => "1",
            "start_date" | "end_date" => "2026-01-01",
            "style" => "positron",
            "host" => "https://tilecache.rainviewer.com",
            "path" => "/v2/radar/1760000000",
            "size" => "512",
            "zoom" => "3",
            "x" => "1",
            "y" => "2",
            "scheme" => "2",
            "prefix" => "2026/11/13/KTWX/",
            "continuation" => "abc+def/=",
            "key" => "2026/11/13/KTWX/KTWX20261113_000000_V06",
            "window" => "2.5_day",
            "bbox" => "37.0000,-98.0000,38.0000,-97.0000",
            "site" => "07144570",
            "layer" => "MODIS_Terra_CorrectedReflectance_TrueColor",
            "level" => "9",
            "format" => "image%2Fjpeg",
            "time" => "2026-11-13",
            "hourly" => "temperature_2m",
            "query" => "Emporia, KS",
            "limit" => "8",
            "from_date" => "11/01/2026",
            "to_date" => "11/13/2026",
            "hours" => "2",
            "hemisphere" => "north",
            "code" => "N",
            "month" => "11",
            "vnum" => "332010",
            "notice" => "abc123",
            _ => return None,
        })
    }

    fn placeholder_names(template: &str) -> Result<Vec<&str>, ()> {
        let mut names = Vec::new();
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                return Err(());
            };
            let key = &after[..close];
            names.push(key.strip_prefix("raw:").unwrap_or(key));
            rest = &after[close + 1..];
        }
        Ok(names)
    }

    fn values_for(template: &str) -> Result<Vec<(&str, &'static str)>, ()> {
        placeholder_names(template)?
            .into_iter()
            .map(|name| sample_value(name).map(|value| (name, value)).ok_or(()))
            .collect()
    }

    fn endpoint(source: &str, operation: &str) -> &'static Endpoint {
        let Some(found) = find(source) else {
            panic!("unknown source {source}");
        };
        let Some(endpoint) = found
            .endpoints
            .iter()
            .find(|endpoint| endpoint.operation == operation)
        else {
            panic!("{source} has no {operation} operation");
        };
        endpoint
    }

    fn built(source: &str, operation: &str) -> String {
        let endpoint = endpoint(source, operation);
        let values = match values_for(endpoint.template) {
            Ok(values) => values,
            Err(()) => panic!("{source}/{operation} has an unnamed placeholder"),
        };
        match endpoint.build(&values) {
            Ok(url) => url,
            Err(error) => panic!("{source}/{operation} did not build: {error}"),
        }
    }

    #[test]
    fn every_admitted_operation_builds_a_complete_url() {
        let mut admitted = 0;
        for source in SOURCES {
            if source.admits().is_err() {
                continue;
            }
            admitted += 1;
            let mut seen = Vec::new();
            for endpoint in source.endpoints {
                assert!(
                    !seen.contains(&endpoint.operation),
                    "{} repeats operation {}",
                    source.id.as_str(),
                    endpoint.operation
                );
                seen.push(endpoint.operation);
                let values = match values_for(endpoint.template) {
                    Ok(values) => values,
                    Err(()) => panic!(
                        "{}/{} has a placeholder with no generated sample",
                        source.id.as_str(),
                        endpoint.operation
                    ),
                };
                let url = match endpoint.build(&values) {
                    Ok(url) => url,
                    Err(error) => panic!(
                        "{}/{} did not build: {error}",
                        source.id.as_str(),
                        endpoint.operation
                    ),
                };
                assert!(
                    url.starts_with("https://"),
                    "{}/{} is not HTTPS: {url}",
                    source.id.as_str(),
                    endpoint.operation
                );
                assert!(
                    !url.contains('{') && !url.contains('}'),
                    "{}/{} left a variable: {url}",
                    source.id.as_str(),
                    endpoint.operation
                );
                assert!(
                    !url.contains(' ') && !url.contains('\n'),
                    "{}/{} has whitespace: {url}",
                    source.id.as_str(),
                    endpoint.operation
                );
                let after_scheme = &url["https://".len()..];
                assert!(
                    !after_scheme.contains("//"),
                    "{}/{} doubled a separator: {url}",
                    source.id.as_str(),
                    endpoint.operation
                );
                assert!(
                    !url.contains("&&") && !url.contains("?&") && !url.contains("=&"),
                    "{}/{} has an empty parameter: {url}",
                    source.id.as_str(),
                    endpoint.operation
                );
                assert!(
                    endpoint.subject.len() > 3 && endpoint.accept.contains('/'),
                    "{}/{} has incomplete metadata",
                    source.id.as_str(),
                    endpoint.operation
                );
            }
            assert!(!seen.is_empty(), "{} has no operations", source.id.as_str());
        }
        assert!(admitted >= 15, "only {admitted} sources admit");
    }

    #[test]
    fn former_module_composed_urls_are_exact() {
        assert_eq!(
            built("nws-api", "stations"),
            "https://api.weather.gov/radar/stations"
        );
        assert_eq!(
            built("nws-api", "alerts"),
            "https://api.weather.gov/alerts/active?status=actual&message_type=alert"
        );
        assert_eq!(
            built("usgs-earthquakes", "feed"),
            "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/2.5_day.geojson"
        );
        assert_eq!(
            built("nexrad-level-2", "list"),
            "https://unidata-nexrad-level2.s3.amazonaws.com?list-type=2&prefix=2026/11/13/KTWX/"
        );
        assert_eq!(
            built("nexrad-level-2", "list-page"),
            "https://unidata-nexrad-level2.s3.amazonaws.com?list-type=2&prefix=2026/11/13/KTWX/&continuation-token=abc%2Bdef%2F%3D"
        );
        assert_eq!(
            built("nexrad-level-2", "object"),
            "https://unidata-nexrad-level2.s3.amazonaws.com/2026/11/13/KTWX/KTWX20261113_000000_V06"
        );
        assert_eq!(
            built("rainviewer", "tile"),
            "https://tilecache.rainviewer.com/v2/radar/1760000000/512/3/1/2/2/1_1.png"
        );
        assert_eq!(
            built("usgs-water", "history"),
            "https://waterservices.usgs.gov/nwis/iv/?format=json&sites=07144570&parameterCd=00065,00060&period=P7D"
        );
        assert_eq!(
            built("nsidc-sea-ice", "month"),
            "https://noaadata.apps.nsidc.org/NOAA/G02135/north/monthly/data/N_11_extent_v4.0.csv"
        );
        assert_eq!(
            built("usgs-volcano", "notice"),
            "https://volcanoes.usgs.gov/hans-public/api/notice/getNotice/abc123"
        );
    }

    fn host_fragment(line: &str) -> bool {
        for domain in [".com", ".gov", ".org", ".net"] {
            let mut offset = 0;
            while let Some(found) = line[offset..].find(domain) {
                let absolute = offset + found;
                let after = line[absolute + domain.len()..].chars().next();
                if matches!(after, None | Some('/' | '"' | '\'' | ')' | ',')) {
                    return true;
                }
                offset = absolute + domain.len();
            }
        }
        false
    }

    #[test]
    fn production_code_has_no_remote_url_or_host_outside_the_registry() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => panic!("cannot read {}: {error}", directory.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !name.ends_with(".rs") || name == "sources.rs" || name == "config.rs" {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => panic!("cannot read {}: {error}", path.display()),
            };
            let production = text.split("#[cfg(test)]").next().unwrap_or("");
            for line in production.lines() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                assert!(
                    !code.contains("https://") && !code.contains("http://") && !host_fragment(code),
                    "{} composes a remote address outside the registry: {line}",
                    path.display()
                );
            }
        }
    }
}
