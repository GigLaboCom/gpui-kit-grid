//! The rows the operations simulator writes and the grids show: alerts
//! keyed by their dedup key, the fleet as a tree, and the services. No GPUI
//! types; `value` borrows everything it shows, so it never allocates.

use std::{any::TypeId, cmp::Ordering, sync::Arc};

use gpui_kit_grid::{CellValue, ColumnIx, GridColumns, GridRow, GridValue, ImageValue};

/// How bad something is: an alert's severity, or the status of a host, a
/// branch of the fleet or a service. Worst first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    Major,
    Minor,
    Warning,
    #[default]
    Ok,
}

impl Severity {
    /// The severities an alert can have, worst first: the order of the
    /// filter chips, the caption counts and the chart's series.
    pub const ALERTS: [Self; 4] = [Self::Critical, Self::Major, Self::Minor, Self::Warning];

    pub fn ix(self) -> usize {
        self as usize
    }

    /// The word on a chip and in the chart's legend.
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::Major => "Major",
            Self::Minor => "Minor",
            Self::Warning => "Warning",
            Self::Ok => "OK",
        }
    }

    /// A shape for each severity, so it never rests on colour alone:
    /// square, triangle, diamond, circle, check.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Critical => "■",
            Self::Major => "▲",
            Self::Minor => "◆",
            Self::Warning => "●",
            Self::Ok => "✓",
        }
    }

    /// The icon and the label, as a cell's pill shows them.
    pub fn pill(self) -> &'static str {
        match self {
            Self::Critical => "■ Critical",
            Self::Major => "▲ Major",
            Self::Minor => "◆ Minor",
            Self::Warning => "● Warning",
            Self::Ok => "✓ OK",
        }
    }

    /// The severity's mark as an image cell: a colour SVG the services
    /// grid's image source draws (`services.rs`), with its label as the
    /// cell's text.
    pub fn mark(self) -> CellValue<'static> {
        let id = match self {
            Self::Critical => "ops/severity/critical",
            Self::Major => "ops/severity/major",
            Self::Minor => "ops/severity/minor",
            Self::Warning => "ops/severity/warning",
            Self::Ok => "ops/severity/ok",
        };
        CellValue::Image(ImageValue::new(id, self.label()))
    }

    /// The severity of a mark's id.
    pub fn from_mark(id: &str) -> Option<Self> {
        [
            Self::Critical,
            Self::Major,
            Self::Minor,
            Self::Warning,
            Self::Ok,
        ]
        .into_iter()
        .find(|severity| matches!(severity.mark(), CellValue::Image(image) if image.id() == id))
    }

    /// The worse of two.
    pub fn worst(self, other: Self) -> Self {
        self.min(other)
    }
}

// #region severity-value
/// A severity is a cell value of its own (`CellValue::Custom`): it sorts by
/// rank, worst first, and reads as its label ("Critical") in a copy, the
/// search box and a screen reader.
impl GridValue for Severity {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        match other.downcast_ref::<Severity>() {
            Some(other) => self.cmp(other),
            None => TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        out.push_str(self.label());
    }
}
// #endregion severity-value

/// Where an alert is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlertState {
    Firing,
    Acknowledged,
    Resolved,
}

impl AlertState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Firing => "FIRING",
            Self::Acknowledged => "ACKED",
            Self::Resolved => "RESOLVED",
        }
    }

    /// Firing or acknowledged: it still counts.
    pub fn is_active(self) -> bool {
        self != Self::Resolved
    }
}

/// What an alert watches: a rule over a hall, a rack, a host or a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    PowerFeedDown,
    CoolingDegraded,
    PduOneFeed,
    HostUnreachable,
    InletTempHigh,
    CpuThrottled,
    HostErrors,
    DiskLatency,
    MemoryPressure,
    ClockDrift,
    ErrorRate,
    LatencySlo,
    BudgetBurn,
}

impl Rule {
    /// The alert's text in the console.
    pub fn name(self) -> &'static str {
        match self {
            Self::PowerFeedDown => "power feed B down",
            Self::CoolingDegraded => "cooling capacity low",
            Self::PduOneFeed => "PDU on a single feed",
            Self::HostUnreachable => "host unreachable",
            Self::InletTempHigh => "inlet temperature high",
            Self::CpuThrottled => "CPU thermal throttling",
            Self::HostErrors => "host error rate high",
            Self::DiskLatency => "disk latency high",
            Self::MemoryPressure => "memory pressure",
            Self::ClockDrift => "clock drift",
            Self::ErrorRate => "5xx error rate high",
            Self::LatencySlo => "p99 latency over SLO",
            Self::BudgetBurn => "error budget burn",
        }
    }
}

/// The thing an alert is about: a fleet node by its id, or a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subject {
    Node(u32),
    Service(u16),
}

