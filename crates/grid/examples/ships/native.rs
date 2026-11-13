//! The native transport: the examples' shared TLS and WebSocket
//! (`../mqtt/native.rs`) with the `Digitraffic-User` header on every
//! request, and the REST snapshot as one HTTPS GET each. Also here: the
//! recording file the native build can write (`SHIPS_RECORD`) and the
//! settings it reads from the environment. Native only: the browser has
//! its own WebSocket and `fetch`.

use std::{
    fs::File,
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{
    Settings, SourcePlan,
    driver::{EventSink, Fallback, FallbackRecording},
    live::{self, Connector, Snapshot, USER, wall_ms},
    mqtt::{
        native::NativeOpener,
        socket::{Opener, Socket},
    },
    source::{Recording, SourceEvent},
};

/// Where the native build keeps the recording: gitignored, with the
/// downloaded data of the other examples.
pub const RECORDING_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/data/ais-recording.txt"
);

/// tungstenite over rustls, opened at once, and the REST snapshot, every
/// request with the `Digitraffic-User` header.
pub struct NativeConnector {
    opener: NativeOpener,
}

impl NativeConnector {
    pub fn new() -> Self {
        Self {
            opener: NativeOpener::new(&[("Digitraffic-User", USER)]),
        }
    }
}

impl Opener for NativeConnector {
    fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
        self.opener.open(url, protocol)
    }
}

impl Connector for NativeConnector {
    fn snapshot(&mut self) -> Option<Result<Snapshot, String>> {
        let now = wall_ms();
        Some((|| {
            let locations = self.opener.get(&live::locations_url(now))?;
            let vessels = self.opener.get(&live::vessels_url(now))?;
            out!(
                "ships: snapshot of {} + {} bytes",
                locations.len(),
                vessels.len()
            );
            Ok(Snapshot { locations, vessels })
        })())
    }
}

/// A recording file being written: `SHIPS_RECORD=<file>`.
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

impl EventSink for RecordingFile {
    fn record(&mut self, events: &[SourceEvent], clock_ms: f64) {
        let start_ms = *self.start_ms.get_or_insert_with(|| {
            let _ = self.file.write_all(Recording::header(clock_ms).as_bytes());
            clock_ms
        });
        for event in events {
            // The snapshot's messages before the start, the stream's after.
            let offset = if event.snapshot {
                (event.at_ms - start_ms).min(-1.)
            } else {
                (event.at_ms - start_ms).max(0.)
            };
            let _ = self
                .file
                .write_all(Recording::line(offset, &event.message).as_bytes());
            self.lines += 1;
        }
        if self.file.flush().is_err() {
            err!("ships: cannot write {}", self.path.display());
        }
    }
}

impl Drop for RecordingFile {
    fn drop(&mut self) {
        let _ = self.file.flush();
        out!(
            "ships: {} messages recorded in {}",
            self.lines,
            self.path.display()
        );
    }
}

/// The recording file to replay when Digitraffic cannot be reached, read
/// once it is needed.
struct RecordingFallback(PathBuf);

impl FallbackRecording for RecordingFallback {
    fn recording(&mut self) -> Option<Result<Recording, String>> {
        Some(read_recording(&self.0))
    }
}

/// The recording at `path`.
pub fn read_recording(path: &Path) -> Result<Recording, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Recording::parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The settings of the environment:
///
/// - `SHIPS_SOURCE`: `live` (the default) or `replay`;
/// - `SHIPS_REPLAY`: the recording to replay, or to fall back to when
///   Digitraffic or its MQTT feed cannot be reached
///   (`examples/data/ais-recording.txt`);
/// - `SHIPS_SPEED`: how fast a replay runs (1);
/// - `SHIPS_RECORD`: a file to record the live messages into.
pub fn settings_from_env() -> Settings {
    let recording = std::env::var_os("SHIPS_REPLAY")
        .map_or_else(|| PathBuf::from(RECORDING_FILE), PathBuf::from);
    let speed = std::env::var("SHIPS_SPEED")
        .ok()
        .and_then(|speed| speed.parse::<f64>().ok())
        .filter(|speed| *speed > 0.)
        .unwrap_or(1.);
    let seed = wall_ms() as u64 ^ u64::from(std::process::id());
    let replay = std::env::var("SHIPS_SOURCE").is_ok_and(|source| source == "replay");
    if replay {
        let recording = read_recording(&recording).map(Arc::new);
        return Settings::new(SourcePlan::Replay { recording, speed }, seed);
    }
    let sink = std::env::var_os("SHIPS_RECORD").and_then(|path| {
        RecordingFile::create(Path::new(&path))
            .inspect_err(|error| err!("ships: SHIPS_RECORD: {error}"))
            .ok()
            .map(|file| Box::new(file) as Box<dyn EventSink>)
    });
    let fallback: Fallback = Box::new(RecordingFallback(recording));
    Settings::new(
        SourcePlan::Live {
            connector: Box::new(NativeConnector::new()),
            fallback: Some(fallback),
        },
        seed,
    )
    .with_sink(sink)
}
