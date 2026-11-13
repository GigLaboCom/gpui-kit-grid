//! The GPU's side of a sample: the device's utilization and memory, and
//! each process's share of GPU time. A [`GpuSampler`] reads them once per
//! sample on the sampler's thread (`sampler.rs`); a [`GpuSamplerFactory`]
//! makes it there, so a sampler needs to be neither `Sync` nor made on the
//! UI thread. [`PlatformGpu`], the default factory, picks the platform's
//! sampler:
//!
//! - macOS: [`IoregSampler`], IOKit's registry as `ioreg` prints it, with
//!   no root and no private framework;
//! - Linux: [`DrmFdinfoSampler`](super::drm::DrmFdinfoSampler), the DRM
//!   clients' `fdinfo` and the cards in sysfs (`drm.rs`), for any DRM
//!   driver;
//! - elsewhere, for now, [`NoGpu`]: the GPU columns stay blank and the
//!   status bar says there is no GPU source.
//!
//! Where the other samplers would go, each one more implementation of
//! [`GpuSampler`] chosen by [`PlatformGpu`]:
//!
//! - **NVIDIA (Linux and Windows): NVML** (`libnvidia-ml`, through the
//!   `nvml-wrapper` crate). `nvmlDeviceGetUtilizationRates` and
//!   `nvmlDeviceGetMemoryInfo` give the device; per process,
//!   `nvmlDeviceGetProcessUtilization` gives each pid's SM share over a
//!   window since a timestamp, and `nvmlDeviceGetComputeRunningProcesses`
//!   / `…GraphicsRunningProcesses` each pid's video memory. Needs the
//!   driver, no root. The proprietary NVIDIA driver has no DRM fdinfo
//!   engine counters, so on Linux an NVML sampler would come before the
//!   DRM one, loaded at run time (`nvml-wrapper` opens the library with
//!   `libloading`) and falling back to it when the library is missing.
//! - **Windows: PDH**, the performance counters Task Manager reads:
//!   `\GPU Engine(*)\Utilization Percentage`, whose instance names carry
//!   `pid_<n>_…_engtype_<type>` (sum a pid's instances, per engine type
//!   take the busiest), and `\GPU Process Memory(*)\Dedicated Usage` /
//!   `\GPU Adapter Memory(*)\Dedicated Usage` for memory. Two collections a
//!   sample apart, as a rate counter needs. No administrator rights.

use std::{collections::HashMap, io::Cursor, process::Command, sync::Arc, time::Duration};

use plist::{Dictionary, Value};

use super::drm::DrmFdinfoSampler;
use web_time::Instant;

/// The GPU as a whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuDevice {
    /// The model (`Apple M3 Pro`), if the source names it.
    pub model: Option<Arc<str>>,
    /// % of the time the GPU was busy, if the source says.
    pub utilization: Option<f64>,
    /// macOS: the renderer's and the tiler's share.
    pub renderer: Option<f64>,
    pub tiler: Option<f64>,
    /// Linux without a device counter: the engine whose clients were
    /// busiest, which `utilization` then is (`render`, `gfx`, `video`).
    pub engine: Option<Arc<str>>,
    /// System memory the GPU uses now (unified memory) or video memory.
    pub memory_in_use: Option<u64>,
    /// Memory the driver has set aside for the GPU (macOS).
    pub memory_allocated: Option<u64>,
    /// Video memory in all (Linux, `mem_info_vram_total`).
    pub memory_total: Option<u64>,
}

/// One sample of the GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSample {
    pub device: Option<GpuDevice>,
    /// % of GPU time by pid over the interval since the last sample; a
    /// process without GPU clients, or seen for the first time, is not in
    /// it.
    pub processes: HashMap<u32, f64>,
    /// GPU memory by pid, where the source gives it (Linux fdinfo).
    pub memory: HashMap<u32, u64>,
}

/// Reads the GPU, once per sample, on the sampler's thread. One spawn or
/// one query per call: never per process.
pub trait GpuSampler: Send {
    /// Where the numbers come from, for the status bar: `IOKit (ioreg)`,
    /// `none`.
    fn source(&self) -> &str;

    /// The GPU now, its per-process shares measured since the previous
    /// call (`now` is the store's clock). A failure gives an empty sample
    /// and says why once (`last_error`).
    fn sample(&mut self, now: Instant) -> GpuSample;

