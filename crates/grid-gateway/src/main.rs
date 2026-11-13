//! The live quotes gateway of the web example: the trading examples'
//! simulator (150 instruments, every price simulated) served over
//! WebSocket with the `gpui-kit-grid-feed` protocol, on 127.0.0.1 only.
//!
//! One thread runs the simulator in 10 ms batches and publishes the changed
//! quotes to the hub; each client has a thread that sends a snapshot, then
//! every `--send-ms` the latest value of each key changed since its last
//! message (conflation: a slow client never builds a queue), and a
//! heartbeat every second.
//!
//! ```text
//! grid-gateway [--port 9001] [--rate 300] [--send-ms 20] [--seed N]
//!              [--drop-every N] [--stall-ms N]
//! ```
//!
//! Fault injection: `--drop-every N` numbers every Nth delta of a client but
//! does not send it (a gap the client must resync); `--stall-ms N` sleeps
//! after every delta, as a client that reads slowly. A line of counters goes
//! to stdout every second.

mod hub;
mod session;
mod trading;

use std::{
    io::{self, Write as _},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use gpui_kit_grid_feed::{ClientMessage, QuoteRecord, ServerMessage};
use tungstenite::{Message, WebSocket};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use hub::{Hub, SessionId};
use session::Session;
use trading::{
    market::{self, Market},
    rows::Quote,
};

const USAGE: &str = "usage: grid-gateway [--port N] [--rate N] [--send-ms 16..50] [--seed N] \
                     [--drop-every N] [--stall-ms N]";
/// How long a new connection may take to say `Hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(1);
/// How long one write may block before the client counts as slow: its
/// changes then keep conflating until the socket drains.
const WRITE_TIMEOUT: Duration = Duration::from_millis(50);

/// The command line.
#[derive(Clone, Debug, PartialEq)]
struct Options {
    port: u16,
    rate: u64,
    send_interval: Duration,
    seed: Option<u64>,
    drop_every: Option<u64>,
    stall: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            port: 9001,
            rate: market::DEFAULT_RATE,
            send_interval: Duration::from_millis(20),
            seed: None,
            drop_every: None,
            stall: Duration::ZERO,
        }
    }
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let mut value = |flag: &str| -> Result<u64, String> {
                let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
                value
                    .parse()
                    .map_err(|_| format!("{flag}: not a number: {value}"))
            };
            match flag.as_str() {
                "--port" => {
                    options.port = u16::try_from(value(&flag)?).map_err(|_| "--port: too large")?
                }
                "--rate" => options.rate = value(&flag)?.min(market::MAX_RATE),
                "--send-ms" => {
                    options.send_interval = Duration::from_millis(value(&flag)?.clamp(16, 50))
                }
                "--seed" => options.seed = Some(value(&flag)?),
                "--drop-every" => options.drop_every = Some(value(&flag)?).filter(|n| *n > 0),
                "--stall-ms" => options.stall = Duration::from_millis(value(&flag)?),
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown flag {other}\n{USAGE}")),
            }
        }
        Ok(options)
    }
}

/// The gateway's counters, for the line it prints every second.
#[derive(Default)]
struct Counters {
    sessions: AtomicU64,
    snapshots: AtomicU64,
    deltas: AtomicU64,
    rows: AtomicU64,
    dropped: AtomicU64,
    resyncs: AtomicU64,
}

// A closed stdout, for example a parent that exited, must not abort the
// gateway.
macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(io::stdout().lock(), $($arg)*);
    }};
}

fn main() {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            std::process::exit(2);
        }
    };
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "bind 127.0.0.1:{}: {error}",
                options.port
            );
            std::process::exit(1);
        }
    };
    let seed = options.seed.unwrap_or_else(now_ms);
    let market = Market::new(Some(seed));
    let hub = Arc::new(Hub::new(market.quotes().iter().map(record).collect()));
    let counters = Arc::new(Counters::default());
    {
        let (hub, rate) = (hub.clone(), options.rate);
        thread::Builder::new()
            .name("simulator".into())
            .spawn(move || simulate(market, &hub, rate))
            .expect("spawn the simulator thread");
    }
    {
        let (hub, counters) = (hub.clone(), counters.clone());
        thread::Builder::new()
            .name("counters".into())
            .spawn(move || report(&hub, &counters))
            .expect("spawn the counters thread");
    }
    let address = listener.local_addr().map(|address| address.port());
    out!(
        "listening ws://127.0.0.1:{} rate={} send_ms={} drop_every={} stall_ms={}",
        address.unwrap_or(options.port),
        options.rate,
        options.send_interval.as_millis(),
        options.drop_every.unwrap_or(0),
        options.stall.as_millis(),
    );
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (hub, counters, options) = (hub.clone(), counters.clone(), options.clone());
        let spawned = thread::Builder::new().name("client".into()).spawn(move || {
            if let Err(error) = serve(stream, &hub, &counters, &options) {
                out!("client: {error}");
            }
        });
        if let Err(error) = spawned {
            out!("client thread: {error}");
        }
    }
}

