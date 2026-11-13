//! AIS as Digitraffic publishes it: a vessel's location and its metadata,
//! parsed from the MQTT messages (`vessels-v2/<mmsi>/location`,
//! `vessels-v2/<mmsi>/metadata`) and from the REST snapshot
//! (`/api/ais/v1/locations`, GeoJSON, and `/api/ais/v1/vessels`). Values
//! are kept as published; "not available" codes become `None`. No GPUI
//! types.
//!
//! Digitraffic passes on class A messages only, without fishing vessels,
//! and folds ship types into their decades (60–69 to 60, Passenger). The
//! data is Fintraffic's, licensed CC BY 4.0.

use std::{fmt::Write as _, sync::Arc};

use serde::{Deserialize, Serialize};

/// Where a vessel is and how it moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    /// When the vessel reported it, Unix ms.
    pub time_ms: f64,
    /// Speed over ground, knots.
    pub speed: Option<f32>,
    /// Course over ground, degrees.
    pub course: Option<f32>,
    /// True heading, degrees.
    pub heading: Option<u16>,
    pub status: NavStatus,
    /// Latitude and longitude, degrees.
    pub position: Option<(f64, f64)>,
}

impl Location {
    /// Moving: half a knot or more over ground.
    pub fn is_moving(&self) -> bool {
        self.speed.is_some_and(|speed| speed >= 0.5)
    }

    /// Where the bow points: the heading, else the course while moving.
    pub fn bearing(&self) -> Option<f32> {
        self.heading
            .map(f32::from)
            .or(self.course.filter(|_| self.is_moving()))
    }
}

/// What a vessel says about itself every few minutes.
#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub name: Arc<str>,
    pub call_sign: Arc<str>,
    pub destination: Arc<str>,
    pub eta: Eta,
    /// Draught in tenths of a metre; 0 is not available.
    pub draught: u8,
    pub imo: u32,
    pub ship_type: u8,
    /// From the antenna to the bow, stern, port and starboard, metres.
    pub dimensions: [u16; 4],
    /// When Digitraffic recorded it, Unix ms.
    pub time_ms: f64,
}

impl Metadata {
    pub fn category(&self) -> ShipCategory {
        ShipCategory::from_ship_type(self.ship_type)
    }

    /// Length overall, metres, if reported.
    pub fn length(&self) -> Option<u16> {
        let [bow, stern, ..] = self.dimensions;
        let length = bow.saturating_add(stern);
        (length > 0).then_some(length)
    }

    /// Beam, metres, if reported.
    pub fn beam(&self) -> Option<u16> {
        let [_, _, port, starboard] = self.dimensions;
        let beam = port.saturating_add(starboard);
        (beam > 0).then_some(beam)
    }

    /// Draught in metres, if reported.
    pub fn draught_m(&self) -> Option<f32> {
        (self.draught > 0).then(|| f32::from(self.draught) / 10.)
    }

    /// The same as `other` but for when it was sent: a repeat.
    pub fn says_the_same(&self, other: &Self) -> bool {
        Self {
            time_ms: other.time_ms,
            ..self.clone()
        } == *other
    }
}

/// One message from a source.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Location {
        mmsi: u32,
        location: Location,
    },
    Metadata {
        mmsi: u32,
        metadata: Arc<Metadata>,
    },
    /// The feed's own heartbeat (`vessels-v2/status`).
    Status,
}

impl Message {
    /// The topic and payload of this message as Digitraffic's MQTT would
    /// carry it, for a recording.
    pub fn to_mqtt(&self) -> (String, String) {
        match self {
            Self::Location { mmsi, location } => (
                format!("vessels-v2/{mmsi}/location"),
                json(&MqttLocation::from(location)),
            ),
            Self::Metadata { mmsi, metadata } => (
                format!("vessels-v2/{mmsi}/metadata"),
                json(&MqttMetadata::from(metadata.as_ref())),
            ),
            Self::Status => ("vessels-v2/status".into(), "{}".into()),
        }
    }
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
}

