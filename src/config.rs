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
//! The typed configuration every part of the application reads.
//!
//! Values resolve in three layers: built-in defaults, then a profile file,
//! then an explicit selection from the command line. Each layer records which
//! fields it set, so the effective configuration can be printed with the
//! origin of every value.
//!
//! A fresh install is locale-neutral. Units follow the locale unless the
//! profile or the selection fixes them, and no field defaults to a station,
//! feed or identifier from one country.

use std::error::Error;
use std::fmt::{self, Display, Formatter, Write as _};
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::basemap::MapStyle;
use crate::colormap::ColorTable;
use crate::derived::StormMotion;
use crate::error::ConfigError;
use crate::paths;
use crate::place::{Place, PlaceError};
use crate::render::Sampling;
use crate::sources::{self, Source};
use crate::stations::SiteId;
use crate::theme::Theme;
use crate::units::UnitSystem;

const HEADER: &str = "# oxrad configuration.\n";
const NEUTRAL_LOCALE: &str = "en";
const MIN_CACHE_MIB: u64 = 8;
const MAX_CACHE_MIB: u64 = 4096;
const MIN_CACHE_ENTRIES: usize = 16;
const MAX_CACHE_ENTRIES: usize = 65_536;

/// A language tag with an optional region.
///
/// The tag is validated at the boundary: ASCII letters and digits in groups of
/// two to eight, joined by hyphens, with a language group of two or three
/// letters. The region is the final two-letter group when one is present, and
/// it decides the default unit system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    tag: String,
    region: Option<String>,
}

impl Locale {
    /// The neutral locale used when the environment sets nothing.
    pub fn neutral() -> Self {
        Self {
            tag: String::from(NEUTRAL_LOCALE),
            region: None,
        }
    }

    /// Parses a BCP 47 language tag.
    pub fn parse(text: &str) -> Result<Self, LocaleError> {
        if text.is_empty() {
            return Err(LocaleError::Empty);
        }
        let groups: Vec<&str> = text.split('-').collect();
        let first = groups.first().copied().unwrap_or_default();
        if !(2..=3).contains(&first.len()) || !first.bytes().all(|byte| byte.is_ascii_alphabetic())
        {
            return Err(LocaleError::Shape {
                value: String::from(text),
            });
        }
        for group in &groups {
            if group.is_empty() || group.len() > 8 {
                return Err(LocaleError::Shape {
                    value: String::from(text),
                });
            }
            if let Some(byte) = group.bytes().find(|byte| !byte.is_ascii_alphanumeric()) {
                return Err(LocaleError::Character {
                    value: String::from(text),
                    byte,
                });
            }
        }
        let region = groups
            .last()
            .filter(|group| {
                group.len() == 2 && group.bytes().all(|byte| byte.is_ascii_alphabetic())
            })
            .map(|group| group.to_ascii_uppercase());
        Ok(Self {
            tag: String::from(text),
            region,
        })
    }

    /// Reads the locale the environment declares, if it declares one.
    ///
    /// POSIX values such as `en_US.UTF-8` are normalised; `C` and `POSIX`
    /// carry no locale information and fall back to neutral.
    pub fn from_environment() -> Self {
        for name in ["LC_ALL", "LC_MEASUREMENT", "LANG"] {
            let Ok(value) = std::env::var(name) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let base = value.split('.').next().map_or("", |part| part);
            if base == "C" || base == "POSIX" {
                return Self::neutral();
            }
            let normalised = base.replace('_', "-");
            match Self::parse(&normalised) {
                Ok(locale) => return locale,
                Err(_) => return Self::neutral(),
            }
        }
        Self::neutral()
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }
}

/// Why a language tag is not usable.
#[derive(Debug)]
pub enum LocaleError {
    Empty,
    Shape { value: String },
    Character { value: String, byte: u8 },
}

impl Display for LocaleError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "the locale is empty"),
            Self::Shape { value } => write!(
                formatter,
                "{value:?} is not a language tag such as en, en-US or zh-Hans-CN"
            ),
            Self::Character { value, byte } => write!(
                formatter,
                "{value:?} contains {byte}; a language tag uses letters, digits and hyphens"
            ),
        }
    }
}

impl Error for LocaleError {}

/// Whether the unit system follows the locale or is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitPreference {
    Auto,
    Fixed(UnitSystem),
}

/// The size budget for the persistent response cache.
///
/// `max_bytes` is held in bytes and is written as whole mebibytes; the allowed
/// range is 8 to 4096 MiB. The default is 128 MiB, which holds roughly five
/// radar volumes plus a day of basemap and composite tiles, so a restart
/// re-uses what it fetched without letting the directory grow without bound.
/// `max_entries` is the count of bodies and sidecars the store will keep, from
/// 16 to 65536; 256 matches the byte budget at the observed average entry size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    max_bytes: u64,
    max_entries: usize,
}

impl CachePolicy {
    pub const DEFAULT: Self = Self {
        max_bytes: 128 * 1024 * 1024,
        max_entries: 256,
    };

    pub fn from_mib(mib: u64) -> Result<Self, CachePolicyError> {
        if !(MIN_CACHE_MIB..=MAX_CACHE_MIB).contains(&mib) {
            return Err(CachePolicyError::Size { mib });
        }
        Ok(Self {
            max_bytes: mib * 1024 * 1024,
            max_entries: Self::DEFAULT.max_entries,
        })
    }

