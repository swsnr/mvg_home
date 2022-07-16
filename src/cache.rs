// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{config::Config, mvg::Connection};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionsCache {
    pub config: Config,
    pub connections: Vec<Connection>,
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
}