    /// Why the last sample is empty, if it failed.
    fn last_error(&self) -> Option<&str> {
        None
    }
}

/// Makes the sampler on the sampler's thread.
pub trait GpuSamplerFactory: Send + Sync {
    fn create(&self) -> Box<dyn GpuSampler>;
}

/// The platform's sampler: [`IoregSampler`] on macOS,
/// [`DrmFdinfoSampler`] on Linux, [`NoGpu`] elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformGpu;

impl GpuSamplerFactory for PlatformGpu {
    fn create(&self) -> Box<dyn GpuSampler> {
        if cfg!(target_os = "macos") {
            Box::new(IoregSampler::default())
        } else if cfg!(target_os = "linux") {
            Box::new(DrmFdinfoSampler::default())
        } else {
            Box::new(NoGpu)
        }
    }
}

/// No GPU source: blank GPU columns. The default off macOS, and
/// `PERF_GPU=none`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGpu;

impl GpuSampler for NoGpu {
    fn source(&self) -> &str {
        "none"
    }

    fn sample(&mut self, _: Instant) -> GpuSample {
        GpuSample::default()
    }
}

impl GpuSamplerFactory for NoGpu {
    fn create(&self) -> Box<dyn GpuSampler> {
        Box::new(NoGpu)
    }
}

/// macOS: `ioreg -a -l -r -c IOAccelerator`, the accelerators with their
/// user clients as a property list, about 20 ms a call.
///
/// - The device: each accelerator's `PerformanceStatistics` (`Device
///   Utilization %`, `Renderer Utilization %`, `Tiler Utilization %`, `In
///   use system memory`, `Alloc system memory`).
/// - Per process: each `AGXDeviceUserClient` child names its creator
///   (`IOUserClientCreator` = `pid 617, WindowServer`) and carries an
///   `AppUsage` array, one entry per command queue, each with the
///   `accumulatedGPUTime` in nanoseconds. A pid's GPU time is the sum
///   over its clients and queues, and its share the delta of that sum
///   over the interval: what Activity Monitor shows as `% GPU`.
///
/// A client closed between two samples takes its time with it: that pid's
/// delta then counts as 0 for one sample rather than going negative.
#[derive(Debug, Default)]
pub struct IoregSampler {
    previous: Option<(Instant, HashMap<u32, u64>)>,
    error: Option<String>,
}

impl IoregSampler {
    fn read() -> Result<Vec<u8>, String> {
        let output = Command::new("ioreg")
            .args(["-a", "-l", "-r", "-c", "IOAccelerator"])
            .output()
            .map_err(|error| format!("ioreg: {error}"))?;
        if !output.status.success() {
            return Err(format!("ioreg: {}", output.status));
        }
        Ok(output.stdout)
    }
}

impl GpuSampler for IoregSampler {
    fn source(&self) -> &str {
        "IOKit (ioreg)"
    }

    fn sample(&mut self, now: Instant) -> GpuSample {
        let reading = match Self::read().and_then(|bytes| parse_ioreg(&bytes)) {
            Ok(reading) => reading,
            Err(error) => {
                if self.error.as_deref() != Some(&error) {
                    err!("perf: GPU: {error}");
                }
                self.error = Some(error);
                self.previous = None;
                return GpuSample::default();
            }
        };
        self.error = None;
        let processes = match &self.previous {
            Some((then, previous)) => gpu_shares(
                previous,
                &reading.gpu_time,
                now.saturating_duration_since(*then),
            ),
            None => HashMap::new(),
        };
        self.previous = Some((now, reading.gpu_time));
        GpuSample {
            device: reading.device,
            processes,
            memory: HashMap::new(),
        }
    }

