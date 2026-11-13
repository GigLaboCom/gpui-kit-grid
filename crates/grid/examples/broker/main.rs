//! A retail broker app over a simulated market: a watchlist on the left,
//! the open instrument in the centre with a large price, an area chart and
//! the session's stats, the account and an order ticket on the right, and
//! the positions, the orders and the day's movers below; light and
//! friendly, with cards, rounded corners and Manrope.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Manrope
//! cargo run --release -p gpui-kit-grid --example broker
//! # No window: the app drawn offscreen to PNG files (`snapshot.rs`)
//! BROKER_SNAPSHOT=target/broker cargo run --release -p gpui-kit-grid --features test-support --example broker
//! ```
//!
//! **Everything is simulated.** The instruments are real US-listed tickers
//! and names, but their prices random-walk from approximate seed levels
//! (`trading/instruments.rs`, `trading/market.rs`), the account and its
//! holdings are made up, and an order fills against the simulated quote
//! only (`account.rs`). The app shows no fundamentals: only the stats of
//! the simulated session.
//!
//! - The top bar: the account's name, a search box that filters the
//!   watchlist through the grid's quick filter, the price colour convention
//!   (green up or red up) and the light or dark theme.
//! - The watchlist: symbol, name, last and Chg %, tall rows, no grid lines.
//!   Selecting a row opens the instrument; so does a row of the positions
//!   or of the movers.
//! - The instrument: the large last price and the day's change, the chart
//!   with its range tabs, and the session's stats.
//! - The account (total value, the day's P&L, cash, buying power) and the
//!   order ticket (Buy or Sell, Market or Limit, shares, limit price,
//!   estimate, Submit).
//! - Below: the positions with their P&L, the orders, and the top gainers
//!   and losers.
//!
//! The price colour convention is one setting (`colors.rs`): the grids'
//! text and change paint, the headline, the chart, the P&L and the
//! ticket's sides all follow it, live.
//!
//! The keys are one table (`KEYS`), which the bindings and the status
//! bar's hints are made from: `secondary` is Cmd on macOS and Ctrl
//! elsewhere (in a browser, Ctrl everywhere). A second table binds the
//! characters a Mac's Option key types in a browser (`BROWSER_OPTION_KEYS`).
//!
//! The app itself is `app.rs`; this file opens it in a window. The browser
//! build (`crates/grid-web/apps/broker`, the page `broker/`) runs the same
//! `app.rs` on GPUI's web platform.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `BROKER_RATE` | 150 | Simulated price updates per second |
//! | `BROKER_SEED` | none | A fixed seed: the same simulated market every run |
//! | `BROKER_SECONDS` | none | Print the status every second and quit after N seconds |
//! | `BROKER_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the app's own modules (`quotes.rs`, `ticket.rs`, ...) are
// its children in this folder; the browser build includes it the same way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("BROKER_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "BROKER_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (search, sun and moon): without them icons are blank.
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
                let broker = app::open(&settings, window, cx);
                // Closing the window ends the simulation and the app.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                broker
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
