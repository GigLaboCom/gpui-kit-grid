//! The sampler: on a thread of its own, every interval (1 s by default,
//! `PERF_INTERVAL_MS`), it reads the processes (`sysinfo`, `task.rs`) and
//! the GPU (a [`GpuSampler`], `gpu.rs`), and writes one batch to the grid
//! store: an upsert per process, a remove per process gone (it fades out),
//! and the machine's rows pinned on top. The UI thread does nothing that
//! grows with the number of processes; it reads the status and the history
//! for the chart from [`Control`].
//!
//! Most values move a little every second. Each row says which of its
//! columns moved far enough since they were last shown as changed
//! ([`Flashed`], `ProcRow::notable`), and the grid tints only those; a row
//! with none is written without highlight at all (`upsert_silent`).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    path::Path,
    sync::Arc,
    time::Duration,
};

use gpui_kit_grid::{Clock, ColumnSet, GridColumns as _, GridWriter, PinnedEdge};
use parking_lot::Mutex;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, Uid, UpdateKind, Users};
use web_time::Instant;

use super::{
    gpu::{GpuDevice, GpuSample, GpuSampler},
    rows::{ByteRate, Bytes, Machine, PerfColumn, ProcRow, RowKey, Uptime, push_bytes},
    task,
};

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);
/// The shortest and longest intervals `PERF_INTERVAL_MS` may set.
pub const MIN_INTERVAL: Duration = Duration::from_millis(250);
pub const MAX_INTERVAL: Duration = Duration::from_secs(10);
/// Samples the chart keeps.
pub const HISTORY_LEN: usize = 120;

/// How far a value has to move from the one last shown as changed before
/// it is tinted again: CPU and GPU in points, memory relative with a
/// floor, disk in bytes a second, threads in threads.
const CPU_STEP: f64 = 10.;
const GPU_STEP: f64 = 10.;
const MEMORY_STEP: f64 = 0.10;
const MEMORY_MIN_STEP: u64 = 16 << 20;
const DISK_STEP: u64 = 1 << 20;
const THREADS_STEP: u32 = 8;
/// How often an unknown user id may reload the list of users.
const USERS_REFRESH: Duration = Duration::from_secs(10);

/// The values of a row last shown as changed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flashed {
    cpu: Option<f64>,
    gpu: Option<f64>,
    memory: Option<u64>,
    read: u64,
    write: u64,
    threads: Option<u32>,
}

impl Flashed {
    /// The values of `row`, as if just shown.
    pub fn of(row: &ProcRow) -> Self {
        Self {
            cpu: row.cpu,
            gpu: row.gpu,
            memory: row.memory.map(|bytes| bytes.0),
            read: row.read.0,
            write: row.write.0,
            threads: row.threads,
        }
    }

    /// The columns of `row` that moved far enough from these values; they
    /// become the values shown. Uptime is never one: it moves every second
    /// by definition.
    pub fn notable(&mut self, row: &ProcRow) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        let mut mark = |column: PerfColumn| columns.insert(column.ix());
        if moved_f64(self.cpu, row.cpu, CPU_STEP) {
            self.cpu = row.cpu;
            mark(PerfColumn::Cpu);
        }
        if moved_f64(self.gpu, row.gpu, GPU_STEP) {
            self.gpu = row.gpu;
            mark(PerfColumn::Gpu);
        }
        let memory = row.memory.map(|bytes| bytes.0);
        let memory_moved = match (self.memory, memory) {
            (Some(before), Some(now)) => {
                let step = ((before as f64 * MEMORY_STEP) as u64).max(MEMORY_MIN_STEP);
                before.abs_diff(now) >= step
            }
            (before, now) => before.is_some() != now.is_some(),
        };
        if memory_moved {
            self.memory = memory;
            mark(PerfColumn::Memory);
        }
        if moved_rate(self.read, row.read.0) {
            self.read = row.read.0;
            mark(PerfColumn::DiskRead);
        }
        if moved_rate(self.write, row.write.0) {
            self.write = row.write.0;
            mark(PerfColumn::DiskWrite);
        }
        let threads_moved = match (self.threads, row.threads) {
            (Some(before), Some(now)) => before.abs_diff(now) >= THREADS_STEP,
            (before, now) => before.is_some() != now.is_some(),
        };
        if threads_moved {
            self.threads = row.threads;
            mark(PerfColumn::Threads);
        }
        columns
    }
}

