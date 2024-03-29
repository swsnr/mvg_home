// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Duration;
use serde::{Deserialize, Serialize};

/// The configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub connections: Vec<DesiredConnection>,
}

mod human_readable_duration {
    use chrono::Duration;
    use serde::de::Unexpected;
    use serde::{de, Deserialize};
    use serde::{ser, Serialize};
    use serde::{Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let value = String::deserialize(deserializer)?;
            Duration::from_std(humantime::parse_duration(&value).map_err(|err| {
                de::Error::invalid_value(Unexpected::Str(&value), &format!("{}", err).as_str())
            })?)
            .map_err(|err| {
                de::Error::invalid_value(Unexpected::Str(&value), &format!("{}", err).as_str())
            })
        } else {
            Ok(Duration::seconds(i64::deserialize(deserializer)?))
        }
    }

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let std_duration = value
                .to_std()
                .map_err(|error| ser::Error::custom(format!("Invalid range: {}", error)))?;

            let formatted = ::humantime::format_duration(std_duration);
            serializer.serialize_str(&formatted.to_string())
        } else {
            value.num_seconds().serialize(serializer)
        }
    }
}

/// A desired connection in the config file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredConnection {
    /// The name of the start station.
    pub start: String,
    /// The name of the destination station.
    pub destination: String,
    /// How much time to account for to walk to the start station.
    #[serde(with = "human_readable_duration")]
    pub walk_to_start: Duration,
    /// A list of product labels (e.g. S2, 12, 947) to ignore
    #[serde(default)]
    pub ignore_starting_with: Vec<String>,
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let data = std::fs::read(path.as_ref()).with_context(|| {
            format!(
                "Failed to read configuration file from {}",
                path.as_ref().display()
            )
        })?;
        let contents = std::str::from_utf8(&data).with_context(|| {
            format!(
                "Contents of configuration file {} are not valid UTF-8",
                path.as_ref().display()
            )
        })?;
        toml::from_str(contents).with_context(|| {
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
