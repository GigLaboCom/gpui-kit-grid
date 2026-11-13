//! The rows the producer writes and the sky table shows: one per object,
//! keyed by its NORAD number, and the tree's branches (constellations and
//! their orbital shells) with their roll-ups, all in one store. No GPUI
//! types; `value` borrows everything it shows, so it never allocates.

use std::sync::Arc;

use gpui_kit_grid::{CellValue, ChangeDirection, ColumnIx, ColumnSet, GridColumns, GridRow};

use super::sky::{Constellation, ShellKey};

/// A row's key: an object by its NORAD number, or a branch of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkyKey {
    Constellation(Constellation),
    Shell(Constellation, ShellKey),
    Object(u32),
}

impl SkyKey {
    /// The NORAD number of an object's key.
    pub fn norad(self) -> Option<u32> {
        match self {
            Self::Object(norad) => Some(norad),
            Self::Constellation(_) | Self::Shell(..) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkyColumn {
    Name,
    Norad,
    Constellation,
    /// The orbital shell; hidden, the tree's middle level.
    Shell,
    /// The objects under a branch; 1 for an object, so the totals count
    /// objects. Shown in the tree.
    Objects,
    /// The objects above the horizon under a branch; 1 or 0 for an
    /// object. Shown in the tree, summed in the group captions.
    UpCount,
    Azimuth,
    /// An object's elevation; a branch's highest.
    Elevation,
    /// An object's range; a branch's nearest object above the horizon.
    Range,
    RangeRate,
    Altitude,
    Speed,
    Sunlit,
    /// Up or down: above the horizon or not.
    Pass,
    /// The range of an object above the horizon, else blank: its minimum is
    /// a group caption's nearest object. Hidden.
    UpRange,
}

impl GridColumns for SkyColumn {
    fn all() -> &'static [Self] {
        use SkyColumn::*;
        &[
            Name,
            Norad,
            Constellation,
            Shell,
            Objects,
            UpCount,
            Azimuth,
            Elevation,
            Range,
            RangeRate,
            Altitude,
            Speed,
            Sunlit,
            Pass,
            UpRange,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Norad => "norad",
            Self::Constellation => "constellation",
            Self::Shell => "shell",
            Self::Objects => "objects",
            Self::UpCount => "up_count",
            Self::Azimuth => "azimuth",
            Self::Elevation => "elevation",
            Self::Range => "range",
            Self::RangeRate => "range_rate",
            Self::Altitude => "altitude",
            Self::Speed => "speed",
            Self::Sunlit => "sunlit",
            Self::Pass => "pass",
            Self::UpRange => "up_range",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// An object as the observer sees it at one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sighting {
    pub norad: u32,
    /// Degrees.
    pub azimuth: f64,
    pub elevation: f64,
    /// Km.
    pub range: f64,
    /// Km/s, negative while approaching.
    pub range_rate: f64,
    /// Km.
    pub altitude: f64,
    /// Km/s.
    pub speed: f64,
    pub sunlit: bool,
}

impl Sighting {
    pub fn is_up(&self) -> bool {
        self.elevation > 0.
    }
}

/// A branch's roll-up of the objects under it, written by the producer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RollUp {
    pub objects: u32,
    pub up: u32,
    /// The highest elevation under the branch.
    pub highest: Option<f64>,
    /// The nearest object above the horizon, km.
    pub nearest: Option<f64>,
}

/// What a row holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Body {
    Object(Sighting),
    Branch(RollUp),
}

/// One row of the sky table.
#[derive(Clone, Debug, PartialEq)]
pub struct SkyRow {
    pub key: SkyKey,
    pub parent: Option<SkyKey>,
    pub name: Arc<str>,
    pub constellation: Constellation,
    /// The shell's label; empty for a constellation.
    pub shell: Arc<str>,
    pub body: Body,
}

/// What the Sunlit and Pass columns show. The shapes differ as well as the
/// colours, and sorting the text ascending puts up before down.
pub const SUNLIT: &str = "● sunlit";
pub const SHADOW: &str = "○ shadow";
pub const UP: &str = "▲ up";
pub const DOWN: &str = "▽ down";

impl SkyRow {
    pub fn sighting(&self) -> Option<&Sighting> {
        match &self.body {
            Body::Object(sighting) => Some(sighting),
            Body::Branch(_) => None,
        }
    }

    pub fn roll_up(&self) -> Option<&RollUp> {
        match &self.body {
            Body::Branch(roll_up) => Some(roll_up),
            Body::Object(_) => None,
        }
    }

    pub fn is_object(&self) -> bool {
        matches!(self.body, Body::Object(_))
    }

    /// An object above the horizon.
    pub fn is_up(&self) -> bool {
        self.sighting().is_some_and(Sighting::is_up)
    }
}

/// Km to whole km, as the Range and Altitude columns show them.
fn km(value: f64) -> CellValue<'static> {
    CellValue::Int(value.round() as i64)
}

impl GridRow for SkyRow {
    type Key = SkyKey;
    type Column = SkyColumn;

    fn key(&self) -> &SkyKey {
        &self.key
    }

