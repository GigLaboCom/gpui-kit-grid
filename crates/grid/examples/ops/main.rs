//! A data center operations console during an incident storm, outside
//! finance: a simulated fleet of about six thousand hosts, its services
//! and their alerts. A storm of over a thousand alert updates a second
//! collapses into one keyed row per alert: the simulator counts each
//! alert's firings and their rate, and the grid keeps the latest row of
//! each key. The fleet updates live in a tree. Graphite and violet, set in
//! Barlow Semi Condensed and Iosevka.
//!
//! ```sh
//! python3 -I script/fetch-example-fonts   # once: Barlow Semi Condensed, Iosevka
//! cargo run --release -p gpui-kit-grid --example ops
//! # No window: the console drawn offscreen to PNG files (`snapshot.rs`)
//! OPS_SNAPSHOT=target/ops cargo run --release -p gpui-kit-grid --features test-support --example ops
//! ```
//!
//! **Everything is simulated** (`sim.rs`), and every name is generic: the
//! regions, data centers, halls, racks, hosts and services are made up.
//!
//! - **Alerts** (`alerts.rs`): one row per alert key (rule and host, rack,
//!   hall or service), upserted from the simulator's thread on every
//!   firing with the count and the rate (firings a second over the last
//!   10 s) the simulator keeps, so both climb in place. Grouped by service
//!   or by data center, each caption counting its active alerts by
//!   severity, the groups with the most critical alerts first; within a
//!   group the worst, then the fastest firing first, so a storm's density
//!   reads as a sorted rate rather than as flashing. Severity chips and a
//!   search box (the grid's quick filter); Enter, `A`, a double click or
//!   the menu's first item acknowledges the selection, or on a caption the
//!   whole group. F9 pauses the order: values update in place, rows stop
//!   moving. Resolved alerts linger, then fade out.
//! - **Fleet** (`fleet.rs`): regions → data centers → halls → racks →
//!   hosts as a tree, the roll-ups on every branch, heat-coloured
//!   temperatures and CPU, the hosts' totals at the bottom. When a storm
//!   starts, the tree opens on its hall.
//! - **Service health** (`services.rs`): SLO, error budget left, p99 and
//!   errors, the three lowest budgets pinned on top.
//! - **Chart** (`chart.rs`): alert updates per second by severity.
//! - **Status bar**: alert updates a second and alert keys (both from the
//!   simulator), rows, rebuilds a second with the last rebuild and frame
//!   times, and the storm.
//!
//! The keys are one table (`app.rs`, `KEYS`), which the bindings and the
//! status bar's hints are made from: `secondary` is Cmd on macOS and Ctrl
//! elsewhere.
//!
//! The example is `app.rs`; this file opens it in a window. The browser
//! build (`crates/grid-web/apps/ops`, the page `ops/`) opens the same code
//! in the page's canvas, the simulator stepping on the executor's timer
//! instead of a thread of its own.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `OPS_HOSTS` | 5760 | Hosts in the fleet, rounded to whole racks of 20 |
//! | `OPS_SEED` | none | A fixed seed: the same fleet and storm every run |
//! | `OPS_STORM_SECONDS` | 45 | How long a storm lasts before the feed comes back on its own |
//! | `OPS_LAYOUT` | none | A file the alert console's layout is restored from and saved to |
//! | `OPS_SECONDS` | none | Print the status every second and quit after N seconds |
//! | `OPS_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`snapshot.rs`) |

// The unit tests (`test = true`) do not run `main`, which reaches the rest.
#![cfg_attr(test, allow(dead_code))]

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

use gpui_kit::{Bounds, WindowBounds, WindowOptions, px, size};

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../trading/mod.rs"]
mod trading;

// By path, so that the example's own modules (`sim.rs`, `alerts.rs`, ...)
// are its children in this folder; the browser build includes it the same
// way.
#[path = "app.rs"]
mod app;

fn main() {
    let settings = app::native::settings_from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("OPS_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        app::snapshot::run(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "OPS_SNAPSHOT={}: needs --features test-support",
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
                let ops = app::open(&settings, window, cx);
                // Closing the window ends the simulation and the console.
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                ops
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
