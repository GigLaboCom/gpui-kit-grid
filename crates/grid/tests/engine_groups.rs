//! Grouping, aggregates and expansion against a naive reference model
//! (`.agents/COMPONENT_TEST_RULES.md`, "Pure layers get plain tests").
//!
//! Every check takes the engine, so another [`ProjectionEngine`] runs them
//! by adding a line to [`engines`]. The data sets are small: up to 300
//! rows. Thread independence with small chunks and a panicking `GroupKey`
//! on several threads are unit tests of `engine/mod.rs`, which can split a
//! small snapshot.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    mem,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit_grid::{
    Aggregate, AggregateFn, AggregateOrder, CellValue, ChangeDirection, ColumnId, ColumnIx,
    ColumnSet, ContainsAllTerms, DisplayRow, EveryGroupFooter, ExpandDepth, Expansion, GridColumns,
    GridRow, GroupBy, GroupFooter, GroupInfo, GroupKey, GroupPath, HighlightPolicy, Ingest,
    KeyOrder, NoGroupFooter, Op, OwnedValue, Projection, ProjectionEngine, QuickFilter, Rebuild,
    RowChange, RowId, SortDirection, Stamp, StoreSnapshot, ViewSpec,
};

/// Counts the allocations of each thread, so a test can prove a rebuild
/// allocates per group, not per row, while other tests run in parallel.
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

// ---------------------------------------------------------------------------
// Rows: few values per column, so keys tie often; every kind of value, with
// `Empty`, NaN, both zeros and equal `Int` and `Float`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Col {
    Id,
    Group,
    Price,
    Name,
    Flag,
    Mixed,
    Qty,
}

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &[
            Self::Id,
            Self::Group,
            Self::Price,
            Self::Name,
            Self::Flag,
            Self::Mixed,
            Self::Qty,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Group => "group",
            Self::Price => "price",
            Self::Name => "name",
            Self::Flag => "flag",
            Self::Mixed => "mixed",
            Self::Qty => "qty",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// The columns a test may change: all but the key.
const MUTABLE: [Col; 6] = [
    Col::Group,
    Col::Price,
    Col::Name,
    Col::Flag,
    Col::Mixed,
    Col::Qty,
];

#[derive(Clone, Debug)]
struct TestRow {
    id: u32,
    group: i64,
    price: f64,
    name: String,
    flag: Option<bool>,
    /// A value of every kind, see [`mixed_value`].
    mixed: u8,
    qty: i64,
}

fn mixed_value(mixed: u8) -> CellValue<'static> {
    match mixed {
        0 => CellValue::Empty,
        1 => CellValue::Bool(true),
        2 => CellValue::Int(1),
        3 => CellValue::Float(1.0),
        4 => CellValue::Int(2),
        5 => CellValue::Float(2.5),
        6 => CellValue::Text("x"),
        7 => CellValue::Float(-0.0),
        8 => CellValue::Int(0),
        _ => CellValue::Float(0.0),
    }
}

impl GridRow for TestRow {
    type Key = u32;
    type Column = Col;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        match column {
            Col::Id => CellValue::Int(self.id.into()),
            Col::Group => CellValue::Int(self.group),
            Col::Price => CellValue::Float(self.price),
            Col::Name => CellValue::Text(&self.name),
            Col::Flag => self.flag.map_or(CellValue::Empty, CellValue::Bool),
            Col::Mixed => mixed_value(self.mixed),
            Col::Qty => CellValue::Int(self.qty),
        }
    }
}

/// Prices whose float sums depend on the order of addition.
const PRICES: [f64; 8] = [0.5, 1.0, 1.5, -0.0, 0.0, 2.25, 0.1, 0.7];
const NAME_CHARS: [char; 3] = ['a', 'B', 'я'];

fn random_row(rng: &mut fastrand::Rng, id: u32) -> TestRow {
    TestRow {
        id,
        group: rng.i64(0..4),
        price: if rng.u8(0..40) == 0 {
            f64::NAN
        } else {
            PRICES[rng.usize(..PRICES.len())]
        },
        name: (0..rng.usize(0..=2))
            .map(|_| NAME_CHARS[rng.usize(..NAME_CHARS.len())])
            .collect(),
        flag: [None, Some(false), Some(true)][rng.usize(..3)],
        mixed: rng.u8(0..10),
        // Now and then near `i64::MAX`: two of them overflow an `i64` sum.
        qty: if rng.u8(0..25) == 0 {
            i64::MAX - rng.i64(0..3)
        } else {
            rng.i64(-5..=5)
        },
    }
}

/// `row` with a new value in `column`, which is not the key.
fn with_changed(rng: &mut fastrand::Rng, row: &TestRow, column: Col) -> TestRow {
    let fresh = random_row(rng, row.id);
    let mut row = row.clone();
    match column {
        Col::Id => unreachable!("the key does not change"),
        Col::Group => row.group = fresh.group,
        Col::Price => row.price = fresh.price,
        Col::Name => row.name = fresh.name,
        Col::Flag => row.flag = fresh.flag,
        Col::Mixed => row.mixed = fresh.mixed,
        Col::Qty => row.qty = fresh.qty,
    }
    row
}

fn columns(columns: &[Col]) -> ColumnSet {
    let mut set = ColumnSet::EMPTY;
    for column in columns {
        set.insert(column.ix());
    }
    set
}

fn first_letter(row: &TestRow) -> CellValue<'_> {
    let end = row.name.chars().next().map_or(0, char::len_utf8);
    CellValue::Text(&row.name[..end])
}

// ---------------------------------------------------------------------------
// The store and its reference: rows by key with their insertion sequence.

/// A removal fade no test outlasts: removed rows fade until a test moves
/// the clock past it.
const FADE: Duration = Duration::from_secs(3_600);

/// The store's highlight policy: removed rows fade with `fade`.
fn policy(fade: bool) -> HighlightPolicy {
    if fade {
        HighlightPolicy::default().with_removal_fade(FADE)
    } else {
        HighlightPolicy::default()
    }
}

#[derive(Default)]
struct Reference {
    rows: HashMap<u32, (TestRow, u64)>,
    /// Removed rows that fade, by key.
    fading: HashMap<u32, (TestRow, u64)>,
    next_seq: u64,
    /// Removed rows fade.
    fade: bool,
}

impl Reference {
    fn new(fade: bool) -> Self {
        Self {
            fade,
            ..Self::default()
        }
    }

    /// A new row of a fading key takes its place: the fading one goes.
    fn upsert(&mut self, row: TestRow) {
        if let Some(entry) = self.rows.get_mut(&row.id) {
            entry.0 = row;
        } else {
            self.fading.remove(&row.id);
            self.rows.insert(row.id, (row, self.next_seq));
            self.next_seq += 1;
        }
    }

    fn remove(&mut self, key: u32) {
        if let Some(entry) = self.rows.remove(&key)
            && self.fade
        {
            self.fading.insert(key, entry);
        }
    }
}

/// A store of `count` random rows with some removed and more added, so
/// slots are reused and storage order differs from insertion order.
fn random_store(rng: &mut fastrand::Rng, count: u32) -> (Ingest<TestRow>, Reference) {
    random_store_with(rng, count, false)
}

/// [`random_store`], with the removed rows fading when `fade`: the new rows
/// take their slots, so a fading row often shares its slot with a live one.
fn random_store_with(
    rng: &mut fastrand::Rng,
    count: u32,
    fade: bool,
) -> (Ingest<TestRow>, Reference) {
    let epoch = Instant::now();
    let mut ingest = Ingest::new(policy(fade), epoch);
    let mut reference = Reference::new(fade);
    let upsert = |ingest: &mut Ingest<TestRow>, reference: &mut Reference, row: TestRow| {
        reference.upsert(row.clone());
        ingest.apply([Op::UpsertSilent(Arc::new(row))], epoch);
    };
    for id in 0..count {
        let row = random_row(rng, id);
        upsert(&mut ingest, &mut reference, row);
    }
    for _ in 0..count / 4 {
        let key = rng.u32(..count.max(1));
        reference.remove(key);
        ingest.apply([Op::Remove(key)], epoch);
    }
    for id in count..count + count / 5 {
        let row = random_row(rng, id);
        upsert(&mut ingest, &mut reference, row);
    }
    (ingest, reference)
}

// ---------------------------------------------------------------------------
// Views as the tests drive them, built both as a `ViewSpec` and naively.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Level {
    Column(Col),
    /// `GroupKey` "letter": the first letter of the name, borrowed.
    Letter,
    /// `GroupKey` "sign": the sign of the quantity.
    Sign,
}

impl Level {
    fn group_by(self, direction: SortDirection) -> GroupBy<TestRow> {
        match self {
            Self::Column(column) => GroupBy::column(column),
            Self::Letter => GroupBy::key(
                GroupKey::new("letter", columns(&[Col::Name]), first_letter)
                    .with_title("First letter"),
            ),
            Self::Sign => GroupBy::key(GroupKey::new(
                "sign",
                columns(&[Col::Qty]),
                |row: &TestRow| CellValue::Int(row.qty.signum()),
            )),
        }
        .with_direction(direction)
    }

    fn id(self) -> ColumnId {
        match self {
            Self::Column(column) => column.column_id(),
            Self::Letter => ColumnId::from("letter"),
            Self::Sign => ColumnId::from("sign"),
        }
    }

    fn value(self, row: &TestRow) -> CellValue<'_> {
        match self {
            Self::Column(column) => row.value(column),
            Self::Letter => first_letter(row),
            Self::Sign => CellValue::Int(row.qty.signum()),
        }
    }
}

/// The application's aggregates: a combinable one and one that reads rows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Agg {
    Sum,
    Avg,
    Min,
    Max,
    Count,
    /// The rows seen, combined from subgroups' counts.
    RowsSeen,
    /// The name of the first row, in key order: cannot combine.
    FirstName,
}

const AGGS: [Agg; 7] = [
    Agg::Sum,
    Agg::Avg,
    Agg::Min,
    Agg::Max,
    Agg::Count,
    Agg::RowsSeen,
    Agg::FirstName,
];

struct RowsSeen;

impl AggregateFn<TestRow> for RowsSeen {
    fn aggregate(&self, _: Col, rows: &mut dyn Iterator<Item = &TestRow>) -> OwnedValue {
        OwnedValue::Int(rows.count() as i64)
    }

    fn combine(&self, parts: &[&OwnedValue]) -> Option<OwnedValue> {
        let mut sum = 0;
        for part in parts {
            match part {
                OwnedValue::Int(count) => sum += count,
                _ => return None,
            }
        }
        Some(OwnedValue::Int(sum))
    }

    fn depends_on(&self, _: Col) -> ColumnSet {
        ColumnSet::EMPTY
    }

    fn name(&self) -> &str {
        "Rows seen"
    }
}

struct FirstName;

impl AggregateFn<TestRow> for FirstName {
    fn aggregate(&self, _: Col, rows: &mut dyn Iterator<Item = &TestRow>) -> OwnedValue {
        rows.next().map_or(OwnedValue::Empty, |row| {
            OwnedValue::Text(row.name.as_str().into())
        })
    }

    fn depends_on(&self, column: Col) -> ColumnSet {
        columns(&[column, Col::Name])
    }
}

impl Agg {
    fn aggregate(self) -> Aggregate<TestRow> {
        match self {
            Self::Sum => Aggregate::Sum,
            Self::Avg => Aggregate::Avg,
            Self::Min => Aggregate::Min,
            Self::Max => Aggregate::Max,
            Self::Count => Aggregate::Count,
            Self::RowsSeen => Aggregate::Custom(Arc::new(RowsSeen)),
            Self::FirstName => Aggregate::Custom(Arc::new(FirstName)),
        }
    }
}

/// The footers of the top level only: a footer rule of the application.
struct TopFooter;

