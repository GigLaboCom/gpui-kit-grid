//! `WIKI_SNAPSHOT=<dir>`: the example drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`; no window opens.
//!
//! From a recording (`WIKI_REPLAY`, the default one otherwise), at ×4:
//! the start screen (`wiki-start.png`), the wikis by family
//! (`wiki-families.png`), flat with the busiest selected (`wiki-flat.png`),
//! a search (`wiki-search.png`) and the page at a phone's width
//! (`wiki-narrow.png`). With `WIKI_SOURCE=live` the same from the live
//! stream instead, one connection for `WIKI_SECONDS` (40 by default), into
//! `wiki-live-*.png`. Each save prints the status line, and a measured run
//! prints the frame times.

use std::{path::Path, time::Duration};

use gpui_kit::{App, AppContext as _, Window, px, size};

use super::{Settings, Start, Wiki, table::Arrangement};
use crate::trading::snapshot::Snapshot;

const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// A phone's viewport.
const NARROW: (f32, f32) = (390., 844.);
/// Real time at ×4 before the first image of a replay: a few minutes of
/// the recording.
const WARM_UP: Duration = Duration::from_secs(10);
/// Real time after a step, so the table rebuilds.
const SETTLE: Duration = Duration::from_millis(2_000);
/// Frames measured.
const MEASURED: usize = 120;

pub fn run(settings: impl Fn() -> Settings, live_seconds: Option<u64>, dir: &Path) {
    let live = live_seconds.is_some();
    let prefix = if live { "wiki-live" } else { "wiki" };
    let start = if live { Start::Live } else { Start::Idle };
    let Some((mut snapshot, wiki)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| super::open(settings_for(&settings, start), window, cx),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(&format!("{prefix}-{name}"));
        let line = snapshot.read(&wiki, |wiki, cx| wiki.status_line(cx));
        out!("{prefix}-{name}: {line}");
    };
    let update =
        |snapshot: &mut Snapshot,
         change: &dyn Fn(&mut Wiki, &mut Window, &mut gpui_kit::Context<Wiki>)| {
            snapshot.update(|cx| {
                in_window(cx, |window, cx| {
                    wiki.update(cx, |wiki, cx| change(wiki, window, cx));
                })
            });
        };

    match live_seconds {
        Some(seconds) => snapshot.run_for(Duration::from_secs(seconds.max(5))),
        None => {
            snapshot.run_for(Duration::from_millis(500));
            save(&mut snapshot, "start");
            if !snapshot.read(&wiki, |wiki, _| wiki.has_recording()) {
                err!("snapshot: no recording; record one with WIKI_RECORD=<seconds>");
                return;
            }
            update(&mut snapshot, &|wiki, _, cx| {
                wiki.set_speed(4, cx);
                wiki.start_replay(false, cx);
            });
            snapshot.run_for(WARM_UP);
        }
    }
    save(&mut snapshot, "families");

    update(&mut snapshot, &|wiki, _, cx| {
        wiki.set_arrangement(Arrangement::Flat, cx)
    });
    snapshot.run_for(SETTLE);
    update(&mut snapshot, &|wiki, _, cx| {
        if let Some(first) = wiki.first_shown(cx) {
            wiki.select(&first, cx);
        }
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "flat");
    snapshot.measure(&format!("{prefix} flat"), MEASURED, |cx| {
        wiki.read(cx).grid().read(cx).stats().cell_hosts()
    });
    let stats = snapshot.read(&wiki, |wiki, cx| {
        let stats = wiki.grid().read(cx).stats();
        (stats.frames(), stats.rebuilds(), stats.max_rebuild())
    });
    out!(
        "{prefix}: {} grid frames, {} rebuilds, max rebuild {:.2} ms",
        stats.0,
        stats.1,
        stats.2.as_secs_f64() * 1e3
    );

    update(&mut snapshot, &|wiki, window, cx| {
        wiki.type_search("de", window, cx)
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "search");
    drop(snapshot);

    if live {
        return;
    }
    let Some((mut narrow, wiki)) = Snapshot::open(
        dir,
        size(px(NARROW.0), px(NARROW.1)),
        super::configure,
        |window, cx| {
            super::open(
                settings_for(&settings, Start::Replay).with_speed(4),
                window,
                cx,
            )
        },
    ) else {
        return;
    };
    narrow.run_for(WARM_UP);
    narrow.save(&format!("{prefix}-narrow"));
    let line = narrow.read(&wiki, |wiki, cx| wiki.status_line(cx));
    out!("{prefix}-narrow: {line}");
}

fn settings_for(settings: &impl Fn() -> Settings, start: Start) -> Settings {
    let mut settings = settings();
    settings.start = start;
    settings
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
