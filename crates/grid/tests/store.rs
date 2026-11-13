//! Store tests without a grid view: pinned rows and their highlight, the
//! progress and its order against a reload, `GridWriter::batch`,
//! `GridWriter::flush`, the direction of changes, fading removed rows,
//! parent changes and write coalescing.

use std::{
    cell::{Cell, RefCell},
    cmp::Ordering,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui_kit::TestAppContext;
use gpui_kit_grid::{
    CellValue, ChangeDirection, ChangeSet, ColumnIx, ColumnSet, GridColumns, GridProgress, GridRow,
    GridStore, GridWriter, HighlightPolicy, Ingest, Op, PinnedEdge, Reload, ReloadPolicy,
    RowChange, RowId, RowStamps, Stamp, StoreOptions, StoreSnapshot, WriteCoalescing,
};

use ChangeDirection::{Down, Up};
use PinnedEdge::{Bottom, Top};
use RowChange::{Inserted, Updated};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum QuoteColumn {
    Id,
    Price,
    Qty,
}

impl GridColumns for QuoteColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Price, Self::Qty]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Price => "price",
            Self::Qty => "qty",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Debug)]
struct Quote {
    id: u32,
    price: i64,
    qty: i64,
}

impl GridRow for Quote {
    type Key = u32;
    type Column = QuoteColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: QuoteColumn) -> CellValue<'_> {
        match column {
            QuoteColumn::Id => CellValue::Int(self.id.into()),
            QuoteColumn::Price => CellValue::Int(self.price),
            QuoteColumn::Qty => CellValue::Int(self.qty),
        }
    }
}

fn quote(id: u32, price: i64, qty: i64) -> Arc<Quote> {
    Arc::new(Quote { id, price, qty })
}

fn pin(edge: PinnedEdge, rows: Vec<Arc<Quote>>) -> Op<Quote> {
    Op::SetPinnedRows { edge, rows }
}

fn keys(rows: &[Arc<Quote>]) -> Vec<u32> {
    rows.iter().map(|row| row.id).collect()
}

/// What each column's stamp records, `None` where nothing changed.
fn marks(stamps: Option<RowStamps<'_>>) -> Vec<Option<RowChange>> {
    stamps.map_or_else(Vec::new, |stamps| {
        QuoteColumn::all()
            .iter()
            .map(|column| stamps.get(column.ix()))
            .map(|stamp| stamp.is_set().then(|| stamp.change()))
            .collect()
    })
}

/// One ingest batch: the publish pause of the previous one is over.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
}

/// Records every snapshot `store` publishes from now on: (has progress,
/// rows). The publish pause lets the subscriber take each one.
fn record(cx: &TestAppContext, store: &GridStore<Quote>) -> Rc<RefCell<Vec<(bool, usize)>>> {
    let published = Rc::new(RefCell::new(Vec::new()));
    let subscription = store.subscribe();
    cx.foreground_executor()
        .spawn({
            let published = published.clone();
            async move {
                while subscription.changed().await {
                    let (snapshot, _) = subscription.take();
                    let entry = (snapshot.progress().is_some(), snapshot.len());
                    published.borrow_mut().push(entry);
                }
            }
        })
        .detach();
    published
}

/// Awaits `writer.flush()` on the test thread: `None` until it resolves.
fn flush(cx: &TestAppContext, writer: &GridWriter<Quote>) -> Rc<Cell<Option<Option<u64>>>> {
    let result = Rc::new(Cell::new(None));
    let flushed = writer.flush();
    cx.foreground_executor()
        .spawn({
            let result = result.clone();
            async move { result.set(Some(flushed.await)) }
        })
        .detach();
    result
}

#[test]
fn test_grid_progress_builder() {
    let progress = GridProgress::new(0.4).with_label("Loading trades");
    assert_eq!(progress.fraction(), Some(0.4));
    assert_eq!(progress.label(), Some("Loading trades"));
    assert!(!progress.is_indeterminate());
    assert_ne!(progress, GridProgress::new(0.4), "the label is part of it");

    let indeterminate = GridProgress::indeterminate();
    assert_eq!(indeterminate.fraction(), None);
    assert_eq!(indeterminate.label(), None);
    assert!(indeterminate.is_indeterminate());

    // Fractions are clamped; NaN is work of unknown length.
    assert_eq!(GridProgress::new(1.5).fraction(), Some(1.0));
    assert_eq!(GridProgress::new(-0.5).fraction(), Some(0.0));
    assert_eq!(GridProgress::new(f32::NAN), indeterminate);
}

#[gpui_kit::test]
fn test_pinned_rows_highlight_and_lifecycle(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy = HighlightPolicy::new(Duration::from_millis(1_000), 4);
    let mut ingest = Ingest::<Quote>::new(policy, epoch);
    ingest.apply([Op::ReplaceAll(vec![quote(1, 10, 1)])], at(0));

    // The first fill of an empty band is not highlighted, like `replace_all`.
    let changes = ingest.apply([pin(Top, vec![quote(1, 100, 0), quote(7, 70, 0)])], at(10));
    assert!(changes.has_pinned_change());
    assert!(!changes.has_progress_change());
    assert!(!changes.has_structure_change());
    assert!(
        changes.columns().is_empty(),
        "the display order needs no work"
    );
    assert!(changes.updated().is_empty());
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.version(), 2);
    assert_eq!(keys(snapshot.pinned_rows(Top)), [1, 7]);
    assert!(snapshot.pinned_rows(Bottom).is_empty());
    assert_eq!(snapshot.pinned_stamps(Top, 0), None);
    assert_eq!(snapshot.pinned_stamps(Top, 1), None);
    assert_eq!(snapshot.pinned_stamps(Top, 2), None);
    // Pinned keys are a space of their own: key 1 is also a data row.
    assert_eq!(snapshot.get(&1).map(|row| row.price), Some(10));
    assert_eq!(snapshot.len(), 1);

    // A changed price stamps only the price, and only of that row.
    ingest.apply([pin(Top, vec![quote(1, 101, 0), quote(7, 70, 0)])], at(20));
    let snapshot = ingest.snapshot();
    assert_eq!(
        marks(snapshot.pinned_stamps(Top, 0)),
        [None, Some(Updated), None]
    );
    assert_eq!(snapshot.pinned_stamps(Top, 1), None);

    // A new key flashes. A kept key keeps its stamps where it moves to.
    ingest.apply(
        [pin(
            Top,
            vec![quote(9, 90, 0), quote(1, 101, 0), quote(7, 70, 0)],
        )],
        at(30),
    );
    let snapshot = ingest.snapshot();
    assert_eq!(keys(snapshot.pinned_rows(Top)), [9, 1, 7]);
    assert_eq!(marks(snapshot.pinned_stamps(Top, 0)), [Some(Inserted); 3]);
    let price = snapshot
        .pinned_stamps(Top, 1)
        .map(|stamps| stamps.get(QuoteColumn::Price.ix()));
    assert_eq!(price.map(Stamp::ms), Some(20));
    assert_eq!(snapshot.pinned_stamps(Top, 2), None);

    // A repeated key keeps its last row, in that row's position.
    ingest.apply(
        [pin(
            Top,
            vec![
                quote(9, 90, 0),
                quote(1, 101, 0),
                quote(7, 70, 0),
                quote(9, 95, 5),
            ],
        )],
        at(40),
    );
    let snapshot = ingest.snapshot();
    assert_eq!(keys(snapshot.pinned_rows(Top)), [1, 7, 9]);
    assert_eq!(snapshot.pinned_rows(Top)[2].price, 95);
    assert_eq!(
        marks(snapshot.pinned_stamps(Top, 2)),
        [Some(Inserted), Some(Updated), Some(Updated)]
    );

    // New rows with the same values are published for a repaint: hooks may
    // read data the columns do not show. Nothing is stamped, and the stamps
    // carry over.
    let before = ingest.snapshot();
    let band = vec![quote(1, 101, 0), quote(7, 70, 0), quote(9, 95, 5)];
    let changes = ingest.apply([pin(Top, band.clone())], at(50));
    assert!(changes.has_pinned_change());
    assert!(changes.updated().is_empty() && changes.columns().is_empty());
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.version(), before.version() + 1);
    for (ix, row) in band.iter().enumerate() {
        assert!(Arc::ptr_eq(&snapshot.pinned_rows(Top)[ix], row));
        assert_eq!(
            snapshot.pinned_stamps(Top, ix),
            before.pinned_stamps(Top, ix)
        );
    }
    // The same rows again change nothing and publish nothing.
    let changes = ingest.apply([pin(Top, band)], at(55));
    assert!(changes.is_empty());
    assert_eq!(ingest.snapshot().version(), before.version() + 1);

    // `replace_all` and `clear` leave both bands alone. The bottom band may
    // reuse a key of the top band.
    let changes = ingest.apply(
        [
            pin(Bottom, vec![quote(1, 1_000, 0)]),
            Op::ReplaceAll(vec![quote(3, 30, 3)]),
            Op::Clear,
        ],
        at(60),
    );
    assert!(changes.has_pinned_change());
    assert!(changes.is_reset());
    let snapshot = ingest.snapshot();
    assert!(snapshot.is_empty());
    assert_eq!(keys(snapshot.pinned_rows(Top)), [1, 7, 9]);
    assert_eq!(keys(snapshot.pinned_rows(Bottom)), [1]);
    assert_eq!(snapshot.pinned_stamps(Bottom, 0), None, "first fill");

    // Band stamps expire with the store's highlight, like data stamps.
    ingest.sweep(at(2_000));
    let snapshot = ingest.snapshot();
    assert!((0..3).all(|ix| snapshot.pinned_stamps(Top, ix).is_none()));

    // An empty band clears it; clearing it again changes nothing.
    let changes = ingest.apply([pin(Top, Vec::new())], at(2_010));
    assert!(changes.has_pinned_change());
    assert!(ingest.snapshot().pinned_rows(Top).is_empty());
    assert_eq!(keys(ingest.snapshot().pinned_rows(Bottom)), [1]);
    assert!(ingest.apply([pin(Top, Vec::new())], at(2_020)).is_empty());

    // Progress is a change only when it differs.
    let loading = GridProgress::new(0.25).with_label("Loading");
    let changes = ingest.apply([Op::SetProgress(Some(loading.clone()))], at(2_030));
    assert!(changes.has_progress_change());
    assert!(!changes.has_pinned_change());
    assert_eq!(ingest.snapshot().progress(), Some(&loading));
    let changes = ingest.apply([Op::SetProgress(Some(loading))], at(2_040));
    assert!(changes.is_empty());
    let changes = ingest.apply([Op::SetProgress(None)], at(2_050));
    assert!(changes.has_progress_change());
    assert_eq!(ingest.snapshot().progress(), None);

    // Subscribers accumulate both flags.
    let mut accumulated = ChangeSet::default();
    accumulated.merge(&changes);
    accumulated.merge(&ingest.apply([pin(Bottom, Vec::new())], at(2_060)));
    assert!(accumulated.has_progress_change());
    assert!(accumulated.has_pinned_change());
    assert!(!accumulated.is_empty());
}

