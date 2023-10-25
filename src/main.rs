// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![deny(warnings, missing_docs, clippy::all)]

//! MVG connections for the way home.

use std::fmt::{Display, Formatter};
use std::path::PathBuf;

use anstyle::{AnsiColor, Style};
use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
use clap::Parser;
use tracing::{debug, event, warn, Level};

use tracing_futures::Instrument;
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
}

impl<'a> Display for ConnectionDisplay<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let departure_time = self
            .connection
            .actual_departure_time()
            .with_timezone(&Local);
        let arrival = self.connection.actual_arrival_time().with_timezone(&Local);
        let start_in = departure_time - self.walk_to_start - Local::now();

        let departure_stop = self.connection.departure();
        let departure_color = match self.connection.departure_delay() {
            None => None,
            Some(d) if d.is_zero() => Some(AnsiColor::Green.into()),
            Some(_) => Some(AnsiColor::Red.into()),
        };
        let departure_style = Style::new().fg_color(departure_color);
        let arrival_color = match self.connection.arrival_delay() {
            None => None,
            Some(d) if d.is_zero() => Some(AnsiColor::Green.into()),
            Some(_) => Some(AnsiColor::Red.into()),
        };
        let arrival_style = Style::new().fg_color(arrival_color);

        write!(
            f,
            "🏡 In {: >2} min, ⚐{}{}{} ⚑{}{}{}, 🚏{}",
            ((start_in.num_seconds() as f64) / 60.0).ceil(),
            departure_style.render(),
            departure_time.format("%H:%M"),
            departure_style.render_reset(),
            arrival_style.render(),
            arrival.format("%H:%M"),
            arrival_style.render_reset(),
            self.connection.departure().from().name(),
        )?;
        if self.connection.parts.len() == 1 {
            match departure_stop.line_transport_type() {
                // There's only one part in the connection so if it's a footway
                //  we'll just walk to the destination
                TransportType::Pedestrian => write!(f, " 🏃"),
                _ => {
                    write!(
                        f,
                        " {}{}",
                        departure_stop.line_transport_type().icon(),
                        departure_stop.line_label()
                    )
                }
            }
        } else if 2 <= self.connection.parts.len() {
            match departure_stop.line_transport_type() {
                TransportType::Pedestrian => write!(f, " → 🏃{}", departure_stop.to().name()),
                _ => {
                    write!(
                        f,
                        " → {} {}{}",
                        departure_stop.to().name(),
                        departure_stop.line_transport_type().icon(),
                        departure_stop.line_label()
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
) -> impl Display + '_ {
    ConnectionDisplay {
        connection,
        walk_to_start,
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
    /// Show contents of the cache and exit.
    #[arg(long)]
    dump_cache: bool,
    /// Start at the given time instead of now.
    #[arg(short = 's', long, default_value_t = Local::now())]
    start_time: DateTime<Local>,
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

    let desired_start_time = args.start_time.with_timezone(&Utc);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let cache = args.load_cache().update_config(config);
    event!(
        Level::INFO,
        "Found {} connections in cache for current configuration",
        cache.all_connections().len()
    );

    let new_cache = if args.dump_cache {
        cache
    } else {
        let number_of_cached_connections = cache.all_connections().len();
        let cleared_cache = cache
            .evict_unreachable_connections(desired_start_time)
            .evict_too_few_connections(3);
        event!(
            Level::INFO,
            "{} connections remained in cache after eviction, evicted {} connections",
            cleared_cache.all_connections().len(),
            number_of_cached_connections - cleared_cache.all_connections().len()
        );

        // Create single client upfront; this resolves the HTTP proxy (if any) only once.
        let mvg = rt.block_on(Mvg::new().in_current_span())?;

        rt.block_on(
            cleared_cache
                .refresh_empty::<anyhow::Error, _, _>(|desired| async {
                    let desired_departure_time = desired_start_time + desired.walk_to_start;
                    let start = mvg.find_unambiguous_station_by_name(&desired.start).await?;
                    let destination = mvg
                        .find_unambiguous_station_by_name(&desired.destination)
                        .await?;
                    let connections = mvg
                        .get_connections(&start, &destination, desired_departure_time)
                        .await?;
                    Ok((desired, connections))
                })
                .in_current_span(),
        )?
        // Evict unreachable connections again, in case the MVG API returned nonsense
        .evict_unreachable_connections(desired_start_time)
        // And evict anything that starts with walking
        .evict_starts_with_pedestrian()
    };

    debug!("Saving cache");
    if let Err(error) = new_cache.save() {
        warn!("Failed to save cached connections: {:#}", error);
    }

    for (walk_to_start, connection) in new_cache
        .all_connections()
        .iter()
        .take(args.connections as usize)
    {
        println!("{}", display_with_walk_time(connection, *walk_to_start));
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