impl GroupFooter for TopFooter {
    fn has_footer(&self, group: &GroupInfo<'_>) -> bool {
        group.depth() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Footer {
    None,
    Every,
    Top,
}

impl Footer {
    fn has_footer(self, depth: u16) -> bool {
        match self {
            Self::None => false,
            Self::Every => true,
            Self::Top => depth == 0,
        }
    }
}

#[derive(Clone, Debug)]
struct GroupView {
    levels: Vec<(Level, SortDirection)>,
    sort: Vec<(Col, SortDirection)>,
    /// Only groups below 3.
    filter: bool,
    /// The quick filter on the name.
    quick: String,
    /// One aggregate per column, in schema order.
    aggregates: Vec<(Col, Agg)>,
    /// `AggregateOrder` by a column, aggregated or not.
    order: Option<(Col, SortDirection)>,
    footer: Footer,
    depth: ExpandDepth,
    /// Groups set expanded or collapsed against the depth.
    toggles: Vec<(GroupPath, bool)>,
}

fn direction(rng: &mut fastrand::Rng) -> SortDirection {
    [SortDirection::Ascending, SortDirection::Descending][rng.usize(..2)]
}

impl GroupView {
    fn random(rng: &mut fastrand::Rng) -> Self {
        let choices = [
            Level::Column(Col::Group),
            Level::Column(Col::Price),
            Level::Column(Col::Flag),
            Level::Column(Col::Mixed),
            Level::Column(Col::Name),
            Level::Letter,
            Level::Sign,
        ];
        let mut levels: Vec<(Level, SortDirection)> = Vec::new();
        for _ in 0..rng.usize(1..=3) {
            let level = choices[rng.usize(..choices.len())];
            if levels.iter().all(|(kept, _)| *kept != level) {
                levels.push((level, direction(rng)));
            }
        }
        let sort = (0..rng.usize(0..=2))
            .map(|_| (Col::all()[rng.usize(..Col::all().len())], direction(rng)))
            .collect();
        let mut aggregates: Vec<(Col, Agg)> = Vec::new();
        for &column in Col::all() {
            if rng.u8(0..10) < 6 {
                aggregates.push((column, AGGS[rng.usize(..AGGS.len())]));
            }
        }
        let order = match rng.u8(0..3) {
            0 => None,
            // Mostly an aggregated column; else the key order stays.
            1 if !aggregates.is_empty() => {
                Some((aggregates[rng.usize(..aggregates.len())].0, direction(rng)))
            }
            _ => Some((Col::all()[rng.usize(..Col::all().len())], direction(rng))),
        };
        Self {
            levels,
            sort,
            filter: rng.u8(0..4) == 0,
            quick: ["", "", "a", "b", "я", "ab"][rng.usize(..6)].to_owned(),
            aggregates,
            order,
            footer: [Footer::None, Footer::Every, Footer::Top][rng.usize(..3)],
            depth: [
                ExpandDepth::All,
                ExpandDepth::All,
                ExpandDepth::Levels(0),
                ExpandDepth::Levels(1),
                ExpandDepth::Levels(2),
                ExpandDepth::Captions,
            ][rng.usize(..6)],
            toggles: Vec::new(),
        }
    }

    /// Toggles a random few of `paths`, and one path no group has.
    fn randomize_toggles(&mut self, rng: &mut fastrand::Rng, paths: &[GroupPath]) {
        self.toggles.clear();
        for path in paths {
            if rng.u8(0..4) == 0 {
                self.toggles.push((path.clone(), rng.bool()));
            }
        }
        self.toggles.push((
            GroupPath::new([(ColumnId::from("group"), OwnedValue::Int(99))]),
            rng.bool(),
        ));
    }

    fn levels(&self) -> usize {
        self.levels.len()
    }

    fn spec(&self) -> ViewSpec<TestRow> {
        let quick = QuickFilter::new(&self.quick, columns(&[Col::Name]), &ContainsAllTerms);
        let mut spec = ViewSpec::new()
            .with_sort(self.sort.clone())
            .with_quick_filter(quick)
            .with_group_by(
                self.levels
                    .iter()
                    .map(|&(level, direction)| level.group_by(direction)),
            )
            .with_aggregates(
                self.aggregates
                    .iter()
                    .map(|&(column, aggregate)| (column, aggregate.aggregate())),
            );
        if self.filter {
            spec = spec.with_filter(columns(&[Col::Group]), |row: &TestRow| row.group < 3);
        }
        spec = match self.order {
            Some((column, direction)) => {
                spec.with_group_order(AggregateOrder::new(column, direction))
            }
            None => spec.with_group_order(KeyOrder),
        };
        spec = match self.footer {
            Footer::None => spec.with_group_footer(NoGroupFooter),
            Footer::Every => spec.with_group_footer(EveryGroupFooter),
            Footer::Top => spec.with_group_footer(TopFooter),
        };
        let expansion =
            self.toggles
                .iter()
                .fold(Expansion::new(self.depth), |expansion, (path, expanded)| {
                    expansion.with_group(path.clone(), *expanded, self.levels())
                });
        spec.with_expansion(expansion)
    }

    /// The same view with everything expanded and nothing toggled.
    fn expanded(&self) -> Self {
        Self {
            depth: ExpandDepth::All,
            toggles: Vec::new(),
            ..self.clone()
        }
    }

    fn passes(&self, row: &TestRow) -> bool {
        (!self.filter || row.group < 3) && {
            let name = row.name.to_lowercase();
            self.quick
                .to_lowercase()
                .split_whitespace()
                .all(|term| name.contains(term))
        }
    }

    /// The expansion rule of `ExpandDepth`, with the last toggle of a path.
    fn is_expanded(&self, path: &GroupPath) -> bool {
        let depth = usize::from(path.depth());
        let default = match self.depth {
            ExpandDepth::All => true,
            ExpandDepth::Levels(levels) => depth < usize::from(levels),
            ExpandDepth::Captions => depth + 1 < self.levels(),
            _ => unreachable!("the tests use the depths above"),
        };
        self.toggles
            .iter()
            .rev()
            .find(|(toggled, _)| toggled == path)
            .map_or(default, |&(_, expanded)| expanded)
    }
}

fn directed(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Ascending => ordering,
        SortDirection::Descending => ordering.reverse(),
    }
}

// ---------------------------------------------------------------------------
// The reference model.

/// The numbers of a sum or a mean: integers add up in `i128`, floats apart;
/// a sum is an `Int` while it has no float and fits `i64`, a `Float`
/// otherwise; a mean is a `Float`.
#[derive(Clone, Copy, Default)]
struct Numbers {
    int: i128,
    float: f64,
    has_float: bool,
    count: u64,
}

impl Numbers {
    fn of(values: impl Iterator<Item = OwnedValue>) -> Self {
        let mut numbers = Self::default();
        for value in values {
            match value {
                OwnedValue::Int(value) => {
                    numbers.int += i128::from(value);
                    numbers.count += 1;
                }
                OwnedValue::Float(value) => {
                    numbers.float += value;
                    numbers.has_float = true;
                    numbers.count += 1;
                }
                _ => {}
            }
        }
        numbers
    }

    /// Subgroups' numbers added in key order.
    fn sum_of(parts: impl Iterator<Item = Self>) -> Self {
        let mut numbers = Self::default();
        for part in parts {
            numbers.int += part.int;
            numbers.float += part.float;
            numbers.has_float |= part.has_float;
            numbers.count += part.count;
        }
        numbers
    }

    fn sum(&self) -> OwnedValue {
        if self.count == 0 {
            OwnedValue::Empty
        } else if self.has_float {
            OwnedValue::Float(self.float + self.int as f64)
        } else {
            i64::try_from(self.int).map_or(OwnedValue::Float(self.int as f64), OwnedValue::Int)
        }
    }

    fn mean(&self) -> OwnedValue {
        if self.count == 0 {
            OwnedValue::Empty
        } else {
            OwnedValue::Float((self.float + self.int as f64) / self.count as f64)
        }
    }
}

/// The extreme value of `values` by `cmp_total`, `Empty` aside; the first
/// of equal ones.
fn extreme(values: impl Iterator<Item = OwnedValue>, side: Ordering) -> OwnedValue {
    values
        .filter(|value| *value != OwnedValue::Empty)
        .fold(OwnedValue::Empty, |kept, value| {
            if kept == OwnedValue::Empty || value.cmp_total(&kept) == side {
                value
            } else {
                kept
            }
        })
}

/// A group of the reference, in key order until ordered.
struct RefGroup {
    path: GroupPath,
    /// Rows in key order, each live or fading.
    rows: Vec<(TestRow, bool)>,
    children: Vec<RefGroup>,
    /// A sum or a mean's numbers per aggregate, for the parent.
    numbers: Vec<Numbers>,
    /// One value per aggregate of the view.
    values: Vec<OwnedValue>,
}

/// One aggregate of `rows`, a group's in key order, and of its subgroups'
/// (`children`, in key order; empty for a group of the last level or for
/// totals without groups).
fn aggregate(
    column: Col,
    function: Agg,
    rows: &[TestRow],
    children: &[(Numbers, &OwnedValue)],
    has_children: bool,
) -> (Numbers, OwnedValue) {
    let values = || {
        rows.iter()
            .map(|row| OwnedValue::from_cell(row.value(column)))
    };
    let numbers = if has_children {
        Numbers::sum_of(children.iter().map(|(numbers, _)| *numbers))
    } else {
        Numbers::of(values())
    };
    let value = match function {
        Agg::Sum => numbers.sum(),
        Agg::Avg => numbers.mean(),
        Agg::Min => extreme(values(), Ordering::Less),
        Agg::Max => extreme(values(), Ordering::Greater),
        Agg::Count => {
            OwnedValue::Int(values().filter(|value| *value != OwnedValue::Empty).count() as i64)
        }
        Agg::RowsSeen => OwnedValue::Int(rows.len() as i64),
        Agg::FirstName => rows.first().map_or(OwnedValue::Empty, |row| {
            OwnedValue::Text(row.name.as_str().into())
        }),
    };
    (numbers, value)
}

/// The live rows of `rows`.
fn live_rows(rows: &[(TestRow, bool)]) -> Vec<TestRow> {
    rows.iter()
        .filter(|(_, live)| *live)
        .map(|(row, _)| row.clone())
        .collect()
}

/// Splits rows sorted in key order into the groups of `depth` and below.
/// Aggregates read the live rows alone.
fn split(
    view: &GroupView,
    rows: &[(TestRow, bool)],
    depth: usize,
    parent: &GroupPath,
) -> Vec<RefGroup> {
    let (level, _) = view.levels[depth];
    let mut groups = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let key = level.value(&rows[start].0);
        let end = start
            + rows[start..]
                .iter()
                .take_while(|(row, _)| level.value(row).cmp_total(&key).is_eq())
                .count();
        let path = GroupPath::new(
            parent
                .levels()
                .iter()
                .cloned()
                .chain([(level.id(), OwnedValue::from_cell(key))]),
        );
        let group_rows = rows[start..end].to_vec();
        let children = if depth + 1 < view.levels() {
            split(view, &group_rows, depth + 1, &path)
        } else {
            Vec::new()
        };
        let live = live_rows(&group_rows);
        let mut numbers = Vec::new();
        let mut values = Vec::new();
        for (place, &(column, function)) in view.aggregates.iter().enumerate() {
            let parts: Vec<(Numbers, &OwnedValue)> = children
                .iter()
                .map(|child: &RefGroup| (child.numbers[place], &child.values[place]))
                .collect();
            let (number, value) = aggregate(column, function, &live, &parts, !children.is_empty());
            numbers.push(number);
            values.push(value);
        }
        groups.push(RefGroup {
            path,
            rows: group_rows,
            children,
            numbers,
            values,
        });
        start = end;
    }
    groups
}

/// Sorts siblings stably by the group order, at every level.
fn order(view: &GroupView, groups: &mut [RefGroup]) {
    if let Some((column, direction)) = view.order
        && let Some(place) = view.aggregates.iter().position(|(kept, _)| *kept == column)
    {
        groups.sort_by(|a, b| directed(a.values[place].cmp_total(&b.values[place]), direction));
    }
    for group in groups {
        order(view, &mut group.children);
    }
}

#[derive(Clone, Debug, PartialEq)]
enum RefLine {
    Data(u32, u16),
    Caption(GroupPath, u16),
    Footer(GroupPath, u16),
}

/// What a group of a projection should read.
#[derive(Debug)]
struct RefInfo {
    path: GroupPath,
    parent: Option<GroupPath>,
    rows: Vec<u32>,
    /// Live rows: fading ones are in `rows` alone.
    live: usize,
    expanded: bool,
    footer: bool,
    values: Vec<OwnedValue>,
}

