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

use crate::error::ConfigError;
use crate::paths;
use crate::stations::SiteId;

const HEADER: &str = "# oxrad configuration.\n";

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// The radar station to open with, or `None` for the site picker.
    pub site: Option<SiteId>,
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

fn parse_text_line(path: &Path, text: &str) -> Result<Config, ConfigError> {
    let mut config = Config::default();
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
            _ => {
                return Err(ConfigError::UnknownKey {
                    path: path.to_path_buf(),
                    line: number,
                    key: String::from(key),
                });
            }
        }
    }
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
    let body = config.site.as_ref().map_or_else(
        || String::from(HEADER),
        |site| format!("{HEADER}site = \"{site}\"\n"),
    );
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
            parse("palette = \"nws\"\n"),
            Err(ConfigError::UnknownKey { line: 1, ref key, .. }) if key == "palette"
        ));
    }

    #[test]
    fn save_and_load_round_trip_the_site() -> Result<(), ConfigError> {
        let directory = std::env::temp_dir().join("oxrad-config-test");
        std::fs::create_dir_all(&directory).map_err(|source| ConfigError::Write {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join("config.toml");
        let id = SiteId::parse("KOHX").map_err(|source| ConfigError::Site {
            path: path.clone(),
            line: 0,
            source,
        })?;
        save(&Config { site: Some(id) }, &path)?;
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
        Ok(())
    }
}
