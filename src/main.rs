// Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! Get homewards commuting rules

use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, TimeZone, Utc};
use log::{debug, warn};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(from = "i64", into = "i64")]
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

impl From<Timestamp> for i64 {
    fn from(ts: Timestamp) -> Self {
        ts.milliseconds_since_epoch
    }
}

impl ToString for Timestamp {
    fn to_string(&self) -> String {
        self.milliseconds_since_epoch.to_string()
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(ts: Timestamp) -> Self {
        Utc.timestamp_millis(ts.milliseconds_since_epoch)
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(v: DateTime<Utc>) -> Self {
        Self {
            milliseconds_since_epoch: v.timestamp_millis(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Station {
    id: String,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum Location {
    #[serde(rename = "station")]
    Station(Station),
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocationsResponse {
    locations: Vec<Location>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionPart {
    from: Location,
    to: Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    from: Location,
    departure: Timestamp,
    to: Location,
    arrival: Timestamp,
    #[serde(rename = "connectionPartList")]
    connection_parts: Vec<ConnectionPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    fn find_unambiguous_station_by_name<S: AsRef<str>>(&self, name: S) -> Result<Station> {
        let mut stations: Vec<Station> = self
            .get_location_by_name(name.as_ref())?
            .into_iter()
            .filter_map(|loc| match loc {
                Location::Station(station) => Some(station),
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

/// The configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    /// The name of the start station.
    start: String,
    /// The name of the destination station.
    destination: String,
    /// How much time to account for to walk to the start station.
    walk_to_start_in_minutes: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectionsCache {
    config: Config,
    connections: Vec<Connection>,
}

impl ConnectionsCache {
    fn cache_path() -> PathBuf {
        dirs::cache_dir()
            .expect("cache directory missing")
            .join("de.swsnr.home")
            .join("connections")
    }

    fn load() -> Result<Self> {
        let path = Self::cache_path();
        let mut source = File::open(&path)
            .with_context(|| format!("Failed to open cache file {} for reading", path.display()))?;

        let mut buf = Vec::new();
        source
            .read_to_end(&mut buf)
            .with_context(|| format!("Failed to read cache from {}", path.display()))?;
        flexbuffers::from_slice(&buf)
            .with_context(|| format!("Failed to deserialize cache from {}", path.display()))
    }

    fn save(&self) -> Result<()> {
        let path = Self::cache_path();
        let cache_dir = path.parent().with_context(|| {
            format!(
                "Failed to determine directory of cache path {}",
                path.display()
            )
        })?;
        std::fs::create_dir_all(cache_dir).with_context(|| {
            format!(
                "Failed to create cache directory at {}",
                cache_dir.display()
            )
        })?;
        let mut sink = File::create(&path)
            .with_context(|| format!("Failed to open cache file {} for writing", path.display()))?;
        let buffer = flexbuffers::to_vec(self)
            .with_context(|| "Failed to serialize connection cache".to_string())?;
        sink.write_all(&buffer)
            .with_context(|| format!("Failed to write cache to {}", path.display()))?;
        Ok(())
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
            if let Location::Station(station) = &self.connection.connection_parts[0].to {
                write!(f, ", via {}", station.name)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

fn display_with_walk_time(connection: &'_ Connection, walk_time: Duration) -> impl Display + '_ {
    ConnectionDisplay::new(connection, walk_time)
}

fn load_config() -> Result<Config> {
    let config_path = dirs::config_dir()
        .with_context(|| "Missing HOME directory".to_string())?
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

    let walk_time_to_start = Duration::minutes(config.walk_to_start_in_minutes as i64);
    let desired_departure_time = Utc::now() + walk_time_to_start;

    // start:
    // destination: StationId::resolve_name_unambiguously(mvg, &config.destination)
    // .with_context(|| format!("Failed to resolve station {}", &config.destination))?,
    // walk_to_start: ,
    let cache = ConnectionsCache::load()
        .map_err(|err| {
            debug!("Failed to read cached connections: {:#}", err);
            err
        })
        .ok()
        // Discard cache if config doesn't match
        .filter(|cache| cache.config == config)
        .map(|cache| {
            debug!("Cached passed config check");
            cache
        })
        // Discard cache if it's empty or if the first connection departs before the desired departure time,
        // that is if the connection cache's outdated.
        .filter(|cache| {
            cache.connections.first().map_or(false, |r| {
                desired_departure_time.timestamp_millis() < r.departure.milliseconds_since_epoch
            })
        });
    let connections = match cache {
        Some(ConnectionsCache { connections, .. }) => {
            debug!("Using cached connections");
            connections
        }
        None => {
            debug!("Cache invalidated, fetching routes");
            let mvg = Mvg::new()?;
            let start = mvg
                .find_unambiguous_station_by_name(&config.start)
                .with_context(|| format!("Failed to find station {}", &config.start))?;
            let destination = mvg
                .find_unambiguous_station_by_name(&config.destination)
                .with_context(|| format!("Failed to find station {}", &config.destination))?;
            let cache = ConnectionsCache {
                config,
                connections: mvg.get_connections(
                    &start.id,
                    &destination.id,
                    desired_departure_time.into(),
                )?,
            };
            if let Err(error) = cache.save() {
                warn!("Failed to cache routes: {:#}", error);
            }
            cache.connections
        }
    };

    for connection in connections.iter().take(args.number_of_connections as usize) {
        println!(
            "{}",
            display_with_walk_time(&connection, walk_time_to_start)
        );
    }

    // TODO: Add command line flag to clear cache forcibly

    Ok(())
}

fn main() {
    env_logger::init();

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