    fn last_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

impl GpuSamplerFactory for IoregSampler {
    fn create(&self) -> Box<dyn GpuSampler> {
        Box::new(IoregSampler::default())
    }
}

/// What one `ioreg` call says.
#[derive(Debug, Default, PartialEq)]
pub struct IoregReading {
    /// The first accelerator with statistics.
    pub device: Option<GpuDevice>,
    /// GPU time so far by pid, in nanoseconds.
    pub gpu_time: HashMap<u32, u64>,
}

/// Parses `ioreg -a -l -r -c IOAccelerator`: an array of accelerators.
pub fn parse_ioreg(bytes: &[u8]) -> Result<IoregReading, String> {
    let value =
        Value::from_reader(Cursor::new(bytes)).map_err(|error| format!("ioreg: {error}"))?;
    let accelerators = value
        .as_array()
        .ok_or_else(|| "ioreg: not an array of accelerators".to_owned())?;
    let mut reading = IoregReading::default();
    for accelerator in accelerators.iter().filter_map(Value::as_dictionary) {
        if reading.device.is_none() {
            reading.device = device(accelerator);
        }
        let children = accelerator
            .get("IORegistryEntryChildren")
            .and_then(Value::as_array);
        for client in children
            .into_iter()
            .flatten()
            .filter_map(Value::as_dictionary)
        {
            let Some(pid) = client
                .get("IOUserClientCreator")
                .and_then(Value::as_string)
                .and_then(creator_pid)
            else {
                continue;
            };
            let time: u64 = client
                .get("AppUsage")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_dictionary)
                .filter_map(|usage| {
                    usage
                        .get("accumulatedGPUTime")
                        .and_then(Value::as_unsigned_integer)
                })
                .fold(0, u64::saturating_add);
            let total = reading.gpu_time.entry(pid).or_default();
            *total = total.saturating_add(time);
        }
    }
    Ok(reading)
}

/// An accelerator's `PerformanceStatistics`, if it has them.
fn device(accelerator: &Dictionary) -> Option<GpuDevice> {
    let stats = accelerator
        .get("PerformanceStatistics")
        .and_then(Value::as_dictionary)?;
    let number = |key: &str| stats.get(key).and_then(number);
    let bytes = |key: &str| stats.get(key).and_then(Value::as_unsigned_integer);
    Some(GpuDevice {
        model: accelerator
            .get("model")
            .and_then(Value::as_string)
            .map(Arc::from),
        utilization: Some(number("Device Utilization %")?),
        renderer: number("Renderer Utilization %"),
        tiler: number("Tiler Utilization %"),
        engine: None,
        memory_in_use: bytes("In use system memory"),
        memory_allocated: bytes("Alloc system memory"),
        memory_total: None,
    })
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_unsigned_integer()
        .map(|value| value as f64)
        .or_else(|| value.as_signed_integer().map(|value| value as f64))
        .or_else(|| value.as_real())
}

