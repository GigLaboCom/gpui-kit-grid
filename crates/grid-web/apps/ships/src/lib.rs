//! Ships in the browser (`www/ships/`): every vessel around Finland, live
//! from Digitraffic's AIS (Fintraffic, CC BY 4.0), the ships example's own
//! `app.rs` (`crates/grid/examples/ships/`), the same code as natively;
//! only its native `main.rs` and transport (`native.rs`) are left out. The
//! page connects to Digitraffic itself, with no server of ours: its
//! `fetch` brings the REST snapshot, and the browser's WebSocket (the
//! examples' shared `mqtt/browser.rs`) carries MQTT.
//!
//! Before `run` the page hands over either the snapshot ([`ships_live`])
//! or a recording to replay ([`ships_replay`]): `?source=replay`, or when
//! Digitraffic cannot be reached, with a notice. When the snapshot came
//! but MQTT stays out of reach, the module asks for the recording
//! ([`ships_fallback_wanted`]) and the page hands it over
//! ([`ships_fallback`]): it is fetched only then.

// GPUI's `Action` derive spells its paths `gpui::` (the ships' actions).
extern crate gpui_kit as gpui;

use std::{cell::RefCell, sync::Arc, time::Duration};

use gpui_kit::{App, Entity, SharedString, Window};
use gpui_kit_grid_web_shell::{
    FontFiles, PageDelegate, PageParams, ms, sort_json, stats_json, with_view,
};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// What only the example's native `main` uses is dead code here.
#[allow(dead_code)]
#[path = "../../../../grid/examples/ships/app.rs"]
mod ships;

use ships::{
    Settings, Ships, ShipsTheme, SourcePlan,
    driver::FallbackRecording,
    live::{Connector, Snapshot},
    mqtt::{
        browser::BrowserOpener,
        socket::{Opener, Socket},
    },
    source::Recording,
};
use trading::theme::TradingTheme as _;

thread_local! {
    static HANDED: RefCell<Option<Settings>> = const { RefCell::new(None) };
    /// The fallback recording: asked for by the driver, then handed over by
    /// the page.
    static FALLBACK: RefCell<FallbackSlot> = const { RefCell::new(FallbackSlot::Idle) };
}

/// Where the fallback recording is.
enum FallbackSlot {
    Idle,
    Wanted,
    Handed(Result<Recording, String>),
    Taken,
}

/// The page's recording, fetched when the driver first asks.
struct PageFallback;

impl FallbackRecording for PageFallback {
    fn recording(&mut self) -> Option<Result<Recording, String>> {
        FALLBACK.with_borrow_mut(|slot| match std::mem::replace(slot, FallbackSlot::Taken) {
            FallbackSlot::Handed(recording) => Some(recording),
            FallbackSlot::Idle | FallbackSlot::Wanted => {
                *slot = FallbackSlot::Wanted;
                None
            }
            FallbackSlot::Taken => Some(Err("the recording was handed over already".into())),
        })
    }
}

/// Whether the module waits for the fallback recording: MQTT stayed out
/// of reach. The page polls it while live and then calls
/// [`ships_fallback`].
#[wasm_bindgen]
pub fn ships_fallback_wanted() -> bool {
    FALLBACK.with_borrow(|slot| matches!(slot, FallbackSlot::Wanted))
}

/// The fallback recording's bytes, or why the page has none.
#[wasm_bindgen]
pub fn ships_fallback(recording: Option<Vec<u8>>, error: Option<String>) {
    let recording = recording
        .ok_or_else(|| error.unwrap_or_else(|| "no recording".into()))
        .and_then(|bytes| Recording::parse(&bytes));
    FALLBACK.set(FallbackSlot::Handed(recording));
}

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(ShipsPage::default());
}

/// A seed from the browser's clock: `fastrand` has no entropy in this
/// build.
fn seed() -> u64 {
    js_sys::Date::now() as u64 ^ (js_sys::Math::random() * 1e15) as u64
}

/// Live from Digitraffic, before `run`: the bytes of the REST snapshot the
/// page fetched (`/api/ais/v1/locations` and `/api/ais/v1/vessels`).
#[wasm_bindgen]
pub fn ships_live(locations: Vec<u8>, vessels: Vec<u8>) {
    let connector = BrowserConnector {
        opener: BrowserOpener,
        snapshot: Some(Ok(Snapshot { locations, vessels })),
    };
    let settings = Settings::new(
        SourcePlan::Live {
            connector: Box::new(connector),
            fallback: Some(Box::new(PageFallback)),
        },
        seed(),
    );
    HANDED.set(Some(settings));
}

