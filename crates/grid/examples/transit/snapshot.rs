//! `TRANSIT_SNAPSHOT=<dir>`: the example drawn offscreen into PNG files
//! with the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`, from the compiled timetable:
//!
//! ```sh
//! TRANSIT_SNAPSHOT=target/transit cargo run --release -p gpui-kit-grid \
//!     --features test-support --example transit
//! ```
//!
//! The clock starts at the morning rush of a Tuesday (`TRANSIT_TIME`, else
//! 2026-11-10 07:45 in Munich) and runs in real time. Saved as the keys
//! would leave the window: grouped by line (`transit-rush.png`); the same
//! with Marienplatz's board of scheduled departures (`transit-board.png`);
//! flat with
//! the U-Bahn train with the most stations ahead selected, its run on the
//! map (`transit-selected.png`);
//! grouped by mode with the whole region on the map at time ×60, where
//! starts and departures flash (`transit-region.png`); and a Sunday night,
//! 02:30, grouped by line (`transit-night.png`). Each save prints the
//! status line, and the rush and the night each measure 120 frames. The
//! window is 1600×960.
//!
//! With `TRANSIT_CITY=helsinki` (and `TRANSIT_SOURCE=replay`, so no
//! connection is made) it draws Helsinki instead: grouped by line
//! (`transit-helsinki.png`, 120 frames measured), then flat by delay with
//! the latest tram followed (`transit-helsinki-selected.png`); with
//! `TRANSIT_SECONDS=<n>` it runs on until `n` seconds after the start,
//! printing the status every ten (`transit-helsinki-end.png`). With
//! `TRANSIT_REGIONAL=1`, Munich's region with its regional lines
//! (`transit-regional.png`) comes after the rush.

use std::time::Duration;

use gpui_kit::{App, AppContext as _, Window, px, size};

use super::{
    Settings, Transit,
    map::MapZoom,
    producer::Warp,
    source::{City, Mode},
    table::Arrangement,
    zone::{self, Day},
};
use crate::trading::snapshot::Snapshot;

/// The start unless `TRANSIT_TIME` says otherwise: a Tuesday's rush.
const START: &str = "2026-11-10T07:45";
/// A Sunday night.
const NIGHT: &str = "2026-11-08T02:30";
/// The station whose board is drawn.
const BOARD_STATION: &str = "Marienplatz";
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time before the first image: the timetable parsed and a few
/// sweeps.
const WARM_UP: Duration = Duration::from_millis(3_500);
/// Real time after a step, so the table rebuilds and a sweep runs.
const SETTLE: Duration = Duration::from_millis(2_200);
/// Real time at ×60: a few minutes of the city.
const WARPED: Duration = Duration::from_millis(3_000);
/// Frames each measure paints.
const FRAMES: usize = 120;
/// Real time before Helsinki's first image: a recording's first seconds,
/// or a connection and its first messages.
const HELSINKI_WARM_UP: Duration = Duration::from_millis(6_000);

fn munich(text: &str) -> f64 {
    let day = Day::from_ymd(2026, 11, 10);
    zone::parse_munich(text, day).expect("a fixed time")
}

