//! The vessels table's rows: one per vessel, keyed by its MMSI, with what
//! its last location and metadata say. No GPUI types; `value` borrows
//! everything it shows, so it never allocates.
//!
//! Numbers are plain floats; the table's columns show knots to a tenth and
//! positions to four places (`NumberFormat`), and a copy gives what they
//! show. The times (ETA, last heard) are `Custom` values with their own
//! text.

use std::{any::TypeId, cmp::Ordering, fmt::Write as _, sync::Arc};

use gpui_kit_grid::{CellValue, ChangeDirection, ColumnIx, GridColumns, GridRow, GridValue};

use super::{
    ais::{Eta, Location, Metadata, NavStatus, ShipCategory},
    speed::SpeedRule,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShipColumn {
    Name,
    Mmsi,
    /// The ship type, folded into categories.
    Category,
    /// The navigational status.
    Status,
    /// Knots over ground.
    Speed,
    Course,
    Heading,
    Destination,
    Eta,
    /// Length overall, metres.
    Length,
    /// Draught, metres.
    Draught,
    CallSign,
    Latitude,
    Longitude,
    /// When the vessel was last heard, UTC.
    Seen,
    /// 1 a vessel: summed, it counts a group's vessels. Hidden.
    Vessels,
    /// 1 while moving. Hidden, summed in the captions.
    Moving,
    /// 1 while moored or at anchor. Hidden, summed in the captions.
    Berthed,
    /// The speed of a moving vessel, else blank: its mean is a group's
    /// average speed under way. Hidden.
    MovingSpeed,
    /// 1 once the vessel has gone quiet. Hidden; the row dims.
    Stale,
}

impl GridColumns for ShipColumn {
    fn all() -> &'static [Self] {
        use ShipColumn::*;
        &[
            Name,
            Mmsi,
            Category,
            Status,
            Speed,
            Course,
            Heading,
            Destination,
            Eta,
            Length,
            Draught,
            CallSign,
            Latitude,
            Longitude,
            Seen,
            Vessels,
            Moving,
            Berthed,
            MovingSpeed,
            Stale,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Mmsi => "mmsi",
            Self::Category => "category",
            Self::Status => "status",
            Self::Speed => "speed",
            Self::Course => "course",
            Self::Heading => "heading",
            Self::Destination => "destination",
            Self::Eta => "eta",
            Self::Length => "length",
            Self::Draught => "draught",
            Self::CallSign => "call_sign",
            Self::Latitude => "latitude",
            Self::Longitude => "longitude",
            Self::Seen => "seen",
            Self::Vessels => "vessels",
            Self::Moving => "moving",
            Self::Berthed => "berthed",
            Self::MovingSpeed => "moving_speed",
            Self::Stale => "stale",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// Values of two custom types order by type, so the order stays total.
fn by_type(this: &dyn GridValue, other: &dyn GridValue) -> Ordering {
    let id = |value: &dyn GridValue| value.as_any().type_id();
    let (a, b): (TypeId, TypeId) = (id(this), id(other));
    a.cmp(&b)
}

/// A time of day in UTC, `14:02:37`, ordered by the instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock(pub f64);

impl Clock {
    /// `14:02:37` for Unix ms.
    pub fn write(ms: f64, out: &mut String) {
        let seconds = (ms / 1e3).floor().rem_euclid(86_400.) as u32;
        let _ = write!(
            out,
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        );
    }
}

impl GridValue for Clock {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Self>() {
            Some(other) => self.0.total_cmp(&other.0),
            None => by_type(self, other),
        }
    }

    fn write_text(&self, out: &mut String) {
        Self::write(self.0, out);
    }
}

impl GridValue for Eta {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Self>() {
            Some(other) => self.cmp(other),
            None => by_type(self, other),
        }
    }

    fn write_text(&self, out: &mut String) {
        self.write(out);
    }
}

/// One vessel as the table shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct VesselRow {
    pub mmsi: u32,
    pub name: Arc<str>,
    pub call_sign: Arc<str>,
    pub destination: Arc<str>,
    pub category: ShipCategory,
    pub status: NavStatus,
    /// Knots.
    pub speed: Option<f64>,
    pub course: Option<f32>,
    pub heading: Option<u16>,
    pub eta: Eta,
    pub length: Option<u16>,
    /// Metres.
    pub draught: Option<f64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub seen: Clock,
    pub moving: bool,
    pub stale: bool,
}

