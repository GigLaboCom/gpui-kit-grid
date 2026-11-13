//! `ORBIT_SNAPSHOT=<dir>`: the example drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`, from the real data file:
//!
//! ```sh
//! ORBIT_SNAPSHOT=target/orbit cargo run --release -p gpui-kit-grid \
//!     --features test-support --example orbit
//! ```
//!
//! The clock starts at a fixed time (`ORBIT_TIME`, else 2026-11-06 17:30
//! UTC: dusk at Greenwich, so the plot has both sunlit objects and objects
//! in the Earth's shadow) and runs in real time. Saved as the keys would
//! leave the window: the flat table by elevation with the ISS selected
//! (`orbit-sky.png`); grouped by constellation, above the horizon only
//! (`orbit-grouped.png`); the tree (`orbit-tree.png`); and time ×60 with
//! the lowest objects above the horizon first, where rises flash
//! (`orbit-rising.png`). Each save prints the status line. The window is
//! 1600×960.

use std::{path::Path, time::Duration};

use gpui_kit::{App, AppContext as _, Window, px, size};
use gpui_kit_grid::SortDirection;

use super::{Orbit, Settings, producer::Warp, rows::SkyColumn, sky, table::Arrangement};
use crate::trading::snapshot::Snapshot;

/// The start unless `ORBIT_TIME` says otherwise.
const START: &str = "2026-11-06T17:30:00Z";
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time before the first image: the data parsed and a few sweeps.
const WARM_UP: Duration = Duration::from_millis(3_500);
/// Real time after a step, so the table rebuilds.
const SETTLE: Duration = Duration::from_millis(2_200);
/// Real time at ×60: a few minutes of the sky.
const WARPED: Duration = Duration::from_millis(4_500);

pub fn run(settings: Settings, dir: &Path) {
    let settings = match settings.start_ms {
        Some(_) => settings,
        None => settings.with_start(sky::parse_utc(START).expect("a fixed time")),
    };
    let Some((mut snapshot, orbit)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| super::open(settings, window, cx),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&orbit, |orbit, cx| orbit.status_line(cx));
        out!("{name}: {line}");
    };
    let update =
        |snapshot: &mut Snapshot,
         change: &dyn Fn(&mut Orbit, &mut Window, &mut gpui_kit::Context<Orbit>)| {
            snapshot.update(|cx| {
                in_window(cx, |window, cx| {
                    orbit.update(cx, |orbit, cx| change(orbit, window, cx));
                })
            });
        };

    snapshot.run_for(WARM_UP);
    save(&mut snapshot, "orbit-sky");

    update(&mut snapshot, &|orbit, _, cx| {
        orbit.set_arrangement(Arrangement::Grouped, cx);
        orbit.set_up_only(true, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "orbit-grouped");

    update(&mut snapshot, &|orbit, _, cx| {
        orbit.set_up_only(false, cx);
        orbit.set_arrangement(Arrangement::Tree, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "orbit-tree");

    update(&mut snapshot, &|orbit, _, cx| {
        orbit.set_arrangement(Arrangement::Flat, cx);
        orbit.set_up_only(true, cx);
        orbit.set_sort(SkyColumn::Elevation, SortDirection::Ascending, cx);
        orbit.set_warp(Warp::Sixty, cx);
    });
    snapshot.run_for(WARPED);
    save(&mut snapshot, "orbit-rising");
}

/// Runs `update` in the snapshot's one window.
fn in_window(cx: &mut App, update: impl FnOnce(&mut Window, &mut App)) {
    let Some(&handle) = cx.windows().first() else {
        err!("snapshot: no window");
        return;
    };
    if let Err(error) = cx.update_window(handle, |_, window, cx| update(window, cx)) {
        err!("snapshot: {error}");
    }
}
