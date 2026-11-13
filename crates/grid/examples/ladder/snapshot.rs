//! `LADDER_SNAPSHOT=<dir>`: the ladder drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! LADDER_SNAPSHOT=target/ladder cargo run --release -p gpui-kit-grid \
//!     --features test-support --example ladder
//! ```
//!
//! The exchange runs for a few seconds in real time with `LADDER_SEED` (7
//! unless set); then the scenario clicks as a trader would: two buys under
//! the market and two sells over it in the ladder's order columns, one of
//! them cancelled with a right click, and a market buy. After a few more
//! seconds the window is saved (`ladder-es.png`), then with two more
//! contracts of other tick sizes (`ladder-gc.png`, `ladder-ng.png`). The
//! window is 1600×960 at a scale of 2.

use std::{path::Path, time::Duration};

use gpui_kit::{AppContext as _, MouseButton, px, size};

use crate::{
    Command, Screen, Settings, dom,
    rows::LadderColumn,
    trading::{instruments::ContractId, snapshot::Snapshot},
};

/// The seed unless `LADDER_SEED` says otherwise: the same market every run.
const SEED: u64 = 7;
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time the exchange runs before the first clicks.
const WARM_UP: Duration = Duration::from_secs(3);
/// Real time after the clicks, so orders fill and the chart's draw-in ends.
const TRADING: Duration = Duration::from_secs(4);
/// Real time after another contract is picked.
const SETTLE: Duration = Duration::from_millis(1_500);

pub fn run(mut settings: Settings, dir: &Path) {
    settings.seed = settings.seed.or(Some(SEED));
    let Some((mut snapshot, screen)) =
        Snapshot::open(dir, size(px(WIDTH), px(HEIGHT)), crate::setup, |_, cx| {
            cx.new(|cx| Screen::new(&settings, cx))
        })
    else {
        return;
    };

    snapshot.run_for(WARM_UP);
    snapshot.update(|cx| {
        screen.update(cx, |screen, cx| {
            let (contract, bid, ask) = (screen.contract, screen.status.bid, screen.status.ask);
            let click = |tick, column, button, quantity| {
                dom::click_request(contract, tick, column, button, quantity)
            };
            let requests = [
                click(bid - 1, LadderColumn::Buys, MouseButton::Left, 2),
                click(bid - 4, LadderColumn::Buys, MouseButton::Left, 5),
                click(ask + 2, LadderColumn::Sells, MouseButton::Left, 2),
                click(ask + 6, LadderColumn::Sells, MouseButton::Left, 1),
                click(ask + 9, LadderColumn::Sells, MouseButton::Left, 3),
                click(ask + 9, LadderColumn::Sells, MouseButton::Right, 0),
            ];
            for request in requests.into_iter().flatten() {
                screen.link.send(request);
            }
            screen.quantity = 3;
            screen.run(Command::BuyMarket, cx);
        })
    });
    snapshot.run_for(TRADING);
    snapshot.save("ladder-es");

    for root in ["GC", "NG"] {
        let id = ContractId::find(root).expect("a contract");
        snapshot.update(|cx| screen.update(cx, |screen, cx| screen.select(id, cx)));
        snapshot.run_for(SETTLE);
        snapshot.save(&format!("ladder-{}", root.to_ascii_lowercase()));
    }
}
