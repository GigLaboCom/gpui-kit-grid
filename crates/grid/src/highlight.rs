//! Change highlight: packed stamps, the sparse stamps of a row and the fade
//! policy. No GPUI types.
//!
//! Stamps are written by the ingest task for every operation. The view reads
//! them for visible cells only and fades them on one global tick, so every
//! hot cell changes its step on the same frames. A row keeps only the
//! stamps that are still hot.

use std::{sync::Arc, time::Duration};

use crate::model::{ColumnIx, ColumnSet};

/// What happened to the row when the stamp was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowChange {
    /// The row was inserted.
    Inserted,
    /// The row was replaced and this column changed.
    Updated,
    /// The row was removed and fades out
    /// ([`HighlightPolicy::with_removal_fade`]). Never stored in a cell
    /// stamp: a fading row ([`StoreSnapshot::fading_row`]) carries its
    /// removal stamp, which reads back as `Updated` without a direction.
    ///
    /// [`StoreSnapshot::fading_row`]: crate::StoreSnapshot::fading_row
    Removed,
}

/// The direction of a changed numeric value (`GridRow::change_direction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeDirection {
    /// The value rose.
    Up,
    /// The value fell.
    Down,
}

/// A packed change stamp: milliseconds since the store epoch in the upper 30
/// bits, the change in bits 0–1: `00` updated without a direction, `01`
/// inserted, `10` updated up, `11` updated down. Zero means "never changed".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp(u32);

impl Stamp {
    /// No change recorded.
    pub const NONE: Self = Self(0);
    const MAX_MS: u32 = (1 << 30) - 1;
    const CHANGE_BITS: u32 = 0b11;
    const INSERTED: u32 = 0b01;
    const UP: u32 = 0b10;
    const DOWN: u32 = 0b11;

    /// Creates a stamp of `change` without a direction. `ms` wraps after ~12
    /// days of store uptime.
    pub fn new(ms: u32, change: RowChange) -> Self {
        Self::with_direction(ms, change, None)
    }

    /// Creates a stamp of `change` with a direction; `direction` counts only
    /// for [`RowChange::Updated`]. `ms` wraps after ~12 days of store uptime.
    ///
    /// [`RowChange::Removed`] is not a cell's change: the store writes a
    /// removal stamp for a fading row itself (a debug assertion).
    pub fn with_direction(ms: u32, change: RowChange, direction: Option<ChangeDirection>) -> Self {
        debug_assert!(
            change != RowChange::Removed,
            "a removal stamp is written by the store for a fading row"
        );
        let bits = match (change, direction) {
            (RowChange::Inserted, _) => Self::INSERTED,
            (RowChange::Updated, Some(ChangeDirection::Up)) => Self::UP,
            (RowChange::Updated, Some(ChangeDirection::Down)) => Self::DOWN,
            _ => 0,
        };
        Self::packed(ms, bits)
    }

    /// The removal stamp of a fading row: the time of removal, read back as
    /// `Updated` without a direction.
    pub(crate) fn removal(ms: u32) -> Self {
        Self::packed(ms, 0)
    }

    fn packed(ms: u32, bits: u32) -> Self {
        // Never zero, which is `NONE`.
        let ms = (ms & Self::MAX_MS).max(1);
        Self((ms << 2) | bits)
    }

    /// Returns true if a change was recorded.
    pub fn is_set(self) -> bool {
        self.0 != 0
    }

    /// Milliseconds since the store epoch.
    pub fn ms(self) -> u32 {
        self.0 >> 2
    }

    /// What happened to the row.
    pub fn change(self) -> RowChange {
        if self.0 & Self::CHANGE_BITS == Self::INSERTED {
            RowChange::Inserted
        } else {
            RowChange::Updated
        }
    }

    /// The direction of an update; `None` for an insert or an update with
    /// no direction.
    pub fn direction(self) -> Option<ChangeDirection> {
        match self.0 & Self::CHANGE_BITS {
            Self::UP => Some(ChangeDirection::Up),
            Self::DOWN => Some(ChangeDirection::Down),
            _ => None,
        }
    }
}