/// What a projection should read.
struct RefProjection {
    rows: Vec<u32>,
    /// Live rows: `Projection::row_count`.
    live: usize,
    /// In caption order.
    groups: Vec<RefInfo>,
    lines: Vec<RefLine>,
    expanded_lines: Vec<RefLine>,
    totals: Vec<OwnedValue>,
}

/// The keys of a group's rows in the order shown: its subgroups in the
/// group order. Aggregates read them in key order ([`RefGroup::rows`]).
fn shown_order(group: &RefGroup) -> Vec<u32> {
    if group.children.is_empty() {
        group.rows.iter().map(|(row, _)| row.id).collect()
    } else {
        group.children.iter().flat_map(shown_order).collect()
    }
}

fn lay_out(
    view: &GroupView,
    groups: &[RefGroup],
    parent: Option<&GroupPath>,
    out: &mut RefProjection,
    expanded_out: &mut Vec<RefLine>,
    shown: bool,
) {
    for group in groups {
        let depth = group.path.depth();
        let expanded = view.is_expanded(&group.path);
        let footer = view.footer.has_footer(depth);
        out.groups.push(RefInfo {
            path: group.path.clone(),
            parent: parent.cloned(),
            rows: shown_order(group),
            live: group.rows.iter().filter(|(_, live)| *live).count(),
            expanded,
            footer,
            values: group.values.clone(),
        });
        if shown {
            out.lines.push(RefLine::Caption(group.path.clone(), depth));
        }
        expanded_out.push(RefLine::Caption(group.path.clone(), depth));
        if group.children.is_empty() {
            for (row, _) in &group.rows {
                out.rows.push(row.id);
                let line = RefLine::Data(row.id, view.levels() as u16);
                if shown && expanded {
                    out.lines.push(line.clone());
                }
                expanded_out.push(line);
            }
        } else {
            lay_out(
                view,
                &group.children,
                Some(&group.path),
                out,
                expanded_out,
                shown && expanded,
            );
        }
        if shown && expanded && footer {
            out.lines.push(RefLine::Footer(group.path.clone(), depth));
        }
        if footer {
            expanded_out.push(RefLine::Footer(group.path.clone(), depth));
        }
    }
}

/// Filter, sort stably by (level keys, view sort, insertion), split into
/// groups, aggregate in key order, order the groups, lay them out. Fading
/// rows take their places by their last values and sequences, in a group
/// of the last level with a live row; they are counted nowhere.
fn reference_projection<'a>(reference: &'a Reference, view: &GroupView) -> RefProjection {
    let tagged = |rows: &'a HashMap<u32, (TestRow, u64)>, live: bool| {
        rows.values()
            .filter(|(row, _)| view.passes(row))
            .map(move |(row, seq)| (row, *seq, live))
            .collect::<Vec<_>>()
    };
    let level_keys = |row: &TestRow| -> Vec<OwnedValue> {
        view.levels
            .iter()
            .map(|(level, _)| OwnedValue::from_cell(level.value(row)))
            .collect()
    };
    let live = tagged(&reference.rows, true);
    let live_groups: HashSet<Vec<OwnedValue>> =
        live.iter().map(|(row, ..)| level_keys(row)).collect();
    let mut rows: Vec<(&TestRow, u64, bool)> = live;
    rows.extend(
        tagged(&reference.fading, false)
            .into_iter()
            .filter(|(row, ..)| view.levels.is_empty() || live_groups.contains(&level_keys(row))),
    );
    rows.sort_by(|(a, a_seq, _), (b, b_seq, _)| {
        view.levels
            .iter()
            .map(|&(level, direction)| {
                directed(level.value(a).cmp_total(&level.value(b)), direction)
            })
            .chain(view.sort.iter().map(|&(column, direction)| {
                directed(a.value(column).cmp_total(&b.value(column)), direction)
            }))
            .find(|ordering| ordering.is_ne())
            .unwrap_or(Ordering::Equal)
            .then(a_seq.cmp(b_seq))
    });
    let rows: Vec<(TestRow, bool)> = rows
        .into_iter()
        .map(|(row, _, live)| (row.clone(), live))
        .collect();
    let live = live_rows(&rows);
    let mut groups = if view.levels.is_empty() {
        Vec::new()
    } else {
        split(view, &rows, 0, &GroupPath::default())
    };
    let totals = view
        .aggregates
        .iter()
        .enumerate()
        .map(|(place, &(column, function))| {
            let parts: Vec<(Numbers, &OwnedValue)> = groups
                .iter()
                .map(|group| (group.numbers[place], &group.values[place]))
                .collect();
            aggregate(column, function, &live, &parts, !groups.is_empty()).1
        })
        .collect();
    order(view, &mut groups);
    let mut projection = RefProjection {
        rows: if view.levels.is_empty() {
            rows.iter().map(|(row, _)| row.id).collect()
        } else {
            Vec::new()
        },
        live: live.len(),
        groups: Vec::new(),
        lines: Vec::new(),
        expanded_lines: Vec::new(),
        totals,
    };
    let mut expanded_lines = Vec::new();
    lay_out(
        view,
        &groups,
        None,
        &mut projection,
        &mut expanded_lines,
        true,
    );
    projection.expanded_lines = expanded_lines;
    projection
}

// ---------------------------------------------------------------------------
// Checks.

fn engines() -> Vec<(&'static str, Box<dyn ProjectionEngine<TestRow>>)> {
    vec![
        ("Rebuild", Box::new(Rebuild::default())),
        (
            "Rebuild on one thread",
            Box::new(Rebuild::default().with_max_threads(1)),
        ),
    ]
}

/// The key of a projected row, live or fading.
fn key(snapshot: &StoreSnapshot<TestRow>, id: gpui_kit_grid::RowId) -> u32 {
    snapshot
        .row(id)
        .or_else(|| snapshot.fading_row(id).map(|(row, _)| row))
        .expect("projected rows are live or fading")
        .id
}

fn lines_of(
    projection: &Projection,
    snapshot: &StoreSnapshot<TestRow>,
    lines: impl Iterator<Item = DisplayRow>,
) -> Vec<RefLine> {
    let path = |group| {
        projection
            .groups()
            .get(group)
            .expect("a line's group is in the table")
            .path()
            .clone()
    };
    lines
        .map(|line| match line {
            DisplayRow::Data { row, depth } => RefLine::Data(key(snapshot, row), depth),
            DisplayRow::GroupCaption { group, depth } => RefLine::Caption(path(group), depth),
            DisplayRow::GroupFooter { group, depth } => RefLine::Footer(path(group), depth),
            _ => unreachable!("a grouped projection has no other lines"),
        })
        .collect()
}

/// Values equal by `cmp_total` and of the same kind; floats within a
/// relative 1e-12, as another engine may add in another order.
fn close(a: &OwnedValue, b: &OwnedValue) -> bool {
    match (a, b) {
        (OwnedValue::Float(a), OwnedValue::Float(b)) => {
            a.to_bits() == b.to_bits()
                || (a.is_nan() && b.is_nan())
                || (a - b).abs() <= 1e-12 * a.abs().max(b.abs())
        }
        _ => mem::discriminant(a) == mem::discriminant(b) && a == b,
    }
}

fn check_values(
    view: &GroupView,
    actual: gpui_kit_grid::AggregateRow<'_>,
    expected: &[OwnedValue],
    context: &str,
) {
    assert_eq!(
        actual.columns(),
        columns(&view.aggregates.iter().map(|(c, _)| *c).collect::<Vec<_>>())
    );
    for (&(column, function), expected) in view.aggregates.iter().zip(expected) {
        let value = actual.get(column.ix()).expect("an aggregated column");
        assert!(
            close(value, expected),
            "{context}: {function:?} of {column:?} is {value:?}, expected {expected:?}"
        );
    }
}

/// Checks every reader of a grouped projection against the reference.
fn check_projection(
    projection: &Projection,
    snapshot: &StoreSnapshot<TestRow>,
    view: &GroupView,
    expected: &RefProjection,
    context: &str,
) {
    assert_eq!(
        (projection.is_grouped(), projection.is_tree()),
        (!view.levels.is_empty(), false),
        "{context}"
    );
    let rows: Vec<u32> = projection
        .rows()
        .iter()
        .map(|&id| key(snapshot, id))
        .collect();
    assert_eq!(rows, expected.rows, "{context}: rows");
    assert_eq!(projection.row_count(), expected.live, "{context}");
    // A flat view's lines are its rows.
    let expected_lines: Vec<RefLine> = if view.levels.is_empty() {
        expected
            .rows
            .iter()
            .map(|&key| RefLine::Data(key, 0))
            .collect()
    } else {
        expected.lines.clone()
    };
    let lines = lines_of(
        projection,
        snapshot,
        (0..projection.len()).map(|ix| projection.line(ix).unwrap()),
    );
    assert_eq!(lines, expected_lines, "{context}: lines");
    assert_eq!(projection.line(projection.len()), None);
    if !view.levels.is_empty() {
        let expanded = lines_of(projection, snapshot, projection.expanded_lines());
        assert_eq!(
            expanded, expected.expanded_lines,
            "{context}: expanded lines"
        );
    }
    check_values(
        view,
        projection.totals(),
        &expected.totals,
        &format!("{context}, totals"),
    );
    assert_eq!(projection.totals().row_count(), expected.live);

    // Rows: their place in the order, and on screen.
    let mut data_lines = HashMap::new();
    let mut caption_lines = HashMap::new();
    for (ix, line) in expected_lines.iter().enumerate() {
        match line {
            RefLine::Data(key, _) => {
                data_lines.insert(*key, ix);
            }
            RefLine::Caption(path, _) => {
                caption_lines.insert(path.clone(), ix);
            }
            RefLine::Footer(..) => {}
        }
    }
    for (order_ix, &id) in projection.rows().iter().enumerate() {
        assert_eq!(projection.order_ix(id), Some(order_ix), "{context}");
        assert!(projection.contains(id));
        let shown = data_lines.get(&key(snapshot, id)).copied();
        assert_eq!(
            projection.display_ix(id),
            shown,
            "{context}: row {}",
            key(snapshot, id)
        );
        if let Some(ix) = shown {
            assert_eq!(projection.row(ix), Some(id));
        }
    }

    // Groups, in caption order.
    let table = projection.groups();
    assert_eq!(table.len(), expected.groups.len(), "{context}: groups");
    for (group, expected_group) in table.iter().zip(&expected.groups) {
        let context = format!("{context}, group {:?}", expected_group.path);
        assert_eq!(group.path(), &expected_group.path, "{context}");
        let depth = expected_group.path.depth();
        assert_eq!(group.depth(), depth, "{context}");
        assert_eq!(group.level_id(), &view.levels[usize::from(depth)].0.id());
        assert_eq!(Some(group.value()), expected_group.path.value(depth));
        assert_eq!(
            group
                .parent()
                .map(|parent| table.get(parent).unwrap().path().clone()),
            expected_group.parent,
            "{context}"
        );
        let rows: Vec<u32> = group.rows().iter().map(|&id| key(snapshot, id)).collect();
        assert_eq!(rows, expected_group.rows, "{context}");
        assert_eq!(group.row_count(), expected_group.live, "{context}");
        assert!(expected_group.live > 0, "{context}: a group of fading rows");
        assert_eq!(group.is_expanded(), expected_group.expanded, "{context}");
        assert_eq!(group.has_footer(), expected_group.footer, "{context}");
        check_values(view, group.aggregates(), &expected_group.values, &context);
        assert_eq!(group.aggregates().row_count(), expected_group.live);
        let caption = caption_lines.get(&expected_group.path).copied();
        assert_eq!(projection.group_line(group.id()), caption, "{context}");
        assert_eq!(
            table.find(group.path()).map(|found| found.id()),
            Some(group.id())
        );
    }
}