pub fn run(settings: Settings, dir: &std::path::Path, seconds: Option<u64>) {
    if settings.choice().city() == City::Helsinki {
        return run_helsinki(settings, dir, seconds);
    }
    let regional = settings.choice().is_regional();
    let settings = match settings.start_ms() {
        Some(_) => settings,
        None => settings.with_start(munich(START)),
    };
    let Some((mut snapshot, transit)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| super::open(settings, window, cx),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&transit, |transit, cx| transit.status_line(cx));
        out!("{name}: {line}");
    };
    let update =
        |snapshot: &mut Snapshot,
         change: &dyn Fn(&mut Transit, &mut Window, &mut gpui_kit::Context<Transit>)| {
            snapshot.update(|cx| {
                in_window(cx, |window, cx| {
                    transit.update(cx, |transit, cx| change(transit, window, cx));
                })
            });
        };
    let hosts = |cx: &App| transit.read(cx).grid().read(cx).stats().cell_hosts();

    snapshot.run_for(WARM_UP);
    save(&mut snapshot, "transit-rush");
    snapshot.measure("rush", FRAMES, hosts);

    update(&mut snapshot, &|transit, window, cx| {
        let station = transit.find_station(BOARD_STATION);
        transit.set_station(station, window, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "transit-board");
    let line = snapshot.read(&transit, |transit, cx| transit.board_line(cx));
    out!("transit-board: {line}");

    if regional {
        update(&mut snapshot, &|transit, _, cx| {
            transit.set_arrangement(Arrangement::ByMode, cx);
            transit.set_zoom(MapZoom::Region, cx);
        });
        snapshot.run_for(SETTLE);
        save(&mut snapshot, "transit-regional");
        snapshot.measure("regional", FRAMES, hosts);
        update(&mut snapshot, &|transit, _, cx| {
            transit.set_arrangement(Arrangement::ByLine, cx);
            transit.set_zoom(MapZoom::City, cx);
        });
    }

    update(&mut snapshot, &|transit, _, cx| {
        transit.set_arrangement(Arrangement::Flat, cx);
        if let Some(key) = transit.longest_run(Mode::UBahn, cx) {
            transit.select(key, cx);
        }
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "transit-selected");

    update(&mut snapshot, &|transit, _, cx| {
        transit.set_arrangement(Arrangement::ByMode, cx);
        transit.set_zoom(MapZoom::Region, cx);
        transit.set_warp(Warp::Sixty, cx);
    });
    snapshot.run_for(WARPED);
    save(&mut snapshot, "transit-region");

    update(&mut snapshot, &|transit, _, cx| {
        transit.set_warp(Warp::RealTime, cx);
        transit.control().jump_to(munich(NIGHT));
        transit.set_arrangement(Arrangement::ByLine, cx);
        transit.set_zoom(MapZoom::City, cx);
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "transit-night");
    snapshot.measure("night", FRAMES, hosts);
}

/// Helsinki: what the feed (or the recording) brings in the first seconds,
/// grouped by line, then flat by delay with the latest vehicle followed.
fn run_helsinki(settings: Settings, dir: &std::path::Path, seconds: Option<u64>) {
    let started = std::time::Instant::now();
    let Some((mut snapshot, transit)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| super::open(settings, window, cx),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&transit, |transit, cx| transit.status_line(cx));
        out!("{name}: {line}");
    };
    let hosts = |cx: &App| transit.read(cx).grid().read(cx).stats().cell_hosts();
    snapshot.run_for(HELSINKI_WARM_UP);
    save(&mut snapshot, "transit-helsinki");
    snapshot.measure("helsinki", FRAMES, hosts);
    snapshot.update(|cx| {
        in_window(cx, |_, cx| {
            transit.update(cx, |transit, cx| {
                transit.set_arrangement(Arrangement::Flat, cx);
                if let Some(key) = transit.latest(cx) {
                    transit.select(key, cx);
                }
            });
        })
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "transit-helsinki-selected");
    let Some(seconds) = seconds else {
        return;
    };
    // On until `seconds` after the start, the status every ten seconds:
    // a live session's numbers, and its recording with `TRANSIT_RECORD`.
    let every = Duration::from_secs(10);
    let mut next = started.elapsed().div_duration_f64(every).ceil() as u32 * every;
    while started.elapsed() < Duration::from_secs(seconds) {
        snapshot.run_for(Duration::from_millis(500));
        if started.elapsed() >= next {
            next += every;
            let line = snapshot.read(&transit, |transit, cx| transit.status_line(cx));
            out!("{:>4} s: {line}", started.elapsed().as_secs());
        }
    }
    save(&mut snapshot, "transit-helsinki-end");
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
