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
//! A model vertical profile at one point, with the quantities a chaser reads
//! off a sounding worked out from it.
//!
//! The profile is a forecast-model column, not a balloon: it is the model's
//! temperature, humidity, and wind on each pressure level above the grid point
//! for one hour. Everything derived here — precipitable water, the freezing
//! level, bulk shear, the Bunkers storm motion, storm-relative helicity — is
//! computed from those levels, so the rail can name the assumptions it made.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo forecast, pressure levels";

const BASE: &str = "https://api.open-meteo.com/v1/forecast";
/// The pressure levels the profile asks for, in hectopascals.
pub const LEVELS: [u16; 12] = [1000, 975, 925, 850, 800, 700, 600, 500, 400, 300, 250, 200];
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Gravity, for the precipitable-water integral.
const GRAVITY: f64 = 9.80665;
/// The Bunkers deviation from the mean wind, in metres per second.
const BUNKERS_DEVIATION: f64 = 7.5;
/// The Magnus coefficients for saturation vapour pressure over water.
const MAGNUS_A: f64 = 17.62;
const MAGNUS_B: f64 = 243.12;

/// Why a profile could not be read.
#[derive(Debug)]
pub enum SoundingError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for SoundingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => write!(formatter, "The profile request failed: {source}"),
            Self::Shape { detail } => {
                write!(formatter, "The profile is not usable: it wants {detail}.")
            }
            Self::WorkerStopped => write!(
                formatter,
                "The profile worker stopped, so no sounding can be requested."
            ),
        }
    }
}

impl Error for SoundingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl SoundingError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One pressure level above the point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub pressure_hpa: f64,
    /// The geopotential height of the level, above mean sea level.
    pub height_m: Option<f64>,
    pub temperature_c: Option<f64>,
    /// Worked out from the provider's relative humidity.
    pub dewpoint_c: Option<f64>,
    /// The direction the wind comes from.
    pub wind_from_deg: Option<f64>,
    pub wind_speed_mps: Option<f64>,
}

impl Level {
    /// The dew point depression, the difference that drives a cloud base.
    pub fn depression_c(&self) -> Option<f64> {
        match (self.temperature_c, self.dewpoint_c) {
            (Some(temperature), Some(dewpoint)) => Some(temperature - dewpoint),
            _ => None,
        }
    }

    /// The wind vector, eastward and northward, in metres per second.
    pub fn wind_vector(&self) -> Option<(f64, f64)> {
        let from = self.wind_from_deg?;
        let speed = self.wind_speed_mps?;
        let radians = from.to_radians();
        Some((-speed * radians.sin(), -speed * radians.cos()))
    }

    /// The saturation mixing ratio at the level's dew point, grams per
    /// kilogram.
    pub fn mixing_ratio_gkg(&self) -> Option<f64> {
        let dewpoint = self.dewpoint_c?;
        let vapour = saturation_vapour_hpa(dewpoint);
        let dry = self.pressure_hpa - vapour;
        if dry <= 0.0 {
            return None;
        }
        Some(622.0 * vapour / dry)
    }
}

/// The model column above one point at one hour.
#[derive(Debug, Clone, PartialEq)]
pub struct Sounding {
    pub latitude: f64,
    pub longitude: f64,
    /// The model's grid elevation, which the levels are measured from.
    pub elevation_m: Option<f64>,
    pub time_ms: u64,
    /// The surface report: the provider's two-metre temperature, dew point,
    /// pressure, and ten-metre wind, as a level whose pressure is the surface
    /// pressure.
    pub surface: Level,
    /// The pressure levels, ordered from the ground upwards.
    pub levels: Vec<Level>,
}

/// The quantities a chaser reads off the column, each one absent when the
/// levels it needs are missing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Indices {
    /// Precipitable water, millimetres.
    pub precipitable_water_mm: Option<f64>,
    /// The lifted condensation level above the point, metres.
    pub lcl_m: Option<f64>,
    /// The height of the freezing level above mean sea level, metres.
    pub freezing_level_m: Option<f64>,
    /// The 700-500 hPa lapse rate, degrees Celsius per kilometre.
    pub lapse_rate_700_500: Option<f64>,
    /// The bulk shear from the surface to the top of the column, or to six
    /// kilometres when the column reaches that far: the depth in metres and
    /// the speed in metres per second.
    pub deep_shear: Option<(f64, f64)>,
    /// The 0-1 km bulk shear, metres per second.
    pub shear_0_1_mps: Option<f64>,
    /// The Bunkers right-moving storm motion, metres per second.
    pub right_mover: Option<(f64, f64)>,
    /// The 0-3 km storm-relative helicity against that motion, square metres
    /// per second squared.
    pub srh_0_3: Option<f64>,
}

