// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::fmt::Display;

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::{debug, instrument};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Address {
    latitude: f64,
    longitude: f64,
    place: Option<String>,
    street: Option<String>,
    poi: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Location {
    Station(Station),
    Address(Address),
    // TODO: There are likely other location variants as well
}

impl Location {
    pub fn human_readable(&self) -> HumanReadableLocation {
        HumanReadableLocation(self)
    }
}

pub struct HumanReadableLocation<'a>(&'a Location);

impl<'a> Display for HumanReadableLocation<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Location::Station(station) => write!(f, "{}", station.name),
            Location::Address(address) => match (&address.street, &address.place) {
                (None, None) => write!(f, "{:.4},{:.4}", address.latitude, address.longitude),
                (Some(street), Some(place)) => write!(f, "{}, {}", street, place),
                (None, Some(street)) => write!(f, "{}", street),
                (Some(place), None) => write!(f, "{}", place),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocationsResponse {
    locations: Vec<Location>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TransportationProduct {
    SBahn,
    UBahn,
    Tram,
    Bus,
    #[serde(rename = "REGIONAL_BUS")]
    RegionalBus,
}

impl TransportationProduct {
    pub fn icon(self) -> &'static str {
        match self {
            TransportationProduct::SBahn => "🚆",
            TransportationProduct::UBahn => "🚇",
            TransportationProduct::Tram => "🚊",
            TransportationProduct::Bus => "🚍",
            TransportationProduct::RegionalBus => "🚍",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transportation {
    pub label: String,
    pub product: TransportationProduct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "connectionPartType", rename_all = "UPPERCASE")]
pub enum ConnectionPartTransportation {
    Footway,
    Transportation(Transportation),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPart {
    pub from: Location,
    pub to: Location,
    #[serde(flatten)]
    pub transportation: ConnectionPartTransportation,
}

impl ConnectionPart {
    pub fn is_footway(&self) -> bool {
        match &self.transportation {
            ConnectionPartTransportation::Footway => true,
            ConnectionPartTransportation::Transportation(_) => false,
        }
    }

    pub fn is_transportation_with_product_label<S: AsRef<str>>(&self, label: S) -> bool {
        match &self.transportation {
            ConnectionPartTransportation::Footway => false,
            ConnectionPartTransportation::Transportation(t) => t.label == label.as_ref(),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl Connection {
    pub fn starts_with_footway(&self) -> bool {
        match self.connection_parts.get(0) {
            None => false,
            Some(part) => part.is_footway(),
        }
    }

    pub fn starts_with_transportation_with_product_label<S: AsRef<str>>(&self, label: S) -> bool {
        match self.connection_parts.get(0) {
            None => false,
            Some(part) => part.is_transportation_with_product_label(label),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionsResponse {
    connection_list: Vec<Connection>,
}

pub struct Mvg {
    client: Client,
}

impl Mvg {
    pub async fn new() -> Result<Self> {
        let portal_resolver = system_proxy::unix::FreedesktopPortalProxyResolver::connect()
            .await
            .with_context(|| "Failed to connect to freedesktop proxy portal".to_string())?;
        let env_proxies = system_proxy::env::from_curl_env();
        let proxy = reqwest::Proxy::custom(move |url| {
            let proxy = env_proxies.lookup(url).map(Clone::clone);
            debug!("Environment provided proxy {:?} for {}", proxy, &url);
            proxy.or_else(|| {
                debug!("Environment HTTP proxy empty, checking desktop portal");
                // Create a one-shot channel to bridge from the async proxy resolver to the synchronous
                // proxy interface of reqwest.
                let (tx, rx) = tokio::sync::oneshot::channel();
                let url_inner = url.clone();
                let portal_resolver = portal_resolver.clone();
                tokio::task::spawn(async move {
                    let result = portal_resolver.lookup(&url_inner).await;
                    tx.send(result).unwrap();
                });
                let proxy = tokio::task::block_in_place(|| rx.blocking_recv())
                    .unwrap()
                    .unwrap_or_else(|err| {
                        eprintln!("Proxy lookup on portal failed: {}", err);
                        None
                    });
                debug!("XDG desktop portal provided proxy {:?} for {}", proxy, &url);
                proxy
            })
        });

        Ok(Self {
            client: reqwest::ClientBuilder::new()
                .user_agent("home")
                .proxy(proxy)
                .build()?,
        })
    }

    #[instrument(skip(self), fields(name=name.as_ref()))]
    pub async fn get_location_by_name<S: AsRef<str>>(&self, name: S) -> Result<Vec<Location>> {
        debug!("Finding locations for {}", name.as_ref());
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fahrinfo/location/queryWeb",
            &[("q", name.as_ref())],
        )?;
        let response = self
            .client
            .get(url.clone())
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| {
                format!("Failed to query URL to resolve location {}", name.as_ref())
            })?;
        response
            .json::<LocationsResponse>()
            .await
            .map(|response| {
                let ls = response.locations;
                debug!("Received {} locations for {}", ls.len(), name.as_ref());
                ls
            })
            .with_context(|| format!("Failed to parse response from {}", url))
    }

    #[instrument(skip(self), fields(name=name.as_ref()))]
    pub async fn find_unambiguous_station_by_name<S: AsRef<str>>(
        &self,
        name: S,
    ) -> Result<Station> {
        debug!("Looking for single station with name {}", name.as_ref());
        let mut stations: Vec<Station> = self
            .get_location_by_name(name.as_ref())
            .await?
            .into_iter()
            .filter_map(|loc| match loc {
                Location::Station(station) => Some(station),
                other => {
                    debug!(
                        "Skipping location {} returned for name {}, not a station",
                        other.human_readable(),
                        name.as_ref()
                    );
                    None
                }
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
            let station = stations
                .pop()
                .with_context(|| format!("No matches for {}", name.as_ref()))?;
            debug!(
                "Found station with name {} and id {} for {}",
                station.name,
                station.id,
                name.as_ref()
            );
            Ok(station)
        }
    }

    #[instrument(skip(self), fields(from_station_id=from_station_id.as_ref(), to_station_id=to_station_id.as_ref(), start=%start))]
    pub async fn get_connections<S: AsRef<str>, T: AsRef<str>>(
        &self,
        from_station_id: S,
        to_station_id: T,
        start: OffsetDateTime,
    ) -> Result<Vec<Connection>> {
        debug!(
            "Fetching connections between station ID {} and station ID {} starting at {}",
            from_station_id.as_ref(),
            to_station_id.as_ref(),
            start
        );
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
            .await
            .with_context(|| format!("Failed to query URL to resolve location {}", url.as_ref()))?;
        response
            .json::<ConnectionsResponse>()
            .await
            .map(|response| {
                let connections = response.connection_list;
                debug!("Received {} connections", connections.len());
                connections
            })
            .with_context(|| format!("Failed to decode response from {}", url))
    }
}

#[cfg(test)]
mod tests {
    use crate::mvg::*;
    use pretty_assertions::assert_eq;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn big_well_known_station() {
        let mvg = Mvg::new().await.unwrap();
        let name = "Marienplatz";
        let locations = mvg.get_location_by_name(name).await.unwrap();
        assert!(1 < locations.len(), "Too few locations: {:?}", locations);
        if let Location::Station(station) = &locations[0] {
            assert_eq!(station.name, name);
            assert_eq!(
                &mvg.find_unambiguous_station_by_name(name).await.unwrap(),
                station
            );
        } else {
            panic!("First location not a station: {:?}", &locations[0]);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn small_rural_bus_stop() {
        let mvg = Mvg::new().await.unwrap();
        let name = "Fuchswinkl";
        let locations = mvg.get_location_by_name("Fuchswinkl").await.unwrap();
        assert!(!locations.is_empty());
        if let Location::Station(station) = &locations[0] {
            assert_eq!(station.name, "Fuchswinkl, Abzw.");
            assert_eq!(
                &mvg.find_unambiguous_station_by_name(name).await.unwrap(),
                station
            );
        } else {
            panic!("First location not a station: {:?}", &locations[0]);
        }
    }
}
