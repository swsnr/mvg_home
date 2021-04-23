// Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! Get homewards commuting rules

use std::fs::File;
use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

use url::Url;

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

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
enum Location {
    #[serde(rename = "station")]
    Station { id: String, name: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, Clone)]
struct LocationsResponse {
    locations: Vec<Location>,
}

struct Mvg {
    client: Client,
}

impl Mvg {
    fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::ClientBuilder::new()
                .user_agent("home")
                .build()?,
        })
    }

    fn get_location_by_name<S: AsRef<str>>(&self, name: S) -> Result<Vec<Location>> {
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fahrinfo/location/queryWeb",
            &[("q", name.as_ref())],
        )?;
        let response =
            self.client.get(url.clone()).send().with_context(|| {
                format!("Failed query URL to resolve location {}", name.as_ref())
            })?;
        response
            .json::<LocationsResponse>()
            .map(|response| response.locations)
            .with_context(|| format!("Failed to parse response from {}", url))
    }
}

/// A station ID.
#[derive(Debug)]
struct StationId(String);

impl StationId {
    fn resolve_name_unambiguously<S: AsRef<str>>(mvg: &Mvg, name: S) -> Result<Self> {
        let locations = mvg.get_location_by_name(name.as_ref())?;
        let mut stations: Vec<StationId> = locations
            .into_iter()
            .filter_map(|loc| match loc {
                Location::Station { id, .. } => Some(StationId(id)),
                _ => None,
            })
            .collect();
        if 1 < stations.len() {
            Err(anyhow!("Ambiguous results for {}", name.as_ref()))
        } else {
            stations
                .pop()
                .with_context(|| format!("No matches for {}", name.as_ref()))
        }
    }
}

#[derive(Debug)]
struct RoutePlan {
    /// The ID of the start station.
    start: StationId,
    /// The ID of the destination station.
    destination: StationId,
    /// The time to walk to the start station.
    walk_to_start: Duration,
}

impl RoutePlan {
    fn resolve_from_config(mvg: &Mvg, config: &Config) -> Result<Self> {
        Ok(Self {
            start: StationId::resolve_name_unambiguously(mvg, &config.start)
                .with_context(|| format!("Failed to resolve station {}", &config.start))?,
            destination: StationId::resolve_name_unambiguously(mvg, &config.destination)
                .with_context(|| format!("Failed to resolve station {}", &config.destination))?,
            walk_to_start: Duration::from_secs((config.walk_to_start_in_minutes as u64) * 60),
        })
    }
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

fn doit() -> Result<()> {
    let config = load_config()?;
    println!("{:?}", config);

    let mvg = Mvg::new()?;
    let plan = RoutePlan::resolve_from_config(&mvg, &config)?;
    dbg!(plan);

    // TODO: Query for the route plan to get actual routes
    // TODO: Print a given number of routes including the first transit station
    // TODO: Add argument for number of routes
    // TODO: Cache routes and print routes from cache
    // TODO: Evict cache if the first cached route is in the past
    // TODO: Add command line flag to clear cache forcibly

    Ok(())
}

fn main() {
    if let Err(err) = doit() {
        eprintln!("{:#}", err);
        std::process::exit(1);
    }
}
