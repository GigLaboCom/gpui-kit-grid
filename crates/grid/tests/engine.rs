//! Engine and ingest tests against a naive reference model
//! (`.agents/COMPONENT_TEST_RULES.md`, "Pure layers get plain tests").

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::HashMap,
    ops::{ControlFlow, RangeInclusive},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit_grid::{
    CellTexts, CellValue, ColumnIx, ColumnSet, ContainsAllTerms, GridColumns, GridRow,
    HighlightPolicy, Ingest, Op, ProjectionEngine, QuickFilter, QuickFilterMatcher,
    QuickFilterQuery, QuickFilterRule, Rebuild, RowChange, SortDirection, StoreSnapshot, ViewSpec,
};

/// Counts the allocations of each thread, so a test can prove a pass does
/// not allocate per row while other tests run in parallel.
struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TestColumn {
    Id,
    Group,
    Price,
    Name,
    Flag,
    /// Money, with the `decimal` feature.
    #[cfg(feature = "decimal")]
    Amount,
}

impl GridColumns for TestColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Group,
            Self::Price,
            Self::Name,
            Self::Flag,
            #[cfg(feature = "decimal")]
            Self::Amount,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Group => "group",
            Self::Price => "price",
            Self::Name => "name",
            Self::Flag => "flag",
            #[cfg(feature = "decimal")]
            Self::Amount => "amount",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TestRow {
    id: u32,
    group: i64,
    price: f64,
    name: String,
    /// `None` is an `Empty` cell.
    flag: Option<bool>,
    #[cfg(feature = "decimal")]
    amount: rust_decimal::Decimal,
}

impl GridRow for TestRow {
    type Key = u32;
    type Column = TestColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: TestColumn) -> CellValue<'_> {
        match column {
            TestColumn::Id => CellValue::Int(self.id.into()),
            TestColumn::Group => CellValue::Int(self.group),
            TestColumn::Price => CellValue::Float(self.price),
            TestColumn::Name => CellValue::Text(&self.name),
            TestColumn::Flag => self.flag.map_or(CellValue::Empty, CellValue::Bool),
            #[cfg(feature = "decimal")]
            TestColumn::Amount => CellValue::Decimal(self.amount),
        }
    }
}

/// A small alphabet in both cases, ASCII and Cyrillic, so names tie often and
/// quick filter terms often match.
const NAME_CHARS: &[char] = &['a', 'A', 'b', 'B', 'я', 'Я', 'ж', 'Ж'];

/// Quick filter terms also search the numbers and the check mark of `Bool`.
const TERM_CHARS: &[char] = &[
    'a', 'A', 'b', 'B', 'я', 'Я', 'ж', 'Ж', '0', '1', '5', '.', '✓',
];

fn random_text(rng: &mut fastrand::Rng, chars: &[char], len: RangeInclusive<usize>) -> String {
    (0..rng.usize(len))
        .map(|_| chars[rng.usize(..chars.len())])
        .collect()
}

fn random_row(rng: &mut fastrand::Rng, id: u32) -> TestRow {
    TestRow {
        id,
        group: rng.i64(0..4),
        // Few distinct prices, so ties exercise the stable tie-breaker.
        price: f64::from(rng.u8(0..10)) / 2.0,
        name: random_text(rng, NAME_CHARS, 0..=3),
        flag: [None, Some(false), Some(true)][rng.usize(..3)],
        #[cfg(feature = "decimal")]
        amount: random_amount(rng),
    }
}

/// Few distinct amounts, -2.00 to 2.00 by 0.25, each with two scales
/// (`0.50` and `0.5`), zeros with either sign: ties across scales exercise
/// the stable tie-breaker, and the quick filter searches their text.
#[cfg(feature = "decimal")]
fn random_amount(rng: &mut fastrand::Rng) -> rust_decimal::Decimal {
    let mut amount = rust_decimal::Decimal::new(rng.i64(-8..=8) * 25, 2);
    if rng.bool() {
        amount = amount.normalize();
    }
    if amount.is_zero() && rng.bool() {
        amount.set_sign_negative(true);
    }
    amount
}

/// A view as the tests drive it, built both as a [`ViewSpec`] and naively.
#[derive(Clone, Debug)]
struct TestView {
    sort: Vec<(TestColumn, SortDirection)>,
    /// Only even groups.
    filter: bool,
    quick_text: String,
    quick_columns: Vec<TestColumn>,
}

