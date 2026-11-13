//! Linux: the GPU from the kernel's DRM interfaces, for any DRM driver
//! (amdgpu, i915, xe, nouveau, msm, panfrost, panthor, v3d, virtio-gpu…),
//! without root for the user's own processes.
//!
//! - **Per process**: each open DRM file has an `fdinfo`
//!   (`/proc/<pid>/fdinfo/<fd>`, the kernel's `drm-usage-stats`) with its
//!   driver, its card (`drm-pdev`), its client (`drm-client-id`), the time
//!   each engine spent on its work (`drm-engine-<engine>: <ns> ns`, or
//!   `drm-cycles-<engine>` over `drm-total-cycles-<engine>` on xe and
//!   panthor) and its memory (`drm-resident-<region>`, else
//!   `drm-memory-<region>`). One client may be open through several files
//!   (a `dup`, a fork): clients are counted once, by card and client id,
//!   for the lowest pid that holds them. A process's share of an engine is
//!   the delta of its clients' counters over the interval, divided by the
//!   engine's capacity (`drm-engine-capacity-<engine>`); its GPU % is its
//!   **busiest engine**, as `nvtop` and `intel_gpu_top` show, since the
//!   engines run side by side. A client seen for the first time has no
//!   delta yet, and one that went away (its process exited) is dropped.
//!   Another user's files need root (ptrace access): their columns stay
//!   blank.
//! - **The device**: each card in `/sys/class/drm/card<n>/device`, named
//!   from its PCI vendor and device (`uevent`'s `PCI_ID`) and its driver;
//!   its utilization from `gpu_busy_percent` where the driver has it
//!   (amdgpu), else the busiest engine over every client; its memory from
//!   `mem_info_vram_used` / `mem_info_vram_total` (amdgpu), else the sum of
//!   the clients' memory.
//!
//! Without a card or a client, as in a container drawing with llvmpipe,
//! the source is "none found" and the GPU columns stay blank; a file that
//! cannot be read is skipped. Nothing here fails.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use web_time::Instant;

use super::gpu::{GpuDevice, GpuSample, GpuSampler, GpuSamplerFactory};

/// One engine's counter in one `fdinfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineCounter {
    /// Busy time, ns (`drm-engine-<engine>`).
    pub nanos: Option<u64>,
    /// Busy cycles and all cycles (`drm-cycles-`, `drm-total-cycles-`).
    pub cycles: Option<u64>,
    pub total_cycles: Option<u64>,
    /// How many of the engine run at once (`drm-engine-capacity-`), 1 when
    /// not said.
    pub capacity: u32,
}

/// What one DRM file's `fdinfo` says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FdInfo {
    pub driver: String,
    /// The card's PCI address, `0000:03:00.0`; none on some platform GPUs.
    pub pdev: Option<String>,
    pub client_id: u64,
    pub engines: BTreeMap<String, EngineCounter>,
    /// Bytes: the resident sum when the driver gives one, else the memory
    /// sum; `None` without either.
    pub memory: Option<u64>,
}

/// A value with an optional unit: `123 ns`, `1024 KiB`, `5 MiB`, `42`.
fn amount(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let value: u64 = parts.next()?.parse().ok()?;
    let scale = match parts.next() {
        None | Some("ns") | Some("B") => 1,
        Some("KiB") | Some("kB") => 1 << 10,
        Some("MiB") => 1 << 20,
        Some("GiB") => 1 << 30,
        Some(_) => return None,
    };
    value.checked_mul(scale)
}

/// Parses an `fdinfo`; `None` if it is not a DRM client's.
pub fn parse_fdinfo(text: &str) -> Option<FdInfo> {
    let mut info = FdInfo::default();
    let (mut driver, mut client) = (None, None);
    let (mut resident, mut memory) = (None::<u64>, None::<u64>);
    let add = |sum: &mut Option<u64>, value: u64| *sum = Some(sum.unwrap_or(0) + value);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let engine = |name: &str| name.to_owned();
        if key == "drm-driver" {
            driver = Some(value.to_owned());
        } else if key == "drm-pdev" {
            info.pdev = Some(value.to_owned());
        } else if key == "drm-client-id" {
            client = value.parse().ok();
        } else if let Some(name) = key.strip_prefix("drm-engine-capacity-") {
            if let Ok(capacity) = value.parse() {
                info.engines.entry(engine(name)).or_default().capacity = capacity;
            }
        } else if let Some(name) = key.strip_prefix("drm-engine-") {
            info.engines.entry(engine(name)).or_default().nanos = amount(value);
        } else if let Some(name) = key.strip_prefix("drm-total-cycles-") {
            info.engines.entry(engine(name)).or_default().total_cycles = amount(value);
        } else if let Some(name) = key.strip_prefix("drm-cycles-") {
            info.engines.entry(engine(name)).or_default().cycles = amount(value);
        } else if key.starts_with("drm-resident-") {
            if let Some(value) = amount(value) {
                add(&mut resident, value);
            }
        } else if key.starts_with("drm-memory-")
            && let Some(value) = amount(value)
        {
            add(&mut memory, value);
        }
    }
    info.driver = driver?;
    info.client_id = client?;
    info.memory = resident.or(memory);
    Some(info)
}

