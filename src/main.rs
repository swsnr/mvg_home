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
use time::macros::format_description;
use time::{Duration, OffsetDateTime, UtcOffset};
use tracing::{debug, info_span, warn};
use tracing_futures::*;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod cache;
mod config;
mod mvg;

use cache::*;
use config::*;
use mvg::*;

struct ConnectionDisplay<'a> {
    connection: &'a Connection,
    walk_to_start: Duration,
    local_offset: UtcOffset,
}

impl<'a> Display for ConnectionDisplay<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let departure = self.connection.departure.to_offset(self.local_offset);
        let arrival = self.connection.arrival.to_offset(self.local_offset);
        let start_in = departure - self.walk_to_start - OffsetDateTime::now_utc();

        let hh_mm = format_description!("[hour]:[minute]");

        let first_part = &self.connection.connection_parts[0];

        write!(
            f,
            "🚆 In {: >2} min, ⚐{} ⚑{}, 🚏{}",
            ((start_in.whole_seconds() as f64) / 60.0).ceil(),
            departure.time().format(hh_mm).unwrap(),
            arrival.time().format(hh_mm).unwrap(),
            self.connection.from.human_readable(),
        )?;
        if 2 <= self.connection.connection_parts.len() {
            match &first_part.label {
                Some(label) => write!(f, " via {} with {}", first_part.to.human_readable(), label),
                None => write!(f, " via {}", first_part.to.human_readable()),
            }
        } else {
            Ok(())
        }
    }
}

fn display_with_walk_time(
    connection: &'_ Connection,
    walk_to_start: Duration,
    local_offset: UtcOffset,
) -> impl Display + '_ {
    ConnectionDisplay {
        connection,
        walk_to_start,
        local_offset,
    }
}

#[derive(Debug, Clone)]
struct Arguments {
    config_file: Option<PathBuf>,
    number_of_connections: u16,
    discard_cache: bool,
}

impl Arguments {
    fn load_cache(&self) -> ConnectionsCache {
        if self.discard_cache {
            debug!("Cache discarded per command line arguments");
            ConnectionsCache::default()
        } else {
            debug!("Using cache");
            ConnectionsCache::load()
                .map_err(|err| {
                    debug!("Failed to read cached connections: {:#}", err);
                    err
                })
                .unwrap_or_default()
        }
    }
}

fn process_args(args: Arguments) -> Result<()> {
    let config = match &args.config_file {
        Some(file) => Config::from_file(file)?,
        None => Config::from_default_location()?,
    };

    let local_offset = UtcOffset::current_local_offset()
        .with_context(|| "Cannot determine current local timezone offset")?;
    let now = OffsetDateTime::now_utc();

    let mvg = Mvg::new()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .instrument(info_span!("main::rt"));

    let cleared_cache = args
        .load_cache()
        .update_config(config)
        .evict_unreachable_connections(now)
        .evict_too_few_connections(3);

    let new_cache = rt
        .inner()
        .block_on(
            cleared_cache.refresh_empty::<anyhow::Error, _, _>(|desired| async {
                debug!(
                    "Updating results for desired connection from {} to {}",
                    desired.start, desired.destination
                );
                let desired_departure_time = now + desired.walk_to_start;
                let start = mvg.find_unambiguous_station_by_name(&desired.start).await?;
                let destination = mvg
                    .find_unambiguous_station_by_name(&desired.destination)
                    .await?;
                let connections = mvg
                    .get_connections(&start.id, &destination.id, desired_departure_time)
                    .await?;
                Ok((desired, connections))
            }),
        )?
        // Evict unreachable connections again, in case the MVG API returned nonsense
        .evict_unreachable_connections(now);

    debug!("Saving cache");
    if let Err(error) = new_cache.save() {
        warn!("Failed to save cached connections: {:#}", error);
    }

    for (walk_to_start, connection) in new_cache
        .all_connections()
        .iter()
        .take(args.number_of_connections as usize)
    {
        println!(
            "{}",
            display_with_walk_time(connection, *walk_to_start, local_offset)
        );
    }

    Ok(())
}

fn main() {
    tracing_subscriber::registry()
        .with(fmt::layer().pretty())
        .with(
            EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("error"))
                .unwrap(),
        )
        .init();
    // And redirect glib to log and hence to tracing
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
