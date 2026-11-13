//! Which reported speeds the table believes. A transponder sometimes
//! reports a speed its vessel cannot make (a cargo ship at 102 knots, a
//! tug at 60): a GPS glitch, a test, a wrong setting. A [`SpeedRule`]
//! says whether a speed is plausible for a type of vessel; the fleet
//! (`fleet.rs`) shows one that is not as unknown, so it sorts with the
//! unknown speeds and stays out of the captions' averages. The default,
//! [`SpeedLimits`], is a limit a category; a stricter or a learned rule
//! is another implementation, given to the fleet with
//! `Fleet::with_speed_rule`. No GPUI types.

use super::ais::ShipCategory;

/// Whether a reported speed is believed.
pub trait SpeedRule: Send + Sync {
    /// Can a vessel of `category` make `knots` over ground?
    fn is_plausible(&self, category: ShipCategory, knots: f32) -> bool;
}

/// The highest speed believed of each category, knots. Displacement hulls
/// (cargo ships, tankers, tugs and passenger ships) stay well under 40
/// knots; high-speed craft, service boats (pilots, rescue) and leisure
/// craft go faster; a vessel whose type is not known yet gets the most
/// room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeedLimits {
    displacement: f32,
    fast: f32,
    other: f32,
}

impl Default for SpeedLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl SpeedLimits {
    /// 40 knots for displacement hulls, 70 for fast craft and vessels of
    /// no known type yet, 60 for the rest.
    pub const DEFAULT: Self = Self {
        displacement: 40.,
        fast: 70.,
        other: 60.,
    };

    /// The limit of `category`, knots.
    pub fn limit(&self, category: ShipCategory) -> f32 {
        match category {
            ShipCategory::Cargo
            | ShipCategory::Tanker
            | ShipCategory::Tug
            | ShipCategory::Passenger => self.displacement,
            ShipCategory::HighSpeed | ShipCategory::Leisure | ShipCategory::Unknown => self.fast,
            ShipCategory::Service | ShipCategory::Special | ShipCategory::Other => self.other,
        }
    }
}

impl SpeedRule for SpeedLimits {
    fn is_plausible(&self, category: ShipCategory, knots: f32) -> bool {
        knots.is_finite() && knots >= 0. && knots <= self.limit(category)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_limits_by_category() {
        let rule = SpeedLimits::default();
        for category in [
            ShipCategory::Cargo,
            ShipCategory::Tanker,
            ShipCategory::Tug,
            ShipCategory::Passenger,
        ] {
            assert!(rule.is_plausible(category, 21.5), "{category:?}");
            assert!(rule.is_plausible(category, 40.), "{category:?}");
            assert!(!rule.is_plausible(category, 40.1), "{category:?}");
        }
        assert!(rule.is_plausible(ShipCategory::HighSpeed, 45.));
        assert!(!rule.is_plausible(ShipCategory::HighSpeed, 80.));
        assert!(rule.is_plausible(ShipCategory::Unknown, 55.));
        assert!(rule.is_plausible(ShipCategory::Service, 50.));
        assert!(!rule.is_plausible(ShipCategory::Service, 61.));
        assert!(!rule.is_plausible(ShipCategory::Leisure, f32::NAN));
        assert!(!rule.is_plausible(ShipCategory::Leisure, -1.));
    }
}