fn moved_f64(before: Option<f64>, now: Option<f64>, step: f64) -> bool {
    match (before, now) {
        (Some(before), Some(now)) => (before - now).abs() >= step,
        (before, now) => before.is_some() != now.is_some(),
    }
}

/// A disk rate moved: by a step, or between quiet and busy.
fn moved_rate(before: u64, now: u64) -> bool {
    before.abs_diff(now) >= DISK_STEP
        || ((before == 0) != (now == 0) && before.max(now) >= 64 << 10)
}

/// The application bundle `exe` is in, the outermost: `Safari` for
/// `/Applications/Safari.app/Contents/MacOS/Safari` and for its helpers
/// inside it.
pub fn bundle_name(exe: &Path) -> Option<&str> {
    exe.components()
        .find_map(|part| part.as_os_str().to_str()?.strip_suffix(".app"))
        .filter(|name| !name.is_empty())
}

/// One sample of the history: the machine's CPU and GPU, %.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistoryPoint {
    pub sample: u64,
    pub cpu: f64,
    pub gpu: Option<f64>,
}

/// What the status bar shows, as of the last sample.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub samples: u64,
    pub processes: usize,
    /// How long the last sample took on the sampler's thread.
    pub sample_time: Duration,
    pub interval: Duration,
    pub gpu_source: Arc<str>,
    pub gpu_error: Option<Arc<str>>,
}

/// Shared between the sampler's thread and the UI: the status and the
/// history, each behind its own short lock taken once per sample.
#[derive(Default)]
pub struct Control {
    status: Mutex<Status>,
    history: Mutex<(u64, VecDeque<HistoryPoint>)>,
}

impl Control {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// The history's version, which changes with every sample.
    pub fn history_version(&self) -> u64 {
        self.history.lock().0
    }

    pub fn history(&self) -> (u64, Vec<HistoryPoint>) {
        let history = self.history.lock();
        (history.0, history.1.iter().copied().collect())
    }

    fn push(&self, point: HistoryPoint) {
        let mut history = self.history.lock();
        history.0 += 1;
        if history.1.len() == HISTORY_LEN {
            history.1.pop_front();
        }
        history.1.push_back(point);
    }
}

/// What the sampler keeps of a process between samples.
struct Known {
    name: Arc<str>,
    app: Arc<str>,
    flashed: Flashed,
}

/// Who a process is: its start time (Unix seconds, 0 if unknown), its
/// parent's pid and its user.
struct Who {
    start: u64,
    parent: Option<u32>,
    uid: Option<Uid>,
}

impl Who {
    fn of(pid: u32, process: &sysinfo::Process) -> Self {
        match task::identity(pid) {
            Some(identity) => Self {
                start: identity.start,
                parent: identity.parent,
                uid: Uid::try_from(identity.uid as usize).ok(),
            },
            None => Self {
                start: process.start_time(),
                parent: process.parent().map(|parent| parent.as_u32()),
                uid: process.user_id().cloned(),
            },
        }
    }
}

/// Reads the machine; owned by the sampler's thread.
pub struct Sampler {
    system: System,
    users: Users,
    user_names: HashMap<Uid, Arc<str>>,
    users_loaded: Instant,
    gpu: Box<dyn GpuSampler>,
    clock: Arc<dyn Clock>,
    known: HashMap<RowKey, Known>,
    machine: HashMap<Machine, Flashed>,
    last: Instant,
    samples: u64,
}

