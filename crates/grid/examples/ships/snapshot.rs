//! `SHIPS_SNAPSHOT=<dir>`: the example drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`, from the source the environment names: a
//! recording with `SHIPS_SOURCE=replay`, else Digitraffic live.
//!
//! ```sh
//! SHIPS_SNAPSHOT=target/ships SHIPS_SOURCE=replay cargo run --release \
//!     -p gpui-kit-grid --features test-support --example ships
//! ```
//!
//! Saved as the keys would leave the window: the flat table by speed with
//! the fastest vessel selected (`ships-flat.png`), grouped by type
//! (`ships-by-type.png`), grouped by status with only the moving vessels
//! and the map on the Gulf of Finland (`ships-by-status.png`), and a search
//! for Helsinki among the destinations, with Åland on the map
//! (`ships-search.png`). Each save prints the status line, and a measure of
//! forced frames follows the first. With `SHIPS_SECONDS=<n>` it then runs
//! on until `n` seconds after the start, printing the status every five
//! (a live run that records with `SHIPS_RECORD`), and saves
//! `ships-end.png`. The window is 1600×960.

use std::time::{Duration, Instant};

use gpui_kit::{App, AppContext as _, Window, px, size};

use super::{Settings, Ships, table::Grouping};
use crate::trading::snapshot::Snapshot;

const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// The longest wait for the first rows: a live start fetches the snapshot
/// first.
const FIRST_ROWS: Duration = Duration::from_secs(30);
/// Real time after the first rows, and after a step, so the table settles.
const WARM_UP: Duration = Duration::from_millis(4_000);
const SETTLE: Duration = Duration::from_millis(1_500);

pub fn run(settings: Settings, dir: &std::path::Path, seconds: Option<u64>) {
    let started = Instant::now();
    let Some((mut snapshot, ships)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| super::open(settings, window, cx),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&ships, |ships, cx| ships.status_line(cx));
        out!("{name}: {line}");
    };
    let update =
        |snapshot: &mut Snapshot,
         change: &dyn Fn(&mut Ships, &mut Window, &mut gpui_kit::Context<Ships>)| {
            snapshot.update(|cx| {
                in_window(cx, |window, cx| {
                    ships.update(cx, |ships, cx| change(ships, window, cx));
                })
            });
        };

    while started.elapsed() < FIRST_ROWS {
        snapshot.run_for(Duration::from_millis(250));
        let rows = snapshot.read(&ships, |ships, cx| {
            ships.grid().read(cx).projection().row_count()
        });
        if rows > 0 {
            out!(
                "ships: first rows after {:.1} s",
                started.elapsed().as_secs_f64()
            );
            break;
        }
    }
    snapshot.run_for(WARM_UP);
    save(&mut snapshot, "ships-flat");
    snapshot.measure("flat", 120, |_| 0);

    update(&mut snapshot, &|ships, _, cx| {
        ships.set_grouping(Grouping::Category, cx)
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "ships-by-type");
    snapshot.measure("by-type", 60, |_| 0);

    update(&mut snapshot, &|ships, _, cx| {
        ships.set_grouping(Grouping::Status, cx);
        ships.set_moving_only(true, cx);
        ships.set_area(1, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "ships-by-status");

    update(&mut snapshot, &|ships, window, cx| {
        ships.set_grouping(Grouping::None, cx);
        ships.set_moving_only(false, cx);
        ships.set_area(2, cx);
        ships.type_search("helsinki", window, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "ships-search");

    let Some(seconds) = seconds else {
        return;
    };
    update(&mut snapshot, &|ships, window, cx| {
        ships.type_search("", window, cx);
        ships.set_area(0, cx);
    });
    let until = Duration::from_secs(seconds);
    let every = Duration::from_secs(5);
    let mut next = started.elapsed().div_duration_f64(every).ceil() as u32 * every;
    while started.elapsed() < until {
        snapshot.run_for(Duration::from_millis(500));
        if started.elapsed() >= next {
            next += every;
            let line = snapshot.read(&ships, |ships, cx| ships.status_line(cx));
            out!("{:>4} s: {line}", started.elapsed().as_secs());
        }
    }
    save(&mut snapshot, "ships-end");
    snapshot.measure("end", 120, |_| 0);
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
