// Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! MVG connections for the way home.

use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{Read, Write};
use std::ops::Not;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use log::{debug, warn};
use reqwest::blocking::Client;
use reqwest::Proxy;
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};
use url::Url;

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

mod unix_millis {
    use serde::{
        de::{self, Unexpected},
        Deserialize, Deserializer, Serializer,
    };
    use time::OffsetDateTime;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)? / 1000;
        OffsetDateTime::from_unix_timestamp(value).map_err(|err| {
            de::Error::invalid_value(Unexpected::Signed(value), &format!("{}", err).as_str())
        })
    }

    pub fn serialize<S>(value: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(value.unix_timestamp() * 1000)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    from: Location,
    #[serde(with = "unix_millis")]
    departure: OffsetDateTime,
    to: Location,
    #[serde(with = "unix_millis")]
    arrival: OffsetDateTime,
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
        let proxy = system_proxy::default();
        Ok(Self {
            client: reqwest::blocking::ClientBuilder::new()
                .user_agent("home")
                .proxy(Proxy::custom(move |url| proxy.for_url(url)))
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
            // If we find more than one station let's see if there's one which
            // matches the given name exactly.
            match stations.iter().find(|s| s.name == name.as_ref()) {
                // Uhg, a clone, but I have no idea to teach rust that we can
                // safely move out of "stations" here.
                Some(station) => Ok(station.clone()),
                None => Err(anyhow!(
                    "Ambiguous results for {}: {}",
                    name.as_ref(),
                    stations
                        .into_iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
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
        start: OffsetDateTime,
    ) -> Result<Vec<Connection>> {
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fahrinfo/routing",
            &[
                ("fromStation", from_station_id.as_ref()),
                ("toStation", to_station_id.as_ref()),
                ("time", &(start.unix_timestamp() * 1000).to_string()),
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
    local_offset: UtcOffset,
}

impl<'a> ConnectionDisplay<'a> {
    fn new(connection: &'a Connection, walk_time: Duration, local_offset: UtcOffset) -> Self {
        Self {
            connection,
            walk_time,
            local_offset,
        }
    }
}

impl<'a> Display for ConnectionDisplay<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let departure = self.connection.departure.to_offset(self.local_offset);
        let arrival = self.connection.arrival.to_offset(self.local_offset);
        let start = departure - self.walk_time;
        let start_in = start - OffsetDateTime::now_utc().to_offset(self.local_offset);

        let hh_mm = format_description!("[hour]:[minute]");
        write!(
            f,
            "🚆 In {} min, dep. {} arr. {}",
            ((start_in.whole_seconds() as f64) / 60.0).ceil(),
            departure.time().format(hh_mm).unwrap(),
            arrival.time().format(hh_mm).unwrap()
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

fn display_with_walk_time(
    connection: &'_ Connection,
    walk_time: Duration,
    offset: UtcOffset,
) -> impl Display + '_ {
    ConnectionDisplay::new(connection, walk_time, offset)
}

fn load_config<P: AsRef<Path>>(config_path: P) -> Result<Config> {
    let mut source = File::open(config_path.as_ref()).with_context(|| {
        format!(
            "Failed to open configuration file at {}",
            config_path.as_ref().display()
        )
    })?;

    let mut buffer = Vec::new();
    source.read_to_end(&mut buffer).with_context(|| {
        format!(
            "Failed to read configuration file at {}",
            config_path.as_ref().display()
        )
    })?;
    toml::from_slice(&buffer).with_context(|| {
        format!(
            "Failed to parse configuration from {}",
            config_path.as_ref().display()
        )
    })
}

#[derive(Debug, Clone)]
struct Arguments {
    config_file: Option<PathBuf>,
    number_of_connections: u16,
    discard_cache: bool,
}

fn process_args(args: Arguments) -> Result<()> {
    let config_file = match args.config_file {
        Some(file) => file,
        None => dirs::config_dir()
            .with_context(|| "Missing HOME directory".to_string())?
            .join("de.swsnr.home")
            .join("home.toml"),
    };
    let config = load_config(config_file)?;

    let walk_time_to_start = Duration::minutes(config.walk_to_start_in_minutes as i64);
    let local_offset = UtcOffset::current_local_offset()
        .with_context(|| "Cannot determine current local timezone offset")?;
    let now = OffsetDateTime::now_utc().to_offset(local_offset);
    let desired_departure_time = now + walk_time_to_start;

    let cache = args
        .discard_cache
        .not()
        .then(|| {
            debug!("Using cache");
            ConnectionsCache::load()
                .map_err(|err| {
                    debug!("Failed to read cached connections: {:#}", err);
                    err
                })
                .ok()
        })
        .flatten()
        // Discard cache if config doesn't match
        .filter(|cache| cache.config == config)
        .map(|cache| {
            debug!("Cached passed config check");
            cache
        })
        // Discard cache if it's empty or if the first connection departs before the desired departure time,
        // that is if the connection cache's outdated.
        .filter(|cache| {
            cache
                .connections
                .first()
                .map_or(false, |r| desired_departure_time < r.departure)
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
                    desired_departure_time,
                )?,
            };
            if let Err(error) = cache.save() {
                warn!("Failed to cache routes: {:#}", error);
            } else {
                debug!("Cached routes")
            }
            cache.connections
        }
    };

    for connection in connections.iter().take(args.number_of_connections as usize) {
        println!(
            "{}",
            display_with_walk_time(connection, walk_time_to_start, local_offset)
        );
    }

    Ok(())
}

fn main() {
    env_logger::init();
    glib::log_set_default_handler(glib::rust_log_handler);

    use clap::*;
    let mut matches = command!()
        .dont_collapse_args_in_usage(true)
        .setting(AppSettings::DeriveDisplayOrder)
        .term_width(80)
        .arg(
            Arg::new("config")
                .long("config")
                .takes_value(true)
                .value_name("FILE")
                .default_value("$XDG_CONFIG_HOME/de.swsnr.home/config.toml")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Config file"),
        )
        .arg(
            Arg::new("number_of_connections")
                .short('n')
                .long("connections")
                .takes_value(true)
                .value_name("N")
                .default_value("10")
                .value_parser(clap::value_parser!(u16))
                .help("The number of connections to show"),
        )
        .arg(
            Arg::new("fresh")
                .long("fresh")
                .help("Get fresh connections")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();
    let args = Arguments {
        config_file: match matches.value_source("config") {
            None | Some(clap::ValueSource::DefaultValue) => None,
            Some(_) => Some(matches.remove_one("config").unwrap()),
        },
        number_of_connections: matches.remove_one("number_of_connections").unwrap(),
        discard_cache: matches.remove_one("fresh").unwrap(),
    };
    if let Err(err) = process_args(args) {
        eprintln!("{:#}", err);
        std::process::exit(1);
    }
}
