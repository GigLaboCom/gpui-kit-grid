//! `WIRE_SNAPSHOT=<dir>`: the wire drawn offscreen into PNG files with the
//! trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! WIRE_SNAPSHOT=target/wire cargo run --release -p gpui-kit-grid \
//!     --features test-support --example wire
//! ```
//!
//! The simulator and the newsroom run for a few seconds in real time with
//! `WIRE_SEED` (7 unless set), then the wire is saved as the keys and the
//! controls would leave it: the News tab with the newest story open
//! (`wire-news.png`); two stories further down, read, with the Equities
//! chip alone (`wire-equities.png`); a search (`wire-search.png`); every
//! story marked read (`wire-read.png`); the Markets and Watchlist tabs
//! (`wire-markets.png`, `wire-watchlist.png`). The window is 1600×960.

use std::{path::Path, time::Duration};

use gpui_kit::{App, AppContext as _, SharedString, Window, px, size};

use crate::{
    Section, Settings, Wire, news::Categories, newsroom::Category, trading::snapshot::Snapshot,
};

/// The seed unless `WIRE_SEED` says otherwise: the same market every run.
const SEED: u64 = 7;
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time the simulator and the newsroom run before the first image:
/// the backlog, the newest story open, a live story or two.
const WARM_UP: Duration = Duration::from_secs(6);
/// Real time after a step, so the grids rebuild and the chart draws in.
const SETTLE: Duration = Duration::from_millis(1_500);
/// What the search step types.
const SEARCH: &str = "volume";

pub fn run(mut settings: Settings, dir: &Path) {
    settings.seed = settings.seed.or(Some(SEED));
    settings.quit_after = None;

    let Some((mut snapshot, wire)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        crate::configure,
        |window, cx| cx.new(|cx| Wire::new(&settings, window, cx)),
    ) else {
        return;
    };
    let step = |snapshot: &mut Snapshot,
                name: &str,
                act: &dyn Fn(&mut Wire, &mut Window, &mut gpui_kit::Context<Wire>)| {
        snapshot.update(|cx| {
            in_window(cx, |window, cx| {
                wire.update(cx, |wire, cx| act(wire, window, cx));
            })
        });
        snapshot.run_for(SETTLE);
        snapshot.save(name);
    };

    snapshot.run_for(WARM_UP);
    snapshot.save("wire-news");
    step(&mut snapshot, "wire-equities", &|wire, window, cx| {
        wire.open_adjacent(1, window, cx);
        wire.open_adjacent(1, window, cx);
        wire.news.update(cx, |monitor, cx| {
            monitor.set_categories(Categories::ALL.toggled(Category::Equities), cx)
        });
    });
    step(&mut snapshot, "wire-search", &|wire, window, cx| {
        wire.news.update(cx, |monitor, cx| {
            monitor.set_categories(Categories::ALL, cx)
        });
        // A typed search: the box shows it, the monitor filters by it.
        wire.search
            .update(cx, |input, cx| input.set_value(SEARCH, window, cx));
        wire.news.update(cx, |monitor, cx| {
            monitor.set_search(SharedString::from(SEARCH), cx)
        });
    });
    step(&mut snapshot, "wire-read", &|wire, window, cx| {
        wire.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        wire.news
            .update(cx, |monitor, cx| monitor.set_search("".into(), cx));
        wire.news
            .update(cx, |monitor, cx| monitor.mark_all_read(cx));
    });
    step(&mut snapshot, "wire-markets", &|wire, window, cx| {
        wire.show(Section::Markets, window, cx);
    });
    step(&mut snapshot, "wire-watchlist", &|wire, window, cx| {
        wire.show(Section::Watchlist, window, cx);
    });
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