    fn with_entries(self, entries: usize) -> Result<Self, CachePolicyError> {
        if !(MIN_CACHE_ENTRIES..=MAX_CACHE_ENTRIES).contains(&entries) {
            return Err(CachePolicyError::Entries { entries });
        }
        Ok(Self {
            max_bytes: self.max_bytes,
            max_entries: entries,
        })
    }

    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    pub const fn max_entries(self) -> usize {
        self.max_entries
    }

    pub const fn mib(self) -> u64 {
        self.max_bytes / (1024 * 1024)
    }
}

/// Why a cache budget is out of range.
#[derive(Debug)]
pub enum CachePolicyError {
    Size { mib: u64 },
    Entries { entries: usize },
}

impl Display for CachePolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Size { mib } => write!(
                formatter,
                "{mib} MiB is outside the allowed {MIN_CACHE_MIB} to {MAX_CACHE_MIB} MiB"
            ),
            Self::Entries { entries } => write!(
                formatter,
                "{entries} entries is outside the allowed {MIN_CACHE_ENTRIES} to {MAX_CACHE_ENTRIES}"
            ),
        }
    }
}

impl Error for CachePolicyError {}

/// The providers a configuration enables.
///
/// Every entry comes from the source registry and must pass its admission
/// test, so a keyed, unlicensed or uncredited source cannot be enabled by any
/// layer. Credits are derived from this set in registry order.
#[derive(Debug, Clone)]
pub struct EnabledProviders {
    sources: Vec<&'static Source>,
}

impl EnabledProviders {
    /// Every source whose terms admit it and whose data path has been parsed.
    pub fn all_admitted() -> Self {
        Self {
            sources: sources::SOURCES
                .iter()
                .filter(|source| source.admits().is_ok() && source.level.is_publishable())
                .collect(),
        }
    }

    /// Parses a comma-separated list of registry identifiers.
    pub fn parse(text: &str) -> Result<Self, ProviderError> {
        let mut chosen: Vec<&'static Source> = Vec::new();
        for token in text.split(',') {
            let id = token.trim();
            if id.is_empty() {
                return Err(ProviderError::Empty);
            }
            let source = sources::find(id).ok_or_else(|| ProviderError::Unknown {
                id: String::from(id),
            })?;
            source
                .admits()
                .map_err(|error| ProviderError::NotAdmitted {
                    id: String::from(id),
                    reason: error.to_string(),
                })?;
            if !source.level.is_publishable() {
                return Err(ProviderError::NotAdmitted {
                    id: String::from(id),
                    reason: format!("it is only {}", source.level.label()),
                });
            }
            if chosen.iter().any(|existing| existing.id == source.id) {
                return Err(ProviderError::Duplicate {
                    id: String::from(id),
                });
            }
            chosen.push(source);
        }
        Ok(Self { sources: chosen })
    }

    pub fn sources(&self) -> &[&'static Source] {
        &self.sources
    }

    /// Whether the effective configuration enables this source.
    pub fn enables(&self, id: &str) -> bool {
        self.sources.iter().any(|source| source.id.as_str() == id)
    }

    /// The credit lines the interface must display, in registry order.
    pub fn credits(&self) -> Vec<&'static str> {
        sources::SOURCES
            .iter()
            .filter(|source| self.sources.iter().any(|chosen| chosen.id == source.id))
            .map(|source| source.credit)
            .collect()
    }

    /// The identifiers written back to the profile.
    pub fn tokens(&self) -> String {
        self.sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<&str>>()
            .join(",")
    }
}

/// Why a provider list cannot be enabled.
#[derive(Debug)]
pub enum ProviderError {
    Empty,
    Unknown { id: String },
    NotAdmitted { id: String, reason: String },
    Duplicate { id: String },
}

impl Display for ProviderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "the provider list is empty"),
            Self::Unknown { id } => write!(
                formatter,
                "{id} is not in the source registry, so it cannot be enabled"
            ),
            Self::NotAdmitted { id, reason } => {
                write!(formatter, "{id} cannot be enabled: {reason}")
            }
            Self::Duplicate { id } => write!(formatter, "{id} is listed more than once"),
        }
    }
}

impl Error for ProviderError {}

/// Every setting the application reads.
#[derive(Debug, Clone)]
pub struct Config {
    /// BCP 47 language tag. Default: the environment locale, or `en` when the
    /// environment declares none. Reason: formatting should match the user's
    /// system rather than one country's practice.
    pub locale: Locale,
    /// The unit system used for every displayed measurement. Default: derived
    /// from the locale region, which is metric everywhere except the United
    /// States, Liberia and Myanmar. Reason: metric is the international
    /// convention and the application must not assume a US reader.
    pub units: UnitSystem,
    /// Whether `units` was derived from the locale rather than chosen.
    pub units_from_locale: bool,
    /// The radar station to open with, or `None` for the site picker.
    /// Default: none. Reason: a fresh install has no defensible station.
    pub site: Option<SiteId>,
    /// The searched forecast point restored on the next launch. Default: none.
    /// Reason: the reader chooses where to look.
    pub place: Option<Place>,
    /// The colour theme for the interface. Default: dark. Reason: the radar
    /// colour tables are calibrated against a dark canvas, so the first view
    /// must match them.
    pub theme: Theme,
    /// The independent OpenFreeMap style. Default: liberty, the most detailed
    /// style that draws every label the map expects.
    pub map_style: MapStyle,
    /// The radar color table. Default: standard; the four color-vision tables
    /// are equivalent choices, not fallbacks.
    pub color_table: ColorTable,
    /// How radar gates are sampled onto the screen. Default: nearest. Reason:
    /// a displayed value must be a measured gate, so interpolation is opt-in.
    pub sampling: Sampling,
    /// The storm motion storm-relative velocity is computed against. Default:
    /// the zero vector, which leaves the product unavailable until a reader
    /// supplies a motion rather than inventing one.
    pub storm_motion: StormMotion,
    /// The persistent cache budget. Default: 128 MiB and 256 entries, which
    /// holds several radar volumes and a day of tiles without unbounded growth.
    pub cache: CachePolicy,
    /// The providers this configuration enables. Default: every source the
    /// registry admits and our code has parsed, so a new install can show data
    /// on every domain it supports.
    pub providers: EnabledProviders,
}

