//! Ops in the browser (`www/ops/`): a data center's operations console
//! during an incident storm, the ops example's own `app.rs`
//! (`crates/grid/examples/ops/`), the same code as natively; only its
//! native `main.rs` and `native.rs` (the environment, the layout file) are
//! left out, and its simulator steps on the executor's timer instead of a
//! thread. Everything shown is simulated.
//!
//! The page's parameters: `hosts` (the fleet, 5,760 unless given, in whole
//! racks of 20), `seed` (the same fleet and storm every run),
//! `storm_seconds` (how long a storm lasts, 45 unless given) and `storm`
//! (a storm from the start). [`ops_storm`] starts a storm or restores the
//! feed, as F8 does.

// GPUI's `Action` derive spells its paths `gpui::` (the console's actions).
extern crate gpui_kit as gpui;

use std::time::Duration;

use gpui_kit::{App, Entity, Window};
use gpui_kit_grid_web_shell::{
    FontFiles, PageDelegate, PageParams, ScrollRoom, grid_json, ms, stats_json, with_view,
};
use serde_json::Value;
use wasm_bindgen::prelude::*;

// First: its `out!` and `err!` macros reach the modules after it.
#[macro_use]
#[path = "../../../../grid/examples/trading/mod.rs"]
mod trading;

// What only the example's native `main` uses is dead code here.
#[allow(dead_code)]
#[path = "../../../../grid/examples/ops/app.rs"]
mod ops;

use ops::{Ops, OpsTheme, rows::AlertKey};
use trading::theme::TradingTheme as _;

#[wasm_bindgen(start)]
fn start() {
    gpui_kit_grid_web_shell::register(OpsPage::default());
}

/// Starts a storm, or restores the failed feed (F8); returns the phase
/// before the toggle, empty before the view opens.
#[wasm_bindgen]
pub fn ops_storm() -> String {
    with_view::<Ops, _>(|ops, _, cx| {
        let phase = ops.read(cx).control().phase().label();
        ops.update(cx, |ops, cx| ops.toggle_storm(cx));
        Some(phase.to_owned())
    })
    .unwrap_or_default()
}

/// The ops example's side of the page.
#[derive(Default)]
struct OpsPage {
    /// What `setup` read from the page, for `open`.
    settings: Option<ops::Settings>,
    /// `?storm`: a storm from the start.
    storm: bool,
}

impl PageDelegate for OpsPage {
    type View = Ops;

    fn font_files(&self, _: &str) -> FontFiles {
        FontFiles::from_themes(OpsTheme::Graphite.file(), [])
    }

    fn add_font(&self, face: Vec<u8>) {
        trading::fonts::provide(face);
    }

    fn install_themes(&self, cx: &mut App) {
        trading::theme::install::<OpsTheme>(cx);
    }

    fn setup(&mut self, params: &PageParams, cx: &mut App) {
        let settings = ops::Settings::new(
            params.number("hosts"),
            Some(params.seed()),
            params.number("storm_seconds").map(u64::from),
        );
        ops::setup(cx);
        self.settings = Some(settings);
        self.storm = params.get("storm").is_some();
    }

    fn open(&mut self, window: &mut Window, cx: &mut App) -> Entity<Ops> {
        let settings = self.settings.as_ref().expect("setup before open");
        let view = ops::open(settings, window, cx);
        if self.storm {
            view.update(cx, |ops, cx| ops.toggle_storm(cx));
        }
        view
    }

    /// The alert console's counters: `updates` counts the alert updates
    /// (firings), `rate` those of the last half second a second; then the
    /// storm, and the fleet tree's frame and rebuild times.
    fn stats(&self, view: &Entity<Ops>, built: Duration, cx: &App) -> Value {
        let ops = view.read(cx);
        let control = ops.control();
        let mut stats = stats_json(
            control.updates(),
            ops.rate(),
            &ops.alert_grid(cx).read(cx).stats(),
        );
        let fleet = ops.fleet_grid().read(cx).stats();
        stats.extend([
            ("build_ms".into(), ms(built).into()),
            ("keys".into(), control.keys().into()),
            ("phase".into(), control.phase().label().into()),
            ("hosts_down".into(), control.hosts_down().into()),
            ("fleet_frames".into(), fleet.frames().into()),
            ("fleet_rebuilds".into(), fleet.rebuilds().into()),
            (
                "fleet_last_rebuild_ms".into(),
                ms(fleet.last_rebuild()).into(),
            ),
            (
                "fleet_max_rebuild_ms".into(),
                ms(fleet.max_rebuild()).into(),
            ),
            (
                "fleet_last_prepaint_ms".into(),
                ms(fleet.last_prepaint()).into(),
            ),
        ]);
        stats.into()
    }

    /// The alert grid's scroll, selection and sort, each alert by its
    /// subject and rule; `app` the console's own state.
    fn grid_state(&self, view: &Entity<Ops>, _: &Window, cx: &App) -> Value {
        let ops = view.read(cx);
        let grid = ops.alert_grid(cx).read(cx);
        let name = |key: &AlertKey| {
            grid.snapshot().get(key).map_or_else(String::new, |alert| {
                format!("{} · {}", alert.subject, alert.key.rule.name())
            })
        };
        let mut state = grid_json(grid, name);
        state.insert("app".into(), ops.page_state(cx));
        state.into()
    }

    /// The room of the alerts' rows.
    fn scroll_room(&self, view: &Entity<Ops>, cx: &App) -> ScrollRoom {
        ScrollRoom::of_grid(view.read(cx).alert_grid(cx).read(cx))
    }
}