impl TestView {
    fn random(rng: &mut fastrand::Rng) -> Self {
        let mut view = Self {
            sort: Vec::new(),
            filter: false,
            quick_text: String::new(),
            quick_columns: Vec::new(),
        };
        view.randomize_sort(rng);
        view.filter = rng.bool();
        view.randomize_quick_filter(rng);
        view
    }

    fn randomize_sort(&mut self, rng: &mut fastrand::Rng) {
        let columns = TestColumn::all();
        self.sort = (0..rng.usize(0..=2))
            .map(|_| {
                let direction = if rng.bool() {
                    SortDirection::Ascending
                } else {
                    SortDirection::Descending
                };
                (columns[rng.usize(..columns.len())], direction)
            })
            .collect();
    }

    /// One to three terms between random whitespace, sometimes blank text.
    fn randomize_quick_filter(&mut self, rng: &mut fastrand::Rng) {
        let space = |rng: &mut fastrand::Rng| [" ", "  ", "\t", " \n "][rng.usize(..4)];
        self.quick_text = match rng.u8(0..10) {
            0 => String::new(),
            1 => space(rng).to_owned(),
            _ => {
                let mut text = String::new();
                if rng.bool() {
                    text.push_str(space(rng));
                }
                for ix in 0..rng.usize(1..=3) {
                    if ix > 0 {
                        text.push_str(space(rng));
                    }
                    text.push_str(&random_text(rng, TERM_CHARS, 1..=2));
                }
                if rng.bool() {
                    text.push_str(space(rng));
                }
                text
            }
        };
        self.quick_columns = TestColumn::all()
            .iter()
            .copied()
            .filter(|_| rng.u8(0..10) < 7)
            .collect();
    }

    fn spec(&self) -> ViewSpec<TestRow> {
        let mut quick_columns = ColumnSet::EMPTY;
        for column in &self.quick_columns {
            quick_columns.insert(column.ix());
        }
        let quick_filter = QuickFilter::new(&self.quick_text, quick_columns, &ContainsAllTerms);
        assert_eq!(quick_filter.is_none(), self.quick_text.trim().is_empty());
        let spec = ViewSpec::new()
            .with_sort(self.sort.clone())
            .with_quick_filter(quick_filter);
        if self.filter {
            let mut depends = ColumnSet::EMPTY;
            depends.insert(TestColumn::Group.ix());
            spec.with_filter(depends, |row: &TestRow| row.group % 2 == 0)
        } else {
            spec
        }
    }

    /// The naive rule: every term occurs, lowercased, in the display text
    /// of some quick filter column; `Empty` and `Bool` cells have no text.
    fn passes(&self, row: &TestRow) -> bool {
        if self.filter && row.group % 2 != 0 {
            return false;
        }
        let texts: Vec<String> = self
            .quick_columns
            .iter()
            .filter_map(|column| match row.value(*column) {
                CellValue::Text(text) => Some(text.to_lowercase()),
                CellValue::Int(value) => Some(format!("{value}")),
                CellValue::Float(value) => Some(format!("{value:.2}")),
                #[cfg(feature = "decimal")]
                CellValue::Decimal(value) => Some(format!("{value}")),
                _ => None,
            })
            .collect();
        self.quick_text
            .to_lowercase()
            .split_whitespace()
            .all(|term| texts.iter().any(|text| text.contains(term)))
    }
}

/// The reference: a map from key to (row, insertion sequence).
#[derive(Default)]
struct Reference {
    rows: HashMap<u32, (TestRow, u64)>,
    next_seq: u64,
}

impl Reference {
    fn upsert(&mut self, row: TestRow) {
        if let Some(entry) = self.rows.get_mut(&row.id) {
            entry.0 = row;
        } else {
            self.rows.insert(row.id, (row, self.next_seq));
            self.next_seq += 1;
        }
    }

    fn order(&self, view: &TestView) -> Vec<u32> {
        let mut rows: Vec<&(TestRow, u64)> = self
            .rows
            .values()
            .filter(|(row, _)| view.passes(row))
            .collect();
        rows.sort_by(|(a, a_seq), (b, b_seq)| {
            view.sort
                .iter()
                .map(|(column, direction)| {
                    let ordering = a.value(*column).cmp_total(&b.value(*column));
                    match direction {
                        SortDirection::Ascending => ordering,
                        SortDirection::Descending => ordering.reverse(),
                    }
                })
                .find(|ordering| ordering.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a_seq.cmp(b_seq))
        });
        rows.into_iter().map(|(row, _)| row.id).collect()
    }
}