/// The dedup key: one row per rule and subject, however often it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AlertKey {
    pub rule: Rule,
    pub subject: Subject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlertColumn {
    Severity,
    State,
    Alert,
    Subject,
    Service,
    Dc,
    Count,
    Rate,
    FirstSeen,
    LastSeen,
    /// 1 for an active critical alert, else 0: summed, the captions'
    /// per-severity counts. Internal, like the three after it.
    Critical,
    Major,
    Minor,
    Warning,
}

impl AlertColumn {
    /// The internal counter column of an alert severity.
    pub fn counter(severity: Severity) -> Option<Self> {
        match severity {
            Severity::Critical => Some(Self::Critical),
            Severity::Major => Some(Self::Major),
            Severity::Minor => Some(Self::Minor),
            Severity::Warning => Some(Self::Warning),
            Severity::Ok => None,
        }
    }
}

impl GridColumns for AlertColumn {
    fn all() -> &'static [Self] {
        use AlertColumn::*;
        &[
            Severity, State, Alert, Subject, Service, Dc, Count, Rate, FirstSeen, LastSeen,
            Critical, Major, Minor, Warning,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Severity => "severity",
            Self::State => "state",
            Self::Alert => "alert",
            Self::Subject => "subject",
            Self::Service => "service",
            Self::Dc => "dc",
            Self::Count => "count",
            Self::Rate => "rate",
            Self::FirstSeen => "first_seen",
            Self::LastSeen => "last_seen",
            Self::Critical => "critical",
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Warning => "warning",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// One alert, keyed by rule and subject. The simulator writes it again on
/// every firing of its key, with the count and the rate it keeps; the grid
/// stores the latest row of each key and counts nothing.
#[derive(Clone, Debug)]
pub struct Alert {
    pub key: AlertKey,
    pub severity: Severity,
    pub state: AlertState,
    /// The host, rack, hall or service name.
    pub subject: Arc<str>,
    /// The service it hurts; `facilities` for power and cooling.
    pub service: Arc<str>,
    /// The data center; `global` for a service alert.
    pub dc: Arc<str>,
    /// Firings since the alert first fired, counted by the simulator.
    pub count: i64,
    /// Firings a second over the simulator's last few seconds; 0 once
    /// resolved.
    pub rate: f64,
    /// Simulated wall clock, `hh:mm:ss`.
    pub first_seen: Arc<str>,
    pub last_seen: Arc<str>,
}

impl GridRow for Alert {
    type Key = AlertKey;
    type Column = AlertColumn;

    fn key(&self) -> &AlertKey {
        &self.key
    }

    fn value(&self, column: AlertColumn) -> CellValue<'_> {
        match column {
            AlertColumn::Severity => CellValue::Custom(&self.severity),
            AlertColumn::State => CellValue::Text(self.state.label()),
            AlertColumn::Alert => CellValue::Text(self.key.rule.name()),
            AlertColumn::Subject => CellValue::Text(&self.subject),
            AlertColumn::Service => CellValue::Text(&self.service),
            AlertColumn::Dc => CellValue::Text(&self.dc),
            AlertColumn::Count => CellValue::Int(self.count),
            AlertColumn::Rate => CellValue::Float(self.rate),
            AlertColumn::FirstSeen => CellValue::Text(&self.first_seen),
            AlertColumn::LastSeen => CellValue::Text(&self.last_seen),
            AlertColumn::Critical
            | AlertColumn::Major
            | AlertColumn::Minor
            | AlertColumn::Warning => {
                let counts =
                    self.state.is_active() && AlertColumn::counter(self.severity) == Some(column);
                CellValue::Int(counts.into())
            }
        }
    }
}

/// Where a node sits in the fleet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Region,
    Dc,
    Hall,
    Rack,
    Host,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FleetColumn {
    Name,
    Status,
    Hosts,
    InAlert,
    Temp,
    Power,
    Cpu,
    Mem,
    Net,
    Errors,
    Service,
}

impl GridColumns for FleetColumn {
    fn all() -> &'static [Self] {
        use FleetColumn::*;
        &[
            Name, Status, Hosts, InAlert, Temp, Power, Cpu, Mem, Net, Errors, Service,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Status => "status",
            Self::Hosts => "hosts",
            Self::InAlert => "in_alert",
            Self::Temp => "temp",
            Self::Power => "power",
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Net => "net",
            Self::Errors => "errors",
            Self::Service => "service",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A node of the fleet: a host with its metrics, or a region, data center,
/// hall or rack with its hosts' roll-ups, which the simulator writes as
/// the node's own values.
#[derive(Clone, Debug, PartialEq)]
pub struct FleetNode {
    pub id: u32,
    pub parent: Option<u32>,
    pub level: Level,
    pub name: Arc<str>,
    /// A host's worst active alert; a branch's worst of its own alerts and
    /// its children's.
    pub status: Severity,
    /// Hosts under a branch; 1 for a host.
    pub hosts: u32,
    /// Hosts with an active alert: under a branch, or 1 or 0 for a host.
    pub in_alert: u32,
    /// Inlet temperature, °C; a branch's hottest host.
    pub temp: i64,
    /// Power draw, kW; a branch's sum.
    pub power: f64,
    /// CPU and memory use, %; a branch's average.
    pub cpu: i64,
    pub mem: i64,
    /// Network, Gb/s; a branch's sum.
    pub net: f64,
    /// Errors per second; a branch's sum.
    pub errors: f64,
    /// A host's service.
    pub service: Option<Arc<str>>,
}

impl FleetNode {
    pub fn is_host(&self) -> bool {
        self.level == Level::Host
    }
}

impl GridRow for FleetNode {
    type Key = u32;
    type Column = FleetColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: FleetColumn) -> CellValue<'_> {
        match column {
            FleetColumn::Name => CellValue::Text(&self.name),
            FleetColumn::Status => CellValue::Custom(&self.status),
            // A host counts itself: the totals add up the hosts.
            FleetColumn::Hosts => CellValue::Int(self.hosts.into()),
            // Blank rather than 0: a quiet screen stays quiet.
            FleetColumn::InAlert if self.in_alert == 0 => CellValue::Empty,
            FleetColumn::InAlert => CellValue::Int(self.in_alert.into()),
            FleetColumn::Temp => CellValue::Int(self.temp),
            FleetColumn::Power => CellValue::Float(self.power),
            FleetColumn::Cpu => CellValue::Int(self.cpu),
            FleetColumn::Mem => CellValue::Int(self.mem),
            FleetColumn::Net => CellValue::Float(self.net),
            FleetColumn::Errors => CellValue::Float(self.errors),
            FleetColumn::Service => self
                .service
                .as_deref()
                .map_or(CellValue::Empty, CellValue::Text),
        }
    }