/// A DRM client: one per card and client id, however many files hold it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientKey {
    pub card: String,
    pub id: u64,
}

/// A client as last read, and the process it is counted for.
#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    pub pid: u32,
    pub driver: String,
    pub engines: BTreeMap<String, EngineCounter>,
    pub memory: Option<u64>,
}

/// The clients of `files`, `(pid, fdinfo)` pairs: each counted once, for
/// the lowest pid that holds it.
pub fn clients(files: impl IntoIterator<Item = (u32, FdInfo)>) -> HashMap<ClientKey, Client> {
    let mut files: Vec<(u32, FdInfo)> = files.into_iter().collect();
    files.sort_by_key(|(pid, _)| *pid);
    let mut clients = HashMap::new();
    for (pid, info) in files {
        let key = ClientKey {
            card: info.pdev.clone().unwrap_or_else(|| info.driver.clone()),
            id: info.client_id,
        };
        clients.entry(key).or_insert(Client {
            pid,
            driver: info.driver,
            engines: info.engines,
            memory: info.memory,
        });
    }
    clients
}

/// What two readings of the clients say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shares {
    /// Each pid's busiest engine, % (at most 100).
    pub processes: HashMap<u32, f64>,
    /// The busiest engine over every client, with its %.
    pub busiest: Option<(String, f64)>,
    /// Each pid's GPU memory now, bytes.
    pub memory: HashMap<u32, u64>,
}

/// An engine's busy share between two counters `interval` apart, 0 to 1.
fn engine_share(before: &EngineCounter, now: &EngineCounter, interval: Duration) -> Option<f64> {
    let capacity = f64::from(now.capacity.max(1));
    let share = match (before.nanos, now.nanos, before.cycles, now.cycles) {
        (Some(before), Some(now), ..) => {
            let nanos = interval.as_nanos() as f64;
            (nanos > 0.).then(|| now.saturating_sub(before) as f64 / nanos)?
        }
        (_, _, Some(cycles_before), Some(cycles_now)) => {
            let total = now.total_cycles?.saturating_sub(before.total_cycles?);
            (total > 0).then(|| cycles_now.saturating_sub(cycles_before) as f64 / total as f64)?
        }
        _ => return None,
    };
    Some((share / capacity).min(1.))
}

/// The processes' shares between `previous` and `current`, readings
/// `interval` apart: per pid its busiest engine (summed over its clients),
/// and the busiest engine over all.
pub fn shares(
    previous: &HashMap<ClientKey, Client>,
    current: &HashMap<ClientKey, Client>,
    interval: Duration,
) -> Shares {
    let mut by_pid: HashMap<(u32, &str), f64> = HashMap::new();
    let mut by_engine: BTreeMap<&str, f64> = BTreeMap::new();
    let mut memory: HashMap<u32, u64> = HashMap::new();
    for (key, client) in current {
        if let Some(bytes) = client.memory {
            *memory.entry(client.pid).or_default() += bytes;
        }
        let Some(before) = previous.get(key) else {
            continue;
        };
        for (engine, now) in &client.engines {
            let Some(share) = before
                .engines
                .get(engine)
                .and_then(|before| engine_share(before, now, interval))
            else {
                continue;
            };
            *by_pid.entry((client.pid, engine)).or_default() += share;
            *by_engine.entry(engine).or_default() += share;
        }
    }
    let mut processes: HashMap<u32, f64> = HashMap::new();
    for ((pid, _), share) in by_pid {
        let percent = (share * 100.).min(100.);
        let busiest = processes.entry(pid).or_default();
        *busiest = busiest.max(percent);
    }
    let busiest = by_engine
        .into_iter()
        .map(|(engine, share)| (engine.to_owned(), (share * 100.).min(100.)))
        .fold(
            None,
            |best: Option<(String, f64)>, (engine, percent)| match best {
                Some((_, top)) if top >= percent => best,
                _ => Some((engine, percent)),
            },
        );
    Shares {
        processes,
        busiest,
        memory,
    }
}