fn keys(snapshot: &StoreSnapshot<TestRow>, spec: &ViewSpec<TestRow>) -> Vec<u32> {
    keys_of(Rebuild::default(), snapshot, spec)
}

fn keys_of(
    engine: impl ProjectionEngine<TestRow>,
    snapshot: &StoreSnapshot<TestRow>,
    spec: &ViewSpec<TestRow>,
) -> Vec<u32> {
    let projection = engine.rebuild(snapshot, spec);
    for (display_ix, id) in projection.rows().iter().enumerate() {
        assert_eq!(projection.display_ix(*id), Some(display_ix));
    }
    projection
        .rows()
        .iter()
        .map(|id| snapshot.row(*id).expect("projected rows are live").id)
        .collect()
}

/// `row` with a new value in `column`, which is not the key.
fn with_changed_column(rng: &mut fastrand::Rng, row: &TestRow, column: TestColumn) -> TestRow {
    let fresh = random_row(rng, row.id);
    let mut row = row.clone();
    match column {
        TestColumn::Id => unreachable!("the key does not change"),
        TestColumn::Group => row.group = fresh.group,
        TestColumn::Price => row.price = fresh.price,
        TestColumn::Name => row.name = fresh.name,
        TestColumn::Flag => row.flag = fresh.flag,
        #[cfg(feature = "decimal")]
        TestColumn::Amount => row.amount = fresh.amount,
    }
    row
}

#[test]
fn test_rebuild_matches_reference_model() {
    let epoch = Instant::now();
    for seed in 0..60 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut ingest = Ingest::<TestRow>::new(HighlightPolicy::default(), epoch);
        let mut reference = Reference::default();
        let mut view = TestView::random(&mut rng);

        for _step in 0..40 {
            // The view changes between data batches, as a user sorts and types.
            if rng.u8(0..10) < 3 {
                view.randomize_sort(&mut rng);
            }
            if rng.u8(0..10) < 3 {
                view.filter = rng.bool();
            }
            if rng.u8(0..10) < 4 {
                view.randomize_quick_filter(&mut rng);
            }
            let mut ops = Vec::new();
            for _ in 0..rng.usize(1..30) {
                let id = rng.u32(0..60);
                match rng.u8(0..100) {
                    0..=59 => {
                        let row = random_row(&mut rng, id);
                        reference.upsert(row.clone());
                        ops.push(Op::Upsert(Arc::new(row)));
                    }
                    60..=89 => {
                        reference.rows.remove(&id);
                        ops.push(Op::Remove(id));
                    }
                    90..=95 => {
                        let rows: Vec<TestRow> = (0..rng.u32(0..20))
                            .map(|id| random_row(&mut rng, id * 3))
                            .collect();
                        reference.rows.clear();
                        for row in &rows {
                            reference.upsert(row.clone());
                        }
                        ops.push(Op::ReplaceAll(rows.into_iter().map(Arc::new).collect()));
                    }
                    _ => {
                        reference.rows.clear();
                        ops.push(Op::Clear);
                    }
                }
            }
            ingest.apply(ops, Instant::now());
            let snapshot = ingest.snapshot();
            assert_eq!(snapshot.len(), reference.rows.len(), "seed {seed}");

            let spec = view.spec();
            let shown = keys(&snapshot, &spec);
            assert_eq!(shown, reference.order(&view), "seed {seed}, {view:?}");

            // A change outside the dependencies moves no row: the grid skips
            // the rebuild for it.
            let dependencies = spec.dependencies();
            let outside: Vec<TestColumn> = TestColumn::all()
                .iter()
                .copied()
                .filter(|column| *column != TestColumn::Id && !dependencies.contains(column.ix()))
                .collect();
            if outside.is_empty() {
                continue;
            }
            let column = outside[rng.usize(..outside.len())];
            let changed: Vec<TestRow> = reference
                .rows
                .values()
                .map(|(row, _)| with_changed_column(&mut rng, row, column))
                .collect();
            for row in &changed {
                reference.upsert(row.clone());
            }
            ingest.apply(
                changed.into_iter().map(|row| Op::Upsert(Arc::new(row))),
                Instant::now(),
            );
            assert_eq!(
                keys(&ingest.snapshot(), &spec),
                shown,
                "seed {seed}, {column:?} changed, {view:?}"
            );
        }
    }
}