/// The change stamps of one row's cells while any of them is hot
/// ([`StoreSnapshot::stamps`]).
///
/// The store keeps them sparse: a stamp for the whole row (the flash of an
/// insert) and the stamps of the columns written since, without the ones
/// that expired. [`Self::get`] reads the stamp of any cell in O(1), without
/// allocating.
///
/// [`StoreSnapshot::stamps`]: crate::StoreSnapshot::stamps
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowStamps<'a> {
    /// The row's stamp, the column mask and the places of its words
    /// ([`SparseStamps`]).
    header: &'a [Stamp; HEADER],
    /// One stamp per stamped column, in schema order.
    stamps: &'a [Stamp],
}

impl RowStamps<'_> {
    const EMPTY: RowStamps<'static> = RowStamps {
        header: &[Stamp::NONE; HEADER],
        stamps: &[],
    };

    /// The stamp of the cell of `column`: its own if the column was written
    /// since the row's stamp, else the row's; [`Stamp::NONE`] if neither.
    #[inline]
    pub fn get(self, column: ColumnIx) -> Stamp {
        let (ix, row) = (column.get(), self.header[ROW]);
        if ix >= MAX_COLUMNS {
            return row;
        }
        // One 32-bit word of the mask, and the place of its first stamp:
        // no `u128` arithmetic on the paint path.
        let word = ix / 32;
        let (mask, bit) = (self.header[MASK + word].0, 1u32 << (ix % 32));
        if mask & bit == 0 {
            return row;
        }
        let first = (self.header[PLACES].0 >> (8 * word)) & 0xff;
        let rank = first + (mask & (bit - 1)).count_ones();
        self.stamps.get(rank as usize).copied().unwrap_or(row)
    }

    /// The stamp of every cell without one of its own.
    fn row(self) -> Stamp {
        self.header[ROW]
    }

    /// The columns with a stamp of their own.
    fn columns(self) -> ColumnSet {
        let bits = (0..MASK_WORDS).fold(0u128, |bits, word| {
            bits | u128::from(self.header[MASK + word].0) << (32 * word)
        });
        ColumnSet::from_bits(bits)
    }
}

/// The header of [`SparseStamps`]: the whole row's stamp, the column mask
/// in four words (the low word first), and the places: byte `w` is the
/// number of stamped columns in the words before word `w`.
const ROW: usize = 0;
const MASK: usize = 1;
const MASK_WORDS: usize = 4;
const PLACES: usize = MASK + MASK_WORDS;
const HEADER: usize = PLACES + 1;
/// The most columns a row has ([`ColumnSet`]).
const MAX_COLUMNS: usize = 32 * MASK_WORDS;

/// The change stamps of a hot row as the store keeps them: one allocation of
/// a header (the row's stamp and the column mask) and one stamp per column
/// of the mask, in schema order.
///
/// An insert is the row stamp alone. A write keeps the stamps of the
/// columns still hot and drops the expired ones, so a row holds only what
/// can still show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SparseStamps(Arc<[Stamp]>);

impl SparseStamps {
    /// Every cell of the row stamped `stamp`: the flash of an insert.
    pub(crate) fn whole_row(stamp: Stamp) -> Self {
        Self(Arc::new(Self::header(stamp, ColumnSet::EMPTY)))
    }