/// The message a topic and payload carry. Other topics are an error.
pub fn parse_message(topic: &str, payload: &[u8]) -> Result<Message, String> {
    let mut parts = topic.split('/');
    let (Some("vessels-v2"), Some(second)) = (parts.next(), parts.next()) else {
        return Err(format!("not a vessel topic: {topic}"));
    };
    let third = parts.next();
    if second == "status" && third.is_none() {
        return Ok(Message::Status);
    }
    let mmsi: u32 = second
        .parse()
        .map_err(|_| format!("no MMSI in the topic: {topic}"))?;
    match third {
        // The documentation names both.
        Some("location" | "locations") => {
            let raw: MqttLocation =
                serde_json::from_slice(payload).map_err(|error| format!("{topic}: {error}"))?;
            Ok(Message::Location {
                mmsi,
                location: raw.into(),
            })
        }
        Some("metadata") => {
            let raw: MqttMetadata =
                serde_json::from_slice(payload).map_err(|error| format!("{topic}: {error}"))?;
            Ok(Message::Metadata {
                mmsi,
                metadata: Arc::new(raw.into()),
            })
        }
        _ => Err(format!("not a vessel topic: {topic}")),
    }
}

/// A location message as published on MQTT.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MqttLocation {
    /// Unix seconds.
    #[serde(default)]
    time: u64,
    #[serde(default = "no_speed")]
    sog: f32,
    #[serde(default = "no_course")]
    cog: f32,
    #[serde(default = "no_status")]
    nav_stat: u8,
    #[serde(default = "no_heading")]
    heading: u16,
    #[serde(default = "no_longitude")]
    lon: f64,
    #[serde(default = "no_latitude")]
    lat: f64,
}

fn no_speed() -> f32 {
    102.3
}

fn no_course() -> f32 {
    360.
}

fn no_status() -> u8 {
    NavStatus::NOT_DEFINED.0
}

fn no_heading() -> u16 {
    511
}

fn no_longitude() -> f64 {
    181.
}

fn no_latitude() -> f64 {
    91.
}

/// AIS's "not available" codes as `None`.
fn location(
    time_ms: f64,
    sog: f32,
    cog: f32,
    nav_stat: u8,
    heading: u16,
    lat: f64,
    lon: f64,
) -> Location {
    let position =
        ((-90.0..=90.).contains(&lat) && (-180.0..=180.).contains(&lon)).then_some((lat, lon));
    Location {
        time_ms,
        // 102.3 is "not available" and 102.2 "102.2 or more": neither is a
        // speed to sort by (a moored tug reported it).
        speed: (0.0..102.15).contains(&sog).then_some(sog),
        course: (0.0..360.).contains(&cog).then_some(cog),
        heading: (heading < 360).then_some(heading),
        status: NavStatus::new(nav_stat),
        position,
    }
}

impl From<MqttLocation> for Location {
    fn from(raw: MqttLocation) -> Self {
        location(
            raw.time as f64 * 1e3,
            raw.sog,
            raw.cog,
            raw.nav_stat,
            raw.heading,
            raw.lat,
            raw.lon,
        )
    }
}

impl From<&Location> for MqttLocation {
    fn from(location: &Location) -> Self {
        let (lat, lon) = location.position.unwrap_or((91., 181.));
        Self {
            time: (location.time_ms / 1e3).round().max(0.) as u64,
            sog: location.speed.unwrap_or(102.3),
            cog: location.course.unwrap_or(360.),
            nav_stat: location.status.code(),
            heading: location.heading.unwrap_or(511),
            lon,
            lat,
        }
    }
}

/// A metadata message as published on MQTT.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MqttMetadata {
    /// Unix ms.
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    destination: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    draught: u8,
    #[serde(default)]
    eta: u32,
    #[serde(default)]
    ref_a: u16,
    #[serde(default)]
    ref_b: u16,
    #[serde(default)]
    ref_c: u16,
    #[serde(default)]
    ref_d: u16,
    #[serde(default)]
    call_sign: String,
    #[serde(default)]
    imo: u32,
    #[serde(default, rename = "type")]
    ship_type: u8,
}

