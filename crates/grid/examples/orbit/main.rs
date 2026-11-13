//! Everything in orbit right now, as a live table: about 17,000 active
//! satellites from CelesTrak's public data, each propagated with SGP4 every
//! second and seen from one place on Earth. Sorted by elevation, the table
//! reorders on every update, so what is overhead stays on top; a rise or a
//! set flashes its row. Night red, set in Manrope and JetBrains Mono.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Manrope, JetBrains Mono
//! python3 -I script/fetch-orbit-data      # the data, at most every two hours
//! cargo run --release -p gpui-kit-grid --example orbit
//! ORBIT_SITE=35.6586,139.7454,40 cargo run --release -p gpui-kit-grid --example orbit
//! # No window: the example drawn offscreen to PNG files (`snapshot.rs`)
//! ORBIT_SNAPSHOT=target/orbit cargo run --release -p gpui-kit-grid --features test-support --example orbit
//! ```
//!
//! **Nothing is simulated** but the optional time warp: the orbital
//! elements are CelesTrak's "active satellites" group (OMM JSON, derived
//! from the US Space Force's public catalog), downloaded by
//! `script/fetch-orbit-data` into `examples/data/` (not committed) at most
//! once per CelesTrak update; the positions are SGP4's (`sgp4` crate) for
//! the real UTC clock, approximate as public elements are.
//!
//! - **Sky table** (`table.rs`): one row per object keyed by its NORAD
//!   number, written every second by the producer (`producer.rs`) on its
//!   own thread: azimuth, elevation, range, range rate (its arrow points
//!   down while the object approaches), altitude, speed, sunlit or in the
//!   Earth's shadow, up or down. Flat, grouped by constellation (each
//!   caption counts the objects, those up and the nearest), or a tree of
//!   constellations, orbital shells and objects with the producer's
//!   roll-ups. A search box (the grid's quick filter) and Above horizon.
//!   The ISS and Tiangong are pinned on top.
//! - **Sky plot** (`plot.rs`): the objects above the horizon by azimuth
//!   and elevation, the selected one ringed with its next pass.
//! - **Object** (`detail.rs`): the selected object now, its mean elements
//!   and its next pass (rise, highest point, set), found by search.
//! - **Status bar**: objects, objects up, updates a second, the sweep's
//!   propagation time, the last rebuild and frame, rises and sets in the
//!   last sweep, and the data's age.
//!
//! The model (`sky.rs`) has no GPUI types. The example is `app.rs`; this
//! file opens it in a window. The browser build
//! (`crates/grid-web/apps/orbit`, the page `orbit/`) runs the same `app.rs`.
//!
//! The keys are one table (`app.rs`, `KEYS`), which the bindings and the
//! app bar's hints are made from: W time ×1, ×10, ×60; H above the horizon
//! only; G grouping; T tree; N the next pass after the one shown;
//! Cmd/Ctrl+F the search box (Enter there returns to the table).
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `ORBIT_SITE` | `51.4769,-0.0005,46` | The observer: latitude, longitude (degrees, north and east positive), altitude in metres; the Royal Observatory, Greenwich |
//! | `ORBIT_TIME` | now | A fixed start of the clock, UTC (`2026-11-06T17:30:00Z`) |
//! | `ORBIT_DATA` | `examples/data/celestrak-active.json` | Another OMM JSON file |
//! | `ORBIT_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules (`sky.rs`, `table.rs`, ...)
// are its children in this folder; the browser build includes it the same
// way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("ORBIT_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "ORBIT_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons (search, sort arrows, chevrons): without them
        // icons are blank.
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
                let orbit = app::open(settings, window, cx);
                // Closing the window ends the producer and the example.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                orbit
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
