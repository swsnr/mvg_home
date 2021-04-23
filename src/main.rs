// Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! Get homewards commuting rules

use std::fs::File;
use std::io::Read;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

use chrono::{DateTime, Duration, TimeZone, Utc};
use std::fmt::{Display, Formatter};
use std::ops::Add;
use url::Url;

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(from = "i64")]
struct Timestamp {
    milliseconds_since_epoch: i64,
}

impl From<i64> for Timestamp {
    fn from(v: i64) -> Self {
        Self {
            milliseconds_since_epoch: v,
        }
    }
}

impl ToString for Timestamp {
    fn to_string(&self) -> String {
        self.milliseconds_since_epoch.to_string()
    }
}

impl Into<DateTime<Utc>> for Timestamp {
    fn into(self) -> DateTime<Utc> {
        Utc.timestamp_millis(self.milliseconds_since_epoch)
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(v: DateTime<Utc>) -> Self {
        Self {
            milliseconds_since_epoch: v.timestamp_millis(),
        }
    }
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

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConnectionPart {
    from: Location,
    to: Location,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Connection {
    from: Location,
    departure: Timestamp,
    to: Location,
    arrival: Timestamp,
    #[serde(rename = "connectionPartList")]
    connection_parts: Vec<ConnectionPart>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]

struct ConnectionsResponse {
    connection_list: Vec<Connection>,
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
        let response = self
            .client
            .get(url.clone())
            .header("Accept", "application/json")
            .send()
            .with_context(|| {
                format!("Failed to query URL to resolve location {}", name.as_ref())
            })?;
        response
            .json::<LocationsResponse>()
            .map(|response| response.locations)
            .with_context(|| format!("Failed to parse response from {}", url))
    }

    fn get_connections<S: AsRef<str>, T: AsRef<str>>(
        &self,
        from_station_id: S,
        to_station_id: T,
        start: Timestamp,
    ) -> Result<Vec<Connection>> {
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fahrinfo/routing",
            &[
                ("fromStation", from_station_id.as_ref()),
                ("toStation", to_station_id.as_ref()),
                ("time", &start.to_string()),
            ],
        )?;
        let response = self
            .client
            .get(url.clone())
            .header("Accept", "application/json")
            .send()
            .with_context(|| format!("Failed to query URL to resolve location {}", url.as_ref()))?;
        response
            .json::<ConnectionsResponse>()
            .map(|response| response.connection_list)
            .with_context(|| format!("Failed to decode response from {}", url))
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

#[derive(Debug)]
struct ConnectionPlan {
    /// The ID of the start station.
    start: StationId,
    /// The ID of the destination station.
    destination: StationId,
    /// The time to walk to the start station.
    walk_to_start: Duration,
}

impl ConnectionPlan {
    fn resolve_from_config(mvg: &Mvg, config: &Config) -> Result<Self> {
        Ok(Self {
            start: StationId::resolve_name_unambiguously(mvg, &config.start)
                .with_context(|| format!("Failed to resolve station {}", &config.start))?,
            destination: StationId::resolve_name_unambiguously(mvg, &config.destination)
                .with_context(|| format!("Failed to resolve station {}", &config.destination))?,
            walk_to_start: Duration::minutes(config.walk_to_start_in_minutes as i64),
        })
    }
}

struct ConnectionDisplay<'a> {
    connection: &'a Connection,
    walk_time: Duration,
}

impl<'a> ConnectionDisplay<'a> {
    fn new(connection: &'a Connection, walk_time: Duration) -> Self {
        Self {
            connection,
            walk_time,
        }
    }
}

impl<'a> Display for ConnectionDisplay<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let departure: DateTime<Utc> = self.connection.departure.into();
        let arrival: DateTime<Utc> = self.connection.arrival.into();
        let start = departure - self.walk_time;
        let start_in = start - Utc::now();

        write!(
            f,
            "🚆 In {} min, dep. {} arr. {}",
            ((start_in.num_seconds() as f64) / 60.0).ceil(),
            departure.naive_local().time().format("%H:%M"),
            arrival.naive_local().format("%H:%M")
        )?;
        if 2 <= self.connection.connection_parts.len() {
            if let Location::Station { name, .. } = &self.connection.connection_parts[0].to {
                write!(f, ", via {}", name)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

fn display_with_walk_time<'a>(
    connection: &'a Connection,
    walk_time: Duration,
) -> impl Display + 'a {
    ConnectionDisplay::new(connection, walk_time)
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

#[derive(Debug, Clone)]
struct Args {
    number_of_connections: u16,
}

fn process_args(args: Args) -> Result<()> {
    let config = load_config()?;

    let mvg = Mvg::new()?;
    let plan = ConnectionPlan::resolve_from_config(&mvg, &config)?;
    let start = Utc::now().add(plan.walk_to_start);
    let connections = mvg.get_connections(plan.start.0, plan.destination.0, start.into())?;

    for connection in connections.iter().take(args.number_of_connections as usize) {
        println!(
            "{}",
            display_with_walk_time(&connection, plan.walk_to_start)
        );
    }

    // TODO: Cache routes and print routes from cache
    // TODO: Evict cache if the first cached route is in the past
    // TODO: Add command line flag to clear cache forcibly

    Ok(())
}

fn main() {
    use clap::*;
    let matches = app_from_crate!()
        .setting(AppSettings::UnifiedHelpMessage)
        .setting(AppSettings::DontCollapseArgsInUsage)
        .setting(AppSettings::DeriveDisplayOrder)
        .set_term_width(80)
        .arg(
            Arg::with_name("number_of_connections")
                .short("n")
                .long("connections")
                .takes_value(true)
                .value_name("N")
                .default_value("10")
                .help("The number of connections to show"),
        )
        .get_matches();
    let args = Args {
        number_of_connections: value_t!(matches, "number_of_connections", u16)
            .unwrap_or_else(|e| e.exit()),
    };
    if let Err(err) = process_args(args) {
        eprintln!("{:#}", err);
        std::process::exit(1);
    }
}
