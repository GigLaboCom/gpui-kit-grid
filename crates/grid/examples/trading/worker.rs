//! Work that runs beside the window: the simulator and an example's own
//! periodic jobs. Natively each runs on a thread of its own; in the browser,
//! where GPUI's web platform has one thread, each runs one step at a time
//! on the executor's timer, between frames. The one place the trading
//! examples differ by target in what they run.

use std::{sync::Arc, time::Duration};

use async_channel::Receiver;
use gpui_kit::{App, Task};

use super::market::{Control, Feeds, Market};

/// Starts the simulator over `feeds` at `control`'s rate, with a fixed
/// `seed` for the same market every run. Returns a receiver told once the
/// first load is written, and the task to keep while it runs: in the
/// browser it runs the simulator, natively it is done at once, and the
/// simulator's thread runs until every store in `feeds` is gone.
///
/// Natively the market (two hours of bars for 150 instruments) is built on
/// the simulator's thread; in the browser on the calling one.
pub fn start_market(
    seed: Option<u64>,
    feeds: Feeds,
    control: Arc<Control>,
    cx: &App,
) -> (Receiver<()>, Task<()>) {
    let (loaded, first_load) = async_channel::bounded(1);
    #[cfg(not(target_family = "wasm"))]
    let task = {
        let _ = cx;
        std::thread::spawn(move || super::market::run(Market::new(seed), feeds, control, loaded));
        Task::ready(())
    };
    #[cfg(target_family = "wasm")]
    let task = {
        let mut pump = super::market::Pump::start(Market::new(seed), feeds, control);
        let _ = loaded.try_send(());
        cx.spawn(async move |cx| {
            let executor = cx.background_executor().clone();
            while !pump.is_closed() {
                // Never a zero timer: the loop must yield to the page.
                let wait = pump.step().max(Duration::from_millis(1));
                executor.timer(wait).await;
            }
        })
    };
    (first_load, task)
}

/// Runs `step` every `interval` until it returns false. Returns the task to
/// keep while it runs: in the browser it runs `step`, natively it is done
/// at once and `step` runs on a thread of its own.
pub fn repeat(
    interval: Duration,
    mut step: impl FnMut() -> bool + Send + 'static,
    cx: &App,
) -> Task<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = cx;
        std::thread::spawn(move || {
            while step() {
                std::thread::sleep(interval);
            }
        });
        Task::ready(())
    }
    #[cfg(target_family = "wasm")]
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        while step() {
            executor.timer(interval).await;
        }
    })
}