/// A card in sysfs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub name: String,
    pub driver: Option<String>,
    /// `1002:744C`.
    pub pci_id: Option<String>,
    pub busy: Option<f64>,
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
}

impl Card {
    /// `AMD 744C (amdgpu)`, `Intel 56A0 (i915)`, `virtio_gpu`, `card0`.
    pub fn model(&self) -> String {
        let pci = self.pci_id.as_deref().and_then(|id| id.split_once(':'));
        let named = pci.map(|(vendor, device)| {
            let vendor = match vendor.to_ascii_lowercase().as_str() {
                "1002" => "AMD".to_owned(),
                "8086" => "Intel".to_owned(),
                "10de" => "NVIDIA".to_owned(),
                "1af4" => "virtio".to_owned(),
                "15ad" => "VMware".to_owned(),
                "1234" => "QEMU".to_owned(),
                "5143" => "Qualcomm".to_owned(),
                other => format!("PCI {other}"),
            };
            format!("{vendor} {}", device.to_ascii_uppercase())
        });
        match (named, &self.driver) {
            (Some(named), Some(driver)) => format!("{named} ({driver})"),
            (Some(named), None) => named,
            (None, Some(driver)) => driver.clone(),
            (None, None) => self.name.clone(),
        }
    }
}

/// `DRIVER=` and `PCI_ID=` of a device's `uevent`.
pub fn parse_uevent(text: &str) -> (Option<String>, Option<String>) {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_owned)
    };
    (field("DRIVER"), field("PCI_ID"))
}

/// `card0`, `card1`; not the connectors (`card0-HDMI-A-1`) nor the render
/// nodes.
fn is_card(name: &str) -> bool {
    name.strip_prefix("card")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// The DRM sampler, reading `/proc` and `/sys/class/drm` (other roots for
/// a test).
pub struct DrmFdinfoSampler {
    proc_root: PathBuf,
    drm_root: PathBuf,
    previous: Option<(Instant, HashMap<ClientKey, Client>)>,
    source: &'static str,
}

impl Default for DrmFdinfoSampler {
    fn default() -> Self {
        Self::new("/proc", "/sys/class/drm")
    }
}

impl DrmFdinfoSampler {
    pub fn new(proc_root: impl Into<PathBuf>, drm_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            drm_root: drm_root.into(),
            previous: None,
            source: "DRM fdinfo",
        }
    }

    /// Every DRM file the user may read: the files of `/proc/<pid>/fd`
    /// that point into `/dev/dri/`, and their `fdinfo`.
    fn read_files(&self) -> Vec<(u32, FdInfo)> {
        let mut files = Vec::new();
        let Ok(processes) = fs::read_dir(&self.proc_root) else {
            return files;
        };
        for process in processes.flatten() {
            let Some(pid) = process
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = process.path();
            // Another user's process: not readable without root.
            let Ok(fds) = fs::read_dir(dir.join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let is_drm =
                    fs::read_link(fd.path()).is_ok_and(|target| target.starts_with("/dev/dri/"));
                if !is_drm {
                    continue;
                }
                let info = fs::read_to_string(dir.join("fdinfo").join(fd.file_name()));
                if let Some(info) = info.ok().as_deref().and_then(parse_fdinfo) {
                    files.push((pid, info));
                }
            }
        }
        files
    }

    fn read_cards(&self) -> Vec<Card> {
        let Ok(entries) = fs::read_dir(&self.drm_root) else {
            return Vec::new();
        };
        let mut cards: Vec<Card> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_owned();
                is_card(&name).then(|| read_card(&entry.path().join("device"), name))
            })
            .collect();
        cards.sort_by(|a, b| a.name.cmp(&b.name));
        cards
    }
}

fn read_card(device: &Path, name: String) -> Card {
    let read = |file: &str| fs::read_to_string(device.join(file)).ok();
    let number = |file: &str| read(file).and_then(|text| text.trim().parse::<u64>().ok());
    let (driver, pci_id) = read("uevent").as_deref().map_or((None, None), parse_uevent);
    Card {
        name,
        driver,
        pci_id,
        busy: number("gpu_busy_percent").map(|busy| busy as f64),
        vram_used: number("mem_info_vram_used"),
        vram_total: number("mem_info_vram_total"),
    }
}