impl Config {
    /// The built-in defaults for a real run.
    ///
    /// The locale comes from the environment, and units follow it, so a first
    /// launch matches the system the reader already uses.
    pub fn from_environment() -> Self {
        let locale = Locale::from_environment();
        Self {
            units: UnitSystem::for_region(locale.region()),
            locale,
            ..Self::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            locale: Locale::neutral(),
            units: UnitSystem::Metric,
            units_from_locale: true,
            site: None,
            place: None,
            theme: Theme::default(),
            map_style: MapStyle::default(),
            color_table: ColorTable::default(),
            sampling: Sampling::default(),
            storm_motion: StormMotion::default(),
            cache: CachePolicy::DEFAULT,
            providers: EnabledProviders::all_admitted(),
        }
    }
}

/// The configuration fields, in the order the report prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Locale,
    Units,
    Theme,
    MapStyle,
    Palette,
    Sampling,
    StormSpeed,
    StormBearing,
    Site,
    Place,
    CacheSize,
    CacheEntries,
    Providers,
}

impl Field {
    pub const ALL: [Self; 13] = [
        Self::Locale,
        Self::Units,
        Self::Theme,
        Self::MapStyle,
        Self::Palette,
        Self::Sampling,
        Self::StormSpeed,
        Self::StormBearing,
        Self::Site,
        Self::Place,
        Self::CacheSize,
        Self::CacheEntries,
        Self::Providers,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Locale => "locale",
            Self::Units => "units",
            Self::Theme => "theme",
            Self::MapStyle => "map_style",
            Self::Palette => "palette",
            Self::Sampling => "interpolation",
            Self::StormSpeed => "storm_speed_kmh",
            Self::StormBearing => "storm_bearing_deg",
            Self::Site => "site",
            Self::Place => "place",
            Self::CacheSize => "cache_max_mb",
            Self::CacheEntries => "cache_max_entries",
            Self::Providers => "providers",
        }
    }
}

/// Where a field's effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    Profile(PathBuf),
    Selection,
}

impl Origin {
    fn label(&self) -> String {
        match self {
            Self::Builtin => String::from("built-in"),
            Self::Profile(path) => format!("profile {}", path.display()),
            Self::Selection => String::from("selection"),
        }
    }
}

/// A resolved configuration with the origin of every field.
#[derive(Debug, Clone)]
pub struct Effective {
    pub config: Config,
    origins: Vec<(Field, Origin)>,
}

impl Effective {
    pub fn origin(&self, field: Field) -> Origin {
        self.origins
            .iter()
            .rev()
            .find(|(candidate, _)| *candidate == field)
            .map_or(Origin::Builtin, |(_, origin)| origin.clone())
    }

    /// Prints the effective configuration, the enabled providers and the
    /// credit lines they require.
    pub fn render(&self) -> Result<String, fmt::Error> {
        let mut out = String::new();
        out.push_str("# Effective configuration\n\n");
        for field in Field::ALL {
            let (value, origin) = self.line(field);
            writeln!(
                &mut out,
                "{:<20} = {:<28} # {}",
                field.label(),
                value,
                origin
            )?;
        }
        writeln!(
            &mut out,
            "\n# Providers ({} enabled)\n",
            self.config.providers.sources().len()
        )?;
        for source in self.config.providers.sources() {
            writeln!(&mut out, "{}", source.id.as_str())?;
        }
        out.push_str("\n# Attribution\n");
        for credit in self.config.providers.credits() {
            writeln!(&mut out, "{credit}")?;
        }
        Ok(out)
    }

    fn line(&self, field: Field) -> (String, String) {
        let origin = match field {
            Field::Units if self.origin(Field::Units) == Origin::Builtin => {
                self.origin(Field::Locale)
            }
            _ => self.origin(field),
        };
        let value = self.value(field);
        (value, origin.label())
    }

    fn value(&self, field: Field) -> String {
        let config = &self.config;
        match field {
            Field::Locale => format!("{:?}", config.locale.tag()),
            Field::Units => format!("{:?}", config.units.token()),
            Field::Theme => format!("{:?}", config.theme.token()),
            Field::MapStyle => format!("{:?}", config.map_style.token()),
            Field::Palette => format!("{:?}", config.color_table.token()),
            Field::Sampling => format!("{:?}", config.sampling.token()),
            Field::StormSpeed => format!("{:.1}", config.storm_motion.speed_kmh),
            Field::StormBearing => format!("{:.1}", config.storm_motion.bearing_deg),
            Field::Site => config.site.as_ref().map_or_else(
                || String::from("none (picker opens)"),
                |site| site.as_str().into(),
            ),
            Field::Place => config.place.as_ref().map_or_else(
                || String::from("none"),
                |place| {
                    format!(
                        "{}, {} ({:.4}, {:.4})",
                        place.name, place.country, place.lat, place.lon
                    )
                },
            ),
            Field::CacheSize => format!("{}", config.cache.mib()),
            Field::CacheEntries => format!("{}", config.cache.max_entries()),
            Field::Providers => format!("{}", config.providers.sources().len()),
        }
    }
}

