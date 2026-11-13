//! The fleet: every vessel heard, from a source's messages. A location
//! makes a vessel's row (or moves it); metadata fills its name, type and
//! voyage, kept aside until the vessel's first location if it comes
//! first. A vessel silent for `dim_after` dims, and after `leave_after`
//! leaves. The fleet only says what to write ([`Writes`]); the driver
//! writes it to the grid store in one batch. No GPUI types.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use super::{
    ais::{Location, Message, Metadata, ShipCategory},
    rows::VesselRow,
    source::SourceEvent,
    speed::{SpeedLimits, SpeedRule},
};

/// When a quiet vessel dims and when it leaves. A moored vessel reports
/// its position every three minutes, a moving one every few seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaleRule {
    pub dim_after: Duration,
    pub leave_after: Duration,
}

impl Default for StaleRule {
    fn default() -> Self {
        Self {
            dim_after: Duration::from_secs(10 * 60),
            leave_after: Duration::from_secs(30 * 60),
        }
    }
}

impl StaleRule {
    fn is_stale(&self, seen_ms: f64, now_ms: f64) -> bool {
        now_ms - seen_ms >= self.dim_after.as_secs_f64() * 1e3
    }

    fn is_gone(&self, seen_ms: f64, now_ms: f64) -> bool {
        now_ms - seen_ms >= self.leave_after.as_secs_f64() * 1e3
    }
}

/// Points a vessel's wake keeps, and the distance between two, degrees of
/// latitude (about 150 m).
const TRAIL_POINTS: usize = 48;
const TRAIL_STEP: f64 = 0.0014;

/// One vessel's state.
#[derive(Clone, Debug)]
pub struct Vessel {
    row: Arc<VesselRow>,
    location: Location,
    metadata: Option<Arc<Metadata>>,
    seen_ms: f64,
    /// Where it has been, oldest first: longitude and latitude.
    trail: VecDeque<(f32, f32)>,
}

impl Vessel {
    pub fn row(&self) -> &Arc<VesselRow> {
        &self.row
    }

    pub fn metadata(&self) -> Option<&Arc<Metadata>> {
        self.metadata.as_ref()
    }

    pub fn seen_ms(&self) -> f64 {
        self.seen_ms
    }

    pub fn trail(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.trail.iter().copied()
    }

    fn rebuild(&mut self, stale: bool, speeds: &dyn SpeedRule) -> Arc<VesselRow> {
        self.row = Arc::new(VesselRow::new(
            self.row.mmsi,
            &self.location,
            self.metadata.as_deref(),
            self.seen_ms,
            stale,
            speeds,
        ));
        self.row.clone()
    }

    fn follow(&mut self) {
        let Some((lat, lon)) = self.location.position else {
            return;
        };
        let point = (lon as f32, lat as f32);
        let far_enough = self.trail.back().is_none_or(|&(last_lon, last_lat)| {
            let dlat = f64::from(lat as f32 - last_lat);
            let dlon = f64::from(lon as f32 - last_lon) * lat.to_radians().cos();
            dlat.hypot(dlon) >= TRAIL_STEP
        });
        if far_enough {
            if self.trail.len() == TRAIL_POINTS {
                self.trail.pop_front();
            }
            self.trail.push_back(point);
        } else if let Some(last) = self.trail.back_mut() {
            *last = point;
        }
    }
}

/// What to write to the store.
#[derive(Debug, Default)]
pub struct Writes {
    /// New or changed rows, highlighted.
    pub upserts: Vec<Arc<VesselRow>>,
    /// Rows rewritten without a highlight: a vessel that dims.
    pub silent: Vec<Arc<VesselRow>>,
    /// Vessels that left.
    pub removed: Vec<u32>,
}

impl Writes {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.silent.is_empty() && self.removed.is_empty()
    }
}

/// Every vessel heard.
pub struct Fleet {
    rule: StaleRule,
    /// Which reported speeds are believed.
    speeds: Arc<dyn SpeedRule>,
    vessels: HashMap<u32, Vessel>,
    /// Metadata of vessels without a location yet.
    waiting: HashMap<u32, Arc<Metadata>>,
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new(StaleRule::default())
    }
}

