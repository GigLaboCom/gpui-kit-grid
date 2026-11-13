//! Orbit in the browser (`www/orbit/`): every active satellite seen from
//! one place, live from real orbital data, the orbit example's own
//! `app.rs` (`crates/grid/examples/orbit/`), the same code as natively;
//! only its native `main.rs` is left out, and its sweep of every object is
//! spread over steps on the page's only thread.
//!
//! The page fetches the data (`data/celestrak-active.json`) and hands it
//! to [`orbit_data`] before `run`; [`set_warp`] runs the clock faster. The
//! positions shown are not simulated.

// GPUI's `Action` derive spells its paths `gpui::` (orbit's actions).
extern crate gpui_kit as gpui;

use std::{cell::RefCell, time::Duration};

use gpui_kit::{App, Entity, Window};
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
#[path = "../../../../grid/examples/orbit/app.rs"]
mod orbit;

use orbit::{Orbit, OrbitTheme};
use trading::theme::TradingTheme as _;

thread_local! {
    static HANDED: RefCell<Option<orbit::Settings>> = const { RefCell::new(None) };
}

/// The steps the sweep of every object is spread over in the browser,
/// where it runs on the page's only thread: one sweep a second, a quarter
/// of the objects every 250 ms.
const CHUNKS: u32 = 4;

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(OrbitPage::default());
}

/// The orbit example's data, before `run`: the bytes of
/// `data/celestrak-active.json` (OMM JSON) with the time it was
/// downloaded (Unix ms, the file's `Last-Modified`), or why the page has
/// none; the observer (`site`: `lat,lon,alt_m`, Greenwich if empty), a
/// fixed start of the clock (`time`, UTC) and the steps a sweep is spread
/// over (`chunks`, 4 unless given).
#[wasm_bindgen]
pub fn orbit_data(
    data: Option<Vec<u8>>,
    error: Option<String>,
    fetched_ms: Option<f64>,
    site: Option<String>,
    time: Option<String>,
    chunks: Option<u32>,
) {
    let data = data.ok_or_else(|| error.unwrap_or_else(|| "no data".into()));
    let settings = orbit::Settings::new(
        data,
        site.as_deref(),
        time.as_deref().filter(|time| !time.is_empty()),
        fetched_ms,
        chunks.unwrap_or(CHUNKS).max(1) as usize,
    );
    HANDED.set(Some(settings));
}

/// Runs the clock 1, 10 or 60 times as fast (the nearest of them);
/// returns the factor set, 0 before the view opens.
#[wasm_bindgen]
pub fn set_warp(factor: u32) -> u32 {
    with_view::<Orbit, _>(|page, _, cx| {
        let warp = orbit::producer::Warp::ALL
            .into_iter()
            .min_by_key(|warp| (warp.factor() as i64 - i64::from(factor)).abs())?;
        page.update(cx, |orbit, cx| orbit.set_warp(warp, cx));
        Some(warp.factor() as u32)
    })
    .unwrap_or(0)
}

/// The orbit example's side of the page.
#[derive(Default)]
struct OrbitPage {
    /// What `setup` took from the page, for `open`.
    settings: Option<orbit::Settings>,
}

impl PageDelegate for OrbitPage {
    type View = Orbit;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(OrbitTheme::Night.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<OrbitTheme>(cx);
    }

    /// The data [`orbit_data`] handed over, or why there is none.
    fn setup(&mut self, _: &PageParams, cx: &mut App) {
        let settings = HANDED.take().unwrap_or_else(|| {
            let missing = Err("the page handed over no data".into());
            orbit::Settings::new(missing, None, None, None, CHUNKS as usize)
        });
        orbit::setup(cx);
        self.settings = Some(settings);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Orbit> {
        let settings = self.settings.take().expect("setup before open");
        orbit::open(settings, window, cx)
    }

    /// The sky table's counters, as the other pages report their main
    /// grid's, and the producer's last sweep. `updates` counts sweeps,
    /// `rate` the rows written a second; `propagate_ms` is a whole sweep's
    /// propagation over its steps.
    fn stats(&self, page: &Entity<Orbit>, built: Duration, cx: &App) -> Value {
        let page = page.read(cx);
        let frame = page.control().frame();
        let grid = page.grid().read(cx);
        let mut stats = stats_json(frame.sweeps, frame.rate, &grid.stats());
        let Value::Object(own) = json!({
            "build_ms": ms(built),
            "objects": frame.objects,
            "up": frame.up,
            "shown": grid.projection().row_count(),
            "propagate_ms": ms(frame.propagate),
            "write_ms": ms(frame.write),
            "load_ms": ms(frame.load),
            "rises": frame.rises,
            "sets": frame.sets,
            "error": page.control().error(),
        }) else {
            unreachable!("an object")
        };
        stats.extend(own);
        stats.into()
    }

    /// The arrangement, the horizon filter, the warp, the search, the
    /// selection and the rows, for input checks.
    fn grid_state(&self, page: &Entity<Orbit>, _: &Window, cx: &App) -> Value {
        let page = page.read(cx);
        let grid = page.grid().read(cx);
        let name = |key: &orbit::rows::SkyKey| {
            grid.snapshot()
                .get(key)
                .map_or_else(String::new, |row| row.name.to_string())
        };
        let rows = grid.visible_rows();
        let frame = page.control().frame();
        json!({
            "rows": [rows.start, rows.end],
            "visible": grid.visible_keys().iter().map(name).collect::<Vec<_>>(),
            "selected": grid.selection().iter().map(name).collect::<Vec<_>>(),
            "sort": sort_json(grid.sort()),
            "app": {
                "arrangement": page.arrangement().label(),
                "up_only": page.is_up_only(),
                "warp": page.control().warp().factor(),
                "search": grid.quick_filter().to_string(),
                "shown": grid.projection().row_count(),
                "objects": frame.objects,
                "up": frame.up,
                "selected": page.control().selected(),
                "detail": frame.detail.as_ref().map(|detail| detail.name.to_string()),
                "pass_skip": page.control().pass_skip(),
            },
        })
    }

    /// The room of the sky's rows.
    fn scroll_room(&self, page: &Entity<Orbit>, cx: &App) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(page.read(cx).grid().read(cx))
    }
}