#[gpui_kit::test]
fn test_replacement_without_changed_column(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy = HighlightPolicy::new(Duration::from_millis(1_000), 4);
    let mut ingest = Ingest::<Quote>::new(policy, epoch);
    ingest.apply(
        [Op::ReplaceAll((1..=3).map(|id| quote(id, 10, 1)).collect())],
        at(0),
    );
    ingest.apply([Op::Upsert(quote(2, 20, 1))], at(10));
    let before = ingest.snapshot();
    let id = before.row_id(&2).expect("row 2");

    // A new row with the same values: in `updated` for the view's "is it on
    // screen" check, with no columns (no work for the order) and no stamp.
    // Silent or not, the same.
    let writes: [(fn(Arc<Quote>) -> Op<Quote>, u64); 2] =
        [(Op::Upsert, 20), (Op::UpsertSilent, 30)];
    for (op, ms) in writes {
        let replacement = quote(2, 20, 1);
        let version = ingest.snapshot().version();
        let changes = ingest.apply([op(replacement.clone())], at(ms));
        assert_eq!(changes.updated(), [id]);
        assert!(changes.columns().is_empty());
        assert!(!changes.has_structure_change() && !changes.has_pinned_change());
        let snapshot = ingest.snapshot();
        assert_eq!(snapshot.version(), version + 1);
        assert!(Arc::ptr_eq(snapshot.get(&2).expect("row 2"), &replacement));
        assert_eq!(snapshot.stamps(id), before.stamps(id), "no new highlight");
        assert_eq!(ingest.hot_rows(), 1);

        // The same `Arc` again changes nothing and publishes nothing.
        assert!(ingest.apply([op(replacement)], at(ms + 5)).is_empty());
        assert_eq!(ingest.snapshot().version(), version + 1);
    }
    // A row without stamps stays without.
    let changes = ingest.apply([Op::Upsert(quote(3, 10, 1))], at(40));
    assert_eq!(changes.updated().len(), 1);
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.stamps(snapshot.row_id(&3).expect("row 3")), None);

    // Through the store: the next snapshot has the new row, published once.
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    settle(cx);
    let published = record(cx, &store);
    let version = store.snapshot().version();
    let replacement = quote(2, 10, 1);
    writer.upsert(replacement.clone());
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version + 1);
    assert!(Arc::ptr_eq(snapshot.get(&2).expect("row 2"), &replacement));
    assert_eq!(snapshot.stamps(snapshot.row_id(&2).expect("row 2")), None);
    writer.upsert(replacement);
    settle(cx);
    assert_eq!(store.snapshot().version(), version + 1);
    assert_eq!(*published.borrow(), [(false, 3)]);
}

/// Records which live operations of a reload set pinned rows, then keeps
/// only the reloaded snapshot.
struct LiveOps(Arc<Mutex<Vec<Option<PinnedEdge>>>>);

impl ReloadPolicy<Quote> for LiveOps {
    fn resolve(&mut self, snapshot: Vec<Arc<Quote>>, live: Vec<Op<Quote>>) -> Vec<Op<Quote>> {
        self.0
            .lock()
            .unwrap()
            .extend(live.iter().map(|op| match op {
                Op::SetPinnedRows { edge, .. } => Some(*edge),
                _ => None,
            }));
        vec![Op::ReplaceAll(snapshot)]
    }
}

#[gpui_kit::test]
fn test_pinned_rows_replay_after_reload(cx: &mut TestAppContext) {
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    writer.set_pinned_rows(Bottom, [quote(0, 30, 3)]);
    settle(cx);

    // A reload buffers pinned rows like any other write.
    let reload = writer.begin_reload();
    writer.set_pinned_rows(Bottom, [quote(0, 50, 5)]);
    writer.set_pinned_rows(Top, [quote(0, 1, 1)]);
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.pinned_rows(Bottom)[0].price, 30);
    assert!(snapshot.pinned_rows(Top).is_empty());

    // `ReplayLive` replays them over the snapshot, highlighted by key.
    reload.finish((1..=5).map(|id| quote(id, 20, 2)));
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.len(), 5);
    assert_eq!(snapshot.pinned_rows(Bottom)[0].price, 50);
    assert_eq!(
        marks(snapshot.pinned_stamps(Bottom, 0)),
        [None, Some(Updated), Some(Updated)]
    );
    assert_eq!(keys(snapshot.pinned_rows(Top)), [0]);
    assert_eq!(snapshot.pinned_stamps(Top, 0), None, "first fill");

    // A custom policy sees them among the live operations; the progress
    // is never buffered, so it is not there.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = cx.update(|cx| {
        GridStore::<Quote>::builder(StoreOptions::default())
            .with_reload_policy(LiveOps(seen.clone()))
            .build(cx)
    });
    let writer = store.writer();
    let reload = writer.begin_reload();
    writer.set_pinned_rows(Top, [quote(0, 1, 1)]);
    writer.set_progress(Some(GridProgress::indeterminate()));
    writer.upsert(quote(1, 1, 1));
    reload.finish([quote(2, 2, 2)]);
    settle(cx);
    assert_eq!(*seen.lock().unwrap(), [Some(Top), None]);
    let snapshot = store.snapshot();
    assert!(
        snapshot.pinned_rows(Top).is_empty(),
        "the policy dropped it"
    );
    assert!(snapshot.progress().is_some());
    assert_eq!(snapshot.len(), 1);
}

#[gpui_kit::test]
fn test_progress_is_ordered_but_not_buffered(cx: &mut TestAppContext) {
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    settle(cx);

    let published = record(cx, &store);

    writer.set_progress(Some(
        GridProgress::indeterminate().with_label("Loading trades"),
    ));
    let reload = writer.begin_reload();
    writer.upsert(quote(1, 111, 1));
    writer.set_progress(Some(GridProgress::new(0.5).with_label("Loading trades")));
    settle(cx);
    let snapshot = store.snapshot();
    let progress = snapshot.progress().expect("shown while the reload runs");
    assert_eq!(progress.fraction(), Some(0.5));
    assert_eq!(progress.label(), Some("Loading trades"));
    assert_eq!(
        snapshot.get(&1).map(|row| row.price),
        Some(10),
        "the live write is still buffered"
    );

    // Cleared after `finish`: not before the snapshot that applies the
    // reload (in the same one only within a batch, see below).
    reload.finish((1..=5).map(|id| quote(id, 20, 2)));
    writer.set_progress(None);
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.progress(), None);
    assert_eq!(snapshot.len(), 5);
    assert_eq!(snapshot.get(&1).map(|row| row.price), Some(111));
    let published = published.borrow();
    assert_eq!(published.first(), Some(&(true, 3)));
    assert_eq!(published.last(), Some(&(false, 5)));
    assert!(
        published
            .iter()
            .all(|&(progress, len)| progress || len == 5),
        "progress cleared before the reload is applied: {published:?}"
    );
}