    fn value(&self, column: SkyColumn) -> CellValue<'_> {
        match (&self.body, column) {
            (_, SkyColumn::Name) => CellValue::Text(&self.name),
            (_, SkyColumn::Constellation) => CellValue::Text(self.constellation.label()),
            (_, SkyColumn::Shell) if self.shell.is_empty() => CellValue::Empty,
            (_, SkyColumn::Shell) => CellValue::Text(&self.shell),
            (Body::Object(sighting), column) => match column {
                SkyColumn::Norad => CellValue::Int(sighting.norad.into()),
                SkyColumn::Objects => CellValue::Int(1),
                SkyColumn::UpCount => CellValue::Int(sighting.is_up().into()),
                SkyColumn::Azimuth => CellValue::Float(sighting.azimuth),
                SkyColumn::Elevation => CellValue::Float(sighting.elevation),
                SkyColumn::Range => km(sighting.range),
                SkyColumn::RangeRate => CellValue::Float(sighting.range_rate),
                SkyColumn::Altitude => km(sighting.altitude),
                SkyColumn::Speed => CellValue::Float(sighting.speed),
                SkyColumn::Sunlit if sighting.sunlit => CellValue::Text(SUNLIT),
                SkyColumn::Sunlit => CellValue::Text(SHADOW),
                SkyColumn::Pass if sighting.is_up() => CellValue::Text(UP),
                SkyColumn::Pass => CellValue::Text(DOWN),
                SkyColumn::UpRange if sighting.is_up() => km(sighting.range),
                SkyColumn::UpRange => CellValue::Empty,
                SkyColumn::Name | SkyColumn::Constellation | SkyColumn::Shell => {
                    unreachable!("matched above")
                }
            },
            (Body::Branch(roll_up), column) => match column {
                SkyColumn::Objects => CellValue::Int(roll_up.objects.into()),
                SkyColumn::UpCount => CellValue::Int(roll_up.up.into()),
                SkyColumn::Elevation => roll_up.highest.map_or(CellValue::Empty, CellValue::Float),
                SkyColumn::Range => roll_up.nearest.map_or(CellValue::Empty, km),
                _ => CellValue::Empty,
            },
        }
    }

    /// The fields an object changes every tick, compared directly; a
    /// branch column by column.
    fn changed_columns(&self, previous: &Self, out: &mut ColumnSet) {
        let (Body::Object(now), Body::Object(before)) = (&self.body, &previous.body) else {
            for &column in SkyColumn::all() {
                if self.value(column) != previous.value(column) {
                    out.insert(column.ix());
                }
            }
            return;
        };
        let mut changed = |column: SkyColumn, differs: bool| {
            if differs {
                out.insert(column.ix());
            }
        };
        let whole = |value: f64| value.round() as i64;
        changed(SkyColumn::Azimuth, now.azimuth != before.azimuth);
        changed(SkyColumn::Elevation, now.elevation != before.elevation);
        changed(SkyColumn::Range, whole(now.range) != whole(before.range));
        changed(SkyColumn::RangeRate, now.range_rate != before.range_rate);
        changed(
            SkyColumn::Altitude,
            whole(now.altitude) != whole(before.altitude),
        );
        changed(SkyColumn::Speed, now.speed != before.speed);
        changed(SkyColumn::Sunlit, now.sunlit != before.sunlit);
        let rose_or_set = now.is_up() != before.is_up();
        changed(SkyColumn::Pass, rose_or_set);
        changed(SkyColumn::UpCount, rose_or_set);
        changed(
            SkyColumn::UpRange,
            rose_or_set || (now.is_up() && whole(now.range) != whole(before.range)),
        );
        changed(SkyColumn::Name, !Arc::ptr_eq(&self.name, &previous.name));
        changed(SkyColumn::Norad, now.norad != before.norad);
    }

    /// A rise is up and a set down, for every changed column, so the whole
    /// row flashes one way (`RiseSetChanges`); nothing else has a
    /// direction.
    fn change_direction(&self, previous: &Self, _: SkyColumn) -> Option<ChangeDirection> {
        match (self.is_up(), previous.is_up()) {
            (true, false) => Some(ChangeDirection::Up),
            (false, true) if self.is_object() => Some(ChangeDirection::Down),
            _ => None,
        }
    }

    fn parent_key(&self) -> Option<&SkyKey> {
        self.parent.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(elevation: f64) -> SkyRow {
        SkyRow {
            key: SkyKey::Object(25544),
            parent: None,
            name: "ISS (ZARYA)".into(),
            constellation: Constellation::Stations,
            shell: "LEO 400–450 km 52°".into(),
            body: Body::Object(Sighting {
                norad: 25544,
                azimuth: 120.,
                elevation,
                range: 1_200.4,
                range_rate: -5.,
                altitude: 420.,
                speed: 7.66,
                sunlit: true,
            }),
        }
    }

    #[test]
    fn test_changed_columns_match_the_values() {
        let before = object(-0.5);
        let mut after = object(0.5);
        after.name = before.name.clone();
        let mut fast = ColumnSet::EMPTY;
        after.changed_columns(&before, &mut fast);
        for &column in SkyColumn::all() {
            let differs = after.value(column) != before.value(column);
            assert_eq!(fast.contains(column.ix()), differs, "{column:?}");
        }
        assert_eq!(
            after.change_direction(&before, SkyColumn::Elevation),
            Some(ChangeDirection::Up)
        );
        assert_eq!(
            before.change_direction(&after, SkyColumn::Pass),
            Some(ChangeDirection::Down)
        );
        assert_eq!(after.change_direction(&after, SkyColumn::Pass), None);
    }

    #[test]
    fn test_pass_text_sorts_up_first() {
        assert!(UP < DOWN);
    }
}