    /// `existing` (or none) with each of `columns` stamped
    /// `stamp(column)`, keeping of `existing` only the stamps `is_hot`
    /// keeps. `every` is every column of the row; a column outside it is
    /// not stamped. One allocation; `None` if no stamp is left.
    pub(crate) fn written(
        existing: Option<&Self>,
        columns: ColumnSet,
        every: ColumnSet,
        is_hot: impl Fn(Stamp) -> bool,
        mut stamp: impl FnMut(ColumnIx) -> Stamp,
    ) -> Option<Self> {
        let columns = columns.intersection(every);
        let old = existing.map_or(RowStamps::EMPTY, Self::view);
        let old_columns = old.columns();
        let mut touched = old_columns;
        touched.union_with(columns);
        // Built on the stack, then copied once into the allocation.
        let mut words = [Stamp::NONE; HEADER + MAX_COLUMNS];
        let (mut kept, mut count, mut old_ix) = (ColumnSet::EMPTY, 0, 0);
        for column in touched.iter() {
            let own = old_columns.contains(column).then(|| {
                old_ix += 1;
                old.stamps.get(old_ix - 1).copied().unwrap_or(Stamp::NONE)
            });
            let value = if columns.contains(column) {
                stamp(column)
            } else {
                match own {
                    Some(own) if is_hot(own) => own,
                    _ => continue,
                }
            };
            words[HEADER + count] = value;
            kept.insert(column);
            count += 1;
        }
        let mut row = if is_hot(old.row()) {
            old.row()
        } else {
            Stamp::NONE
        };
        if kept == every {
            // Every cell has its own stamp: the row's is never read, and
            // the same stamp in every cell is the row's alone.
            let first = words[HEADER];
            let uniform = words[HEADER..HEADER + count]
                .iter()
                .all(|&stamp| stamp == first);
            (row, kept, count) = if uniform {
                (first, ColumnSet::EMPTY, 0)
            } else {
                (Stamp::NONE, kept, count)
            };
        }
        if count == 0 && !row.is_set() {
            return None;
        }
        words[..HEADER].copy_from_slice(&Self::header(row, kept));
        Some(Self(Arc::from(&words[..HEADER + count])))
    }

    fn header(row: Stamp, columns: ColumnSet) -> [Stamp; HEADER] {
        let bits = columns.bits();
        let mut header = [Stamp::NONE; HEADER];
        header[ROW] = row;
        let mut before = 0;
        for word in 0..MASK_WORDS {
            let mask = (bits >> (32 * word)) as u32;
            header[MASK + word] = Stamp(mask);
            // At most 96 columns before the last word: a byte holds it.
            header[PLACES].0 |= before << (8 * word);
            before += mask.count_ones();
        }
        header
    }

    /// The stamps for reading.
    #[inline]
    pub(crate) fn view(&self) -> RowStamps<'_> {
        match self.0.split_first_chunk::<HEADER>() {
            Some((header, stamps)) => RowStamps { header, stamps },
            None => RowStamps::EMPTY,
        }
    }

    /// Returns true if no stamp of the row is hot: the store frees them.
    pub(crate) fn is_cold(&self, is_hot: impl Fn(Stamp) -> bool) -> bool {
        let stamps = self.view();
        !is_hot(stamps.row()) && !stamps.stamps.iter().any(|&stamp| is_hot(stamp))
    }

    /// The bytes of the allocation, with its reference counts.
    pub(crate) fn heap_bytes(&self) -> usize {
        2 * size_of::<usize>() + size_of_val::<[Stamp]>(&self.0)
    }
}

/// How long a change stays highlighted, in how many steps it fades, and how
/// long a removed row fades out.
///
/// This is the default implementation of the `HighlightPolicy` seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightPolicy {
    duration_ms: u32,
    steps: u32,
    removal_fade_ms: u32,
}

impl Default for HighlightPolicy {
    fn default() -> Self {
        Self::new(Duration::from_secs(5), 8)
    }
}

impl HighlightPolicy {
    /// A highlight that lasts `duration` and fades in `steps` steps. Removed
    /// rows go at once.
    pub fn new(duration: Duration, steps: u32) -> Self {
        // Ages live in the 30-bit stamp ring; keep durations well inside it.
        let duration_ms = duration.as_millis().clamp(1, u128::from(Stamp::MAX_MS / 4)) as u32;
        let steps = steps.clamp(1, duration_ms);
        Self {
            duration_ms,
            steps,
            removal_fade_ms: 0,
        }
    }