#[gpui_kit::test]
fn test_batch_clears_progress_with_reloaded_rows(cx: &mut TestAppContext) {
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    let loading = || Some(GridProgress::indeterminate().with_label("Loading trades"));
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    settle(cx);
    let published = record(cx, &store);

    // Two calls can split: when the ingest takes the reload before the
    // second call, the reloaded rows are published with the bar still on.
    writer.set_progress(loading());
    let reload = writer.begin_reload();
    settle(cx);
    reload.finish((1..=5).map(|id| quote(id, 20, 2)));
    settle(cx);
    writer.set_progress(None);
    settle(cx);
    assert_eq!(*published.borrow(), [(true, 3), (true, 5), (false, 5)]);

    // One batch cannot: nothing of it is written until the closure returns,
    // even when the ingest runs meanwhile, and then it is one snapshot.
    published.borrow_mut().clear();
    writer.set_progress(loading());
    let reload = writer.begin_reload();
    settle(cx);
    let version = store.snapshot().version();
    writer.batch(|batch| {
        batch.finish_reload(reload, (1..=7).map(|id| quote(id, 30, 3)));
        settle(cx);
        assert_eq!(store.snapshot().version(), version, "nothing written yet");
        batch.set_progress(None);
    });
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version + 1);
    assert_eq!(snapshot.progress(), None);
    assert_eq!(snapshot.get(&1).map(|row| row.price), Some(30));
    assert_eq!(*published.borrow(), [(true, 5), (false, 7)]);
}

#[gpui_kit::test]
fn test_batch_order_and_reload_buffer(cx: &mut TestAppContext) {
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    writer.set_progress(Some(GridProgress::indeterminate()));
    settle(cx);

    // Data, a band and the progress: applied in order, in one snapshot.
    let version = store.snapshot().version();
    writer.batch(|batch| {
        batch.replace_all((1..=3).map(|id| quote(id, 10, 1)));
        batch.upsert(quote(4, 40, 4));
        batch.upsert_silent(quote(5, 50, 5));
        batch.mark_changed(5, ColumnSet::first(1));
        batch.remove(1);
        batch.set_pinned_rows(Bottom, [quote(0, 30, 3)]);
        batch.set_progress(None);
    });
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version + 1);
    assert_eq!(snapshot.len(), 4);
    assert!(snapshot.get(&1).is_none());
    let silent = snapshot.row_id(&5).and_then(|id| snapshot.stamps(id));
    assert_eq!(marks(silent), [Some(Updated), None, None]);
    assert_eq!(keys(snapshot.pinned_rows(Bottom)), [0]);
    assert_eq!(snapshot.progress(), None);

    // An empty batch writes and publishes nothing.
    writer.batch(|_| {});
    settle(cx);
    assert_eq!(store.snapshot().version(), version + 1);

    // A running reload buffers a batch's writes, but not its progress.
    let reload = writer.begin_reload();
    writer.batch(|batch| {
        batch.upsert(quote(2, 22, 2));
        batch.set_progress(Some(GridProgress::new(0.5)));
    });
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.get(&2).map(|row| row.price), Some(10), "buffered");
    assert_eq!(
        snapshot.progress().and_then(GridProgress::fraction),
        Some(0.5)
    );

    // Writes before `finish_reload` are buffered and replayed over the
    // reloaded rows (`ReplayLive`); writes after it follow the reload. All
    // of it is one snapshot.
    let version = snapshot.version();
    writer.batch(|batch| {
        batch.upsert(quote(3, 33, 3));
        batch.finish_reload(reload, [2, 3, 6].map(|id| quote(id, 60, 6)));
        batch.remove(6);
        batch.set_progress(None);
    });
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version + 1);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot.get(&2).map(|row| row.price), Some(22));
    assert_eq!(snapshot.get(&3).map(|row| row.price), Some(33));
    assert_eq!(snapshot.progress(), None);

    // A superseded reload's finish is ignored; the rest of the batch is
    // written.
    let older = writer.begin_reload();
    let newer = writer.begin_reload();
    writer.batch(|batch| {
        batch.finish_reload(older, [quote(9, 9, 9)]);
        batch.set_progress(Some(GridProgress::indeterminate()));
    });
    settle(cx);
    assert!(store.snapshot().get(&9).is_none());
    assert!(store.snapshot().progress().is_some());
    drop(newer);

    // A reload of another store is finished there, outside the batch.
    let other = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let foreign = other.writer().begin_reload();
    writer.batch(|batch| batch.finish_reload(foreign, [quote(7, 7, 7)]));
    settle(cx);
    assert!(other.snapshot().get(&7).is_some());
    assert!(store.snapshot().get(&7).is_none());
}

#[gpui_kit::test]
fn test_writer_flush(cx: &mut TestAppContext) {
    let store = cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default(), cx));
    let writer = store.writer();

    // Resolves with the version of the snapshot that has the writes.
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    writer.upsert(quote(4, 40, 4));
    let flushed = flush(cx, &writer);
    assert_eq!(flushed.get(), None);
    cx.run_until_parked();
    let version = flushed.get().flatten().expect("the store is alive");
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version);
    assert_eq!(snapshot.len(), 4);

    // Nothing written: the current version, nothing published.
    let flushed = flush(cx, &writer);
    settle(cx);
    assert_eq!(flushed.get(), Some(Some(version)));
    assert_eq!(store.snapshot().version(), version);

    // Pinned rows and progress are covered like data.
    writer.set_pinned_rows(Top, [quote(0, 1, 1)]);
    writer.set_progress(Some(GridProgress::indeterminate()));
    let flushed = flush(cx, &writer);
    settle(cx);
    let version = flushed.get().flatten().expect("the store is alive");
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version);
    assert_eq!(keys(snapshot.pinned_rows(Top)), [0]);
    assert!(snapshot.progress().is_some());

    // Writes buffered by a running reload are not waited for...
    let reload = writer.begin_reload();
    writer.upsert(quote(5, 50, 5));
    let flushed = flush(cx, &writer);
    settle(cx);
    assert_eq!(flushed.get(), Some(Some(version)));
    assert!(store.snapshot().get(&5).is_none());

    // ...until `finish` writes them.
    reload.finish((1..=3).map(|id| quote(id, 10, 1)));
    let flushed = flush(cx, &writer);
    settle(cx);
    let version = flushed.get().flatten().expect("the store is alive");
    let snapshot = store.snapshot();
    assert_eq!(snapshot.version(), version);
    assert!(snapshot.get(&5).is_some());

    // A flush pending when the store goes, and any later one, is `None`.
    writer.upsert(quote(6, 60, 6));
    let pending = flush(cx, &writer);
    drop(store);
    settle(cx);
    assert_eq!(pending.get(), Some(None));
    assert!(writer.is_closed());
    let late = flush(cx, &writer);
    cx.run_until_parked();
    assert_eq!(late.get(), Some(None));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LegColumn {
    Id,
    Qty,
    Price,
    Venue,
}

impl GridColumns for LegColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Qty, Self::Price, Self::Venue]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Qty => "qty",
            Self::Price => "price",
            Self::Venue => "venue",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A quantity written as an integer or as a fraction.
#[derive(Clone, Copy, Debug)]
enum Qty {
    Int(i64),
    Float(f64),
}

/// A row of a tree, with numbers and a text, and the default
/// `GridRow::change_direction`.
#[derive(Clone, Debug)]
struct Leg {
    id: u32,
    parent: Option<u32>,
    qty: Qty,
    price: f64,
    venue: &'static str,
}

impl Leg {
    fn new(id: u32, qty: Qty, price: f64) -> Self {
        Self {
            id,
            parent: None,
            qty,
            price,
            venue: "XLON",
        }
    }

    fn venue(self, venue: &'static str) -> Self {
        Self { venue, ..self }
    }

    fn parent(self, parent: Option<u32>) -> Self {
        Self { parent, ..self }
    }

    fn op(self) -> Op<Leg> {
        Op::Upsert(Arc::new(self))
    }
}

