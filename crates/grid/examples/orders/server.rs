//! The "server" of the orders example, and the market around it.
//!
//! ```text
//! UI (EditCommit) ─► channel ─► server thread: sleep(latency) ─► rule (band, reject %)
//!     ├─ Ok: writer.upsert(order with the value, rounded) ─► reply Ok
//!     └─ Err: reply Err(reason)
//! reply channel ─► UI task ─► grid.resolve_edit(edit, result)
//! market thread: Last ticks (RATE), fills (FILLS), another trader's limits
//! (ORDERS_CONFLICT_PERCENT), filled orders removed (ORDERS_REMOVE_FILLED_MS)
//! ```
//!
//! The server writes the store before it answers: the confirmation reaches
//! the grid in a snapshot before or with `resolve_edit`, which the grid's
//! settle rule covers either way.

use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use gpui_kit_grid::{EditId, GridWriter, OwnedValue};

use crate::model::{self, Order, OrderColumn, Status};

/// What the window asks of the server.
pub enum Command {
    /// Apply an edit and answer it.
    Edit {
        edit: EditId,
        key: u64,
        column: OrderColumn,
        value: OwnedValue,
    },
    /// Cancel an order (its Cancel button).
    Cancel(u64),
    /// For the end-to-end run: 300 ms after an edit starts, another trader
    /// changes the order's limit (a conflict), or the order fills and
    /// leaves.
    ConflictLater(u64),
    FillLater(u64),
}

/// The answer to an edit.
pub type Reply = (EditId, Result<(), String>);

/// How the server and the market behave (the environment, docs of
/// `main.rs`).
#[derive(Clone, Copy, Debug)]
pub struct Market {
    pub latency: Duration,
    pub reject_percent: u64,
    /// Last updates per second.
    pub rate: u64,
    /// Fills per second.
    pub fills: u64,
    pub conflict_percent: u64,
    /// `None`: filled orders stay.
    pub remove_filled: Option<Duration>,
    pub seed: u64,
}

/// Starts the server thread and the market thread over `writer`. Returns
/// where the window sends its commands and where the answers come.
pub fn start(writer: GridWriter<Order>, market: Market) -> (Sender<Command>, Receiver<Reply>) {
    let (commands, inbox) = async_channel::unbounded::<Command>();
    let (replies, answers) = async_channel::unbounded::<Reply>();
    let (later, scheduled) = async_channel::unbounded::<(Instant, Later)>();
    {
        let writer = writer.clone();
        thread::spawn(move || serve(writer, market, inbox, replies, later));
    }
    thread::spawn(move || run_market(writer, market, scheduled));
    (commands, answers)
}

/// A change the market makes later.
enum Later {
    Conflict(u64),
    FillAndRemove(u64),
    Remove(u64),
}

fn serve(
    writer: GridWriter<Order>,
    market: Market,
    inbox: Receiver<Command>,
    replies: Sender<Reply>,
    later: Sender<(Instant, Later)>,
) {
    let mut rng = fastrand::Rng::with_seed(market.seed ^ 0x5eed);
    while let Ok(command) = inbox.recv_blocking() {
        match command {
            Command::Edit {
                edit,
                key,
                column,
                value,
            } => {
                thread::sleep(market.latency);
                let snapshot = writer.snapshot();
                let result = match snapshot.get(&key) {
                    None => Err("Order is gone".to_string()),
                    Some(order) => {
                        let rejected = rng.u64(..100) < market.reject_percent;
                        match model::server_rule(order, column, &value) {
                            Err(reason) => Err(reason.to_string()),
                            Ok(_) if rejected => Err("Exchange rejected".to_string()),
                            Ok(value) => match order.with(column, &value) {
                                Some(order) => {
                                    writer.upsert(order);
                                    Ok(())
                                }
                                None => Err("Not supported".to_string()),
                            },
                        }
                    }
                };
                if replies.send_blocking((edit, result)).is_err() {
                    return;
                }
            }
            Command::Cancel(key) => {
                if let Some(order) = writer.snapshot().get(&key)
                    && !order.status.is_final()
                {
                    let mut order = (**order).clone();
                    order.status = Status::Cancelled;
                    order.active = false;
                    writer.upsert(order);
                }
            }
            Command::ConflictLater(key) => {
                let _ = later.send_blocking((
                    Instant::now() + Duration::from_millis(300),
                    Later::Conflict(key),
                ));
            }
            Command::FillLater(key) => {
                let _ = later.send_blocking((
                    Instant::now() + Duration::from_millis(300),
                    Later::FillAndRemove(key),
                ));
            }
        }
    }
}

