//! The live quotes feed between a gateway and a grid in the browser: binary,
//! versioned messages and the client's sequence state machine. Standard
//! library only, with no GPUI, so the gateway (native) and the page
//! (`wasm32-unknown-unknown`) share it.
//!
//! # Messages
//!
//! One WebSocket binary message carries one protocol message. Every message
//! starts with the protocol version ([`PROTOCOL_VERSION`]) and its type; the
//! numbers are little-endian. Quote rows are fixed-width records keyed by
//! instrument id ([`QuoteRecord`], [`QuoteRecord::LEN`] bytes).
//!
//! - client → server: [`ClientMessage::Hello`] (the last sequence number the
//!   client applied, if any, and an optional token) once the socket opens;
//!   [`ClientMessage::Resync`] after a gap;
//! - server → client: [`ServerMessage::Snapshot`] (every row),
//!   [`ServerMessage::Delta`] (rows changed since the last message, and keys
//!   removed) and [`ServerMessage::Heartbeat`] (every second).
//!
//! # Sequence numbers
//!
//! A connection's snapshots and deltas take consecutive sequence numbers:
//! the first snapshot after `Hello` takes the client's last one plus one
//! (or 1), so the numbers a client sees never go back. A heartbeat repeats
//! the last number taken, so a lost trailing delta shows at the next one.
//! The server keeps no history: it answers `Hello` and `Resync` with a
//! snapshot.
//!
//! The server merges changes per client: a delta holds the latest value of
//! each key changed since the previous message, so a slow client gets fresh
//! values, never a queue.
//!
//! # The client
//!
//! [`FeedClient`] is the client's state machine, free of any socket: it
//! tracks the expected sequence number and turns messages into
//! [`FeedAction`]s. A snapshot replaces every row (`GridWriter::replace_all`),
//! a delta in order is one batch (`GridWriter::batch`); a gap sends `Resync`
//! once and drops deltas until a snapshot covers it.
//!
//! The grid store's reload API (`begin_reload`, `ReplayLive`) is not used:
//! it replays live writes made while a snapshot loads over another channel.
//! Here the snapshot and the deltas share one ordered stream, so every delta
//! before the snapshot is older than it (dropped) and every delta after it
//! is applied after it in order; nothing is left to replay.

mod client;
mod message;

pub use client::{FeedAction, FeedClient, FeedState};
pub use message::{ClientMessage, DecodeError, PROTOCOL_VERSION, QuoteRecord, ServerMessage};