impl VesselRow {
    /// The row of a vessel at its last `location`, with its `metadata` if
    /// it has sent any, last heard at `seen_ms`. A speed `speeds` does not
    /// believe of its type is unknown: blank, sorted with the unknown, out
    /// of the averages, and the vessel not counted as moving.
    pub fn new(
        mmsi: u32,
        location: &Location,
        metadata: Option<&Metadata>,
        seen_ms: f64,
        stale: bool,
        speeds: &dyn SpeedRule,
    ) -> Self {
        let category = metadata.map_or(ShipCategory::Unknown, Metadata::category);
        let location = &Location {
            speed: location
                .speed
                .filter(|knots| speeds.is_plausible(category, *knots)),
            ..*location
        };
        let empty: Arc<str> = Arc::from("");
        let text =
            |pick: fn(&Metadata) -> &Arc<str>| metadata.map_or(empty.clone(), |m| pick(m).clone());
        Self {
            mmsi,
            name: text(|m| &m.name),
            call_sign: text(|m| &m.call_sign),
            destination: text(|m| &m.destination),
            category,
            status: location.status,
            speed: location.speed.map(f64::from),
            course: location.course,
            heading: location.heading,
            eta: metadata.map_or(Eta::default(), |metadata| metadata.eta),
            length: metadata.and_then(Metadata::length),
            draught: metadata.and_then(Metadata::draught_m).map(f64::from),
            latitude: location.position.map(|(lat, _)| lat),
            longitude: location.position.map(|(_, lon)| lon),
            seen: Clock(seen_ms),
            moving: location.is_moving(),
            stale,
        }
    }

    pub fn is_berthed(&self) -> bool {
        self.status.is_berthed()
    }

    /// The name, or the MMSI while the vessel has not said its name.
    pub fn title(&self) -> String {
        if self.name.is_empty() {
            format!("MMSI {}", self.mmsi)
        } else {
            self.name.to_string()
        }
    }
}

fn text(text: &str) -> CellValue<'_> {
    if text.is_empty() {
        CellValue::Empty
    } else {
        CellValue::Text(text)
    }
}

fn float(value: Option<f64>) -> CellValue<'static> {
    value.map_or(CellValue::Empty, CellValue::Float)
}

impl GridRow for VesselRow {
    type Key = u32;
    type Column = ShipColumn;

    fn key(&self) -> &u32 {
        &self.mmsi
    }

