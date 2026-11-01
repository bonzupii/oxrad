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
//! The user configuration file.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::colormap::ColorTable;
use crate::error::ConfigError;
use crate::paths;
use crate::place::{Place, PlaceError};
use crate::stations::SiteId;
use crate::theme::Theme;
use crate::units::UnitSystem;

const HEADER: &str = "# oxrad configuration.\n";

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// The radar station to open with, or `None` for the site picker.
    pub site: Option<SiteId>,
    /// The searched forecast point restored on the next launch.
    pub place: Option<Place>,
    /// The unit system used for every displayed measurement.
    pub units: UnitSystem,
    /// The colour theme for the interface.
    pub theme: Theme,
    /// The radar color table, including color-vision-safe options.
    pub color_table: ColorTable,
}

fn strip_quotes(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?;
    inner.strip_suffix('"')
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

fn quoted_text(path: &Path, line: usize, value: &str) -> Result<String, ConfigError> {
    strip_quotes(value)
        .map(String::from)
        .ok_or_else(|| ConfigError::Syntax {
            path: path.to_path_buf(),
            line,
        })
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

fn parse_text_line(path: &Path, text: &str) -> Result<Config, ConfigError> {
    let mut config = Config::default();
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
        let key = key.trim();
        let value = value.trim();
        match key {
            "site" => config.site = Some(parse_site(path, number, value)?),
            "units" => {
                let text = strip_quotes(value).ok_or_else(|| ConfigError::Syntax {
                    path: path.to_path_buf(),
                    line: number,
                })?;
                config.units = UnitSystem::parse(text).ok_or_else(|| ConfigError::InvalidUnit {
                    path: path.to_path_buf(),
                    line: number,
                    value: String::from(text),
                })?;
            }
            "theme" => {
                let text = strip_quotes(value).ok_or_else(|| ConfigError::Syntax {
                    path: path.to_path_buf(),
                    line: number,
                })?;
                config.theme = Theme::parse(text).ok_or_else(|| ConfigError::InvalidTheme {
                    path: path.to_path_buf(),
                    line: number,
                    value: String::from(text),
                })?;
            }
            "palette" => {
                let text = strip_quotes(value).ok_or_else(|| ConfigError::Syntax {
                    path: path.to_path_buf(),
                    line: number,
                })?;
                config.color_table =
                    ColorTable::parse(text).ok_or_else(|| ConfigError::InvalidPalette {
                        path: path.to_path_buf(),
                        line: number,
                        value: String::from(text),
                    })?;
            }
            "place_name" => {
                place.name = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            "place_region" => {
                place.region = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            "place_country" => {
                place.country = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            "place_country_code" => {
                place.country_code = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            "place_lat" => {
                place.lat = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            "place_lon" => {
                place.lon = Some(quoted_text(path, number, value)?);
                if place.line == 0 {
                    place.line = number;
                }
            }
            _ => {
                return Err(ConfigError::UnknownKey {
                    path: path.to_path_buf(),
                    line: number,
                    key: String::from(key),
                });
            }
        }
    }
    config.place = finish_place(path, place)?;
    Ok(config)
}

/// Reads `path`, reporting an absent file separately from a malformed one.
pub fn load(path: &Path) -> Result<Option<Config>, ConfigError> {
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
    parse_text_line(path, &text).map(Some)
}

/// Writes the configuration, creating `~/.oxrad` when it is absent.
pub fn save(config: &Config, path: &Path) -> Result<(), ConfigError> {
    let directory: &Path = path.parent().unwrap_or(path);
    fs::create_dir_all(directory).map_err(|source| ConfigError::Write {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut lines: Vec<String> = vec![String::from(HEADER.trim_end())];
    if let Some(site) = &config.site {
        lines.push(format!("site = \"{site}\""));
    }
    lines.push(format!("units = \"{}\"", config.units.token()));
    lines.push(format!("theme = \"{}\"", config.theme.token()));
    lines.push(format!("palette = \"{}\"", config.color_table.token()));
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
    fs::write(path, body).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Loads the configuration from the application directory.
pub fn load_default() -> Result<Option<Config>, ConfigError> {
    let path: PathBuf = paths::config_file().map_err(|source| ConfigError::Path { source })?;
    load(&path)
}

/// Saves the configuration in the application directory.
pub fn save_default(config: &Config) -> Result<(), ConfigError> {
    let path = paths::config_file().map_err(|source| ConfigError::Path { source })?;
    save(config, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        parse_text_line(Path::new("config.toml"), text)
    }

    #[test]
    fn reads_a_saved_site() -> Result<(), ConfigError> {
        let config = parse("# oxrad configuration.\nsite = \"KPAH\"\n")?;
        assert_eq!(config.site.as_ref().map(SiteId::as_str), Some("KPAH"));
        Ok(())
    }

    #[test]
    fn an_empty_file_leaves_the_site_unset() -> Result<(), ConfigError> {
        let config = parse("# oxrad configuration.\n")?;
        assert!(config.site.is_none());
        Ok(())
    }

    #[test]
    fn malformed_lines_name_the_line() {
        assert!(matches!(
            parse("site KPAH\n"),
            Err(ConfigError::Syntax { line: 1, .. })
        ));
        assert!(matches!(
            parse("site = KPAH\n"),
            Err(ConfigError::Syntax { line: 1, .. })
        ));
        assert!(matches!(
            parse("colormap = \"nws\"\n"),
            Err(ConfigError::UnknownKey { line: 1, ref key, .. }) if key == "colormap"
        ));
    }

    #[test]
    fn save_and_load_round_trip_the_site() -> Result<(), ConfigError> {
        let directory =
            std::env::temp_dir().join(format!("oxrad-config-test-{}", std::process::id()));
        std::fs::create_dir(&directory).map_err(|source| ConfigError::Write {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join("config.toml");
        let id = SiteId::parse("KOHX").map_err(|source| ConfigError::Site {
            path: path.clone(),
            line: 0,
            source,
        })?;
        save(
            &Config {
                site: Some(id),
                place: None,
                units: UnitSystem::Metric,
                theme: Theme::Dark,
                color_table: ColorTable::Standard,
            },
            &path,
        )?;
        match load(&path)? {
            Some(config) => {
                assert_eq!(config.site.as_ref().map(SiteId::as_str), Some("KOHX"));
            }
            None => {
                return Err(ConfigError::Syntax { path, line: 0 });
            }
        }
        let absent = directory.join("absent.toml");
        assert!(load(&absent)?.is_none());
        std::fs::remove_dir_all(&directory).map_err(|source| ConfigError::Write {
            path: directory,
            source,
        })?;
        Ok(())
    }
    #[test]
    fn a_saved_place_round_trips_without_restoring_a_site() -> Result<(), ConfigError> {
        let directory =
            std::env::temp_dir().join(format!("oxrad-config-place-test-{}", std::process::id()));
        std::fs::create_dir(&directory).map_err(|source| ConfigError::Write {
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
        save(
            &Config {
                site: None,
                place: Some(place),
                units: UnitSystem::Metric,
                theme: Theme::Dark,
                color_table: ColorTable::Deuteranopia,
            },
            &path,
        )?;
        let loaded = load(&path)?.ok_or(ConfigError::Syntax {
            path: path.clone(),
            line: 0,
        })?;
        assert_eq!(loaded.color_table, ColorTable::Deuteranopia);
        let restored = loaded.place.ok_or(ConfigError::Syntax {
            path: path.clone(),
            line: 0,
        })?;
        assert_eq!(restored.name, "Dublin");
        assert_eq!(restored.country_code, "ie");
        assert!((restored.lat - 53.3494).abs() < 1e-6);
        assert!(
            loaded.site.is_none(),
            "a place must not restore a radar site"
        );
        std::fs::remove_dir_all(&directory).map_err(|source| ConfigError::Write {
            path: directory,
            source,
        })?;
        Ok(())
    }

    #[test]
    fn units_round_trip_and_reject_unknown_values() -> Result<(), ConfigError> {
        let config = parse("units = \"imperial\"\n")?;
        assert_eq!(config.units, UnitSystem::Imperial);
        assert!(matches!(
            parse("units = \"kelvin\"\n"),
            Err(ConfigError::InvalidUnit {
                line: 1,
                ref value,
                ..
            }) if value == "kelvin"
        ));
        Ok(())
    }

    #[test]
    fn palette_round_trips_and_rejects_unknown_values() -> Result<(), ConfigError> {
        let config = parse("palette = \"tritanopia\"\n")?;
        assert_eq!(config.color_table, ColorTable::Tritanopia);
        assert!(matches!(
            parse("palette = \"nws\"\n"),
            Err(ConfigError::InvalidPalette {
                line: 1,
                ref value,
                ..
            }) if value == "nws"
        ));
        Ok(())
    }

    #[test]
    fn theme_round_trips_and_rejects_unknown_values() -> Result<(), ConfigError> {
        let config = parse("theme = \"light\"\n")?;
        assert_eq!(config.theme, Theme::Light);
        assert!(matches!(
            parse("theme = \"sepia\"\n"),
            Err(ConfigError::InvalidTheme {
                line: 1,
                ref value,
                ..
            }) if value == "sepia"
        ));
        Ok(())
    }

    #[test]
    fn an_incomplete_saved_place_names_the_problem() {
        assert!(matches!(
            parse("place_name = \"Dublin\"\n"),
            Err(ConfigError::IncompletePlaceEntry { line: 1, .. })
        ));
    }
}