#[test]
fn test_large_rebuild_matches_reference_model() {
    // Large enough that `Rebuild` splits the work across up to four threads
    // (8192 rows each): the order is the reference order on any number of
    // them, by default (the machine's cores) or as set.
    let epoch = Instant::now();
    let mut rng = fastrand::Rng::with_seed(46);
    let mut ingest = Ingest::<TestRow>::new(HighlightPolicy::default(), epoch);
    let mut reference = Reference::default();
    let rows: Vec<TestRow> = (0..40_000).map(|id| random_row(&mut rng, id)).collect();
    for row in &rows {
        reference.upsert(row.clone());
    }
    ingest.apply(rows.into_iter().map(|row| Op::Upsert(Arc::new(row))), epoch);
    // Reused slots put storage order out of insertion order.
    let removed: Vec<u32> = (0..10_000).map(|_| rng.u32(..40_000)).collect();
    for id in &removed {
        reference.rows.remove(id);
    }
    ingest.apply(removed.into_iter().map(Op::Remove), epoch);
    let fresh: Vec<TestRow> = (40_000..45_000)
        .map(|id| random_row(&mut rng, id))
        .collect();
    for row in &fresh {
        reference.upsert(row.clone());
    }
    ingest.apply(
        fresh.into_iter().map(|row| Op::Upsert(Arc::new(row))),
        epoch,
    );
    let snapshot = ingest.snapshot();

    let engines = [
        Rebuild::default(),
        Rebuild::default().with_max_threads(1),
        Rebuild::default().with_max_threads(2),
        Rebuild::default().with_max_threads(3),
        Rebuild::default().with_max_threads(16),
    ];
    for _ in 0..6 {
        let view = TestView::random(&mut rng);
        let expected = reference.order(&view);
        for engine in engines {
            assert_eq!(
                keys_of(engine, &snapshot, &view.spec()),
                expected,
                "{view:?}, {engine:?}"
            );
        }
    }
}

/// A second quick filter rule through the seam: a term must be a whole cell.
struct WholeCell;

impl QuickFilterMatcher for WholeCell {
    fn compile(&self, text: &str) -> QuickFilterRule {
        QuickFilterRule::Custom(Arc::new(WholeCellQuery(text.to_owned())))
    }
}

struct WholeCellQuery(String);