/// The pid of `pid 617, WindowServer`.
fn creator_pid(creator: &str) -> Option<u32> {
    creator
        .strip_prefix("pid ")?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Each pid's share of GPU time between two readings `interval` apart, in
/// % of the interval, at most 100. Only pids in both readings: a new one
/// has no delta yet. A sum that dropped (a client closed) counts as 0.
pub fn gpu_shares(
    previous: &HashMap<u32, u64>,
    current: &HashMap<u32, u64>,
    interval: Duration,
) -> HashMap<u32, f64> {
    let nanos = interval.as_nanos() as f64;
    if nanos <= 0. {
        return HashMap::new();
    }
    current
        .iter()
        .filter_map(|(pid, time)| {
            let before = previous.get(pid)?;
            let delta = time.saturating_sub(*before) as f64;
            Some((*pid, (delta / nanos * 100.).min(100.)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two accelerators' worth of `ioreg -a -l -r -c IOAccelerator`, cut
    /// down to the keys read: WindowServer with two clients, one of them
    /// with two queues, and a client of pid 621 without usage.
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>IOClass</key><string>AGXAcceleratorG15X</string>
    <key>model</key><string>Apple M3 Pro</string>
    <key>PerformanceStatistics</key>
    <dict>
      <key>Alloc system memory</key><integer>4383064064</integer>
      <key>Device Utilization %</key><integer>23</integer>
      <key>In use system memory</key><integer>1461878784</integer>
      <key>Renderer Utilization %</key><integer>21</integer>
      <key>Tiler Utilization %</key><integer>9</integer>
    </dict>
    <key>IORegistryEntryChildren</key>
    <array>
      <dict>
        <key>AppUsage</key><array/>
        <key>IOObjectClass</key><string>AGXDeviceUserClient</string>
        <key>IOUserClientCreator</key><string>pid 621, runningboardd</string>
      </dict>
      <dict>
        <key>AppUsage</key>
        <array>
          <dict>
            <key>API</key><string>Metal</string>
            <key>accumulatedGPUTime</key><integer>2000000000</integer>
          </dict>
          <dict>
            <key>API</key><string>Metal</string>
            <key>accumulatedGPUTime</key><integer>500000000</integer>
          </dict>
        </array>
        <key>IOUserClientCreator</key><string>pid 617, WindowServer</string>
      </dict>
      <dict>
        <key>AppUsage</key>
        <array>
          <dict>
            <key>accumulatedGPUTime</key><integer>250000000</integer>
          </dict>
        </array>
        <key>IOUserClientCreator</key><string>pid 617, WindowServer</string>
      </dict>
      <dict>
        <key>IOUserClientCreator</key><string>not a pid</string>
      </dict>
    </array>
  </dict>
  <dict>
    <key>IOClass</key><string>IOAccelerator</string>
  </dict>
</array>
</plist>
"#;

    #[test]
    fn test_parse_ioreg_reads_the_device_and_sums_each_pid() {
        let reading = parse_ioreg(FIXTURE.as_bytes()).expect("the fixture parses");
        let device = reading
            .device
            .expect("the first accelerator has statistics");
        assert_eq!(device.model.as_deref(), Some("Apple M3 Pro"));
        assert_eq!(device.utilization, Some(23.));
        assert_eq!(device.renderer, Some(21.));
        assert_eq!(device.tiler, Some(9.));
        assert_eq!(device.memory_in_use, Some(1_461_878_784));
        assert_eq!(device.memory_allocated, Some(4_383_064_064));
        assert_eq!(reading.gpu_time.get(&617), Some(&2_750_000_000));
        assert_eq!(reading.gpu_time.get(&621), Some(&0));
        assert_eq!(reading.gpu_time.len(), 2);
    }

    #[test]
    fn test_parse_ioreg_refuses_what_is_not_a_list_of_accelerators() {
        assert!(parse_ioreg(b"").is_err());
        let dict = r#"<?xml version="1.0"?><plist version="1.0"><dict/></plist>"#;
        assert!(parse_ioreg(dict.as_bytes()).is_err());
        let empty = r#"<?xml version="1.0"?><plist version="1.0"><array/></plist>"#;
        assert_eq!(parse_ioreg(empty.as_bytes()), Ok(IoregReading::default()));
    }

    #[test]
    fn test_creator_pid() {
        assert_eq!(creator_pid("pid 617, WindowServer"), Some(617));
        assert_eq!(creator_pid("pid 1,launchd"), Some(1));
        assert_eq!(creator_pid("WindowServer"), None);
        assert_eq!(creator_pid("pid x, y"), None);
    }

    #[test]
    fn test_gpu_shares_are_the_delta_over_the_interval() {
        let previous = HashMap::from([(617, 1_000_000_000), (621, 5_000), (700, 900)]);
        let current = HashMap::from([
            // A quarter of a second over half a second: 50 %.
            (617, 1_250_000_000),
            // Idle.
            (621, 5_000),
            // A client closed: the sum dropped, 0 rather than negative.
            (700, 100),
            // New: no delta yet.
            (800, 10_000_000),
        ]);
        let shares = gpu_shares(&previous, &current, Duration::from_millis(500));
        assert_eq!(shares.get(&617), Some(&50.));
        assert_eq!(shares.get(&621), Some(&0.));
        assert_eq!(shares.get(&700), Some(&0.));
        assert_eq!(shares.get(&800), None);
        // Several queues at once never show more than the whole GPU.
        let busy = HashMap::from([(617, 3_000_000_000)]);
        let shares = gpu_shares(&previous, &busy, Duration::from_secs(1));
        assert_eq!(shares.get(&617), Some(&100.));
        assert!(gpu_shares(&previous, &current, Duration::ZERO).is_empty());
    }

    #[test]
    fn test_no_gpu_samples_nothing() {
        let mut sampler = NoGpu.create();
        assert_eq!(sampler.source(), "none");
        assert_eq!(sampler.sample(Instant::now()), GpuSample::default());
        assert_eq!(sampler.last_error(), None);
    }
}