#[derive(Default)]
struct PlaceFields {
    name: Option<String>,
    region: Option<String>,
    country: Option<String>,
    country_code: Option<String>,
    lat: Option<String>,
    lon: Option<String>,
    line: usize,
}

impl PlaceFields {
    const fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.region.is_none()
            && self.country.is_none()
            && self.country_code.is_none()
            && self.lat.is_none()
            && self.lon.is_none()
    }
}

/// The fields one layer sets.
#[derive(Default)]
struct Settings {
    locale: Option<Locale>,
    units: Option<UnitPreference>,
    site: Option<SiteId>,
    place: Option<Place>,
    theme: Option<Theme>,
    map_style: Option<MapStyle>,
    color_table: Option<ColorTable>,
    sampling: Option<Sampling>,
    storm_speed: Option<f64>,
    storm_bearing: Option<f64>,
    cache_mib: Option<u64>,
    cache_entries: Option<usize>,
    providers: Option<EnabledProviders>,
    fields: Vec<Field>,
}

impl Settings {
    fn record(&mut self, field: Field) {
        if !self.fields.contains(&field) {
            self.fields.push(field);
        }
    }
}

fn strip_quotes(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?;
    inner.strip_suffix('"')
}

fn quoted_text(path: &Path, line: usize, value: &str) -> Result<String, ConfigError> {
    strip_quotes(value)
        .map(String::from)
        .ok_or_else(|| ConfigError::Syntax {
            path: path.to_path_buf(),
            line,
        })
}

fn plain_token(value: &str) -> &str {
    strip_quotes(value).unwrap_or(value)
}

fn parse_site(path: &Path, line: usize, value: &str) -> Result<SiteId, ConfigError> {
    let text = strip_quotes(value).ok_or_else(|| ConfigError::Syntax {
        path: path.to_path_buf(),
        line,
    })?;
    SiteId::parse(text).map_err(|source| ConfigError::Site {
        path: path.to_path_buf(),
        line,
        source,
    })
}

fn parse_number(
    path: &Path,
    line: usize,
    field: &'static str,
    value: &str,
) -> Result<f64, ConfigError> {
    let text = plain_token(value);
    text.parse::<f64>().map_err(|_| ConfigError::PlaceNumber {
        path: path.to_path_buf(),
        line,
        field,
        value: String::from(text),
    })
}

fn record_place(
    slot: &mut Option<String>,
    first_line: &mut usize,
    path: &Path,
    line: usize,
    value: &str,
) -> Result<(), ConfigError> {
    *slot = Some(quoted_text(path, line, value)?);
    if *first_line == 0 {
        *first_line = line;
    }
    Ok(())
}

fn finish_place(path: &Path, fields: PlaceFields) -> Result<Option<Place>, ConfigError> {
    if fields.is_empty() {
        return Ok(None);
    }
    let (Some(name), Some(region), Some(country), Some(country_code), Some(lat), Some(lon)) = (
        fields.name,
        fields.region,
        fields.country,
        fields.country_code,
        fields.lat,
        fields.lon,
    ) else {
        return Err(ConfigError::IncompletePlaceEntry {
            path: path.to_path_buf(),
            line: fields.line,
        });
    };
    let lat = lat.parse::<f64>().map_err(|_| ConfigError::PlaceNumber {
        path: path.to_path_buf(),
        line: fields.line,
        field: "latitude",
        value: lat,
    })?;
    let lon = lon.parse::<f64>().map_err(|_| ConfigError::PlaceNumber {
        path: path.to_path_buf(),
        line: fields.line,
        field: "longitude",
        value: lon,
    })?;
    Place::new(&name, &region, &country, &country_code, lat, lon)
        .map(Some)
        .map_err(|source: PlaceError| ConfigError::PlaceEntry {
            path: path.to_path_buf(),
            line: fields.line,
            source,
        })
}

fn apply_field(
    settings: &mut Settings,
    place: &mut PlaceFields,
    path: &Path,
    line: usize,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "locale" => apply_locale(settings, path, line, value)?,
        "units" => apply_units(settings, path, line, value)?,
        "theme" => {
            let text = plain_token(value);
            settings.theme = Some(Theme::parse(text).ok_or_else(|| ConfigError::InvalidTheme {
                path: path.to_path_buf(),
                line,
                value: String::from(text),
            })?);
            settings.record(Field::Theme);
        }
        "map_style" => {
            let text = plain_token(value);
            settings.map_style =
                Some(
                    MapStyle::parse(text).ok_or_else(|| ConfigError::InvalidMapStyle {
                        path: path.to_path_buf(),
                        line,
                        value: String::from(text),
                    })?,
                );
            settings.record(Field::MapStyle);
        }
        "palette" => {
            let text = plain_token(value);
            settings.color_table =
                Some(
                    ColorTable::parse(text).ok_or_else(|| ConfigError::InvalidPalette {
                        path: path.to_path_buf(),
                        line,
                        value: String::from(text),
                    })?,
                );
            settings.record(Field::Palette);
        }
        "interpolation" => {
            let text = plain_token(value);
            settings.sampling =
                Some(
                    Sampling::parse(text).ok_or_else(|| ConfigError::InvalidSampling {
                        path: path.to_path_buf(),
                        line,
                        value: String::from(text),
                    })?,
                );
            settings.record(Field::Sampling);
        }
        "site" => {
            settings.site = Some(parse_site(path, line, value)?);
            settings.record(Field::Site);
        }
        _ => apply_acquisition(settings, place, path, line, key, value)?,
    }
    Ok(())
}