    fn value(&self, column: ShipColumn) -> CellValue<'_> {
        match column {
            ShipColumn::Name => text(&self.name),
            ShipColumn::Mmsi => CellValue::Int(self.mmsi.into()),
            ShipColumn::Category => CellValue::Text(self.category.label()),
            ShipColumn::Status => CellValue::Text(self.status.label()),
            ShipColumn::Speed => float(self.speed),
            ShipColumn::Course => self.course.map_or(CellValue::Empty, |course| {
                CellValue::Int(course.round() as i64 % 360)
            }),
            ShipColumn::Heading => self
                .heading
                .map_or(CellValue::Empty, |heading| CellValue::Int(heading.into())),
            ShipColumn::Destination => text(&self.destination),
            ShipColumn::Eta if self.eta.is_known() => CellValue::Custom(&self.eta),
            ShipColumn::Eta => CellValue::Empty,
            ShipColumn::Length => self
                .length
                .map_or(CellValue::Empty, |length| CellValue::Int(length.into())),
            ShipColumn::Draught => float(self.draught),
            ShipColumn::CallSign => text(&self.call_sign),
            ShipColumn::Latitude => float(self.latitude),
            ShipColumn::Longitude => float(self.longitude),
            ShipColumn::Seen => CellValue::Custom(&self.seen),
            ShipColumn::Vessels => CellValue::Int(1),
            ShipColumn::Moving => CellValue::Int(self.moving.into()),
            ShipColumn::Berthed => CellValue::Int(self.is_berthed().into()),
            ShipColumn::MovingSpeed => match self.speed {
                Some(speed) if self.moving => CellValue::Float(speed),
                _ => CellValue::Empty,
            },
            ShipColumn::Stale => CellValue::Int(self.stale.into()),
        }
    }

    /// Faster is up and slower down, for the speed's arrow; nothing else
    /// has a direction.
    fn change_direction(&self, previous: &Self, column: ShipColumn) -> Option<ChangeDirection> {
        if column != ShipColumn::Speed {
            return None;
        }
        let (now, before) = (self.speed?, previous.speed?);
        match now.total_cmp(&before) {
            Ordering::Greater => Some(ChangeDirection::Up),
            Ordering::Less => Some(ChangeDirection::Down),
            Ordering::Equal => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit_grid::ColumnSet;

    use super::super::speed::SpeedLimits;
    use super::*;

    const LIMITS: SpeedLimits = SpeedLimits::DEFAULT;

    fn location(speed: f32) -> Location {
        Location {
            time_ms: 0.,
            speed: Some(speed),
            course: Some(359.6),
            heading: None,
            status: NavStatus::UNDER_WAY,
            position: Some((60.123456, 24.9)),
        }
    }

    fn text_of(value: CellValue<'_>) -> String {
        let mut out = String::new();
        match value {
            CellValue::Custom(value) => value.write_text(&mut out),
            CellValue::Text(text) => out.push_str(text),
            CellValue::Int(value) => out.push_str(&value.to_string()),
            CellValue::Empty => {}
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }

    #[test]
    fn test_values_show_their_own_precision() {
        let row = VesselRow::new(
            230000001,
            &location(12.3),
            None,
            45_757_000.,
            false,
            &LIMITS,
        );
        // Plain floats: the columns show a tenth and four places.
        assert_eq!(
            row.value(ShipColumn::Speed),
            CellValue::Float(f64::from(12.3_f32))
        );
        assert_eq!(row.value(ShipColumn::Latitude), CellValue::Float(60.123456));
        assert_eq!(text_of(row.value(ShipColumn::Seen)), "12:42:37");
        // 359.6° rounds to north, not 360.
        assert_eq!(text_of(row.value(ShipColumn::Course)), "0");
        assert_eq!(row.value(ShipColumn::Name), CellValue::Empty);
        assert_eq!(row.value(ShipColumn::Heading), CellValue::Empty);
        assert_eq!(
            row.value(ShipColumn::MovingSpeed),
            CellValue::Float(f64::from(12.3_f32))
        );
        assert_eq!(row.title(), "MMSI 230000001");
        assert_eq!(row.category, ShipCategory::Unknown);
    }

    #[test]
    fn test_speed_changes_have_a_direction() {
        let slow = VesselRow::new(1, &location(5.), None, 0., false, &LIMITS);
        let fast = VesselRow::new(1, &location(6.), None, 0., false, &LIMITS);
        assert_eq!(
            fast.change_direction(&slow, ShipColumn::Speed),
            Some(ChangeDirection::Up)
        );
        assert_eq!(
            slow.change_direction(&fast, ShipColumn::Speed),
            Some(ChangeDirection::Down)
        );
        assert_eq!(fast.change_direction(&slow, ShipColumn::Course), None);
        let mut changed = ColumnSet::EMPTY;
        fast.changed_columns(&slow, &mut changed);
        assert!(changed.contains(ShipColumn::Speed.ix()));
        assert!(changed.contains(ShipColumn::MovingSpeed.ix()));
        assert!(!changed.contains(ShipColumn::Course.ix()));
        // Going quiet is a change the grid sees: the row dims.
        let stale = VesselRow::new(1, &location(6.), None, 0., true, &LIMITS);
        let mut changed = ColumnSet::EMPTY;
        stale.changed_columns(&fast, &mut changed);
        assert!(changed.contains(ShipColumn::Stale.ix()));
    }

    #[test]
    fn test_implausible_speeds_are_unknown() {
        let cargo = Metadata {
            name: "CARGO".into(),
            call_sign: "".into(),
            destination: "".into(),
            eta: Eta::default(),
            draught: 0,
            imo: 0,
            ship_type: 70,
            dimensions: [0; 4],
            time_ms: 0.,
        };
        let row = VesselRow::new(1, &location(102.), Some(&cargo), 0., false, &LIMITS);
        assert_eq!(row.speed, None);
        assert!(!row.moving);
        assert_eq!(row.value(ShipColumn::Speed), CellValue::Empty);
        assert_eq!(row.value(ShipColumn::MovingSpeed), CellValue::Empty);
        // Sorted with the unknown speeds, below every known one.
        assert!(
            row.value(ShipColumn::Speed)
                .cmp_total(&CellValue::Float(0.))
                .is_lt()
        );
        let row = VesselRow::new(1, &location(18.), Some(&cargo), 0., false, &LIMITS);
        assert_eq!(row.speed, Some(18.));
        assert!(row.moving);
        // A high-speed craft may go faster.
        let mut fast = cargo.clone();
        fast.ship_type = 40;
        let row = VesselRow::new(1, &location(45.), Some(&fast), 0., false, &LIMITS);
        assert_eq!(row.speed, Some(45.));
    }

    #[test]
    fn test_custom_values_order_by_value_and_type() {
        let (a, b) = (Clock(1.5), Clock(10.));
        assert_eq!(a.cmp_value(&b), Ordering::Less);
        let eta = Eta::default();
        assert_ne!(a.cmp_value(&eta), Ordering::Equal);
        assert_eq!(a.cmp_value(&eta), eta.cmp_value(&a).reverse());
    }
}
