//! The client's sequence state machine.

use crate::message::{ClientMessage, QuoteRecord, ServerMessage};

/// Where a [`FeedClient`] is, for a status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedState {
    /// No connection: before the first, or after one closed.
    Offline,
    /// Connected, `Hello` sent, waiting for the first snapshot.
    Connecting,
    /// Applying deltas in order.
    Live,
    /// A gap: `Resync` sent, deltas dropped until a snapshot covers it.
    Resyncing,
}

impl FeedState {
    /// A lowercase word for the state.
    pub fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Connecting => "connecting",
            Self::Live => "live",
            Self::Resyncing => "resyncing",
        }
    }
}

/// What to do with a message.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedAction {
    /// A snapshot: replace every row (`GridWriter::replace_all`).
    ReplaceAll(Vec<QuoteRecord>),
    /// A delta in order: upsert these rows and remove these keys in one
    /// batch (`GridWriter::batch`).
    Batch {
        upserts: Vec<QuoteRecord>,
        removes: Vec<u32>,
    },
    /// Send this to the server.
    Send(ClientMessage),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Offline,
    Connecting,
    Live,
    /// A snapshot is accepted from sequence number `covers` on: the newest
    /// number seen since the gap.
    Resyncing {
        covers: u64,
    },
}

/// The client's side of the feed, free of any socket: tell it when the
/// connection opens and closes and what arrives; it answers with what to
/// apply and what to send.
///
/// - A snapshot is applied when it is the first on a connection, when it
///   covers a gap, or when it is newer than the last applied number.
/// - A delta is applied when its number is the next one; an older or
///   repeated number is ignored; a newer one is a gap.
/// - A heartbeat newer than the last applied number is a gap too: the
///   deltas up to it were lost.
/// - A gap sends `Resync` once and drops deltas until a snapshot covers it.
#[derive(Clone, Debug)]
pub struct FeedClient {
    phase: Phase,
    token: Option<String>,
    /// The last sequence number applied, kept across connections for
    /// `Hello`.
    last_seq: Option<u64>,
    snapshots: u64,
    deltas: u64,
    resyncs: u64,
    ignored: u64,
}

impl Default for FeedClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedClient {
    /// A client before its first connection.
    pub fn new() -> Self {
        Self {
            phase: Phase::Offline,
            token: None,
            last_seq: None,
            snapshots: 0,
            deltas: 0,
            resyncs: 0,
            ignored: 0,
        }
    }

    /// Sends `token` in every `Hello`.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The connection opened: returns the `Hello` to send.
    pub fn on_open(&mut self) -> ClientMessage {
        self.phase = Phase::Connecting;
        ClientMessage::Hello {
            last_seq: self.last_seq,
            token: self.token.clone(),
        }
    }

    /// The connection closed or failed.
    pub fn on_close(&mut self) {
        self.phase = Phase::Offline;
    }

    /// A message arrived: what to apply or send, if anything.
    pub fn on_message(&mut self, message: ServerMessage) -> Option<FeedAction> {
        let last = self.last_seq.unwrap_or(0);
        match (self.phase, message) {
            (Phase::Offline, _) => self.ignore(),
            (phase, ServerMessage::Snapshot { seq, rows, .. }) => {
                let accepted = match phase {
                    Phase::Connecting => true,
                    Phase::Resyncing { covers } => seq >= covers,
                    _ => self.last_seq.is_none_or(|last| seq > last),
                };
                if !accepted {
                    return self.ignore();
                }
                self.phase = Phase::Live;
                self.last_seq = Some(seq);
                self.snapshots += 1;
                Some(FeedAction::ReplaceAll(rows))
            }
            (Phase::Connecting, _) => self.ignore(),
            (Phase::Resyncing { covers }, message) => {
                self.phase = Phase::Resyncing {
                    covers: covers.max(message.seq()),
                };
                self.ignore()
            }
            (
                Phase::Live,
                ServerMessage::Delta {
                    seq,
                    upserts,
                    removes,
                    ..
                },
            ) => {
                if seq <= last {
                    self.ignore()
                } else if seq == last + 1 {
                    self.last_seq = Some(seq);
                    self.deltas += 1;
                    Some(FeedAction::Batch { upserts, removes })
                } else {
                    self.gap(seq)
                }
            }
            (Phase::Live, ServerMessage::Heartbeat { seq, .. }) => {
                if seq > last {
                    self.gap(seq)
                } else {
                    None
                }
            }
        }
    }

