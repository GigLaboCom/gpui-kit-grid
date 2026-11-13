//! The native build's sources and settings: Munich's compiled timetables
//! and Helsinki's network read from `examples/data/`, HSL's broker over
//! the examples' shared TLS and WebSocket (`../mqtt/native.rs`), a
//! recording to replay or fall back to, and the recording file
//! `TRANSIT_RECORD` writes. Native only: the browser fetches its data on
//! the page.

use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{
    Choice, Settings, Sources,
    hsl::{self, HfpRecording, HslFeed, HslSource, MessageSink, RecordingSupply},
    mqtt::native::NativeOpener,
    schedule::TimetableSource,
    source::{City, VehicleSource},
};

/// Where the native build finds the data `script/fetch-transit-data`
/// compiles, and keeps Helsinki's recording.
const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/data");

/// The sources of the files and the network.
pub struct NativeSources {
    /// Another compiled timetable for Munich (`TRANSIT_DATA`).
    timetable: Option<PathBuf>,
    /// Helsinki: replay the recording instead of going live, how fast, the
    /// recording, and the file the live messages are recorded into.
    replay: bool,
    speed: f64,
    recording: PathBuf,
    record: Option<PathBuf>,
    seed: u64,
}

impl NativeSources {
    fn munich(&self, regional: bool) -> Result<Box<dyn VehicleSource>, String> {
        let path = self.timetable.clone().unwrap_or_else(|| {
            let name = if regional {
                "mvv-transit-regional.tsv"
            } else {
                "mvv-transit.tsv"
            };
            Path::new(DATA).join(name)
        });
        let data = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Box::new(TimetableSource::new(data)))
    }

    fn helsinki(&mut self, buses: bool) -> Result<Box<dyn VehicleSource>, String> {
        let network_path = Path::new(DATA).join("hsl-network.tsv");
        let network = std::fs::read(&network_path)
            .inspect_err(|error| {
                err!(
                    "transit: {}: {error}; run {} --helsinki for the stops' names and the map",
                    network_path.display(),
                    super::FETCH_SCRIPT
                )
            })
            .ok();
        let feed = if self.replay {
            let recording = read_recording(&self.recording)?;
            HslFeed::Replay {
                recording: Arc::new(recording),
                speed: self.speed,
            }
        } else {
            HslFeed::Live {
                opener: Box::new(NativeOpener::new(&[])),
                fallback: Some(Box::new(RecordingFallback(self.recording.clone()))),
            }
        };
        self.seed = self.seed.wrapping_add(1);
        let sink = (!self.replay)
            .then(|| self.record.as_deref())
            .flatten()
            .and_then(|path| {
                RecordingFile::create(path)
                    .inspect_err(|error| err!("transit: TRANSIT_RECORD: {error}"))
                    .ok()
                    .map(|file| Box::new(file) as Box<dyn MessageSink>)
            });
        Ok(Box::new(
            HslSource::new(network, feed, buses, self.seed).with_sink(sink),
        ))
    }
}

impl Sources for NativeSources {
    fn source(&mut self, choice: Choice) -> Option<Result<Box<dyn VehicleSource>, String>> {
        Some(match choice.city() {
            City::Munich => self.munich(choice.is_regional()),
            City::Helsinki => self.helsinki(choice.has_buses()),
        })
    }
}

