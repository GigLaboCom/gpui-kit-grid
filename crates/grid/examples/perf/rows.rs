//! The rows the sampler writes and the grid shows: one per process, keyed
//! by its pid and start time (pids are reused), and the machine's own rows
//! pinned on top. Bytes, byte rates and run times are cell values of their
//! own (`CellValue::Custom`): they sort as numbers, export as numbers and
//! read with their units. No GPUI types; `value` borrows everything it
//! shows, so it never allocates.

use std::{any::TypeId, cmp::Ordering, fmt::Write as _, sync::Arc};

use gpui_kit_grid::{
    AggregateFn, CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, GridValue, OwnedValue,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PerfColumn {
    Name,
    Pid,
    Cpu,
    Gpu,
    GpuMemory,
    Memory,
    DiskRead,
    DiskWrite,
    Threads,
    User,
    Uptime,
    App,
}

impl GridColumns for PerfColumn {
    fn all() -> &'static [Self] {
        use PerfColumn::*;
        &[
            Name, Pid, Cpu, Gpu, GpuMemory, Memory, DiskRead, DiskWrite, Threads, User, Uptime, App,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Pid => "pid",
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::GpuMemory => "gpu_memory",
            Self::Memory => "memory",
            Self::DiskRead => "disk_read",
            Self::DiskWrite => "disk_write",
            Self::Threads => "threads",
            Self::User => "user",
            Self::Uptime => "uptime",
            Self::App => "app",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// The machine's rows, pinned on top in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Machine {
    Cpu,
    Memory,
    Gpu,
    GpuMemory,
}

/// A row's identity: a process by its pid and its start time (Unix
/// seconds), so a reused pid is a new row; or one of the machine's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowKey {
    Process { pid: u32, start: u64 },
    Machine(Machine),
}

/// A process, or a machine row. Values the sampler did not get are `None`
/// and show blank: another user's process without root, the GPU columns
/// without a GPU source.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcRow {
    pub key: RowKey,
    /// The parent process, if it is still running.
    pub parent: Option<RowKey>,
    pub name: Arc<str>,
    /// The application bundle the executable is in (`Safari` for its
    /// helpers), or the process name.
    pub app: Arc<str>,
    pub user: Option<Arc<str>>,
    /// % of one core, as Activity Monitor counts: above 100 on several.
    pub cpu: Option<f64>,
    /// % of the GPU's time over the last sample.
    pub gpu: Option<f64>,
    /// GPU memory, where the source gives it per process (Linux).
    pub gpu_memory: Option<Bytes>,
    /// The memory footprint where the platform gives it, else the
    /// resident size.
    pub memory: Option<Bytes>,
    pub read: ByteRate,
    pub write: ByteRate,
    pub threads: Option<u32>,
    pub uptime: Option<Uptime>,
    /// The columns whose last change is large enough to show: the grid
    /// tints only these (`table.rs`), so a busy process does not flash
    /// every second for a tenth of a percent.
    pub notable: ColumnSet,
}

impl ProcRow {
    pub fn pid(&self) -> Option<u32> {
        match self.key {
            RowKey::Process { pid, .. } => Some(pid),
            RowKey::Machine(_) => None,
        }
    }

    pub fn is_machine(&self) -> bool {
        matches!(self.key, RowKey::Machine(_))
    }
}

impl GridRow for ProcRow {
    type Key = RowKey;
    type Column = PerfColumn;

    fn key(&self) -> &RowKey {
        &self.key
    }

    fn value(&self, column: PerfColumn) -> CellValue<'_> {
        match column {
            PerfColumn::Name => CellValue::Text(&self.name),
            PerfColumn::Pid => self
                .pid()
                .map_or(CellValue::Empty, |pid| CellValue::Int(pid.into())),
            PerfColumn::Cpu => self.cpu.map_or(CellValue::Empty, CellValue::Float),
            PerfColumn::Gpu => self.gpu.map_or(CellValue::Empty, CellValue::Float),
            PerfColumn::GpuMemory => self
                .gpu_memory
                .as_ref()
                .map_or(CellValue::Empty, |bytes| CellValue::Custom(bytes)),
            PerfColumn::Memory => self
                .memory
                .as_ref()
                .map_or(CellValue::Empty, |bytes| CellValue::Custom(bytes)),
            // Blank rather than 0: a quiet disk stays quiet.
            PerfColumn::DiskRead => self.read.cell(),
            PerfColumn::DiskWrite => self.write.cell(),
            PerfColumn::Threads => self
                .threads
                .map_or(CellValue::Empty, |threads| CellValue::Int(threads.into())),
            PerfColumn::User => self
                .user
                .as_deref()
                .map_or(CellValue::Empty, CellValue::Text),
            PerfColumn::Uptime => self
                .uptime
                .as_ref()
                .map_or(CellValue::Empty, |uptime| CellValue::Custom(uptime)),
            PerfColumn::App => CellValue::Text(&self.app),
        }
    }

    fn parent_key(&self) -> Option<&RowKey> {
        self.parent.as_ref()
    }
}

