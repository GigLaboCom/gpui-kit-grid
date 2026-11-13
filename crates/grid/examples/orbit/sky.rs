//! The sky model: the catalog of orbital elements, SGP4 propagation and
//! where each object is seen from the observer. No GPUI and no grid types.
//!
//! - **Catalog** (`Catalog::parse`): OMM JSON (CelesTrak's `FORMAT=json`)
//!   straight into `sgp4::Elements`, then SGP4 constants, a constellation
//!   from the name and an orbital shell from the mean elements.
//! - **Position** (`Satellite::position`): SGP4 gives position and velocity
//!   in TEME; a rotation by Greenwich mean sidereal time turns them into
//!   Earth-fixed coordinates (polar motion and the equation of the equinoxes
//!   are left out), and the observer's east-north-up frame gives azimuth,
//!   elevation, range and range rate. Altitude is geodetic (WGS 84); speed
//!   is inertial.
//! - **Sunlight**: a low-precision Sun (the Astronomical Almanac's series,
//!   about 0.01°) and a cylindrical Earth shadow.
//! - **Passes** (`find_pass`): a search for the next rise, culmination and
//!   set of one object, for the selected object only.
//!
//! Positions are approximate: SGP4 from public elements is good to a few
//! kilometres near the elements' epoch and drifts away from it.

use std::{collections::HashMap, sync::Arc};

use sgp4::MinutesSinceEpoch;

/// WGS 84: the equatorial radius in km and the flattening.
pub const EARTH_RADIUS: f64 = 6_378.137;
const FLATTENING: f64 = 1. / 298.257_223_563;
/// The square of the first eccentricity.
const E2: f64 = FLATTENING * (2. - FLATTENING);
/// The Earth's rotation, rad/s.
const EARTH_ROTATION: f64 = 7.292_115e-5;
/// The Earth's gravitational parameter, km³/s².
const MU: f64 = 398_600.441_8;
const MS_PER_DAY: f64 = 86_400_000.;
const MS_PER_MINUTE: f64 = 60_000.;
/// The Julian date of the Unix epoch, and of J2000.
const UNIX_EPOCH_JD: f64 = 2_440_587.5;
const J2000_JD: f64 = 2_451_545.;

/// The Julian date of a Unix time in milliseconds.
fn julian_date(unix_ms: f64) -> f64 {
    unix_ms / MS_PER_DAY + UNIX_EPOCH_JD
}

/// Greenwich mean sidereal time in radians (IAU 1982, UT1 taken as UTC).
pub fn gmst(unix_ms: f64) -> f64 {
    let t = (julian_date(unix_ms) - J2000_JD) / 36_525.;
    let seconds = 67_310.548_41 + (876_600. * 3_600. + 8_640_184.812_866) * t + 0.093_104 * t * t
        - 6.2e-6 * t * t * t;
    (seconds / 240.)
        .to_radians()
        .rem_euclid(std::f64::consts::TAU)
}