fn apply_locale(
    settings: &mut Settings,
    path: &Path,
    line: usize,
    value: &str,
) -> Result<(), ConfigError> {
    let text = plain_token(value);
    settings.locale = Some(Locale::parse(text).map_err(|source| ConfigError::Locale {
        path: path.to_path_buf(),
        line,
        source,
    })?);
    settings.record(Field::Locale);
    Ok(())
}

fn apply_units(
    settings: &mut Settings,
    path: &Path,
    line: usize,
    value: &str,
) -> Result<(), ConfigError> {
    let text = plain_token(value);
    settings.units = Some(if text == "auto" {
        UnitPreference::Auto
    } else {
        UnitPreference::Fixed(
            UnitSystem::parse(text).ok_or_else(|| ConfigError::InvalidUnit {
                path: path.to_path_buf(),
                line,
                value: String::from(text),
            })?,
        )
    });
    settings.record(Field::Units);
    Ok(())
}

fn apply_acquisition(
    settings: &mut Settings,
    place: &mut PlaceFields,
    path: &Path,
    line: usize,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "storm_speed_kmh" => {
            let speed = parse_number(path, line, "storm speed", value)?;
            settings.storm_speed = Some(speed);
            settings.record(Field::StormSpeed);
            check_motion(path, line, speed, settings.storm_bearing.unwrap_or(0.0))?;
        }
        "storm_bearing_deg" => {
            let bearing = parse_number(path, line, "storm bearing", value)?;
            settings.storm_bearing = Some(bearing);
            settings.record(Field::StormBearing);
            check_motion(path, line, settings.storm_speed.unwrap_or(0.0), bearing)?;
        }
        "cache_max_mb" => {
            let mib = parse_u64(path, line, "cache size", value)?;
            settings.cache_mib = Some(mib);
            CachePolicy::from_mib(mib).map_err(|source| ConfigError::CachePolicy {
                path: path.to_path_buf(),
                line,
                field: "cache_max_mb",
                source,
            })?;
            settings.record(Field::CacheSize);
        }
        "cache_max_entries" => {
            let entries = parse_usize(path, line, "cache entries", value)?;
            settings.cache_entries = Some(entries);
            CachePolicy::DEFAULT
                .with_entries(entries)
                .map_err(|source| ConfigError::CachePolicy {
                    path: path.to_path_buf(),
                    line,
                    field: "cache_max_entries",
                    source,
                })?;
            settings.record(Field::CacheEntries);
        }
        "providers" => {
            let text = quoted_text(path, line, value)?;
            settings.providers =
                Some(
                    EnabledProviders::parse(&text).map_err(|source| ConfigError::Provider {
                        path: path.to_path_buf(),
                        line,
                        source,
                    })?,
                );
            settings.record(Field::Providers);
        }
        _ => apply_place(settings, place, path, line, key, value)?,
    }
    Ok(())
}

fn apply_place(
    settings: &mut Settings,
    place: &mut PlaceFields,
    path: &Path,
    line: usize,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "place_name" => record_place(&mut place.name, &mut place.line, path, line, value)?,
        "place_region" => record_place(&mut place.region, &mut place.line, path, line, value)?,
        "place_country" => record_place(&mut place.country, &mut place.line, path, line, value)?,
        "place_country_code" => {
            record_place(&mut place.country_code, &mut place.line, path, line, value)?;
        }
        "place_lat" => record_place(&mut place.lat, &mut place.line, path, line, value)?,
        "place_lon" => record_place(&mut place.lon, &mut place.line, path, line, value)?,
        _ => {
            return Err(ConfigError::UnknownKey {
                path: path.to_path_buf(),
                line,
                key: String::from(key),
            });
        }
    }
    settings.record(Field::Place);
    Ok(())
}

fn check_motion(path: &Path, line: usize, speed: f64, bearing: f64) -> Result<(), ConfigError> {
    let motion = StormMotion {
        speed_kmh: speed,
        bearing_deg: bearing,
    };
    if motion.is_usable() {
        return Ok(());
    }
    Err(ConfigError::StormMotion {
        path: path.to_path_buf(),
        line,
        speed_kmh: speed,
        bearing_deg: bearing,
    })
}

fn parse_u64(
    path: &Path,
    line: usize,
    field: &'static str,
    value: &str,
) -> Result<u64, ConfigError> {
    plain_token(value)
        .parse::<u64>()
        .map_err(|_| ConfigError::Number {
            path: path.to_path_buf(),
            line,
            field,
            value: String::from(value),
        })
}

fn parse_usize(
    path: &Path,
    line: usize,
    field: &'static str,
    value: &str,
) -> Result<usize, ConfigError> {
    plain_token(value)
        .parse::<usize>()
        .map_err(|_| ConfigError::Number {
            path: path.to_path_buf(),
            line,
            field,
            value: String::from(value),
        })
}

fn parse_text(path: &Path, text: &str) -> Result<Settings, ConfigError> {
    let mut settings = Settings::default();
    let mut place = PlaceFields::default();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| ConfigError::Syntax {
            path: path.to_path_buf(),
            line: number,
        })?;
        apply_field(
            &mut settings,
            &mut place,
            path,
            number,
            key.trim(),
            value.trim(),
        )?;
    }
    settings.place = finish_place(path, place)?;
    Ok(settings)
}

fn read_settings(path: &Path) -> Result<Option<Settings>, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_text(path, &text).map(Some)
}

