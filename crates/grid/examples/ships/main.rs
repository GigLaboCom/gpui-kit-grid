//! Every vessel around Finland, live from AIS: Digitraffic's open marine
//! data (Fintraffic, CC BY 4.0, no key). A row a vessel, keyed by its
//! MMSI, updated in place as its messages arrive (a few dozen a second
//! over all of them); the table sorted by speed, so the order moves; a map
//! drawn by the vessels themselves. Night bridge, set in Barlow Semi
//! Condensed and IBM Plex Mono.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Barlow, IBM Plex Mono
//! cargo run --release -p gpui-kit-grid --example ships
//! # Record a few minutes for offline runs (gitignored, examples/data/)
//! SHIPS_RECORD=crates/grid/examples/data/ais-recording.txt cargo run --release -p gpui-kit-grid --example ships
//! SHIPS_SOURCE=replay cargo run --release -p gpui-kit-grid --example ships
//! # No window: the example drawn offscreen to PNG files (`snapshot.rs`)
//! SHIPS_SNAPSHOT=target/ships SHIPS_SOURCE=replay cargo run --release -p gpui-kit-grid --features test-support --example ships
//! ```
//!
//! **Nothing is simulated.** The latest locations and metadata come once
//! from Digitraffic's REST API at the start, then every message over MQTT
//! on a WebSocket (`wss://meri.digitraffic.fi:443/mqtt`). Digitraffic
//! passes on class A transponders only, without fishing vessels; leisure
//! craft with class A transponders are among them. The example shows what
//! they publish (name, type, MMSI, call sign, destination, ETA, speed,
//! course, heading, status) and links no vessel to a person. Source:
//! Fintraffic / digitraffic.fi, license CC 4.0 BY.
//!
//! - **Vessels** (`table.rs`, `rows.rs`): flat, or grouped by type or by
//!   status with each caption's moving and berthed vessels and their
//!   average speed; sorted by speed; a search box (the grid's quick
//!   filter) over name, destination, call sign and MMSI; Moving only.
//!   Changes flash, the speed with an arrow; a vessel silent for ten
//!   minutes dims, and after thirty fades out (`fleet.rs`). A speed the
//!   vessel's type cannot make (over 40 knots for a cargo ship, a tanker,
//!   a tug or a passenger ship, over 70 for high-speed craft) shows as an
//!   unknown, dimmed `—`, sorts with the unknown and stays out of the
//!   averages (`speed.rs`, the `SpeedRule` seam).
//! - **Sea** (`map.rs`): every vessel at its position, an arrow along its
//!   heading while it moves, with its wake; the selected one ringed. A
//!   click selects the nearest vessel. Three areas.
//! - **Feed** (`rate.rs`): messages a second over two minutes, and the
//!   connection.
//! - **Vessel** (`detail.rs`): the selected vessel as it reports itself.
//! - **Status bar**: vessels, messages a second, the connection, the
//!   grid's rebuild and frame times, and the attribution.
//!
//! How it is built: `../mqtt/`, shared with the transit example, is MQTT
//! 3.1.1 without IO, the session that keeps a subscription up with its
//! reconnect backoff, and the sockets (rustls and tungstenite natively, the
//! page's WebSocket in the browser); `live.rs` the live source over a
//! `Connector` (natively `native.rs`, in the browser
//! `crates/grid-web/apps/ships`); `source.rs` the `VesselSource` seam and
//! the recording a `ReplaySource` plays; `driver.rs` polls the source and
//! writes the store. The model (`ais.rs`, `fleet.rs`) has no GPUI types.
//! The example is `app.rs`; this file opens it in a window.
//!
//! Gentle with Digitraffic: one connection; a lost one comes back after
//! 16 s, doubling to five minutes, with jitter, never two starts within
//! 16 s; the REST snapshot once, and again only after an outage of ten
//! minutes; the `Digitraffic-User` header on every request. If Digitraffic
//! cannot be reached at all, or its MQTT feed stays out of reach after
//! three connections or 90 s with nothing heard although the snapshot
//! came, the recording replays, with a notice.
//!
//! The keys are one table (`app.rs`, `KEYS`): G grouping, M moving only, A
//! the map's area, Cmd/Ctrl+F the search box (Enter there returns to the
//! table).
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `SHIPS_SOURCE` | `live` | `replay`: play the recording instead |
//! | `SHIPS_REPLAY` | `examples/data/ais-recording.txt` | The recording to replay or fall back to |
//! | `SHIPS_SPEED` | `1` | How fast a replay runs |
//! | `SHIPS_RECORD` | none | Record the live messages into this file |
//! | `SHIPS_SECONDS` | none | With `SHIPS_SNAPSHOT`: run on until this many seconds, printing the status |
//! | `SHIPS_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules (`live.rs`, `fleet.rs`, ...)
// are its children in this folder; the browser build includes it the same
// way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::native::settings_from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("SHIPS_SNAPSHOT") {
        let seconds = std::env::var("SHIPS_SECONDS")
            .ok()
            .and_then(|seconds| seconds.parse().ok());
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir), seconds);
        #[cfg(not(feature = "test-support"))]
        {
            let _ = (settings, seconds);
            err!(
                "SHIPS_SNAPSHOT={}: needs --features test-support",
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
                let ships = app::open(settings, window, cx);
                // Closing the window ends the driver and the example.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                ships
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