/// The direction of the Sun, a unit vector in the inertial frame of date:
/// the Astronomical Almanac's low-precision series.
pub fn sun_direction(unix_ms: f64) -> [f64; 3] {
    let n = julian_date(unix_ms) - J2000_JD;
    let mean_longitude = 280.460 + 0.985_647_4 * n;
    let anomaly = (357.528 + 0.985_600_3 * n).to_radians();
    let longitude =
        (mean_longitude + 1.915 * anomaly.sin() + 0.020 * (2. * anomaly).sin()).to_radians();
    let obliquity = (23.439 - 0.000_000_4 * n).to_radians();
    [
        longitude.cos(),
        obliquity.cos() * longitude.sin(),
        obliquity.sin() * longitude.sin(),
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The geodetic latitude and longitude (radians) and height (km) of an
/// Earth-fixed position, by fixed-point iteration (WGS 84).
pub fn geodetic(r: [f64; 3]) -> (f64, f64, f64) {
    let [x, y, z] = r;
    let longitude = y.atan2(x);
    let p = x.hypot(y);
    let mut latitude = z.atan2(p * (1. - E2));
    let mut height = 0.;
    for _ in 0..3 {
        let sin = latitude.sin();
        let n = EARTH_RADIUS / (1. - E2 * sin * sin).sqrt();
        height = p * latitude.cos() + z * sin - EARTH_RADIUS * (1. - E2 * sin * sin).sqrt();
        latitude = z.atan2(p * (1. - E2 * n / (n + height)));
    }
    (latitude, longitude, height)
}

/// Where the sky is seen from: a point on the WGS 84 ellipsoid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observer {
    latitude: f64,
    longitude: f64,
    altitude_m: f64,
    /// Earth-fixed position, km.
    position: [f64; 3],
    sin_lat: f64,
    cos_lat: f64,
    sin_lon: f64,
    cos_lon: f64,
}

impl Observer {
    /// The Royal Observatory, Greenwich: the default observer.
    pub const GREENWICH: (f64, f64, f64) = (51.4769, -0.0005, 46.);

    /// An observer at `latitude` and `longitude` (degrees, north and east
    /// positive) and `altitude_m` metres above the ellipsoid.
    pub fn new(latitude: f64, longitude: f64, altitude_m: f64) -> Self {
        let (lat, lon) = (latitude.to_radians(), longitude.to_radians());
        let (sin_lat, cos_lat, sin_lon, cos_lon) = (lat.sin(), lat.cos(), lon.sin(), lon.cos());
        let n = EARTH_RADIUS / (1. - E2 * sin_lat * sin_lat).sqrt();
        let height = altitude_m / 1_000.;
        let position = [
            (n + height) * cos_lat * cos_lon,
            (n + height) * cos_lat * sin_lon,
            (n * (1. - E2) + height) * sin_lat,
        ];
        Self {
            latitude,
            longitude,
            altitude_m,
            position,
            sin_lat,
            cos_lat,
            sin_lon,
            cos_lon,
        }
    }

    pub fn greenwich() -> Self {
        let (latitude, longitude, altitude) = Self::GREENWICH;
        Self::new(latitude, longitude, altitude)
    }

    /// `lat,lon,alt_m` (degrees, degrees, metres; the altitude may be left
    /// out), as `ORBIT_SITE` and the page's `site` take it.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(',').map(|part| part.trim().parse::<f64>());
        let latitude = parts.next()?.ok()?;
        let longitude = parts.next()?.ok()?;
        let altitude = parts.next().map_or(Some(0.), Result::ok)?;
        let valid = (-90. ..=90.).contains(&latitude)
            && (-180. ..=180.).contains(&longitude)
            && altitude.is_finite()
            && parts.next().is_none();
        valid.then(|| Self::new(latitude, longitude, altitude))
    }

    /// `51.4769° N 0.0005° W · 46 m`.
    pub fn label(&self) -> String {
        let north = if self.latitude >= 0. { 'N' } else { 'S' };
        let east = if self.longitude >= 0. { 'E' } else { 'W' };
        format!(
            "{:.4}° {north} {:.4}° {east} · {:.0} m",
            self.latitude.abs(),
            self.longitude.abs(),
            self.altitude_m
        )
    }

    /// Azimuth, elevation, range and range rate of an Earth-fixed position
    /// and velocity (km, km/s).
    pub fn look(&self, position: [f64; 3], velocity: [f64; 3]) -> Look {
        let d = [
            position[0] - self.position[0],
            position[1] - self.position[1],
            position[2] - self.position[2],
        ];
        let east = -self.sin_lon * d[0] + self.cos_lon * d[1];
        let north = -self.sin_lat * self.cos_lon * d[0] - self.sin_lat * self.sin_lon * d[1]
            + self.cos_lat * d[2];
        let up = self.cos_lat * self.cos_lon * d[0]
            + self.cos_lat * self.sin_lon * d[1]
            + self.sin_lat * d[2];
        let range = dot(d, d).sqrt();
        Look {
            azimuth: east.atan2(north).to_degrees().rem_euclid(360.),
            elevation: (up / range).clamp(-1., 1.).asin().to_degrees(),
            range,
            range_rate: dot(d, velocity) / range,
        }
    }

    /// The elevation of a direction given in Earth-fixed coordinates
    /// (parallax ignored): the Sun's, for twilight.
    pub fn elevation_of(&self, direction: [f64; 3]) -> f64 {
        let up = [
            self.cos_lat * self.cos_lon,
            self.cos_lat * self.sin_lon,
            self.sin_lat,
        ];
        let length = dot(direction, direction).sqrt();
        (dot(up, direction) / length)
            .clamp(-1., 1.)
            .asin()
            .to_degrees()
    }
}

/// An object as the observer sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// Degrees from north through east, 0–360.
    pub azimuth: f64,
    /// Degrees above the horizon (negative below it).
    pub elevation: f64,
    /// Km.
    pub range: f64,
    /// Km/s, negative while approaching.
    pub range_rate: f64,
}

impl Look {
    pub fn is_up(&self) -> bool {
        self.elevation > 0.
    }
}

/// The Earth's orientation and the Sun at one instant, shared by every
/// object propagated to it.
#[derive(Clone, Copy, Debug)]
pub struct Epoch {
    unix_ms: f64,
    cos: f64,
    sin: f64,
    /// Unit vector, inertial.
    sun: [f64; 3],
}

impl Epoch {
    pub fn at(unix_ms: f64) -> Self {
        let theta = gmst(unix_ms);
        Self {
            unix_ms,
            cos: theta.cos(),
            sin: theta.sin(),
            sun: sun_direction(unix_ms),
        }
    }

    pub fn unix_ms(&self) -> f64 {
        self.unix_ms
    }

