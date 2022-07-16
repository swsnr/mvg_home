// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{config::Config, connection::CompleteConnection};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionsCache {
    pub config: Config,
    pub connections: Vec<CompleteConnection>,
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

    fn start_evict(self) -> EvictableCache {
        EvictableCache::Cached(self)
    }

    /// Extract valid connections from the cache.
    ///
    /// Evict the cache if it doesn't match `config`, filter all routes starting
    /// before `now` and then check if there are sufficiently many routes
    /// remaining.
    pub fn into_connections(
        self,
        config: &Config,
        now: OffsetDateTime,
    ) -> Option<Vec<CompleteConnection>> {
        self.start_evict()
            .evict_mismatched_config(config)
            .evict_outdated_routes(now)
            .evict_empty()
            .into_connections()
    }
}

#[derive(Debug, Clone)]
pub enum EvictableCache {
    Evicted,
    Cached(ConnectionsCache),
}

impl EvictableCache {
    fn do_evict<F>(self, f: F) -> Self
    where
        F: Fn(ConnectionsCache) -> EvictableCache,
    {
        match self {
            Self::Evicted => Self::Evicted,
            Self::Cached(cache) => f(cache),
        }
    }

    /// Evict the cache if the cached config doesn't match `config`.
    fn evict_mismatched_config(self, config: &Config) -> Self {
        self.do_evict(|cache| {
            if &cache.config == config {
                debug!("Cached config matches current config");
                Self::Cached(cache)
            } else {
                debug!("Evicting cache, config does not match");
                Self::Evicted
            }
        })
    }

    /// Remove all routes which start after `now`.
    fn evict_outdated_routes(self, now: OffsetDateTime) -> Self {
        self.do_evict(|cache| {
            let connections = cache
                .connections
                .into_iter()
                .filter(|c| now <= c.start_to_walk())
                .collect();
            Self::Cached(ConnectionsCache {
                connections,
                config: cache.config,
            })
        })
    }

    /// Evict the cache if it has no connections.
    fn evict_empty(self) -> Self {
        self.do_evict(|cache| {
            if cache.connections.is_empty() {
                Self::Evicted
            } else {
                Self::Cached(cache)
            }
        })
    }

    /// Extract connections from this cache.
    fn into_connections(self) -> Option<Vec<CompleteConnection>> {
        match self {
            Self::Evicted => None,
            Self::Cached(cache) => Some(cache.connections),
        }
    }
}