/// The device of a sample: the first card with a busy counter, else the
/// first card, with what the clients add; or the clients' own driver
/// without a card in sysfs.
pub fn device(
    cards: &[Card],
    shares: &Shares,
    clients: &HashMap<ClientKey, Client>,
) -> Option<GpuDevice> {
    let card = cards
        .iter()
        .find(|card| card.busy.is_some())
        .or_else(|| cards.first());
    let model = match card {
        Some(card) => card.model(),
        None => clients.values().map(|client| client.driver.clone()).min()?,
    };
    let client_memory = (!shares.memory.is_empty()).then(|| shares.memory.values().sum());
    let busy = card.and_then(|card| card.busy);
    Some(GpuDevice {
        model: Some(model.into()),
        utilization: busy.or(shares.busiest.as_ref().map(|(_, percent)| *percent)),
        engine: busy
            .is_none()
            .then(|| {
                shares
                    .busiest
                    .as_ref()
                    .map(|(engine, _)| Arc::from(engine.as_str()))
            })
            .flatten(),
        memory_in_use: card.and_then(|card| card.vram_used).or(client_memory),
        memory_total: card.and_then(|card| card.vram_total),
        ..GpuDevice::default()
    })
}

impl GpuSampler for DrmFdinfoSampler {
    fn source(&self) -> &str {
        self.source
    }

    fn sample(&mut self, now: Instant) -> GpuSample {
        let current = clients(self.read_files());
        let cards = self.read_cards();
        let shares = match &self.previous {
            Some((then, previous)) => {
                shares(previous, &current, now.saturating_duration_since(*then))
            }
            None => shares(&HashMap::new(), &current, Duration::ZERO),
        };
        let device = device(&cards, &shares, &current);
        self.source = if device.is_some() {
            "DRM fdinfo"
        } else {
            "none found"
        };
        self.previous = Some((now, current));
        GpuSample {
            device,
            processes: shares.processes,
            memory: shares.memory,
        }
    }
}