    /// An inertial (TEME) vector in Earth-fixed coordinates.
    fn to_earth_fixed(&self, v: [f64; 3]) -> [f64; 3] {
        [
            self.cos * v[0] + self.sin * v[1],
            -self.sin * v[0] + self.cos * v[1],
            v[2],
        ]
    }

    /// The Sun's elevation for `observer`, degrees.
    pub fn sun_elevation(&self, observer: &Observer) -> f64 {
        observer.elevation_of(self.to_earth_fixed(self.sun))
    }

    /// Whether an inertial position (km) is in sunlight: outside the
    /// cylinder of the Earth's shadow.
    fn is_sunlit(&self, position: [f64; 3]) -> bool {
        let along = dot(position, self.sun);
        along > 0. || (dot(position, position) - along * along).sqrt() > EARTH_RADIUS
    }
}

/// Where an object is at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub look: Look,
    /// Geodetic, km.
    pub altitude: f64,
    /// Inertial, km/s.
    pub speed: f64,
    pub sunlit: bool,
    /// Geodetic, degrees.
    pub latitude: f64,
    pub longitude: f64,
}

/// The families of objects the catalog names, by name; the rest is
/// `Other`. Order: the tree's and the groups' key order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Constellation {
    Starlink,
    OneWeb,
    Kuiper,
    Qianfan,
    Hulianwang,
    Iridium,
    Globalstar,
    Orbcomm,
    O3b,
    Gps,
    Glonass,
    Galileo,
    Beidou,
    Flock,
    Lemur,
    Yaogan,
    Stations,
    Other,
}

impl Constellation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starlink => "Starlink",
            Self::OneWeb => "OneWeb",
            Self::Kuiper => "Kuiper",
            Self::Qianfan => "Qianfan",
            Self::Hulianwang => "Hulianwang",
            Self::Iridium => "Iridium",
            Self::Globalstar => "Globalstar",
            Self::Orbcomm => "Orbcomm",
            Self::O3b => "O3b",
            Self::Gps => "GPS",
            Self::Glonass => "GLONASS",
            Self::Galileo => "Galileo",
            Self::Beidou => "BeiDou",
            Self::Flock => "Flock",
            Self::Lemur => "Lemur",
            Self::Yaogan => "Yaogan",
            Self::Stations => "Stations",
            Self::Other => "Other",
        }
    }

    /// The family of a catalog name (`STARLINK-1007`, `NAVSTAR 43 (USA
    /// 132)`, `COSMOS 2433 [GLONASS-M]`).
    pub fn of_name(name: &str) -> Self {
        const PREFIXES: [(&str, Constellation); 14] = [
            ("STARLINK", Constellation::Starlink),
            ("ONEWEB", Constellation::OneWeb),
            ("KUIPER", Constellation::Kuiper),
            ("QIANFAN", Constellation::Qianfan),
            ("HULIANWANG", Constellation::Hulianwang),
            ("IRIDIUM", Constellation::Iridium),
            ("GLOBALSTAR", Constellation::Globalstar),
            ("ORBCOMM", Constellation::Orbcomm),
            ("O3B", Constellation::O3b),
            ("NAVSTAR", Constellation::Gps),
            ("BEIDOU", Constellation::Beidou),
            ("FLOCK", Constellation::Flock),
            ("LEMUR", Constellation::Lemur),
            ("YAOGAN", Constellation::Yaogan),
        ];
        if let Some((_, family)) = PREFIXES.iter().find(|(prefix, _)| name.starts_with(prefix)) {
            return *family;
        }
        if name.contains("GLONASS") {
            Self::Glonass
        } else if name.contains("GALILEO") {
            Self::Galileo
        } else if name.starts_with("ISS (") || name.starts_with("CSS (") {
            Self::Stations
        } else {
            Self::Other
        }
    }
}

/// An orbital regime, in the order shells are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Regime {
    Leo,
    Meo,
    Geo,
    Heo,
    Beyond,
}

/// An orbital shell: a regime, a band of mean altitude and an inclination,
/// from the mean elements, so an object keeps its shell from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShellKey {
    regime: Regime,
    /// The lower edge of the altitude band, km (0 where the regime has no
    /// bands).
    band: u16,
    /// Whole degrees, rounded to 1° in LEO and 5° elsewhere.
    inclination: u8,
}

/// LEO's altitude bands, km; MEO's.
const LEO_BAND: f64 = 50.;
const MEO_BAND: f64 = 1_000.;
const LEO_TOP: f64 = 2_000.;
const GEO_ALTITUDE: f64 = 35_786.;
const GEO_SPREAD: f64 = 500.;
/// Above this eccentricity an orbit is highly elliptical.
const HEO_ECCENTRICITY: f64 = 0.25;

