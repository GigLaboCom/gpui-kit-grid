//! The latest quotes and, per client, the keys changed since its last
//! message. Memory is bounded by the number of keys: a client that reads
//! slowly gets the latest value of each changed key, never a queue.

use gpui_kit_grid_feed::QuoteRecord;
use parking_lot::Mutex;

/// A client's place in the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(usize);

/// The keys changed since a client's last message: at most one entry per
/// key, however often the key changes.
#[derive(Debug)]
pub struct Conflator {
    marked: Vec<bool>,
    keys: Vec<u32>,
}

impl Conflator {
    /// For keys `0..len`.
    pub fn new(len: usize) -> Self {
        Self {
            marked: vec![false; len],
            keys: Vec::new(),
        }
    }

    /// `key` changed. A key outside the range is ignored.
    pub fn mark(&mut self, key: u32) {
        if let Some(marked) = self.marked.get_mut(key as usize)
            && !*marked
        {
            *marked = true;
            self.keys.push(key);
        }
    }

    /// Appends the latest value of every changed key to `out`, in the order
    /// the keys first changed, and forgets them.
    pub fn take(&mut self, latest: &[QuoteRecord], out: &mut Vec<QuoteRecord>) {
        for key in self.keys.drain(..) {
            self.marked[key as usize] = false;
            out.push(latest[key as usize]);
        }
    }

    /// Forgets every change: a snapshot covers them.
    pub fn clear(&mut self) {
        for key in self.keys.drain(..) {
            self.marked[key as usize] = false;
        }
    }
}

struct State {
    /// The latest quote of each key, by key.
    quotes: Vec<QuoteRecord>,
    /// Price updates published since the start, repeats included.
    produced: u64,
    sessions: Vec<Option<Conflator>>,
}

/// What the simulator thread and the client threads share.
pub struct Hub(Mutex<State>);

impl Hub {
    /// The first quotes, keyed `0..quotes.len()`.
    pub fn new(quotes: Vec<QuoteRecord>) -> Self {
        Self(Mutex::new(State {
            quotes,
            produced: 0,
            sessions: Vec::new(),
        }))
    }

    /// Price updates: each replaces its key's quote and marks the key
    /// changed for every client. A key outside the first quotes is ignored.
    pub fn publish(&self, updates: impl IntoIterator<Item = QuoteRecord>) {
        let mut state = self.0.lock();
        let State {
            quotes,
            produced,
            sessions,
        } = &mut *state;
        for update in updates {
            *produced += 1;
            let Some(quote) = quotes.get_mut(update.id as usize) else {
                continue;
            };
            *quote = update;
            for conflator in sessions.iter_mut().flatten() {
                conflator.mark(update.id);
            }
        }
    }

    /// A new client, with nothing changed yet.
    pub fn join(&self) -> SessionId {
        let mut state = self.0.lock();
        let conflator = Conflator::new(state.quotes.len());
        let slot = state.sessions.iter().position(Option::is_none);
        let ix = match slot {
            Some(ix) => {
                state.sessions[ix] = Some(conflator);
                ix
            }
            None => {
                state.sessions.push(Some(conflator));
                state.sessions.len() - 1
            }
        };
        SessionId(ix)
    }

    /// The client is gone.
    pub fn leave(&self, id: SessionId) {
        if let Some(slot) = self.0.lock().sessions.get_mut(id.0) {
            *slot = None;
        }
    }

    /// Every quote, for a snapshot; the client's changes are forgotten.
    pub fn snapshot(&self, id: SessionId) -> Vec<QuoteRecord> {
        let mut state = self.0.lock();
        if let Some(Some(conflator)) = state.sessions.get_mut(id.0) {
            conflator.clear();
        }
        state.quotes.clone()
    }

    /// Appends the latest value of each key changed since the client's last
    /// message to `out`.
    pub fn take_changes(&self, id: SessionId, out: &mut Vec<QuoteRecord>) {
        let mut state = self.0.lock();
        let State {
            quotes, sessions, ..
        } = &mut *state;
        if let Some(Some(conflator)) = sessions.get_mut(id.0) {
            conflator.take(quotes, out);
        }
    }

    /// Price updates published since the start.
    pub fn produced(&self) -> u64 {
        self.0.lock().produced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(id: u32, last: f64) -> QuoteRecord {
        QuoteRecord {
            id,
            last,
            bid: last - 0.01,
            ask: last + 0.01,
            volume: 0,
        }
    }

    #[test]
    fn test_conflation_keeps_the_latest_value_per_key() {
        let hub = Hub::new((0..4).map(|id| quote(id, 10.)).collect());
        let session = hub.join();
        // A thousand updates of two keys, and one unknown key.
        hub.publish((0..1000).map(|n| quote(n % 2 * 3, f64::from(n))));
        hub.publish([quote(9, 1.)]);
        assert_eq!(hub.produced(), 1001);
        let mut changes = Vec::new();
        hub.take_changes(session, &mut changes);
        assert_eq!(changes, [quote(0, 998.), quote(3, 999.)]);
        // Taken once.
        changes.clear();
        hub.take_changes(session, &mut changes);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_conflator_is_bounded_by_the_keys() {
        let mut conflator = Conflator::new(150);
        for n in 0..100_000u32 {
            conflator.mark(n % 150);
            conflator.mark(1_000 + n);
        }
        assert_eq!(conflator.keys.len(), 150);
        assert!(conflator.keys.capacity() < 512);
    }

    #[test]
    fn test_sessions_conflate_apart() {
        let hub = Hub::new((0..3).map(|id| quote(id, 10.)).collect());
        let (a, b) = (hub.join(), hub.join());
        hub.publish([quote(1, 11.)]);
        let mut changes = Vec::new();
        hub.take_changes(a, &mut changes);
        assert_eq!(changes, [quote(1, 11.)]);
        hub.publish([quote(2, 12.)]);
        // A snapshot forgets b's changes and has every quote.
        assert_eq!(
            hub.snapshot(b),
            [quote(0, 10.), quote(1, 11.), quote(2, 12.)]
        );
        changes.clear();
        hub.take_changes(b, &mut changes);
        assert!(changes.is_empty());
        changes.clear();
        hub.take_changes(a, &mut changes);
        assert_eq!(changes, [quote(2, 12.)]);
        // A slot left is reused.
        hub.leave(a);
        assert_eq!(hub.join(), a);
    }
}