impl GridRow for Leg {
    type Key = u32;
    type Column = LegColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: LegColumn) -> CellValue<'_> {
        match column {
            LegColumn::Id => CellValue::Int(self.id.into()),
            LegColumn::Qty => match self.qty {
                Qty::Int(qty) => CellValue::Int(qty),
                Qty::Float(qty) => CellValue::Float(qty),
            },
            LegColumn::Price => CellValue::Float(self.price),
            LegColumn::Venue => CellValue::Text(self.venue),
        }
    }

    fn parent_key(&self) -> Option<&u32> {
        self.parent.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BookColumn {
    Id,
    Bid,
    Ask,
    Spread,
}

impl GridColumns for BookColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Bid, Self::Ask, Self::Spread]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Bid => "bid",
            Self::Ask => "ask",
            Self::Spread => "spread",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

thread_local! {
    /// `Book::change_direction` calls on this thread (ingest runs on the
    /// test thread when driven directly).
    static DIRECTION_CALLS: Cell<usize> = const { Cell::new(0) };
}

/// A quote whose wider spread is bad news: its own `change_direction` shows
/// it as a fall. Counts its calls.
#[derive(Debug)]
struct Book {
    id: u32,
    bid: i64,
    ask: i64,
}

fn book(id: u32, bid: i64, ask: i64) -> Arc<Book> {
    Arc::new(Book { id, bid, ask })
}

impl GridRow for Book {
    type Key = u32;
    type Column = BookColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: BookColumn) -> CellValue<'_> {
        match column {
            BookColumn::Id => CellValue::Int(self.id.into()),
            BookColumn::Bid => CellValue::Int(self.bid),
            BookColumn::Ask => CellValue::Int(self.ask),
            BookColumn::Spread => CellValue::Int(self.ask - self.bid),
        }
    }

    fn change_direction(&self, previous: &Self, column: BookColumn) -> Option<ChangeDirection> {
        DIRECTION_CALLS.with(|calls| calls.set(calls.get() + 1));
        let order = self.value(column).cmp_total(&previous.value(column));
        let order = match column {
            BookColumn::Spread => order.reverse(),
            _ => order,
        };
        match order {
            Ordering::Greater => Some(Up),
            Ordering::Less => Some(Down),
            Ordering::Equal => None,
        }
    }
}

fn direction_calls() -> usize {
    DIRECTION_CALLS.with(Cell::get)
}

type Trend = Option<(RowChange, Option<ChangeDirection>)>;

/// What the stamp of each column `C` records with its direction, `None`
/// where nothing changed.
fn trends<C: GridColumns>(stamps: Option<RowStamps<'_>>) -> Vec<Trend> {
    stamps.map_or_else(Vec::new, |stamps| {
        C::all()
            .iter()
            .map(|column| stamps.get(column.ix()))
            .map(|stamp| stamp.is_set().then(|| (stamp.change(), stamp.direction())))
            .collect()
    })
}

/// The trends of the data row with `key` in the ingest's latest snapshot.
fn row_trends<R: GridRow<Key = u32>>(ingest: &Ingest<R>, key: u32) -> Vec<Trend> {
    let snapshot = ingest.snapshot();
    trends::<R::Column>(snapshot.row_id(&key).and_then(|id| snapshot.stamps(id)))
}

fn sorted(mut ids: Vec<RowId>) -> Vec<RowId> {
    ids.sort_by_key(|id| id.ix());
    ids
}

#[gpui_kit::test]
fn test_ingest_writes_direction(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy = HighlightPolicy::new(Duration::from_millis(1_000), 4);
    let mut ingest = Ingest::<Leg>::new(policy, epoch);
    let rows = (1..=5).map(|id| Arc::new(Leg::new(id, Qty::Int(10), 1.5)));
    ingest.apply([Op::ReplaceAll(rows.collect())], at(0));

    // The default rule: numbers by `cmp_total`, anything else without one.
    ingest.apply(
        [
            Leg::new(1, Qty::Int(12), 1.5).op(),
            Leg::new(2, Qty::Int(10), 1.25).op(),
            Leg::new(3, Qty::Int(10), 1.5).venue("XPAR").op(),
            Leg::new(4, Qty::Float(10.5), 1.5).op(),
            // Another value of the same number: changed, without a direction.
            Leg::new(5, Qty::Float(10.0), 1.5).op(),
        ],
        at(10),
    );
    let up = Some((Updated, Some(Up)));
    let down = Some((Updated, Some(Down)));
    let plain = Some((Updated, None));
    assert_eq!(row_trends(&ingest, 1), [None, up, None, None]);
    assert_eq!(row_trends(&ingest, 2), [None, None, down, None]);
    assert_eq!(row_trends(&ingest, 3), [None, None, None, plain]);
    assert_eq!(row_trends(&ingest, 4), [None, up, None, None]);
    assert_eq!(row_trends(&ingest, 5), [None, plain, None, None]);

    // A → B → C in one batch: each against the version before it, so the
    // last change of a column sets its direction, and a column C leaves
    // alone keeps the one of B.
    ingest.apply(
        [
            Leg::new(1, Qty::Int(20), 1.5).op(),
            Leg::new(1, Qty::Int(15), 1.5).op(),
            Leg::new(2, Qty::Int(10), 1.75).op(),
            Leg::new(2, Qty::Int(10), 1.75).venue("XPAR").op(),
        ],
        at(20),
    );
    assert_eq!(row_trends(&ingest, 1), [None, down, None, None]);
    assert_eq!(row_trends(&ingest, 2), [None, None, up, plain]);
    let snapshot = ingest.snapshot();
    let stamps = snapshot.row_id(&1).and_then(|id| snapshot.stamps(id));
    assert_eq!(
        stamps.map(|stamps| stamps.get(LegColumn::Qty.ix()).ms()),
        Some(20)
    );

    // No direction without a highlight (`upsert_silent`), for a mark (the
    // values did not change) and for an insert.
    let mut price = ColumnSet::EMPTY;
    price.insert(LegColumn::Price.ix());
    ingest.apply(
        [
            Op::UpsertSilent(Arc::new(Leg::new(3, Qty::Int(99), 1.5).venue("XPAR"))),
            Op::MarkChanged(4, price),
            Leg::new(6, Qty::Int(1), 1.0).op(),
        ],
        at(30),
    );
    assert_eq!(row_trends(&ingest, 3), [None, None, None, plain]);
    assert_eq!(row_trends(&ingest, 4), [None, up, plain, None]);
    assert_eq!(row_trends(&ingest, 6), [Some((Inserted, None)); 4]);

    // The row's own rule: a wider spread is a fall, in data rows...
    let mut ingest = Ingest::<Book>::new(policy, epoch);
    ingest.apply([Op::ReplaceAll(vec![book(1, 100, 102)])], at(0));
    ingest.apply([Op::Upsert(book(1, 101, 105))], at(10));
    assert_eq!(row_trends(&ingest, 1), [None, up, up, down]);

    // ...and in a pinned band, by key.
    ingest.apply([pin_books(Top, vec![book(9, 100, 104)])], at(20));
    ingest.apply(
        [pin_books(Top, vec![book(8, 1, 2), book(9, 99, 101)])],
        at(30),
    );
    let snapshot = ingest.snapshot();
    assert_eq!(
        trends::<BookColumn>(snapshot.pinned_stamps(Top, 1)),
        [None, down, down, up]
    );
    assert_eq!(
        trends::<BookColumn>(snapshot.pinned_stamps(Top, 0)),
        [Some((Inserted, None)); 4]
    );
}

fn pin_books(edge: PinnedEdge, rows: Vec<Arc<Book>>) -> Op<Book> {
    Op::SetPinnedRows { edge, rows }
}

#[gpui_kit::test]
fn test_direction_costs_one_call_per_changed_cell(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let mut ingest = Ingest::<Book>::new(HighlightPolicy::default(), epoch);
    let calls = direction_calls();
    let since = || direction_calls() - calls;

    // Only a highlighted replacement asks, and only for its changed cells.
    ingest.apply(
        [Op::ReplaceAll(
            (1..=3).map(|id| book(id, 100, 102)).collect(),
        )],
        at(0),
    );
    ingest.apply([Op::Upsert(book(4, 100, 102))], at(10));
    assert_eq!(since(), 0, "inserts have no direction");
    ingest.apply([Op::Upsert(book(1, 101, 102))], at(20));
    assert_eq!(since(), 2, "the bid and the spread changed");
    ingest.apply(
        [
            Op::UpsertSilent(book(2, 50, 60)),
            Op::Upsert(book(3, 100, 102)),
            Op::MarkChanged(3, ColumnSet::first(4)),
            Op::Remove(4),
        ],
        at(30),
    );
    assert_eq!(since(), 2, "silent, unchanged, marked and removed rows");

    // A band asks for the changed cells of a kept key only.
    ingest.apply([pin_books(Bottom, vec![book(9, 1, 2)])], at(40));
    ingest.apply(
        [pin_books(Bottom, vec![book(9, 1, 3), book(8, 1, 2)])],
        at(50),
    );
    assert_eq!(since(), 4, "the ask and the spread of the kept key");

    // Without a removal fade, nothing more is kept: a removed row is gone,
    // and no fading row is ever published.
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.fading_rows().count(), 0);
    let changes = ingest.apply([Op::Remove(1), Op::Upsert(book(1, 1, 1))], at(60));
    assert!(changes.faded().is_empty());
    assert_eq!(ingest.snapshot().fading_rows().count(), 0);
}

