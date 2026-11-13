//! Transit in the browser (`www/transit/`): Munich's trains, trams and
//! buses under way by MVV's timetable, or Helsinki's live from HSL, the
//! transit example's own `app.rs` (`crates/grid/examples/transit/`), the
//! same code as natively; only its native `main.rs` and `native.rs` are
//! left out, and its producer runs on the page's only thread, a sweep a
//! second (four while time runs fast).
//!
//! The page reads its choice from its query (`?city=helsinki`,
//! `?regional`, `?buses`) and hands the module that choice's data before
//! `run`:
//!
//! - Munich ([`transit_data`]): the compiled timetable
//!   (`data/mvv-transit.tsv.gz`, or `data/mvv-transit-regional.tsv.gz`
//!   with the regional lines; © Münchner Verkehrs- und Tarifverbund GmbH
//!   (MVV), CC BY 4.0, which allows serving this transformation of it with
//!   the attribution the page and the example show), scheduled, not
//!   real-time; [`set_warp`] runs the clock faster;
//! - Helsinki ([`transit_helsinki`]): HSL's network
//!   (`data/hsl-network.tsv.gz`, © HSL, CC BY 4.0), and either the page's
//!   WebSocket to HSL's broker (the examples' shared `mqtt/browser.rs`) or
//!   a recording (`?source=replay`, `data/hsl-recording.txt.gz`). When the
//!   broker stays out of reach, the module asks for the recording
//!   ([`transit_fallback_wanted`]) and the page hands it over
//!   ([`transit_fallback`]): it is fetched only then.
//!
//! Another choice in the app bar loads the page again with its query.

// GPUI's `Action` derive spells its paths `gpui::` (transit's actions).
extern crate gpui_kit as gpui;

use std::{cell::RefCell, sync::Arc, time::Duration};

use gpui_kit::{App, Entity, Window};
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
#[path = "../../../../grid/examples/transit/app.rs"]
mod transit;

use trading::theme::TradingTheme as _;
use transit::{
    Choice, Settings, Sources, Transit, TransitTheme,
    hsl::{HfpRecording, HslFeed, HslSource, RecordingSupply},
    mqtt::browser::BrowserOpener,
    schedule::TimetableSource,
    source::{City, VehicleSource},
};

thread_local! {
    static HANDED: RefCell<Option<Settings>> = const { RefCell::new(None) };
    /// The fallback recording: asked for by the source, then handed over
    /// by the page.
    static FALLBACK: RefCell<FallbackSlot> = const { RefCell::new(FallbackSlot::Idle) };
}

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(TransitPage::default());
}

/// A seed from the browser's clock: `fastrand` has no entropy in this
/// build.
fn seed() -> u64 {
    js_sys::Date::now() as u64 ^ (js_sys::Math::random() * 1e15) as u64
}

/// The page's one choice and its source; any other choice loads the page
/// again with that choice's query.
struct PageSources {
    choice: Choice,
    source: Option<Result<Box<dyn VehicleSource>, String>>,
}

impl Sources for PageSources {
    fn source(&mut self, choice: Choice) -> Option<Result<Box<dyn VehicleSource>, String>> {
        if choice == self.choice
            && let Some(source) = self.source.take()
        {
            return Some(source);
        }
        let location = web_sys::window()?.location();
        if let Err(error) = location.set_search(&choice.query()) {
            err!("transit: cannot load {}: {error:?}", choice.query());
        }
        None
    }
}

/// Munich's data, before `run`: the compiled timetable's bytes (unpacked),
/// or why the page has none, a fixed start of the clock (`time`: `07:45`
/// today in Munich, `2026-11-10T07:45`, or UTC with a `Z`; the real clock
/// if empty), and whether it holds the regional lines.
#[wasm_bindgen]
pub fn transit_data(
    data: Option<Vec<u8>>,
    error: Option<String>,
    time: Option<String>,
    regional: Option<bool>,
) {
    let choice = Choice::new(City::Munich).with_regional(regional.unwrap_or(false));
    let source = data
        .ok_or_else(|| error.unwrap_or_else(|| "no data".into()))
        .map(|data| Box::new(TimetableSource::new(data)) as Box<dyn VehicleSource>);
    let sources = PageSources {
        choice,
        source: Some(source),
    };
    HANDED.set(Some(Settings::new(
        choice,
        Box::new(sources),
        time.as_deref(),
    )));
}

/// Helsinki's data, before `run`: HSL's compiled network (or none: the
/// stops' ids stand in for their names), a recording to replay instead of
/// going live (or none), why there is no recording to replay, whether the
/// buses are asked for, and a replay's speed.
#[wasm_bindgen]
pub fn transit_helsinki(
    network: Option<Vec<u8>>,
    recording: Option<Vec<u8>>,
    error: Option<String>,
    buses: bool,
    speed: Option<f64>,
) {
    let choice = Choice::new(City::Helsinki).with_buses(buses);
    let source = match (recording, error) {
        (Some(bytes), _) => HfpRecording::parse(&bytes).map(|recording| HslFeed::Replay {
            recording: Arc::new(recording),
            speed: speed.filter(|speed| *speed > 0.).unwrap_or(1.),
        }),
        (None, Some(error)) => Err(error),
        (None, None) => Ok(HslFeed::Live {
            opener: Box::new(BrowserOpener),
            fallback: Some(Box::new(PageFallback)),
        }),
    }
    .map(|feed| Box::new(HslSource::new(network, feed, buses, seed())) as Box<dyn VehicleSource>);
    let sources = PageSources {
        choice,
        source: Some(source),
    };
    HANDED.set(Some(Settings::new(choice, Box::new(sources), None)));
}