/// A recording to replay, before `run`: its bytes, or why the page has
/// none; `notice` says why it replays (Digitraffic unreachable), and
/// `speed` how fast (1 unless given).
#[wasm_bindgen]
pub fn ships_replay(
    recording: Option<Vec<u8>>,
    error: Option<String>,
    notice: Option<String>,
    speed: Option<f64>,
) {
    let recording = recording
        .ok_or_else(|| error.unwrap_or_else(|| "no recording".into()))
        .and_then(|bytes| Recording::parse(&bytes))
        .map(Arc::new);
    let settings = Settings::new(
        SourcePlan::Replay {
            recording,
            speed: speed.filter(|speed| *speed > 0.).unwrap_or(1.),
        },
        seed(),
    )
    .with_notice(notice.map(SharedString::from));
    HANDED.set(Some(settings));
}

/// The ships example's side of the page.
#[derive(Default)]
struct ShipsPage {
    /// What `setup` took from the page, for `open`.
    settings: Option<Settings>,
}

impl PageDelegate for ShipsPage {
    type View = Ships;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(ShipsTheme::Bridge.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<ShipsTheme>(cx);
    }

    /// What [`ships_live`] or [`ships_replay`] handed over.
    fn setup(&mut self, _: &PageParams, cx: &mut App) {
        let settings = HANDED.take().unwrap_or_else(|| {
            let missing = Err("the page handed over no source".into());
            Settings::new(
                SourcePlan::Replay {
                    recording: missing,
                    speed: 1.,
                },
                seed(),
            )
        });
        ships::setup(cx);
        self.settings = Some(settings);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Ships> {
        let settings = self.settings.take().expect("setup before open");
        ships::open(settings, window, cx)
    }

    /// The vessels table's counters, as the other pages report their main
    /// grid's: `updates` counts the messages, `rate` is messages a second.
    fn stats(&self, page: &Entity<Ships>, built: Duration, cx: &App) -> Value {
        let page = page.read(cx);
        let frame = page.frame();
        let grid = page.grid().read(cx);
        let mut stats = stats_json(frame.messages, frame.rate, &grid.stats());
        let Value::Object(own) = json!({
            "build_ms": ms(built),
            "vessels": frame.counts.vessels,
            "named": frame.counts.named,
            "moving": frame.counts.moving,
            "berthed": frame.counts.berthed,
            "quiet": frame.counts.stale,
            "shown": grid.projection().row_count(),
            "apply_ms": ms(frame.apply),
            "first_rows_ms": frame.first_rows.map(ms),
            "connection": format!("{:?}", frame.connection),
            "notice": frame.notice.as_deref(),
            "error": None::<String>,
        }) else {
            unreachable!("an object")
        };
        stats.extend(own);
        stats.into()
    }

    /// The grouping, the filter, the area, the search, the selection and
    /// the rows, for input checks.
    fn grid_state(&self, page: &Entity<Ships>, _: &Window, cx: &App) -> Value {
        let page = page.read(cx);
        let grid = page.grid().read(cx);
        let name = |mmsi: &u32| {
            grid.snapshot()
                .get(mmsi)
                .map_or_else(String::new, |row| row.title())
        };
        let rows = grid.visible_rows();
        json!({
            "rows": [rows.start, rows.end],
            "visible": grid.visible_keys().iter().map(name).collect::<Vec<_>>(),
            "selected": grid.selection().iter().map(name).collect::<Vec<_>>(),
            "sort": sort_json(grid.sort()),
            "app": {
                "grouping": page.grouping().label(),
                "moving_only": page.is_moving_only(),
                "area": page.area(),
                "search": grid.quick_filter().to_string(),
                "shown": grid.projection().row_count(),
                "vessels": page.frame().counts.vessels,
                "selected": page.control().selected(),
            },
        })
    }

    /// The room of the vessels' rows.
    fn scroll_room(&self, page: &Entity<Ships>, cx: &App) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(page.read(cx).grid().read(cx))
    }
}

/// Selects the vessel `mmsi` in the table, the card and the map; returns
/// whether the view is open.
#[wasm_bindgen]
pub fn select_vessel(mmsi: u32) -> bool {
    with_view::<Ships, _>(|page, _, cx| {
        page.update(cx, |ships, cx| ships.select(mmsi, cx));
        Some(())
    })
    .is_some()
}

/// Opens browser WebSockets; gives the page's snapshot once.
struct BrowserConnector {
    opener: BrowserOpener,
    snapshot: Option<Result<Snapshot, String>>,
}

impl Opener for BrowserConnector {
    fn open(&mut self, url: &str, protocol: &str) -> Result<Box<dyn Socket>, String> {
        self.opener.open(url, protocol)
    }
}

impl Connector for BrowserConnector {
    fn snapshot(&mut self) -> Option<Result<Snapshot, String>> {
        self.snapshot.take()
    }
}