#[gpui_kit::test]
fn test_removal_fade_lifecycle(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy =
        HighlightPolicy::new(Duration::from_secs(5), 8).with_removal_fade(Duration::from_secs(1));
    let mut ingest = Ingest::<Quote>::new(policy, epoch);
    ingest.apply(
        [Op::ReplaceAll((1..=5).map(|id| quote(id, 10, 1)).collect())],
        at(0),
    );
    let before = ingest.snapshot();
    let id = before.row_id(&3).expect("row 3");
    let last = before.row(id).expect("row 3").clone();

    // Removed at once for the selection; the last version fades under its
    // old id, which never resolves to a live row.
    let changes = ingest.apply([Op::Remove(3)], at(100));
    assert_eq!(changes.removed(), [id]);
    assert!(changes.faded().is_empty());
    assert!(changes.has_structure_change());
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.len(), 4);
    assert!(snapshot.row(id).is_none() && snapshot.get(&3).is_none());
    assert_eq!(snapshot.row_id(&3), None);
    let (row, removed) = snapshot.fading_row(id).expect("fading");
    assert!(Arc::ptr_eq(row, &last));
    // The removal stamp: the time of removal, an update without direction.
    assert_eq!(
        (removed.ms(), removed.change(), removed.direction()),
        (100, Updated, None)
    );
    assert_eq!(
        policy.removal_intensity(removed, policy.tick(100)),
        Some(1.0)
    );
    let fading: Vec<_> = snapshot
        .fading_rows()
        .map(|(id, row, stamp, seq)| (id, row.id, stamp, seq))
        .collect();
    assert_eq!(fading, [(id, 3, removed, before.seq(id))]);

    // A new row may take the slot: the fading id still does not resolve.
    ingest.apply([Op::Upsert(quote(6, 60, 6))], at(200));
    let snapshot = ingest.snapshot();
    let reused = snapshot.row_id(&6).expect("row 6");
    assert_eq!(reused.ix(), id.ix());
    assert_ne!(reused, id);
    assert!(snapshot.row(id).is_none());
    assert!(snapshot.fading_row(id).is_some());

    // A batch before the end keeps it; the first one at or after the end
    // drops it, even with no write at all.
    let changes = ingest.apply([Op::Upsert(quote(1, 11, 1))], at(1_099));
    assert!(changes.faded().is_empty());
    let held = ingest.snapshot();
    assert!(held.fading_row(id).is_some());
    let changes = ingest.apply([], at(1_100));
    assert_eq!(changes.faded(), [id]);
    assert!(!changes.is_empty() && !changes.has_structure_change());
    assert!(changes.removed().is_empty());
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.version(), held.version() + 1);
    assert!(snapshot.fading_row(id).is_none());
    assert_eq!(snapshot.fading_rows().count(), 0);
    // A held snapshot keeps its fading row.
    assert!(held.fading_row(id).is_some());
    // Nothing is left to drop.
    assert!(ingest.apply([], at(5_000)).is_empty());

    // Through the store, in a batch, on the store clock.
    let store =
        cx.update(|cx| GridStore::<Quote>::new(StoreOptions::default().with_highlight(policy), cx));
    let writer = store.writer();
    writer.replace_all((1..=3).map(|id| quote(id, 10, 1)));
    settle(cx);
    let subscription = store.subscribe();
    let id = store.snapshot().row_id(&2).expect("row 2");
    writer.batch(|batch| {
        batch.remove(2);
        batch.upsert(quote(4, 40, 4));
    });
    settle(cx);
    let (snapshot, changes) = subscription.take();
    assert_eq!(changes.removed(), [id]);
    assert_eq!(snapshot.fading_row(id).map(|(row, _)| row.id), Some(2));

    // A quiet store does not wake for it: the row stays in the snapshot,
    // and a view stops painting it by its stamp. The next wake drops it.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert!(store.snapshot().fading_row(id).is_some());
    flush(cx, &writer);
    settle(cx);
    let (snapshot, changes) = subscription.take();
    assert_eq!(changes.faded(), [id]);
    assert!(snapshot.fading_row(id).is_none());
}

#[gpui_kit::test]
fn test_fading_row_dropped_on_reinsert_and_reset(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy = HighlightPolicy::default().with_removal_fade(Duration::from_secs(1));
    let mut ingest = Ingest::<Quote>::new(policy, epoch);
    let rows = || (1..=5).map(|id| quote(id, 10, 1)).collect::<Vec<_>>();
    ingest.apply([Op::ReplaceAll(rows())], at(0));
    let id_of = |ingest: &Ingest<Quote>, key: u32| ingest.snapshot().row_id(&key).expect("row");

    // A new row of a fading key drops the fading row at once: no ghost
    // beside it.
    let (one, two) = (id_of(&ingest, 1), id_of(&ingest, 2));
    ingest.apply([Op::Remove(1), Op::Remove(2)], at(10));
    let changes = ingest.apply([Op::Upsert(quote(1, 20, 2))], at(20));
    assert_eq!(changes.faded(), [one]);
    assert_eq!(changes.inserted(), 1);
    let snapshot = ingest.snapshot();
    assert!(snapshot.fading_row(one).is_none());
    assert!(snapshot.fading_row(two).is_some());
    assert_eq!(snapshot.get(&1).map(|row| row.price), Some(20));
    // The end of the fades reports only the row still fading.
    assert_eq!(ingest.apply([], at(1_010)).faded(), [two]);

    // Removed and written again in one batch: never published as fading.
    let one = id_of(&ingest, 1);
    let changes = ingest.apply([Op::Remove(1), Op::Upsert(quote(1, 30, 3))], at(1_020));
    assert_eq!(
        (changes.removed(), changes.faded()),
        ([one].as_slice(), [one].as_slice())
    );
    assert_eq!(ingest.snapshot().fading_rows().count(), 0);

    // `replace_all` drops every fading row: a new data set does not fade.
    let (three, four) = (id_of(&ingest, 3), id_of(&ingest, 4));
    ingest.apply([Op::Remove(3), Op::Remove(4)], at(1_030));
    let changes = ingest.apply([Op::ReplaceAll(rows())], at(1_040));
    assert!(changes.is_reset());
    assert_eq!(sorted(changes.faded().to_vec()), sorted(vec![three, four]));
    assert_eq!(ingest.snapshot().fading_rows().count(), 0);
    assert!(
        ingest.apply([], at(3_000)).is_empty(),
        "nothing left to end"
    );

    // So does `clear`.
    let three = id_of(&ingest, 3);
    ingest.apply([Op::Remove(3)], at(3_010));
    let changes = ingest.apply([Op::Clear], at(3_020));
    assert_eq!(changes.faded(), [three]);
    assert_eq!(ingest.snapshot().fading_rows().count(), 0);

    // And a finished reload, by `ReplayLive`'s `replace_all`.
    ingest.apply([Op::ReplaceAll(rows())], at(3_030));
    let two = id_of(&ingest, 2);
    ingest.apply([Op::Remove(2)], at(3_040));
    let changes = ingest.apply(
        [Op::FinishReload {
            snapshot: rows(),
            live: vec![Op::Upsert(quote(9, 90, 9))],
        }],
        at(3_050),
    );
    assert_eq!(changes.faded(), [two]);
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.fading_rows().count(), 0);
    assert_eq!(snapshot.len(), 6);

    // Subscribers accumulate fading rows like removed ones.
    let mut accumulated = ChangeSet::default();
    accumulated.merge(&changes);
    assert_eq!(accumulated.faded(), [two]);
}