impl From<MqttMetadata> for Metadata {
    fn from(raw: MqttMetadata) -> Self {
        Self {
            name: clean(&raw.name),
            call_sign: clean(&raw.call_sign),
            destination: clean(&raw.destination),
            eta: Eta(raw.eta),
            draught: raw.draught,
            imo: raw.imo,
            ship_type: raw.ship_type,
            dimensions: [raw.ref_a, raw.ref_b, raw.ref_c, raw.ref_d],
            time_ms: raw.timestamp as f64,
        }
    }
}

impl From<&Metadata> for MqttMetadata {
    fn from(metadata: &Metadata) -> Self {
        let [ref_a, ref_b, ref_c, ref_d] = metadata.dimensions;
        Self {
            timestamp: metadata.time_ms.max(0.) as u64,
            destination: metadata.destination.to_string(),
            name: metadata.name.to_string(),
            draught: metadata.draught,
            eta: metadata.eta.0,
            ref_a,
            ref_b,
            ref_c,
            ref_d,
            call_sign: metadata.call_sign.to_string(),
            imo: metadata.imo,
            ship_type: metadata.ship_type,
        }
    }
}

/// AIS text: six-bit ASCII padded with `@` or spaces.
fn clean(text: &str) -> Arc<str> {
    text.trim_end_matches(['@', ' ']).trim().into()
}

/// One feature of the REST locations (GeoJSON).
#[derive(Deserialize)]
struct RestFeature {
    mmsi: u32,
    geometry: RestGeometry,
    properties: RestLocation,
}

#[derive(Deserialize)]
struct RestGeometry {
    coordinates: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestLocation {
    #[serde(default = "no_speed")]
    sog: f32,
    #[serde(default = "no_course")]
    cog: f32,
    #[serde(default = "no_status")]
    nav_stat: u8,
    #[serde(default = "no_heading")]
    heading: u16,
    /// When Digitraffic received it, Unix ms.
    #[serde(default)]
    timestamp_external: u64,
}

#[derive(Deserialize)]
struct RestLocations {
    features: Vec<RestFeature>,
}

/// The latest locations (`/api/ais/v1/locations`), each with its MMSI.
pub fn parse_locations(json: &[u8]) -> Result<Vec<(u32, Location)>, String> {
    let collection: RestLocations =
        serde_json::from_slice(json).map_err(|error| format!("locations: {error}"))?;
    Ok(collection
        .features
        .into_iter()
        .map(|feature| {
            let (lon, lat) = match feature.geometry.coordinates.as_slice() {
                [lon, lat, ..] => (*lon, *lat),
                _ => (181., 91.),
            };
            let raw = feature.properties;
            (
                feature.mmsi,
                location(
                    raw.timestamp_external as f64,
                    raw.sog,
                    raw.cog,
                    raw.nav_stat,
                    raw.heading,
                    lat,
                    lon,
                ),
            )
        })
        .collect())
}

/// One vessel of the REST metadata.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestVessel {
    mmsi: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ship_type: u8,
    #[serde(default)]
    reference_point_a: u16,
    #[serde(default)]
    reference_point_b: u16,
    #[serde(default)]
    reference_point_c: u16,
    #[serde(default)]
    reference_point_d: u16,
    #[serde(default)]
    draught: u8,
    #[serde(default)]
    imo: u32,
    #[serde(default)]
    call_sign: String,
    #[serde(default)]
    eta: u32,
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    destination: String,
}

/// The vessels' metadata (`/api/ais/v1/vessels`), each with its MMSI.
pub fn parse_vessels(json: &[u8]) -> Result<Vec<(u32, Metadata)>, String> {
    let vessels: Vec<RestVessel> =
        serde_json::from_slice(json).map_err(|error| format!("vessels: {error}"))?;
    Ok(vessels
        .into_iter()
        .map(|raw| {
            (
                raw.mmsi,
                Metadata {
                    name: clean(&raw.name),
                    call_sign: clean(&raw.call_sign),
                    destination: clean(&raw.destination),
                    eta: Eta(raw.eta),
                    draught: raw.draught,
                    imo: raw.imo,
                    ship_type: raw.ship_type,
                    dimensions: [
                        raw.reference_point_a,
                        raw.reference_point_b,
                        raw.reference_point_c,
                        raw.reference_point_d,
                    ],
                    time_ms: raw.timestamp as f64,
                },
            )
        })
        .collect())
}