/// Every reader of two projections, aggregates within [`close`].
fn assert_same(a: &Projection, b: &Projection, context: &str) {
    assert_eq!(a.store_version(), b.store_version(), "{context}: version");
    assert_eq!(a.rows(), b.rows(), "{context}: rows");
    assert_eq!(a.row_count(), b.row_count(), "{context}");
    assert_eq!(a.len(), b.len(), "{context}: lines");
    for ix in 0..a.len() {
        assert_eq!(a.line(ix), b.line(ix), "{context}: line {ix}");
    }
    for &id in a.rows() {
        assert_eq!(a.display_ix(id), b.display_ix(id), "{context}");
    }
    let same_values = |x: gpui_kit_grid::AggregateRow<'_>, y: gpui_kit_grid::AggregateRow<'_>| {
        x.columns() == y.columns()
            && x.row_count() == y.row_count()
            && Col::all()
                .iter()
                .all(|column| match (x.get(column.ix()), y.get(column.ix())) {
                    (Some(x), Some(y)) => close(x, y),
                    (x, y) => x.is_none() && y.is_none(),
                })
    };
    assert!(same_values(a.totals(), b.totals()), "{context}: totals");
    assert_eq!(a.groups().len(), b.groups().len(), "{context}: groups");
    for (x, y) in a.groups().iter().zip(b.groups().iter()) {
        assert_eq!(x.path(), y.path(), "{context}");
        assert_eq!(x.parent(), y.parent(), "{context}");
        assert_eq!(x.rows(), y.rows(), "{context}");
        assert_eq!(x.row_count(), y.row_count(), "{context}");
        assert_eq!(x.is_expanded(), y.is_expanded(), "{context}");
        assert_eq!(x.has_footer(), y.has_footer(), "{context}");
        assert_eq!(a.group_line(x.id()), b.group_line(y.id()), "{context}");
        assert!(
            same_values(x.aggregates(), y.aggregates()),
            "{context}: {x:?}"
        );
    }
}

/// Runs `check` for every engine.
fn for_each_engine(check: impl Fn(&dyn ProjectionEngine<TestRow>, &str)) {
    for (name, engine) in engines() {
        check(&*engine, name);
    }
}

const COUNTS: [u32; 7] = [0, 1, 2, 5, 30, 120, 300];

fn check_matches_reference(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    for seed in 0..48 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let count = COUNTS[rng.usize(..COUNTS.len())];
        let (ingest, reference) = random_store(&mut rng, count);
        let snapshot = ingest.snapshot();
        for view_ix in 0..8 {
            let mut view = GroupView::random(&mut rng);
            // Toggle groups the view has.
            let paths: Vec<GroupPath> = reference_projection(&reference, &view)
                .groups
                .into_iter()
                .map(|group| group.path)
                .collect();
            view.randomize_toggles(&mut rng, &paths);
            let context = format!("{name}, seed {seed}, view {view_ix}, {count} rows, {view:?}");
            let projection = engine.rebuild(&snapshot, &view.spec());
            assert_eq!(projection.store_version(), snapshot.version());
            check_projection(
                &projection,
                &snapshot,
                &view,
                &reference_projection(&reference, &view),
                &context,
            );
        }
    }
}

#[test]
fn test_grouped_rebuild_matches_reference() {
    for_each_engine(check_matches_reference);
}

fn check_task_chains(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    for seed in 0..32 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let count = COUNTS[rng.usize(..COUNTS.len())];
        // Removed rows fade in odd seeds: compaction keeps them, as a
        // rebuild does.
        let (mut ingest, mut reference) = random_store_with(&mut rng, count, seed % 2 == 1);
        let snapshot = ingest.snapshot();
        let mut view = GroupView::random(&mut rng);
        let context = format!("{name}, seed {seed}, {count} rows, {view:?}");

        // The expand job lays a projection out as a rebuild for the new
        // expansion would.
        let built = engine.rebuild(&snapshot, &view.spec());
        let paths: Vec<GroupPath> = built
            .groups()
            .iter()
            .map(|group| group.path().clone())
            .collect();
        let mut other = view.clone();
        other.depth = [
            ExpandDepth::All,
            ExpandDepth::Levels(0),
            ExpandDepth::Levels(1),
            ExpandDepth::Captions,
        ][rng.usize(..4)];
        other.randomize_toggles(&mut rng, &paths);
        let expanded = engine.expand(&built, &snapshot, &other.spec());
        assert_same(
            &expanded,
            &engine.rebuild(&snapshot, &other.spec()),
            &format!("{context}: expand"),
        );
        check_projection(
            &expanded,
            &snapshot,
            &other,
            &reference_projection(&reference, &other),
            &context,
        );

        // The aggregate job after changes to aggregated columns alone
        // gives what a rebuild gives.
        let spec = view.spec();
        let changeable: Vec<Col> = MUTABLE
            .into_iter()
            .filter(|column| {
                spec.aggregate_columns().contains(column.ix())
                    && !spec.dependencies().contains(column.ix())
            })
            .collect();
        let keys: Vec<u32> = reference.rows.keys().copied().collect();
        if !changeable.is_empty() && !keys.is_empty() {
            for _ in 0..rng.usize(1..=20) {
                let key = keys[rng.usize(..keys.len())];
                let column = changeable[rng.usize(..changeable.len())];
                let row = with_changed(&mut rng, &reference.rows[&key].0, column);
                reference.upsert(row.clone());
                ingest.apply([Op::Upsert(Arc::new(row))], Instant::now());
            }
        }
        let changed = ingest.snapshot();
        let aggregated = engine.aggregate(&built, &changed, &spec);
        assert_same(
            &aggregated,
            &engine.rebuild(&changed, &spec),
            &format!("{context}: aggregate"),
        );
        check_projection(
            &aggregated,
            &changed,
            &view,
            &reference_projection(&reference, &view),
            &context,
        );

        // Compacting after removals, then aggregating, gives what a rebuild
        // gives when the group order reads no aggregate. The footers here do
        // not depend on counts.
        view.order = None;
        let spec = view.spec();
        let built = engine.rebuild(&changed, &spec);
        for _ in 0..rng.usize(0..=count as usize / 2 + 1) {
            if let Some(&key) = keys.get(rng.usize(..keys.len().max(1))) {
                reference.remove(key);
                ingest.apply([Op::Remove(key)], Instant::now());
            }
        }
        let removed = ingest.snapshot();
        let compacted = engine.compact(&built, &removed);
        assert_eq!(compacted.store_version(), removed.version());
        let refreshed = engine.aggregate(&compacted, &removed, &spec);
        let rebuilt = engine.rebuild(&removed, &spec);
        assert_same(&refreshed, &rebuilt, &format!("{context}: compact"));
        check_projection(
            &refreshed,
            &removed,
            &view,
            &reference_projection(&reference, &view),
            &context,
        );
        // Compaction alone already has the order, groups and lines.
        assert_eq!(compacted.rows(), rebuilt.rows(), "{context}");
        assert_eq!(compacted.len(), rebuilt.len(), "{context}");
        for (a, b) in compacted.groups().iter().zip(rebuilt.groups().iter()) {
            assert_eq!(
                (a.path(), a.rows(), a.row_count()),
                (b.path(), b.rows(), b.row_count())
            );
        }
    }
}

#[test]
fn test_group_task_chains() {
    for_each_engine(check_task_chains);
}

fn check_expanded_lines(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    for seed in 0..32 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let count = COUNTS[rng.usize(..COUNTS.len())];
        let (ingest, _) = random_store(&mut rng, count);
        let snapshot = ingest.snapshot();
        let mut view = GroupView::random(&mut rng);
        let all = engine.rebuild(&snapshot, &view.expanded().spec());
        let paths: Vec<GroupPath> = all
            .groups()
            .iter()
            .map(|group| group.path().clone())
            .collect();
        view.randomize_toggles(&mut rng, &paths);
        let projection = engine.rebuild(&snapshot, &view.spec());
        let shown: Vec<DisplayRow> = (0..all.len()).map(|ix| all.line(ix).unwrap()).collect();
        assert_eq!(
            projection.expanded_lines().collect::<Vec<_>>(),
            shown,
            "{name}, seed {seed}, {view:?}"
        );
    }
}

#[test]
fn test_expanded_lines() {
    for_each_engine(check_expanded_lines);
}

/// A snapshot of `rows`, inserted in order.
fn snapshot_of(rows: impl IntoIterator<Item = TestRow>) -> Arc<StoreSnapshot<TestRow>> {
    let epoch = Instant::now();
    let mut ingest = Ingest::new(HighlightPolicy::default(), epoch);
    ingest.apply(
        rows.into_iter().map(|row| Op::UpsertSilent(Arc::new(row))),
        epoch,
    );
    ingest.snapshot()
}

/// A row with these values and defaults elsewhere.
fn row(id: u32, mixed: u8, qty: i64, name: &str) -> TestRow {
    TestRow {
        id,
        group: i64::from(id % 2),
        price: 0.5,
        name: name.to_owned(),
        flag: None,
        mixed,
        qty,
    }
}

fn check_paths_and_lookup(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    // `Mixed`: Int(1) and Float(1.0) are one group; Float(-0.0) and
    // Float(0.0) two, Int(0) with 0.0; `Empty` a group of its own.
    let snapshot = snapshot_of([
        row(0, 2, 0, ""), // Int(1)
        row(1, 3, 0, ""), // Float(1.0)
        row(2, 7, 0, ""), // Float(-0.0)
        row(3, 9, 0, ""), // Float(0.0)
        row(4, 8, 0, ""), // Int(0)
        row(5, 0, 0, ""), // Empty
        row(6, 2, 0, ""), // Int(1)
    ]);
    let spec = ViewSpec::new().with_group_by([
        GroupBy::column(Col::Mixed),
        GroupBy::key(GroupKey::new(
            "sign",
            columns(&[Col::Qty]),
            |row: &TestRow| CellValue::Int(row.qty.signum()),
        )),
    ]);
    let projection = engine.rebuild(&snapshot, &spec);
    let top: Vec<(OwnedValue, Vec<u32>)> = projection
        .groups()
        .iter()
        .filter(|group| group.depth() == 0)
        .map(|group| {
            let keys = group.rows().iter().map(|&id| key(&snapshot, id)).collect();
            (group.value().clone(), keys)
        })
        .collect();
    assert_eq!(top.len(), 4, "{name}: {top:?}");
    // Ascending by `cmp_total`: `Empty`, -0.0, 0, 1. A group's key is its
    // first row's value.
    assert!(matches!(top[0].0, OwnedValue::Empty));
    assert_eq!(top[0].1, [5]);
    assert!(matches!(top[1].0, OwnedValue::Float(zero) if zero.is_sign_negative()));
    assert_eq!(top[1].1, [2]);
    assert!(matches!(top[2].0, OwnedValue::Float(zero) if zero == 0.0));
    assert_eq!(top[2].1, [3, 4]);
    assert!(matches!(top[3].0, OwnedValue::Int(1)));
    assert_eq!(top[3].1, [0, 1, 6]);

    // A path is the level ids with the values, from the top down; `find`
    // takes any value equal by `cmp_total`.
    let path = GroupPath::new([
        (ColumnId::from("mixed"), OwnedValue::Float(1.0)),
        (ColumnId::from("sign"), OwnedValue::Int(0)),
    ]);
    let group = projection.groups().find(&path).expect("the group of 1");
    assert_eq!(group.path(), &path);
    assert_eq!(group.level_id().as_str(), "sign");
    assert_eq!((group.depth(), group.row_count()), (1, 3));
    let parent = projection.groups().get(group.parent().unwrap()).unwrap();
    assert_eq!(parent.path(), &path.parent().unwrap());
    assert_eq!(parent.level_id().as_str(), "mixed");
    assert!(
        projection
            .groups()
            .find(&GroupPath::new([(
                ColumnId::from("mixed"),
                OwnedValue::Int(5)
            )]))
            .is_none()
    );
    assert!(projection.groups().find(&GroupPath::default()).is_none());
    // A group's rows are a slice of the projection's rows.
    let rows = projection.rows();
    for group in projection.groups().iter() {
        let start = rows
            .iter()
            .position(|id| Some(id) == group.rows().first())
            .unwrap();
        assert_eq!(&rows[start..start + group.rows().len()], group.rows());
    }

    // NaN is a group of its own, after the numbers.
    let nan = |id: u32, price: f64| TestRow {
        price,
        ..row(id, 0, 0, "")
    };
    let snapshot = snapshot_of([
        nan(0, f64::NAN),
        nan(1, 1.0),
        nan(2, f64::NAN),
        nan(3, f64::INFINITY),
    ]);
    let projection = engine.rebuild(
        &snapshot,
        &ViewSpec::new().with_group_by([GroupBy::column(Col::Price)]),
    );
    let values: Vec<OwnedValue> = projection
        .groups()
        .iter()
        .map(|group| group.value().clone())
        .collect();
    assert_eq!(values.len(), 3, "{name}");
    assert!(matches!(values[2], OwnedValue::Float(value) if value.is_nan()));
    assert_eq!(
        projection
            .groups()
            .get(projection.groups().iter().last().unwrap().id())
            .unwrap()
            .row_count(),
        2
    );
}