impl Sounding {
    /// The column ordered from the ground upwards, with the surface report
    /// first when it carries a wind and a temperature.
    pub fn column(&self) -> Vec<Level> {
        let mut column = Vec::with_capacity(self.levels.len() + 1);
        if self.surface.wind_vector().is_some() && self.surface.temperature_c.is_some() {
            column.push(self.surface);
        }
        column.extend(self.levels.iter().copied());
        column
    }

    /// The height of a level above the point, when the model gave one.
    pub fn agl(&self, level: &Level) -> Option<f64> {
        let elevation = self.elevation_m?;
        Some(level.height_m? - elevation)
    }

    /// The wind vector interpolated to a height above the point.
    pub fn wind_at(&self, agl_m: f64) -> Option<(f64, f64)> {
        let column = self.column();
        let mut below: Option<(f64, (f64, f64))> = None;
        for level in &column {
            let Some(height) = self.agl(level) else {
                continue;
            };
            let Some(vector) = level.wind_vector() else {
                continue;
            };
            if (height - agl_m).abs() < 1.0 {
                return Some(vector);
            }
            if height < agl_m {
                below = Some((height, vector));
                continue;
            }
            let (low_height, low) = below?;
            if height <= low_height {
                return None;
            }
            let share = (agl_m - low_height) / (height - low_height);
            return Some((
                low.0 + (vector.0 - low.0) * share,
                low.1 + (vector.1 - low.1) * share,
            ));
        }
        None
    }

    /// The height at which the temperature falls to `target_c`, searched
    /// upwards from the surface report's temperature.
    pub fn level_temperature_reaches(&self, target_c: f64) -> Option<f64> {
        let mut below: Option<(f64, f64)> = match (self.elevation_m, self.surface.temperature_c) {
            (Some(height), Some(temperature)) => Some((height, temperature)),
            _ => None,
        };
        for level in self.levels.iter() {
            let (Some(height), Some(temperature)) = (level.height_m, level.temperature_c) else {
                continue;
            };
            if below.is_none() {
                below = Some((height, temperature));
                continue;
            }
            let (low_height, low) = below?;
            if (low - target_c) * (temperature - target_c) <= 0.0 && temperature != low {
                let share = (low - target_c) / (low - temperature);
                return Some(low_height + (height - low_height) * share);
            }
            below = Some((height, temperature));
        }
        None
    }

    /// The 0-6 km mean wind, averaged at half-kilometre steps.
    pub fn mean_wind_0_6(&self) -> Option<(f64, f64)> {
        let mut east = 0.0;
        let mut north = 0.0;
        let mut count = 0.0;
        for step in 0..=12 {
            let height = f64::from(step) * 500.0;
            let (u, v) = self.wind_at(height)?;
            east += u;
            north += v;
            count += 1.0;
        }
        if count <= 0.0 {
            return None;
        }
        Some((east / count, north / count))
    }

    /// The Bunkers right-moving storm motion. Its deviation is taken
    /// perpendicular, to the right of the 0-6 km shear vector.
    pub fn right_mover(&self) -> Option<(f64, f64)> {
        let (mean_u, mean_v) = self.mean_wind_0_6()?;
        let (low_u, low_v) = self.wind_at(0.0)?;
        let (top_u, top_v) = self.wind_at(6000.0)?;
        let shear_u = top_u - low_u;
        let shear_v = top_v - low_v;
        let magnitude = (shear_u * shear_u + shear_v * shear_v).sqrt();
        if magnitude <= 0.0 {
            return Some((mean_u, mean_v));
        }
        Some((
            mean_u + BUNKERS_DEVIATION * shear_v / magnitude,
            mean_v - BUNKERS_DEVIATION * shear_u / magnitude,
        ))
    }