/// Orders a value of type `T` against another cell value: by `T`'s own
/// order, or by type.
fn cmp_as<T: GridValue + Ord>(value: &T, other: &dyn GridValue) -> Ordering {
    match other.downcast_ref::<T>() {
        Some(other) => value.cmp(other),
        None => TypeId::of::<T>().cmp(&other.as_any().type_id()),
    }
}

const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// `bytes` in binary units: `512 B`, `12.3 KiB`, `1.5 GiB`.
pub fn push_bytes(bytes: u64, out: &mut String) {
    if bytes < 1024 {
        let _ = write!(out, "{bytes} B");
        return;
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    let _ = write!(out, "{value:.1} {}", UNITS[unit]);
}

/// A size in bytes: shown in binary units, sorted and exported as the
/// number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(pub u64);

impl GridValue for Bytes {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        cmp_as(self, other)
    }

    fn write_text(&self, out: &mut String) {
        push_bytes(self.0, out);
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(self.0.min(i64::MAX as u64) as i64))
    }

    fn to_owned_value(&self) -> OwnedValue {
        OwnedValue::custom(*self)
    }
}

/// Bytes a second: `1.2 MiB/s`; blank when 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRate(pub u64);

impl ByteRate {
    fn cell(&self) -> CellValue<'_> {
        if self.0 == 0 {
            CellValue::Empty
        } else {
            CellValue::Custom(self)
        }
    }
}

impl GridValue for ByteRate {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        cmp_as(self, other)
    }

    fn write_text(&self, out: &mut String) {
        push_bytes(self.0, out);
        out.push_str("/s");
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(self.0.min(i64::MAX as u64) as i64))
    }

    fn to_owned_value(&self) -> OwnedValue {
        OwnedValue::custom(*self)
    }
}

/// How long a process has run, in seconds: `42s`, `12m 05s`, `3h 07m`,
/// `4d 02h`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uptime(pub u64);

impl GridValue for Uptime {
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
        cmp_as(self, other)
    }

    fn write_text(&self, out: &mut String) {
        let seconds = self.0;
        let (days, hours, minutes) = (seconds / 86_400, seconds / 3_600 % 24, seconds / 60 % 60);
        let _ = if seconds < 60 {
            write!(out, "{seconds}s")
        } else if seconds < 3_600 {
            write!(out, "{minutes}m {:02}s", seconds % 60)
        } else if seconds < 86_400 {
            write!(out, "{hours}h {minutes:02}m")
        } else {
            write!(out, "{days}d {hours:02}h")
        };
    }

    fn export_value(&self) -> Option<CellValue<'_>> {
        Some(CellValue::Int(self.0.min(i64::MAX as u64) as i64))
    }

    fn to_owned_value(&self) -> OwnedValue {
        OwnedValue::custom(*self)
    }
}

/// The sum of a column of [`Bytes`] or [`ByteRate`], kept in its unit for
/// the group captions (`Aggregate::Sum` adds plain numbers only).
pub struct ByteSum;

impl ByteSum {
    /// The row's value in `column`; `None` for a blank (unknown memory, a
    /// quiet disk).
    fn of(row: &ProcRow, column: PerfColumn) -> Option<u64> {
        match column {
            PerfColumn::Memory => row.memory.map(|bytes| bytes.0),
            PerfColumn::GpuMemory => row.gpu_memory.map(|bytes| bytes.0),
            PerfColumn::DiskRead => Some(row.read.0).filter(|rate| *rate > 0),
            PerfColumn::DiskWrite => Some(row.write.0).filter(|rate| *rate > 0),
            _ => None,
        }
    }

    /// The sum in the column's unit; blank when no row had a value.
    fn value(column: PerfColumn, sum: Option<u64>) -> OwnedValue {
        match (column, sum) {
            (_, None) => OwnedValue::Empty,
            (PerfColumn::Memory | PerfColumn::GpuMemory, Some(sum)) => {
                OwnedValue::custom(Bytes(sum))
            }
            (_, Some(sum)) => OwnedValue::custom(ByteRate(sum)),
        }
    }
}

impl AggregateFn<ProcRow> for ByteSum {
    fn aggregate(
        &self,
        column: PerfColumn,
        rows: &mut dyn Iterator<Item = &ProcRow>,
    ) -> OwnedValue {
        let sum = rows
            .filter_map(|row| Self::of(row, column))
            .fold(None, |sum: Option<u64>, value| {
                Some(sum.unwrap_or(0) + value)
            });
        Self::value(column, sum)
    }

