//! The desk in the browser (`www/desk/`): the trading desk's own `app.rs`
//! (`crates/grid/examples/desk/`), the same code as natively, with the
//! trading module; only its native `main.rs` is left out, and its simulator
//! runs on the executor's timer instead of a thread.
//!
//! The page's parameters: `theme` (the start theme: `amber`, `depth` or
//! `heat`), `rate` (simulated updates a second) and `seed` (a repeatable
//! market). Every price shown is simulated.

// GPUI's `Action` derive spells its paths `gpui::` (the desk's actions).
extern crate gpui_kit as gpui;

use std::time::Duration;

use gpui_kit::{App, Entity, Window};
use gpui_kit_grid_web_shell::{FontFiles, PageDelegate, PageParams, grid_json, ms, stats_json};
use serde_json::Value;
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// What only the example's native `main` uses is dead code here.
#[allow(dead_code)]
#[path = "../../../../grid/examples/desk/app.rs"]
mod desk;

use desk::{Desk, DeskTheme};
use trading::{
    instruments::{INSTRUMENTS, InstrumentId},
    theme::TradingTheme as _,
};

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(DeskPage::default());
}

/// The desk example's side of the page.
#[derive(Default)]
struct DeskPage {
    /// What `setup` read from the page, for `open`.
    settings: Option<desk::Settings>,
}

impl PageDelegate for DeskPage {
    type View = Desk;

    /// The start theme's faces (Amber's by default), then the other two
    /// themes'.
    fn font_files(&self, theme: &str) -> FontFiles {
        let start = DeskTheme::parse(theme).unwrap_or(DeskTheme::Amber);
        let others = DeskTheme::ALL.iter().filter(|theme| **theme != start);
        FontFiles::from_themes(start.file(), others.map(|theme| theme.file()))
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<DeskTheme>(cx);
    }

    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        let theme = params.get("theme").unwrap_or_default();
        let settings = desk::Settings::new(
            params.number("rate").map(u64::from),
            DeskTheme::parse(&theme),
            Some(params.seed()),
        );
        desk::setup(&settings, cx);
        self.settings = Some(settings);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Desk> {
        let settings = self.settings.as_ref().expect("setup before open");
        desk::open(settings, window, cx)
    }

    /// The watchlist's counters.
    fn stats(&self, desk: &Entity<Desk>, built: Duration, cx: &App) -> Value {
        let desk = desk.read(cx);
        let control = desk.control();
        let grid = desk.watchlist_grid(cx).read(cx).stats();
        let mut stats = stats_json(control.updates(), control.rate(), &grid);
        stats.insert("market_ms".into(), ms(Duration::ZERO).into());
        stats.insert("build_ms".into(), ms(built).into());
        stats.into()
    }

    /// The watchlist's scroll, selection and sort; `app` the current
    /// instrument, the theme, the panel with the keyboard and more.
    fn grid_state(&self, desk: &Entity<Desk>, window: &Window, cx: &App) -> Value {
        let desk = desk.read(cx);
        let mut state = grid_json(desk.watchlist_grid(cx).read(cx), ticker);
        state.insert("app".into(), desk.page_state(window, cx));
        state.into()
    }

    /// The room of the watchlist's rows.
    fn scroll_room(&self, desk: &Entity<Desk>, cx: &App) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(desk.read(cx).watchlist_grid(cx).read(cx))
    }

    fn set_rate(&self, desk: &Entity<Desk>, rate: u32, cx: &mut App) -> Option<u32> {
        let set = desk.read(cx).control().set_rate(u64::from(rate));
        u32::try_from(set).ok()
    }
}

/// A quote's ticker.
fn ticker(id: &InstrumentId) -> &'static str {
    INSTRUMENTS
        .get(usize::from(id.0))
        .map_or("?", |instrument| instrument.ticker)
}
