// Copyright Sebastian Wiesner <sebastian@swsnr.de>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, FixedOffset, Utc};
use reqwest::{Client, Proxy, Url};
use serde::{Deserialize, Serialize};
use tracing::{event, instrument, span, Instrument, Level};

pub trait Place {
    fn name(&self) -> &str;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    global_id: String,
    name: String,
}

impl Place for Station {
    fn name(&self) -> &str {
        &self.name
    }
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
    name: String,
    planned_departure: DateTime<FixedOffset>,
}

impl Place for ConnectionPartPlace {
    fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    label: String,
    transport_type: TransportType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPart {
    from: ConnectionPartPlace,
    to: ConnectionPartPlace,
    line: Line,
}

impl ConnectionPart {
    pub fn from(&self) -> &ConnectionPartPlace {
        &self.from
    }

    pub fn to(&self) -> &ConnectionPartPlace {
        &self.to
    }

    pub fn line_transport_type(&self) -> TransportType {
        self.line.transport_type
    }

    pub fn line_label(&self) -> &str {
        &self.line.label
    }
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

    pub fn planned_departure_time(&self) -> DateTime<FixedOffset> {
        self.departure().from.planned_departure
    }

    pub fn arrival(&self) -> &ConnectionPart {
        self.parts
            .last()
            .expect("Connection without at least one part makes no sense at all!")
    }

    pub fn planned_arrival_time(&self) -> DateTime<FixedOffset> {
        self.arrival().to.planned_departure
    }
}

async fn get_portal_proxy_for_url(url: &Url) -> Result<Option<Url>> {
    system_proxy::unix::FreedesktopPortalProxyResolver::connect()
        .await
        .with_context(|| "Failed to connect to freedesktop proxy portal".to_string())?
        .lookup(url)
        .await
        .with_context(|| format!("Failed to lookup proxy for {}", url))
}

async fn get_proxy_for_url(url: &Url) -> Option<Url> {
    event!(Level::DEBUG, "Looking up proxy for {url} in environment");
    if let Some(proxy) = system_proxy::env::from_curl_env().lookup(url) {
        Some(proxy.clone())
    } else {
        event!(
            Level::DEBUG,
            "Asking freedesktop proxy portal for proxy for {url}"
        );
        if let Some(proxy) = get_portal_proxy_for_url(url)
            .await
            .map_err(|err| event!(Level::WARN, "Portal proxy lookup failed: {err}"))
            .ok()
            .flatten()
        {
            Some(proxy)
        } else {
            event!(Level::DEBUG, "Found no proxy for {url}");
            None
        }
    }
}

pub struct Mvg {
    base_url: Url,
    client: Client,
}

impl Mvg {
    pub async fn new() -> Result<Self> {
        let base_url = Url::parse("https://www.mvg.de/api/fib/v2/")?;

        let builder = reqwest::ClientBuilder::new().user_agent("home");
        // Get the proxy to use for the base API url.  Even though we're technically
        // supposed to resolve the proxy for each URL, it's really unlikely that
        // some PAC thing drills down into the MVG API URLs.
        let builder = match get_proxy_for_url(&base_url).await {
            Some(proxy) => {
                event!(Level::INFO, "Using proxy {proxy} for {base_url}");
                builder.proxy(Proxy::all(proxy)?)
            }
            None => {
                event!(Level::INFO, "Using direct connection for {base_url}");
                builder.no_proxy()
            }
        };

        Ok(Self {
            base_url,
            client: builder.build()?,
        })
    }

    #[instrument(skip(self), fields(name=name.as_ref()))]
    pub async fn get_location_by_name<S: AsRef<str>>(&self, name: S) -> Result<Vec<Location>> {
        event!(Level::INFO, "Finding locations for {}", name.as_ref());
        let mut url = self.base_url.join("location")?;
        url.query_pairs_mut().append_pair("query", name.as_ref());

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
                    Level::INFO,
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
            Level::INFO,
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
                Level::INFO,
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
        start: DateTime<Utc>,
    ) -> Result<Vec<Connection>> {
        event!(
            Level::INFO,
            "Fetching connections between station {} ({}) and station {} ({}) starting at {}",
            origin_station.name,
            origin_station.global_id,
            destination_station.name,
            destination_station.global_id,
            start
        );
        let mut url = self.base_url.join("connection")?;
        url.query_pairs_mut()
            .append_pair("originStationGlobalId", origin_station.global_id.as_str())
            .append_pair(
                "destinationStationGlobalId",
                destination_station.global_id.as_ref(),
            )
            .append_pair(
                "routingDateTime",
                &start.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            )
            .append_pair("routingDateTimeIsArrival", "false")
            .append_pair(
                "transportTypes",
                "SCHIFF,RUFTAXI,BAHN,UBAHN,TRAM,SBAHN,BUS,REGIONAL_BUS",
            );

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
                event!(Level::INFO, "Received {} connections", connections.len());
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
    use chrono::{Duration, Local, Timelike};
    use futures::future::try_join;
    use pretty_assertions::assert_eq;

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
    async fn connections() {
        let mvg = Mvg::new().await.unwrap();
        let (departure, destination) = try_join(
            mvg.find_unambiguous_station_by_name("Waldfriedhof"),
            mvg.find_unambiguous_station_by_name("Schwanthaler Höhe"),
        )
        .await
        .unwrap();
        let tomorrow_morning = (Local::now() + Duration::days(1))
            .with_hour(9)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap();

        let connections = mvg
            .get_connections(
                &departure,
                &destination,
                tomorrow_morning.with_timezone(&Utc),
            )
            .await
            .unwrap();
        assert!(!connections.is_empty());
        let first_connection = &connections[0];
        assert!(tomorrow_morning <= first_connection.planned_arrival_time());
    }
}
