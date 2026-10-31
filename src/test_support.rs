use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::error::{
    ColorMapError, CoordinateError, MomentError, RadarError, RenderError, SiteIdError,
};
use crate::forecast::ForecastError;
use crate::global_radar::GlobalRadarError;
use crate::map::MapError;
use crate::place::PlaceError;
use crate::provider::JsonError;

#[derive(Debug)]
pub enum TestError {
    Message(String),
    SiteId(SiteIdError),
    Coordinate(CoordinateError),
    Moment(MomentError),
    Render(RenderError),
    Json(JsonError),
    Forecast(ForecastError),
    Radar(RadarError),
    Place(PlaceError),
    Map(MapError),
    GlobalRadar(GlobalRadarError),
    ColorMap(ColorMapError),
    Time(std::time::SystemTimeError),
}

impl Display for TestError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::SiteId(error) => Display::fmt(error, formatter),
            Self::Coordinate(error) => Display::fmt(error, formatter),
            Self::Moment(error) => Display::fmt(error, formatter),
            Self::Render(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
            Self::Forecast(error) => Display::fmt(error, formatter),
            Self::Radar(error) => Display::fmt(error, formatter),
            Self::Place(error) => Display::fmt(error, formatter),
            Self::Map(error) => Display::fmt(error, formatter),
            Self::GlobalRadar(error) => Display::fmt(error, formatter),
            Self::ColorMap(error) => Display::fmt(error, formatter),
            Self::Time(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for TestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SiteId(error) => Some(error),
            Self::Coordinate(error) => Some(error),
            Self::Moment(error) => Some(error),
            Self::Render(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Forecast(error) => Some(error),
            Self::Radar(error) => Some(error),
            Self::Place(error) => Some(error),
            Self::Map(error) => Some(error),
            Self::GlobalRadar(error) => Some(error),
            Self::ColorMap(error) => Some(error),
            Self::Time(error) => Some(error),
            Self::Message(_) => None,
        }
    }
}

impl From<&str> for TestError {
    fn from(value: &str) -> Self {
        Self::Message(String::from(value))
    }
}

impl From<String> for TestError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<SiteIdError> for TestError {
    fn from(value: SiteIdError) -> Self {
        Self::SiteId(value)
    }
}

impl From<CoordinateError> for TestError {
    fn from(value: CoordinateError) -> Self {
        Self::Coordinate(value)
    }
}

impl From<MomentError> for TestError {
    fn from(value: MomentError) -> Self {
        Self::Moment(value)
    }
}

impl From<RenderError> for TestError {
    fn from(value: RenderError) -> Self {
        Self::Render(value)
    }
}

impl From<JsonError> for TestError {
    fn from(value: JsonError) -> Self {
        Self::Json(value)
    }
}

impl From<ForecastError> for TestError {
    fn from(value: ForecastError) -> Self {
        Self::Forecast(value)
    }
}

impl From<RadarError> for TestError {
    fn from(value: RadarError) -> Self {
        Self::Radar(value)
    }
}

impl From<PlaceError> for TestError {
    fn from(value: PlaceError) -> Self {
        Self::Place(value)
    }
}

impl From<MapError> for TestError {
    fn from(value: MapError) -> Self {
        Self::Map(value)
    }
}

impl From<GlobalRadarError> for TestError {
    fn from(value: GlobalRadarError) -> Self {
        Self::GlobalRadar(value)
    }
}

impl From<ColorMapError> for TestError {
    fn from(value: ColorMapError) -> Self {
        Self::ColorMap(value)
    }
}

impl From<std::time::SystemTimeError> for TestError {
    fn from(value: std::time::SystemTimeError) -> Self {
        Self::Time(value)
    }
}