/// The market: one tick every 20 ms.
fn run_market(writer: GridWriter<Order>, market: Market, scheduled: Receiver<(Instant, Later)>) {
    const TICK: Duration = Duration::from_millis(20);
    let ticks_per_second = 1_000 / TICK.as_millis() as u64;
    let mut rng = fastrand::Rng::with_seed(market.seed ^ 0x3a7c);
    let mut pending: VecDeque<(Instant, Later)> = VecDeque::new();
    let mut tick: u64 = 0;
    // Fractional rates carry over from tick to tick.
    let (mut lasts, mut fills) = (0u64, 0u64);
    loop {
        thread::sleep(TICK);
        tick += 1;
        while let Ok(item) = scheduled.try_recv() {
            pending.push_back(item);
        }
        let snapshot = writer.snapshot();
        let ids: Vec<u64> = snapshot.iter().map(|(_, order)| order.id).collect();
        if ids.is_empty() {
            continue;
        }
        let pick = |rng: &mut fastrand::Rng| ids[rng.usize(..ids.len())];
        // Last prices.
        lasts += market.rate;
        let updates = lasts / ticks_per_second;
        lasts %= ticks_per_second;
        for _ in 0..updates {
            if let Some(order) = snapshot.get(&pick(&mut rng)) {
                let mut order = (**order).clone();
                order.last = model::round_to(order.last * (1. + (rng.f64() - 0.5) / 200.), 0.01);
                writer.upsert(order);
            }
        }
        // Fills of working orders.
        fills += market.fills;
        let filled = fills / ticks_per_second;
        fills %= ticks_per_second;
        for _ in 0..filled {
            if let Some(order) = snapshot.get(&pick(&mut rng))
                && matches!(order.status, Status::Working | Status::PartFilled)
            {
                let mut order = (**order).clone();
                order.filled = (order.filled + 0.25).min(1.);
                order.status = if order.filled >= 1. {
                    Status::Filled
                } else {
                    Status::PartFilled
                };
                if order.status == Status::Filled
                    && let Some(after) = market.remove_filled
                {
                    pending.push_back((Instant::now() + after, Later::Remove(order.id)));
                }
                writer.upsert(order);
            }
        }
        // Another trader changes a limit, in some seconds.
        if tick % ticks_per_second == 0 && rng.u64(..100) < market.conflict_percent {
            if let Some(order) = snapshot.get(&pick(&mut rng))
                && !order.status.is_final()
            {
                writer.upsert(other_trader(order));
            }
        }
        // What is due.
        let now = Instant::now();
        while pending.front().is_some_and(|(at, _)| *at <= now) {
            let Some((_, later)) = pending.pop_front() else {
                break;
            };
            let snapshot = writer.snapshot();
            match later {
                Later::Conflict(key) => {
                    if let Some(order) = snapshot.get(&key) {
                        writer.upsert(other_trader(order));
                    }
                }
                Later::FillAndRemove(key) => {
                    if let Some(order) = snapshot.get(&key) {
                        let mut order = (**order).clone();
                        order.filled = 1.;
                        order.status = Status::Filled;
                        writer.upsert(order);
                        writer.remove(key);
                    }
                }
                Later::Remove(key) => writer.remove(key),
            }
        }
    }
}

/// The order with its limit moved one price step up by another trader.
fn other_trader(order: &Order) -> Order {
    let mut order = order.clone();
    order.limit = model::round_to(order.limit + model::STEP * 2., model::STEP);
    order
}