#[gpui_kit::test]
fn test_parent_change_reported(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let mut ingest = Ingest::<Leg>::new(HighlightPolicy::default(), epoch);
    let leg = |id: u32, parent: Option<u32>| Leg::new(id, Qty::Int(10), 1.5).parent(parent);
    ingest.apply(
        [Op::ReplaceAll(vec![
            Arc::new(leg(1, None)),
            Arc::new(leg(2, None)),
            Arc::new(leg(3, Some(1))),
        ])],
        at(0),
    );
    let id = ingest.snapshot().row_id(&3).expect("row 3");

    // Another parent with the same columns: published, not highlighted, no
    // work for the order by columns, but a tree rebuilds.
    let changes = ingest.apply([leg(3, Some(2)).op()], at(10));
    assert!(changes.has_parent_change());
    assert_eq!(changes.updated(), [id]);
    assert!(changes.columns().is_empty() && !changes.has_structure_change());
    let snapshot = ingest.snapshot();
    assert_eq!(snapshot.row(id).and_then(|row| row.parent), Some(2));
    assert_eq!(snapshot.stamps(id), None);

    // The same parent: a repaint only, as before.
    let changes = ingest.apply([leg(3, Some(2)).op()], at(20));
    assert!(!changes.has_parent_change());
    assert_eq!(changes.updated(), [id]);
    // The same `Arc` again changes nothing and publishes nothing.
    let same = Arc::new(leg(3, None));
    assert!(
        ingest
            .apply([Op::Upsert(same.clone())], at(30))
            .has_parent_change()
    );
    assert!(ingest.apply([Op::Upsert(same)], at(40)).is_empty());

    // Silent or with changed columns, the same; an insert has no previous
    // parent.
    let changes = ingest.apply([Op::UpsertSilent(Arc::new(leg(3, Some(1))))], at(50));
    assert!(changes.has_parent_change());
    let changes = ingest.apply(
        [Leg::new(3, Qty::Int(11), 1.5).parent(Some(2)).op()],
        at(60),
    );
    assert!(changes.has_parent_change());
    assert!(!changes.columns().is_empty());
    let changes = ingest.apply([leg(4, Some(1)).op()], at(70));
    assert!(!changes.has_parent_change());

    // Subscribers accumulate it.
    let mut accumulated = ChangeSet::default();
    accumulated.merge(&ingest.apply([leg(4, Some(2)).op()], at(80)));
    accumulated.merge(&ingest.apply([leg(1, None).op()], at(90)));
    assert!(accumulated.has_parent_change());

    // Through the store, inside a batch.
    let store = cx.update(|cx| GridStore::<Leg>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    writer.replace_all([Arc::new(leg(1, None)), Arc::new(leg(2, Some(1)))]);
    settle(cx);
    let subscription = store.subscribe();
    writer.batch(|batch| {
        batch.upsert(leg(2, None));
        batch.upsert(Leg::new(1, Qty::Int(11), 1.5));
    });
    settle(cx);
    let (snapshot, changes) = subscription.take();
    assert!(changes.has_parent_change());
    assert_eq!(snapshot.get(&2).and_then(|row| row.parent), None);
}

#[gpui_kit::test]
fn test_fading_rows_bounded(cx: &mut TestAppContext) {
    let epoch = cx.executor().now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let policy = HighlightPolicy::default().with_removal_fade(Duration::from_millis(200));
    let mut ingest = Ingest::<Quote>::new(policy, epoch);
    ingest.apply(
        [Op::ReplaceAll(
            (0..400).map(|id| quote(id, 10, 1)).collect(),
        )],
        at(0),
    );

    // 100 removals and 100 inserts a second for 3 s, one of each per 10 ms
    // batch: only the removals of the last 200 ms fade. (Off the epoch: a
    // stamp is never zero, so one at 0 ms reads as 1 ms.)
    let mut most = 0;
    let mut faded = 0;
    let mut held = None;
    for step in 0..300u32 {
        let ms = u64::from(step + 1) * 10;
        let changes = ingest.apply(
            [Op::Remove(step), Op::Upsert(quote(1_000 + step, 10, 1))],
            at(ms),
        );
        faded += changes.faded().len();
        let snapshot = ingest.snapshot();
        let fading = snapshot.fading_rows().count();
        assert!(
            snapshot
                .fading_rows()
                .all(|(_, _, removed, _)| policy.removal_intensity(removed, ms as u32).is_some())
        );
        most = most.max(fading);
        if step == 150 {
            held = Some((snapshot, fading));
        }
    }
    assert_eq!(most, 20, "removals of the last 200 ms");
    assert_eq!(faded, 300 - 20);
    assert_eq!(ingest.len(), 400);

    // Snapshots share the map: a held one keeps what it had.
    let (held, fading) = held.expect("held snapshot");
    assert_eq!(held.fading_rows().count(), fading);
}

// Sparse change stamps against a dense model: one stamp per cell, written
// by the same rules and never dropped.

/// Wider than 64 columns, so the stamps span both halves of a `ColumnSet`.
const WIDE: usize = 70;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct WideColumn(u16);

static WIDE_COLUMNS: [WideColumn; WIDE] = {
    let mut columns = [WideColumn(0); WIDE];
    let mut ix = 0;
    while ix < WIDE {
        columns[ix] = WideColumn(ix as u16);
        ix += 1;
    }
    columns
};

impl GridColumns for WideColumn {
    fn all() -> &'static [Self] {
        &WIDE_COLUMNS
    }

    /// Not read by the store.
    fn id(self) -> &'static str {
        "wide"
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self.0)
    }
}

#[derive(Clone, Debug)]
struct WideRow {
    id: u32,
    values: [i64; WIDE],
}

impl GridRow for WideRow {
    type Key = u32;
    type Column = WideColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: WideColumn) -> CellValue<'_> {
        CellValue::Int(self.values[usize::from(column.0)])
    }
}

/// One stamp per cell of a row, the dense form of the store's sparse
/// stamps; `None` for a row that was never stamped.
type DenseStamps = Option<Vec<Stamp>>;

/// Writes `stamp(column)` into each of `columns` of a dense row.
fn dense_write(dense: &mut DenseStamps, columns: ColumnSet, stamp: impl Fn(WideColumn) -> Stamp) {
    let cells = dense.get_or_insert_with(|| vec![Stamp::NONE; WIDE]);
    for &column in WideColumn::all() {
        if columns.contains(column.ix()) {
            cells[usize::from(column.0)] = stamp(column);
        }
    }
}

#[gpui_kit::test]
fn test_sparse_stamps_match_dense_model(cx: &mut TestAppContext) {
    let policy = HighlightPolicy::new(Duration::from_millis(1_000), 4);
    for seed in 0..12 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let epoch = cx.executor().now();
        let mut ingest = Ingest::<WideRow>::new(policy, epoch);
        let mut model: std::collections::BTreeMap<u32, (WideRow, DenseStamps)> = Default::default();
        let mut now_ms = 0;
        for _ in 0..120 {
            now_ms += rng.u32(0..400);
            let ms = now_ms;
            let mut ops = Vec::new();
            for _ in 0..rng.usize(1..6) {
                let key = rng.u32(0..12);
                let choice = rng.u8(0..20);
                if choice == 0 {
                    model.remove(&key);
                    ops.push(Op::Remove(key));
                    continue;
                }
                let Some((row, dense)) = model.get_mut(&key) else {
                    // An insert flashes every cell, unless it is silent.
                    let row = WideRow {
                        id: key,
                        values: std::array::from_fn(|_| rng.i64(0..4)),
                    };
                    let highlight = choice != 1;
                    let dense = highlight.then(|| vec![Stamp::new(ms, Inserted); WIDE]);
                    model.insert(key, (row.clone(), dense));
                    let row = Arc::new(row);
                    ops.push(if highlight {
                        Op::Upsert(row)
                    } else {
                        Op::UpsertSilent(row)
                    });
                    continue;
                };
                match choice {
                    // A mark, sometimes of every column: the row's stamp.
                    2 | 3 => {
                        let columns = if choice == 2 {
                            ColumnSet::first(WIDE)
                        } else {
                            let mut columns = ColumnSet::EMPTY;
                            for _ in 0..rng.usize(1..4) {
                                columns.insert(ColumnIx::new(rng.u16(0..WIDE as u16)));
                            }
                            columns
                        };
                        dense_write(dense, columns, |_| Stamp::new(ms, Updated));
                        ops.push(Op::MarkChanged(key, columns));
                    }
                    // A replacement: a few cells, or every cell up at once.
                    _ => {
                        let previous = row.clone();
                        if choice == 4 {
                            row.values.iter_mut().for_each(|value| *value += 1);
                        } else {
                            for _ in 0..rng.usize(0..4) {
                                row.values[rng.usize(0..WIDE)] += rng.i64(-2..=2);
                            }
                        }
                        let highlight = choice != 5;
                        let mut columns = ColumnSet::EMPTY;
                        row.changed_columns(&previous, &mut columns);
                        if highlight {
                            dense_write(dense, columns, |column| {
                                let direction = row.change_direction(&previous, column);
                                Stamp::with_direction(ms, Updated, direction)
                            });
                        }
                        let row = Arc::new(row.clone());
                        ops.push(if highlight {
                            Op::Upsert(row)
                        } else {
                            Op::UpsertSilent(row)
                        });
                    }
                }
            }
            let now = epoch + Duration::from_millis(u64::from(ms));
            ingest.apply(ops, now);
            ingest.sweep(now);

            // Every tick a frame can read from now on, until the newest
            // stamp is over: a stamp the model shows is the store's, and a
            // stamp the model does not show, the store does not either.
            let snapshot = ingest.snapshot();
            assert_eq!(snapshot.len(), model.len());
            let mut tick = policy.tick(ms);
            for _ in 0..6 {
                let hot = |stamp: Stamp| policy.intensity(stamp, tick).is_some();
                for (key, (_, dense)) in &model {
                    let id = snapshot.row_id(key).expect("live row");
                    let sparse = snapshot.stamps(id);
                    for &column in WideColumn::all() {
                        let expected = dense
                            .as_ref()
                            .map_or(Stamp::NONE, |cells| cells[usize::from(column.0)]);
                        let actual = sparse.map_or(Stamp::NONE, |stamps| stamps.get(column.ix()));
                        if hot(expected) {
                            assert_eq!(actual, expected, "seed {seed}, key {key}, {column:?}");
                        } else {
                            assert!(!hot(actual), "seed {seed}, key {key}, {column:?}");
                        }
                    }
                }
                tick = policy.next_tick(tick);
            }
        }
        // Sparse rows hold fewer bytes than one stamp per cell.
        let snapshot = ingest.snapshot();
        assert!(snapshot.stamp_bytes() <= ingest.hot_rows() * (16 + WIDE * 4));
    }
}