impl ShellKey {
    /// The shell of an orbit with this mean altitude (km), eccentricity and
    /// inclination (degrees).
    pub fn of(mean_altitude: f64, eccentricity: f64, inclination: f64) -> Self {
        let to_five = |degrees: f64| ((degrees / 5.).round() * 5.) as u8;
        let (regime, band, inclination) = if eccentricity > HEO_ECCENTRICITY {
            (Regime::Heo, 0, to_five(inclination))
        } else if mean_altitude < LEO_TOP {
            let band = (mean_altitude.max(0.) / LEO_BAND).floor() * LEO_BAND;
            (Regime::Leo, band as u16, inclination.round() as u8)
        } else if (mean_altitude - GEO_ALTITUDE).abs() <= GEO_SPREAD {
            (Regime::Geo, 0, to_five(inclination))
        } else if mean_altitude > GEO_ALTITUDE {
            (Regime::Beyond, 0, to_five(inclination))
        } else {
            let band = (mean_altitude / MEO_BAND).floor() * MEO_BAND;
            (Regime::Meo, band as u16, to_five(inclination))
        };
        Self {
            regime,
            band,
            inclination,
        }
    }

    /// `LEO 450–500 km 53°`, `MEO 20–21k km 55°`, `GEO 0°`: short, the
    /// tree's middle level.
    pub fn label(&self) -> String {
        let inclination = self.inclination;
        match self.regime {
            Regime::Leo => format!(
                "LEO {}–{} km {inclination}°",
                self.band,
                self.band + LEO_BAND as u16
            ),
            Regime::Meo => format!(
                "MEO {}–{}k km {inclination}°",
                self.band / 1_000,
                (self.band + MEO_BAND as u16) / 1_000
            ),
            Regime::Geo => format!("GEO {inclination}°"),
            Regime::Heo => format!("HEO {inclination}°"),
            Regime::Beyond => format!("beyond GEO {inclination}°"),
        }
    }
}

/// `5000` as `5,000`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The mean elements an object's detail shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Elements {
    /// Unix ms.
    pub epoch_ms: f64,
    /// Degrees.
    pub inclination: f64,
    pub eccentricity: f64,
    pub right_ascension: f64,
    pub argument_of_perigee: f64,
    pub mean_anomaly: f64,
    /// Revolutions per day.
    pub mean_motion: f64,
    pub drag_term: f64,
    /// Minutes.
    pub period: f64,
    /// Km above the equatorial radius.
    pub perigee: f64,
    pub apogee: f64,
    pub mean_altitude: f64,
}

/// One object of the catalog.
pub struct Satellite {
    norad: u32,
    name: Arc<str>,
    designator: Arc<str>,
    constellation: Constellation,
    shell: ShellKey,
    shell_label: Arc<str>,
    elements: Elements,
    constants: sgp4::Constants,
}

impl Satellite {
    pub fn norad(&self) -> u32 {
        self.norad
    }

    pub fn name(&self) -> &Arc<str> {
        &self.name
    }

    /// The international designator (`1998-067A`); empty if unknown.
    pub fn designator(&self) -> &Arc<str> {
        &self.designator
    }

    pub fn constellation(&self) -> Constellation {
        self.constellation
    }

    pub fn shell(&self) -> ShellKey {
        self.shell
    }

    pub fn shell_label(&self) -> &Arc<str> {
        &self.shell_label
    }

    pub fn elements(&self) -> &Elements {
        &self.elements
    }

    /// Where the object is at `epoch` for `observer`; `None` where SGP4
    /// fails (an orbit that has decayed in the model).
    pub fn position(&self, epoch: &Epoch, observer: &Observer) -> Option<Position> {
        let prediction = self.propagate(epoch.unix_ms)?;
        let (position, velocity) = (prediction.position, prediction.velocity);
        let fixed = epoch.to_earth_fixed(position);
        let rotated = epoch.to_earth_fixed(velocity);
        // The frame turns under the object: v - ω × r.
        let fixed_velocity = [
            rotated[0] + EARTH_ROTATION * fixed[1],
            rotated[1] - EARTH_ROTATION * fixed[0],
            rotated[2],
        ];
        let (latitude, longitude, altitude) = geodetic(fixed);
        Some(Position {
            look: observer.look(fixed, fixed_velocity),
            altitude,
            speed: dot(velocity, velocity).sqrt(),
            sunlit: epoch.is_sunlit(position),
            latitude: latitude.to_degrees(),
            longitude: longitude.to_degrees(),
        })
    }

    /// The elevation at `unix_ms`, degrees; -90 where SGP4 fails.
    pub fn elevation(&self, unix_ms: f64, observer: &Observer) -> f64 {
        self.look(unix_ms, observer)
            .map_or(-90., |look| look.elevation)
    }