impl GpuSamplerFactory for DrmFdinfoSampler {
    fn create(&self) -> Box<dyn GpuSampler> {
        Box::new(Self::new(self.proc_root.clone(), self.drm_root.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An amdgpu client as the kernel writes it (`drm-usage-stats`).
    const AMDGPU: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t1063\n\
        drm-driver:\tamdgpu\ndrm-client-id:\t17\ndrm-pdev:\t0000:03:00.0\n\
        pasid:\t32771\ndrm-memory-vram:\t65536 KiB\ndrm-memory-gtt:\t2048 KiB\n\
        drm-memory-cpu:\t0 KiB\ndrm-engine-gfx:\t1000000000 ns\n\
        drm-engine-compute:\t0 ns\ndrm-engine-dma:\t5000000 ns\n";

    /// An xe client: cycles over total cycles, resident memory, and a
    /// video engine of capacity 2.
    const XE: &str = "drm-driver:\txe\ndrm-client-id:\t4\ndrm-pdev:\t0000:00:02.0\n\
        drm-resident-system:\t1024 KiB\ndrm-resident-vram0:\t3 MiB\n\
        drm-cycles-rcs:\t1000\ndrm-total-cycles-rcs:\t10000\n\
        drm-cycles-vcs:\t0\ndrm-total-cycles-vcs:\t10000\ndrm-engine-capacity-vcs:\t2\n";

    fn with(text: &str, edits: &[(&str, &str)]) -> FdInfo {
        let text = edits
            .iter()
            .fold(text.to_owned(), |text, (from, to)| text.replace(from, to));
        parse_fdinfo(&text).expect("a DRM fdinfo")
    }

    #[test]
    fn test_parse_fdinfo_reads_engines_memory_and_the_client() {
        let info = parse_fdinfo(AMDGPU).expect("a DRM fdinfo");
        assert_eq!(info.driver, "amdgpu");
        assert_eq!(info.pdev.as_deref(), Some("0000:03:00.0"));
        assert_eq!(info.client_id, 17);
        assert_eq!(info.engines["gfx"].nanos, Some(1_000_000_000));
        assert_eq!(info.engines["dma"].nanos, Some(5_000_000));
        assert_eq!(info.engines["gfx"].capacity, 0);
        // No resident lines: the memory lines, summed.
        assert_eq!(info.memory, Some((65_536 + 2_048) << 10));

        let xe = parse_fdinfo(XE).expect("a DRM fdinfo");
        assert_eq!(xe.engines["rcs"].cycles, Some(1_000));
        assert_eq!(xe.engines["rcs"].total_cycles, Some(10_000));
        assert_eq!(xe.engines["vcs"].capacity, 2);
        assert_eq!(xe.memory, Some((1 << 20) + (3 << 20)));

        // A file that is not a DRM client's.
        assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t26\n"), None);
        assert_eq!(parse_fdinfo("drm-driver:\ti915\n"), None);
    }

    #[test]
    fn test_one_client_through_two_files_counts_once() {
        // The same client open twice in one process (a dup), and inherited
        // by a child: counted once, for the lowest pid.
        let files = [
            (4200, parse_fdinfo(AMDGPU).expect("fdinfo")),
            (4100, parse_fdinfo(AMDGPU).expect("fdinfo")),
            (4100, parse_fdinfo(AMDGPU).expect("fdinfo")),
            // The same client id on another card is another client.
            (4100, with(AMDGPU, &[("0000:03:00.0", "0000:04:00.0")])),
        ];
        let clients = clients(files);
        assert_eq!(clients.len(), 2);
        assert!(clients.values().all(|client| client.pid == 4100));
        let shares = shares(&HashMap::new(), &clients, Duration::from_secs(1));
        // Memory now, without a previous reading; no shares yet.
        assert_eq!(
            shares.memory.get(&4100),
            Some(&(2 * ((65_536 + 2_048) << 10)))
        );
        assert!(shares.processes.is_empty());
        assert_eq!(shares.busiest, None);
    }

    #[test]
    fn test_shares_are_the_busiest_engine_over_the_interval() {
        let before = clients([
            (10, parse_fdinfo(AMDGPU).expect("fdinfo")),
            (20, with(AMDGPU, &[("client-id:\t17", "client-id:\t18")])),
            // Exits before the next reading.
            (30, with(AMDGPU, &[("client-id:\t17", "client-id:\t19")])),
        ]);
        let after = clients([
            // gfx +250 ms, dma +50 ms over 500 ms: 50 % and 10 %.
            (
                10,
                with(
                    AMDGPU,
                    &[
                        ("gfx:\t1000000000", "gfx:\t1250000000"),
                        ("dma:\t5000000", "dma:\t55000000"),
                    ],
                ),
            ),
            // gfx +100 ms: 20 %.
            (
                20,
                with(
                    AMDGPU,
                    &[
                        ("client-id:\t17", "client-id:\t18"),
                        ("gfx:\t1000000000", "gfx:\t1100000000"),
                    ],
                ),
            ),
            // New: no share yet.
            (40, with(AMDGPU, &[("client-id:\t17", "client-id:\t20")])),
        ]);
        let shares = shares(&before, &after, Duration::from_millis(500));
        assert_eq!(shares.processes.get(&10), Some(&50.));
        assert_eq!(shares.processes.get(&20), Some(&20.));
        assert_eq!(shares.processes.get(&30), None);
        assert_eq!(shares.processes.get(&40), None);
        // gfx over every client: 50 + 20.
        assert_eq!(shares.busiest, Some(("gfx".to_owned(), 70.)));
        assert!(shares.memory.contains_key(&40));
        assert!(!shares.memory.contains_key(&30));
    }

    #[test]
    fn test_cycles_and_capacity() {
        let before = clients([(1, parse_fdinfo(XE).expect("fdinfo"))]);
        let after = clients([(
            1,
            with(
                XE,
                &[
                    ("cycles-rcs:\t1000", "cycles-rcs:\t3000"),
                    ("total-cycles-rcs:\t10000", "total-cycles-rcs:\t20000"),
                    ("cycles-vcs:\t0", "cycles-vcs:\t10000"),
                    ("total-cycles-vcs:\t10000", "total-cycles-vcs:\t20000"),
                ],
            ),
        )]);
        let shares = shares(&before, &after, Duration::from_secs(1));
        // rcs 2000 / 10000 = 20 %; vcs 10000 / 10000 over 2 engines = 50 %.
        assert_eq!(shares.processes.get(&1), Some(&50.));
        assert_eq!(shares.busiest, Some(("vcs".to_owned(), 50.)));
    }

    #[test]
    fn test_cards_are_named_and_lead_the_device() {
        let (driver, pci_id) = parse_uevent("DRIVER=amdgpu\nPCI_CLASS=30000\nPCI_ID=1002:744C\n");
        let amd = Card {
            name: "card1".into(),
            driver,
            pci_id,
            busy: Some(37.),
            vram_used: Some(1 << 30),
            vram_total: Some(8 << 30),
        };
        assert_eq!(amd.model(), "AMD 744C (amdgpu)");
        let virtio = Card {
            name: "card0".into(),
            driver: Some("virtio_gpu".into()),
            ..Card::default()
        };
        assert_eq!(virtio.model(), "virtio_gpu");
        assert_eq!(
            Card {
                name: "card2".into(),
                ..Card::default()
            }
            .model(),
            "card2"
        );
        assert!(is_card("card0") && is_card("card12"));
        assert!(!is_card("card0-HDMI-A-1") && !is_card("renderD128") && !is_card("card"));

        let none = Shares::default();
        let device = device(&[virtio.clone(), amd], &none, &HashMap::new()).expect("a card");
        assert_eq!(device.model.as_deref(), Some("AMD 744C (amdgpu)"));
        assert_eq!(device.utilization, Some(37.));
        assert_eq!(device.memory_total, Some(8 << 30));
        // A card without a busy counter: the busiest engine stands in.
        let busy = Shares {
            busiest: Some(("render".into(), 12.)),
            ..Shares::default()
        };
        let device = super::device(&[virtio], &busy, &HashMap::new()).expect("a card");
        assert_eq!(device.utilization, Some(12.));
        assert_eq!(device.engine.as_deref(), Some("render"));
        // No card and no client: no device.
        assert_eq!(super::device(&[], &none, &HashMap::new()), None);
    }

    /// A made-up `/proc` and `/sys/class/drm` on disk: pid 300 with two
    /// files of one client and a file that is not DRM, pid 301 not
    /// readable (no `fd`), and card0 with a connector beside it.
    #[cfg(unix)]
    #[test]
    fn test_the_sampler_reads_files_and_cards() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("perf-drm-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (proc_root, drm_root) = (root.join("proc"), root.join("drm"));
        let process = proc_root.join("300");
        fs::create_dir_all(process.join("fd")).expect("fd");
        fs::create_dir_all(process.join("fdinfo")).expect("fdinfo");
        fs::create_dir_all(proc_root.join("301")).expect("301");
        fs::create_dir_all(proc_root.join("self")).expect("self");
        for (fd, target) in [
            ("5", "/dev/dri/renderD128"),
            ("6", "/dev/dri/card0"),
            ("7", "/dev/null"),
        ] {
            symlink(target, process.join("fd").join(fd)).expect("symlink");
            fs::write(process.join("fdinfo").join(fd), AMDGPU).expect("fdinfo");
        }
        let device = drm_root.join("card0").join("device");
        fs::create_dir_all(&device).expect("device");
        fs::create_dir_all(drm_root.join("card0-DP-1")).expect("connector");
        fs::write(device.join("uevent"), "DRIVER=amdgpu\nPCI_ID=1002:73BF\n").expect("uevent");
        fs::write(device.join("gpu_busy_percent"), "42\n").expect("busy");
        fs::write(device.join("mem_info_vram_used"), "1073741824\n").expect("used");
        fs::write(device.join("mem_info_vram_total"), "17179869184\n").expect("total");

        let mut sampler = DrmFdinfoSampler::new(&proc_root, &drm_root);
        let started = Instant::now();
        let first = sampler.sample(started);
        assert_eq!(sampler.source(), "DRM fdinfo");
        let device = first.device.expect("card0");
        assert_eq!(device.model.as_deref(), Some("AMD 73BF (amdgpu)"));
        assert_eq!(device.utilization, Some(42.));
        assert_eq!(device.memory_in_use, Some(1 << 30));
        assert_eq!(device.memory_total, Some(16 << 30));
        // One client through two files: its memory once; no share yet.
        assert_eq!(first.memory.get(&300), Some(&((65_536 + 2_048) << 10)));
        assert!(first.processes.is_empty());

        let busier = AMDGPU.replace("gfx:\t1000000000", "gfx:\t1500000000");
        for fd in ["5", "6"] {
            fs::write(process.join("fdinfo").join(fd), &busier).expect("fdinfo");
        }
        let second = sampler.sample(started + Duration::from_secs(1));
        assert_eq!(second.processes.get(&300), Some(&50.));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_a_machine_without_drm_is_none_found() {
        let empty = std::env::temp_dir().join("perf-drm-test-missing-root");
        let mut sampler = DrmFdinfoSampler::new(empty.join("proc"), empty.join("drm"));
        let sample = sampler.sample(Instant::now());
        assert_eq!(sample, GpuSample::default());
        assert_eq!(sampler.source(), "none found");
        assert_eq!(sampler.last_error(), None);
    }
}
