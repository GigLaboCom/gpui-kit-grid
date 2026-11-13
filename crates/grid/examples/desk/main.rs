//! A trading desk: a dense, keyboard-first workstation over a simulated
//! market, with the grid in several dock panels and three themes that show
//! how far it can be styled.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: the themes' fonts
//! cargo run --release -p gpui-kit-grid --example desk
//! DESK_THEME=heat DESK_RATE=20000 cargo run --release -p gpui-kit-grid --example desk
//! # No window: the desk drawn offscreen to one PNG per theme (`snapshot.rs`)
//! DESK_SNAPSHOT=target/desk cargo run --release -p gpui-kit-grid --features test-support --example desk
//! ```
//!
//! **Everything is simulated.** The instruments are real US-listed tickers
//! and names, but their prices start from approximate seed levels typed in
//! from memory (`trading/instruments.rs`) and random-walk from there; the
//! trades, the order book, the positions and the headlines are made up by
//! the simulator (`trading/market.rs`), and the headlines only describe the
//! simulation. The status bar says SIMULATED.
//!
//! The simulator, the instruments, the chart, the theme and font loading
//! and the offscreen harness are shared with the other trading examples
//! (`examples/trading/`). The desk itself is `app.rs`; this file opens it
//! in a window. The browser build (`crates/grid-web/apps/desk`, the page
//! `desk/`) runs the same `app.rs` on GPUI's web platform.
//!
//! The command line on top takes a ticker (`NVDA`) to make an instrument
//! current, `THEME amber|depth|heat`, `RATE <n>`, `GROUP <column>|OFF`,
//! `TF 5s|1m|5m` (the chart's candles), `TAPE CUR|ALL` and `HELP`
//! (`command.rs`). Selecting a watchlist row or a position makes its
//! instrument current too; the news, the chart, the order book and the
//! trades tape follow it. Keys: F1 help, F2/F3/F4 the themes, F5 the
//! chart's next timeframe, F6 the command line, F7 the watchlist, F8 the
//! news, F9 the order book, F10 the trades, F12 the positions (F11 is left
//! to macOS, which shows the desktop with it).
//!
//! The themes are gpui-kit theme files (`themes/*.json`); their fonts are
//! downloaded by `script/fetch-example-fonts` into `examples/fonts/` (not
//! committed). Without them the desk runs with the system's monospace font
//! and says so once on stderr.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `DESK_RATE` | 300 | Simulated price updates per second, about two per instrument (`RATE` changes it; 5000 and up stress the grid) |
//! | `DESK_THEME` | `amber` | The theme at start: `amber`, `depth` or `heat` |
//! | `DESK_SEED` | none | A fixed seed: the same simulated market every run |
//! | `DESK_SECONDS` | none | Print the status every second and quit after N seconds |
//! | `DESK_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the desk's own modules (`book.rs`, `panels.rs`, ...) are
// its children in this folder; the browser build includes it the same way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("DESK_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "DESK_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (sort arrows, menu checks): without them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            app::setup(&settings, cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1600.), px(960.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let desk = app::open(&settings, window, cx);
                // Closing the window ends the simulation and the desk.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                desk
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