    /// Storm-relative helicity between two heights, against a storm motion.
    /// The trapezoid rule over the layer's wind vectors is the standard
    /// discrete form.
    pub fn srh(&self, from_agl_m: f64, to_agl_m: f64, motion: (f64, f64)) -> Option<f64> {
        if to_agl_m <= from_agl_m {
            return None;
        }
        let mut total = 0.0;
        let mut previous: Option<(f64, f64)> = None;
        let mut height = from_agl_m;
        loop {
            let (u, v) = self.wind_at(height)?;
            if let Some((last_u, last_v)) = previous {
                total +=
                    (u - motion.0) * (last_v - motion.1) - (last_u - motion.0) * (v - motion.1);
            }
            previous = Some((u, v));
            if height >= to_agl_m {
                break;
            }
            height = (height + 250.0).min(to_agl_m);
        }
        Some(total)
    }

    /// Every derived quantity the column supports.
    pub fn indices(&self) -> Indices {
        let right_mover = self.right_mover();
        Indices {
            precipitable_water_mm: self.precipitable_water_mm(),
            lcl_m: self.lcl_m(),
            freezing_level_m: self.level_temperature_reaches(0.0),
            lapse_rate_700_500: self.lapse_rate(700.0, 500.0),
            deep_shear: self.deep_shear(),
            shear_0_1_mps: self.shear_between(0.0, 1000.0),
            right_mover,
            srh_0_3: right_mover.and_then(|motion| self.srh(0.0, 3000.0, motion)),
        }
    }

    /// The highest height above the point the column reaches.
    pub fn top_agl(&self) -> Option<f64> {
        let elevation = self.elevation_m?;
        let highest = self
            .levels
            .iter()
            .filter_map(|level| level.height_m)
            .fold(f64::NEG_INFINITY, f64::max);
        if !highest.is_finite() {
            return None;
        }
        let top = highest - elevation;
        if top <= 0.0 { None } else { Some(top) }
    }

    /// The bulk shear over the deepest layer the column supports, which is six
    /// kilometres when the model reaches that far and the column's own top
    /// otherwise. The depth is returned with the speed so the rail can say
    /// which layer it measured.
    pub fn deep_shear(&self) -> Option<(f64, f64)> {
        let top = self.top_agl()?;
        let depth = top.min(6000.0);
        if depth < 1000.0 {
            return None;
        }
        let speed = self.shear_between(0.0, depth)?;
        Some((depth, speed))
    }

    /// The magnitude of the vector wind difference between two heights.
    pub fn shear_between(&self, from_agl_m: f64, to_agl_m: f64) -> Option<f64> {
        let (from_u, from_v) = self.wind_at(from_agl_m)?;
        let (to_u, to_v) = self.wind_at(to_agl_m)?;
        Some(((to_u - from_u).powi(2) + (to_v - from_v).powi(2)).sqrt())
    }

    /// Precipitable water: the water in a column of unit area, from the
    /// mixing ratio between neighbouring levels.
    pub fn precipitable_water_mm(&self) -> Option<f64> {
        let mut column: Vec<(f64, f64)> = Vec::with_capacity(self.levels.len() + 1);
        if let (Some(pressure), Some(ratio)) = (
            Some(self.surface.pressure_hpa),
            self.surface.mixing_ratio_gkg(),
        ) {
            column.push((pressure, ratio));
        }
        for level in &self.levels {
            if let Some(ratio) = level.mixing_ratio_gkg() {
                column.push((level.pressure_hpa, ratio));
            }
        }
        if column.len() < 2 {
            return None;
        }
        let mut total = 0.0;
        for pair in column.windows(2) {
            let (Some(low), Some(high)) = (pair.first(), pair.get(1)) else {
                return None;
            };
            let depth = low.0 - high.0;
            if depth <= 0.0 {
                continue;
            }
            let mean_ratio = (low.1 + high.1) / 2000.0;
            total += mean_ratio * depth * 100.0 / GRAVITY;
        }
        Some(total)
    }

    /// The lifted condensation level above the point, from the surface dew
    /// point depression. The estimate is the customary 125 m per degree,
    /// which holds for the well-mixed boundary layer this layer is read for.
    pub fn lcl_m(&self) -> Option<f64> {
        let depression = self.surface.depression_c()?;
        if depression < 0.0 {
            return None;
        }
        Some(depression * 125.0)
    }

