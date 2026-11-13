//! Wiki in the browser (`www/wiki/`): every Wikimedia wiki being edited,
//! live, the wiki example's own `app.rs` (`crates/grid/examples/wiki/`),
//! the same code as natively; only its native `main.rs` and HTTP client
//! (`native.rs`) are left out. The stream is the browser's `EventSource`
//! (`stream.rs`), straight to Wikimedia: no server of ours.
//!
//! The page never opens the stream by itself: the view waits for Start,
//! closes the stream on a tab hidden for a minute (and opens it again when
//! the tab is shown) or after five minutes without an
//! interaction (`presence.rs`), and falls back to a recording if the
//! stream cannot be reached. The page may hand a recording over
//! ([`wiki_recording`]) before or after `run`; `?replay` plays it at
//! start (the checks do), `?speed=` sets its speed.

// GPUI's `Action` derive spells its paths `gpui::` (the example's actions).
extern crate gpui_kit as gpui;

use std::{cell::RefCell, sync::Arc, time::Duration};

use gpui_kit::{App, Entity, Window, web::CanvasFontFallback};
use gpui_kit_grid_web_shell::{
    FontFiles, PageDelegate, PageParams, grid_json, ms, stats_json, with_view,
};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// What only the example's native `main` uses is dead code here.
#[allow(dead_code)]
#[path = "../../../../grid/examples/wiki/app.rs"]
mod wiki;

mod presence;
mod stream;

use trading::theme::TradingTheme as _;
use wiki::{Settings, Start, Wiki, WikiTheme, source::Recording};

thread_local! {
    /// A recording handed over before the view opened.
    static RECORDING: RefCell<Option<Arc<Recording>>> = const { RefCell::new(None) };
}

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(WikiPage::default());
}

/// A recording (JSON lines, `crates/grid/examples/data/`), before or after
/// `run`; returns the edits it holds, 0 if it does not parse.
#[wasm_bindgen]
pub fn wiki_recording(bytes: Vec<u8>) -> u32 {
    let recording = match Recording::parse(&String::from_utf8_lossy(&bytes)) {
        Ok(recording) => Arc::new(recording),
        Err(error) => {
            log::warn!("wiki recording: {error}");
            return 0;
        }
    };
    let edits = recording.len() as u32;
    let handed = with_view::<Wiki, _>(|view, _, cx| {
        view.update(cx, |wiki, cx| {
            wiki.set_recording(Some(recording.clone()), cx)
        });
        Some(())
    });
    if handed.is_none() {
        RECORDING.set(Some(recording));
    }
    edits
}

/// The wiki example's side of the page.
#[derive(Default)]
struct WikiPage {
    start: Start,
    speed: u32,
}

impl PageDelegate for WikiPage {
    type View = Wiki;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(WikiTheme::Folio.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<WikiTheme>(cx);
    }

    /// Titles in Chinese, Japanese and Korean, which no face the page
    /// loads has, drawn by the browser.
    fn font_fallback(&self) -> CanvasFontFallback {
        CanvasFontFallback::EmojiAndCjk
    }

    /// `?replay` plays the recording at start, `?speed=` its speed; the
    /// live stream waits for Start in any case.
    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        self.start = if params.get("replay").is_some() {
            Start::Replay
        } else {
            Start::Idle
        };
        self.speed = params.number("speed").unwrap_or(1);
        wiki::setup(cx);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Wiki> {
        let settings = Settings::new(self.start)
            .with_transport(stream::BrowserStream)
            .with_recording(RECORDING.take())
            .with_presence(presence::PagePresence::listen())
            .with_glyphs(wiki::loaded_glyphs(cx))
            .with_speed(self.speed);
        wiki::open(settings, window, cx)
    }

    /// The table's counters, as the other pages report their main grid's,
    /// and the pump's. `updates` counts the pump's steps, `rate` the rows
    /// written a second.
    fn stats(&self, page: &Entity<Wiki>, built: Duration, cx: &App) -> Value {
        let page = page.read(cx);
        let frame = page.control().frame();
        let grid = page.grid().read(cx);
        let mut stats = stats_json(frame.steps, frame.rows_per_second, &grid.stats());
        let Value::Object(own) = json!({
            "build_ms": ms(built),
            "mode": page.mode().label(),
            "feed": frame.feed.as_ref().map(|feed| feed.label()),
            "events_per_second": frame.events_per_second,
            "events": frame.events,
            "edits": frame.edits,
            "wikis": frame.wikis,
            "per_minute": frame.per_minute,
            "shown": grid.projection().row_count(),
            "rows_written": frame.rows_written,
            "step_ms": ms(frame.step),
            "canary": frame.counters.canary,
            "malformed": frame.counters.malformed,
            "reconnects": frame.counters.reconnects,
            "has_recording": page.has_recording(),
            "error": frame.error,
        }) else {
            unreachable!("an object")
        };
        stats.extend(own);
        stats.into()
    }

    /// The table's scroll, selection and sort, the mode, the arrangement,
    /// the search and the replay's speed, for input checks.
    fn grid_state(&self, page: &Entity<Wiki>, _: &Window, cx: &App) -> Value {
        let page = page.read(cx);
        let grid = page.grid().read(cx);
        let mut state = grid_json(grid, |key| key.to_string());
        let Value::Object(own) = json!({
            "app": {
                "mode": page.mode().label(),
                "arrangement": page.arrangement().label(),
                "search": grid.quick_filter().to_string(),
                "speed": page.speed(),
                "selected": page.control().selected().map(|wiki| wiki.to_string()),
                "wikis": page.control().frame().wikis,
            },
        }) else {
            unreachable!("an object")
        };
        state.extend(own);
        state.into()
    }

    /// The room of the wikis' rows.
    fn scroll_room(&self, page: &Entity<Wiki>, cx: &App) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(page.read(cx).grid().read(cx))
    }

    /// The bench's rates are the replay's speeds.
    fn set_rate(&self, page: &Entity<Wiki>, rate: u32, cx: &mut App) -> Option<u32> {
        page.update(cx, |wiki, cx| wiki.set_speed(rate, cx));
        Some(page.read(cx).speed())
    }
}