/// Where the fallback recording is.
enum FallbackSlot {
    Idle,
    Wanted,
    Handed(Result<HfpRecording, String>),
    Taken,
}

/// The page's recording, fetched when the source first asks.
struct PageFallback;

impl RecordingSupply for PageFallback {
    fn recording(&mut self) -> Option<Result<HfpRecording, String>> {
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

/// Whether the module waits for the fallback recording: HSL stayed out of
/// reach. The page polls it while live and then calls
/// [`transit_fallback`].
#[wasm_bindgen]
pub fn transit_fallback_wanted() -> bool {
    FALLBACK.with_borrow(|slot| matches!(slot, FallbackSlot::Wanted))
}

/// The fallback recording's bytes (unpacked), or why the page has none.
#[wasm_bindgen]
pub fn transit_fallback(recording: Option<Vec<u8>>, error: Option<String>) {
    let recording = recording
        .ok_or_else(|| error.unwrap_or_else(|| "no recording".into()))
        .and_then(|bytes| HfpRecording::parse(&bytes));
    FALLBACK.set(FallbackSlot::Handed(recording));
}

/// Runs the clock 1, 10 or 60 times as fast (the nearest of them);
/// returns the factor set, 0 before the view opens. A live clock stays at
/// 1.
#[wasm_bindgen]
pub fn set_warp(factor: u32) -> u32 {
    with_view::<Transit, _>(|page, _, cx| {
        let warp = transit::producer::Warp::ALL
            .into_iter()
            .min_by_key(|warp| (warp.factor() as i64 - i64::from(factor)).abs())?;
        page.update(cx, |transit, cx| transit.set_warp(warp, cx));
        Some(page.read(cx).control().warp().factor() as u32)
    })
    .unwrap_or(0)
}

/// Opens the board of the station named `name`; returns whether there is
/// one (Munich only: the board is by the timetable).
#[wasm_bindgen]
pub fn transit_station(name: String) -> bool {
    with_view::<Transit, _>(|page, window, cx| {
        let station = page.read(cx).find_station(&name)?;
        page.update(cx, |transit, cx| {
            transit.set_station(Some(station), window, cx)
        });
        Some(())
    })
    .is_some()
}

/// The transit example's side of the page.
#[derive(Default)]
struct TransitPage {
    /// What `setup` took from the page, for `open`.
    settings: Option<Settings>,
}

impl PageDelegate for TransitPage {
    type View = Transit;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(TransitTheme::Paper.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<TransitTheme>(cx);
    }

    /// What [`transit_data`] or [`transit_helsinki`] handed over.
    fn setup(&mut self, _: &PageParams, cx: &mut App) {
        let settings = HANDED.take().unwrap_or_else(|| {
            let choice = Choice::default();
            let sources = PageSources {
                choice,
                source: Some(Err("the page handed over no data".into())),
            };
            Settings::new(choice, Box::new(sources), None)
        });
        transit::setup(cx);
        self.settings = Some(settings);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Transit> {
        let settings = self.settings.take().expect("setup before open");
        transit::open(settings, window, cx)
    }

    /// The table's counters, as the other pages report their main grid's,
    /// and the producer's last sweep. `updates` counts sweeps, `rate` the
    /// rows written a second; a live feed adds its messages.
    fn stats(&self, page: &Entity<Transit>, built: Duration, cx: &App) -> Value {
        let page = page.read(cx);
        let frame = page.control().frame();
        let grid = page.grid().read(cx);
        let mut stats = stats_json(frame.sweeps, frame.rate, &grid.stats());
        let feed = frame.feed.as_ref();
        let Value::Object(own) = json!({
            "build_ms": ms(built),
            "city": page.choice().city().id(),
            "regional": page.choice().is_regional(),
            "buses": page.choice().has_buses(),
            "under_way": frame.under_way,
            "by_mode": frame.by_mode,
            "trips_today": frame.trips_today,
            "shown": grid.projection().row_count(),
            "compute_ms": ms(frame.compute),
            "write_ms": ms(frame.write),
            "load_ms": ms(frame.load),
            "started": frame.started,
            "departures": frame.departures,
            "finished": frame.finished,
            "sim_ms": frame.sim_ms,
            "board": frame.board,
            "station": frame.station,
            "feed": feed.map(|feed| feed.connection.to_string()),
            "messages": feed.map(|feed| feed.messages),
            "messages_per_s": feed.map(|feed| feed.rate),
            "notice": feed.and_then(|feed| feed.notice.as_deref().map(str::to_owned)),
            "error": page.control().error(),
        }) else {
            unreachable!("an object")
        };
        stats.extend(own);
        stats.into()
    }

    /// The choice, the arrangement, the zoom, the warp, the search, the
    /// selection, the board and the rows, for input checks.
    fn grid_state(&self, page: &Entity<Transit>, _: &Window, cx: &App) -> Value {
        let page = page.read(cx);
        let grid = page.grid().read(cx);
        let name = |key: &transit::source::VehicleKey| {
            grid.snapshot().get(key).map_or_else(String::new, |row| {
                format!("{} {}", row.line.name, row.headsign)
            })
        };
        let mut state = grid_json(grid, name);
        state.insert(
            "app".into(),
            json!({
                "city": page.choice().city().id(),
                "arrangement": page.arrangement().label(),
                "zoom": page.zoom(cx).label(),
                "warp": page.control().warp().factor(),
                "search": grid.quick_filter().to_string(),
                "shown": grid.projection().row_count(),
                "selected": page.control().selected().map(|key| key.get()),
                "board": page.board_line(cx),
            }),
        );
        state.into()
    }

    /// The room of the vehicles' rows.
    fn scroll_room(&self, page: &Entity<Transit>, cx: &App) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(page.read(cx).grid().read(cx))
    }
}