#[test]
fn test_group_paths_and_lookup() {
    for_each_engine(check_paths_and_lookup);
}

#[test]
fn test_dependencies_with_groups() {
    let sorted = ViewSpec::<TestRow>::new().with_sort([(Col::Price, SortDirection::Ascending)]);
    assert_eq!(sorted.dependencies(), columns(&[Col::Price]));
    let grouped = sorted
        .clone()
        .with_group_by([
            GroupBy::column(Col::Group),
            GroupBy::key(GroupKey::new("letter", columns(&[Col::Name]), first_letter)),
        ])
        .with_aggregates([
            (Col::Qty, Aggregate::Sum),
            (Col::Mixed, Aggregate::Custom(Arc::new(FirstName))),
        ])
        .with_group_order(AggregateOrder::new(Col::Flag, SortDirection::Descending));
    // Level columns, what a key reads, what the group order reads.
    assert_eq!(
        grouped.dependencies(),
        columns(&[Col::Price, Col::Group, Col::Name, Col::Flag])
    );
    // An order by an aggregate reads what that aggregate reads.
    let by_first_name = ViewSpec::<TestRow>::new()
        .with_group_by([GroupBy::column(Col::Group)])
        .with_aggregates([(Col::Mixed, Aggregate::Custom(Arc::new(FirstName)))])
        .with_group_order(AggregateOrder::new(Col::Mixed, SortDirection::Ascending));
    assert_eq!(
        by_first_name.dependencies(),
        columns(&[Col::Group, Col::Mixed, Col::Name])
    );
    // Aggregated columns and what a custom aggregate reads.
    assert_eq!(
        grouped.aggregate_columns(),
        columns(&[Col::Qty, Col::Mixed, Col::Name])
    );
    assert_eq!(grouped.group_order().depends_on(), columns(&[Col::Flag]));
    // The key order reads no column.
    let by_key = grouped.clone().with_group_order(KeyOrder);
    assert!(by_key.group_order().depends_on().is_empty());
    assert_eq!(
        by_key.dependencies(),
        columns(&[Col::Price, Col::Group, Col::Name])
    );
    // Without grouping, neither the levels nor the group order count.
    assert_eq!(
        grouped.with_group_by([]).dependencies(),
        columns(&[Col::Price])
    );
}

fn check_flat_totals(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    for seed in 0..24 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let count = COUNTS[rng.usize(..COUNTS.len())];
        let (mut ingest, mut reference) = random_store(&mut rng, count);
        let snapshot = ingest.snapshot();
        let mut view = GroupView::random(&mut rng);
        view.levels.clear();
        let spec = view.spec();
        let context = format!("{name}, seed {seed}, {view:?}");
        let projection = engine.rebuild(&snapshot, &spec);
        // A flat view: the lines are the rows at depth 0.
        assert!(!projection.is_grouped() && projection.groups().is_empty());
        assert_eq!(projection.len(), projection.rows().len());
        assert_eq!(projection.row_count(), projection.rows().len());
        for (ix, &id) in projection.rows().iter().enumerate() {
            assert_eq!(
                projection.line(ix),
                Some(DisplayRow::Data { row: id, depth: 0 })
            );
        }
        let totals =
            |reference: &Reference, snapshot: &StoreSnapshot<TestRow>, projection: &Projection| {
                let rows: Vec<TestRow> = projection
                    .rows()
                    .iter()
                    .map(|&id| reference.rows[&key(snapshot, id)].0.clone())
                    .collect();
                view.aggregates
                    .iter()
                    .map(|&(column, function)| aggregate(column, function, &rows, &[], false).1)
                    .collect::<Vec<_>>()
            };
        check_values(
            &view,
            projection.totals(),
            &totals(&reference, &snapshot, &projection),
            &context,
        );
        assert_eq!(projection.totals().row_count(), projection.rows().len());

        // The aggregate job recounts the totals of the same order.
        let keys: Vec<u32> = reference.rows.keys().copied().collect();
        for _ in 0..keys.len().min(10) {
            let key = keys[rng.usize(..keys.len())];
            let column = MUTABLE[rng.usize(..MUTABLE.len())];
            let row = with_changed(&mut rng, &reference.rows[&key].0, column);
            reference.upsert(row.clone());
            ingest.apply([Op::Upsert(Arc::new(row))], Instant::now());
        }
        let changed = ingest.snapshot();
        let aggregated = engine.aggregate(&projection, &changed, &spec);
        assert_eq!(aggregated.rows(), projection.rows());
        assert_eq!(aggregated.store_version(), changed.version());
        check_values(
            &view,
            aggregated.totals(),
            &totals(&reference, &changed, &aggregated),
            &context,
        );
    }
}

#[test]
fn test_flat_totals() {
    for_each_engine(check_flat_totals);
}

#[test]
fn test_group_rebuild_allocations() {
    // A rebuild allocates per group and per job, not per row: the keys and
    // the extremes are borrowed from the rows. The counter is per thread,
    // so the rebuild runs on this thread alone (small snapshots do anyway).
    let engine = Rebuild::default().with_max_threads(1);
    let rows = |count: u32| {
        snapshot_of((0..count).map(|id| TestRow {
            name: ["ab", "bB", "яa"][id as usize % 3].to_owned(),
            ..row(id, (id % 10) as u8, i64::from(id % 7), "")
        }))
    };
    let bucket = |buckets: i64| {
        GroupBy::key(GroupKey::new(
            "bucket",
            columns(&[Col::Id]),
            move |row: &TestRow| CellValue::Int(i64::from(row.id) % buckets),
        ))
    };
    let spec = |level: GroupBy<TestRow>| {
        ViewSpec::new()
            .with_sort([(Col::Qty, SortDirection::Descending)])
            .with_group_by([level])
            .with_aggregates([
                (Col::Qty, Aggregate::Sum),
                (Col::Price, Aggregate::Avg),
                (Col::Name, Aggregate::Min),
                (Col::Mixed, Aggregate::Max),
                (Col::Flag, Aggregate::Count),
            ])
            .with_group_footer(EveryGroupFooter)
    };
    let measure = |snapshot: &StoreSnapshot<TestRow>, spec: &ViewSpec<TestRow>| {
        let before = allocations();
        let projection = engine.rebuild(snapshot, spec);
        let allocated = allocations() - before;
        drop(projection);
        allocated
    };
    let (small, large) = (rows(300), rows(3_000));
    let few = measure(&small, &spec(bucket(3)));
    let many = measure(&small, &spec(bucket(30)));
    let few_large = measure(&large, &spec(bucket(3)));
    // Text keys borrowed from the rows, by a `GroupKey`.
    let letters = measure(
        &small,
        &spec(GroupBy::key(GroupKey::new(
            "letter",
            columns(&[Col::Name]),
            first_letter,
        ))),
    );
    assert!(few < 80, "3 groups: {few} allocations");
    // Each group allocates its path and its text extreme, and the vectors
    // of groups grow.
    assert!(many <= few + 27 * 3, "30 groups: {many}, 3 groups: {few}");
    // Ten times the rows add a few vector growths, not allocations per row.
    assert!(
        few_large <= few + 20,
        "3,000 rows: {few_large}, 300 rows: {few}"
    );
    assert!(
        letters <= few + 3 * 3,
        "3 text keys: {letters}, 3 groups: {few}"
    );
}

/// The rows a custom aggregate sees, by key.
struct SeenKeys;

impl AggregateFn<TestRow> for SeenKeys {
    fn aggregate(&self, _: Col, rows: &mut dyn Iterator<Item = &TestRow>) -> OwnedValue {
        let keys: Vec<String> = rows.map(|row| row.id.to_string()).collect();
        OwnedValue::Text(keys.join(",").into())
    }
}

fn check_aggregate_semantics(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    let aggregates = |spec: ViewSpec<TestRow>, snapshot: &StoreSnapshot<TestRow>| {
        let projection = engine.rebuild(snapshot, &spec);
        let of = |row: gpui_kit_grid::AggregateRow<'_>| {
            Col::all()
                .iter()
                .filter_map(|column| row.get(column.ix()).cloned())
                .collect::<Vec<_>>()
        };
        let groups: Vec<Vec<OwnedValue>> = projection
            .groups()
            .iter()
            .map(|group| of(group.aggregates()))
            .collect();
        (groups, of(projection.totals()))
    };
    let by_group = || ViewSpec::new().with_group_by([GroupBy::column(Col::Group)]);
    use OwnedValue::{Empty, Float, Int, Text};

    // Sums: integers exactly, an overflow of `i64` as a float; with a float
    // among them, a float.
    let snapshot = snapshot_of([
        TestRow {
            mixed: 2,
            ..row(0, 0, i64::MAX, "b")
        }, // group 0
        TestRow {
            mixed: 5,
            ..row(2, 0, i64::MAX, "a")
        }, // group 0
        TestRow {
            mixed: 4,
            ..row(1, 0, -1, "")
        }, // group 1
        TestRow {
            mixed: 6,
            ..row(3, 0, 3, "")
        }, // group 1
    ]);
    let (groups, totals) = aggregates(
        by_group().with_aggregates([(Col::Qty, Aggregate::Sum), (Col::Mixed, Aggregate::Sum)]),
        &snapshot,
    );
    // Runs are in schema order: `Mixed`, then `Qty`.
    assert_eq!(
        groups[0],
        [Float(3.5), Float(2.0 * i64::MAX as f64)],
        "{name}"
    );
    // Text is not a number: group 1 sums its `Int(2)` alone.
    assert!(matches!(groups[1][0], Int(2)), "{name}: {groups:?}");
    assert!(matches!(groups[1][1], Int(2)), "{name}: {groups:?}");
    assert!(matches!(totals[0], Float(sum) if sum == 5.5) && matches!(totals[1], Float(_)));

    // Only text, booleans and blanks: no sum, no mean; `Count` skips blanks;
    // text extremes by `cmp_total`.
    let snapshot = snapshot_of([
        TestRow {
            flag: Some(true),
            ..row(0, 6, 0, "b")
        },
        TestRow {
            flag: None,
            ..row(2, 0, 0, "")
        },
        TestRow {
            flag: Some(false),
            ..row(4, 1, 0, "Ab")
        },
    ]);
    let (groups, totals) = aggregates(
        by_group().with_aggregates([
            (Col::Name, Aggregate::Sum),
            (Col::Flag, Aggregate::Avg),
            (Col::Mixed, Aggregate::Count),
            (Col::Qty, Aggregate::Min),
        ]),
        &snapshot,
    );
    assert!(
        matches!(groups[0][0], Empty) && matches!(groups[0][1], Empty),
        "{name}: {groups:?}"
    );
    assert!(
        matches!(groups[0][2], Int(2)),
        "x and true, not the blank: {groups:?}"
    );
    assert!(matches!(totals[2], Int(2)));
    let (groups, _) = aggregates(
        by_group().with_aggregates([(Col::Name, Aggregate::Min), (Col::Mixed, Aggregate::Max)]),
        &snapshot,
    );
    // "" < "Ab" < "b"; blanks are no value: the mixed maximum is the text.
    assert_eq!(groups[0], [Text("".into()), Text("x".into())], "{name}");

    // A group whose values are all blank has no mean, and an empty view
    // totals to blanks and a zero count.
    let snapshot = snapshot_of([TestRow {
        mixed: 0,
        ..row(0, 0, 0, "")
    }]);
    let (groups, _) = aggregates(
        by_group().with_aggregates([(Col::Mixed, Aggregate::Avg)]),
        &snapshot,
    );
    assert!(matches!(groups[0][0], Empty));
    let empty = snapshot_of([]);
    let (groups, totals) = aggregates(
        by_group().with_aggregates([
            (Col::Qty, Aggregate::Sum),
            (Col::Price, Aggregate::Avg),
            (Col::Name, Aggregate::Min),
            (Col::Flag, Aggregate::Count),
        ]),
        &empty,
    );
    assert!(groups.is_empty());
    // Price, name, flag, quantity.
    assert!(
        matches!(totals[..], [Empty, Empty, Int(0), Empty]),
        "{name}: {totals:?}"
    );

    // A custom aggregate without `combine` reads the live rows of an upper
    // group, in key order; one with it combines its subgroups' values.
    let snapshot = snapshot_of((0..6).map(|id| TestRow {
        group: i64::from(id / 3),
        ..row(id, 0, i64::from(id % 2), "")
    }));
    let spec = ViewSpec::new()
        .with_group_by([
            GroupBy::column(Col::Group),
            GroupBy::column(Col::Qty).with_direction(SortDirection::Descending),
        ])
        .with_aggregates([
            (Col::Name, Aggregate::Custom(Arc::new(SeenKeys))),
            (Col::Flag, Aggregate::Custom(Arc::new(RowsSeen))),
        ]);
    let projection = engine.rebuild(&snapshot, &spec);
    let seen: Vec<(String, OwnedValue, OwnedValue)> = projection
        .groups()
        .iter()
        .map(|group| {
            let aggregates = group.aggregates();
            (
                format!(
                    "{:?}",
                    group
                        .path()
                        .levels()
                        .iter()
                        .map(|(_, value)| value)
                        .collect::<Vec<_>>()
                ),
                aggregates.get(Col::Name.ix()).unwrap().clone(),
                aggregates.get(Col::Flag.ix()).unwrap().clone(),
            )
        })
        .collect();
    let expected = [
        ("[Int(0)]", "1,0,2", 3),
        ("[Int(0), Int(1)]", "1", 1),
        ("[Int(0), Int(0)]", "0,2", 2),
        ("[Int(1)]", "3,5,4", 3),
        ("[Int(1), Int(1)]", "3,5", 2),
        ("[Int(1), Int(0)]", "4", 1),
    ];
    assert_eq!(seen.len(), expected.len(), "{name}: {seen:?}");
    for ((path, keys, count), (expected_path, expected_keys, expected_count)) in
        seen.iter().zip(expected)
    {
        assert_eq!(path, expected_path, "{name}");
        assert_eq!(keys, &Text(expected_keys.into()), "{name}: {path}");
        assert_eq!(count, &Int(expected_count), "{name}: {path}");
    }
    let totals = projection.totals();
    assert_eq!(
        totals.get(Col::Name.ix()),
        Some(&Text("1,0,2,3,5,4".into()))
    );
    assert_eq!(totals.get(Col::Flag.ix()), Some(&Int(6)));
}