impl QuickFilterQuery for WholeCellQuery {
    fn matches(&self, cells: &CellTexts<'_>) -> bool {
        let mut found = false;
        cells.visit(|_, text| {
            found = text == self.0;
            if found {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        found
    }
}

#[test]
fn test_quick_filter_seam_and_allocations() {
    let epoch = Instant::now();
    let mut rng = fastrand::Rng::with_seed(7);
    let mut ingest = Ingest::<TestRow>::new(HighlightPolicy::default(), epoch);
    let rows: Vec<TestRow> = (0..20_000).map(|id| random_row(&mut rng, id)).collect();
    ingest.apply(
        rows.iter().cloned().map(|row| Op::Upsert(Arc::new(row))),
        epoch,
    );
    let snapshot = ingest.snapshot();
    let all = ColumnSet::first(TestColumn::all().len());

    // An application rule replaces the built-in one.
    let spec = ViewSpec::new().with_quick_filter(QuickFilter::new("Ab", all, &WholeCell));
    let expected: Vec<u32> = rows
        .iter()
        .filter(|row| row.name == "Ab")
        .map(|row| row.id)
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(keys(&snapshot, &spec), expected);

    // Neither rule allocates per row: a rebuild of 20k rows allocates only
    // while its buffers and the projection grow. Cyrillic names take the
    // lowercase buffer, numbers the format buffer. The counter is per
    // thread, so the rebuild runs on this thread alone and the counter sees
    // every row.
    for (text, matcher) in [
        ("я 0.5 жб", &ContainsAllTerms as &dyn QuickFilterMatcher),
        ("zzz", &ContainsAllTerms),
        ("Ab", &WholeCell),
    ] {
        let spec = ViewSpec::new()
            .with_sort([(TestColumn::Name, SortDirection::Ascending)])
            .with_quick_filter(QuickFilter::new(text, all, matcher));
        let before = allocations();
        let projection = Rebuild::default()
            .with_max_threads(1)
            .rebuild(&snapshot, &spec);
        let allocated = allocations() - before;
        assert!(allocated < 100, "{text}: {allocated} allocations");
        drop(projection);
    }
}

#[test]
fn test_ingest_row_ids_and_stamps() {
    let epoch = Instant::now();
    let policy = HighlightPolicy::new(Duration::from_millis(1_000), 4);
    let mut ingest = Ingest::<TestRow>::new(policy, epoch);
    let row = TestRow {
        id: 1,
        group: 0,
        price: 1.0,
        name: "a".into(),
        flag: None,
        #[cfg(feature = "decimal")]
        amount: rust_decimal::Decimal::ONE,
    };

    // Silent load: no stamps.
    ingest.apply([Op::UpsertSilent(Arc::new(row.clone()))], epoch);
    let snapshot = ingest.snapshot();
    let (id, _) = snapshot.iter().next().expect("one row");
    assert!(snapshot.stamps(id).is_none());

    // An update stamps only the changed column.
    let at = epoch + Duration::from_millis(100);
    let changes = ingest.apply(
        [Op::Upsert(Arc::new(TestRow {
            price: 2.0,
            ..row.clone()
        }))],
        at,
    );
    assert_eq!(changes.updated().len(), 1);
    assert!(changes.columns().contains(TestColumn::Price.ix()));
    assert!(!changes.columns().contains(TestColumn::Name.ix()));
    // A new row with the same values is published, without columns and
    // without a stamp (tests/store.rs covers it); the same `Arc` is no change.
    let same = Arc::new(TestRow {
        price: 2.0,
        ..row.clone()
    });
    let unchanged = ingest.apply([Op::Upsert(same.clone())], at);
    assert_eq!(unchanged.updated(), [id]);
    assert!(unchanged.columns().is_empty());
    assert!(ingest.apply([Op::Upsert(same)], at).is_empty());
    let snapshot = ingest.snapshot();
    let stamps = snapshot.stamps(id).expect("hot row");
    assert!(stamps.get(TestColumn::Price.ix()).is_set());
    assert_eq!(
        stamps.get(TestColumn::Price.ix()).change(),
        RowChange::Updated
    );
    assert!(!stamps.get(TestColumn::Name.ix()).is_set());
    assert_eq!(ingest.hot_rows(), 1);

    // A held snapshot keeps the old value: pages are copied on write.
    let held = ingest.snapshot();
    ingest.apply(
        [Op::Upsert(Arc::new(TestRow {
            price: 3.0,
            ..row.clone()
        }))],
        at,
    );
    assert_eq!(held.row(id).expect("live").price, 2.0);
    assert_eq!(ingest.snapshot().row(id).expect("live").price, 3.0);

    // Sweeping frees stamps after the highlight expired.
    ingest.sweep(epoch + Duration::from_millis(2_000));
    assert_eq!(ingest.hot_rows(), 0);
    assert!(ingest.snapshot().stamps(id).is_none());

    // A removed row's id stops resolving, even when its slot is reused, and
    // replace_all invalidates every id.
    ingest.apply([Op::Remove(1)], at);
    ingest.apply(
        [Op::Upsert(Arc::new(TestRow {
            id: 2,
            ..row.clone()
        }))],
        at,
    );
    let snapshot = ingest.snapshot();
    assert!(snapshot.row(id).is_none());
    let (reused, inserted) = snapshot.iter().next().expect("one row");
    assert_eq!(reused.ix(), id.ix());
    assert_eq!(inserted.id, 2);
    assert_eq!(
        snapshot
            .stamps(reused)
            .expect("inserted rows flash")
            .get(ColumnIx::new(0))
            .change(),
        RowChange::Inserted
    );
    ingest.apply(
        [Op::ReplaceAll(vec![Arc::new(TestRow { id: 3, ..row })])],
        at,
    );
    assert!(ingest.snapshot().row(reused).is_none());
}

#[test]
fn test_compact_and_key_index_match_reference() {
    let epoch = Instant::now();
    for seed in 0..60 {
        let mut rng = fastrand::Rng::with_seed(1_000 + seed);
        let mut ingest = Ingest::<TestRow>::new(HighlightPolicy::default(), epoch);
        let mut reference = Reference::default();
        let rows: Vec<TestRow> = (0..rng.u32(1..80))
            .map(|id| random_row(&mut rng, id))
            .collect();
        for row in &rows {
            reference.upsert(row.clone());
        }
        ingest.apply(rows.into_iter().map(|row| Op::Upsert(Arc::new(row))), epoch);

        let view = TestView::random(&mut rng);
        let spec = view.spec();
        let before = Rebuild::default().rebuild(&ingest.snapshot(), &spec);

        // Only removals: compacting the old projection must give exactly the
        // order a full rebuild gives.
        let removed: Vec<u32> = (0..rng.u32(0..40)).map(|_| rng.u32(0..80)).collect();
        let changes = ingest.apply(removed.iter().map(|id| Op::Remove(*id)), epoch);
        for id in &removed {
            reference.rows.remove(id);
        }
        assert!(!changes.has_insertions_or_reset());
        let snapshot = ingest.snapshot();
        let compacted = Rebuild::default().compact(&before, &snapshot);
        let rebuilt = Rebuild::default().rebuild(&snapshot, &spec);
        assert_eq!(compacted.rows(), rebuilt.rows(), "seed {seed}");
        let keys: Vec<u32> = compacted
            .rows()
            .iter()
            .map(|id| snapshot.row(*id).expect("live").id)
            .collect();
        assert_eq!(keys, reference.order(&view), "seed {seed}");

        // The published key index agrees with the rows, and removed ids are
        // reported for the view's "is it on screen" check.
        for id in 0..80 {
            assert_eq!(
                snapshot.get(&id).map(|row| row.id),
                reference.rows.get(&id).map(|(row, _)| row.id),
                "seed {seed}, key {id}"
            );
        }
        for removed_id in changes.removed() {
            assert!(snapshot.row(*removed_id).is_none());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MixedColumn {
    Id,
    Value,
}

impl GridColumns for MixedColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Value]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Value => "value",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A row whose one column holds `Int` and `Float` values alike, as a column
/// fed by two sources does.
struct MixedRow {
    id: u32,
    value: CellValue<'static>,
}

impl GridRow for MixedRow {
    type Key = u32;
    type Column = MixedColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: MixedColumn) -> CellValue<'_> {
        match column {
            MixedColumn::Id => CellValue::Int(self.id.into()),
            MixedColumn::Value => self.value,
        }
    }
}

#[test]
fn test_rebuild_sorts_mixed_numbers_with_signed_zeros() {
    use CellValue::{Float, Int};
    // Ascending, written out by hand: each rung holds equal values. -0.0
    // sorts just below 0, as under `f64::total_cmp`.
    let p63 = 9_223_372_036_854_775_808.0;
    #[cfg_attr(not(feature = "decimal"), expect(unused_mut))]
    let mut ladder: Vec<Vec<CellValue<'static>>> = vec![
        vec![Float(f64::from_bits(0xFFF8_0000_0000_0000))],
        vec![Float(f64::NEG_INFINITY)],
        vec![Int(i64::MIN), Float(-p63)],
        vec![Int(-1), Float(-1.0)],
        vec![Float(-0.5)],
        vec![Float(-0.0)],
        vec![Int(0), Float(0.0)],
        vec![Float(0.5)],
        vec![Int(1), Float(1.0)],
        vec![Int((1 << 53) + 1)],
        vec![Float(f64::INFINITY)],
        vec![Float(f64::from_bits(0x7FF8_0000_0000_0000))],
    ];
    // Decimals of several scales join the numbers of their value; their
    // zeros, of either sign, join `Int(0)` above -0.0.
    #[cfg(feature = "decimal")]
    {
        use rust_decimal::Decimal;
        let mut negative_zero = Decimal::new(0, 2);
        negative_zero.set_sign_negative(true);
        for (rung, value) in [
            (2, Decimal::from(i64::MIN)),
            (3, Decimal::new(-100, 2)),
            (4, Decimal::new(-5, 1)),
            (6, Decimal::ZERO),
            (6, Decimal::new(0, 3)),
            (6, negative_zero),
            (7, Decimal::new(50, 2)),
            (8, Decimal::ONE),
            (9, Decimal::from((1i64 << 53) + 1)),
        ] {
            ladder[rung].push(CellValue::Decimal(value));
        }
    }
    let epoch = Instant::now();
    let engines = [
        Rebuild::default().with_max_threads(1),
        Rebuild::default().with_max_threads(3),
    ];
    // Small sets on many seeds, then one large enough for three threads.
    let sets = (0..40)
        .map(|seed| (seed, 1 + seed * 37))
        .chain([(99, 30_000)]);
    for (seed, count) in sets {
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut ingest = Ingest::<MixedRow>::new(HighlightPolicy::default(), epoch);
        // Mostly zeros of the three kinds, interleaved, then the rest.
        let rungs: Vec<usize> = (0..count)
            .map(|_| {
                if rng.u8(0..4) == 0 {
                    rng.usize(..ladder.len())
                } else {
                    rng.usize(5..=6)
                }
            })
            .collect();
        ingest.apply(
            rungs.iter().enumerate().map(|(id, &rung)| {
                let values = &ladder[rung];
                Op::Upsert(Arc::new(MixedRow {
                    id: id as u32,
                    value: values[rng.usize(..values.len())],
                }))
            }),
            epoch,
        );
        let snapshot = ingest.snapshot();
        for direction in [SortDirection::Ascending, SortDirection::Descending] {
            // The reference: by rung, ties in insertion order (here the id).
            let mut expected: Vec<u32> = (0..count as u32).collect();
            expected.sort_by_key(|&id| {
                let rung = rungs[id as usize];
                match direction {
                    SortDirection::Ascending => rung as isize,
                    SortDirection::Descending => -(rung as isize),
                }
            });
            let spec = ViewSpec::new().with_sort([(MixedColumn::Value, direction)]);
            for engine in engines {
                let shown: Vec<u32> = engine
                    .rebuild(&snapshot, &spec)
                    .rows()
                    .iter()
                    .map(|id| snapshot.row(*id).expect("projected rows are live").id)
                    .collect();
                assert_eq!(shown, expected, "seed {seed}, {direction:?}, {engine:?}");
            }
        }
    }
}

#[test]
#[cfg(feature = "decimal")]
fn test_decimal_quick_filter_and_raw() {
    use gpui_kit_grid::{CellExport, CellExporter, DisplayedCells, ExportRow, RawCells};
    use rust_decimal::Decimal;

    let row = |id: u32, amount: Decimal| TestRow {
        id,
        group: 0,
        price: 0.0,
        name: String::new(),
        flag: None,
        amount,
    };
    let epoch = Instant::now();
    let mut ingest = Ingest::<TestRow>::new(HighlightPolicy::default(), epoch);
    ingest.apply(
        [
            row(1, Decimal::new(1230, 2)),
            row(2, Decimal::new(-5, 2)),
            row(3, Decimal::new(123, 1)),
        ]
        .map(|row| Op::Upsert(Arc::new(row))),
        epoch,
    );
    let snapshot = ingest.snapshot();

    // The quick filter searches the text the cell shows, scale included:
    // "12.3" occurs in both 12.30 and 12.3, "12.30" only in the first.
    let mut amount = ColumnSet::EMPTY;
    amount.insert(TestColumn::Amount.ix());
    let found = |text: &str| {
        keys(
            &snapshot,
            &ViewSpec::new()
                .with_sort([(TestColumn::Amount, SortDirection::Ascending)])
                .with_quick_filter(QuickFilter::new(text, amount, &ContainsAllTerms)),
        )
    };
    assert_eq!(found("12.3"), [1, 3]);
    assert_eq!(found("12.30"), [1]);
    assert_eq!(found("-0.05"), [2]);
    assert_eq!(found("0.0"), [2]);
    assert_eq!(found("0"), [2, 1]);
    assert_eq!(found("12,30"), Vec::<u32>::new());
    // Without a filter, ascending by value: -0.05, then 12.30 and 12.3,
    // equal, in insertion order.
    assert_eq!(found(" "), [2, 1, 3]);

    // Raw cells keep the scale and take the decimal separator; displayed
    // cells are the painted text.
    let text = |cells: &CellExport<TestRow>, value: Decimal| {
        let row = row(0, value);
        let cell = ExportRow::new(&row).cell(TestColumn::Amount).unwrap();
        let mut out = String::from(">");
        cells.write_cell(&cell, &mut out);
        out
    };
    let raw = CellExport::Raw(RawCells::new());
    let comma = CellExport::Raw(RawCells::new().with_decimal_separator(','));
    let displayed = CellExport::Displayed(DisplayedCells);
    assert_eq!(text(&raw, Decimal::new(1230, 2)), ">12.30");
    assert_eq!(text(&comma, Decimal::new(1230, 2)), ">12,30");
    assert_eq!(text(&comma, Decimal::new(-5, 2)), ">-0,05");
    assert_eq!(text(&comma, Decimal::new(-1234, 0)), ">-1234");
    assert_eq!(text(&raw, Decimal::MAX), ">79228162514264337593543950335");
    assert_eq!(text(&displayed, Decimal::new(1230, 2)), ">12.30");
}

/// Ten columns as in the S11 scenario: five text, five numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct BenchColumn(u16);

const BENCH_COLUMNS: [BenchColumn; 10] = {
    let mut columns = [BenchColumn(0); 10];
    let mut ix = 0;
    while ix < 10 {
        columns[ix] = BenchColumn(ix as u16);
        ix += 1;
    }
    columns
};

impl GridColumns for BenchColumn {
    fn all() -> &'static [Self] {
        &BENCH_COLUMNS
    }

    fn id(self) -> &'static str {
        ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"][usize::from(self.0)]
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self.0)
    }
}

