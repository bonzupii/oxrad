// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
//! Every file and directory path the application uses.

use std::path::PathBuf;

use crate::error::PathError;

const APPLICATION_DIRECTORY: &str = ".oxrad";
const CONFIG_FILE: &str = "config.toml";

fn home_directory() -> Result<PathBuf, PathError> {
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(variable)
            && !value.is_empty()
        {
            return Ok(PathBuf::from(value));
        }
    }
    Err(PathError::HomeUnavailable)
}

/// `~/.oxrad`.
pub fn application_directory() -> Result<PathBuf, PathError> {
    Ok(home_directory()?.join(APPLICATION_DIRECTORY))
}

/// `~/.oxrad/config.toml`.
pub fn config_file() -> Result<PathBuf, PathError> {
    Ok(application_directory()?.join(CONFIG_FILE))
}
