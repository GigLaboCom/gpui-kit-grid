//! One client's sequence numbers, and the fault injection that drops
//! deltas. Free of sockets.

use gpui_kit_grid_feed::{QuoteRecord, ServerMessage};

/// A connection's numbering: snapshots and deltas take consecutive
/// sequence numbers from the client's last one plus one; heartbeats repeat
/// the last number taken.
#[derive(Debug)]
pub struct Session {
    seq: u64,
    /// Every Nth delta is numbered but not sent (`--drop-every`).
    drop_every: Option<u64>,
    deltas: u64,
}

impl Session {
    /// After `Hello` with the client's `last_seq`.
    pub fn new(last_seq: Option<u64>, drop_every: Option<u64>) -> Self {
        Self {
            seq: last_seq.unwrap_or(0),
            drop_every: drop_every.filter(|every| *every > 0),
            deltas: 0,
        }
    }

    /// Every row, as of the next number.
    pub fn snapshot(&mut self, rows: Vec<QuoteRecord>, time_ms: u64) -> ServerMessage {
        self.seq += 1;
        ServerMessage::Snapshot {
            seq: self.seq,
            time_ms,
            rows,
        }
    }

    /// The changed rows under the next number; `None` when fault injection
    /// drops this delta, which the client sees as a gap.
    pub fn delta(&mut self, upserts: Vec<QuoteRecord>, time_ms: u64) -> Option<ServerMessage> {
        self.seq += 1;
        self.deltas += 1;
        if self
            .drop_every
            .is_some_and(|every| self.deltas.is_multiple_of(every))
        {
            return None;
        }
        Some(ServerMessage::Delta {
            seq: self.seq,
            time_ms,
            upserts,
            removes: Vec::new(),
        })
    }

    /// The last number taken, sent or dropped.
    pub fn heartbeat(&self, time_ms: u64) -> ServerMessage {
        ServerMessage::Heartbeat {
            seq: self.seq,
            time_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use gpui_kit_grid_feed::{ClientMessage, FeedAction, FeedClient, FeedState};

    use super::*;
    use crate::{hub::Hub, record, trading::market::Market};

    #[test]
    fn test_numbers_continue_from_the_client() {
        let mut session = Session::new(Some(41), None);
        assert_eq!(session.snapshot(Vec::new(), 0).seq(), 42);
        assert_eq!(session.delta(Vec::new(), 0).map(|m| m.seq()), Some(43));
        assert_eq!(session.heartbeat(0).seq(), 43);
        assert_eq!(Session::new(None, None).snapshot(Vec::new(), 0).seq(), 1);
    }

    #[test]
    fn test_drop_every_numbers_the_dropped_delta() {
        let mut session = Session::new(None, Some(3));
        session.snapshot(Vec::new(), 0);
        let sent = (0..7)
            .map(|_| session.delta(Vec::new(), 0).map(|m| m.seq()))
            .collect::<Vec<_>>();
        assert_eq!(
            sent,
            [Some(2), Some(3), None, Some(5), Some(6), None, Some(8)]
        );
        // The heartbeat shows the dropped number.
        session.delta(Vec::new(), 0);
        assert_eq!(session.heartbeat(0).seq(), 9);
    }

    /// The simulator, the hub, a session dropping every fifth delta and the
    /// client's state machine over the encoded bytes: the client resyncs
    /// after each drop and ends with the simulator's quotes.
    #[test]
    fn test_client_recovers_from_dropped_deltas() {
        let mut market = Market::new(Some(7));
        let hub = Hub::new(market.quotes().iter().map(record).collect());
        let id = hub.join();
        let mut session = Session::new(None, Some(5));
        let mut client = FeedClient::new();
        let mut rows = vec![None; market.quotes().len()];
        // The ordered stream to the client.
        let mut stream = VecDeque::new();

        let hello = ClientMessage::decode(&client.on_open().to_bytes()).unwrap();
        assert!(matches!(hello, ClientMessage::Hello { last_seq: None, .. }));
        stream.push_back(session.snapshot(hub.snapshot(id), 0).to_bytes());
        let mut changed = Vec::new();
        for round in 0..=60 {
            if round < 60 {
                changed.clear();
                market.step_quotes(20, 2_000., &mut changed);
                hub.publish(changed.iter().map(|id| record(market.quote(*id))));
                let mut upserts = Vec::new();
                hub.take_changes(id, &mut upserts);
                assert!(!upserts.is_empty() && upserts.len() <= 20);
                stream.extend(session.delta(upserts, 0).map(|m| m.to_bytes()));
            } else {
                // A heartbeat shows a dropped last delta.
                stream.push_back(session.heartbeat(0).to_bytes());
            }
            while let Some(bytes) = stream.pop_front() {
                let message = ServerMessage::decode(&bytes).unwrap();
                match client.on_message(message) {
                    Some(FeedAction::ReplaceAll(records)) => {
                        rows.fill(None);
                        for record in records {
                            rows[record.id as usize] = Some(record);
                        }
                    }
                    Some(FeedAction::Batch { upserts, .. }) => {
                        for record in upserts {
                            rows[record.id as usize] = Some(record);
                        }
                    }
                    Some(FeedAction::Send(message)) => {
                        let request = ClientMessage::decode(&message.to_bytes());
                        assert_eq!(request, Ok(ClientMessage::Resync));
                        stream.push_back(session.snapshot(hub.snapshot(id), 0).to_bytes());
                    }
                    None => {}
                }
            }
        }
        assert_eq!(client.state(), FeedState::Live);
        assert_eq!(client.resyncs(), 12);
        let expected = market.quotes().iter().map(record).collect::<Vec<_>>();
        assert_eq!(rows.into_iter().flatten().collect::<Vec<_>>(), expected);
    }
}