/// The wire record of a simulated quote.
pub(crate) fn record(quote: &Quote) -> QuoteRecord {
    QuoteRecord {
        id: u32::from(quote.id.0),
        last: quote.last,
        bid: quote.bid,
        ask: quote.ask,
        volume: quote.volume,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Runs the simulator: `rate` updates a second in 10 ms batches, as the
/// examples' `Pump` does, published to the hub.
fn simulate(mut market: Market, hub: &Hub, rate: u64) {
    let started = Instant::now();
    let (mut batch, mut due) = (0u64, 0f64);
    let mut changed = Vec::with_capacity(1024);
    loop {
        due += rate as f64 / 100.;
        let count = due as u64;
        due -= count as f64;
        changed.clear();
        market.step_quotes(count, rate as f64, &mut changed);
        hub.publish(changed.iter().map(|id| record(market.quote(*id))));
        batch += 1;
        let next = started + Duration::from_millis(batch * 10);
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else if now - next > Duration::from_millis(200) {
            // Too far behind: do not catch up in a burst.
            batch = (started.elapsed().as_millis() / 10) as u64;
        }
    }
}

/// Prints the counters every second, when they changed.
fn report(hub: &Hub, counters: &Counters) {
    let started = Instant::now();
    let mut last = String::new();
    loop {
        thread::sleep(Duration::from_secs(1));
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let line = format!(
            "produced={} rows_sent={} deltas={} snapshots={} resyncs={} dropped={} sessions={}",
            hub.produced(),
            load(&counters.rows),
            load(&counters.deltas),
            load(&counters.snapshots),
            load(&counters.resyncs),
            load(&counters.dropped),
            load(&counters.sessions),
        );
        if line != last {
            out!("stats t={:.1} {line}", started.elapsed().as_secs_f64());
            last = line;
        }
    }
}

/// Leaves the hub when the client's thread ends.
struct Joined<'a> {
    hub: &'a Hub,
    id: SessionId,
    counters: &'a Counters,
}

impl Drop for Joined<'_> {
    fn drop(&mut self) {
        self.hub.leave(self.id);
        self.counters.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Serves one client until it goes: `Hello`, a snapshot, then deltas,
/// heartbeats and snapshots on `Resync`.
fn serve(
    stream: TcpStream,
    hub: &Hub,
    counters: &Counters,
    options: &Options,
) -> Result<(), tungstenite::Error> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut socket = tungstenite::accept(stream).map_err(|error| match error {
        tungstenite::HandshakeError::Failure(error) => error,
        tungstenite::HandshakeError::Interrupted(_) => {
            tungstenite::Error::Io(io::ErrorKind::TimedOut.into())
        }
    })?;
    let last_seq = loop {
        match socket.read()? {
            Message::Binary(bytes) => match ClientMessage::decode(&bytes) {
                Ok(ClientMessage::Hello { last_seq, .. }) => break last_seq,
                Ok(ClientMessage::Resync) => continue,
                Err(error) => return Err(protocol_error(error)),
            },
            Message::Close(_) => return Ok(()),
            _ => continue,
        }
    };

    let id = hub.join();
    counters.sessions.fetch_add(1, Ordering::Relaxed);
    let _joined = Joined { hub, id, counters };
    let mut session = Session::new(last_seq, options.drop_every);
    let mut link = Link::new(socket);
    link.send(&session.snapshot(hub.snapshot(id), now_ms()))?;
    counters.snapshots.fetch_add(1, Ordering::Relaxed);

    let mut next_send = Instant::now() + options.send_interval;
    let mut next_heartbeat = Instant::now() + HEARTBEAT;
    let mut upserts = Vec::new();
    loop {
        let now = Instant::now();
        let wait = next_send.min(next_heartbeat).saturating_duration_since(now);
        match link.receive(wait.max(Duration::from_millis(1)))? {
            Received::Nothing => {}
            Received::Closed => return Ok(()),
            Received::Message(ClientMessage::Resync | ClientMessage::Hello { .. }) => {
                counters.resyncs.fetch_add(1, Ordering::Relaxed);
                link.send(&session.snapshot(hub.snapshot(id), now_ms()))?;
                counters.snapshots.fetch_add(1, Ordering::Relaxed);
            }
        }
        let now = Instant::now();
        if now >= next_send {
            next_send = (next_send + options.send_interval).max(now);
            // A client still draining the last message keeps conflating.
            if link.drain()? {
                upserts.clear();
                hub.take_changes(id, &mut upserts);
                if !upserts.is_empty() {
                    let rows = upserts.len() as u64;
                    match session.delta(std::mem::take(&mut upserts), now_ms()) {
                        Some(delta) => {
                            link.send(&delta)?;
                            counters.deltas.fetch_add(1, Ordering::Relaxed);
                            counters.rows.fetch_add(rows, Ordering::Relaxed);
                        }
                        None => {
                            counters.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if !options.stall.is_zero() {
                        thread::sleep(options.stall);
                    }
                }
            }
        }
        if now >= next_heartbeat {
            next_heartbeat = (next_heartbeat + HEARTBEAT).max(now);
            if link.drain()? {
                link.send(&session.heartbeat(now_ms()))?;
            }
        }
    }
}

fn protocol_error(error: gpui_kit_grid_feed::DecodeError) -> tungstenite::Error {
    tungstenite::Error::Io(io::Error::new(io::ErrorKind::InvalidData, error))
}

/// What a read brought.
enum Received {
    Nothing,
    Message(ClientMessage),
    Closed,
}

/// The socket, with a write that may stay pending while the client drains.
struct Link {
    socket: WebSocket<TcpStream>,
    pending: bool,
}

impl Link {
    fn new(socket: WebSocket<TcpStream>) -> Self {
        Self {
            socket,
            pending: false,
        }
    }

    /// Writes `message`; returns false if it is still pending (the client
    /// reads slowly), in which case nothing more is written until
    /// [`Self::drain`] says so.
    fn send(&mut self, message: &ServerMessage) -> Result<bool, tungstenite::Error> {
        self.socket
            .write(Message::binary(message.to_bytes()))
            .or_else(would_block)?;
        self.pending = true;
        self.drain()
    }

    /// Flushes a pending write; true once nothing is pending.
    fn drain(&mut self) -> Result<bool, tungstenite::Error> {
        if self.pending {
            match self.socket.flush() {
                Ok(()) => self.pending = false,
                Err(error) => would_block(error)?,
            }
        }
        Ok(!self.pending)
    }

    /// Waits up to `wait` for a client message.
    fn receive(&mut self, wait: Duration) -> Result<Received, tungstenite::Error> {
        self.socket.get_ref().set_read_timeout(Some(wait))?;
        match self.socket.read() {
            Ok(Message::Binary(bytes)) => ClientMessage::decode(&bytes)
                .map(Received::Message)
                .map_err(protocol_error),
            Ok(Message::Close(_))
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(Received::Closed)
            }
            Ok(_) => Ok(Received::Nothing),
            Err(error) => would_block(error).map(|()| Received::Nothing),
        }
    }
}

/// A read or write that timed out is not an error.
fn would_block(error: tungstenite::Error) -> Result<(), tungstenite::Error> {
    match error {
        tungstenite::Error::Io(ref io)
            if matches!(
                io.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        error => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &str) -> Result<Options, String> {
        Options::parse(args.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn test_flags() {
        assert_eq!(parse(""), Ok(Options::default()));
        let options = parse("--port 0 --rate 5000 --drop-every 100 --stall-ms 200 --seed 7")
            .expect("valid flags");
        assert_eq!(
            options,
            Options {
                port: 0,
                rate: 5000,
                seed: Some(7),
                drop_every: Some(100),
                stall: Duration::from_millis(200),
                ..Options::default()
            }
        );
        assert_eq!(
            parse("--send-ms 1").map(|o| o.send_interval),
            Ok(Duration::from_millis(16))
        );
        assert_eq!(parse("--drop-every 0").map(|o| o.drop_every), Ok(None));
        assert!(parse("--port").is_err());
        assert!(parse("--port 70000").is_err());
        assert!(parse("--rate fast").is_err());
        assert!(parse("--verbose").is_err());
    }
}
