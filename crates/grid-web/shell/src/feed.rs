//! Quotes from a gateway (`crates/grid-gateway`) over WebSocket instead of
//! an in-page simulator: the client a page connects with
//! `?feed=ws://127.0.0.1:<port>`.
//!
//! The socket's callbacks only queue events; one task on the page's main
//! thread runs the protocol's state machine ([`FeedClient`]) and writes the
//! grid store: a snapshot with `replace_all`, a delta as one `batch`. A lost
//! connection, or a second without a heartbeat, reconnects after an
//! exponential backoff with jitter, and `Hello` names the last sequence
//! number applied.

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::{AsyncApp, Context, Task};
use gpui_kit_grid::{GridRow, GridWriter};
use gpui_kit_grid_feed::{
    ClientMessage, FeedAction, FeedClient, FeedState, QuoteRecord, ServerMessage,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use web_sys::{BinaryType, MessageEvent, WebSocket};

/// The first wait before reconnecting; it doubles up to `BACKOFF_MAX`.
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How often the connection checks that the server still speaks.
const TICK: Duration = Duration::from_millis(250);
/// Silence (no message, or no open) that counts as a lost connection: three
/// missed heartbeats.
const SILENCE: Duration = Duration::from_secs(3);
/// Latency samples kept for the measuring hook (the quotes page's
/// `feed_stats`).
const SAMPLES_KEPT: usize = 100_000;

/// A grid row the feed writes from the gateway's quote records.
pub trait FeedRow: GridRow + Sized {
    /// The row a record carries; `None` for an instrument the page does
    /// not know.
    fn from_record(record: &QuoteRecord) -> Option<Self>;

    /// The key of the row a removal names by its record id; `None` for one
    /// the page does not know.
    fn key_of_record(id: u32) -> Option<Self::Key>;
}

/// The feed's state and counters, shared by the connection task, the
/// status line and the measuring hook.
pub struct Feed {
    client: FeedClient,
    /// Rows written to the store: snapshot rows and delta upserts.
    rows: u64,
    reconnects: u64,
    /// Server → ingest latencies of the deltas since the hook last reset
    /// them, in milliseconds.
    samples: Vec<f64>,
    /// The same since the status line last read them.
    recent: Vec<f64>,
}

impl Feed {
    /// A feed before its first connection, shared.
    pub fn shared() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            client: FeedClient::new(),
            rows: 0,
            reconnects: 0,
            samples: Vec::new(),
            recent: Vec::new(),
        }))
    }

    pub fn state(&self) -> FeedState {
        self.client.state()
    }

    /// Rows written to the store since the start.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The median latency since the last call, in milliseconds.
    pub fn take_latency(&mut self) -> Option<f64> {
        let latency = median(&mut self.recent);
        self.recent.clear();
        latency
    }

    /// For the status line: the state, the sequence number, the resyncs
    /// and `latency`.
    pub fn status(&self, latency: Option<f64>) -> String {
        let client = &self.client;
        // Nothing has answered yet: most likely no gateway runs, as when a
        // published page is opened with `?feed=` on a machine without one.
        if client.last_seq().is_none() && client.state() != FeedState::Live {
            return format!(
                "feed {}: no gateway yet, start one with \
                 `cargo run --release -p gpui-kit-grid-gateway`",
                client.state().label(),
            );
        }
        let mut status = format!(
            "feed {} · seq {} · {} resync{}",
            client.state().label(),
            client.last_seq().unwrap_or(0),
            client.resyncs(),
            if client.resyncs() == 1 { "" } else { "s" },
        );
        if let Some(latency) = latency {
            status.push_str(&format!(" · latency {latency:.0} ms"));
        }
        status
    }

    /// The counters and the latency percentiles as JSON; `reset` starts a
    /// new set of latency samples.
    pub fn stats_json(&mut self, reset: bool) -> String {
        let client = &self.client;
        let mut sorted = self.samples.clone();
        sorted.sort_by(f64::total_cmp);
        let at = |p: usize| -> f64 {
            sorted
                .get((sorted.len() * p / 100).min(sorted.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0.)
        };
        let json = format!(
            concat!(
                "{{\"state\":\"{}\",\"seq\":{},\"resyncs\":{},\"snapshots\":{},",
                "\"deltas\":{},\"ignored\":{},\"rows\":{},\"reconnects\":{},",
                "\"latency_ms\":{{\"n\":{},\"p50\":{},\"p99\":{},\"max\":{}}}}}"
            ),
            client.state().label(),
            client.last_seq().unwrap_or(0),
            client.resyncs(),
            client.snapshots(),
            client.deltas(),
            client.ignored(),
            self.rows,
            self.reconnects,
            sorted.len(),
            at(50),
            at(99),
            sorted.last().copied().unwrap_or(0.),
        );
        if reset {
            self.samples.clear();
        }
        json
    }

    fn record_latency(&mut self, latency: f64) {
        if self.samples.len() < SAMPLES_KEPT {
            self.samples.push(latency);
        }
        if self.recent.len() < SAMPLES_KEPT {
            self.recent.push(latency);
        }
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mid = values.len() / 2;
    Some(*values.select_nth_unstable_by(mid, f64::total_cmp).1)
}

/// Milliseconds since the Unix epoch, by the browser's clock (whole
/// milliseconds, as the server stamps them).
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// What the socket's callbacks queue for the connection task.
enum Event {
    Open,
    Message(Vec<u8>),
    Closed,
    Tick,
}

/// A browser WebSocket with its callbacks; dropping it closes the socket.
struct Socket {
    socket: WebSocket,
    _callbacks: [Closure<dyn FnMut(JsValue)>; 3],
}

impl Socket {
    fn open(url: &str, events: &async_channel::Sender<Event>) -> Result<Self, JsValue> {
        let socket = WebSocket::new(url)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let queue = |make: fn(JsValue) -> Event| {
            let events = events.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
                let _ = events.try_send(make(value));
            })
        };
        let callbacks = [
            queue(|_| Event::Open),
            queue(|event| {
                // A text message is not the protocol: an empty message
                // decodes to an error.
                let bytes = event
                    .dyn_into::<MessageEvent>()
                    .ok()
                    .and_then(|event| event.data().dyn_into::<js_sys::ArrayBuffer>().ok())
                    .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                    .unwrap_or_default();
                Event::Message(bytes)
            }),
            queue(|_| Event::Closed),
        ];
        socket.set_onopen(Some(callbacks[0].as_ref().unchecked_ref()));
        socket.set_onmessage(Some(callbacks[1].as_ref().unchecked_ref()));
        socket.set_onclose(Some(callbacks[2].as_ref().unchecked_ref()));
        Ok(Self {
            socket,
            _callbacks: callbacks,
        })
    }

    fn send(&self, message: &ClientMessage) {
        let _ = self.socket.send_with_u8_array(&message.to_bytes());
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

/// Connects to `url` and keeps the store of `writer` fed until the view
/// that spawned it goes; the view is notified whenever the feed's state
/// changes.
pub fn connect<V: 'static, R: FeedRow>(
    url: String,
    feed: Rc<RefCell<Feed>>,
    writer: GridWriter<R>,
    cx: &mut Context<V>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let executor = cx.background_executor().clone();
        let mut rng = fastrand::Rng::with_seed(now_ms() as u64);
        let mut backoff = BACKOFF_MIN;
        let notify = |cx: &mut AsyncApp| this.update(cx, |_, cx| cx.notify()).is_ok();
        loop {
            let (events, received) = async_channel::unbounded();
            let ticker = cx.spawn({
                let (events, executor) = (events.clone(), executor.clone());
                async move |_| {
                    loop {
                        executor.timer(TICK).await;
                        if events.send(Event::Tick).await.is_err() {
                            return;
                        }
                    }
                }
            });
            let mut went_live = false;
            match Socket::open(&url, &events) {
                Ok(socket) => {
                    drop(events);
                    let mut heard = executor.now();
                    while let Ok(event) = received.recv().await {
                        let before = feed.borrow().state();
                        match event {
                            Event::Open => {
                                heard = executor.now();
                                socket.send(&feed.borrow_mut().client.on_open());
                            }
                            Event::Message(bytes) => {
                                heard = executor.now();
                                let reply = receive(&bytes, &mut feed.borrow_mut(), &writer);
                                if let Some(reply) = reply {
                                    socket.send(&reply);
                                }
                            }
                            Event::Closed => break,
                            Event::Tick => {
                                if executor.now() - heard > SILENCE {
                                    break;
                                }
                            }
                        }
                        let after = feed.borrow().state();
                        went_live |= after == FeedState::Live;
                        if after != before && !notify(cx) {
                            return;
                        }
                    }
                }
                Err(error) => log::warn!("feed {url}: {error:?}"),
            }
            drop(ticker);
            {
                let mut feed = feed.borrow_mut();
                feed.client.on_close();
                feed.reconnects += 1;
            }
            if !notify(cx) {
                return;
            }
            if went_live {
                backoff = BACKOFF_MIN;
            }
            // Full jitter over the upper half of the backoff.
            let wait = backoff.mul_f64(0.5 + rng.f64() * 0.5);
            executor.timer(wait).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    })
}