// Write coalescing (`WriteCoalescing`): a store that merges the waiting
// upserts of a key publishes what a store that applies every write
// publishes, and its queue grows with the keys written, not the writes.

/// One store per `WriteCoalescing` over the same clock, written the same:
/// `ByKey` first. Each records the changes it publishes.
struct Twins {
    stores: [GridStore<Leg>; 2],
    writers: [GridWriter<Leg>; 2],
    changes: [Rc<RefCell<ChangeSet>>; 2],
    reloads: [Option<Reload<Leg>>; 2],
    policy: HighlightPolicy,
}

impl Twins {
    fn new(cx: &mut TestAppContext) -> Self {
        let policy = HighlightPolicy::new(Duration::from_millis(600), 4)
            .with_removal_fade(Duration::from_millis(300));
        let stores = [WriteCoalescing::ByKey, WriteCoalescing::KeepEveryWrite].map(|coalescing| {
            let options = StoreOptions::default()
                .with_highlight(policy)
                .with_write_coalescing(coalescing);
            cx.update(|cx| GridStore::<Leg>::new(options, cx))
        });
        let changes = std::array::from_fn(|ix| {
            let changes = Rc::new(RefCell::new(ChangeSet::default()));
            let subscription = stores[ix].subscribe();
            cx.foreground_executor()
                .spawn({
                    let changes = changes.clone();
                    async move {
                        while subscription.changed().await {
                            changes.borrow_mut().merge(&subscription.take().1);
                        }
                    }
                })
                .detach();
            changes
        });
        Self {
            writers: std::array::from_fn(|ix| stores[ix].writer()),
            stores,
            changes,
            reloads: [None, None],
            policy,
        }
    }

    /// Writes the same to both stores.
    fn write(&self, write: impl Fn(&GridWriter<Leg>)) {
        self.writers.iter().for_each(write);
    }

    /// Both stores published the same: rows by id, change stamps while
    /// they show, fading rows, bands, progress, version and changes.
    fn assert_same(&self, now_ms: u32, context: &str) {
        let [merged, every] = [0, 1].map(|ix| self.stores[ix].snapshot());
        assert_eq!(merged.version(), every.version(), "{context}");
        assert_eq!(merged.capacity(), every.capacity(), "{context}");
        let rows = |snapshot: &StoreSnapshot<Leg>| -> Vec<(RowId, *const Leg, u64)> {
            snapshot
                .iter()
                .map(|(id, row)| (id, Arc::as_ptr(row), snapshot.seq(id)))
                .collect()
        };
        assert_eq!(rows(&merged), rows(&every), "{context}");
        for (id, _) in every.iter() {
            self.assert_same_stamps(merged.stamps(id), every.stamps(id), now_ms, context);
        }
        let fading = |snapshot: &StoreSnapshot<Leg>| {
            let mut rows: Vec<_> = snapshot
                .fading_rows()
                .map(|(id, row, removed, seq)| (id.ix(), Arc::as_ptr(row), removed, seq))
                .collect();
            rows.sort_by_key(|(ix, ..)| *ix);
            rows
        };
        assert_eq!(fading(&merged), fading(&every), "{context}");
        for edge in [Top, Bottom] {
            let band = |snapshot: &StoreSnapshot<Leg>| -> Vec<*const Leg> {
                snapshot.pinned_rows(edge).iter().map(Arc::as_ptr).collect()
            };
            assert_eq!(band(&merged), band(&every), "{context}");
            for ix in 0..every.pinned_rows(edge).len() {
                self.assert_same_stamps(
                    merged.pinned_stamps(edge, ix),
                    every.pinned_stamps(edge, ix),
                    now_ms,
                    context,
                );
            }
        }
        assert_eq!(merged.progress(), every.progress(), "{context}");

        let [merged, every] = [0, 1].map(|ix| std::mem::take(&mut *self.changes[ix].borrow_mut()));
        let ids = |ids: &[RowId]| {
            let mut ids = ids.to_vec();
            ids.sort_by_key(|id| id.ix());
            ids
        };
        assert_eq!(merged.inserted(), every.inserted(), "{context}");
        assert_eq!(merged.removed(), every.removed(), "{context}");
        assert_eq!(ids(merged.faded()), ids(every.faded()), "{context}");
        // A row once per merged entry, against once per write.
        let mut updated = ids(every.updated());
        updated.dedup();
        let mut merged_updated = ids(merged.updated());
        merged_updated.dedup();
        assert_eq!(merged_updated, updated, "{context}");
        assert_eq!(merged.columns(), every.columns(), "{context}");
        assert_eq!(merged.is_reset(), every.is_reset(), "{context}");
        assert_eq!(
            merged.has_parent_change(),
            every.has_parent_change(),
            "{context}"
        );
        assert_eq!(
            merged.has_pinned_change(),
            every.has_pinned_change(),
            "{context}"
        );
        assert_eq!(
            merged.has_progress_change(),
            every.has_progress_change(),
            "{context}"
        );
    }

    /// The same stamp in every cell, at every tick from `now_ms` until it
    /// is over, wherever either shows.
    fn assert_same_stamps(
        &self,
        merged: Option<RowStamps<'_>>,
        every: Option<RowStamps<'_>>,
        now_ms: u32,
        context: &str,
    ) {
        let policy = self.policy;
        let mut tick = policy.tick(now_ms);
        for _ in 0..6 {
            for column in LegColumn::all() {
                let stamp = |stamps: Option<RowStamps<'_>>| {
                    stamps.map_or(Stamp::NONE, |stamps| stamps.get(column.ix()))
                };
                let (merged, every) = (stamp(merged), stamp(every));
                let hot = |stamp: Stamp| policy.intensity(stamp, tick).is_some();
                if hot(merged) || hot(every) {
                    assert_eq!(merged, every, "{context}, {column:?}");
                }
            }
            tick = policy.next_tick(tick);
        }
    }
}

/// A random row of `key`: few values, so a value often returns to an
/// earlier one between two batches, and parents that change.
fn random_leg(rng: &mut fastrand::Rng, key: u32) -> Arc<Leg> {
    let qty = rng.i64(0..3);
    let qty = if rng.bool() {
        Qty::Int(qty)
    } else {
        Qty::Float(qty as f64)
    };
    let parent = [None, None, Some(100), Some(101)][rng.usize(..4)];
    let venue = ["XLON", "XPAR"][rng.usize(..2)];
    Arc::new(
        Leg::new(key, qty, rng.i64(0..3) as f64)
            .venue(venue)
            .parent(parent),
    )
}