impl Sampler {
    /// A sampler with the CPU counters primed: CPU use is a delta, so
    /// this reads them once and waits `sysinfo`'s shortest interval.
    pub fn new(gpu: Box<dyn GpuSampler>, clock: Arc<dyn Clock>) -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let now = clock.now();
        Self {
            system,
            users: Users::new_with_refreshed_list(),
            user_names: HashMap::new(),
            users_loaded: now,
            gpu,
            clock,
            known: HashMap::new(),
            machine: HashMap::new(),
            last: now,
            samples: 0,
        }
    }

    /// Reads everything once and writes it to `writer` in one batch: the
    /// first sample replaces the store's rows (no flashes), later ones
    /// upsert and remove.
    pub fn step(&mut self, writer: &GridWriter<ProcRow>, control: &Control) {
        let started = Instant::now();
        let now = self.clock.now();
        let interval = now
            .saturating_duration_since(self.last)
            .max(Duration::from_millis(1));
        self.last = now;
        let seconds = interval.as_secs_f64();

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
        let gpu = self.gpu.sample(now);

        let first = self.samples == 0;
        // Who each process is: the kernel table where it says more than
        // `sysinfo` (another user's process on macOS), else `sysinfo`.
        let who: HashMap<u32, Who> = self
            .system
            .processes()
            .iter()
            .filter(|(_, process)| process.thread_kind().is_none())
            .map(|(pid, process)| (pid.as_u32(), Who::of(pid.as_u32(), process)))
            .collect();
        let mut rows = Vec::with_capacity(who.len());
        let mut seen = HashSet::with_capacity(who.len());
        for (pid, process) in self.system.processes() {
            let pid = pid.as_u32();
            // Linux lists threads as processes too.
            let Some(Who { start, parent, uid }) = who.get(&pid) else {
                continue;
            };
            let (start, parent) = (*start, *parent);
            let key = RowKey::Process { pid, start };
            let parent = parent.and_then(|parent| {
                let start = who.get(&parent)?.start;
                Some(RowKey::Process { pid: parent, start })
            });
            let user = uid.as_ref().and_then(|uid| {
                user_name(
                    uid,
                    &mut self.users,
                    &mut self.user_names,
                    &mut self.users_loaded,
                    now,
                )
            });
            // `sysinfo`'s own clock: its start time plus its run time.
            let unix_now = process.start_time() + process.run_time();
            let uptime = (start > 0).then(|| Uptime(unix_now.saturating_sub(start)));
            let (name, app) = match self.known.get(&key) {
                Some(known) => (known.name.clone(), known.app.clone()),
                None => {
                    let name: Arc<str> = process.name().to_string_lossy().into();
                    let app = process
                        .exe()
                        .and_then(bundle_name)
                        .map_or_else(|| name.clone(), Arc::from);
                    (name, app)
                }
            };
            let info = task::task_info(pid, process);
            let disk = process.disk_usage();
            let memory = info
                .footprint
                .or(Some(process.memory()))
                .filter(|bytes| info.readable && *bytes > 0);
            let mut row = ProcRow {
                key,
                parent,
                name,
                app,
                user,
                cpu: info.readable.then(|| f64::from(process.cpu_usage())),
                gpu: gpu.processes.get(&pid).copied(),
                gpu_memory: gpu.memory.get(&pid).copied().map(Bytes),
                memory: memory.map(Bytes),
                read: ByteRate((disk.read_bytes as f64 / seconds) as u64),
                write: ByteRate((disk.written_bytes as f64 / seconds) as u64),
                threads: info.threads,
                uptime,
                notable: ColumnSet::EMPTY,
            };
            seen.insert(key);
            let inserted = match self.known.get_mut(&key) {
                Some(known) => {
                    row.notable = known.flashed.notable(&row);
                    false
                }
                None => {
                    self.known.insert(
                        key,
                        Known {
                            name: row.name.clone(),
                            app: row.app.clone(),
                            flashed: Flashed::of(&row),
                        },
                    );
                    true
                }
            };
            rows.push((Arc::new(row), inserted));
        }
        let gone: Vec<RowKey> = self
            .known
            .keys()
            .filter(|key| !seen.contains(key))
            .copied()
            .collect();
        for key in &gone {
            self.known.remove(key);
        }
        let pinned = self.machine_rows(&gpu, &rows);
        let processes = rows.len();
        if first {
            writer.batch(|batch| {
                batch.replace_all(rows.into_iter().map(|(row, _)| row));
                batch.set_pinned_rows(PinnedEdge::Top, pinned);
            });
        } else {
            writer.batch(|batch| {
                for (row, inserted) in rows {
                    if inserted || !row.notable.is_empty() {
                        batch.upsert(row);
                    } else {
                        batch.upsert_silent(row);
                    }
                }
                for key in gone {
                    batch.remove(key);
                }
                batch.set_pinned_rows(PinnedEdge::Top, pinned);
            });
        }

        self.samples += 1;
        control.push(HistoryPoint {
            sample: self.samples,
            cpu: f64::from(self.system.global_cpu_usage()),
            gpu: gpu.device.as_ref().and_then(|device| device.utilization),
        });
        *control.status.lock() = Status {
            samples: self.samples,
            processes,
            sample_time: started.elapsed(),
            interval,
            gpu_source: self.gpu.source().into(),
            gpu_error: self.gpu.last_error().map(Arc::from),
        };
    }

    /// The machine's rows: CPU, memory, GPU and GPU memory (the last two
    /// as one "no GPU source" row without a source).
    fn machine_rows(
        &mut self,
        gpu: &GpuSample,
        rows: &[(Arc<ProcRow>, bool)],
    ) -> Vec<Arc<ProcRow>> {
        let system = &self.system;
        let threads: u32 = rows.iter().filter_map(|(row, _)| row.threads).sum();
        let (mut name, mut detail) = (String::new(), String::new());
        let mut machine = Vec::with_capacity(4);
        let _ = write!(detail, "{} cores; % of all", system.cpus().len());
        name.push_str("CPU");
        machine.push(ProcRow {
            cpu: Some(f64::from(system.global_cpu_usage())),
            threads: (threads > 0).then_some(threads),
            uptime: Some(Uptime(System::uptime())),
            ..machine_row(Machine::Cpu, &mut name, &mut detail)
        });
        name.push_str("Memory used of ");
        push_bytes(system.total_memory(), &mut name);
        if let Some(pressure) = task::memory_pressure() {
            pressure.write_label(&mut detail);
        }
        machine.push(ProcRow {
            memory: Some(Bytes(system.used_memory())),
            ..machine_row(Machine::Memory, &mut name, &mut detail)
        });
        match &gpu.device {
            Some(device) => {
                name.push_str("GPU");
                if let Some(model) = &device.model {
                    let _ = write!(name, " · {model}");
                }
                gpu_detail(device, &mut detail);
                machine.push(ProcRow {
                    gpu: device.utilization,
                    ..machine_row(Machine::Gpu, &mut name, &mut detail)
                });
                name.push_str("GPU memory in use");
                if let Some(allocated) = device.memory_allocated {
                    push_bytes(allocated, &mut detail);
                    detail.push_str(" allocated");
                } else if let Some(total) = device.memory_total {
                    detail.push_str("of ");
                    push_bytes(total, &mut detail);
                    detail.push_str(" VRAM");
                }
                machine.push(ProcRow {
                    memory: device.memory_in_use.map(Bytes),
                    ..machine_row(Machine::GpuMemory, &mut name, &mut detail)
                });
            }
            None => {
                name.push_str("GPU");
                let _ = write!(detail, "source: {}", self.gpu.source());
                if self.gpu.last_error().is_some() {
                    detail.push_str(" (unavailable)");
                }
                machine.push(machine_row(Machine::Gpu, &mut name, &mut detail));
            }
        }
        machine
            .into_iter()
            .map(|mut row| {
                let RowKey::Machine(which) = row.key else {
                    unreachable!("machine rows only")
                };
                match self.machine.get_mut(&which) {
                    Some(flashed) => row.notable = flashed.notable(&row),
                    None => {
                        self.machine.insert(which, Flashed::of(&row));
                    }
                }
                Arc::new(row)
            })
            .collect()
    }
}

