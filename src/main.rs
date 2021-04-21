// Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! Get homewards commuting rules

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::time::Duration;

/// The configuration file.
#[derive(Debug, Deserialize)]
struct Config {
    /// The name of the start station.
    start: String,
    /// The name of the destination station.
    destination: String,
    /// How much time to account for to walk to the start station.
    walk_to_start_in_minutes: u8,
}

/// A station ID.
#[derive(Debug)]
struct StationId(String);

#[derive(Debug)]
struct Route {
    /// The ID of the start station.
    start: StationId,
    /// The ID of the destination station.
    destination: StationId,
    /// The time to walk to the start station.
    walk_to_start: Duration,
}

fn load_config() -> Result<Config> {
    let config_path = dirs::config_dir()
        .with_context(|| format!("Missing HOME directory"))?
        .join("de.swsnr.home")
        .join("home.toml");
    let mut source = File::open(&config_path).with_context(|| {
        format!(
            "Failed to open configuration file at {}",
            config_path.display()
        )
    })?;
    let mut buffer = Vec::new();
    source.read_to_end(&mut buffer).with_context(|| {
        format!(
            "Failed to read configuration file at {}",
            config_path.display()
        )
    })?;
    toml::from_slice(&buffer).with_context(|| {
        format!(
            "Failed to parse configuration from {}",
            config_path.display()
        )
    })
}

fn main() -> Result<()> {
    let config = load_config()?;
    println!("{:?}", config);

    Ok(())
}