fn compose(base: &Config, layer: &Settings) -> Config {
    let mut config = base.clone();
    if let Some(locale) = &layer.locale {
        config.locale = locale.clone();
    }
    match layer.units {
        Some(UnitPreference::Fixed(system)) => {
            config.units = system;
            config.units_from_locale = false;
        }
        Some(UnitPreference::Auto) => {
            config.units = UnitSystem::for_region(config.locale.region());
            config.units_from_locale = true;
        }
        None if config.units_from_locale => {
            config.units = UnitSystem::for_region(config.locale.region());
        }
        None => {}
    }
    if layer.site.is_some() {
        config.site = layer.site.clone();
    }
    if layer.place.is_some() {
        config.place = layer.place.clone();
    }
    if let Some(theme) = layer.theme {
        config.theme = theme;
    }
    if let Some(style) = layer.map_style {
        config.map_style = style;
    }
    if let Some(table) = layer.color_table {
        config.color_table = table;
    }
    if let Some(sampling) = layer.sampling {
        config.sampling = sampling;
    }
    if let Some(speed) = layer.storm_speed {
        config.storm_motion.speed_kmh = speed;
    }
    if let Some(bearing) = layer.storm_bearing {
        config.storm_motion.bearing_deg = bearing;
    }
    if let Some(mib) = layer.cache_mib {
        config.cache.max_bytes = mib * 1024 * 1024;
    }
    if let Some(entries) = layer.cache_entries {
        config.cache.max_entries = entries;
    }
    if let Some(providers) = &layer.providers {
        config.providers = providers.clone();
    }
    config
}

fn origins_for(
    profile: Option<&Path>,
    profile_fields: &[Field],
    selection_fields: &[Field],
) -> Vec<(Field, Origin)> {
    let mut origins: Vec<(Field, Origin)> = Field::ALL
        .iter()
        .map(|field| (*field, Origin::Builtin))
        .collect();
    if let Some(path) = profile {
        for field in profile_fields {
            origins.push((*field, Origin::Profile(path.to_path_buf())));
        }
    }
    for field in selection_fields {
        origins.push((*field, Origin::Selection));
    }
    origins
}

/// Resolves the built-in defaults, a profile file and an explicit selection.
///
/// A `None` profile path means the default configuration file, which may be
/// absent. An explicit path that cannot be read is a typed error.
pub fn load_effective(profile: Option<&Path>, selection: &str) -> Result<Effective, ConfigError> {
    let chosen = match profile {
        Some(path) => path.to_path_buf(),
        None => paths::config_file().map_err(|source| ConfigError::Path { source })?,
    };
    let profile_settings = match read_settings(&chosen)? {
        Some(settings) => settings,
        None if profile.is_some() => {
            return Err(ConfigError::MissingProfile { path: chosen });
        }
        None => Settings::default(),
    };
    let selection_settings = parse_text(Path::new("<selection>"), selection)?;
    let base = Config::from_environment();
    let with_profile = compose(&base, &profile_settings);
    let config = compose(&with_profile, &selection_settings);
    let origins = origins_for(
        Some(&chosen),
        &profile_settings.fields,
        &selection_settings.fields,
    );
    Ok(Effective { config, origins })
}

/// Builds the selection layer from `--set key=value` pairs.
///
/// The pairs become the same `key = "value"` text the profile uses, so one
/// parser validates both layers and a selection error names the pair's line.
pub fn selection_text(pairs: &[String]) -> Result<String, ConfigError> {
    let mut text = String::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').ok_or_else(|| ConfigError::Selection {
            field: String::from(pair),
            reason: "expected key=value",
        })?;
        let valid_key = !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if !valid_key || value.is_empty() || value.contains('"') || value.contains('\n') {
            return Err(ConfigError::Selection {
                field: String::from(key),
                reason: "the key is lowercase with underscores and the value carries no quotes",
            });
        }
        writeln!(&mut text, "{key} = \"{value}\"").map_err(|_| ConfigError::Selection {
            field: String::from(key),
            reason: "the selection could not be written",
        })?;
    }
    Ok(text)
}

/// Reads the default profile over the built-in defaults.
pub fn load_default() -> Result<Option<Config>, ConfigError> {
    let path = paths::config_file().map_err(|source| ConfigError::Path { source })?;
    let base = Config::from_environment();
    match read_settings(&path)? {
        Some(settings) => Ok(Some(compose(&base, &settings))),
        None => Ok(Some(base)),
    }
}