    /// Keeps a removed row on screen, fading, for `fade`, on the global tick
    /// and steps of this policy. `Duration::ZERO`, the default, removes at
    /// once. A store setting, like the duration: the store keeps the row in
    /// its snapshots while it fades ([`StoreSnapshot::fading_row`]).
    ///
    /// [`StoreSnapshot::fading_row`]: crate::StoreSnapshot::fading_row
    pub fn with_removal_fade(mut self, fade: Duration) -> Self {
        self.removal_fade_ms = fade.as_millis().min(u128::from(Stamp::MAX_MS / 4)) as u32;
        self
    }

    /// How long a removed row fades out; zero when it goes at once.
    pub fn removal_fade(&self) -> Duration {
        Duration::from_millis(self.removal_fade_ms.into())
    }

    /// The highlight duration in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    /// The number of fade steps.
    pub fn steps(&self) -> u32 {
        self.steps
    }

    fn step_ms(&self) -> u32 {
        (self.duration_ms / self.steps).max(1)
    }

    /// The global tick at or before `now_ms`. All cells are evaluated at the
    /// same tick, so they change steps on the same frames.
    pub fn tick(&self, now_ms: u32) -> u32 {
        now_ms - now_ms % self.step_ms()
    }

    /// The next global tick after `now_ms`: the single deadline the view
    /// waits for while any visible cell is hot.
    pub fn next_tick(&self, now_ms: u32) -> u32 {
        self.tick(now_ms) + self.step_ms()
    }

    /// The age of `stamp` at `now_ms`, computed in the 30-bit ring of the
    /// stamp clock, so it stays right after the clock wraps (~12 days).
    /// A stamp written after `now_ms` (ticks are floored) has age 0.
    fn age(stamp: Stamp, now_ms: u32) -> u32 {
        let age = (now_ms & Stamp::MAX_MS).wrapping_sub(stamp.ms()) & Stamp::MAX_MS;
        if age > Stamp::MAX_MS / 2 { 0 } else { age }
    }

    /// Highlight intensity at `tick_ms`, from 1.0 (just changed) down to
    /// `1 / steps`; `None` once the highlight expired.
    pub fn intensity(&self, stamp: Stamp, tick_ms: u32) -> Option<f32> {
        if !stamp.is_set() {
            return None;
        }
        let age = Self::age(stamp, tick_ms);
        if age >= self.duration_ms {
            return None;
        }
        // Never 0.0: a hot cell is always visible.
        let step = (age / self.step_ms()).min(self.steps - 1);
        Some(1.0 - step as f32 / self.steps as f32)
    }

    /// Returns true if the stamp no longer highlights at `now_ms`.
    pub fn is_expired(&self, stamp: Stamp, now_ms: u32) -> bool {
        !stamp.is_set() || Self::age(stamp, now_ms) >= self.duration_ms
    }

    /// Returns true if `stamp` highlights at no tick from `now_ms` on: it
    /// expired at the global tick of `now_ms`, and a frame reads at that
    /// tick or a later one. The store drops such stamps; a stamp expired at
    /// `now_ms` itself may still show its last step until the next tick.
    pub(crate) fn is_highlight_over(&self, stamp: Stamp, now_ms: u32) -> bool {
        self.is_expired(stamp, self.tick(now_ms))
    }

    /// The intensity of a fading row with the removal stamp `removed` at
    /// `tick_ms`: from 1.0 (just removed) down in the steps of the global
    /// tick that fit in [`Self::removal_fade`]; `None` once the fade ended,
    /// and always without a removal fade.
    pub fn removal_intensity(&self, removed: Stamp, tick_ms: u32) -> Option<f32> {
        if self.is_removal_over(removed, tick_ms) {
            return None;
        }
        let step_ms = self.step_ms();
        let steps = self.removal_fade_ms.div_ceil(step_ms);
        // The age is below the fade, so the step is below `steps`: never 0.0.
        let step = Self::age(removed, tick_ms) / step_ms;
        Some(1.0 - step as f32 / steps as f32)
    }