/// AIS navigational status, 0–15.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NavStatus(u8);

impl NavStatus {
    pub const UNDER_WAY: Self = Self(0);
    pub const AT_ANCHOR: Self = Self(1);
    pub const MOORED: Self = Self(5);
    pub const NOT_DEFINED: Self = Self(15);

    pub fn new(code: u8) -> Self {
        Self(code.min(15))
    }

    pub fn code(self) -> u8 {
        self.0
    }

    /// As the status column shows it.
    pub fn label(self) -> &'static str {
        match self.0 {
            0 => "Under way",
            1 => "At anchor",
            2 => "Not under command",
            3 => "Restricted",
            4 => "Constrained by draught",
            5 => "Moored",
            6 => "Aground",
            7 => "Fishing",
            8 => "Sailing",
            14 => "Rescue transmitter",
            15 => "Not defined",
            _ => "Reserved",
        }
    }

    /// Moored or at anchor.
    pub fn is_berthed(self) -> bool {
        self == Self::MOORED || self == Self::AT_ANCHOR
    }

    /// Under way using its engine.
    pub fn is_under_way(self) -> bool {
        self == Self::UNDER_WAY
    }

    /// A status that asks other vessels to keep clear.
    pub fn is_alert(self) -> bool {
        matches!(self.0, 2 | 3 | 4 | 6 | 14)
    }
}

/// What kind of vessel, from its AIS ship type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ShipCategory {
    Cargo,
    Tanker,
    Passenger,
    HighSpeed,
    Tug,
    /// Pilot, search and rescue, port tender, anti-pollution, law
    /// enforcement, medical.
    Service,
    /// Sailing and pleasure craft.
    Leisure,
    /// Dredging, diving, military, wing in ground.
    Special,
    Other,
    /// No metadata yet, or type 0.
    #[default]
    Unknown,
}

impl ShipCategory {
    pub const ALL: [Self; 10] = [
        Self::Cargo,
        Self::Tanker,
        Self::Passenger,
        Self::HighSpeed,
        Self::Tug,
        Self::Service,
        Self::Leisure,
        Self::Special,
        Self::Other,
        Self::Unknown,
    ];

    pub fn from_ship_type(ship_type: u8) -> Self {
        match ship_type {
            0 => Self::Unknown,
            70..=79 => Self::Cargo,
            80..=89 => Self::Tanker,
            60..=69 => Self::Passenger,
            40..=49 => Self::HighSpeed,
            31 | 32 | 52 => Self::Tug,
            50 | 51 | 53 | 54 | 55 | 58 => Self::Service,
            36 | 37 => Self::Leisure,
            20..=29 | 33..=35 => Self::Special,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cargo => "Cargo",
            Self::Tanker => "Tanker",
            Self::Passenger => "Passenger",
            Self::HighSpeed => "High speed",
            Self::Tug => "Tug",
            Self::Service => "Service",
            Self::Leisure => "Leisure",
            Self::Special => "Special",
            Self::Other => "Other",
            Self::Unknown => "Unknown",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }
}

/// An estimated time of arrival as AIS packs it: month, day, hour and
/// minute in UTC, without a year (`MMDDHHMM` in bits 19–0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Eta(pub u32);

impl Eta {
    pub fn month(self) -> u32 {
        (self.0 >> 16) & 0x0f
    }

    pub fn day(self) -> u32 {
        (self.0 >> 11) & 0x1f
    }

    pub fn hour(self) -> u32 {
        (self.0 >> 6) & 0x1f
    }

    pub fn minute(self) -> u32 {
        self.0 & 0x3f
    }