    /// The mean lapse rate between two pressure levels.
    pub fn lapse_rate(&self, low_hpa: f64, high_hpa: f64) -> Option<f64> {
        let find = |pressure: f64| {
            self.levels
                .iter()
                .find(|level| (level.pressure_hpa - pressure).abs() < 1.0)
        };
        let low = find(low_hpa)?;
        let high = find(high_hpa)?;
        let (Some(low_height), Some(high_height)) = (low.height_m, high.height_m) else {
            return None;
        };
        let (Some(low_t), Some(high_t)) = (low.temperature_c, high.temperature_c) else {
            return None;
        };
        let thickness_km = (high_height - low_height) / 1000.0;
        if thickness_km <= 0.0 {
            return None;
        }
        Some((low_t - high_t) / thickness_km)
    }

    /// The pressure of the top level, which the chart's axis has to reach.
    pub fn top_hpa(&self) -> f64 {
        self.levels.last().map_or(200.0, |level| level.pressure_hpa)
    }
}

/// Saturation vapour pressure over water, hectopascals.
fn saturation_vapour_hpa(temperature_c: f64) -> f64 {
    6.112 * (MAGNUS_A * temperature_c / (MAGNUS_B + temperature_c)).exp()
}

/// The dew point of moist air, from its temperature and relative humidity.
pub fn dewpoint_c(temperature_c: f64, relative_humidity: f64) -> Option<f64> {
    if relative_humidity <= 0.0 || relative_humidity > 100.0 {
        return None;
    }
    let gamma =
        (relative_humidity / 100.0).ln() + MAGNUS_A * temperature_c / (MAGNUS_B + temperature_c);
    let denominator = MAGNUS_A - gamma;
    if denominator.abs() < f64::EPSILON {
        return None;
    }
    Some(MAGNUS_B * gamma / denominator)
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> SoundingError {
    SoundingError::Shape { detail }
}

/// Reads one series, checking it against the hour count.
fn series(
    row: &Map<String, Value>,
    field: &str,
    count: usize,
) -> Result<Vec<Option<f64>>, SoundingError> {
    let Some(values) = row.get(field) else {
        return Ok(vec![None; count]);
    };
    let Some(values) = values.as_array() else {
        return Err(shape("an array for every hourly series"));
    };
    if values.len() != count {
        return Err(shape("one value per hour in every series"));
    }
    values
        .iter()
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let Some(number) = value.as_f64() else {
                return Err(shape("a number for every hourly value"));
            };
            if !number.is_finite() {
                return Err(shape("a finite hourly value"));
            }
            Ok(Some(number))
        })
        .collect()
}

/// The field name the provider uses for one quantity on one level.
fn field(prefix: &str, level: u16) -> String {
    format!("{prefix}_{level}hPa")
}

/// Reads the hourly times and returns them with the index nearest `now_ms`.
fn read_times(
    hourly: &Map<String, Value>,
    now_ms: u64,
) -> Result<(Vec<u64>, usize), SoundingError> {
    let Some(times) = hourly.get("time").and_then(Value::as_array) else {
        return Err(shape("an array of hourly times"));
    };
    if times.is_empty() {
        return Err(shape("at least one hour of record"));
    }
    let mut parsed = Vec::with_capacity(times.len());
    for text in times {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        parsed.push(time_ms);
    }
    let Some(hour) = parsed
        .iter()
        .enumerate()
        .min_by_key(|(_, time_ms)| time_ms.abs_diff(now_ms))
        .map(|(index, _)| index)
    else {
        return Err(shape("an hour to read"));
    };
    Ok((parsed, hour))
}

/// Reads the surface report for one hour.
fn read_surface(
    hourly: &Map<String, Value>,
    count: usize,
    hour: usize,
    elevation_m: Option<f64>,
) -> Result<Level, SoundingError> {
    let at = |values: &Vec<Option<f64>>| values.get(hour).copied().flatten();
    let temperature = at(&series(hourly, "temperature_2m", count)?);
    let humidity = at(&series(hourly, "relative_humidity_2m", count)?);
    let dewpoint = at(&series(hourly, "dew_point_2m", count)?);
    let pressure = at(&series(hourly, "surface_pressure", count)?)
        .or_else(|| elevation_m.and_then(standard_pressure));
    let Some(pressure_hpa) = pressure else {
        return Err(shape("a surface pressure"));
    };
    let dewpoint_c = dewpoint.or_else(|| {
        let temperature = temperature?;
        dewpoint_c(temperature, humidity?)
    });
    Ok(Level {
        pressure_hpa,
        height_m: elevation_m,
        temperature_c: temperature,
        dewpoint_c,
        wind_from_deg: at(&series(hourly, "wind_direction_10m", count)?)
            .filter(|value| (0.0..=360.0).contains(value)),
        wind_speed_mps: at(&series(hourly, "wind_speed_10m", count)?).filter(|value| *value >= 0.0),
    })
}