impl Fleet {
    pub fn new(rule: StaleRule) -> Self {
        Self {
            rule,
            speeds: Arc::new(SpeedLimits::default()),
            vessels: HashMap::new(),
            waiting: HashMap::new(),
        }
    }

    /// The same, believing the speeds `speeds` says are plausible.
    pub fn with_speed_rule(mut self, speeds: Arc<dyn SpeedRule>) -> Self {
        self.speeds = speeds;
        self
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.vessels.len()
    }

    pub fn vessel(&self, mmsi: u32) -> Option<&Vessel> {
        self.vessels.get(&mmsi)
    }

    /// Applies `event` at the source's `now_ms`, saying what to write.
    pub fn apply(&mut self, event: &SourceEvent, now_ms: f64, out: &mut Writes) {
        let at_ms = event.at_ms.min(now_ms);
        match &event.message {
            Message::Location { mmsi, location } => {
                self.on_location(*mmsi, location, at_ms, now_ms, out)
            }
            Message::Metadata { mmsi, metadata } => {
                self.on_metadata(*mmsi, metadata, at_ms, now_ms, out)
            }
            Message::Status => {}
        }
    }

    fn on_location(
        &mut self,
        mmsi: u32,
        location: &Location,
        at_ms: f64,
        now_ms: f64,
        out: &mut Writes,
    ) {
        // Too old to show, or nowhere: nothing to add.
        if self.rule.is_gone(at_ms, now_ms) {
            return;
        }
        let stale = self.rule.is_stale(at_ms, now_ms);
        match self.vessels.get_mut(&mmsi) {
            Some(vessel) => {
                // An older report than the one shown (a snapshot behind
                // the stream) changes nothing.
                if location.time_ms > 0. && location.time_ms < vessel.location.time_ms {
                    return;
                }
                let position = location.position.or(vessel.location.position);
                vessel.location = Location {
                    position,
                    ..*location
                };
                vessel.seen_ms = vessel.seen_ms.max(at_ms);
                vessel.follow();
                let stale = self.rule.is_stale(vessel.seen_ms, now_ms);
                out.upserts.push(vessel.rebuild(stale, &*self.speeds));
            }
            None if location.position.is_some() => {
                let metadata = self.waiting.remove(&mmsi);
                let mut vessel = Vessel {
                    row: Arc::new(VesselRow::new(
                        mmsi,
                        location,
                        metadata.as_deref(),
                        at_ms,
                        stale,
                        &*self.speeds,
                    )),
                    location: *location,
                    metadata,
                    seen_ms: at_ms,
                    trail: VecDeque::new(),
                };
                vessel.follow();
                out.upserts.push(vessel.row.clone());
                self.vessels.insert(mmsi, vessel);
            }
            None => {}
        }
    }

    fn on_metadata(
        &mut self,
        mmsi: u32,
        metadata: &Arc<Metadata>,
        at_ms: f64,
        now_ms: f64,
        out: &mut Writes,
    ) {
        let Some(vessel) = self.vessels.get_mut(&mmsi) else {
            let newer = self
                .waiting
                .get(&mmsi)
                .is_none_or(|kept| kept.time_ms <= metadata.time_ms);
            if newer {
                self.waiting.insert(mmsi, metadata.clone());
            }
            return;
        };
        let changed = vessel
            .metadata
            .as_deref()
            .is_none_or(|kept| !kept.says_the_same(metadata));
        let older = vessel
            .metadata
            .as_ref()
            .is_some_and(|kept| metadata.time_ms > 0. && metadata.time_ms < kept.time_ms);
        let was_stale = vessel.row.stale;
        vessel.seen_ms = vessel.seen_ms.max(at_ms);
        let stale = self.rule.is_stale(vessel.seen_ms, now_ms);
        if changed && !older {
            vessel.metadata = Some(metadata.clone());
            out.upserts.push(vessel.rebuild(stale, &*self.speeds));
        } else if was_stale != stale {
            out.silent.push(vessel.rebuild(stale, &*self.speeds));
        }
    }

