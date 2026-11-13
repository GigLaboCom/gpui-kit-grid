//! `BROKER_SNAPSHOT=<dir>`: the broker drawn offscreen into PNG files with
//! the trading examples' harness (`trading/snapshot.rs`), which needs
//! `--features test-support`:
//!
//! ```sh
//! BROKER_SNAPSHOT=target/broker cargo run --release -p gpui-kit-grid \
//!     --features test-support --example broker
//! ```
//!
//! The simulator runs for a few seconds in real time with `BROKER_SEED`
//! (11 unless set); the ticket buys 10 AAPL at the market and places a
//! limit buy of 5 below it, which works. Then the broker is saved as the
//! keys and the controls would leave it: light with the positions
//! (`broker-light.png`); dark with the orders (`broker-dark.png`); light
//! with red up and the movers (`broker-red-up.png`); a search that matches
//! nothing (`broker-search.png`). The window is 1600×960.

use std::{path::Path, time::Duration};

use gpui_kit::{App, AppContext as _, Context, Window, px, size};

use super::{BottomTab, Broker, BrokerTheme, Settings, account::OrderType, colors::PriceColors};
use crate::trading::{rows::Side, snapshot::Snapshot, theme::TradingTheme as _};

/// The seed unless `BROKER_SEED` says otherwise: the same market every run.
const SEED: u64 = 11;
const WIDTH: f32 = 1600.;
const HEIGHT: f32 = 960.;
/// Real time the simulator runs before the first image: the quotes, the
/// movers and the chart in.
const WARM_UP: Duration = Duration::from_secs(5);
/// Real time after a step, so the grids rebuild and the chart draws in.
const SETTLE: Duration = Duration::from_millis(1_500);
/// What the search step types.
const SEARCH: &str = "zzz";
/// Frames measured after the warm-up.
const MEASURED_FRAMES: usize = 240;

pub fn run(mut settings: Settings, dir: &Path) {
    settings.seed = settings.seed.or(Some(SEED));
    settings.quit_after = None;

    let Some((mut snapshot, broker)) = Snapshot::open(
        dir,
        size(px(WIDTH), px(HEIGHT)),
        super::configure,
        |window, cx| cx.new(|cx| Broker::new(&settings, window, cx)),
    ) else {
        return;
    };
    let step = |snapshot: &mut Snapshot,
                name: &str,
                act: &dyn Fn(&mut Broker, &mut Window, &mut Context<Broker>)| {
        snapshot.update(|cx| {
            in_window(cx, |window, cx| {
                broker.update(cx, |broker, cx| act(broker, window, cx));
            })
        });
        snapshot.run_for(SETTLE);
        snapshot.save(name);
    };

    snapshot.run_for(WARM_UP);
    // Frame times and the elements in the quote lists' cells.
    snapshot.measure("broker", MEASURED_FRAMES, |cx| {
        let broker = broker.read(cx);
        [&broker.watchlist, &broker.gainers, &broker.losers]
            .into_iter()
            .map(|grid| grid.read(cx).stats().cell_hosts())
            .sum()
    });
    step(&mut snapshot, "broker-light", &|broker, window, cx| {
        broker.ticket.update(cx, |ticket, cx| {
            ticket.set_side(Side::Buy, window, cx);
            ticket.set_quantity(10, window, cx);
            ticket.submit(window, cx);
            // A limit buy 3 % under the market: it works.
            ticket.set_order_type(OrderType::Limit, window, cx);
            ticket.set_quantity(5, window, cx);
            ticket.set_limit_below_market(0.03, window, cx);
            ticket.submit(window, cx);
        });
    });
    step(&mut snapshot, "broker-dark", &|broker, window, cx| {
        BrokerTheme::Dark.apply(cx);
        broker.show(BottomTab::Orders, window, cx);
    });
    step(&mut snapshot, "broker-red-up", &|broker, window, cx| {
        BrokerTheme::Light.apply(cx);
        broker.set_price_colors(PriceColors::RedUp, cx);
        broker.show(BottomTab::Movers, window, cx);
    });
    step(&mut snapshot, "broker-search", &|broker, window, cx| {
        broker.set_price_colors(PriceColors::GreenUp, cx);
        broker.show(BottomTab::Positions, window, cx);
        // A typed search: the box shows it, the watchlist filters by it.
        broker
            .search
            .update(cx, |input, cx| input.set_value(SEARCH, window, cx));
        broker
            .watchlist
            .update(cx, |grid, cx| grid.set_quick_filter(SEARCH, cx));
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
