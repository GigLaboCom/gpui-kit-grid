//! What `sysinfo` does not give on macOS, from libproc and sysctl without
//! root: a process's threads and its memory footprint (the number Activity
//! Monitor shows as Memory), and the memory pressure; and for another
//! user's process, which libproc does not answer without root, its start
//! time, parent and user from `kern.proc.pid` ([`identity`]), so it keeps
//! its key and its place in the tree. Its CPU, memory and threads stay
//! blank: only root (or Activity Monitor's privileged helper) reads them.
//!
//! On Linux `sysinfo` reads `/proc` for the start time, parent, user, CPU,
//! resident memory and disk I/O of every process; this adds the threads
//! (`Threads:` of `/proc/<pid>/status`, without listing every thread as
//! `sysinfo`'s `tasks` does) and the memory pressure from PSI
//! (`/proc/pressure/memory`, kernel 4.20 and later). Another user's
//! `/proc/<pid>/io` needs root: its disk columns stay blank, as a quiet
//! disk's do. Windows has no footprint, threads or pressure here yet.

/// A process's threads and memory footprint, where the platform gives them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskInfo {
    pub threads: Option<u32>,
    pub footprint: Option<u64>,
    /// The process's own counters can be read: false for another user's
    /// process on macOS without root, whose CPU `sysinfo` gives as 0.
    pub readable: bool,
}

/// Who a process is, from the kernel's process table, which any user may
/// read: its start time (Unix seconds), parent pid and effective user id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub start: u64,
    pub parent: Option<u32>,
    pub uid: u32,
}

/// The size of `struct kinfo_proc` on 64-bit macOS, and where the fields
/// read sit in it (`<sys/sysctl.h>`, `<sys/proc.h>`): `kp_proc.p_starttime`
/// first (a `timeval`, its seconds an `i64`), `kp_eproc.e_ucred.cr_uid`
/// and `kp_eproc.e_ppid`. The `libc` crate does not declare the struct.
/// macOS reads them; their test runs everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KINFO_PROC_SIZE: usize = 648;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const START_OFFSET: usize = 0;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const UID_OFFSET: usize = 420;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PPID_OFFSET: usize = 560;

/// Reads an [`Identity`] out of a whole `kinfo_proc`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_kinfo_proc(bytes: &[u8]) -> Option<Identity> {
    if bytes.len() != KINFO_PROC_SIZE {
        return None;
    }
    let field =
        |offset: usize| -> Option<[u8; 4]> { bytes.get(offset..offset + 4)?.try_into().ok() };
    let start: [u8; 8] = bytes.get(START_OFFSET..START_OFFSET + 8)?.try_into().ok()?;
    let start = i64::from_ne_bytes(start);
    let uid = u32::from_ne_bytes(field(UID_OFFSET)?);
    let parent = i32::from_ne_bytes(field(PPID_OFFSET)?);
    Some(Identity {
        start: u64::try_from(start).ok()?,
        parent: u32::try_from(parent).ok().filter(|parent| *parent > 0),
        uid,
    })
}

#[cfg(target_os = "macos")]
pub fn identity(pid: u32) -> Option<Identity> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as libc::c_int,
    ];
    let mut buffer = [0u8; KINFO_PROC_SIZE];
    let mut size = buffer.len();
    // SAFETY: `mib` names a four-level sysctl; `buffer` is writable for
    // `size` bytes, and the call writes at most that many and says how
    // many.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    // A pid that is gone writes nothing, which does not parse.
    if status != 0 {
        return None;
    }
    parse_kinfo_proc(&buffer[..size])
}

#[cfg(not(target_os = "macos"))]
pub fn identity(_: u32) -> Option<Identity> {
    None
}

#[cfg(target_os = "macos")]
pub fn task_info(pid: u32, _: &sysinfo::Process) -> TaskInfo {
    use std::mem::{MaybeUninit, size_of};

    let pid = pid as libc::c_int;
    let mut task = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `task` is a writable `proc_taskinfo` of `size` bytes; the
    // call writes at most that many and returns how many it wrote.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: a full write initialised it; zeroed memory is a valid
    // `proc_taskinfo` anyway (plain integers).
    let threads = (written == size).then(|| unsafe { task.assume_init() }.pti_threadnum as u32);

    let mut usage = MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `usage` is a writable `rusage_info_v2`, the struct the
    // `RUSAGE_INFO_V2` flavour fills.
    let status =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, usage.as_mut_ptr().cast()) };
    // SAFETY: filled on success; zeroed plain integers otherwise.
    let footprint = (status == 0).then(|| unsafe { usage.assume_init() }.ri_phys_footprint);
    TaskInfo {
        threads,
        footprint,
        readable: threads.is_some(),
    }
}