#[test]
fn test_aggregate_semantics() {
    for_each_engine(check_aggregate_semantics);
}

// ---------------------------------------------------------------------------
// Fading rows.

/// The slots of the live rows of `projection`.
fn live_slots(projection: &Projection, snapshot: &StoreSnapshot<TestRow>) -> HashSet<usize> {
    projection
        .rows()
        .iter()
        .filter(|&&id| snapshot.row(id).is_some())
        .map(|id| id.ix())
        .collect()
}

fn check_fading_reference(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    // What the random cases covered: fading rows shown, filtered out, left
    // out for a group without live rows, and sharing a slot with a live row.
    let (mut shown, mut filtered, mut dropped, mut shared) = (0, 0, 0, 0);
    for seed in 0..48 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let count = COUNTS[rng.usize(..COUNTS.len())];
        let (ingest, reference) = random_store_with(&mut rng, count, true);
        let snapshot = ingest.snapshot();
        for view_ix in 0..6 {
            let mut view = GroupView::random(&mut rng);
            if view_ix % 3 == 0 {
                view.levels.clear();
            }
            let paths: Vec<GroupPath> = reference_projection(&reference, &view)
                .groups
                .into_iter()
                .map(|group| group.path)
                .collect();
            view.randomize_toggles(&mut rng, &paths);
            let context = format!("{name}, seed {seed}, view {view_ix}, {count} rows, {view:?}");
            let expected = reference_projection(&reference, &view);
            let projection = engine.rebuild(&snapshot, &view.spec());
            check_projection(&projection, &snapshot, &view, &expected, &context);

            let fading_shown = projection.rows().len() - projection.row_count();
            let passing = reference
                .fading
                .values()
                .filter(|(row, _)| view.passes(row))
                .count();
            shown += usize::from(fading_shown > 0);
            filtered += usize::from(passing < reference.fading.len());
            dropped += usize::from(fading_shown < passing);
            let slots = live_slots(&projection, &snapshot);
            shared += usize::from(
                projection
                    .rows()
                    .iter()
                    .any(|&id| snapshot.row(id).is_none() && slots.contains(&id.ix())),
            );
        }
    }
    assert!(
        shown > 100 && filtered > 30 && dropped > 10 && shared > 50,
        "{name}: shown {shown}, filtered {filtered}, dropped {dropped}, shared slots {shared}"
    );
}

#[test]
fn test_fading_rows_keep_their_place() {
    // Fading rows take their places by their last values and sequences, in
    // flat and grouped views; the filters choose them as live rows; they
    // are in `rows()` and the lines, not in the counts and aggregates.
    for_each_engine(check_fading_reference);
}

/// A row of group `group` with a quantity of 1.
fn counted_row(id: u32, group: i64, name: &str) -> TestRow {
    TestRow {
        group,
        qty: 1,
        ..row(id, 0, 1, name)
    }
}

/// The id of the fading row of `key` in `snapshot`.
fn fading_id(snapshot: &StoreSnapshot<TestRow>, key: u32) -> RowId {
    snapshot
        .fading_rows()
        .find(|(_, row, ..)| row.id == key)
        .map(|(id, ..)| id)
        .expect("a fading row")
}

fn check_fading_in_groups(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    // 30 rows in 3 groups by `Group`, a quantity of 1 each.
    let epoch = Instant::now();
    let mut ingest = Ingest::new(policy(true), epoch);
    let name_of = |id: u32| if id == 3 { "x" } else { "" };
    ingest.apply(
        (0..30)
            .map(|id| Op::UpsertSilent(Arc::new(counted_row(id, i64::from(id % 3), name_of(id))))),
        epoch,
    );
    let spec = ViewSpec::new()
        .with_group_by([GroupBy::column(Col::Group)])
        .with_aggregates([(Col::Qty, Aggregate::Sum)]);
    let path = |group: i64| GroupPath::new([(Col::Group.column_id(), OwnedValue::Int(group))]);

    // Row 3 fades in its place in group 0, counted nowhere.
    ingest.apply([Op::Remove(3)], epoch);
    let snapshot = ingest.snapshot();
    let three = fading_id(&snapshot, 3);
    let projection = engine.rebuild(&snapshot, &spec);
    assert_eq!(
        (projection.rows().len(), projection.row_count()),
        (30, 29),
        "{name}"
    );
    assert_eq!(
        projection.totals().get(Col::Qty.ix()),
        Some(&OwnedValue::Int(29))
    );
    assert_eq!(projection.totals().row_count(), 29);
    let zero = projection.groups().find(&path(0)).expect("group 0");
    let keys: Vec<u32> = zero.rows().iter().map(|&id| key(&snapshot, id)).collect();
    assert_eq!(keys, (0..30).step_by(3).collect::<Vec<_>>(), "{name}");
    assert_eq!(zero.row_count(), 9);
    assert_eq!(
        zero.aggregates().get(Col::Qty.ix()),
        Some(&OwnedValue::Int(9))
    );
    assert_eq!(projection.order_ix(three), Some(1));
    // The caption, row 0, then row 3.
    assert_eq!(projection.display_ix(three), Some(2));
    assert_eq!(
        projection.line(2),
        Some(DisplayRow::Data {
            row: three,
            depth: 1
        })
    );

    // Group 2 fades whole: fading rows make no group, so its rows are
    // nowhere.
    ingest.apply((0..30).filter(|id| id % 3 == 2).map(Op::Remove), epoch);
    let snapshot = ingest.snapshot();
    let projection = engine.rebuild(&snapshot, &spec);
    assert_eq!(projection.groups().len(), 2, "{name}");
    assert!(projection.groups().find(&path(2)).is_none());
    assert_eq!((projection.rows().len(), projection.row_count()), (20, 19));
    assert!(!projection.contains(fading_id(&snapshot, 29)));

    // A new row of group 2 takes the slot of a fading one: the group is
    // back with its fading rows, one live row, and both rows of the slot
    // have their places.
    ingest.apply([Op::UpsertSilent(Arc::new(counted_row(32, 2, "")))], epoch);
    let snapshot = ingest.snapshot();
    let new = snapshot.row_id(&32).unwrap();
    let old = snapshot
        .fading_rows()
        .find(|(id, ..)| id.ix() == new.ix())
        .map(|(id, ..)| id)
        .expect("the new row reuses a fading row's slot");
    let projection = engine.rebuild(&snapshot, &spec);
    let two = projection.groups().find(&path(2)).expect("group 2");
    assert_eq!((two.rows().len(), two.row_count()), (11, 1), "{name}");
    assert_eq!(
        two.aggregates().get(Col::Qty.ix()),
        Some(&OwnedValue::Int(1))
    );
    assert_eq!(projection.row_count(), 20);
    for id in [new, old] {
        let order_ix = projection.order_ix(id).expect("in the order");
        assert_eq!(projection.rows()[order_ix], id);
        let display_ix = projection.display_ix(id).expect("shown");
        assert_eq!(projection.row(display_ix), Some(id));
    }
    // Flat too.
    let flat = engine.rebuild(&snapshot, &ViewSpec::new());
    for id in [new, old] {
        assert_eq!(flat.rows()[flat.order_ix(id).unwrap()], id);
        assert_eq!(flat.row(flat.display_ix(id).unwrap()), Some(id));
    }
    assert_eq!((flat.rows().len(), flat.row_count()), (31, 20));

    // The filters choose fading rows by their last versions: row 3 alone.
    let mut name_column = ColumnSet::EMPTY;
    name_column.insert(Col::Name.ix());
    let filtered = engine.rebuild(
        &snapshot,
        &ViewSpec::new().with_quick_filter(QuickFilter::new("x", name_column, &ContainsAllTerms)),
    );
    assert_eq!(filtered.rows(), [three], "{name}");
    assert_eq!(filtered.row_count(), 0);
    // Grouped, it has no live row in its group: nothing.
    let grouped = engine.rebuild(
        &snapshot,
        &spec
            .clone()
            .with_quick_filter(QuickFilter::new("x", name_column, &ContainsAllTerms)),
    );
    assert!(grouped.rows().is_empty() && grouped.groups().is_empty());
}

#[test]
fn test_fading_rows_in_groups() {
    for_each_engine(check_fading_in_groups);
}

