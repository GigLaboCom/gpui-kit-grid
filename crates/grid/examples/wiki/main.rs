//! The whole world editing the Wikimedia wikis, live: one row per wiki
//! (about 900 of them: every Wikipedia, Wiktionary, Commons, Wikidata…),
//! ordered by edits in the last minute, so the order moves with the
//! world's editing. The data is Wikimedia's public EventStreams service
//! (`recentchange`, Server-Sent Events); nothing is simulated. Folio, set
//! in Inter and IBM Plex Mono.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Inter, IBM Plex Mono
//! cargo run --release -p gpui-kit-grid --example wiki
//! # A few minutes of the stream saved for the replay (not committed)
//! WIKI_RECORD=180 cargo run --release -p gpui-kit-grid --example wiki
//! WIKI_SOURCE=replay WIKI_SPEED=4 cargo run --release -p gpui-kit-grid --example wiki
//! # No window: the example drawn offscreen to PNG files (`snapshot.rs`)
//! WIKI_SNAPSHOT=target/wiki cargo run --release -p gpui-kit-grid --features test-support --example wiki
//! ```
//!
//! - **Wikis** (`table.rs`): edits a minute (a sliding window, with its
//!   trend), edits, the bots' share, new pages, bytes added and removed,
//!   the time of the last edit and the last page edited, a link. Grouped
//!   by project family with sums, or flat; a filter by wiki, family or
//!   language. A new edit flashes, a quiet wiki fades.
//! - **Pulse** (`strip.rs`): the last minute as a seismograph, a lane per
//!   family, a tick per edit by the bytes it changed.
//! - **Wiki**: the selected wiki's last edits, each page a link.
//! - **Status bar**: events a second, wikis, edits a minute, the stream's
//!   state, and the attribution.
//!
//! **Wikimedia's rules.** The stream is for small tools like this one: one
//! connection, a descriptive `User-Agent` (`source.rs`), a reconnect with
//! `Last-Event-ID` and backoff with jitter after the server's 15-minute
//! close or a failure (`native.rs`), canary events dropped, filtering on
//! this side. User names are never read and never kept, nor the titles of
//! user pages. Titles are the wikis' content, CC BY-SA 4.0.
//!
//! The model (`edit.rs`, `pulse.rs`, `source.rs`, `sse.rs`) has no GPUI
//! types. The example is `app.rs`; this file opens it in a window. The
//! browser build (`crates/grid-web/apps/wiki`, the page `wiki/`) runs the
//! same `app.rs` with the browser's `EventSource`.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `WIKI_SOURCE` | `live` | `live`: the stream at once; `replay`: the recording; `idle`: wait for Start |
//! | `WIKI_SPEED` | `1` | The replay's speed, 1 to 16 |
//! | `WIKI_REPLAY` | `examples/data/wiki-recording.jsonl` | The recording to replay or to write |
//! | `WIKI_RECORD` | none | Record this many seconds of the stream (5 to 600) into `WIKI_REPLAY`, no window |
//! | `WIKI_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |
//! | `WIKI_SECONDS` | `40` | With `WIKI_SNAPSHOT` and `WIKI_SOURCE=live`: how long the live run lasts |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules are its children in this
// folder; the browser build includes it the same way.
#[path = "app.rs"]
mod app;

use app::{Settings, Start, native::HttpStream, source::Recording};

/// Where the recorder writes and the replay reads by default.
const RECORDING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/data/wiki-recording.jsonl"
);

fn recording_path() -> PathBuf {
    std::env::var_os("WIKI_REPLAY").map_or_else(|| RECORDING.into(), PathBuf::from)
}

/// The recording, if there is one that parses.
fn load_recording() -> Option<Arc<Recording>> {
    let path = recording_path();
    let text = std::fs::read_to_string(&path).ok()?;
    match Recording::parse(&text) {
        Ok(recording) => Some(Arc::new(recording)),
        Err(error) => {
            err!("{}: {error}", path.display());
            None
        }
    }
}

fn start() -> Start {
    match std::env::var("WIKI_SOURCE").as_deref() {
        Ok("replay") => Start::Replay,
        Ok("idle") => Start::Idle,
        Ok("live") | Err(_) => Start::Live,
        Ok(other) => {
            err!("WIKI_SOURCE={other}: not live, replay or idle; live is used");
            Start::Live
        }
    }
}

fn settings(start: Start) -> Settings {
    let speed = std::env::var("WIKI_SPEED")
        .ok()
        .and_then(|speed| speed.parse().ok())
        .unwrap_or(1);
    Settings::new(start)
        .with_transport(HttpStream)
        .with_recording(load_recording())
        .with_speed(speed)
}

fn main() {
    if let Ok(seconds) = std::env::var("WIKI_RECORD") {
        match seconds.parse() {
            Ok(seconds) => app::native::record(seconds, &recording_path()),
            Err(_) => err!("WIKI_RECORD={seconds}: not a number of seconds"),
        }
        return;
    }
    let start = start();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("WIKI_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        {
            let live = (start == Start::Live
                && std::env::var("WIKI_SOURCE").as_deref() == Ok("live"))
            .then(|| {
                std::env::var("WIKI_SECONDS")
                    .ok()
                    .and_then(|seconds| seconds.parse().ok())
                    .unwrap_or(40)
            });
            app::snapshot::run(|| settings(Start::Idle), live, std::path::Path::new(&dir));
        }
        #[cfg(not(feature = "test-support"))]
        err!(
            "WIKI_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    let settings = settings(start);
    gpui_kit::application()
        // gpui-kit's icons (search, sort arrows, chevrons): without them
        // icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            app::setup(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1600.), px(960.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let wiki = app::open(settings, window, cx);
                // Closing the window ends the stream and the example.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                wiki
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