#[cfg(target_os = "linux")]
pub fn task_info(pid: u32, _: &sysinfo::Process) -> TaskInfo {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok();
    TaskInfo {
        threads: status.as_deref().and_then(status_threads),
        footprint: None,
        // `stat` and `status` are readable for every process.
        readable: true,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn task_info(_: u32, _: &sysinfo::Process) -> TaskInfo {
    TaskInfo {
        threads: None,
        footprint: None,
        readable: true,
    }
}

/// The `Threads:` line of `/proc/<pid>/status`. Linux reads it; its test
/// runs everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn status_threads(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|threads| threads.trim().parse().ok())
}

/// How hard the system is pressed for memory: macOS's level, or Linux's
/// pressure stall information, the share of the last 10 s in which some
/// or all tasks waited for memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MemoryPressure {
    // macOS only, and the next one Linux only; their parsers' tests run
    // everywhere.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Normal,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Warning,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Critical,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Stall { some: f64, full: f64 },
}

impl MemoryPressure {
    /// `pressure normal`, `stall 1.2 % some, 0.0 % full`.
    pub fn write_label(self, out: &mut String) {
        use std::fmt::Write as _;
        let _ = match self {
            Self::Normal => write!(out, "pressure normal"),
            Self::Warning => write!(out, "pressure warning"),
            Self::Critical => write!(out, "pressure critical"),
            Self::Stall { some, full } => write!(out, "stall {some:.1} % some, {full:.1} % full"),
        };
    }

    /// The kernel's `kern.memorystatus_vm_pressure_level`: 1, 2 or 4.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn from_level(level: i32) -> Option<Self> {
        match level {
            1 => Some(Self::Normal),
            2 => Some(Self::Warning),
            4 => Some(Self::Critical),
            _ => None,
        }
    }

    /// `/proc/pressure/memory`: the `avg10` of its `some` and `full` lines.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn from_psi(text: &str) -> Option<Self> {
        let avg10 = |kind: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(kind)?.strip_prefix(' '))?
                .split_whitespace()
                .find_map(|field| field.strip_prefix("avg10="))?
                .parse::<f64>()
                .ok()
        };
        Some(Self::Stall {
            some: avg10("some")?,
            full: avg10("full").unwrap_or(0.),
        })
    }
}

#[cfg(target_os = "macos")]
pub fn memory_pressure() -> Option<MemoryPressure> {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated; `level` is a writable `c_int` of
    // `size` bytes, the type this sysctl has.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&raw mut level).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0)
        .then_some(level)
        .and_then(MemoryPressure::from_level)
}

#[cfg(target_os = "linux")]
pub fn memory_pressure() -> Option<MemoryPressure> {
    MemoryPressure::from_psi(&std::fs::read_to_string("/proc/pressure/memory").ok()?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn memory_pressure() -> Option<MemoryPressure> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_kinfo_proc_reads_start_parent_and_user() {
        let mut bytes = [0u8; KINFO_PROC_SIZE];
        bytes[START_OFFSET..START_OFFSET + 8].copy_from_slice(&1_700_000_000i64.to_ne_bytes());
        bytes[UID_OFFSET..UID_OFFSET + 4].copy_from_slice(&88u32.to_ne_bytes());
        bytes[PPID_OFFSET..PPID_OFFSET + 4].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(
            parse_kinfo_proc(&bytes),
            Some(Identity {
                start: 1_700_000_000,
                parent: Some(1),
                uid: 88,
            })
        );
        // The first process has no parent; a short read is nothing.
        bytes[PPID_OFFSET..PPID_OFFSET + 4].copy_from_slice(&0i32.to_ne_bytes());
        assert_eq!(parse_kinfo_proc(&bytes).and_then(|id| id.parent), None);
        assert_eq!(parse_kinfo_proc(&bytes[..100]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_identity_of_this_process() {
        let identity = identity(std::process::id()).expect("this process is in the table");
        assert!(identity.start > 0);
        assert!(identity.parent.is_some());
    }

    #[test]
    fn test_psi_and_status_threads() {
        let psi = "some avg10=1.26 avg60=0.50 avg300=0.10 total=123456\n\
                   full avg10=0.40 avg60=0.10 avg300=0.00 total=2345\n";
        assert_eq!(
            MemoryPressure::from_psi(psi),
            Some(MemoryPressure::Stall {
                some: 1.26,
                full: 0.4
            })
        );
        assert_eq!(MemoryPressure::from_psi(""), None);
        let mut label = String::new();
        MemoryPressure::from_psi(psi)
            .expect("parsed")
            .write_label(&mut label);
        assert_eq!(label, "stall 1.3 % some, 0.4 % full");
        let status = "Name:\tbash\nUid:\t1000\t1000\t1000\t1000\nThreads:\t7\nVmRSS:\t4 kB\n";
        assert_eq!(status_threads(status), Some(7));
        assert_eq!(status_threads("Name:\tbash\n"), None);
    }

    #[test]
    fn test_pressure_levels() {
        assert_eq!(MemoryPressure::from_level(1), Some(MemoryPressure::Normal));
        assert_eq!(MemoryPressure::from_level(2), Some(MemoryPressure::Warning));
        assert_eq!(
            MemoryPressure::from_level(4),
            Some(MemoryPressure::Critical)
        );
        assert_eq!(MemoryPressure::from_level(0), None);
    }
}
