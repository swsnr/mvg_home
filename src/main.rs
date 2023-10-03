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
use clap::Parser;
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
        let departure = self
            .connection
            .departure_time()
            .to_offset(self.local_offset);
        let arrival = self.connection.arrival_time().to_offset(self.local_offset);
        let start_in = departure - self.walk_to_start - OffsetDateTime::now_utc();

        let hh_mm = format_description!("[hour]:[minute]");

        let first_part = self.connection.departure();

        write!(
            f,
            "🏡 In {: >2} min, ⚐{} ⚑{}, 🚏{}",
            ((start_in.whole_seconds() as f64) / 60.0).ceil(),
            departure.time().format(hh_mm).unwrap(),
            arrival.time().format(hh_mm).unwrap(),
            self.connection.departure().from.name,
        )?;
        if self.connection.parts.len() == 1 {
            match first_part.line.transport_type {
                // There's only one part in the connection so if it's a footway
                //  we'll just walk to the destination
                TransportType::Pedestrian => write!(f, " 🏃"),
                _ => {
                    write!(
                        f,
                        " {}{}",
                        first_part.line.transport_type.icon(),
                        first_part.line.label
                    )
                }
            }
        } else if 2 <= self.connection.parts.len() {
            match first_part.line.transport_type {
                TransportType::Pedestrian => write!(f, " → 🏃{}", first_part.to.name),
                _ => {
                    write!(
                        f,
                        " → {} {}{}",
                        first_part.to.name,
                        first_part.line.transport_type.icon(),
                        first_part.line.label
                    )
                }
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

#[derive(Debug, Clone, Parser)]
#[command(author, version, about)]
struct Arguments {
    /// Use a different configuration file
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Number of connections to show
    #[arg(short = 'n', long, default_value_t = 10, value_name = "N")]
    connections: u16,
    /// Get fresh connections
    #[arg(long)]
    fresh: bool,
}

impl Arguments {
    fn load_cache(&self) -> ConnectionsCache {
        if self.fresh {
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
    let config = match &args.config {
        Some(file) => Config::from_file(file)?,
        None => Config::from_default_location()?,
    };

    let local_offset = UtcOffset::current_local_offset()
        .with_context(|| "Cannot determine current local timezone offset")?;
    let now = OffsetDateTime::now_utc();

    let rt = tokio::runtime::Builder::new_multi_thread()
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
                let mvg = Mvg::new().await?;
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
                    .get_connections(&start, &destination, desired_departure_time)
                    .await?;
                Ok((desired, connections))
            }),
        )?
        // Evict unreachable connections again, in case the MVG API returned nonsense
        .evict_unreachable_connections(now)
        // And evict anything that starts with walking
        .evict_starts_with_pedestrian();

    debug!("Saving cache");
    if let Err(error) = new_cache.save() {
        warn!("Failed to save cached connections: {:#}", error);
    }

    for (walk_to_start, connection) in new_cache
        .all_connections()
        .iter()
        .take(args.connections as usize)
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

    let args = Arguments::parse();
    if let Err(err) = process_args(args) {
        eprintln!("{:#}", err);
        std::process::exit(1);
    }
}