    /// A month and a day: AIS's "not available" is month or day 0.
    pub fn is_known(self) -> bool {
        (1..=12).contains(&self.month()) && (1..=31).contains(&self.day())
    }

    /// `11-08 14:30`, or `11-08` without a time; nothing if unknown.
    pub fn write(self, out: &mut String) {
        if !self.is_known() {
            return;
        }
        let _ = write!(out, "{:02}-{:02}", self.month(), self.day());
        if self.hour() < 24 && self.minute() < 60 {
            let _ = write!(out, " {:02}:{:02}", self.hour(), self.minute());
        }
    }

    /// Packs an ETA, for tests.
    #[cfg(test)]
    pub fn pack(month: u32, day: u32, hour: u32, minute: u32) -> Self {
        Self((month << 16) | (day << 11) | (hour << 6) | minute)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Two vessels as Digitraffic's MQTT would send them; synthetic, not a
    /// recording.
    pub const LOCATION: &str = r#"{"time":1794052600,"sog":12.3,"cog":45.6,"navStat":0,"rot":0,"posAcc":true,"raim":false,"heading":44,"lon":24.95,"lat":60.15}"#;
    pub const METADATA: &str = r#"{"timestamp":1794052601000,"destination":"FI TKU@@@","name":"TEST VESSEL  ","draught":68,"eta":733376,"posType":15,"refA":160,"refB":33,"refC":20,"refD":12,"callSign":"OJAB","imo":9000001,"type":70}"#;

    #[test]
    fn test_location_message_parses_with_not_available_codes() {
        let message = parse_message("vessels-v2/230000001/location", LOCATION.as_bytes())
            .expect("a location");
        let Message::Location { mmsi, location } = message else {
            panic!("a location");
        };
        assert_eq!(mmsi, 230000001);
        assert_eq!(location.speed, Some(12.3));
        assert_eq!(location.course, Some(45.6));
        assert_eq!(location.heading, Some(44));
        assert_eq!(location.position, Some((60.15, 24.95)));
        assert_eq!(location.time_ms, 1_794_052_600_000.);
        assert_eq!(location.status, NavStatus::UNDER_WAY);
        assert!(location.is_moving());

        let Ok(Message::Location { location, .. }) = parse_message(
            "vessels-v2/1/location",
            br#"{"time":1,"sog":102.2,"navStat":5,"lon":25.0,"lat":60.0}"#,
        ) else {
            panic!("a location");
        };
        assert_eq!(location.speed, None);

        let unknown = r#"{"time":1,"sog":102.3,"cog":360.0,"navStat":15,"heading":511,"lon":181.0,"lat":91.0}"#;
        let Ok(Message::Location { location, .. }) =
            parse_message("vessels-v2/1/locations", unknown.as_bytes())
        else {
            panic!("a location");
        };
        assert_eq!(location.speed, None);
        assert_eq!(location.course, None);
        assert_eq!(location.heading, None);
        assert_eq!(location.position, None);
        assert_eq!(location.bearing(), None);
        assert!(!location.is_moving());
    }

    #[test]
    fn test_metadata_message_parses_and_cleans_text() {
        let Ok(Message::Metadata { mmsi, metadata }) =
            parse_message("vessels-v2/230000001/metadata", METADATA.as_bytes())
        else {
            panic!("metadata");
        };
        assert_eq!(mmsi, 230000001);
        assert_eq!(metadata.name.as_ref(), "TEST VESSEL");
        assert_eq!(metadata.destination.as_ref(), "FI TKU");
        assert_eq!(metadata.call_sign.as_ref(), "OJAB");
        assert_eq!(metadata.category(), ShipCategory::Cargo);
        assert_eq!(metadata.length(), Some(193));
        assert_eq!(metadata.beam(), Some(32));
        assert_eq!(metadata.draught_m(), Some(6.8));
    }

    #[test]
    fn test_other_topics_are_errors() {
        assert_eq!(
            parse_message("vessels-v2/status", b"{}"),
            Ok(Message::Status)
        );
        assert!(parse_message("tms/1/2", b"{}").is_err());
        assert!(parse_message("vessels-v2/abc/location", LOCATION.as_bytes()).is_err());
        assert!(parse_message("vessels-v2/1/location", b"not json").is_err());
    }

    #[test]
    fn test_messages_round_trip_through_their_mqtt_form() {
        for (topic, payload) in [
            ("vessels-v2/230000001/location", LOCATION),
            ("vessels-v2/230000001/metadata", METADATA),
        ] {
            let message = parse_message(topic, payload.as_bytes()).expect("parses");
            let (again_topic, again) = message.to_mqtt();
            assert_eq!(again_topic, topic);
            assert_eq!(
                parse_message(&again_topic, again.as_bytes()).expect("parses"),
                message
            );
        }
    }

    #[test]
    fn test_rest_snapshot_parses() {
        let locations = r#"{"type":"FeatureCollection","features":[{"mmsi":1,"type":"Feature","geometry":{"type":"Point","coordinates":[25.75,59.83]},"properties":{"mmsi":1,"sog":0.5,"cog":23.2,"navStat":1,"rot":-12,"posAcc":false,"raim":false,"heading":299,"timestamp":46,"timestampExternal":1794052510181}}]}"#;
        let parsed = parse_locations(locations.as_bytes()).expect("locations");
        assert_eq!(parsed.len(), 1);
        let (mmsi, location) = parsed[0];
        assert_eq!(mmsi, 1);
        assert_eq!(location.position, Some((59.83, 25.75)));
        assert_eq!(location.time_ms, 1_794_052_510_181.);
        assert_eq!(location.status, NavStatus::AT_ANCHOR);
        assert!(location.status.is_berthed());

        let vessels = r#"[{"mmsi":1,"name":"ALPHA","shipType":80,"referencePointA":100,"referencePointB":20,"referencePointC":10,"referencePointD":8,"posType":1,"draught":95,"imo":1,"callSign":"AB","eta":0,"timestamp":1,"destination":"RU LED"}]"#;
        let parsed = parse_vessels(vessels.as_bytes()).expect("vessels");
        assert_eq!(parsed[0].1.category(), ShipCategory::Tanker);
        assert_eq!(parsed[0].1.length(), Some(120));
        assert!(!parsed[0].1.eta.is_known());
        assert!(parse_vessels(b"{}").is_err());
    }

    #[test]
    fn test_eta_unpacks_and_writes() {
        let eta = Eta::pack(11, 8, 14, 30);
        assert_eq!(
            (eta.month(), eta.day(), eta.hour(), eta.minute()),
            (11, 8, 14, 30)
        );
        let mut out = String::new();
        eta.write(&mut out);
        assert_eq!(out, "11-08 14:30");
        let mut out = String::new();
        Eta::pack(1, 2, 24, 60).write(&mut out);
        assert_eq!(out, "01-02");
        let mut out = String::new();
        Eta::pack(0, 0, 24, 60).write(&mut out);
        assert_eq!(out, "");
        // The packed value orders by month, day, hour and minute.
        assert!(Eta::pack(2, 1, 0, 0) > Eta::pack(1, 31, 23, 59));
    }

    #[test]
    fn test_ship_types_fold_into_categories() {
        for (ship_type, category) in [
            (0, ShipCategory::Unknown),
            (70, ShipCategory::Cargo),
            (80, ShipCategory::Tanker),
            (60, ShipCategory::Passenger),
            (40, ShipCategory::HighSpeed),
            (52, ShipCategory::Tug),
            (50, ShipCategory::Service),
            (37, ShipCategory::Leisure),
            (35, ShipCategory::Special),
            (90, ShipCategory::Other),
        ] {
            assert_eq!(
                ShipCategory::from_ship_type(ship_type),
                category,
                "{ship_type}"
            );
        }
        assert_eq!(NavStatus::new(99), NavStatus::NOT_DEFINED);
        assert_eq!(NavStatus::new(9).label(), "Reserved");
    }
}
