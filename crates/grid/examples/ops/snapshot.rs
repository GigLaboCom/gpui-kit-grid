//! `OPS_SNAPSHOT=<dir>`: the console drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! OPS_SNAPSHOT=target/ops cargo run --release -p gpui-kit-grid \
//!     --features test-support --example ops
//! ```
//!
//! The simulator runs in real time with `OPS_SEED` (7 unless set), and the
//! console is saved as the keys would leave it: the quiet baseline
//! (`ops-quiet.png`); a storm well under way (`ops-storm.png`); the same
//! storm a few seconds after the order is paused, the values updated in
//! place and the rows where they were (`ops-storm-paused.png`); the storm
//! grouped by data center with only critical and major alerts and a search
//! for the hall (`ops-storm-dc.png`); and a few seconds after the feed is
//! restored, the alerts resolving and fading (`ops-recovery.png`).
//! Each save prints the status line. The window is 1600×960.

use std::{path::Path, time::Duration};

use gpui_kit::{App, AppContext as _, Window, px, size};

use super::{Ops, Settings, alerts::Grouping, rows::Severity};
use crate::trading::snapshot::Snapshot;

/// The seed unless `OPS_SEED` says otherwise: the same fleet and storm
/// every run.
const SEED: u64 = 7;
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time before the first image: the warm-up and the first load.
const WARM_UP: Duration = Duration::from_secs(5);
/// Real time into the storm: the hall hot, hosts down, services hurt.
const STORM: Duration = Duration::from_secs(16);
/// Real time after a step, so the grids rebuild.
const SETTLE: Duration = Duration::from_millis(1_500);
/// Real time after the feed comes back.
const RECOVERY: Duration = Duration::from_secs(9);
/// Real time the order stays paused.
const PAUSED: Duration = Duration::from_secs(3);

pub fn run(mut settings: Settings, dir: &Path) {
    settings.seed = settings.seed.or(Some(SEED));
    settings.quit_after = None;

    let Some((mut snapshot, ops)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| cx.new(|cx| Ops::new(&settings, window, cx)),
    ) else {
        return;
    };
    let save = |snapshot: &mut Snapshot, name: &str| {
        snapshot.save(name);
        let line = snapshot.read(&ops, |ops, cx| ops.status_line(cx));
        out!("{name}: {line}");
    };

    snapshot.run_for(WARM_UP);
    save(&mut snapshot, "ops-quiet");

    snapshot.update(|cx| ops.update(cx, |ops, cx| ops.toggle_storm(cx)));
    snapshot.run_for(STORM);
    save(&mut snapshot, "ops-storm");

    snapshot.update(|cx| ops.update(cx, |ops, cx| ops.toggle_paused(cx)));
    snapshot.run_for(PAUSED);
    save(&mut snapshot, "ops-storm-paused");
    snapshot.update(|cx| ops.update(cx, |ops, cx| ops.toggle_paused(cx)));

    snapshot.update(|cx| {
        in_window(cx, |window, cx| {
            ops.update(cx, |ops, cx| {
                let hall = ops
                    .control
                    .storm_hall()
                    .and_then(|id| ops.fleet.read(cx).snapshot().get(&id).cloned());
                ops.alerts.update(cx, |console, cx| {
                    console.set_grouping(Grouping::Dc, cx);
                    console.toggle_severity(Severity::Minor, cx);
                    console.toggle_severity(Severity::Warning, cx);
                    if let Some(hall) = hall {
                        console.type_search(&hall.name, window, cx);
                    }
                });
            });
        })
    });
    snapshot.run_for(SETTLE);
    save(&mut snapshot, "ops-storm-dc");

    snapshot.update(|cx| {
        in_window(cx, |window, cx| {
            ops.update(cx, |ops, cx| {
                ops.alerts.update(cx, |console, cx| {
                    console.set_grouping(Grouping::Service, cx);
                    console.toggle_severity(Severity::Minor, cx);
                    console.toggle_severity(Severity::Warning, cx);
                    console.type_search("", window, cx);
                });
                ops.toggle_storm(cx);
            });
        })
    });
    snapshot.run_for(RECOVERY);
    save(&mut snapshot, "ops-recovery");
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