/// Reads one pressure level for one hour, when the provider gives it.
fn read_level(
    hourly: &Map<String, Value>,
    count: usize,
    hour: usize,
    pressure: u16,
) -> Result<Option<Level>, SoundingError> {
    let at = |values: &Vec<Option<f64>>| values.get(hour).copied().flatten();
    let temperature = at(&series(hourly, &field("temperature", pressure), count)?);
    let humidity = at(&series(
        hourly,
        &field("relative_humidity", pressure),
        count,
    )?);
    let height = at(&series(
        hourly,
        &field("geopotential_height", pressure),
        count,
    )?);
    let speed =
        at(&series(hourly, &field("wind_speed", pressure), count)?).filter(|value| *value >= 0.0);
    let direction = at(&series(hourly, &field("wind_direction", pressure), count)?)
        .filter(|value| (0.0..=360.0).contains(value));
    if temperature.is_none() && height.is_none() {
        return Ok(None);
    }
    let dewpoint = temperature
        .zip(humidity)
        .and_then(|(temperature, humidity)| dewpoint_c(temperature, humidity));
    Ok(Some(Level {
        pressure_hpa: f64::from(pressure),
        height_m: height,
        temperature_c: temperature,
        dewpoint_c: dewpoint,
        wind_from_deg: direction,
        wind_speed_mps: speed,
    }))
}

/// Reads the provider's document into the hour nearest `now_ms`.
pub fn parse(document: &Value, now_ms: u64) -> Result<Sounding, SoundingError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let elevation_m = document.get("elevation").and_then(Value::as_f64);
    let Some(hourly) = document.get("hourly").and_then(Value::as_object) else {
        return Err(shape("an hourly record"));
    };
    let (times, hour) = read_times(hourly, now_ms)?;
    let count = times.len();
    let surface = read_surface(hourly, count, hour, elevation_m)?;
    let mut levels = Vec::with_capacity(LEVELS.len());
    for pressure in LEVELS {
        if let Some(level) = read_level(hourly, count, hour, pressure)? {
            levels.push(level);
        }
    }
    if levels.is_empty() {
        return Err(shape("at least one pressure level"));
    }
    // The chart draws downwards, so the column runs from the highest pressure
    // to the lowest.
    levels.sort_by(|left, right| right.pressure_hpa.total_cmp(&left.pressure_hpa));
    Ok(Sounding {
        latitude,
        longitude,
        elevation_m,
        time_ms: times.get(hour).copied().unwrap_or(now_ms),
        surface,
        levels,
    })
}

/// A standard-atmosphere pressure for a height, used only when the provider
/// omits the surface pressure but states an elevation.
fn standard_pressure(height_m: f64) -> Option<f64> {
    if !height_m.is_finite() || height_m < -500.0 || height_m > 9000.0 {
        return None;
    }
    Some(1013.25 * (1.0 - 2.255_77e-5 * height_m).powf(5.255_88))
}

/// The pressure levels and surface fields the request asks for.
fn fields() -> String {
    let mut list = vec![
        String::from("temperature_2m"),
        String::from("relative_humidity_2m"),
        String::from("dew_point_2m"),
        String::from("surface_pressure"),
        String::from("wind_speed_10m"),
        String::from("wind_direction_10m"),
    ];
    for level in LEVELS {
        list.push(field("temperature", level));
        list.push(field("relative_humidity", level));
        list.push(field("geopotential_height", level));
        list.push(field("wind_speed", level));
        list.push(field("wind_direction", level));
    }
    list.join(",")
}

/// The request for one point.
pub fn request_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&hourly={}&wind_speed_unit=ms&forecast_days=1&timezone=UTC",
        fields()
    )
}