    /// Bytes that are not a message arrived: the state may have missed a
    /// delta, so resynchronise (while live).
    pub fn on_malformed(&mut self) -> Option<FeedAction> {
        match self.phase {
            Phase::Live => self.gap(self.last_seq.unwrap_or(0) + 1),
            _ => None,
        }
    }

    /// Where the client is.
    pub fn state(&self) -> FeedState {
        match self.phase {
            Phase::Offline => FeedState::Offline,
            Phase::Connecting => FeedState::Connecting,
            Phase::Live => FeedState::Live,
            Phase::Resyncing { .. } => FeedState::Resyncing,
        }
    }

    /// The last sequence number applied, if any.
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// Snapshots applied.
    pub fn snapshots(&self) -> u64 {
        self.snapshots
    }

    /// Deltas applied.
    pub fn deltas(&self) -> u64 {
        self.deltas
    }

    /// Gaps that sent `Resync`.
    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }

    /// Messages ignored: old, repeated, dropped during a resync, or
    /// before the first snapshot.
    pub fn ignored(&self) -> u64 {
        self.ignored
    }

    fn gap(&mut self, seen: u64) -> Option<FeedAction> {
        self.phase = Phase::Resyncing { covers: seen };
        self.resyncs += 1;
        self.ignored += 1;
        Some(FeedAction::Send(ClientMessage::Resync))
    }

    fn ignore(&mut self) -> Option<FeedAction> {
        self.ignored += 1;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32, last: f64) -> QuoteRecord {
        QuoteRecord {
            id,
            last,
            bid: last - 0.01,
            ask: last + 0.01,
            volume: 100,
        }
    }

    fn snapshot(seq: u64) -> ServerMessage {
        ServerMessage::Snapshot {
            seq,
            time_ms: 0,
            rows: vec![record(0, 10.), record(1, 20.)],
        }
    }

    fn delta(seq: u64) -> ServerMessage {
        ServerMessage::Delta {
            seq,
            time_ms: 0,
            upserts: vec![record(0, seq as f64)],
            removes: Vec::new(),
        }
    }

    fn heartbeat(seq: u64) -> ServerMessage {
        ServerMessage::Heartbeat { seq, time_ms: 0 }
    }

    fn live_at(seq: u64) -> FeedClient {
        let mut client = FeedClient::new();
        client.on_open();
        assert!(matches!(
            client.on_message(snapshot(seq)),
            Some(FeedAction::ReplaceAll(_))
        ));
        client
    }

    #[test]
    fn test_hello_carries_the_last_seq_and_token() {
        let mut client = FeedClient::new().with_token("t");
        assert_eq!(client.state(), FeedState::Offline);
        assert_eq!(
            client.on_open(),
            ClientMessage::Hello {
                last_seq: None,
                token: Some("t".into())
            }
        );
        assert_eq!(client.state(), FeedState::Connecting);
        client.on_message(snapshot(1));
        client.on_message(delta(2));
        client.on_close();
        assert_eq!(client.state(), FeedState::Offline);
        assert_eq!(
            client.on_open(),
            ClientMessage::Hello {
                last_seq: Some(2),
                token: Some("t".into())
            }
        );
    }

    #[test]
    fn test_deltas_before_the_first_snapshot_are_dropped() {
        let mut client = FeedClient::new();
        assert_eq!(client.on_message(delta(1)), None, "offline");
        client.on_open();
        assert_eq!(client.on_message(delta(5)), None);
        assert_eq!(client.on_message(heartbeat(5)), None);
        assert!(matches!(
            client.on_message(snapshot(6)),
            Some(FeedAction::ReplaceAll(rows)) if rows.len() == 2
        ));
        assert_eq!(client.state(), FeedState::Live);
        assert_eq!(client.ignored(), 3);
    }

    #[test]
    fn test_deltas_in_order_are_batches() {
        let mut client = live_at(1);
        for seq in 2..10 {
            assert_eq!(
                client.on_message(delta(seq)),
                Some(FeedAction::Batch {
                    upserts: vec![record(0, seq as f64)],
                    removes: Vec::new()
                })
            );
            assert_eq!(client.on_message(heartbeat(seq)), None);
        }
        assert_eq!(client.last_seq(), Some(9));
        assert_eq!((client.snapshots(), client.deltas()), (1, 8));
    }

    #[test]
    fn test_duplicate_and_old_seq_are_ignored() {
        let mut client = live_at(1);
        client.on_message(delta(2));
        client.on_message(delta(3));
        assert_eq!(client.on_message(delta(3)), None, "repeated");
        assert_eq!(client.on_message(delta(2)), None, "old");
        assert_eq!(client.on_message(snapshot(3)), None, "old snapshot");
        assert_eq!(client.on_message(heartbeat(1)), None);
        assert_eq!(client.state(), FeedState::Live);
        assert_eq!(client.last_seq(), Some(3));
        assert_eq!((client.ignored(), client.resyncs()), (3, 0));
        // The next one still applies.
        assert!(client.on_message(delta(4)).is_some());
    }

    #[test]
    fn test_gap_resyncs_once_and_waits_for_a_covering_snapshot() {
        let mut client = live_at(1);
        client.on_message(delta(2));
        // 3 is lost.
        assert_eq!(
            client.on_message(delta(4)),
            Some(FeedAction::Send(ClientMessage::Resync))
        );
        assert_eq!(client.state(), FeedState::Resyncing);
        // Deltas until the snapshot are dropped, and no second Resync.
        assert_eq!(client.on_message(delta(5)), None);
        assert_eq!(client.on_message(delta(7)), None);
        assert_eq!(client.on_message(heartbeat(7)), None);
        // A snapshot older than what was seen does not cover the gap.
        assert_eq!(client.on_message(snapshot(6)), None);
        assert!(matches!(
            client.on_message(snapshot(8)),
            Some(FeedAction::ReplaceAll(_))
        ));
        assert_eq!(client.state(), FeedState::Live);
        assert!(client.on_message(delta(9)).is_some());
        assert_eq!(client.resyncs(), 1);
    }

    #[test]
    fn test_heartbeat_ahead_is_a_gap() {
        let mut client = live_at(1);
        client.on_message(delta(2));
        // The last delta, 3, was lost; the heartbeat says 3 was taken.
        assert_eq!(
            client.on_message(heartbeat(3)),
            Some(FeedAction::Send(ClientMessage::Resync))
        );
        assert!(client.on_message(snapshot(4)).is_some());
        assert_eq!(client.last_seq(), Some(4));
    }

    #[test]
    fn test_malformed_message_resyncs_while_live() {
        let mut client = FeedClient::new();
        assert_eq!(client.on_malformed(), None);
        let mut client = live_at(1);
        assert_eq!(
            client.on_malformed(),
            Some(FeedAction::Send(ClientMessage::Resync))
        );
        assert_eq!(client.on_malformed(), None, "already resyncing");
        assert!(client.on_message(snapshot(2)).is_some());
    }

    #[test]
    fn test_reconnect_takes_any_first_snapshot() {
        let mut client = live_at(10);
        client.on_close();
        client.on_open();
        // A restarted server may number from 1 again.
        assert!(client.on_message(snapshot(1)).is_some());
        assert!(client.on_message(delta(2)).is_some());
    }
}
