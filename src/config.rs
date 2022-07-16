// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::Duration;

/// The configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub connections: Vec<DesiredConnection>,
}

/// A desired connection in the config file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredConnection {
    /// The name of the start station.
    pub start: String,
    /// The name of the destination station.
    pub destination: String,
    /// How much time to account for to walk to the start station.
    pub walk_to_start_in_minutes: u8,
}

impl DesiredConnection {
    pub fn walk_to_start(&self) -> Duration {
        Duration::minutes(self.walk_to_start_in_minutes as i64)
    }
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let contents = std::fs::read(path.as_ref()).with_context(|| {
            format!(
                "Failed to read configuration file from {}",
                path.as_ref().display()
            )
        })?;
        toml::from_slice(&contents).with_context(|| {
            format!(
                "Failed to parse configuration from {}",
                path.as_ref().display()
            )
        })
    }

    /// Load config from `$XDG_CONFIG_HOME`.
    pub fn from_default_location() -> Result<Self> {
        Self::from_file(
            dirs::config_dir()
                .with_context(|| "Missing HOME directory".to_string())?
                .join("de.swsnr.home")
                .join("home.toml"),
        )
    }
}
