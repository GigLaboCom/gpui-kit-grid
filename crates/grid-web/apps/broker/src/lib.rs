//! The broker in the browser (`www/broker/`): the retail broker app's own
//! `app.rs` (`crates/grid/examples/broker/`), the same code as natively,
//! with the trading module; only its native `main.rs` is left out, and its
//! simulator runs on the executor's timer instead of a thread.
//!
//! The page's parameters: `theme` (the start theme: `light` or `dark`),
//! `rate` (simulated updates a second) and `seed` (a repeatable market).
//! Every price shown is simulated.

// GPUI's `Action` derive spells its paths `gpui::` (the broker's actions).
extern crate gpui_kit as gpui;

use std::time::Duration;

use gpui_kit::{App, Entity, Window};
use gpui_kit_grid_web_shell::{
    FontFiles, PageDelegate, PageParams, fonts, grid_json, ms, stats_json,
};
use serde_json::Value;
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// What only the example's native `main` uses is dead code here.
#[allow(dead_code)]
#[path = "../../../../grid/examples/broker/app.rs"]
mod broker;

use broker::{Broker, BrokerTheme};
use trading::{
    instruments::{INSTRUMENTS, InstrumentId},
    theme::TradingTheme as _,
};

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(BrokerPage::default());
}

/// The broker example's side of the page.
#[derive(Default)]
struct BrokerPage {
    /// What `setup` read from the page, for `open`.
    settings: Option<broker::Settings>,
}

impl PageDelegate for BrokerPage {
    type View = Broker;

    /// The start theme's faces (Light's by default) with the ▲ and ▼ its
    /// family lacks, then the other theme's.
    fn font_files(&self, theme: &str) -> FontFiles {
        let start = BrokerTheme::parse(theme).unwrap_or(BrokerTheme::Light);
        let others = BrokerTheme::ALL.iter().filter(|theme| **theme != start);
        FontFiles::from_themes(start.file(), others.map(|theme| theme.file()))
            .with_start_face(fonts::SYMBOLS)
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<BrokerTheme>(cx);
    }

    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        let settings =
            broker::Settings::new(params.number("rate").map(u64::from), Some(params.seed()));
        broker::setup(cx);
        let theme = params.get("theme").unwrap_or_default();
        if let Some(theme) = BrokerTheme::parse(&theme) {
            theme.apply(cx);
        }
        self.settings = Some(settings);
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Broker> {
        let settings = self.settings.as_ref().expect("setup before open");
        broker::open(settings, window, cx)
    }

    /// The watchlist's counters.
    fn stats(&self, broker: &Entity<Broker>, built: Duration, cx: &App) -> Value {
        let broker = broker.read(cx);
        let control = broker.control();
        let grid = broker.watchlist_grid().read(cx).stats();
        let mut stats = stats_json(control.updates(), control.rate(), &grid);
        stats.insert("market_ms".into(), ms(Duration::ZERO).into());
        stats.insert("build_ms".into(), ms(built).into());
        stats.into()
    }

    /// The watchlist's scroll, selection and sort; `app` the open
    /// instrument, the theme, the orders, the ticket and more.
    fn grid_state(&self, broker: &Entity<Broker>, window: &Window, cx: &App) -> Value {
        let broker = broker.read(cx);
        let mut state = grid_json(broker.watchlist_grid().read(cx), ticker);
        state.insert("app".into(), broker.page_state(window, cx));
        state.into()
    }

    /// The room of the watchlist's rows.
    fn scroll_room(
        &self,
        broker: &Entity<Broker>,
        cx: &App,
    ) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(broker.read(cx).watchlist_grid().read(cx))
    }

    fn set_rate(&self, broker: &Entity<Broker>, rate: u32, cx: &mut App) -> Option<u32> {
        let set = broker.read(cx).control().set_rate(u64::from(rate));
        u32::try_from(set).ok()
    }
}

/// A quote's ticker.
fn ticker(id: &InstrumentId) -> &'static str {
    INSTRUMENTS
        .get(usize::from(id.0))
        .map_or("?", |instrument| instrument.ticker)
}