struct BenchRow {
    id: u64,
    texts: [String; 5],
    floats: [f64; 3],
    ints: [i64; 2],
}

impl GridRow for BenchRow {
    type Key = u64;
    type Column = BenchColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: BenchColumn) -> CellValue<'_> {
        match column.0 {
            0..=4 => CellValue::Text(&self.texts[usize::from(column.0)]),
            5..=7 => CellValue::Float(self.floats[usize::from(column.0) - 5]),
            _ => CellValue::Int(self.ints[usize::from(column.0) - 8]),
        }
    }
}

/// Times quick filter rebuilds of 100k rows. Not a gate: run it in release
/// with `cargo test --release -p gpui-kit-grid --test engine -- --ignored
/// --nocapture`.
#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
#[cfg(not(target_family = "wasm"))]
fn test_quick_filter_rebuild_timing() {
    const ROWS: u64 = 100_000;
    let words: [(&str, [&str; 8]); 2] = [
        (
            "ASCII",
            [
                "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
            ],
        ),
        (
            "Cyrillic",
            [
                "Сбербанк",
                "Газпром",
                "Лукойл",
                "Норникель",
                "Роснефть",
                "Татнефть",
                "Полюс",
                "Магнит",
            ],
        ),
    ];
    let all = ColumnSet::first(BENCH_COLUMNS.len());
    let first_text = BenchColumn(1);
    println!("| Text | Rebuild 100000 rows × 10 columns | Shown | median | max |");
    println!("|---|---|---|---|---|");
    for (alphabet, words) in words {
        let epoch = Instant::now();
        let mut rng = fastrand::Rng::with_seed(11);
        let mut ingest = Ingest::<BenchRow>::new(HighlightPolicy::default(), epoch);
        let word = |rng: &mut fastrand::Rng| words[rng.usize(..words.len())];
        ingest.apply(
            (0..ROWS).map(|id| {
                Op::UpsertSilent(Arc::new(BenchRow {
                    id,
                    texts: [
                        format!("T{:05}", rng.u32(..50_000)),
                        format!("{} {}", word(&mut rng), rng.u16(..1_000)),
                        format!("{} {}", word(&mut rng), word(&mut rng)),
                        word(&mut rng).to_owned(),
                        format!("{} {} {}", word(&mut rng), rng.u16(..100), word(&mut rng)),
                    ],
                    floats: [rng.f64() * 500.0, rng.f64() * 10.0, rng.f64() * 1e6],
                    ints: [rng.i64(0..1_000_000), rng.i64(-500..500)],
                }))
            }),
            epoch,
        );
        let snapshot = ingest.snapshot();
        let common = words[2].to_lowercase();
        let rare = format!("{} 9", &words[5][..4]);
        let cases = [
            ("no quick filter, insertion order", None, false),
            ("no quick filter, text sort", None, true),
            ("1 term, never matches", Some("zzq".to_owned()), false),
            (
                "1 term, never matches, text sort",
                Some("zzq".to_owned()),
                true,
            ),
            ("1 term, frequent, text sort", Some(common.clone()), true),
            ("3 terms, rare, text sort", Some(format!("{rare} 99")), true),
            (
                "3 terms, frequent, text sort",
                Some(format!("{common} 1 2")),
                true,
            ),
        ];
        for (name, text, sorted) in cases {
            let spec = ViewSpec::new()
                .with_sort(sorted.then_some((first_text, SortDirection::Ascending)))
                .with_quick_filter(
                    text.and_then(|text| QuickFilter::new(&text, all, &ContainsAllTerms)),
                );
            let mut samples: Vec<Duration> = Vec::new();
            let mut shown = 0;
            for _ in 0..7 {
                let started = Instant::now();
                shown = Rebuild::default().rebuild(&snapshot, &spec).len();
                samples.push(started.elapsed());
            }
            samples.sort();
            println!(
                "| {alphabet} | {name} | {shown} | {:.1} ms | {:.1} ms |",
                samples[samples.len() / 2].as_secs_f64() * 1e3,
                samples[samples.len() - 1].as_secs_f64() * 1e3,
            );
        }
    }
}
