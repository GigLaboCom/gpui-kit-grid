//! The native client of Wikimedia's stream: SSE over HTTPS with `ureq`
//! (rustls, the webpki roots), on a thread of its own, parsed by `sse.rs`
//! as the bytes arrive. One connection at a time; when it ends (the server
//! closes every connection after 15 minutes) the thread waits as
//! [`Backoff`] says and reconnects with `Last-Event-ID`, so no edit is
//! missed and none is asked for twice. It sends a descriptive
//! `User-Agent` ([`USER_AGENT`]) and honours an HTTP `Retry-After` and the
//! stream's `retry`.
//!
//! [`record`] saves a few minutes of the stream to a file for the replay.

use std::{
    any::Any,
    io::Read as _,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui_kit::App;
use gpui_kit_grid::Clock;
use ureq::Agent;
// The recorder's clock, outside any GPUI application; a profiling clock
// is all it needs.
use web_time::Instant;

use super::{
    StreamTransport,
    source::{Backoff, EditSource as _, Inbox, LiveEdits, Recording, STREAM_URL, USER_AGENT},
    sse::{SseItem, SseParser},
};

/// The waits between attempts are slept in slices this long.
const SLICE: Duration = Duration::from_millis(100);

/// The stream over HTTPS, on a thread.
pub struct HttpStream;

impl StreamTransport for HttpStream {
    fn open(&self, inbox: Arc<Inbox>, _: &mut App) -> Box<dyn Any> {
        Box::new(spawn(inbox))
    }
}

/// The reading thread's stop: dropping it stops the thread after its
/// current read (the stream sends many events a second).
pub struct Reading(Arc<AtomicBool>);

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Starts reading the stream into `inbox` on a thread of its own.
pub fn spawn(inbox: Arc<Inbox>) -> Reading {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("wiki-stream".into())
        .spawn(move || read(&inbox, &thread_stop));
    if let Err(error) = spawned {
        err!("wiki: no thread for the stream: {error}");
    }
    Reading(stop)
}

/// Connects, reads and reconnects until `stop`, or until the backoff gives
/// up.
fn read(inbox: &Inbox, stop: &AtomicBool) {
    let agent: Agent = Agent::config_builder()
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        // The server closes every connection after 15 minutes: a read
        // that hangs ends a little after that at the latest.
        .timeout_recv_body(Some(Duration::from_secs(16 * 60)))
        .build()
        .new_agent();
    let mut backoff = Backoff::new();
    let mut rng = fastrand::Rng::new();
    while !stop.load(Ordering::Relaxed) {
        inbox.connecting();
        let opened = inbox.now();
        let (live, reason) = connect(&agent, inbox, stop, &mut backoff);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if live {
            backoff.was_live_for(inbox.now().saturating_duration_since(opened));
        }
        let Some(wait) = backoff.failed(rng.f64()) else {
            inbox.unreachable(reason);
            return;
        };
        inbox.waiting(backoff.attempt(), wait, reason);
        // In slices, so a stop ends the wait.
        let slices = wait.as_millis().div_ceil(SLICE.as_millis());
        for _ in 0..slices {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(SLICE);
        }
    }
}

/// One connection: whether it opened, and why it ended.
fn connect(
    agent: &Agent,
    inbox: &Inbox,
    stop: &AtomicBool,
    backoff: &mut Backoff,
) -> (bool, String) {
    let mut request = agent.get(STREAM_URL).header("Accept", "text/event-stream");
    if let Some(id) = inbox.resume_id() {
        request = request.header("Last-Event-ID", id);
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(error) => return (false, error.to_string()),
    };
    let status = response.status().as_u16();
    if status != 200 {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        if let Some(seconds) = retry_after {
            backoff.set_floor(Duration::from_secs(seconds));
        }
        return (false, format!("HTTP {status}"));
    }
    inbox.opened();
    let mut reader = response.into_body().into_reader();
    let mut parser = SseParser::new();
    let mut buffer = vec![0; 16 * 1024];
    let mut items = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return (true, "stopped".into());
        }
        let read = match reader.read(&mut buffer) {
            Ok(0) => return (true, "closed by the server".into()),
            Ok(read) => read,
            Err(error) => return (true, error.to_string()),
        };
        parser.feed(&buffer[..read], &mut items);
        for item in items.drain(..) {
            match item {
                SseItem::Event(event) if event.kind == "message" => {
                    inbox.receive(&event.data, &event.id)
                }
                SseItem::Event(_) => {}
                SseItem::Retry(retry) => backoff.set_floor(retry),
            }
        }
    }
}

/// A monotonic clock for the recorder, which runs without an application.
struct RecorderClock;

impl Clock for RecorderClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// `WIKI_RECORD=<seconds>`: reads the live stream for `seconds` (at most
/// ten minutes) and saves its edits to `path` as a recording, JSON lines.
/// One connection, no window.
pub fn record(seconds: u64, path: &Path) {
    let seconds = seconds.clamp(5, 600);
    let clock: Arc<dyn Clock> = Arc::new(RecorderClock);
    let inbox = Inbox::new(clock.clone());
    let mut live = LiveEdits::new(inbox.clone(), clock.now());
    let reading = spawn(inbox);
    let mut edits = Vec::new();
    out!(
        "wiki: recording {seconds} s of {STREAM_URL} into {}",
        path.display()
    );
    for elapsed in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let status = live.take(clock.now(), &mut edits);
        if elapsed % 10 == 0 {
            out!(
                "wiki: {elapsed} s, {} edits, {} ({} canary dropped)",
                edits.len(),
                status.state.label(),
                status.counters.canary
            );
        }
    }
    drop(reading);
    let recording = Recording::new(edits);
    if let Some(dir) = path.parent()
        && let Err(error) = std::fs::create_dir_all(dir)
    {
        err!("{}: {error}", dir.display());
    }
    match std::fs::write(path, recording.to_json_lines()) {
        Ok(()) => out!(
            "wiki: {} edits over {:.0} s saved to {}",
            recording.len(),
            recording.span_ms() as f64 / 1e3,
            path.display()
        ),
        Err(error) => err!("{}: {error}", path.display()),
    }
}
