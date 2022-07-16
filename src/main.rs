// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! MVG connections for the way home.

use std::fmt::{Display, Formatter};
use std::path::PathBuf;

use anyhow::{Context, Result};
use log::{debug, warn};
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};

mod cache;
mod config;
mod connection;
mod mvg;

use cache::*;
use config::*;
use connection::*;
use mvg::*;

struct ConnectionDisplay<'a> {
    connection: &'a CompleteConnection,
    local_offset: UtcOffset,
}

impl<'a> ConnectionDisplay<'a> {
    fn new(connection: &'a CompleteConnection, local_offset: UtcOffset) -> Self {
        Self {
            connection,
            local_offset,
        }
    }
}

impl<'a> Display for ConnectionDisplay<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let departure = self
            .connection
            .connection
            .departure
            .to_offset(self.local_offset);
        let arrival = self
            .connection
            .connection
            .arrival
            .to_offset(self.local_offset);
        let start = departure - self.connection.walk_to_start;
        let start_in = start - OffsetDateTime::now_utc().to_offset(self.local_offset);

        let hh_mm = format_description!("[hour]:[minute]");
        write!(
            f,
            "🚆 In {} min, dep. {} arr. {}",
            ((start_in.whole_seconds() as f64) / 60.0).ceil(),
            departure.time().format(hh_mm).unwrap(),
            arrival.time().format(hh_mm).unwrap()
        )?;
        if 2 <= self.connection.connection.connection_parts.len() {
            let first_part = &self.connection.connection.connection_parts[0];
            if let Location::Station(station) = &first_part.to {
                write!(f, ", via {} with {}", station.name, first_part.label)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

fn display_with_walk_time(
    connection: &'_ CompleteConnection,
    offset: UtcOffset,
) -> impl Display + '_ {
    ConnectionDisplay::new(connection, offset)
}

#[derive(Debug, Clone)]
struct Arguments {
    config_file: Option<PathBuf>,
    number_of_connections: u16,
    discard_cache: bool,
}

fn process_args(args: Arguments) -> Result<()> {
    let config = match args.config_file {
        Some(file) => Config::from_file(file)?,
        None => Config::from_default_location()?,
    };

    let walk_time_to_start = Duration::minutes(config.walk_to_start_in_minutes as i64);
    let local_offset = UtcOffset::current_local_offset()
        .with_context(|| "Cannot determine current local timezone offset")?;
    let now = OffsetDateTime::now_utc();
    let desired_departure_time = now + walk_time_to_start;

    let connections: Option<Vec<CompleteConnection>> = if args.discard_cache {
        debug!("Cache discarded per command line arguments");
        None
    } else {
        debug!("Using cache");
        ConnectionsCache::load()
            .map_err(|err| {
                debug!("Failed to read cached connections: {:#}", err);
                err
            })
            .ok()
            .and_then(|c| c.into_connections(&config, now))
    };

    let connections = match connections {
        Some(c) => c,
        None => {
            debug!("Cache invalidated, fetching routes");
            let mvg = Mvg::new()?;
            let start = mvg
                .find_unambiguous_station_by_name(&config.start)
                .with_context(|| format!("Failed to find station {}", &config.start))?;
            let destination = mvg
                .find_unambiguous_station_by_name(&config.destination)
                .with_context(|| format!("Failed to find station {}", &config.destination))?;
            let connections = mvg
                .get_connections(&start.id, &destination.id, desired_departure_time)?
                .into_iter()
                .map(|c| c.with_walk_to_start(walk_time_to_start))
                .collect();
            let cache = ConnectionsCache {
                config,
                connections,
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
        println!("{}", display_with_walk_time(connection, local_offset));
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