    fn combine(&self, parts: &[&OwnedValue]) -> Option<OwnedValue> {
        // Every part of one column has the same unit; a blank part adds
        // nothing.
        let (mut bytes, mut rate) = (None::<u64>, None::<u64>);
        for part in parts {
            if let OwnedValue::Custom(value) = part {
                if let Some(value) = value.downcast_ref::<Bytes>() {
                    bytes = Some(bytes.unwrap_or(0) + value.0);
                } else if let Some(value) = value.downcast_ref::<ByteRate>() {
                    rate = Some(rate.unwrap_or(0) + value.0);
                }
            }
        }
        Some(match (bytes, rate) {
            (Some(sum), _) => OwnedValue::custom(Bytes(sum)),
            (None, Some(sum)) => OwnedValue::custom(ByteRate(sum)),
            (None, None) => OwnedValue::Empty,
        })
    }

    fn name(&self) -> &str {
        "Sum"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &dyn GridValue) -> String {
        let mut out = String::new();
        value.write_text(&mut out);
        out
    }

    #[test]
    fn test_bytes_read_in_binary_units_and_sort_as_numbers() {
        assert_eq!(text(&Bytes(0)), "0 B");
        assert_eq!(text(&Bytes(1023)), "1023 B");
        assert_eq!(text(&Bytes(1024)), "1.0 KiB");
        assert_eq!(text(&Bytes(1536)), "1.5 KiB");
        assert_eq!(text(&Bytes(5 * 1024 * 1024)), "5.0 MiB");
        assert_eq!(text(&Bytes(3 << 30)), "3.0 GiB");
        assert_eq!(text(&ByteRate(2048)), "2.0 KiB/s");
        // 900 MiB sorts below 1 GiB, though its text is longer.
        let (small, large) = (Bytes(900 << 20), Bytes(1 << 30));
        assert!(
            CellValue::Custom(&small)
                .cmp_total(&CellValue::Custom(&large))
                .is_lt()
        );
        assert_eq!(large.export_value(), Some(CellValue::Int(1 << 30)));
    }

    #[test]
    fn test_uptime_reads_in_two_units() {
        assert_eq!(text(&Uptime(42)), "42s");
        assert_eq!(text(&Uptime(12 * 60 + 5)), "12m 05s");
        assert_eq!(text(&Uptime(3 * 3_600 + 7 * 60)), "3h 07m");
        assert_eq!(text(&Uptime(4 * 86_400 + 2 * 3_600)), "4d 02h");
        assert!(
            CellValue::Custom(&Uptime(59))
                .cmp_total(&CellValue::Custom(&Uptime(61)))
                .is_lt()
        );
    }

    fn process(pid: u32, memory: u64, read: u64) -> ProcRow {
        ProcRow {
            key: RowKey::Process { pid, start: 1 },
            parent: None,
            name: "launchd".into(),
            app: "launchd".into(),
            user: None,
            cpu: Some(0.),
            gpu: None,
            gpu_memory: None,
            memory: Some(Bytes(memory)),
            read: ByteRate(read),
            write: ByteRate(0),
            threads: None,
            uptime: None,
            notable: ColumnSet::EMPTY,
        }
    }

    #[test]
    fn test_byte_sum_keeps_the_unit() {
        let rows = [process(1, 1024, 0), process(2, 2048, 512)];
        let memory = ByteSum.aggregate(PerfColumn::Memory, &mut rows.iter());
        let read = ByteSum.aggregate(PerfColumn::DiskRead, &mut rows.iter());
        let write = ByteSum.aggregate(PerfColumn::DiskWrite, &mut rows.iter());
        assert_eq!(memory, OwnedValue::custom(Bytes(3072)));
        assert_eq!(read, OwnedValue::custom(ByteRate(512)));
        assert_eq!(write, OwnedValue::Empty);
        let combined = ByteSum.combine(&[&memory, &memory]);
        assert_eq!(combined, Some(OwnedValue::custom(Bytes(6144))));
        assert_eq!(
            ByteSum.combine(&[&read, &write]),
            Some(OwnedValue::custom(ByteRate(512)))
        );
        // No row with a value: blank, not 0 B.
        let gpu = ByteSum.aggregate(PerfColumn::GpuMemory, &mut rows.iter());
        assert_eq!(gpu, OwnedValue::Empty);
        assert_eq!(ByteSum.combine(&[&gpu, &gpu]), Some(OwnedValue::Empty));
    }

    #[test]
    fn test_a_quiet_disk_is_blank_and_a_machine_row_has_no_pid() {
        let row = process(7, 0, 0);
        assert_eq!(row.value(PerfColumn::DiskRead), CellValue::Empty);
        assert_eq!(row.value(PerfColumn::Pid), CellValue::Int(7));
        let machine = ProcRow {
            key: RowKey::Machine(Machine::Cpu),
            ..row
        };
        assert!(machine.is_machine());
        assert_eq!(machine.value(PerfColumn::Pid), CellValue::Empty);
    }
}