fn check_compact_keeps_fading(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    let epoch = Instant::now();
    let mut ingest = Ingest::new(policy(true), epoch);
    ingest.apply(
        (0..12).map(|id| Op::UpsertSilent(Arc::new(counted_row(id, i64::from(id % 3), "")))),
        epoch,
    );
    let grouped = ViewSpec::new()
        .with_group_by([GroupBy::column(Col::Group)])
        .with_aggregates([(Col::Qty, Aggregate::Sum)]);
    let flat = ViewSpec::new().with_aggregates([(Col::Qty, Aggregate::Sum)]);
    let snapshot = ingest.snapshot();
    let built = [
        engine.rebuild(&snapshot, &grouped),
        engine.rebuild(&snapshot, &flat),
    ];

    // Row 4 fades: compaction keeps it in its place, not counted.
    let four = snapshot.row_id(&4).unwrap();
    ingest.apply([Op::Remove(4)], epoch);
    let removed = ingest.snapshot();
    for (projection, spec) in built.iter().zip([&grouped, &flat]) {
        let compacted = engine.compact(projection, &removed);
        assert_eq!(compacted.rows(), projection.rows(), "{name}");
        assert_eq!(compacted.order_ix(four), projection.order_ix(four));
        assert_eq!(compacted.display_ix(four), projection.display_ix(four));
        assert_eq!(compacted.row_count(), 11);
        assert_same(
            &engine.aggregate(&compacted, &removed, spec),
            &engine.rebuild(&removed, spec),
            name,
        );
    }
    let compacted = engine.compact(&built[0], &removed);
    let one = compacted
        .groups()
        .iter()
        .find(|group| group.rows().contains(&four));
    assert_eq!(
        one.map(|group| (group.rows().len(), group.row_count())),
        Some((4, 3))
    );

    // Group 2 fades whole: compaction drops it with its fading rows, as a
    // rebuild makes no group of them.
    ingest.apply([2, 5, 8, 11].map(Op::Remove), epoch);
    let emptied = ingest.snapshot();
    let compacted = engine.compact(&built[0], &emptied);
    assert_eq!(compacted.groups().len(), 2, "{name}");
    assert_eq!((compacted.rows().len(), compacted.row_count()), (8, 7));
    assert_same(
        &engine.aggregate(&compacted, &emptied, &grouped),
        &engine.rebuild(&emptied, &grouped),
        name,
    );
    let flat_compacted = engine.compact(&built[1], &emptied);
    assert_eq!(
        (flat_compacted.rows().len(), flat_compacted.row_count()),
        (12, 7)
    );

    // Past the fade the store drops them, and so does compaction.
    let changes = ingest.apply([], epoch + FADE + Duration::from_secs(1));
    assert_eq!(changes.faded().len(), 5, "{name}");
    let faded = ingest.snapshot();
    for (projection, spec) in [(&compacted, &grouped), (&flat_compacted, &flat)] {
        let compacted = engine.compact(projection, &faded);
        assert_eq!(compacted.rows().len(), compacted.row_count(), "{name}");
        assert_eq!(compacted.row_count(), 7);
        for id in changes.faded() {
            assert!(!compacted.contains(*id));
        }
        assert_same(
            &engine.aggregate(&compacted, &faded, spec),
            &engine.rebuild(&faded, spec),
            name,
        );
    }
}

#[test]
fn test_compact_keeps_fading() {
    for_each_engine(check_compact_keeps_fading);
}

/// Fading rows in a rebuild (the background part of S21):
/// 100k rows, 1,000 of them removed and fading while 1,000 new rows take
/// their slots, 10k updates a second to a float sort column between
/// rebuilds at 10 Hz; flat and grouped by one column into 10 groups, with
/// `Sum` and `Avg`. Prints p50, p99 and max against the same views without
/// fading; run with `cargo test --release -p gpui-kit-grid --features
/// test-support --test engine_groups -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
#[cfg(not(target_family = "wasm"))]
fn test_fading_rebuild_timing() {
    const ROWS: u32 = 100_000;
    const FADING: u32 = 1_000;
    let random = |rng: &mut fastrand::Rng, id: u32| TestRow {
        id,
        group: rng.i64(0..10),
        price: rng.f64() * 100.0,
        name: String::new(),
        flag: None,
        mixed: 0,
        qty: rng.i64(0..1_000),
    };
    let flat = ViewSpec::new()
        .with_sort([(Col::Price, SortDirection::Descending)])
        .with_aggregates([(Col::Qty, Aggregate::Sum), (Col::Price, Aggregate::Avg)]);
    let grouped = flat.clone().with_group_by([GroupBy::column(Col::Group)]);
    let percentiles = |samples: &mut Vec<Duration>| {
        samples.sort();
        let at = |fraction: f64| {
            let ix = ((samples.len() - 1) as f64 * fraction).round() as usize;
            samples[ix].as_secs_f64() * 1e3
        };
        format!("{:.1} ms | {:.1} ms | {:.1} ms", at(0.5), at(0.99), at(1.0))
    };
    println!("| Rebuild, {ROWS} rows | Fading | p50 | p99 | max |");
    println!("|---|---|---|---|---|");
    for (name, spec) in [("flat", &flat), ("grouped by 1 column", &grouped)] {
        // The same rows and updates without and with a removal fade, the
        // rebuilds interleaved, so both see the same machine.
        let mut rng = fastrand::Rng::with_seed(21);
        let epoch = Instant::now();
        let mut rows: Vec<TestRow> = (0..ROWS + FADING).map(|id| random(&mut rng, id)).collect();
        let mut stores = [false, true].map(|fade| {
            let mut ingest = Ingest::new(policy(fade), epoch);
            ingest.apply(
                rows[..ROWS as usize]
                    .iter()
                    .cloned()
                    .map(|row| Op::UpsertSilent(Arc::new(row))),
                epoch,
            );
            // The new rows take the slots of the removed ones.
            ingest.apply((0..FADING).map(|ix| Op::Remove(ix * 97)), epoch);
            ingest.apply(
                rows[ROWS as usize..]
                    .iter()
                    .cloned()
                    .map(|row| Op::UpsertSilent(Arc::new(row))),
                epoch,
            );
            ingest
        });
        let engine = Rebuild::default();
        let mut samples = [Vec::new(), Vec::new()];
        let mut fading = [0, 0];
        for iteration in 0..80 {
            let updates: Vec<Arc<TestRow>> = (0..1_000)
                .map(|_| {
                    let mut ix = rng.usize(..rows.len());
                    if ix % 97 == 0 && ix < (FADING * 97) as usize {
                        ix += 1;
                    }
                    rows[ix].price = rng.f64() * 100.0;
                    Arc::new(rows[ix].clone())
                })
                .collect();
            for store in &mut stores {
                store.apply(updates.iter().cloned().map(Op::Upsert), epoch);
            }
            for turn in 0..2 {
                let ix = (turn + iteration) % 2;
                let snapshot = stores[ix].snapshot();
                let started = Instant::now();
                let projection = engine.rebuild(&snapshot, spec);
                samples[ix].push(started.elapsed());
                fading[ix] = projection.rows().len() - projection.row_count();
            }
        }
        assert_eq!(fading, [0, FADING as usize]);
        for (fading, samples) in fading.iter().zip(&mut samples) {
            println!("| {name} | {fading} | {} |", percentiles(samples));
        }
    }
}

// ---------------------------------------------------------------------------
// Change stamps of aggregates.

#[test]
fn test_summary_stamps() {
    let policy = HighlightPolicy::default();
    let epoch = Instant::now();
    let mut ingest = Ingest::new(policy, epoch);
    // 20 rows in 4 groups: quantity the key, name "b" but row 4's "a".
    let make = |id: u32, group: i64, qty: i64| TestRow {
        group,
        qty,
        ..row(id, 0, qty, if id == 4 { "a" } else { "b" })
    };
    ingest.apply(
        (0..20).map(|id| Op::UpsertSilent(Arc::new(make(id, i64::from(id % 4), i64::from(id))))),
        epoch,
    );
    let spec = ViewSpec::new()
        .with_group_by([GroupBy::column(Col::Group)])
        .with_aggregates([(Col::Qty, Aggregate::Sum), (Col::Name, Aggregate::Min)]);
    let engine = Rebuild::default();
    let path = |group: i64| GroupPath::new([(Col::Group.column_id(), OwnedValue::Int(group))]);
    let (qty, name) = (Col::Qty.ix().get(), Col::Name.ix().get());
    let unstamped = |projection: &Projection| {
        projection.totals_stamps().is_none()
            && projection.groups().iter().all(|group| {
                projection.group_stamps(group.id()).is_none()
                    && projection.caption_stamp(group.id()).is_none()
            })
    };

    // A projection on its own has no stamps.
    let first = engine.rebuild(&ingest.snapshot(), &spec);
    assert!(unstamped(&first));

    // Row 5 of group 1 grows: the group's sum and the totals go up; its
    // caption keeps its count.
    ingest.apply([Op::Upsert(Arc::new(make(5, 1, 100)))], epoch);
    let mut second = engine.rebuild(&ingest.snapshot(), &spec);
    second.carry_summary_stamps(&first, 1_000, policy);
    let group = |projection: &Projection, group: i64| {
        projection
            .groups()
            .find(&path(group))
            .expect("the group")
            .id()
    };
    let one = second.group_stamps(group(&second, 1)).expect("stamped");
    // A stamp per column up to the last aggregated one.
    assert_eq!(one.len(), qty + 1);
    assert_eq!(
        (one[qty].ms(), one[qty].change(), one[qty].direction()),
        (1_000, RowChange::Updated, Some(ChangeDirection::Up))
    );
    assert!(!one[name].is_set() && !one[Col::Id.ix().get()].is_set());
    assert_eq!(second.caption_stamp(group(&second, 1)), None);
    for other in [0, 2, 3] {
        assert_eq!(second.group_stamps(group(&second, other)), None);
    }
    let totals = second.totals_stamps().expect("totals stamped");
    assert_eq!(totals[qty].direction(), Some(ChangeDirection::Up));

    // A new group flashes its caption; the stamp of group 1, still hot,
    // carries over.
    ingest.apply([Op::Upsert(Arc::new(make(20, 9, 1)))], epoch);
    let mut third = engine.rebuild(&ingest.snapshot(), &spec);
    third.carry_summary_stamps(&second, 2_000, policy);
    let nine = group(&third, 9);
    let caption = third.caption_stamp(nine).expect("a new group");
    assert_eq!(
        (caption.ms(), caption.change()),
        (2_000, RowChange::Inserted)
    );
    assert_eq!(third.group_stamps(nine), None);
    assert_eq!(third.group_stamps(group(&third, 1)).unwrap()[qty], one[qty]);
    assert_eq!(third.totals_stamps().unwrap()[qty].ms(), 2_000);

    // Row 4 of group 0 goes: the caption's count changed, without a
    // direction; the sum falls; the minimum name, text, changed without a
    // direction.
    ingest.apply([Op::Remove(4)], epoch);
    let mut fourth = engine.rebuild(&ingest.snapshot(), &spec);
    fourth.carry_summary_stamps(&third, 3_000, policy);
    let zero = group(&fourth, 0);
    let caption = fourth.caption_stamp(zero).expect("the count changed");
    assert_eq!(
        (caption.change(), caption.direction()),
        (RowChange::Updated, None)
    );
    let stamps = fourth.group_stamps(zero).unwrap();
    assert_eq!(stamps[qty].direction(), Some(ChangeDirection::Down));
    assert_eq!(
        (stamps[name].change(), stamps[name].direction()),
        (RowChange::Updated, None)
    );
    assert!(
        fourth.caption_stamp(group(&fourth, 9)).is_some(),
        "still hot"
    );

    // Within the highlight the stamps carry, past it they do not: with no
    // change, nothing is left.
    let snapshot = ingest.snapshot();
    let mut fifth = engine.rebuild(&snapshot, &spec);
    fifth.carry_summary_stamps(&fourth, 6_500, policy);
    assert_eq!(fifth.caption_stamp(group(&fifth, 0)), Some(caption));
    assert_eq!(fifth.group_stamps(group(&fifth, 1)), None, "expired at 6 s");
    let mut sixth = engine.rebuild(&snapshot, &spec);
    sixth.carry_summary_stamps(&fifth, 9_000, policy);
    assert!(unstamped(&sixth));

    // A change of grouping, or of grouped against flat, stamps nothing.
    for other in [
        spec.clone()
            .with_group_by([GroupBy::column(Col::Group), GroupBy::column(Col::Name)]),
        spec.clone().with_group_by([GroupBy::column(Col::Flag)]),
        spec.clone().with_group_by([]),
        spec.clone().with_aggregates([(Col::Qty, Aggregate::Max)]),
    ] {
        let mut projection = engine.rebuild(&snapshot, &other);
        projection.carry_summary_stamps(&fourth, 3_500, policy);
        assert!(unstamped(&projection));
    }

    // Flat totals, and the derived jobs, compare the same way.
    let flat = spec.clone().with_group_by([]);
    let before = engine.rebuild(&snapshot, &flat);
    ingest.apply([Op::Upsert(Arc::new(make(6, 2, 0)))], epoch);
    let snapshot = ingest.snapshot();
    let mut after = engine.aggregate(&before, &snapshot, &flat);
    after.carry_summary_stamps(&before, 4_000, policy);
    assert_eq!(
        after.totals_stamps().unwrap()[qty].direction(),
        Some(ChangeDirection::Down)
    );
    let grouped_before = engine.rebuild(&ingest.snapshot(), &spec);
    ingest.apply([Op::Remove(9)], epoch);
    let removed = ingest.snapshot();
    let mut compacted = engine.compact(&grouped_before, &removed);
    compacted = engine.aggregate(&compacted, &removed, &spec);
    compacted.carry_summary_stamps(&grouped_before, 4_500, policy);
    let one = group(&compacted, 1);
    assert_eq!(
        compacted.caption_stamp(one).map(Stamp::change),
        Some(RowChange::Updated)
    );
    assert_eq!(
        compacted.group_stamps(one).unwrap()[qty].direction(),
        Some(ChangeDirection::Down)
    );
}