    /// Dims the vessels gone quiet and removes those gone for good, at the
    /// source's `now_ms`.
    pub fn sweep(&mut self, now_ms: f64, out: &mut Writes) {
        let rule = self.rule;
        let speeds = &*self.speeds;
        self.vessels.retain(|mmsi, vessel| {
            if rule.is_gone(vessel.seen_ms, now_ms) {
                out.removed.push(*mmsi);
                return false;
            }
            let stale = rule.is_stale(vessel.seen_ms, now_ms);
            if stale != vessel.row.stale {
                out.silent.push(vessel.rebuild(stale, speeds));
            }
            true
        });
    }

    /// The counts the status bar shows.
    pub fn counts(&self) -> FleetCounts {
        let mut counts = FleetCounts::default();
        for vessel in self.vessels.values() {
            counts.vessels += 1;
            counts.moving += u32::from(vessel.row.moving);
            counts.berthed += u32::from(vessel.row.is_berthed());
            counts.stale += u32::from(vessel.row.stale);
            counts.named += u32::from(!vessel.row.name.is_empty());
        }
        counts
    }

    /// Every vessel with a position, for the map.
    pub fn dots(&self) -> Vec<Dot> {
        self.vessels
            .values()
            .filter_map(|vessel| {
                let (lat, lon) = vessel.location.position?;
                Some(Dot {
                    mmsi: vessel.row.mmsi,
                    lon: lon as f32,
                    lat: lat as f32,
                    bearing: vessel.location.bearing(),
                    moving: vessel.row.moving,
                    category: vessel.row.category,
                    stale: vessel.row.stale,
                })
            })
            .collect()
    }

    /// The wakes of the moving vessels, for the map.
    pub fn trails(&self) -> Vec<Trail> {
        self.vessels
            .values()
            .filter(|vessel| vessel.trail.len() > 1 && !vessel.row.stale)
            .map(|vessel| Trail {
                category: vessel.row.category,
                points: vessel.trail.iter().copied().collect(),
            })
            .collect()
    }
}

/// Vessels by what they are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FleetCounts {
    pub vessels: u32,
    pub moving: u32,
    pub berthed: u32,
    pub stale: u32,
    /// Vessels whose metadata has arrived.
    pub named: u32,
}

/// A vessel on the map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub mmsi: u32,
    pub lon: f32,
    pub lat: f32,
    /// Degrees, where the bow points; `None` for a vessel without heading
    /// that is not moving.
    pub bearing: Option<f32>,
    pub moving: bool,
    pub category: ShipCategory,
    pub stale: bool,
}

/// A moving vessel's wake.
#[derive(Clone, Debug, PartialEq)]
pub struct Trail {
    pub category: ShipCategory,
    pub points: Vec<(f32, f32)>,
}

/// Messages a second, per second of the driver's time: locations and
/// metadata, the last `HISTORY` seconds.
#[derive(Clone, Debug, Default)]
pub struct RateMeter {
    /// The second of the newest bucket.
    second: u64,
    buckets: VecDeque<(u32, u32)>,
}

/// Seconds of history the rate chart shows.
pub const HISTORY: usize = 120;

impl RateMeter {
    /// Counts `event` at the driver's `now`. A snapshot is not traffic.
    pub fn count(&mut self, event: &SourceEvent, now: Duration) {
        if event.snapshot {
            return;
        }
        self.advance(now);
        let Some(bucket) = self.buckets.back_mut() else {
            return;
        };
        match event.message {
            Message::Location { .. } => bucket.0 += 1,
            Message::Metadata { .. } => bucket.1 += 1,
            Message::Status => {}
        }
    }

    /// Moves the newest bucket to `now`'s second.
    pub fn advance(&mut self, now: Duration) {
        let second = now.as_secs();
        if self.buckets.is_empty() {
            self.second = second;
            self.buckets.push_back((0, 0));
        }
        while self.second < second {
            self.second += 1;
            self.buckets.push_back((0, 0));
            if self.buckets.len() > HISTORY + 1 {
                self.buckets.pop_front();
            }
        }
    }