    /// Returns true if a row removed at `removed` no longer fades at
    /// `now_ms`: the store drops it from its snapshots.
    pub(crate) fn is_removal_over(&self, removed: Stamp, now_ms: u32) -> bool {
        !removed.is_set() || Self::age(removed, now_ms) >= self.removal_fade_ms
    }

    /// Returns true if removed rows fade out.
    pub(crate) fn has_removal_fade(&self) -> bool {
        self.removal_fade_ms > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_policy_steps_on_global_tick() {
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        let stamp = Stamp::new(1_050, RowChange::Updated);
        assert_eq!(stamp.ms(), 1_050);
        assert_eq!(stamp.change(), RowChange::Updated);
        assert_eq!(
            Stamp::new(7, RowChange::Inserted).change(),
            RowChange::Inserted
        );

        // Ticks are aligned to 100 ms, whatever the stamp's own phase is.
        assert_eq!(policy.tick(1_099), 1_000);
        assert_eq!(policy.next_tick(1_099), 1_100);
        // A change after the current tick shows at full intensity.
        assert_eq!(policy.intensity(stamp, policy.tick(1_060)), Some(1.0));
        assert_eq!(policy.intensity(stamp, 1_200), Some(1.0 - 1.0 / 8.0));
        assert_eq!(policy.intensity(stamp, 1_849), Some(1.0 - 7.0 / 8.0));
        assert_eq!(policy.intensity(stamp, 1_850), None);
        assert!(policy.is_expired(stamp, 1_850));
        assert_eq!(policy.intensity(Stamp::NONE, 1_000), None);

        // The stamp clock wraps at 2^30 ms; ages stay right across the wrap.
        let wrap = 1u32 << 30;
        let late = Stamp::new(wrap - 100, RowChange::Updated);
        assert_eq!(policy.intensity(late, wrap + 200), Some(1.0 - 3.0 / 8.0));
        assert!(!policy.is_expired(late, wrap + 200));
        assert!(policy.is_expired(late, wrap + 800));

        // Steps that do not divide the duration never reach intensity 0.
        let uneven = HighlightPolicy::new(Duration::from_millis(1_000), 3);
        let stamp = Stamp::new(10, RowChange::Updated);
        assert!(uneven.intensity(stamp, 1_009).is_some_and(|i| i > 0.0));
    }

    #[test]
    fn test_stamp_direction_bits() {
        use ChangeDirection::{Down, Up};
        use RowChange::{Inserted, Updated};

        // Each of the four codes reads back, with the time unchanged.
        for (stamp, change, direction) in [
            (Stamp::with_direction(1_050, Updated, None), Updated, None),
            (Stamp::with_direction(1_050, Inserted, None), Inserted, None),
            (
                Stamp::with_direction(1_050, Updated, Some(Up)),
                Updated,
                Some(Up),
            ),
            (
                Stamp::with_direction(1_050, Updated, Some(Down)),
                Updated,
                Some(Down),
            ),
        ] {
            assert_eq!(stamp.ms(), 1_050);
            assert_eq!(stamp.change(), change);
            assert_eq!(stamp.direction(), direction);
            assert!(stamp.is_set());
        }
        // An insert has no direction; `new` writes none.
        assert_eq!(
            Stamp::with_direction(7, Inserted, Some(Up)),
            Stamp::new(7, Inserted)
        );
        assert_eq!(Stamp::new(7, Updated).direction(), None);
        assert_eq!(Stamp::NONE.direction(), None);
        assert!(!Stamp::NONE.is_set());
        // The removal stamp of a fading row reads as an update without a
        // direction, and is never `NONE`, even at the epoch.
        assert_eq!(Stamp::removal(1_050), Stamp::new(1_050, Updated));
        assert!(Stamp::removal(0).is_set());
        // The largest time keeps its direction.
        let max = Stamp::with_direction(Stamp::MAX_MS, Updated, Some(Down));
        assert_eq!((max.ms(), max.direction()), (Stamp::MAX_MS, Some(Down)));

        // Steps and expiry do not depend on the change bits.
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        let plain = Stamp::new(1_050, Updated);
        for stamp in [
            Stamp::new(1_050, Inserted),
            Stamp::with_direction(1_050, Updated, Some(Up)),
            Stamp::with_direction(1_050, Updated, Some(Down)),
        ] {
            for now in [1_060, 1_200, 1_849, 1_850, 5_000] {
                let tick = policy.tick(now);
                assert_eq!(policy.intensity(stamp, tick), policy.intensity(plain, tick));
                assert_eq!(policy.is_expired(stamp, now), policy.is_expired(plain, now));
            }
        }
    }

    #[test]
    fn test_removal_fade_steps_on_global_tick() {
        // Off by default: a removed row goes at once.
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        assert_eq!(policy.removal_fade(), Duration::ZERO);
        assert!(!policy.has_removal_fade());
        let removed = Stamp::removal(1_050);
        assert_eq!(policy.removal_intensity(removed, 1_050), None);
        assert!(policy.is_removal_over(removed, 1_050));

        // 250 ms on the 100 ms tick: three steps, then gone. The highlight
        // duration is not the fade's.
        let policy = policy.with_removal_fade(Duration::from_millis(250));
        assert_eq!(policy.removal_fade(), Duration::from_millis(250));
        assert!(policy.has_removal_fade());
        assert_eq!(policy.duration_ms(), 800);
        assert_eq!(
            policy.removal_intensity(removed, policy.tick(1_060)),
            Some(1.0)
        );
        assert_eq!(
            policy.removal_intensity(removed, 1_200),
            Some(1.0 - 1.0 / 3.0)
        );
        assert_eq!(
            policy.removal_intensity(removed, 1_299),
            Some(1.0 - 2.0 / 3.0)
        );
        assert!(!policy.is_removal_over(removed, 1_299));
        assert_eq!(policy.removal_intensity(removed, 1_300), None);
        assert!(policy.is_removal_over(removed, 1_300));
        assert_eq!(policy.removal_intensity(Stamp::NONE, 1_100), None);

        // A fade shorter than a step shows at full intensity until it ends.
        let short = policy.with_removal_fade(Duration::from_millis(40));
        assert_eq!(short.removal_intensity(removed, 1_089), Some(1.0));
        assert_eq!(short.removal_intensity(removed, 1_090), None);

        // Across the 2^30 ms wrap of the stamp clock.
        let wrap = 1u32 << 30;
        let late = Stamp::removal(wrap - 100);
        assert_eq!(
            policy.removal_intensity(late, wrap + 100),
            Some(1.0 - 2.0 / 3.0)
        );
        assert!(policy.is_removal_over(late, wrap + 150));

        // Back to zero: off again.
        let off = policy.with_removal_fade(Duration::ZERO);
        assert_eq!(off, HighlightPolicy::new(Duration::from_millis(800), 8));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a removal stamp is written by the store")]
    fn test_removed_is_not_a_cell_stamp() {
        Stamp::new(1_050, RowChange::Removed);
    }

    fn columns(ixs: &[u16]) -> ColumnSet {
        let mut set = ColumnSet::EMPTY;
        for &ix in ixs {
            set.insert(ColumnIx::new(ix));
        }
        set
    }

    /// The stamps of a row of `every` written at `now_ms` under `policy`.
    fn write(
        policy: HighlightPolicy,
        existing: Option<&SparseStamps>,
        written: &[u16],
        every: ColumnSet,
        now_ms: u32,
        stamp: Stamp,
    ) -> Option<SparseStamps> {
        SparseStamps::written(
            existing,
            columns(written),
            every,
            |stamp| !policy.is_highlight_over(stamp, now_ms),
            |_| stamp,
        )
    }

    /// The stamps a row holds: row stamp and column stamps, no header.
    fn held(stamps: &SparseStamps) -> usize {
        let view = stamps.view();
        usize::from(view.row().is_set()) + view.stamps.len()
    }

    #[test]
    fn test_sparse_stamps_keep_written_columns() {
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        let every = ColumnSet::first(40);
        let first = Stamp::new(10, RowChange::Updated);
        let stamps = write(policy, None, &[3, 7], every, 10, first).expect("stamped");
        let view = stamps.view();
        assert_eq!(view.get(ColumnIx::new(3)), first);
        assert_eq!(view.get(ColumnIx::new(7)), first);
        assert_eq!(view.get(ColumnIx::new(0)), Stamp::NONE);
        assert_eq!(view.get(ColumnIx::new(39)), Stamp::NONE);
        assert_eq!(held(&stamps), 2);
        // Header and two stamps in one allocation with its counts.
        assert_eq!(
            stamps.heap_bytes(),
            2 * size_of::<usize>() + (HEADER + 2) * 4
        );

        // Overwriting a column replaces its stamp; the others stay.
        let second = Stamp::with_direction(20, RowChange::Updated, Some(ChangeDirection::Up));
        let stamps = write(policy, Some(&stamps), &[7, 9], every, 20, second).expect("stamped");
        let view = stamps.view();
        assert_eq!(view.get(ColumnIx::new(3)), first);
        assert_eq!(view.get(ColumnIx::new(7)), second);
        assert_eq!(view.get(ColumnIx::new(9)), second);
        assert_eq!(held(&stamps), 3);

        // A column outside the row is not stamped; nothing written, nothing
        // held.
        assert_eq!(write(policy, None, &[40, 41], every, 20, second), None);
        assert_eq!(
            SparseStamps::written(None, ColumnSet::EMPTY, every, |_| true, |_| second),
            None
        );
    }

    #[test]
    fn test_sparse_stamps_drop_expired_on_write() {
        // 800 ms in 100 ms steps.
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        let every = ColumnSet::first(40);
        let early = Stamp::new(100, RowChange::Updated);
        let late = Stamp::new(150, RowChange::Updated);
        let stamps = write(policy, None, &[1], every, 100, early).expect("stamped");
        let stamps = write(policy, Some(&stamps), &[2], every, 150, late).expect("stamped");
        assert_eq!(held(&stamps), 2);

        // At 960 ms the tick is 900: the stamp of 100 ms is over and
        // dropped; the stamp of 150 ms is in its last step at that tick, so
        // it stays although it expired at 950 ms.
        let now = Stamp::new(960, RowChange::Updated);
        let stamps = write(policy, Some(&stamps), &[3], every, 960, now).expect("stamped");
        let view = stamps.view();
        assert_eq!(view.get(ColumnIx::new(1)), Stamp::NONE);
        assert_eq!(view.get(ColumnIx::new(2)), late);
        assert_eq!(view.get(ColumnIx::new(3)), now);
        assert_eq!(held(&stamps), 2);
        let is_hot = |at: u32| move |stamp| !policy.is_highlight_over(stamp, at);
        assert!(!stamps.is_cold(is_hot(960)));

        // At 1000 ms only the new stamps are hot.
        let next = Stamp::new(1_000, RowChange::Updated);
        let stamps = write(policy, Some(&stamps), &[4], every, 1_000, next).expect("stamped");
        assert_eq!(held(&stamps), 2);
        assert_eq!(stamps.view().get(ColumnIx::new(2)), Stamp::NONE);
        assert_eq!(stamps.view().get(ColumnIx::new(3)), now);
        // Cold once every stamp is over at the tick.
        assert!(!stamps.is_cold(is_hot(1_799)));
        assert!(stamps.is_cold(is_hot(1_800)));
    }

    #[test]
    fn test_sparse_stamps_whole_row() {
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        let every = ColumnSet::first(40);
        // An insert is one stamp for every cell.
        let inserted = Stamp::new(10, RowChange::Inserted);
        let stamps = SparseStamps::whole_row(inserted);
        assert_eq!(held(&stamps), 1);
        assert_eq!(stamps.heap_bytes(), 2 * size_of::<usize>() + HEADER * 4);
        assert!((0..40).all(|ix| stamps.view().get(ColumnIx::new(ix)) == inserted));

        // An update while it flashes: its columns over the row's flash.
        let update = Stamp::with_direction(150, RowChange::Updated, Some(ChangeDirection::Down));
        let stamps = write(policy, Some(&stamps), &[5], every, 150, update).expect("stamped");
        assert_eq!(stamps.view().get(ColumnIx::new(5)), update);
        assert_eq!(stamps.view().get(ColumnIx::new(4)), inserted);
        assert_eq!(held(&stamps), 2);

        // Past the flash, a write drops it: the other cells go dark.
        let later = Stamp::new(900, RowChange::Updated);
        let stamps = write(policy, Some(&stamps), &[6], every, 900, later).expect("stamped");
        assert_eq!(stamps.view().get(ColumnIx::new(4)), Stamp::NONE);
        assert_eq!(stamps.view().get(ColumnIx::new(5)), update);
        assert_eq!(held(&stamps), 2);

        // The same stamp in every cell is the row's alone; different ones
        // are the cells' own, without a row stamp.
        let all: Vec<u16> = (0..40).collect();
        let mark = Stamp::new(950, RowChange::Updated);
        let marked = write(policy, Some(&stamps), &all, every, 950, mark).expect("stamped");
        assert_eq!(held(&marked), 1);
        assert!((0..40).all(|ix| marked.view().get(ColumnIx::new(ix)) == mark));
        let mixed = SparseStamps::written(
            Some(&stamps),
            every,
            every,
            |_| true,
            |column| Stamp::new(950 + column.get() as u32, RowChange::Updated),
        )
        .expect("stamped");
        assert_eq!(held(&mixed), 40);
        assert_eq!(mixed.view().row(), Stamp::NONE);
        assert_eq!(mixed.view().get(ColumnIx::new(39)).ms(), 989);

        // The flash of 10 ms shows its last step at the tick of 800 ms and
        // is over for every cell at the tick of 900 ms.
        let is_hot = |at: u32| move |stamp| !policy.is_highlight_over(stamp, at);
        let flash = SparseStamps::whole_row(inserted);
        assert!(!flash.is_cold(is_hot(899)));
        assert!(flash.is_cold(is_hot(900)));
    }

    #[test]
    fn test_sparse_stamps_past_64_and_128_columns() {
        let policy = HighlightPolicy::new(Duration::from_millis(800), 8);
        for width in [70, 100, 128] {
            let every = ColumnSet::first(width);
            let last = width as u16 - 1;
            let written = [0, 31, 32, 63, 64, 65, last];
            // Each column its own stamp, so the order of the stamps shows.
            let stamps = SparseStamps::written(
                None,
                columns(&written),
                every,
                |stamp| !policy.is_highlight_over(stamp, 0),
                |column| Stamp::new(column.get() as u32 + 1, RowChange::Updated),
            )
            .expect("stamped");
            let view = stamps.view();
            assert_eq!(view.columns(), columns(&written));
            // Word 0 stamps columns 0 and 31, word 1 columns 32 and 63: the
            // first stamp of word 1 is the third, of word 2 the fifth.
            assert_eq!(view.header[PLACES].0 & 0xff_ff_ff, 2 << 8 | 4 << 16);
            for ix in 0..width as u16 {
                let expected = if written.contains(&ix) {
                    Stamp::new(u32::from(ix) + 1, RowChange::Updated)
                } else {
                    Stamp::NONE
                };
                assert_eq!(view.get(ColumnIx::new(ix)), expected, "{width}: {ix}");
            }
            // An update of the last column keeps the others in place.
            let update = Stamp::new(500, RowChange::Updated);
            let stamps =
                write(policy, Some(&stamps), &[last], every, 500, update).expect("stamped");
            assert_eq!(stamps.view().get(ColumnIx::new(last)), update);
            assert_eq!(stamps.view().get(ColumnIx::new(64)).ms(), 65);
            assert_eq!(held(&stamps), 7);
        }
    }
}
