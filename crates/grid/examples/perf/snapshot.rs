//! `PERF_SNAPSHOT=<dir>`: the monitor drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! PERF_SNAPSHOT=target/perf cargo run --release -p gpui-kit-grid \
//!     --features test-support --example perf
//! ```
//!
//! The sampler reads this machine in real time, and the monitor is saved
//! after a few samples as a tree (`perf-tree.png`), a flat list
//! (`perf-flat.png`), grouped by user (`perf-user.png`) and by app
//! (`perf-app.png`), and paused a few samples later (`perf-paused.png`).
//! Each save prints the status line. The images show this machine's
//! processes and users: keep them out of the repository (`target/` is
//! ignored).

use std::{path::Path, time::Duration};

use gpui_kit::{AppContext as _, px, size};

use super::{Perf, Settings, table::Arrangement};
use crate::trading::snapshot::Snapshot;

const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time before the first image: a few samples.
const WARM_UP: Duration = Duration::from_secs(5);
/// Real time after a step, so the grid rebuilds.
const SETTLE: Duration = Duration::from_millis(1_500);
/// Real time the order stays paused.
const PAUSED: Duration = Duration::from_secs(3);

pub fn run(settings: &Settings, dir: &Path) {
    // The scenario decides when to quit.
    let settings = &Settings {
        quit_after: None,
        ..settings.clone()
    };
    let Some((mut snapshot, perf)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| cx.new(|cx| Perf::new(settings, window, cx)),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&perf, |perf, cx| perf.status_line(cx));
        out!("{name}: {line}");
    };

    snapshot.run_for(WARM_UP);
    save(&mut snapshot, "perf-tree");

    for (arrangement, name) in [
        (Arrangement::Flat, "perf-flat"),
        (Arrangement::User, "perf-user"),
        (Arrangement::App, "perf-app"),
    ] {
        snapshot.update(|cx| perf.update(cx, |perf, cx| perf.set_arrangement(arrangement, cx)));
        snapshot.run_for(SETTLE);
        save(&mut snapshot, name);
    }

    snapshot.update(|cx| {
        perf.update(cx, |perf, cx| {
            perf.set_arrangement(Arrangement::Flat, cx);
            perf.toggle_paused(cx);
        })
    });
    snapshot.run_for(PAUSED);
    save(&mut snapshot, "perf-paused");
}