/// Runs one message through the state machine and writes what it says;
/// returns what to send back.
fn receive<R: FeedRow>(
    bytes: &[u8],
    feed: &mut Feed,
    writer: &GridWriter<R>,
) -> Option<ClientMessage> {
    let (action, sent) = match ServerMessage::decode(bytes) {
        Ok(message) => {
            let sent = matches!(message, ServerMessage::Delta { .. }).then(|| message.time_ms());
            (feed.client.on_message(message), sent)
        }
        Err(error) => {
            log::warn!("feed: {error}");
            (feed.client.on_malformed(), None)
        }
    };
    match action? {
        FeedAction::ReplaceAll(records) => {
            writer.replace_all(records.iter().filter_map(R::from_record).map(Arc::new));
            feed.rows += records.len() as u64;
        }
        FeedAction::Batch { upserts, removes } => {
            writer.batch(|batch| {
                for row in upserts.iter().filter_map(R::from_record) {
                    batch.upsert(row);
                }
                for key in removes.into_iter().filter_map(R::key_of_record) {
                    batch.remove(key);
                }
            });
            feed.rows += upserts.len() as u64;
            // Server → ingest: from the server's stamp to the batch written.
            if let Some(sent) = sent {
                feed.record_latency(now_ms() - sent as f64);
            }
        }
        FeedAction::Send(message) => return Some(message),
    }
    None
}
