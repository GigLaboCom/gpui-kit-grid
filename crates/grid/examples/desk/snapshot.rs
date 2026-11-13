//! `DESK_SNAPSHOT=<dir>`: the desk drawn offscreen into PNG files with the
//! trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! DESK_SNAPSHOT=target/desk cargo run --release -p gpui-kit-grid \
//!     --features test-support --example desk
//! ```
//!
//! The simulator runs for a few seconds in real time with `DESK_SEED`
//! (7 unless set), then the desk is saved once per theme
//! (`desk-amber.png`, `desk-depth.png`, `desk-heat.png`) and, in the last
//! theme, with the chart's other timeframes (`desk-heat-5s.png`,
//! `desk-heat-5m.png`). The window is 1600×960 at a scale of 2.

use std::{path::Path, time::Duration};

use gpui_kit::{AppContext as _, px, size};

use super::{Desk, DeskTheme, Settings};
use crate::trading::{chart::Timeframe, snapshot::Snapshot, theme::TradingTheme as _};

/// The seed unless `DESK_SEED` says otherwise: the same market every run.
const SEED: u64 = 7;
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time the simulator runs before the first image.
const WARM_UP: Duration = Duration::from_secs(4);
/// Real time after a theme or a timeframe changes, so the charts' draw-in
/// ends and new bars arrive.
const SETTLE: Duration = Duration::from_millis(1_200);
/// Frames measured after the warm-up.
const MEASURED_FRAMES: usize = 240;

pub fn run(mut settings: Settings, dir: &Path) {
    settings.seed = settings.seed.or(Some(SEED));
    settings.quit_after = None;

    let Some((mut snapshot, desk)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        crate::trading::theme::install::<DeskTheme>,
        |window, cx| cx.new(|cx| Desk::new(&settings, window, cx)),
    ) else {
        return;
    };
    let chart = snapshot.read(&desk, |desk, _| desk.chart.clone());

    snapshot.run_for(WARM_UP);
    // Frame times and the elements in the book's cells, in the first theme.
    snapshot.measure("desk", MEASURED_FRAMES, |cx| {
        desk.read(cx)
            .book
            .read(cx)
            .grid()
            .read(cx)
            .stats()
            .cell_hosts()
    });
    let mut last = settings.theme;
    for &theme in DeskTheme::ALL {
        snapshot.update(|cx| theme.apply(cx));
        snapshot.run_for(SETTLE);
        snapshot.save(&format!("desk-{}", file_name(theme)));
        last = theme;
    }
    let initial = snapshot.read(&chart, |chart, _| chart.timeframe());
    for timeframe in Timeframe::ALL
        .into_iter()
        .filter(|timeframe| *timeframe != initial)
    {
        snapshot.update(|cx| chart.update(cx, |chart, cx| chart.set_timeframe(timeframe, cx)));
        snapshot.run_for(SETTLE);
        snapshot.save(&format!("desk-{}-{}", file_name(last), timeframe.label()));
    }
}

fn file_name(theme: DeskTheme) -> String {
    theme.label().to_ascii_lowercase()
}