/// A machine row named by `name`'s text, with `detail`'s in the App
/// column; it takes both.
fn machine_row(machine: Machine, name: &mut String, detail: &mut String) -> ProcRow {
    let name: Arc<str> = std::mem::take(name).into();
    ProcRow {
        key: RowKey::Machine(machine),
        parent: None,
        app: std::mem::take(detail).into(),
        name,
        user: None,
        cpu: None,
        gpu: None,
        gpu_memory: None,
        memory: None,
        read: ByteRate(0),
        write: ByteRate(0),
        threads: None,
        uptime: None,
        notable: ColumnSet::EMPTY,
    }
}

/// `renderer 21 % · tiler 9 %` (macOS), `busiest engine gfx` (Linux).
fn gpu_detail(device: &GpuDevice, out: &mut String) {
    if let Some(renderer) = device.renderer {
        let _ = write!(out, "renderer {renderer:.0} %");
    }
    if let Some(tiler) = device.tiler {
        let _ = write!(out, " · tiler {tiler:.0} %");
    }
    if let Some(engine) = &device.engine {
        let _ = write!(out, "busiest engine {engine}");
    }
}

/// What a sample reads of each process: CPU, memory, disk, the executable
/// once (for the app's name) and the user once. Never the threads, which
/// `sysinfo` would list as processes of their own (`task.rs` counts them).
/// Never the command line: it is personal data, and not shown.
fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_user(UpdateKind::OnlyIfNotSet)
}