/// Fetches the profile for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<Sounding, SoundingError> {
    let now_ms = match time::now_unix_ms() {
        Ok(now_ms) => now_ms,
        Err(_) => return Err(shape("a readable system clock to pick the hour")),
    };
    let document = provider::get_json(&request_url(latitude, longitude), "application/json")
        .map_err(|source| SoundingError::Provider { source })?;
    parse(&document, now_ms)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct SoundingRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum SoundingEvent {
    Ready {
        generation: u64,
        record: Sounding,
    },
    Failed {
        generation: u64,
        error: SoundingError,
    },
}

/// Fetches the profile off the UI thread.
pub struct SoundingFeed {
    requests: Sender<SoundingRequest>,
    events: Receiver<SoundingEvent>,
    generation: u64,
    stopped: bool,
}

impl SoundingFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<SoundingRequest>();
        let (event_sender, events) = mpsc::channel::<SoundingEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.latitude, request.longitude) {
                    Ok(record) => SoundingEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => SoundingEvent::Failed {
                        generation: request.generation,
                        error,
                    },
                };
                if event_sender.send(event).is_err() {
                    return;
                }
            }
        });
        Self {
            requests,
            events,
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: SoundingRequest) -> Result<(), SoundingError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(SoundingError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<SoundingEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(SoundingEvent::Failed {
                    generation: self.generation,
                    error: SoundingError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum SoundingState {
    Idle,
    Loading {
        last: Option<Sounding>,
    },
    Ready {
        record: Sounding,
    },
    Failed {
        error: SoundingError,
        last: Option<Sounding>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl SoundingState {
    /// The most recent profile still worth drawing.
    pub const fn last(&self) -> Option<&Sounding> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct SoundingView {
    feed: SoundingFeed,
    state: SoundingState,
    generation: u64,
}

impl SoundingView {
    pub fn new() -> Self {
        Self {
            feed: SoundingFeed::new(),
            state: SoundingState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &SoundingState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Sounding> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = SoundingState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = SoundingRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = SoundingState::Failed {
                error,
                last: None,
                attempts: attempt,
                retry_due: None,
            };
        }
    }

    /// Takes in the worker's answer and returns whether a retry came due.
    pub fn update(&mut self, now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                SoundingEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = SoundingState::Ready { record };
                }
                SoundingEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = SoundingState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            SoundingState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            SoundingState::Idle | SoundingState::Loading { .. } | SoundingState::Ready { .. } => {
                false
            }
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            SoundingState::Failed { attempts, .. } => *attempts,
            SoundingState::Idle | SoundingState::Loading { .. } | SoundingState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            SoundingState::Failed { retry_due, .. } => retry_due.is_some(),
            SoundingState::Idle | SoundingState::Loading { .. } | SoundingState::Ready { .. } => {
                false
            }
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut SoundingState) -> Option<Sounding> {
    match std::mem::replace(state, SoundingState::Idle) {
        SoundingState::Loading { last } | SoundingState::Failed { last, .. } => last,
        SoundingState::Ready { record } => Some(record),
        SoundingState::Idle => None,
    }
}

/// Exponential backoff for a request that may succeed later.
fn retry_due(transient: bool, attempts: u32) -> Option<Instant> {
    if !transient || attempts == 0 || attempts >= MAX_ATTEMPTS {
        return None;
    }
    let steps = attempts.min(4);
    let delay = RETRY_BASE.saturating_mul(1_u32 << steps).min(RETRY_MAX);
    Instant::now().checked_add(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"{
            "latitude": 37.2, "longitude": -97.5, "elevation": 373.0,
            "hourly": {
                "time": ["2026-11-02T00:00", "2026-11-02T01:00"],
                "temperature_2m": [22.0, 23.0],
                "relative_humidity_2m": [60.0, 55.0],
                "dew_point_2m": [13.8, 13.0],
                "surface_pressure": [970.0, 969.0],
                "wind_speed_10m": [6.0, 7.0],
                "wind_direction_10m": [160.0, 170.0],
                "temperature_925hPa": [15.6, 16.0],
                "relative_humidity_925hPa": [70.0, 65.0],
                "geopotential_height_925hPa": [780.0, 782.0],
                "wind_speed_925hPa": [12.0, 13.0],
                "wind_direction_925hPa": [180.0, 185.0],
                "temperature_700hPa": [3.2, 3.5],
                "relative_humidity_700hPa": [40.0, 35.0],
                "geopotential_height_700hPa": [3100.0, 3105.0],
                "wind_speed_700hPa": [20.0, 21.0],
                "wind_direction_700hPa": [200.0, 205.0],
                "temperature_500hPa": [-14.0, -13.5],
                "relative_humidity_500hPa": [20.0, 18.0],
                "geopotential_height_500hPa": [5700.0, 5705.0],
                "wind_speed_500hPa": [35.0, 36.0],
                "wind_direction_500hPa": [230.0, 235.0]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn sounding() -> Result<Sounding, SoundingError> {
        parse(&document(), 0)
    }

    #[test]
    fn the_hour_nearest_now_is_the_one_read() -> Result<(), SoundingError> {
        let record = sounding()?;
        let first = time::parse_iso_utc("2026-11-02T00:00").unwrap_or_default();
        assert_eq!(record.time_ms, first);
        let second = time::parse_iso_utc("2026-11-02T01:00").unwrap_or_default();
        let later = parse(&document(), second)?;
        assert_eq!(later.time_ms, second);
        assert_eq!(later.surface.temperature_c, Some(23.0));
        Ok(())
    }

    #[test]
    fn a_saturated_parcel_has_its_dew_point_at_its_temperature() {
        assert_eq!(dewpoint_c(20.0, 100.0), Some(20.0));
        let half = dewpoint_c(20.0, 50.0);
        assert!(
            half.is_some_and(|value| value > 8.0 && value < 10.5),
            "{half:?}"
        );
    }

    #[test]
    fn the_mixing_ratio_matches_the_saturation_table() -> Result<(), SoundingError> {
        let record = sounding()?;
        let level = record
            .levels
            .iter()
            .find(|level| (level.pressure_hpa - 925.0).abs() < 1.0)
            .ok_or(shape("the 925 hPa level"))?;
        let ratio = level.mixing_ratio_gkg().ok_or(shape("a mixing ratio"))?;
        // 15.6 °C at 925 hPa saturates near 11 g/kg; the dew point is lower.
        assert!((6.0..=11.0).contains(&ratio), "{ratio}");
        Ok(())
    }

    #[test]
    fn a_wind_from_the_south_blows_north() -> Result<(), SoundingError> {
        let record = sounding()?;
        let (u, v) = record.wind_at(0.0).ok_or(shape("a surface wind"))?;
        assert!(v > 0.0, "v is {v}");
        assert!(u.abs() < 3.0, "u is {u}");
        // A wind from due south has no eastward part at all.
        let (u, v) = Level {
            pressure_hpa: 1000.0,
            height_m: Some(0.0),
            temperature_c: None,
            dewpoint_c: None,
            wind_from_deg: Some(180.0),
            wind_speed_mps: Some(10.0),
        }
        .wind_vector()
        .ok_or(shape("a wind vector"))?;
        assert!(u.abs() < 1e-9, "u is {u}");
        assert!((v - 10.0).abs() < 1e-9, "v is {v}");
        Ok(())
    }

    #[test]
    fn the_shear_is_the_vector_difference_between_two_heights() -> Result<(), SoundingError> {
        let record = sounding()?;
        let shear = record.shear_between(0.0, 5000.0).ok_or(shape("a shear"))?;
        // The surface wind is from 160 and the 500 hPa wind from 230: a
        // veering profile with a strong speed increase, so the shear is large.
        assert!(shear > 20.0, "{shear}");
        Ok(())
    }

    #[test]
    fn the_deep_shear_names_the_depth_the_column_supports() -> Result<(), SoundingError> {
        let record = sounding()?;
        let (depth, speed) = record.deep_shear().ok_or(shape("a deep shear"))?;
        // The fixture tops out at 500 hPa, 5327 m above the point, so the
        // measured layer is that deep and not a fabricated six kilometres.
        assert!((5300.0..=5400.0).contains(&depth), "{depth}");
        assert!(speed > 20.0, "{speed}");
        Ok(())
    }

    #[test]
    fn a_uniform_wind_has_no_shear_and_no_helicity() {
        let flat = Level {
            pressure_hpa: 1000.0,
            height_m: Some(0.0),
            temperature_c: Some(20.0),
            dewpoint_c: Some(10.0),
            wind_from_deg: Some(180.0),
            wind_speed_mps: Some(10.0),
        };
        let top = Level {
            pressure_hpa: 500.0,
            height_m: Some(6000.0),
            ..flat
        };
        let surface = Level {
            pressure_hpa: 1000.0,
            ..flat
        };
        let record = Sounding {
            latitude: 0.0,
            longitude: 0.0,
            elevation_m: Some(0.0),
            time_ms: 0,
            surface,
            levels: vec![flat, top],
        };
        let shear = record.shear_between(0.0, 6000.0).unwrap_or(f64::NAN);
        assert!(shear.abs() < 1e-9, "shear is {shear}");
        let motion = record.right_mover().unwrap_or((0.0, 0.0));
        let srh = record.srh(0.0, 3000.0, motion).unwrap_or(f64::NAN);
        assert!(srh.abs() < 1e-9, "helicity is {srh}");
    }

    #[test]
    fn a_turning_hodograph_has_positive_helicity_for_a_storm_that_moves_with_it() {
        // Wind turns from south-westerly at the surface to westerly aloft while
        // it strengthens, the classic turning-with-height profile.
        let low = Level {
            pressure_hpa: 1000.0,
            height_m: Some(0.0),
            temperature_c: Some(20.0),
            dewpoint_c: Some(10.0),
            wind_from_deg: Some(180.0),
            wind_speed_mps: Some(5.0),
        };
        let mid = Level {
            pressure_hpa: 850.0,
            height_m: Some(1500.0),
            wind_from_deg: Some(225.0),
            wind_speed_mps: Some(15.0),
            ..low
        };
        let top = Level {
            pressure_hpa: 700.0,
            height_m: Some(3000.0),
            wind_from_deg: Some(250.0),
            wind_speed_mps: Some(25.0),
            ..low
        };
        let surface = Level {
            pressure_hpa: 1000.0,
            ..low
        };
        let record = Sounding {
            latitude: 0.0,
            longitude: 0.0,
            elevation_m: Some(0.0),
            time_ms: 0,
            surface,
            levels: vec![low, mid, top],
        };
        let motion = record.right_mover().unwrap_or((0.0, 0.0));
        let srh = record.srh(0.0, 3000.0, motion).unwrap_or(f64::NAN);
        assert!(srh > 0.0, "helicity is {srh} for motion {motion:?}");
    }

    #[test]
    fn the_freezing_level_lies_between_the_levels_that_bracket_it() -> Result<(), SoundingError> {
        let record = sounding()?;
        let freezing = record
            .level_temperature_reaches(0.0)
            .ok_or(shape("a freezing level"))?;
        // Between 925 hPa at 15.6 °C and 700 hPa at 3.2 °C, then 500 hPa at
        // -14 °C, so the height is below the 500 hPa level.
        assert!((3100.0..5700.0).contains(&freezing), "{freezing}");
        Ok(())
    }

    #[test]
    fn precipitable_water_grows_with_the_moisture_in_the_column() -> Result<(), SoundingError> {
        let record = sounding()?;
        let water = record
            .precipitable_water_mm()
            .ok_or(shape("precipitable water"))?;
        assert!((5.0..=60.0).contains(&water), "{water}");
        Ok(())
    }

    #[test]
    fn the_lapse_rate_is_warm_for_a_steep_column() -> Result<(), SoundingError> {
        let record = sounding()?;
        let rate = record
            .lapse_rate(700.0, 500.0)
            .ok_or(shape("a lapse rate"))?;
        // 3.5 °C at 3105 m down to -13.5 °C at 5705 m is 17 K over 2.6 km.
        assert!((6.0..=7.0).contains(&rate), "{rate}");
        Ok(())
    }

    #[test]
    fn the_request_names_every_level_it_needs() {
        let url = request_url(37.2, -97.5);
        for level in LEVELS {
            assert!(url.contains(&format!("temperature_{level}hPa")), "{level}");
            assert!(
                url.contains(&format!("wind_direction_{level}hPa")),
                "{level}"
            );
        }
        assert!(url.contains("wind_speed_unit=ms"), "{url}");
        assert!(url.contains("surface_pressure"), "{url}");
    }

    #[test]
    fn a_profile_without_levels_is_refused() {
        let body =
            r#"{"latitude": 1.0, "longitude": 2.0, "hourly": {"time": ["2026-11-02T00:00"]}}"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(matches!(parse(&value, 0), Err(SoundingError::Shape { .. })));
    }
}
