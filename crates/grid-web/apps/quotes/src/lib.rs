//! The quotes page in the browser (`www/quotes/`): the trading examples'
//! 150 simulated quotes grouped by sector, in the desk's Depth theme
//! (`src/quotes.rs`), from the simulator or from the gateway's feed.
//!
//! The page's parameters: `rate` (simulated updates a second), `seed` (a
//! repeatable market), and a stress test: `rows`, static copies of the
//! quotes (at most 65 000), and `sort=chg` (%Chg descending, so every
//! update moves the order and the grid rebuilds its projection of all rows
//! every 100 ms); `feed` (a `ws://` address of the gateway) takes the
//! quotes from there instead of the simulator, and [`feed_stats`] reports
//! the feed. Every price shown is simulated.

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use std::{sync::Arc, time::Duration};

use gpui_kit::{App, AppContext as _, Entity, Focusable as _, Window};
use gpui_kit_grid_feed::QuoteRecord;
use gpui_kit_grid_web_shell::{
    FontFiles, PageDelegate, PageParams, feed::FeedRow, grid_json, ms, stats_json, with_view,
};
use serde_json::Value;
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// The desk's themes: the quotes are shown in its Depth theme.
#[path = "../../../../grid/examples/desk/theme.rs"]
mod desk_theme;
mod quotes;

use desk_theme::DeskTheme;
use quotes::{Quotes, Setup};
use trading::{
    instruments::{INSTRUMENTS, InstrumentId},
    market::{self, Control},
    rows::Quote,
    theme::TradingTheme as _,
};

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(QuotesPage::default());
}

/// The quotes example's side of the page.
#[derive(Default)]
struct QuotesPage {
    /// What `setup` read from the page, for `open`.
    start: Option<(Arc<Control>, Setup)>,
}

impl PageDelegate for QuotesPage {
    type View = Quotes;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(DeskTheme::Depth.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<DeskTheme>(cx);
    }

    /// gpui-kit, the grid's keys, the desk's Depth theme and the
    /// simulator's rate.
    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        let rate = params.number("rate").map(u64::from);
        let setup = Setup {
            seed: params.seed(),
            extra_rows: params.number("rows").unwrap_or(0).min(65_000),
            sort_by_change: params.get("sort").as_deref() == Some("chg"),
            feed: params.get("feed").filter(|url| !url.is_empty()),
        };
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
        trading::theme::install::<DeskTheme>(cx);
        DeskTheme::Depth.apply(cx);
        let control = Control::new(rate.unwrap_or(market::DEFAULT_RATE));
        self.start = Some((control, setup));
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Quotes> {
        let (control, setup) = self.start.take().expect("setup before open");
        let quotes = cx.new(|cx| Quotes::new(control, setup, cx));
        let focus = quotes.read(cx).grid().focus_handle(cx);
        window.focus(&focus, cx);
        quotes
    }

    fn stats(&self, quotes: &Entity<Quotes>, built: Duration, cx: &App) -> Value {
        let quotes = quotes.read(cx);
        let grid = quotes.grid().read(cx).stats();
        let mut stats = stats_json(quotes.updates(), quotes.control().rate(), &grid);
        stats.insert("market_ms".into(), ms(quotes.market_built()).into());
        stats.insert("build_ms".into(), ms(built).into());
        stats.into()
    }

    fn grid_state(&self, quotes: &Entity<Quotes>, _: &Window, cx: &App) -> Value {
        grid_json(quotes.read(cx).grid().read(cx), ticker).into()
    }

    /// The room of the grid's rows.
    fn scroll_room(
        &self,
        quotes: &Entity<Quotes>,
        cx: &App,
    ) -> gpui_kit_grid_web_shell::ScrollRoom {
        gpui_kit_grid_web_shell::ScrollRoom::of_grid(quotes.read(cx).grid().read(cx))
    }

    fn set_rate(&self, quotes: &Entity<Quotes>, rate: u32, cx: &mut App) -> Option<u32> {
        let set = quotes.read(cx).control().set_rate(u64::from(rate));
        u32::try_from(set).ok()
    }
}

/// The feed's counters and server → ingest latency percentiles (in
/// milliseconds) as JSON, `null` without a feed; `reset` starts a new set
/// of latency samples.
#[wasm_bindgen]
pub fn feed_stats(reset: bool) -> String {
    with_view::<Quotes, _>(|quotes, _, cx| {
        quotes
            .read(cx)
            .feed()
            .map(|feed| feed.borrow_mut().stats_json(reset))
    })
    .unwrap_or_else(|| "null".into())
}

/// A quote's ticker; `?` for a stress test's copy.
fn ticker(id: &InstrumentId) -> &'static str {
    INSTRUMENTS
        .get(usize::from(id.0))
        .map_or("?", |instrument| instrument.ticker)
}

impl FeedRow for Quote {
    /// The quote of a wire record, with the ticker, name, sector and
    /// previous close from the instrument table.
    fn from_record(record: &QuoteRecord) -> Option<Self> {
        let id = u16::try_from(record.id)
            .ok()
            .filter(|id| usize::from(*id) < INSTRUMENTS.len())
            .map(InstrumentId)?;
        let instrument = id.get();
        Some(Quote {
            id,
            ticker: instrument.ticker,
            name: instrument.name,
            sector: instrument.sector,
            last: record.last,
            close: instrument.seed,
            bid: record.bid,
            ask: record.ask,
            volume: record.volume,
        })
    }

    fn key_of_record(id: u32) -> Option<InstrumentId> {
        u16::try_from(id).ok().map(InstrumentId)
    }
}