    fn look(&self, unix_ms: f64, observer: &Observer) -> Option<Look> {
        let prediction = self.propagate(unix_ms)?;
        let theta = gmst(unix_ms);
        let (cos, sin) = (theta.cos(), theta.sin());
        let rotate = |v: [f64; 3]| [cos * v[0] + sin * v[1], -sin * v[0] + cos * v[1], v[2]];
        let fixed = rotate(prediction.position);
        let rotated = rotate(prediction.velocity);
        let velocity = [
            rotated[0] + EARTH_ROTATION * fixed[1],
            rotated[1] - EARTH_ROTATION * fixed[0],
            rotated[2],
        ];
        Some(observer.look(fixed, velocity))
    }

    fn propagate(&self, unix_ms: f64) -> Option<sgp4::Prediction> {
        let minutes = (unix_ms - self.elements.epoch_ms) / MS_PER_MINUTE;
        self.constants.propagate(MinutesSinceEpoch(minutes)).ok()
    }
}

/// Every object of a data set, in the file's order.
pub struct Catalog {
    satellites: Vec<Satellite>,
    /// Records SGP4 could not initialise.
    skipped: usize,
    /// The median epoch of the elements, Unix ms.
    median_epoch_ms: f64,
}

impl Catalog {
    /// Parses OMM JSON (an array of CelesTrak's `FORMAT=json` records).
    pub fn parse(json: &[u8]) -> Result<Self, String> {
        let records: Vec<sgp4::Elements> =
            serde_json::from_slice(json).map_err(|error| format!("not OMM JSON: {error}"))?;
        if records.is_empty() {
            return Err("no objects in the data".into());
        }
        let mut shells: HashMap<ShellKey, Arc<str>> = HashMap::new();
        let mut skipped = 0;
        let mut satellites = Vec::with_capacity(records.len());
        for record in &records {
            let Ok(constants) = sgp4::Constants::from_elements(record) else {
                skipped += 1;
                continue;
            };
            let Ok(norad) = u32::try_from(record.norad_id) else {
                skipped += 1;
                continue;
            };
            let name: Arc<str> = record
                .object_name
                .as_deref()
                .unwrap_or("UNKNOWN")
                .trim()
                .into();
            let elements = mean_elements(record);
            let shell = ShellKey::of(
                elements.mean_altitude,
                elements.eccentricity,
                elements.inclination,
            );
            let shell_label = shells
                .entry(shell)
                .or_insert_with(|| shell.label().into())
                .clone();
            satellites.push(Satellite {
                norad,
                constellation: Constellation::of_name(&name),
                name,
                designator: record
                    .international_designator
                    .as_deref()
                    .unwrap_or_default()
                    .into(),
                shell,
                shell_label,
                elements,
                constants,
            });
        }
        let mut epochs: Vec<f64> = satellites
            .iter()
            .map(|satellite| satellite.elements.epoch_ms)
            .collect();
        epochs.sort_by(f64::total_cmp);
        let median_epoch_ms = epochs.get(epochs.len() / 2).copied().unwrap_or_default();
        Ok(Self {
            satellites,
            skipped,
            median_epoch_ms,
        })
    }

    pub fn satellites(&self) -> &[Satellite] {
        &self.satellites
    }

    pub fn len(&self) -> usize {
        self.satellites.len()
    }

    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub fn median_epoch_ms(&self) -> f64 {
        self.median_epoch_ms
    }

    /// The index of the object with NORAD number `norad`.
    pub fn find(&self, norad: u32) -> Option<usize> {
        self.satellites
            .iter()
            .position(|satellite| satellite.norad == norad)
    }
}

/// The detail's mean elements of a record, with its period, perigee and
/// apogee.
fn mean_elements(record: &sgp4::Elements) -> Elements {
    let epoch_ms = record.datetime.and_utc().timestamp_micros() as f64 / 1_000.;
    let motion = record.mean_motion.max(1e-6) * std::f64::consts::TAU / 86_400.;
    let semi_major = (MU / (motion * motion)).cbrt();
    Elements {
        epoch_ms,
        inclination: record.inclination,
        eccentricity: record.eccentricity,
        right_ascension: record.right_ascension,
        argument_of_perigee: record.argument_of_perigee,
        mean_anomaly: record.mean_anomaly,
        mean_motion: record.mean_motion,
        drag_term: record.drag_term,
        period: 1_440. / record.mean_motion.max(1e-6),
        perigee: semi_major * (1. - record.eccentricity) - EARTH_RADIUS,
        apogee: semi_major * (1. + record.eccentricity) - EARTH_RADIUS,
        mean_altitude: semi_major - EARTH_RADIUS,
    }
}

/// A moment of a pass: when, and where the object is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PassPoint {
    pub unix_ms: f64,
    pub azimuth: f64,
    pub elevation: f64,
}

/// One pass over the observer: rise, culmination and set, and the track
/// in between for the sky plot.
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    pub rise: PassPoint,
    pub culmination: PassPoint,
    pub set: PassPoint,
    /// Azimuth and elevation from rise to set.
    pub track: Vec<(f32, f32)>,
}

