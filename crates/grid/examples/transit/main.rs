//! Munich by the timetable: every train, tram and bus that MVV's published
//! timetable says is under way right now, as a live table that updates
//! every second, with a map of where each one is. About 650 vehicles at
//! the morning rush, 60 at night; sorted by the next arrival, the order
//! changes all the time. Or Helsinki live: every tram, metro, commuter
//! train and ferry (and, as an option, bus) that HSL's vehicles report
//! once a second. Paper light, set in Barlow Semi Condensed and IBM Plex
//! Mono.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Barlow, IBM Plex Mono
//! python3 -I script/fetch-transit-data    # once: the timetable, compiled
//! cargo run --release -p gpui-kit-grid --example transit
//! TRANSIT_TIME=2026-11-10T07:45 cargo run --release -p gpui-kit-grid --example transit
//! # No window: the example drawn offscreen to PNG files (`snapshot.rs`)
//! TRANSIT_SNAPSHOT=target/transit cargo run --release -p gpui-kit-grid --features test-support --example transit
//! # Helsinki: the network once, then live (one connection to mqtt.hsl.fi)
//! python3 -I script/fetch-transit-data --helsinki
//! TRANSIT_CITY=helsinki cargo run --release -p gpui-kit-grid --example transit
//! ```
//!
//! **Scheduled, not real-time.** The data is MVV's GTFS feed of the whole
//! network ("Soll-Fahrplandaten MVV-Gesamtnetz", © Münchner Verkehrs- und
//! Tarifverbund GmbH (MVV), CC BY 4.0) on the City of Munich's open data
//! portal, downloaded once and compiled by `script/fetch-transit-data`
//! into `examples/data/` (not committed): the U-Bahn, the S-Bahn, the
//! trams and the city buses, and as an option the regional trains and
//! buses (a second compiled file). The positions are interpolated between
//! stations by time. Munich uses no live service: MVV's live departures
//! need MVG's written consent.
//!
//! **Helsinki is live** (`hsl.rs`): HSL's high-frequency positioning over
//! MQTT on a WebSocket, © HSL, CC BY 4.0, with the names of the lines and
//! stops and the map's network from HSL's GTFS feed, compiled by
//! `script/fetch-transit-data --helsinki`. One connection, the trams,
//! metro, trains and ferries only unless the buses are asked for, a lost
//! connection back after 16 s doubling to five minutes with jitter, and a
//! recording replayed with a notice when HSL stays out of reach. The clock
//! is the real one there: no warp, and no board (the board is by the
//! timetable).
//!
//! - **Table** (`table.rs`): one row per vehicle under way, keyed by its
//!   run (the timetable's trip on its service day), written by the
//!   producer (`producer.rs`) on its own thread: mode, line (a pill in the
//!   line's colour), destination, the leg (the station left and the next
//!   one), progress along it, the countdown to the next station and the
//!   stations left. Grouped by mode and line (each caption counts its
//!   vehicles; the Next column shows the soonest arrival), by mode, or
//!   flat. A new run flashes, a departure flashes its leg, an ended run
//!   fades out. A search box (the grid's quick filter).
//! - **Map** (`map.rs`): every vehicle over the network's links and
//!   stations, city or region; the selected run's stations and where it
//!   is. A click selects a vehicle.
//! - **Scheduled departures** (`board.rs`): a station's board, found by
//!   name or by a click on the map: the next hour's departures by the
//!   timetable, each with its line pill, destination, platform (where the
//!   feed names one), countdown, next stop and the minutes to it. It
//!   ticks every sweep, the time warp included, and a departure leaves the
//!   board as it departs. From an index of the patterns that call at each
//!   station, made at load: a board costs a few binary searches a sweep.
//!   No delays: the timetable only.
//! - **Status bar**: vehicles under way, trips today, the time warp, the
//!   sweep, write, rebuild and frame times, and the data's attribution.
//!
//! The model has no GPUI types: the timetable (`timetable.rs`), Munich's
//! days and clock (`zone.rs`), the schedule engine (`schedule.rs`) and the
//! source trait between it and the view (`source.rs`), with Helsinki's
//! live feed as the second source (`hsl.rs`, over the MQTT client, session
//! and sockets the examples share in `../mqtt/`). The example is `app.rs`,
//! its native sources and settings `native.rs`; this file opens it in a
//! window. The browser build (`crates/grid-web/apps/transit`, the
//! page `transit/`) runs the same `app.rs`.
//!
//! The keys are one table (`app.rs`, `KEYS`), which the bindings and the
//! app bar's hints are made from: W time ×1, ×10, ×60; G grouping (by
//! line, by mode, flat); Z the map's city or region; C the city (Munich,
//! Helsinki); O the city's more lines (Munich's regional ones, Helsinki's
//! buses); Cmd/Ctrl+F the search box (Enter there returns to the table).
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `TRANSIT_CITY` | `munich` | `helsinki`: HSL's live vehicles |
//! | `TRANSIT_REGIONAL` | off | `1`: Munich's regional trains and buses too |
//! | `TRANSIT_BUSES` | off | `1`: Helsinki's buses too |
//! | `TRANSIT_TIME` | now | A fixed start of the clock in Munich: `07:45` today, or `2026-11-10T07:45` |
//! | `TRANSIT_DATA` | `examples/data/mvv-transit.tsv` | Another compiled timetable |
//! | `TRANSIT_SOURCE` | `live` | `replay`: Helsinki from the recording |
//! | `TRANSIT_REPLAY` | `examples/data/hsl-recording.txt` | The recording to replay or fall back to; `TRANSIT_SPEED` how fast |
//! | `TRANSIT_RECORD` | none | Record Helsinki's live messages into this file |
//! | `TRANSIT_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`); `TRANSIT_SECONDS` runs Helsinki on |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules (`timetable.rs`, `map.rs`,
// ...) are its children in this folder; the browser build includes it the
// same way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::native::settings_from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("TRANSIT_SNAPSHOT") {
        let seconds = std::env::var("TRANSIT_SECONDS")
            .ok()
            .and_then(|seconds| seconds.parse::<u64>().ok());
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir), seconds);
        #[cfg(not(feature = "test-support"))]
        {
            let _ = (settings, seconds);
            err!(
                "TRANSIT_SNAPSHOT={}: needs --features test-support",
                dir.to_string_lossy()
            );
        }
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
                let transit = app::open(settings, window, cx);
                // Closing the window ends the producer and the example.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                transit
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
