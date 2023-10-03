// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::{event, instrument, span, Instrument, Level};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    pub global_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "UPPERCASE")]
pub enum Location {
    Station(Station),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UnknownLocationType {
    pub r#type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum LocationOrUnknown {
    Location(Location),
    Unknown(UnknownLocationType),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TransportType {
    Schiff,
    Ruftaxi,
    Bahn,
    UBahn,
    Tram,
    SBahn,
    Bus,
    #[serde(rename = "REGIONAL_BUS")]
    RegionalBus,
    Pedestrian,
}

impl TransportType {
    pub fn icon(self) -> &'static str {
        match self {
            TransportType::Bahn => "🚆",
            TransportType::SBahn => "🚆",
            TransportType::UBahn => "🚇",
            TransportType::Tram => "🚊",
            TransportType::Bus => "🚍",
            TransportType::RegionalBus => "🚍",
            TransportType::Schiff => "🛳",
            TransportType::Ruftaxi => "🚖",
            TransportType::Pedestrian => "🚶",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPartPlace {
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub planned_departure: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub label: String,
    pub transport_type: TransportType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPart {
    pub from: ConnectionPartPlace,
    pub to: ConnectionPartPlace,
    pub line: Line,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub parts: Vec<ConnectionPart>,
}

impl Connection {
    pub fn departure(&self) -> &ConnectionPart {
        self.parts
            .get(0)
            .expect("Connection without at least one part makes no sense at all!")
    }

    pub fn departure_time(&self) -> OffsetDateTime {
        self.departure().from.planned_departure
    }

    pub fn arrival(&self) -> &ConnectionPart {
        self.parts
            .last()
            .expect("Connection without at least one part makes no sense at all!")
    }

    pub fn arrival_time(&self) -> OffsetDateTime {
        self.arrival().to.planned_departure
    }
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
            event!(
                Level::DEBUG,
                "Environment provided proxy {:?} for {}",
                proxy,
                &url
            );
            proxy.or_else(|| {
                event!(
                    Level::DEBUG,
                    "Environment HTTP proxy empty, checking desktop portal"
                );
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
                event!(
                    Level::DEBUG,
                    "XDG desktop portal provided proxy {:?} for {}",
                    proxy,
                    &url
                );
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
        event!(Level::DEBUG, "Finding locations for {}", name.as_ref());
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fib/v2/location",
            &[("query", name.as_ref())],
        )?;
        let _guard = span!(Level::INFO, "request::GET", %url).entered();
        event!(Level::TRACE, %url, "Sending request");
        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .in_current_span()
            .await
            .with_context(|| {
                format!("Failed to query URL to resolve location {}", name.as_ref())
            })?;
        response
            .json::<Vec<LocationOrUnknown>>()
            .in_current_span()
            .await
            .map(|response| {
                let locations = response
                    .into_iter()
                    .filter_map(|l| match l {
                        LocationOrUnknown::Location(l) => Some(l),
                        LocationOrUnknown::Unknown(l) => {
                            event!(
                                Level::TRACE,
                                "Skipping over unknown location type {} in response",
                                l.r#type
                            );
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                event!(
                    Level::DEBUG,
                    "Received {} known locations for {}",
                    locations.len(),
                    name.as_ref()
                );
                locations
            })
            .with_context(|| {
                format!(
                    "Failed to parse response for location by name {}",
                    name.as_ref()
                )
            })
    }

    #[instrument(skip(self), fields(name=name.as_ref()))]
    pub async fn find_unambiguous_station_by_name<S: AsRef<str>>(
        &self,
        name: S,
    ) -> Result<Station> {
        event!(
            Level::DEBUG,
            "Looking for single station with name {}",
            name.as_ref()
        );
        let mut stations: Vec<Station> = self
            .get_location_by_name(name.as_ref())
            .in_current_span()
            .await?
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
            let station = stations
                .pop()
                .with_context(|| format!("No matches for {}", name.as_ref()))?;
            event!(
                Level::DEBUG,
                "Found station with name {} and id {} for {}",
                station.name,
                station.global_id,
                name.as_ref()
            );
            Ok(station)
        }
    }

    #[instrument(skip(self), fields(start=%start))]
    pub async fn get_connections(
        &self,
        origin_station: &Station,
        destination_station: &Station,
        start: OffsetDateTime,
    ) -> Result<Vec<Connection>> {
        event!(
            Level::DEBUG,
            "Fetching connections between station {} ({}) and station {} ({}) starting at {}",
            origin_station.name,
            origin_station.global_id,
            destination_station.name,
            destination_station.global_id,
            start
        );
        let url = Url::parse_with_params(
            "https://www.mvg.de/api/fib/v2/connection",
            &[
                ("originStationGlobalId", origin_station.global_id.as_str()),
                (
                    "destinationStationGlobalId",
                    destination_station.global_id.as_ref(),
                ),
                ("routingDateTime", ""),
                ("routingDateTimeIsArrival", "false"),
                (
                    "transportTypes",
                    "SCHIFF,RUFTAXI,BAHN,UBAHN,TRAM,SBAHN,BUS,REGIONAL_BUS",
                ),
            ],
        )?;
        let _guard = span!(Level::INFO, "request::GET", %url).entered();
        event!(Level::TRACE, %url, "Sending request");
        let response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .in_current_span()
            .await
            .with_context(|| {
                format!(
                    "Failed to query URL to get a connection from {} to {}",
                    origin_station.global_id, destination_station.global_id
                )
            })?;
        response
            .json::<Vec<Connection>>()
            .in_current_span()
            .await
            .map(|connections| {
                event!(Level::DEBUG, "Received {} connections", connections.len());
                connections
            })
            .with_context(|| {
                format!(
                    "Failed to parse response for connection from from {} to {}",
                    origin_station.global_id, destination_station.global_id
                )
            })
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
        let Location::Station(station) = &locations[0];
        assert_eq!(station.name, name);
        assert_eq!(
            &mvg.find_unambiguous_station_by_name(name).await.unwrap(),
            station
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn small_rural_bus_stop() {
        let mvg = Mvg::new().await.unwrap();
        let name = "Fuchswinkl";
        let locations = mvg.get_location_by_name("Fuchswinkl").await.unwrap();
        assert!(!locations.is_empty());
        let Location::Station(station) = &locations[0];
        assert_eq!(station.name, "Fuchswinkl, Abzw.");
        assert_eq!(
            &mvg.find_unambiguous_station_by_name(name).await.unwrap(),
            station
        );
    }
}
