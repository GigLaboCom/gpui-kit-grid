//! What the trading examples share: the instruments, the market simulator
//! and the rows it writes, the current-instrument bus, theme and font
//! loading, a quiet change paint and the offscreen PNG harness.
//!
//! The folder has no `main.rs`, so cargo does not take it for an example.
//! An example includes it as its first module, so the `out!` and `err!`
//! macros reach the example's own modules too:
//!
//! ```ignore
//! #[macro_use]
//! #[path = "../trading/mod.rs"]
//! mod trading;
//! ```
//!
//! and then uses what it needs:
//!
//! - `instruments`: 150 real US-listed tickers and names, and a few index
//!   and commodity futures with their tick sizes, all with approximate
//!   seed prices; **every price the examples show is simulated**;
//! - `market`: the simulator, on its own thread, writing the grid stores'
//!   writers it is given (`Feeds`): quotes, trades, the order book,
//!   positions, headlines and price bars. A feed left out costs nothing;
//! - `rows`: the rows and columns the simulator writes;
//! - `bus`: the current instrument, shared by an example's panels;
//! - `chart`: a candlestick chart panel with VWAP, an optional EMA and
//!   volume, over any source of price bars (`ChartSource`);
//! - `theme` and `fonts`: an example's own gpui-kit theme files
//!   (`TradingTheme`), with the fonts `script/fetch-example-fonts`
//!   downloads into `examples/fonts/`, or system fonts and a one-line hint;
//! - `paint`: `QuietChanges`, a change paint that keeps coloured text
//!   readable;
//! - `worker`: where the simulator and an example's periodic jobs run: a
//!   thread each natively, the executor's timer in the browser;
//! - `snapshot` (with `--features test-support`): the example drawn
//!   offscreen into PNG files, no window. Each example reads its own
//!   `<EXAMPLE>_SNAPSHOT=<dir>` variable (`DESK_SNAPSHOT`).
//!
//! Each example uses part of it; the rest is dead code there. Each
//! example's browser build (`crates/grid-web/apps/<app>`) includes it the
//! same way.
#![allow(dead_code)]

// A closed stdout or stderr, for example a parent that exited, must not
// abort an example.
#[cfg(not(target_family = "wasm"))]
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

#[cfg(not(target_family = "wasm"))]
macro_rules! err {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

// The browser has no stdout or stderr: the page's console.
#[cfg(target_family = "wasm")]
macro_rules! out {
    ($($arg:tt)*) => {
        log::info!($($arg)*)
    };
}

#[cfg(target_family = "wasm")]
macro_rules! err {
    ($($arg:tt)*) => {
        log::warn!($($arg)*)
    };
}

pub mod bus;
pub mod chart;
pub mod fonts;
pub mod instruments;
pub mod market;
pub mod paint;
pub mod rows;
#[cfg(feature = "test-support")]
pub mod snapshot;
pub mod theme;
pub mod worker;