    fn parent_key(&self) -> Option<&u32> {
        self.parent.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceColumn {
    Service,
    Status,
    Slo,
    Budget,
    P99,
    Errors,
    Hosts,
}

impl GridColumns for ServiceColumn {
    fn all() -> &'static [Self] {
        use ServiceColumn::*;
        &[Service, Status, Slo, Budget, P99, Errors, Hosts]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Status => "status",
            Self::Slo => "slo",
            Self::Budget => "budget",
            Self::P99 => "p99",
            Self::Errors => "errors",
            Self::Hosts => "hosts",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A service's health against its SLO.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceRow {
    pub id: u16,
    pub name: Arc<str>,
    pub status: Severity,
    /// The availability objective, %.
    pub slo: f64,
    /// The error budget left this window, %.
    pub budget: f64,
    /// p99 latency, ms.
    pub p99: i64,
    /// Requests that failed, %.
    pub errors: f64,
    pub hosts: u32,
}

impl GridRow for ServiceRow {
    type Key = u16;
    type Column = ServiceColumn;

    fn key(&self) -> &u16 {
        &self.id
    }

    fn value(&self, column: ServiceColumn) -> CellValue<'_> {
        match column {
            ServiceColumn::Service => CellValue::Text(&self.name),
            ServiceColumn::Status => self.status.mark(),
            ServiceColumn::Slo => CellValue::Float(self.slo),
            ServiceColumn::Budget => CellValue::Float(self.budget),
            ServiceColumn::P99 => CellValue::Int(self.p99),
            ServiceColumn::Errors => CellValue::Float(self.errors),
            ServiceColumn::Hosts => CellValue::Int(self.hosts.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_sorts_worst_first_and_reads_as_its_label() {
        let all = [
            Severity::Critical,
            Severity::Major,
            Severity::Minor,
            Severity::Warning,
            Severity::Ok,
        ];
        for pair in all.windows(2) {
            let (a, b) = (CellValue::Custom(&pair[0]), CellValue::Custom(&pair[1]));
            assert!(a.cmp_total(&b).is_lt(), "{pair:?}");
            assert!(pair[0] < pair[1] && pair[0].worst(pair[1]) == pair[0]);
        }
        let mut text = String::new();
        Severity::Critical.write_text(&mut text);
        assert_eq!(text, "Critical");
        // Its mark: an image with its label, found again by its id.
        for severity in all {
            let CellValue::Image(image) = severity.mark() else {
                panic!("a mark is an image");
            };
            assert_eq!(image.text(), severity.label());
            assert_eq!(Severity::from_mark(image.id()), Some(severity));
        }
        assert_eq!(Severity::from_mark("ops/severity/unknown"), None);
    }
}