// ---------------------------------------------------------------------------
// `Decimal` aggregates.

#[cfg(feature = "decimal")]
mod decimal {
    use rust_decimal::Decimal;

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum MoneyColumn {
        Group,
        Amount,
        Mixed,
    }

    impl GridColumns for MoneyColumn {
        fn all() -> &'static [Self] {
            &[Self::Group, Self::Amount, Self::Mixed]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Group => "group",
                Self::Amount => "amount",
                Self::Mixed => "mixed",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Number {
        Decimal(Decimal),
        Int(i64),
        Float(f64),
    }

    #[derive(Clone, Debug)]
    struct Money {
        id: u32,
        group: i64,
        amount: Decimal,
        mixed: Number,
    }

    impl GridRow for Money {
        type Key = u32;
        type Column = MoneyColumn;

        fn key(&self) -> &u32 {
            &self.id
        }

        fn value(&self, column: MoneyColumn) -> CellValue<'_> {
            match column {
                MoneyColumn::Group => CellValue::Int(self.group),
                MoneyColumn::Amount => CellValue::Decimal(self.amount),
                MoneyColumn::Mixed => match self.mixed {
                    Number::Decimal(value) => CellValue::Decimal(value),
                    Number::Int(value) => CellValue::Int(value),
                    Number::Float(value) => CellValue::Float(value),
                },
            }
        }
    }

    fn dec(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    /// The exact decimal of a value, failing on any other kind.
    fn decimal(value: Option<&OwnedValue>) -> Decimal {
        match value {
            Some(OwnedValue::Decimal(value)) => *value,
            other => panic!("not a decimal: {other:?}"),
        }
    }

    fn money(id: u32, group: i64, amount: &str, mixed: Number) -> Op<Money> {
        Op::UpsertSilent(Arc::new(Money {
            id,
            group,
            amount: dec(amount),
            mixed,
        }))
    }

    #[test]
    fn test_decimal_aggregates() {
        // Group 0: ten amounts of 0.10, decimals and integers mixed. Group
        // 1: 1.00, 2.00, 2.00, decimals and floats mixed. Group 2: sums too
        // large for a `Decimal`.
        let epoch = Instant::now();
        let mut ingest = Ingest::<Money>::new(HighlightPolicy::default(), epoch);
        let mut ops: Vec<Op<Money>> = (0..10)
            .map(|id| {
                let mixed = if id % 2 == 0 {
                    Number::Decimal(dec("0.25"))
                } else {
                    Number::Int(1)
                };
                money(id, 0, "0.10", mixed)
            })
            .collect();
        ops.extend([
            money(10, 1, "1.00", Number::Decimal(dec("0.1"))),
            money(11, 1, "2.00", Number::Float(0.2)),
            money(12, 1, "2.00", Number::Decimal(dec("0.3"))),
        ]);
        ops.extend((13..15).map(|id| {
            Op::UpsertSilent(Arc::new(Money {
                id,
                group: 2,
                amount: Decimal::MAX,
                mixed: Number::Decimal(Decimal::MAX),
            }))
        }));
        ingest.apply(ops, epoch);
        let snapshot = ingest.snapshot();
        let spec = |aggregate: fn() -> Aggregate<Money>| {
            ViewSpec::new()
                .with_group_by([GroupBy::column(MoneyColumn::Group)])
                .with_aggregates([
                    (MoneyColumn::Amount, aggregate()),
                    (MoneyColumn::Mixed, aggregate()),
                ])
        };
        for (name, engine) in [
            ("Rebuild", Rebuild::default()),
            (
                "Rebuild on one thread",
                Rebuild::default().with_max_threads(1),
            ),
        ] {
            let sums = engine.rebuild(&snapshot, &spec(|| Aggregate::Sum));
            let groups: Vec<GroupInfo<'_>> = sums.groups().iter().collect();
            let get = |group: &GroupInfo<'_>, column: MoneyColumn| {
                group.aggregates().get(column.ix()).cloned()
            };
            // Exact, with the scale of the values: no 0.9999999999999999.
            let ten_cents = decimal(get(&groups[0], MoneyColumn::Amount).as_ref());
            assert_eq!(
                (ten_cents, ten_cents.to_string()),
                (dec("1"), "1.00".to_owned()),
                "{name}"
            );
            // Decimals and integers: a decimal; with a float, a float.
            assert_eq!(
                decimal(get(&groups[0], MoneyColumn::Mixed).as_ref()),
                dec("6.25")
            );
            assert!(
                matches!(get(&groups[1], MoneyColumn::Mixed), Some(OwnedValue::Float(sum)) if (sum - 0.6).abs() < 1e-12),
                "{name}"
            );
            // Too large for a `Decimal`: a float.
            assert!(
                matches!(get(&groups[2], MoneyColumn::Amount), Some(OwnedValue::Float(sum)) if sum > 1.5e29),
                "{name}"
            );
            // The totals merge the groups' exact sums; the overflow floats.
            let amounts = engine.rebuild(
                &snapshot,
                &ViewSpec::new()
                    .with_filter(ColumnSet::first(3), |row: &Money| row.group < 2)
                    .with_aggregates([(MoneyColumn::Amount, Aggregate::Sum)]),
            );
            assert_eq!(
                decimal(amounts.totals().get(MoneyColumn::Amount.ix())),
                dec("6.00")
            );
            assert!(matches!(
                sums.totals().get(MoneyColumn::Amount.ix()),
                Some(OwnedValue::Float(_))
            ));

            // Means: two places more than the largest scale.
            let means = engine.rebuild(&snapshot, &spec(|| Aggregate::Avg));
            let groups: Vec<GroupInfo<'_>> = means.groups().iter().collect();
            assert_eq!(
                decimal(get(&groups[0], MoneyColumn::Amount).as_ref()),
                dec("0.1")
            );
            let third = decimal(get(&groups[1], MoneyColumn::Amount).as_ref());
            assert_eq!(third.to_string(), "1.6667", "{name}");
            assert_eq!(
                decimal(get(&groups[0], MoneyColumn::Mixed).as_ref()),
                dec("0.625")
            );
            assert!(matches!(
                get(&groups[1], MoneyColumn::Mixed),
                Some(OwnedValue::Float(_))
            ));

            // Extremes keep the decimal; the aggregate job recounts exactly.
            let extremes = engine.rebuild(&snapshot, &spec(|| Aggregate::Max));
            let zero = extremes.groups().iter().next().expect("group 0");
            assert_eq!(
                decimal(zero.aggregates().get(MoneyColumn::Amount.ix())),
                dec("0.10")
            );
        }

        let spec = spec(|| Aggregate::Sum);
        let engine = Rebuild::default();
        let built = engine.rebuild(&snapshot, &spec);
        ingest.apply(
            (0..10).map(|id| money(id, 0, "0.30", Number::Int(2))),
            epoch,
        );
        let changed = ingest.snapshot();
        let refreshed = engine.aggregate(&built, &changed, &spec);
        let rebuilt = engine.rebuild(&changed, &spec);
        for (a, b) in refreshed.groups().iter().zip(rebuilt.groups().iter()) {
            for column in MoneyColumn::all() {
                let (a, b) = (
                    a.aggregates().get(column.ix()),
                    b.aggregates().get(column.ix()),
                );
                let same = match (a, b) {
                    (Some(OwnedValue::Decimal(a)), Some(OwnedValue::Decimal(b))) => {
                        a == b && a.scale() == b.scale()
                    }
                    (Some(OwnedValue::Float(a)), Some(OwnedValue::Float(b))) => {
                        a.to_bits() == b.to_bits()
                    }
                    (Some(OwnedValue::Int(a)), Some(OwnedValue::Int(b))) => a == b,
                    (a, b) => a.is_none() && b.is_none(),
                };
                assert!(same, "{a:?} {b:?}");
            }
        }
        // Integers alone add up to an integer again.
        let zero = refreshed.groups().iter().next().unwrap();
        assert_eq!(
            decimal(zero.aggregates().get(MoneyColumn::Amount.ix())),
            dec("3.00")
        );
        assert!(matches!(
            zero.aggregates().get(MoneyColumn::Mixed.ix()),
            Some(OwnedValue::Int(20))
        ));
    }
}

/// A line of a transit network: shown by its name, ordered by its rank
/// (U1 before U10 before U2 by text, by rank U1, U2, U10).
#[derive(Clone, Debug, PartialEq)]
struct Line {
    rank: u8,
    name: &'static str,
}

impl gpui_kit_grid::GridValue for Line {
    fn cmp_value(&self, other: &dyn gpui_kit_grid::GridValue) -> Ordering {
        match other.downcast_ref::<Line>() {
            Some(other) => self.rank.cmp(&other.rank),
            None => std::any::TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        out.push_str(self.name);
    }

    fn to_owned_value(&self) -> OwnedValue {
        OwnedValue::custom(self.clone())
    }
}

static LINES: [Line; 3] = [
    Line {
        rank: 0,
        name: "U1",
    },
    Line {
        rank: 1,
        name: "U2",
    },
    Line {
        rank: 2,
        name: "U10",
    },
];

/// The line of a test row: by its quantity.
fn line_of(row: &TestRow) -> CellValue<'_> {
    CellValue::Custom(&LINES[row.qty.rem_euclid(3) as usize])
}

fn check_custom_group_order(engine: &dyn ProjectionEngine<TestRow>, name: &str) {
    let snapshot = snapshot_of((0..12).map(|id| row(id, 0, i64::from(id) * 7, "")));
    let line = || GroupKey::new("line", columns(&[Col::Qty]), line_of);
    let names = |projection: &Projection| -> Vec<String> {
        projection
            .groups()
            .iter()
            .map(|group| {
                let mut text = String::new();
                if let CellValue::Custom(value) = group.value().as_cell() {
                    value.write_text(&mut text);
                }
                text
            })
            .collect()
    };
    // By the values' own order, whatever the view sorts the rows by; the
    // group's value is the line itself, as `to_owned_value` keeps it.
    let spec = ViewSpec::new()
        .with_group_by([GroupBy::key(line())])
        .with_sort([(Col::Id, SortDirection::Descending)]);
    let projection = engine.rebuild(&snapshot, &spec);
    assert_eq!(names(&projection), ["U1", "U2", "U10"], "{name}");
    let first = projection.groups().iter().next().expect("a group");
    assert!(matches!(first.value(), OwnedValue::Custom(line)
        if line.downcast_ref::<Line>() == Some(&LINES[0])));
    // Descending, the level's direction.
    let spec = ViewSpec::new()
        .with_group_by([GroupBy::key(line()).with_direction(SortDirection::Descending)]);
    assert_eq!(
        names(&engine.rebuild(&snapshot, &spec)),
        ["U10", "U2", "U1"],
        "{name}"
    );
}

#[test]
fn test_custom_group_keys_keep_their_order() {
    for_each_engine(check_custom_group_order);
    // A custom value kept by `to_owned_value` equals, hashes and orders
    // as the value; it serializes as its text.
    let (u1, u2) = (
        OwnedValue::custom(LINES[0].clone()),
        OwnedValue::custom(LINES[1].clone()),
    );
    assert_eq!(u1, OwnedValue::from_cell(CellValue::Custom(&LINES[0])));
    assert_ne!(u1, u2);
    assert!(u1.cmp_total(&u2).is_lt());
    assert!(OwnedValue::Text("U2".into()).cmp_total(&u1).is_lt());
    let mut set = HashSet::new();
    set.insert(u1.clone());
    assert!(set.contains(&OwnedValue::custom(LINES[0].clone())));
    assert!(!set.contains(&u2));
    assert_eq!(serde_json::to_string(&u2).expect("text"), r#""U2""#);
}