/// The name of user `uid`, from the cache, else from the list of users,
/// which an unknown id reloads at most every `USERS_REFRESH`.
fn user_name(
    uid: &Uid,
    users: &mut Users,
    names: &mut HashMap<Uid, Arc<str>>,
    loaded: &mut Instant,
    now: Instant,
) -> Option<Arc<str>> {
    if let Some(name) = names.get(uid) {
        return Some(name.clone());
    }
    let mut user = users.get_user_by_id(uid);
    if user.is_none() && now.saturating_duration_since(*loaded) >= USERS_REFRESH {
        users.refresh();
        *loaded = now;
        user = users.get_user_by_id(uid);
    }
    let name: Arc<str> = user?.name().into();
    names.insert(uid.clone(), name.clone());
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cpu: f64, memory: u64, read: u64, threads: u32) -> ProcRow {
        ProcRow {
            cpu: Some(cpu),
            memory: Some(Bytes(memory)),
            read: ByteRate(read),
            threads: Some(threads),
            uptime: Some(Uptime(10)),
            ..machine_row(Machine::Cpu, &mut "launchd".to_owned(), &mut String::new())
        }
    }

    #[test]
    fn test_small_moves_are_not_notable_and_add_up() {
        let mut flashed = Flashed::of(&row(5., 100 << 20, 0, 10));
        // A few points of CPU, a few MiB, a few KiB/s, a thread: nothing.
        assert!(flashed.notable(&row(9., 104 << 20, 8 << 10, 11)).is_empty());
        // Against the value last shown, not the last sample: 5 → 15 is.
        let moved = flashed.notable(&row(15., 104 << 20, 0, 11));
        assert!(moved.contains(PerfColumn::Cpu.ix()));
        assert_eq!(moved.len(), 1);
        // And then 15 is the value to move from.
        assert!(flashed.notable(&row(20., 100 << 20, 0, 10)).is_empty());
        // Memory moves by 10 %, at least 16 MiB; disk by 1 MiB/s or from
        // quiet to busy; threads by 8.
        let moved = flashed.notable(&row(20., 130 << 20, 2 << 20, 18));
        for column in [
            PerfColumn::Memory,
            PerfColumn::DiskRead,
            PerfColumn::Threads,
        ] {
            assert!(moved.contains(column.ix()), "{column:?}");
        }
        assert!(!moved.contains(PerfColumn::Uptime.ix()));
    }

    #[test]
    fn test_a_value_that_appears_or_goes_is_notable() {
        let mut flashed = Flashed::of(&row(1., 1 << 20, 0, 4));
        let without_gpu = row(1., 1 << 20, 0, 4);
        let with_gpu = ProcRow {
            gpu: Some(0.5),
            ..row(1., 1 << 20, 0, 4)
        };
        assert!(flashed.notable(&with_gpu).contains(PerfColumn::Gpu.ix()));
        assert!(flashed.notable(&without_gpu).contains(PerfColumn::Gpu.ix()));
    }

    #[test]
    fn test_bundle_name_is_the_outermost_app() {
        let path = |text: &str| Path::new(text).to_owned();
        assert_eq!(
            bundle_name(&path("/System/Applications/Mail.app/Contents/MacOS/Mail")),
            Some("Mail")
        );
        assert_eq!(
            bundle_name(&path(
                "/Applications/Safari.app/Contents/XPCServices/Web Helper.app/Contents/MacOS/Web Helper"
            )),
            Some("Safari")
        );
        assert_eq!(bundle_name(&path("/usr/libexec/logd")), None);
        assert_eq!(bundle_name(&path("/tmp/.app/x")), None);
    }

    #[test]
    fn test_history_keeps_the_last_samples() {
        let control = Control::new();
        for sample in 0..HISTORY_LEN as u64 + 5 {
            control.push(HistoryPoint {
                sample,
                cpu: 1.,
                gpu: None,
            });
        }
        let (version, points) = control.history();
        assert_eq!(version, HISTORY_LEN as u64 + 5);
        assert_eq!(points.len(), HISTORY_LEN);
        assert_eq!(points[0].sample, 5);
    }
}