fn write_atomic(path: &Path, body: &str) -> Result<(), ConfigError> {
    let directory: &Path = path.parent().unwrap_or(path);
    fs::create_dir_all(directory).map_err(|source| ConfigError::Write {
        path: directory.to_path_buf(),
        source,
    })?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary).map_err(|source| ConfigError::Write {
        path: temporary.clone(),
        source,
    })?;
    file.write_all(body.as_bytes())
        .map_err(|source| ConfigError::Write {
            path: temporary.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| ConfigError::Write {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    fs::rename(&temporary, path).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    sync_directory(directory)?;
    Ok(())
}

fn sync_directory(directory: &Path) -> Result<(), ConfigError> {
    #[cfg(unix)]
    {
        let handle = File::open(directory).map_err(|source| ConfigError::Write {
            path: directory.to_path_buf(),
            source,
        })?;
        handle.sync_all().map_err(|source| ConfigError::Write {
            path: directory.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn serialize(config: &Config) -> String {
    let mut lines: Vec<String> = vec![String::from(HEADER.trim_end())];
    if let Some(site) = &config.site {
        lines.push(format!("site = \"{site}\""));
    }
    lines.push(format!("locale = \"{}\"", config.locale.tag()));
    let units = if config.units_from_locale {
        "auto"
    } else {
        config.units.token()
    };
    lines.push(format!("units = \"{units}\""));
    lines.push(format!("theme = \"{}\"", config.theme.token()));
    lines.push(format!("map_style = \"{}\"", config.map_style.token()));
    lines.push(format!("palette = \"{}\"", config.color_table.token()));
    lines.push(format!("interpolation = \"{}\"", config.sampling.token()));
    lines.push(format!(
        "storm_speed_kmh = \"{:.1}\"",
        config.storm_motion.speed_kmh
    ));
    lines.push(format!(
        "storm_bearing_deg = \"{:.1}\"",
        config.storm_motion.bearing_deg
    ));
    lines.push(format!("cache_max_mb = \"{}\"", config.cache.mib()));
    lines.push(format!(
        "cache_max_entries = \"{}\"",
        config.cache.max_entries()
    ));
    lines.push(format!("providers = \"{}\"", config.providers.tokens()));
    if let Some(place) = &config.place {
        lines.push(format!("place_name = \"{}\"", place.name));
        lines.push(format!("place_region = \"{}\"", place.region));
        lines.push(format!("place_country = \"{}\"", place.country));
        lines.push(format!("place_country_code = \"{}\"", place.country_code));
        lines.push(format!("place_lat = \"{:.6}\"", place.lat));
        lines.push(format!("place_lon = \"{:.6}\"", place.lon));
    }
    let mut body = lines.join("\n");
    body.push('\n');
    body
}

/// Writes the configuration, creating the directory when it is absent.
pub fn save(config: &Config, path: &Path) -> Result<(), ConfigError> {
    write_atomic(path, &serialize(config))
}

pub fn save_default(config: &Config) -> Result<(), ConfigError> {
    let path = paths::config_file().map_err(|source| ConfigError::Path { source })?;
    save(config, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(text: &str) -> Result<Settings, ConfigError> {
        parse_text(Path::new("test.toml"), text)
    }

    fn resolved(profile: &str, selection: &str) -> Result<Config, ConfigError> {
        let base = Config::default();
        let profile = settings(profile)?;
        let selection = settings(selection)?;
        Ok(compose(&compose(&base, &profile), &selection))
    }

    #[test]
    fn a_fresh_install_is_metric_and_locale_neutral() {
        let config = Config::default();
        assert_eq!(config.units, UnitSystem::Metric);
        assert!(config.units_from_locale);
        assert_eq!(config.locale.tag(), "en");
        assert!(config.site.is_none(), "a fresh install has no station");
    }

    #[test]
    fn the_locale_selects_the_unit_system() -> Result<(), ConfigError> {
        assert_eq!(
            resolved("locale = \"en-US\"\n", "")?.units,
            UnitSystem::Imperial
        );
        assert_eq!(
            resolved("locale = \"de-DE\"\n", "")?.units,
            UnitSystem::Metric
        );
        assert_eq!(
            resolved("locale = \"en-GB\"\n", "")?.units,
            UnitSystem::Metric
        );
        Ok(())
    }

    #[test]
    fn an_explicit_unit_system_beats_the_locale() -> Result<(), ConfigError> {
        let config = resolved("locale = \"en-US\"\nunits = \"metric\"\n", "")?;
        assert_eq!(config.units, UnitSystem::Metric);
        assert!(!config.units_from_locale);
        Ok(())
    }

    #[test]
    fn the_selection_beats_the_profile() -> Result<(), ConfigError> {
        let base = Config::default();
        let profile = settings("units = \"imperial\"\ntheme = \"light\"\n")?;
        let selection = settings("units = \"metric\"\n")?;
        let config = compose(&compose(&base, &profile), &selection);
        assert_eq!(config.units, UnitSystem::Metric);
        assert_eq!(config.theme, Theme::Light);
        let origins = origins_for(
            Some(Path::new("profile.toml")),
            &profile.fields,
            &selection.fields,
        );
        let effective = Effective { config, origins };
        assert_eq!(effective.origin(Field::Units), Origin::Selection);
        assert_eq!(
            effective.origin(Field::Theme),
            Origin::Profile(PathBuf::from("profile.toml"))
        );
        assert_eq!(effective.origin(Field::CacheSize), Origin::Builtin);
        Ok(())
    }

    #[test]
    fn an_invalid_value_names_its_field() {
        let units = settings("units = \"kelvin\"\n")
            .err()
            .map(|error| error.to_string());
        assert!(
            units
                .as_deref()
                .is_some_and(|message| message.contains("units")),
            "the units error must name the field"
        );
        let cache = settings("cache_max_mb = 5000\n")
            .err()
            .map(|error| error.to_string());
        assert!(
            cache
                .as_deref()
                .is_some_and(|message| message.contains("cache_max_mb")),
            "the cache error must name the field"
        );
        let locale = settings("locale = \"english_US\"\n")
            .err()
            .map(|error| error.to_string());
        assert!(
            locale
                .as_deref()
                .is_some_and(|message| message.contains("locale")),
            "the locale error must name the field"
        );
        assert!(matches!(
            settings("colormap = \"nws\"\n"),
            Err(ConfigError::UnknownKey { ref key, .. }) if key == "colormap"
        ));
    }

    #[test]
    fn a_non_admitted_provider_cannot_be_enabled() {
        let error = settings("providers = \"dwd-open-data\"\n")
            .err()
            .map(|error| error.to_string());
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("dwd-open-data")),
            "the error must name the refused provider"
        );
    }

    #[test]
    fn an_unknown_provider_cannot_be_enabled() {
        assert!(matches!(
            settings("providers = \"not-a-provider\"\n"),
            Err(ConfigError::Provider {
                source: ProviderError::Unknown { .. },
                ..
            })
        ));
    }

    #[test]
    fn the_default_provider_set_is_admitted_and_credited() {
        let config = Config::default();
        let admitted = crate::sources::SOURCES
            .iter()
            .filter(|source| source.admits().is_ok())
            .count();
        assert_eq!(config.providers.sources().len(), admitted);
        let credits = config.providers.credits();
        assert!(credits.contains(&"Weather data by RainViewer"));
        assert!(credits.contains(&"Sun data by Sunrise-Sunset.org"));
    }

    #[test]
    fn a_chosen_provider_derives_only_its_credit() -> Result<(), ConfigError> {
        let config = resolved("providers = \"rainviewer\"\n", "")?;
        assert_eq!(
            config.providers.credits(),
            vec!["Weather data by RainViewer"]
        );
        assert!(config.providers.enables("rainviewer"));
        assert!(!config.providers.enables("openfreemap"));
        Ok(())
    }

    #[test]
    fn two_enabled_sets_differ_by_exactly_their_credits() -> Result<(), ConfigError> {
        let rain = resolved("providers = \"rainviewer\"\n", "")?;
        let rain_and_basemap = resolved("providers = \"rainviewer,openfreemap\"\n", "")?;
        let extra: Vec<&str> = rain_and_basemap
            .providers
            .credits()
            .into_iter()
            .filter(|credit| !rain.providers.credits().contains(credit))
            .collect();
        assert_eq!(
            extra,
            vec!["OpenFreeMap, OpenMapTiles, OpenStreetMap contributors"]
        );
        Ok(())
    }

    #[test]
    fn saving_is_atomic_and_reloads() -> Result<(), ConfigError> {
        let directory =
            std::env::temp_dir().join(format!("oxrad-config-atomic-{}", std::process::id()));
        fs::create_dir_all(&directory).map_err(|source| ConfigError::Write {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join("config.toml");
        let place = Place::new("Dublin", "County Dublin", "Ireland", "ie", 53.3494, -6.2603)
            .map_err(|source| ConfigError::PlaceEntry {
                path: path.clone(),
                line: 0,
                source,
            })?;
        let config = Config {
            place: Some(place),
            theme: Theme::Light,
            cache: CachePolicy::from_mib(64).map_err(|source| ConfigError::CachePolicy {
                path: path.clone(),
                line: 0,
                field: "cache_max_mb",
                source,
            })?,
            ..Config::default()
        };
        save(&config, &path)?;
        assert!(
            !path.with_extension("tmp").exists(),
            "the temporary file must not survive the rename"
        );
        let text = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        assert!(text.contains("units = \"auto\""));
        let loaded = load_effective(Some(&path), "")?.config;
        assert_eq!(loaded.theme, Theme::Light);
        assert_eq!(loaded.cache.mib(), 64);
        assert_eq!(loaded.units, config.units);
        assert!(loaded.units_from_locale);
        let restored = loaded
            .place
            .ok_or_else(|| ConfigError::MissingProfile { path: path.clone() })?;
        assert_eq!(restored.name, "Dublin");
        assert!((restored.lat - 53.3494).abs() < 1e-6);
        fs::remove_dir_all(&directory).map_err(|source| ConfigError::Write {
            path: directory,
            source,
        })?;
        Ok(())
    }

    #[test]
    fn the_report_prints_values_origins_and_attribution() -> Result<(), ConfigError> {
        let base = Config::default();
        let profile = settings("")?;
        let selection = settings("units = \"imperial\"\nproviders = \"rainviewer\"\n")?;
        let config = compose(&compose(&base, &profile), &selection);
        let origins = origins_for(Some(Path::new("profile.toml")), &[], &selection.fields);
        let effective = Effective { config, origins };
        let report = effective
            .render()
            .map_err(|_| ConfigError::MissingProfile {
                path: PathBuf::from("report"),
            })?;
        assert!(
            report
                .lines()
                .any(|line| line.starts_with("units") && line.contains("# selection")),
            "the report must show the selection as the origin of units"
        );
        assert!(report.contains("\"imperial\""));
        assert!(report.contains("# Providers (1 enabled)"));
        assert!(report.contains("Weather data by RainViewer"));
        Ok(())
    }

    #[test]
    fn a_malformed_selection_is_rejected() {
        assert!(matches!(
            selection_text(&[String::from("units")]),
            Err(ConfigError::Selection { .. })
        ));
        let text = selection_text(&[String::from("units=imperial")]);
        assert_eq!(text.ok().as_deref(), Some("units = \"imperial\"\n"));
    }

    #[test]
    fn storm_motion_round_trips_and_rejects_impossible_values() -> Result<(), ConfigError> {
        let config = resolved(
            "storm_speed_kmh = \"83.5\"\nstorm_bearing_deg = \"245.0\"\n",
            "",
        )?;
        assert!((config.storm_motion.speed_kmh - 83.5).abs() < 1e-9);
        assert!((config.storm_motion.bearing_deg - 245.0).abs() < 1e-9);
        assert!(matches!(
            settings("storm_bearing_deg = \"400\"\n"),
            Err(ConfigError::StormMotion { line: 1, .. })
        ));
        Ok(())
    }

    #[test]
    fn an_incomplete_saved_place_names_the_problem() {
        assert!(matches!(
            settings("place_name = \"Dublin\"\n"),
            Err(ConfigError::IncompletePlaceEntry { line: 1, .. })
        ));
    }
}