/// What a pass search found.
#[derive(Clone, Debug, PartialEq)]
pub enum PassSearch {
    Pass(Pass),
    /// Above the horizon for the whole search window (a visible
    /// geostationary object).
    AlwaysUp,
    /// No rise within the search window.
    NoPass,
}

/// How far ahead a pass is looked for.
pub const PASS_WINDOW_MS: f64 = 2. * MS_PER_DAY;
/// Points of a pass's track.
const TRACK_POINTS: usize = 48;
/// Rise and set to this precision, ms.
const CROSSING_PRECISION_MS: f64 = 500.;

/// The pass of `satellite` over `observer` in progress at `from_ms`, or
/// the next one, after skipping `skip` passes: rise and set by bisection
/// of the elevation's sign, culmination by golden-section search. Steps of
/// a three-hundred-sixtieth of the period (5–60 s), so a pass shorter than
/// a step can be missed.
pub fn find_pass(
    satellite: &Satellite,
    observer: &Observer,
    from_ms: f64,
    skip: u32,
) -> PassSearch {
    let step = (satellite.elements.period * MS_PER_MINUTE / 360.).clamp(5_000., 60_000.);
    let end = from_ms + PASS_WINDOW_MS;
    let elevation = |t: f64| satellite.elevation(t, observer);
    let mut t = from_ms;
    let mut el = elevation(t);
    let mut skipped = 0;
    loop {
        // The rise: behind `t` if the object is up, else ahead of it.
        let rise = if el > 0. {
            let mut back = t;
            let mut back_el = el;
            while back_el > 0. {
                if back < from_ms - PASS_WINDOW_MS / 2. {
                    return PassSearch::AlwaysUp;
                }
                back -= step;
                back_el = elevation(back);
            }
            crossing(&elevation, back, back + step)
        } else {
            while el <= 0. {
                if t > end {
                    return PassSearch::NoPass;
                }
                t += step;
                el = elevation(t);
            }
            crossing(&elevation, t - step, t)
        };
        // The set, and the highest sample on the way.
        let mut highest = (t, el);
        while el > 0. {
            if t > end + PASS_WINDOW_MS {
                return PassSearch::AlwaysUp;
            }
            t += step;
            el = elevation(t);
            if el > highest.1 {
                highest = (t, el);
            }
        }
        let set = crossing(&elevation, t - step, t);
        if skipped < skip {
            skipped += 1;
            continue;
        }
        let culmination = culmination(
            &elevation,
            (highest.0 - step).max(rise),
            (highest.0 + step).min(set),
        );
        let point = |unix_ms: f64| {
            let look = satellite.look(unix_ms, observer).unwrap_or_default();
            PassPoint {
                unix_ms,
                azimuth: look.azimuth,
                elevation: look.elevation,
            }
        };
        let track = (0..=TRACK_POINTS)
            .map(|ix| {
                let at = rise + (set - rise) * ix as f64 / TRACK_POINTS as f64;
                let look = satellite.look(at, observer).unwrap_or_default();
                (look.azimuth as f32, look.elevation.max(0.) as f32)
            })
            .collect();
        return PassSearch::Pass(Pass {
            rise: point(rise),
            culmination: point(culmination),
            set: point(set),
            track,
        });
    }
}

/// Where the elevation crosses zero between `a` and `b`, which are on
/// either side of it.
fn crossing(elevation: &impl Fn(f64) -> f64, mut a: f64, mut b: f64) -> f64 {
    let a_up = elevation(a) > 0.;
    while b - a > CROSSING_PRECISION_MS {
        let mid = (a + b) / 2.;
        if (elevation(mid) > 0.) == a_up {
            a = mid;
        } else {
            b = mid;
        }
    }
    (a + b) / 2.
}

/// The time of the highest elevation between `a` and `b`, by golden-section
/// search.
fn culmination(elevation: &impl Fn(f64) -> f64, mut a: f64, mut b: f64) -> f64 {
    const RATIO: f64 = 0.618_033_988_749_895;
    let mut c = b - RATIO * (b - a);
    let mut d = a + RATIO * (b - a);
    let (mut fc, mut fd) = (elevation(c), elevation(d));
    while b - a > CROSSING_PRECISION_MS {
        if fc > fd {
            b = d;
            d = c;
            fd = fc;
            c = b - RATIO * (b - a);
            fc = elevation(c);
        } else {
            a = c;
            c = d;
            fc = fd;
            d = a + RATIO * (b - a);
            fd = elevation(d);
        }
    }
    (a + b) / 2.
}

/// Unix ms of `2026-11-06T17:30:00` or `2026-11-06T17:30:00Z` (UTC).
pub fn parse_utc(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches('Z');
    let datetime: sgp4::chrono::NaiveDateTime = text.parse().ok()?;
    Some(datetime.and_utc().timestamp_millis() as f64)
}

