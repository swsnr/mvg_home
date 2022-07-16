// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use tracing::{debug, instrument};

use crate::{
    config::{Config, DesiredConnection},
    mvg::Connection,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionsCache {
    pub connections: Vec<(DesiredConnection, Vec<Connection>)>,
}

impl ConnectionsCache {
    fn cache_path() -> PathBuf {
        dirs::cache_dir()
            .expect("cache directory missing")
            .join("de.swsnr.home")
            .join("connections")
    }

    pub fn load() -> Result<Self> {
        let path = Self::cache_path();
        let contents = std::fs::read(&path)
            .with_context(|| format!("Failed to read cache file at {}", path.display()))?;
        flexbuffers::from_slice(&contents)
            .with_context(|| format!("Failed to deserialize cache from {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let cache_file = Self::cache_path();
        let cache_dir = cache_file
            .parent()
            .expect("Cache path should not be a file system root!");
        std::fs::create_dir_all(cache_dir).with_context(|| {
            format!(
                "Failed to create cache directory at {}",
                cache_dir.display()
            )
        })?;
        let contents = flexbuffers::to_vec(self)
            .with_context(|| "Failed to serialize connection cache".to_string())?;
        std::fs::write(&cache_file, contents)
            .with_context(|| format!("Failed to write cache to {}", cache_file.display()))
    }

    /// Update the cache with the config `config`.
    ///
    /// If the desired connections in `config` do not match the cached ones,
    /// discard the entire cache and use the desired connections from `config`.
    ///
    /// Otherwise return this cache as is.
    #[instrument(skip_all)]
    pub fn update_config(self, config: Config) -> Self {
        if config
            .connections
            .iter()
            .eq(self.connections.iter().map(|c| &c.0))
        {
            self
        } else {
            debug!("Discarding cached connections, configuration changed");
            Self {
                connections: config
                    .connections
                    .into_iter()
                    .map(|c| (c, Vec::new()))
                    .collect(),
            }
        }
    }

    /// Remove all connections which begin before the given current time.
    ///
    /// If this removes all connections for a desired connection leave the
    /// desired connection in place with an empty list of connections, which
    /// lets the caller fetch new connections for the desired connection.
    #[instrument(skip(self), fields(now=%now))]
    pub fn evict_outdated_connections(self, now: OffsetDateTime) -> Self {
        let connections = self
            .connections
            .into_iter()
            .map(|(desired, connections)| {
                let connections = if connections.is_empty() {
                    connections
                } else {
                    debug!(
                        "Evicting outdated connections for desired connection from {} to {}",
                        desired.start, desired.destination
                    );
                    let min_departure = now + desired.walk_to_start();
                    connections
                        .into_iter()
                        .filter(|c| min_departure <= c.departure)
                        .collect()
                };
                (desired, connections)
            })
            .collect();
        Self { connections }
    }

    /// Refresh desired connections with the given `update` function.
    ///
    /// Call `update` for every desired connection with an empty list of connections.
    #[instrument(skip_all)]
    pub fn refresh_empty<E, F>(self, update: F) -> std::result::Result<Self, E>
    where
        F: Fn(&DesiredConnection) -> std::result::Result<Vec<Connection>, E>,
    {
        let connections = self
            .connections
            .into_iter()
            .map(|(desired, connections)| {
                if connections.is_empty() {
                    debug!("Desired connection from {} to {} has no connections, refreshing connections", desired.start, desired.destination);
                    update(&desired).map(|cs| (desired, cs))
                } else {
                    Ok((desired, connections))
                }
            })
            .collect::<Result<_, E>>()?;
        Ok(Self { connections })
    }

    /// Return all connections for all desired routes, ordered ascending by start time, with the walk distance to start.
    pub fn all_connections(&self) -> Vec<(Duration, &Connection)> {
        let mut connections = self
            .connections
            .iter()
            .flat_map(|(desired, connections)| {
                connections
                    .iter()
                    .map(|connection| (desired.walk_to_start(), connection))
            })
            .collect::<Vec<_>>();
        connections.sort_by_key(|(walk_to_start, c)| c.departure - *walk_to_start);
        connections
    }
}