/// The settings of the environment:
///
/// - `TRANSIT_CITY`: `munich` (the default) or `helsinki`;
/// - `TRANSIT_REGIONAL=1`: Munich's regional trains and buses too;
/// - `TRANSIT_BUSES=1`: Helsinki's buses too;
/// - `TRANSIT_TIME`: a fixed start in Munich (`07:45`, `2026-11-10T07:45`);
/// - `TRANSIT_DATA`: another compiled timetable for Munich;
/// - `TRANSIT_SOURCE=replay`: Helsinki from the recording, not live;
/// - `TRANSIT_REPLAY`: the recording to replay or fall back to
///   (`examples/data/hsl-recording.txt`); `TRANSIT_SPEED` how fast;
/// - `TRANSIT_RECORD`: a file to record Helsinki's live messages into.
pub fn settings_from_env() -> Settings {
    let flag = |name: &str| {
        std::env::var(name).is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
    };
    let city = std::env::var("TRANSIT_CITY")
        .ok()
        .and_then(|city| {
            let parsed = City::from_id(&city);
            if parsed.is_none() {
                err!("TRANSIT_CITY={city}: munich or helsinki; Munich it is");
            }
            parsed
        })
        .unwrap_or_default();
    let choice = Choice::new(city)
        .with_regional(flag("TRANSIT_REGIONAL"))
        .with_buses(flag("TRANSIT_BUSES"));
    let seed = hsl::wall_ms() as u64 ^ u64::from(std::process::id());
    let sources = NativeSources {
        timetable: std::env::var_os("TRANSIT_DATA").map(PathBuf::from),
        replay: std::env::var("TRANSIT_SOURCE").is_ok_and(|source| source == "replay"),
        speed: std::env::var("TRANSIT_SPEED")
            .ok()
            .and_then(|speed| speed.parse::<f64>().ok())
            .filter(|speed| *speed > 0.)
            .unwrap_or(1.),
        recording: std::env::var_os("TRANSIT_REPLAY")
            .map_or_else(|| Path::new(DATA).join("hsl-recording.txt"), PathBuf::from),
        record: std::env::var_os("TRANSIT_RECORD").map(PathBuf::from),
        seed,
    };
    Settings::new(
        choice,
        Box::new(sources),
        std::env::var("TRANSIT_TIME").ok().as_deref(),
    )
}

/// The recording at `path`.
pub fn read_recording(path: &Path) -> Result<HfpRecording, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    HfpRecording::parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The recording file to replay when HSL cannot be reached, read once it
/// is needed.
struct RecordingFallback(PathBuf);

impl RecordingSupply for RecordingFallback {
    fn recording(&mut self) -> Option<Result<HfpRecording, String>> {
        Some(read_recording(&self.0))
    }
}

/// A recording file being written: `TRANSIT_RECORD=<file>`.
pub struct RecordingFile {
    file: BufWriter<File>,
    path: PathBuf,
    start_ms: Option<f64>,
    lines: u64,
}

impl RecordingFile {
    pub fn create(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        }
        let file = File::create(path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self {
            file: BufWriter::new(file),
            path: path.to_owned(),
            start_ms: None,
            lines: 0,
        })
    }
}

impl MessageSink for RecordingFile {
    fn record(&mut self, clock_ms: f64, topic: &str, payload: &[u8]) {
        let start_ms = *self.start_ms.get_or_insert_with(|| {
            let _ = self
                .file
                .write_all(HfpRecording::header(clock_ms).as_bytes());
            clock_ms
        });
        let line = HfpRecording::line((clock_ms - start_ms).max(0.), topic, payload);
        if self.file.write_all(line.as_bytes()).is_err() {
            err!("transit: cannot write {}", self.path.display());
        }
        self.lines += 1;
        // The process may end without dropping the source: what is
        // written stays written, but a hundred lines at most.
        if self.lines % 100 == 0 {
            let _ = self.file.flush();
        }
    }
}

impl Drop for RecordingFile {
    fn drop(&mut self) {
        let _ = self.file.flush();
        out!(
            "transit: {} messages recorded in {}",
            self.lines,
            self.path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::hsl::{PAYLOAD, TOPIC};
    use super::*;

    #[test]
    fn test_a_recording_file_reads_back() {
        let dir = std::env::temp_dir().join(format!("transit-recording-{}", std::process::id()));
        let path = dir.join("hsl.txt");
        {
            let mut file = RecordingFile::create(&path).expect("a file");
            file.record(1_000., TOPIC, PAYLOAD.as_bytes());
            file.record(1_500., TOPIC, PAYLOAD.as_bytes());
        }
        let recording = read_recording(&path).expect("reads back");
        assert_eq!(recording.start_ms(), 1_000.);
        assert_eq!(recording.len(), 2);
        assert_eq!(recording.duration_ms(), 500.);
        let _ = std::fs::remove_dir_all(dir);
    }
}