/// The UTC calendar date and time of Unix ms: year, month, day, hour,
/// minute, second (the proleptic Gregorian calendar, by Howard Hinnant's
/// `civil_from_days`).
fn civil(unix_ms: f64) -> (i64, u32, u32, u32, u32, u32) {
    let seconds = (unix_ms / 1_000.).floor() as i64;
    let (days, of_day) = (
        seconds.div_euclid(86_400),
        seconds.rem_euclid(86_400) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        month,
        day,
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60,
    )
}

/// `2026-11-06 17:30:05` of Unix ms, UTC.
pub fn format_utc(unix_ms: f64) -> String {
    let (year, month, day, hour, minute, second) = civil(unix_ms);
    format!("{year}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// `17:30:05` of Unix ms, UTC.
pub fn format_time(unix_ms: f64) -> String {
    let (_, _, _, hour, minute, second) = civil(unix_ms);
    format!("{hour:02}:{minute:02}:{second:02}")
}

/// `3 h 12 min`, `12 min`, `45 s` of a duration in ms.
pub fn format_span(ms: f64) -> String {
    let seconds = (ms.abs() / 1_000.).round() as u64;
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        3_600..172_800 => format!("{} h {} min", seconds / 3_600, seconds % 3_600 / 60),
        _ => format!("{} d", seconds / 86_400),
    }
}

/// Four records of the CelesTrak file of 2026-11-06 (OMM JSON): the ISS,
/// Tiangong's core module, a Starlink and a GPS satellite. For tests:
/// never the 7 MB file.
#[cfg(test)]
pub const FIXTURE: &str = r#"[
{"OBJECT_NAME":"ISS (ZARYA)","OBJECT_ID":"1998-067A","EPOCH":"2026-11-05T20:28:36.809184","MEAN_MOTION":15.4919453,"ECCENTRICITY":0.00071954,"INCLINATION":51.6327,"RA_OF_ASC_NODE":319.056,"ARG_OF_PERICENTER":352.5747,"MEAN_ANOMALY":7.5134,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":25544,"ELEMENT_SET_NO":999,"REV_AT_EPOCH":59371,"BSTAR":0.0001274916,"MEAN_MOTION_DOT":6.629e-05,"MEAN_MOTION_DDOT":0},
{"OBJECT_NAME":"CSS (TIANHE)","OBJECT_ID":"2021-035A","EPOCH":"2026-11-05T22:20:18.972096","MEAN_MOTION":15.61437978,"ECCENTRICITY":0.00045347,"INCLINATION":41.4683,"RA_OF_ASC_NODE":180.7545,"ARG_OF_PERICENTER":265.1608,"MEAN_ANOMALY":94.8714,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":48274,"ELEMENT_SET_NO":999,"REV_AT_EPOCH":31530,"BSTAR":0.00021609642,"MEAN_MOTION_DOT":0.00018706,"MEAN_MOTION_DDOT":0},
{"OBJECT_NAME":"STARLINK-1008","OBJECT_ID":"2019-074B","EPOCH":"2026-11-05T10:31:25.582656","MEAN_MOTION":15.68968474,"ECCENTRICITY":9.957e-05,"INCLINATION":53.1444,"RA_OF_ASC_NODE":101.4209,"ARG_OF_PERICENTER":209.3914,"MEAN_ANOMALY":150.7048,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":44714,"ELEMENT_SET_NO":999,"REV_AT_EPOCH":38627,"BSTAR":0.0016385286,"MEAN_MOTION_DOT":0.0019657,"MEAN_MOTION_DDOT":0},
{"OBJECT_NAME":"NAVSTAR 43 (USA 132)","OBJECT_ID":"1997-035A","EPOCH":"2026-11-05T07:11:26.239200","MEAN_MOTION":2.0056466,"ECCENTRICITY":0.01067241,"INCLINATION":56.0804,"RA_OF_ASC_NODE":93.0585,"ARG_OF_PERICENTER":59.4988,"MEAN_ANOMALY":301.5411,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":24876,"ELEMENT_SET_NO":999,"REV_AT_EPOCH":21478,"BSTAR":0,"MEAN_MOTION_DOT":2.7e-07,"MEAN_MOTION_DDOT":0}
]"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-06T00:00:00Z.
    fn t0() -> f64 {
        parse_utc("2026-11-06T00:00:00Z").expect("a time")
    }

    fn catalog() -> Catalog {
        Catalog::parse(FIXTURE.as_bytes()).expect("the fixture parses")
    }

    #[test]
    fn test_catalog_parses_families_and_shells() {
        let catalog = catalog();
        assert_eq!(catalog.len(), 4);
        assert_eq!(catalog.skipped(), 0);
        let families: Vec<Constellation> = catalog
            .satellites()
            .iter()
            .map(Satellite::constellation)
            .collect();
        assert_eq!(
            families,
            [
                Constellation::Stations,
                Constellation::Stations,
                Constellation::Starlink,
                Constellation::Gps
            ]
        );
        let iss = &catalog.satellites()[0];
        assert_eq!(iss.norad(), 25544);
        assert_eq!(iss.designator().as_ref(), "1998-067A");
        assert!((iss.elements().period - 92.95).abs() < 0.1);
        assert_eq!(iss.shell_label().as_ref(), "LEO 400–450 km 52°");
        let gps = &catalog.satellites()[3];
        assert_eq!(gps.shell_label().as_ref(), "MEO 20–21k km 55°");
        assert_eq!(catalog.find(24876), Some(3));
    }

    #[test]
    fn test_observer_at_the_sub_satellite_point_sees_the_zenith() {
        let catalog = catalog();
        for satellite in catalog.satellites() {
            let epoch = Epoch::at(t0() + 3_600_000.);
            let anywhere = Observer::greenwich();
            let position = satellite.position(&epoch, &anywhere).expect("a position");
            let below = Observer::new(position.latitude, position.longitude, 0.);
            let look = satellite.position(&epoch, &below).expect("a position").look;
            assert!(
                look.elevation > 89.9,
                "{}: elevation {}",
                satellite.name(),
                look.elevation
            );
            // Straight up, the range is the altitude.
            assert!(
                (look.range - position.altitude).abs() < 0.5,
                "{}: range {} altitude {}",
                satellite.name(),
                look.range,
                position.altitude
            );
        }
    }

    #[test]
    fn test_plausible_orbits() {
        let catalog = catalog();
        let epoch = Epoch::at(t0());
        let observer = Observer::greenwich();
        let iss = catalog.satellites()[0]
            .position(&epoch, &observer)
            .expect("a position");
        assert!((380. ..460.).contains(&iss.altitude), "{}", iss.altitude);
        assert!((7.5..7.8).contains(&iss.speed), "{}", iss.speed);
        let gps = catalog.satellites()[3]
            .position(&epoch, &observer)
            .expect("a position");
        assert!(
            (19_500. ..21_000.).contains(&gps.altitude),
            "{}",
            gps.altitude
        );
        assert!((3.7..4.1).contains(&gps.speed), "{}", gps.speed);
    }

    #[test]
    fn test_pass_search_brackets_a_rise_and_a_set() {
        let catalog = catalog();
        let iss = &catalog.satellites()[0];
        let observer = Observer::greenwich();
        let PassSearch::Pass(pass) = find_pass(iss, &observer, t0(), 0) else {
            panic!("the ISS passes over Greenwich within two days");
        };
        assert!(pass.rise.unix_ms < pass.culmination.unix_ms);
        assert!(pass.culmination.unix_ms < pass.set.unix_ms);
        assert!(pass.set.unix_ms - pass.rise.unix_ms < 15. * MS_PER_MINUTE);
        assert!(pass.rise.elevation.abs() < 0.2, "{}", pass.rise.elevation);
        assert!(pass.set.elevation.abs() < 0.2, "{}", pass.set.elevation);
        assert!(pass.culmination.elevation > 0.);
        let before = iss.elevation(pass.rise.unix_ms - 5_000., &observer);
        let after = iss.elevation(pass.set.unix_ms + 5_000., &observer);
        assert!(before < 0. && after < 0., "{before} {after}");
        // The next one comes after it.
        let PassSearch::Pass(next) = find_pass(iss, &observer, t0(), 1) else {
            panic!("a second pass");
        };
        assert!(next.rise.unix_ms > pass.set.unix_ms);
    }

    #[test]
    fn test_observer_parses() {
        let site = Observer::parse("35.6586, 139.7454, 40").expect("a site");
        assert_eq!(site, Observer::new(35.6586, 139.7454, 40.));
        assert!(Observer::parse("51.5,-0.1").is_some());
        assert!(Observer::parse("91,0,0").is_none());
        assert!(Observer::parse("north").is_none());
        assert_eq!(Observer::greenwich().label(), "51.4769° N 0.0005° W · 46 m");
    }

    #[test]
    fn test_times() {
        let t = parse_utc("2026-11-06T17:30:05Z").expect("a time");
        assert_eq!(format_utc(t), "2026-11-06 17:30:05");
        assert_eq!(
            format_utc(parse_utc("2000-02-29T00:00:00").expect("a leap day")),
            "2000-02-29 00:00:00"
        );
        assert_eq!(format_utc(0.), "1970-01-01 00:00:00");
        assert_eq!(format_time(t), "17:30:05");
        assert_eq!(format_span(45_000.), "45 s");
        assert_eq!(format_span(12. * 60_000.), "12 min");
        assert_eq!(format_span(3.5 * 3_600_000.), "3 h 30 min");
        assert!(parse_utc("yesterday").is_none());
    }
}