    /// The whole seconds, oldest first, without the one under way.
    pub fn history(&self) -> Vec<(u32, u32)> {
        let whole = self.buckets.len().saturating_sub(1);
        self.buckets.iter().take(whole).copied().collect()
    }

    /// Messages a second over the last `seconds` whole seconds.
    pub fn rate(&self, seconds: usize) -> f32 {
        let history = self.history();
        let recent: Vec<_> = history.iter().rev().take(seconds).collect();
        if recent.is_empty() {
            return 0.;
        }
        let total: u32 = recent.iter().map(|(loc, meta)| loc + meta).sum();
        total as f32 / recent.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::super::ais::{Eta, NavStatus};
    use super::*;

    const MINUTE: f64 = 60_000.;

    fn rule() -> StaleRule {
        StaleRule {
            dim_after: Duration::from_secs(60),
            leave_after: Duration::from_secs(180),
        }
    }

    fn location(time_ms: f64, lat: f64, speed: f32) -> Message {
        Message::Location {
            mmsi: 7,
            location: Location {
                time_ms,
                speed: Some(speed),
                course: Some(90.),
                heading: Some(91),
                status: NavStatus::UNDER_WAY,
                position: Some((lat, 25.)),
            },
        }
    }

    fn metadata(time_ms: f64, name: &str) -> Message {
        Message::Metadata {
            mmsi: 7,
            metadata: Arc::new(Metadata {
                name: name.into(),
                call_sign: "OJAB".into(),
                destination: "HELSINKI".into(),
                eta: Eta::pack(11, 8, 12, 0),
                draught: 50,
                imo: 1,
                ship_type: 60,
                dimensions: [50, 10, 5, 5],
                time_ms,
            }),
        }
    }

    fn event(at_ms: f64, message: Message) -> SourceEvent {
        SourceEvent {
            at_ms,
            message,
            snapshot: false,
        }
    }

    #[test]
    fn test_metadata_waits_for_the_first_location() {
        let mut fleet = Fleet::new(rule());
        let mut out = Writes::default();
        fleet.apply(&event(0., metadata(0., "AURORA")), 0., &mut out);
        assert!(out.is_empty());
        assert_eq!(fleet.len(), 0);
        fleet.apply(&event(1_000., location(1_000., 60., 12.)), 1_000., &mut out);
        assert_eq!(out.upserts.len(), 1);
        let row = &out.upserts[0];
        assert_eq!(row.name.as_ref(), "AURORA");
        assert_eq!(row.category, ShipCategory::Passenger);
        assert!(row.moving);
        // The same metadata again writes nothing; a new destination does.
        out.upserts.clear();
        fleet.apply(&event(2_000., metadata(2_000., "AURORA")), 2_000., &mut out);
        assert!(out.is_empty());
        fleet.apply(
            &event(3_000., metadata(3_000., "AURORA II")),
            3_000.,
            &mut out,
        );
        assert_eq!(out.upserts.len(), 1);
        assert_eq!(fleet.counts().named, 1);
    }

    #[test]
    fn test_speeds_are_believed_by_the_fleet_s_rule() {
        // The default: a passenger ship at 45 knots is not believed.
        let mut fleet = Fleet::new(rule());
        let mut out = Writes::default();
        fleet.apply(&event(0., metadata(0., "AURORA")), 0., &mut out);
        fleet.apply(&event(1_000., location(1_000., 60., 45.)), 1_000., &mut out);
        assert_eq!(out.upserts[0].speed, None);
        assert_eq!(fleet.counts().moving, 0);
        // Another rule, given to the fleet, decides instead.
        struct Anything;
        impl SpeedRule for Anything {
            fn is_plausible(&self, _: ShipCategory, _: f32) -> bool {
                true
            }
        }
        let mut fleet = Fleet::new(rule()).with_speed_rule(Arc::new(Anything));
        let mut out = Writes::default();
        fleet.apply(&event(0., metadata(0., "AURORA")), 0., &mut out);
        fleet.apply(&event(1_000., location(1_000., 60., 45.)), 1_000., &mut out);
        assert_eq!(out.upserts[0].speed, Some(45.));
        assert_eq!(fleet.counts().moving, 1);
    }

    #[test]
    fn test_quiet_vessels_dim_then_leave() {
        let mut fleet = Fleet::new(rule());
        let mut out = Writes::default();
        fleet.apply(&event(0., location(0., 60., 12.)), 0., &mut out);
        out.upserts.clear();
        fleet.sweep(0.5 * MINUTE, &mut out);
        assert!(out.is_empty());
        fleet.sweep(1.0 * MINUTE, &mut out);
        assert_eq!(out.silent.len(), 1);
        assert!(out.silent[0].stale);
        assert_eq!(fleet.counts().stale, 1);
        // Heard again: bright again.
        out.silent.clear();
        fleet.apply(
            &event(1.5 * MINUTE, location(1.5 * MINUTE, 60.01, 12.)),
            1.5 * MINUTE,
            &mut out,
        );
        assert!(!out.upserts[0].stale);
        out.upserts.clear();
        fleet.sweep(4.4 * MINUTE, &mut out);
        assert!(out.removed.is_empty());
        fleet.sweep(4.5 * MINUTE, &mut out);
        assert_eq!(out.removed, [7]);
        assert_eq!(fleet.len(), 0);
    }

    #[test]
    fn test_old_snapshot_vessels_arrive_dim_or_not_at_all() {
        let mut fleet = Fleet::new(rule());
        let mut out = Writes::default();
        let now = 10. * MINUTE;
        fleet.apply(
            &event(now - 2. * MINUTE, location(now - 2. * MINUTE, 60., 0.)),
            now,
            &mut out,
        );
        assert!(out.upserts[0].stale);
        let mut out = Writes::default();
        let mut far = event(now - 4. * MINUTE, location(now - 4. * MINUTE, 60., 0.));
        if let Message::Location { mmsi, .. } = &mut far.message {
            *mmsi = 8;
        }
        fleet.apply(&far, now, &mut out);
        assert!(out.is_empty());
        // A report older than the one shown changes nothing.
        let mut out = Writes::default();
        fleet.apply(
            &event(now, location(now - 3. * MINUTE, 61., 0.)),
            now,
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn test_trails_keep_a_wake_of_distinct_points() {
        let mut fleet = Fleet::new(rule());
        let mut out = Writes::default();
        for step in 0..100 {
            let at = f64::from(step) * 1_000.;
            fleet.apply(
                &event(at, location(at, 60. + f64::from(step) * 0.002, 10.)),
                at,
                &mut out,
            );
        }
        let trails = fleet.trails();
        assert_eq!(trails.len(), 1);
        assert_eq!(trails[0].points.len(), TRAIL_POINTS);
        // The newest point is where the vessel is.
        let vessel = fleet.vessel(7).expect("a vessel");
        let last = vessel.trail().last().expect("a point");
        assert!((last.1 - 60.198).abs() < 1e-4);
        let dots = fleet.dots();
        assert_eq!(dots[0].bearing, Some(91.));
    }

    #[test]
    fn test_rate_meter_counts_whole_seconds() {
        let mut meter = RateMeter::default();
        let stream = event(0., location(0., 60., 1.));
        let snapshot = SourceEvent {
            snapshot: true,
            ..stream.clone()
        };
        for ms in (0..3_000).step_by(100) {
            meter.count(&stream, Duration::from_millis(ms));
            meter.count(&snapshot, Duration::from_millis(ms));
        }
        meter.advance(Duration::from_millis(3_500));
        assert_eq!(meter.history(), [(10, 0), (10, 0), (10, 0)]);
        assert_eq!(meter.rate(5), 10.);
        meter.advance(Duration::from_secs(1_000));
        assert_eq!(meter.history().len(), HISTORY);
        assert_eq!(meter.rate(5), 0.);
    }
}
