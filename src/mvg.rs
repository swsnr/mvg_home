// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Context, Result};
use reqwest::{blocking::Client, Proxy, Url};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Location {
    Station(Station),
    // TODO: There are likely other location variants as well
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocationsResponse {
    locations: Vec<Location>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPart {
    pub from: Location,
    pub to: Location,
    // The label of this connection, e.g. S4
    pub label: String,
    /// The type of transporation, e.g. SBAHN
    pub product: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub from: Location,
    #[serde(with = "unix_millis")]
    pub departure: OffsetDateTime,
    pub to: Location,
    #[serde(with = "unix_millis")]
    pub arrival: OffsetDateTime,
    #[serde(rename = "connectionPartList")]
    pub connection_parts: Vec<ConnectionPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionsResponse {
    connection_list: Vec<Connection>,
}

pub struct Mvg {
    client: Client,
}

// TODO: Make this client asynchronous and fetch all missing routes in parallel!
impl Mvg {
    pub fn new() -> Result<Self> {
        let proxy = system_proxy::default();
        Ok(Self {
            client: reqwest::blocking::ClientBuilder::new()
                .user_agent("home")
                .proxy(Proxy::custom(move |url| proxy.for_url(url)))
                .build()?,
        })
    }

    pub fn get_location_by_name<S: AsRef<str>>(&self, name: S) -> Result<Vec<Location>> {
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

    pub fn find_unambiguous_station_by_name<S: AsRef<str>>(&self, name: S) -> Result<Station> {
        let mut stations: Vec<Station> = self
            .get_location_by_name(name.as_ref())?
            .into_iter()
            .map(|loc| match loc {
                Location::Station(station) => station,
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

    pub fn get_connections<S: AsRef<str>, T: AsRef<str>>(
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