#[gpui_kit::test]
fn test_write_coalescing_publishes_what_every_write_does(cx: &mut TestAppContext) {
    const KEYS: u32 = 8;
    let mut coalesced = 0;
    for seed in 0..40 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut twins = Twins::new(cx);
        let epoch = twins.stores[0].snapshot().epoch();
        let mut last: Vec<Option<Arc<Leg>>> = vec![None; KEYS as usize];
        for round in 0..50 {
            for _ in 0..rng.usize(1..14) {
                let key = rng.u32(0..KEYS);
                match rng.u8(0..100) {
                    0..55 => {
                        let row = random_leg(&mut rng, key);
                        last[key as usize] = Some(row.clone());
                        twins.write(|writer| writer.upsert(row.clone()));
                    }
                    55..63 => {
                        let row = random_leg(&mut rng, key);
                        last[key as usize] = Some(row.clone());
                        twins.write(|writer| writer.upsert_silent(row.clone()));
                    }
                    // The same row again changes nothing.
                    63..68 => {
                        if let Some(row) = last[key as usize].clone() {
                            twins.write(|writer| writer.upsert(row.clone()));
                        }
                    }
                    68..76 => twins.write(|writer| writer.remove(key)),
                    76..80 => {
                        let mut columns = ColumnSet::EMPTY;
                        columns.insert(LegColumn::all()[rng.usize(..4)].ix());
                        twins.write(|writer| writer.mark_changed(key, columns));
                    }
                    80..82 => {
                        let rows: Vec<_> = (0..rng.u32(0..KEYS))
                            .map(|key| random_leg(&mut rng, key))
                            .collect();
                        twins.write(|writer| writer.replace_all(rows.iter().cloned()));
                    }
                    82..83 => twins.write(GridWriter::clear),
                    83..87 => {
                        let edge = if rng.bool() { Top } else { Bottom };
                        let rows: Vec<_> = (0..rng.u32(0..3))
                            .map(|key| random_leg(&mut rng, key))
                            .collect();
                        twins.write(|writer| writer.set_pinned_rows(edge, rows.iter().cloned()));
                    }
                    87..90 => {
                        let progress = rng.bool().then(|| GridProgress::new(rng.f32()));
                        twins.write(|writer| writer.set_progress(progress.clone()));
                    }
                    // A reload: begun, superseded, finished or cancelled.
                    90..94 => match rng.u8(0..4) {
                        0 | 1 => {
                            for ix in 0..2 {
                                twins.reloads[ix] = Some(twins.writers[ix].begin_reload());
                            }
                        }
                        2 => {
                            let rows: Vec<_> = (0..rng.u32(0..KEYS))
                                .map(|key| random_leg(&mut rng, key))
                                .collect();
                            for ix in 0..2 {
                                if let Some(reload) = twins.reloads[ix].take() {
                                    twins.writers[ix].batch(|batch| {
                                        batch.finish_reload(reload, rows.iter().cloned());
                                        batch.set_progress(None);
                                    });
                                }
                            }
                        }
                        _ => twins.reloads = [None, None],
                    },
                    _ => {
                        let rows: Vec<_> = (0..rng.usize(2..6))
                            .map(|_| {
                                let key = rng.u32(0..KEYS);
                                random_leg(&mut rng, key)
                            })
                            .collect();
                        twins.write(|writer| {
                            writer.batch(|batch| {
                                rows.iter().for_each(|row| batch.upsert(row.clone()))
                            })
                        });
                    }
                }
            }
            settle(cx);
            // Stamps age and fades end between batches.
            cx.executor()
                .advance_clock(Duration::from_millis(rng.u64(0..500)));
            cx.run_until_parked();
            let now_ms = (cx.executor().now() - epoch).as_millis() as u32;
            twins.assert_same(now_ms, &format!("seed {seed}, round {round}"));
        }
        twins.reloads = [None, None];
        settle(cx);
        let [merged, every] = [0, 1].map(|ix| twins.stores[ix].stats());
        assert_eq!(merged.writes(), every.writes(), "seed {seed}");
        assert_eq!(merged.applied_writes(), merged.writes(), "seed {seed}");
        assert_eq!(every.applied_writes(), every.writes(), "seed {seed}");
        assert_eq!(every.coalesced_writes(), 0);
        assert!(merged.peak_pending() <= every.peak_pending(), "seed {seed}");
        coalesced += merged.coalesced_writes();
    }
    assert!(coalesced > 500, "{coalesced} writes merged");
}

/// The ordering edge cases of the rules (`.agents/COMPONENT_TEST_RULES.md`)
/// and the queue's bound: a stream over a few keys queues one entry per
/// key, a removal or a reset in between is a barrier, and the latest write
/// of each key wins.
#[gpui_kit::test]
fn test_write_coalescing_bounds_the_queue(cx: &mut TestAppContext) {
    let twins = Twins::new(cx);
    let epoch = twins.stores[0].snapshot().epoch();
    let row = |id: u32, price: f64| Arc::new(Leg::new(id, Qty::Int(1), price));
    // A storm over 10 keys while the ingest is busy: 1000 writes.
    for write in 0..1_000 {
        let row = row(write % 10, f64::from(write));
        twins.write(|writer| writer.upsert(row.clone()));
    }
    let [merged, every] = [0, 1].map(|ix| twins.stores[ix].stats());
    assert_eq!((merged.pending(), merged.coalesced_writes()), (10, 990));
    assert_eq!((every.pending(), every.coalesced_writes()), (1_000, 0));
    settle(cx);
    let [merged, every] = [0, 1].map(|ix| twins.stores[ix].stats());
    assert_eq!((merged.peak_pending(), every.peak_pending()), (10, 1_000));
    assert_eq!((merged.pending(), merged.applied_writes()), (0, 1_000));
    assert_eq!(merged.batches(), every.batches());
    assert!(merged.max_batch() >= merged.last_batch());
    let now_ms = (cx.executor().now() - epoch).as_millis() as u32;
    twins.assert_same(now_ms, "storm");
    let snapshot = twins.stores[0].snapshot();
    assert_eq!(snapshot.get(&3).map(|leg| leg.price), Some(993.));

    // A value that returns within a batch still flashes, with the direction
    // of its last change; a removal, `clear` and `replace_all` are barriers.
    let rows: Vec<Arc<Leg>> = [(1, 2_000.), (1, 991.), (2, 7.), (2, 8.), (4, 1.), (4, 2.)]
        .into_iter()
        .chain([(5, 3.), (5, 4.), (6, 5.), (5, 6.), (6, 7.), (6, 6.)])
        .map(|(id, price)| row(id, price))
        .collect();
    twins.write(|writer| {
        writer.upsert(rows[0].clone());
        writer.upsert(rows[1].clone());
        writer.upsert(rows[2].clone());
        writer.remove(2);
        writer.upsert(rows[3].clone());
        writer.upsert(rows[4].clone());
        writer.clear();
        writer.upsert(rows[5].clone());
        writer.upsert(rows[6].clone());
        writer.replace_all([rows[7].clone(), rows[8].clone()]);
        writer.upsert(rows[9].clone());
        writer.upsert(rows[10].clone());
        writer.upsert(rows[11].clone());
    });
    // Writes after each barrier merge only with each other.
    assert_eq!(twins.stores[0].stats().pending(), 11);
    settle(cx);
    let now_ms = (cx.executor().now() - epoch).as_millis() as u32;
    twins.assert_same(now_ms, "barriers");
    let snapshot = twins.stores[0].snapshot();
    let prices: Vec<(u32, f64)> = snapshot
        .iter()
        .map(|(_, leg)| (leg.id, leg.price))
        .collect();
    assert_eq!(prices.len(), 2, "{prices:?}");
    for (key, price) in [(5, 6.), (6, 6.)] {
        assert_eq!(
            snapshot.get(&key).map(|leg| leg.price),
            Some(price),
            "{key}"
        );
    }
    let stamps = snapshot.stamps(snapshot.row_id(&6).expect("row 6"));
    let price = stamps.map(|stamps| stamps.get(LegColumn::Price.ix()));
    assert_eq!(
        price.map(|stamp| stamp.direction()),
        Some(Some(ChangeDirection::Down))
    );

    // A cancelled reload queues what it buffered after what was written
    // before it: a later upsert does not merge past it.
    let rows = [1., 2., 3.].map(|price| row(5, price));
    for ix in 0..2 {
        twins.writers[ix].upsert(rows[0].clone());
        let reload = twins.writers[ix].begin_reload();
        twins.writers[ix].upsert(rows[1].clone());
        drop(reload);
        twins.writers[ix].upsert(rows[2].clone());
    }
    settle(cx);
    let now_ms = (cx.executor().now() - epoch).as_millis() as u32;
    twins.assert_same(now_ms, "cancelled reload");
    let snapshot = twins.stores[0].snapshot();
    assert_eq!(snapshot.get(&5).map(|leg| leg.price), Some(3.));
}
