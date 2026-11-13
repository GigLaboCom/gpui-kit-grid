//! Tree engine tests against a naive reference model
//! (`.agents/COMPONENT_TEST_RULES.md`, "Pure layers get plain tests").
//!
//! Every check takes the engine as `&dyn ProjectionEngine`, so an
//! incremental engine passes the same set as [`Rebuild`].

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit_grid::{
    Aggregate, CellValue, ColumnIx, ColumnSet, ContainsAllTerms, DisplayRow, ExpandDepth,
    GridColumns, GridRow, HighlightPolicy, Ingest, Op, OwnedValue, Projection, ProjectionEngine,
    QuickFilter, Rebuild, RowId, SortDirection, StoreSnapshot, TreeFilter, TreeFilterContext,
    TreeFilterRule, TreeSpec, TreeTotals, TreeTotalsContext, TreeTotalsRule, ViewSpec,
};

/// Counts the allocations of each thread, so a test can prove a rebuild
/// does not allocate per row while other tests run in parallel.
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
enum NodeColumn {
    Id,
    Group,
    Price,
    Name,
}

impl GridColumns for NodeColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Group, Self::Price, Self::Name]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Group => "group",
            Self::Price => "price",
            Self::Name => "name",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A tree row: few distinct values per column, so sort keys tie often.
#[derive(Clone, Debug, PartialEq)]
struct Node {
    id: u32,
    parent: Option<u32>,
    group: i64,
    price: f64,
    name: String,
}

impl GridRow for Node {
    type Key = u32;
    type Column = NodeColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: NodeColumn) -> CellValue<'_> {
        match column {
            NodeColumn::Id => CellValue::Int(self.id.into()),
            NodeColumn::Group => CellValue::Int(self.group),
            NodeColumn::Price => CellValue::Float(self.price),
            NodeColumn::Name => CellValue::Text(&self.name),
        }
    }

    fn parent_key(&self) -> Option<&u32> {
        self.parent.as_ref()
    }
}

/// A small alphabet in both cases, ASCII and Cyrillic, so names tie often
/// and quick filter terms often match.
const NAME_CHARS: &[char] = &['a', 'A', 'b', 'я', 'Ж'];

fn node(id: u32, parent: Option<u32>, name: &str) -> Node {
    Node {
        id,
        parent,
        group: 0,
        price: 1.0,
        name: name.to_owned(),
    }
}

/// A row of a random forest of keys below `keys`: a root, a child of an
/// earlier key (a tree), of any key (cycles of any length), of itself, or
/// of a key no row has (an orphan).
fn random_node(rng: &mut fastrand::Rng, id: u32, keys: u32) -> Node {
    let parent = match rng.u8(0..20) {
        0..=2 => None,
        3 => Some(id),
        4 => Some(keys + rng.u32(0..10)),
        5 | 6 => Some(rng.u32(0..keys.max(1))),
        _ => Some(rng.u32(0..id.max(1))),
    };
    Node {
        id,
        parent,
        group: rng.i64(0..4),
        price: f64::from(rng.u8(0..6)) / 2.0,
        name: (0..rng.usize(0..=2))
            .map(|_| NAME_CHARS[rng.usize(..NAME_CHARS.len())])
            .collect(),
    }
}

/// Which built-in or custom tree filter a test view uses.
#[derive(Clone, Copy, Debug)]
enum Rule {
    KeepAncestors,
    HideSubtree,
    /// A passing row shows its whole subtree (AG Grid's tree data filter).
    ShowsSubtree,
}

impl Rule {
    fn tree_filter(self) -> TreeFilter {
        match self {
            Self::KeepAncestors => TreeFilter::KeepAncestors,
            Self::HideSubtree => TreeFilter::HideSubtree,
            Self::ShowsSubtree => TreeFilter::Custom(Arc::new(ShowsSubtree)),
        }
    }
}

struct ShowsSubtree;

impl TreeFilterRule for ShowsSubtree {
    fn is_shown(&self, row: &TreeFilterContext) -> bool {
        row.passes() || row.has_passing_descendant() || row.has_passing_ancestor()
    }
}

/// A tree view as the tests drive it, built both as a [`ViewSpec`] and
/// naively.
#[derive(Clone, Debug)]
struct TreeView {
    sort: Vec<(NodeColumn, SortDirection)>,
    /// Hides this group.
    hidden_group: Option<i64>,
    /// Searched in the name only.
    quick_text: String,
    rule: Rule,
    depth: ExpandDepth,
    toggled: Vec<u32>,
}

impl TreeView {
    fn new() -> Self {
        Self {
            sort: Vec::new(),
            hidden_group: None,
            quick_text: String::new(),
            rule: Rule::KeepAncestors,
            depth: ExpandDepth::All,
            toggled: Vec::new(),
        }
    }

    fn random(rng: &mut fastrand::Rng) -> Self {
        let columns = NodeColumn::all();
        Self {
            sort: (0..rng.usize(0..=2))
                .map(|_| {
                    let direction = if rng.bool() {
                        SortDirection::Ascending
                    } else {
                        SortDirection::Descending
                    };
                    (columns[rng.usize(1..columns.len())], direction)
                })
                .collect(),
            hidden_group: rng.bool().then(|| rng.i64(0..4)),
            quick_text: match rng.u8(0..4) {
                0 => String::new(),
                _ => (0..rng.usize(1..=2))
                    .map(|_| NAME_CHARS[rng.usize(..NAME_CHARS.len())].to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            },
            rule: [Rule::KeepAncestors, Rule::HideSubtree, Rule::ShowsSubtree][rng.usize(..3)],
            depth: [
                ExpandDepth::All,
                ExpandDepth::Captions,
                ExpandDepth::Levels(0),
                ExpandDepth::Levels(1),
                ExpandDepth::Levels(2),
            ][rng.usize(..5)],
            toggled: (0..rng.usize(0..12)).map(|_| rng.u32(0..300)).collect(),
        }
    }

    fn spec(&self) -> ViewSpec<Node> {
        let mut name = ColumnSet::EMPTY;
        name.insert(NodeColumn::Name.ix());
        let spec = ViewSpec::new()
            .with_sort(self.sort.clone())
            .with_quick_filter(QuickFilter::new(&self.quick_text, name, &ContainsAllTerms))
            .with_tree(TreeSpec::new().with_filter(self.rule.tree_filter()))
            .with_tree_expansion(self.depth, self.toggled.iter().copied());
        match self.hidden_group {
            Some(hidden) => {
                let mut group = ColumnSet::EMPTY;
                group.insert(NodeColumn::Group.ix());
                spec.with_filter(group, move |node: &Node| node.group != hidden)
            }
            None => spec,
        }
    }

    fn is_filtered(&self) -> bool {
        self.hidden_group.is_some() || !self.quick_text.trim().is_empty()
    }

    /// The view's filter and quick filter: every term occurs, lowercased,
    /// in the lowercased name.
    fn passes(&self, row: &Node) -> bool {
        let name = row.name.to_lowercase();
        self.hidden_group != Some(row.group)
            && self
                .quick_text
                .to_lowercase()
                .split_whitespace()
                .all(|term| name.contains(term))
    }

    /// Whether a branch at `depth` is expanded, by definition.
    fn is_expanded(&self, key: u32, depth: u16) -> bool {
        let by_default = match self.depth {
            ExpandDepth::Levels(levels) => depth < levels,
            _ => true,
        };
        let toggles = self
            .toggled
            .iter()
            .filter(|&&toggled| toggled == key)
            .count();
        by_default != (toggles % 2 == 1)
    }
}

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

/// The reference: a map from key to (row, insertion sequence).
#[derive(Default)]
struct Reference {
    rows: HashMap<u32, (Node, u64)>,
    /// Removed rows that fade, by key.
    fading: HashMap<u32, (Node, u64)>,
    next_seq: u64,
    /// Removed rows fade.
    fade: bool,
}

/// What a tree view shows, by key.
#[derive(Debug, Default, PartialEq)]
struct Expected {
    /// `Projection::rows`: depth first, every row as if all were expanded.
    order: Vec<u32>,
    /// The shown lines: (key, depth).
    lines: Vec<(u32, u16)>,
    /// The branches: key → (children, descendants, expanded).
    branches: HashMap<u32, (usize, usize, bool)>,
    cycles: usize,
    /// Live rows of `order`: `Projection::row_count`.
    live: usize,
}

impl Reference {
    fn new(fade: bool) -> Self {
        Self {
            fade,
            ..Self::default()
        }
    }

    /// A new row of a fading key takes its place: the fading one goes.
    fn upsert(&mut self, row: Node) {
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

    /// The tree, naively: parents by key, cycles by walking up with a set
    /// of visited keys, the filter rule by its definition, recursion. A
    /// fading row that passes is a leaf under its parent if that is shown,
    /// a root if it has no parent (or names itself), else nowhere.
    fn expected(&self, view: &TreeView) -> Expected {
        let named_parent = |key: u32| {
            let parent = self.rows[&key].0.parent?;
            (parent != key && self.rows.contains_key(&parent)).then_some(parent)
        };
        let in_cycle = |key: u32| {
            let mut seen = HashSet::new();
            let mut current = named_parent(key);
            while let Some(parent) = current {
                if parent == key {
                    return true;
                }
                if !seen.insert(parent) {
                    return false;
                }
                current = named_parent(parent);
            }
            false
        };
        let cyclic: HashSet<u32> = self.rows.keys().copied().filter(|&k| in_cycle(k)).collect();
        let parent = |key: u32| {
            if cyclic.contains(&key) {
                None
            } else {
                named_parent(key)
            }
        };
        let ancestors = |key: u32| {
            let mut ancestors = Vec::new();
            let mut current = parent(key);
            while let Some(above) = current {
                ancestors.push(above);
                current = parent(above);
            }
            ancestors
        };
        let passes = |key: u32| view.passes(&self.rows[&key].0);
        // A row has a passing descendant if it is an ancestor of one.
        let with_passing_descendant: HashSet<u32> = self
            .rows
            .keys()
            .copied()
            .filter(|&key| passes(key))
            .flat_map(ancestors)
            .collect();
        let shown: HashSet<u32> = self
            .rows
            .keys()
            .copied()
            .filter(|&key| {
                if !view.is_filtered() {
                    return true;
                }
                let has_passing_descendant = with_passing_descendant.contains(&key);
                let above = ancestors(key);
                let ancestors_pass = above.iter().all(|&ancestor| passes(ancestor));
                let has_passing_ancestor = above.iter().any(|&ancestor| passes(ancestor));
                match view.rule {
                    Rule::KeepAncestors => passes(key) || has_passing_descendant,
                    Rule::HideSubtree => passes(key) && ancestors_pass,
                    Rule::ShowsSubtree => {
                        passes(key) || has_passing_descendant || has_passing_ancestor
                    }
                }
            })
            .collect();

        // Children of each shown row (and the roots under `None`), sorted.
        let mut children: HashMap<Option<u32>, Vec<u32>> = HashMap::new();
        for &key in &shown {
            let shown_parent = ancestors(key).into_iter().find(|a| shown.contains(a));
            children.entry(shown_parent).or_default().push(key);
        }
        for (&key, (row, _)) in &self.fading {
            if !view.passes(row) {
                continue;
            }
            let parent = match row.parent {
                Some(parent) if parent != key => match shown.contains(&parent) {
                    true => Some(parent),
                    false => continue,
                },
                _ => None,
            };
            children.entry(parent).or_default().push(key);
        }
        let entry = |key: &u32| self.rows.get(key).or_else(|| self.fading.get(key)).unwrap();
        for siblings in children.values_mut() {
            siblings.sort_by(|a, b| {
                let ((a, a_seq), (b, b_seq)) = (entry(a), entry(b));
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
        }

        fn walk(
            key: u32,
            depth: u16,
            visible: bool,
            view: &TreeView,
            children: &HashMap<Option<u32>, Vec<u32>>,
            expected: &mut Expected,
        ) {
            let place = expected.order.len();
            expected.order.push(key);
            if visible {
                expected.lines.push((key, depth));
            }
            let Some(kids) = children.get(&Some(key)) else {
                return;
            };
            let expanded = view.is_expanded(key, depth);
            for &kid in kids {
                walk(
                    kid,
                    depth + 1,
                    visible && expanded,
                    view,
                    children,
                    expected,
                );
            }
            let descendants = expected.order.len() - place - 1;
            expected
                .branches
                .insert(key, (kids.len(), descendants, expanded));
        }
        let mut expected = Expected {
            cycles: cyclic.len(),
            live: shown.len(),
            ..Expected::default()
        };
        for &root in children.get(&None).map(Vec::as_slice).unwrap_or_default() {
            walk(root, 0, true, view, &children, &mut expected);
        }
        expected
    }
}

/// The key of a projected row, live or fading.
fn key_of(snapshot: &StoreSnapshot<Node>, id: RowId) -> u32 {
    snapshot
        .row(id)
        .or_else(|| snapshot.fading_row(id).map(|(row, _)| row))
        .expect("projected rows are live or fading")
        .id
}

/// What a projection shows, by key, with every reader checked against the
/// others.
fn actual(projection: &Projection, snapshot: &StoreSnapshot<Node>) -> Expected {
    let key = |id: RowId| key_of(snapshot, id);
    assert!(projection.is_tree() && !projection.is_grouped());
    assert!(projection.groups().is_empty());
    assert!(projection.totals().is_empty());
    let lines: Vec<(u32, u16)> = (0..projection.len())
        .map(|ix| match projection.line(ix) {
            Some(DisplayRow::Data { row, depth }) => {
                assert_eq!(projection.row(ix), Some(row));
                assert_eq!(projection.display_ix(row), Some(ix));
                (key(row), depth)
            }
            line => panic!("a tree shows data rows only, not {line:?}"),
        })
        .collect();
    assert_eq!(projection.line(projection.len()), None);
    let mut branches = HashMap::new();
    let expanded: Vec<DisplayRow> = projection.expanded_lines().collect();
    assert_eq!(expanded.len(), projection.rows().len());
    for (order_ix, &id) in projection.rows().iter().enumerate() {
        assert_eq!(projection.order_ix(id), Some(order_ix));
        assert!(projection.contains(id));
        assert_eq!(expanded[order_ix].row(), Some(id));
        if let Some(branch) = projection.branch(id) {
            assert!(branch.child_count() > 0);
            branches.insert(
                key(id),
                (
                    branch.child_count(),
                    branch.descendant_count(),
                    branch.is_expanded(),
                ),
            );
        }
    }
    // Rows the tree does not show are nowhere.
    for (id, _) in snapshot.iter() {
        if projection.order_ix(id).is_none() {
            assert!(!projection.contains(id));
            assert_eq!(projection.display_ix(id), None);
            assert_eq!(projection.branch(id), None);
        }
    }
    // Every shown line is an expanded line at the same depth.
    let depths: HashMap<RowId, u16> = expanded
        .iter()
        .map(|line| (line.row().unwrap(), line.depth()))
        .collect();
    for ix in 0..projection.len() {
        let line = projection.line(ix).unwrap();
        assert_eq!(depths.get(&line.row().unwrap()), Some(&line.depth()));
    }
    Expected {
        order: projection.rows().iter().map(|&id| key(id)).collect(),
        lines,
        branches,
        cycles: projection.tree_cycles(),
        live: projection.row_count(),
    }
}

/// The engines every check runs for.
fn engines() -> [(&'static str, Box<dyn ProjectionEngine<Node>>); 2] {
    [
        ("Rebuild", Box::new(Rebuild::default())),
        (
            "Rebuild, one thread",
            Box::new(Rebuild::default().with_max_threads(1)),
        ),
    ]
}

/// Checks `engine` against the reference's `expected` tree for `view`.
fn check_tree_engine(
    engine: &dyn ProjectionEngine<Node>,
    snapshot: &StoreSnapshot<Node>,
    view: &TreeView,
    expected: &Expected,
    context: &str,
) -> Projection {
    let projection = engine.rebuild(snapshot, &view.spec());
    assert_eq!(
        &actual(&projection, snapshot),
        expected,
        "{context}, {view:?}"
    );
    projection
}

/// Writes `ops` to both the store and the reference.
fn write(ingest: &mut Ingest<Node>, reference: &mut Reference, ops: Vec<Op<Node>>) {
    for op in &ops {
        match op {
            Op::UpsertSilent(row) | Op::Upsert(row) => reference.upsert((**row).clone()),
            Op::Remove(key) => reference.remove(*key),
            _ => unreachable!("only upserts and removals"),
        }
    }
    ingest.apply(ops, Instant::now());
}

#[test]
fn test_tree_matches_reference_model() {
    // What the random cases covered, so the generator cannot drift into
    // trivial trees.
    let (mut cycles, mut collapsed, mut kept_for_context, mut deep, mut fading) = (0, 0, 0, 0, 0);
    for seed in 0..64 {
        let mut rng = fastrand::Rng::with_seed(seed);
        // Removed rows fade in odd seeds.
        let fade = seed % 2 == 1;
        let mut ingest = Ingest::<Node>::new(policy(fade), Instant::now());
        let mut reference = Reference::new(fade);
        let keys = [0, 1, 2, 5, 40, 150, 300][rng.usize(..7)];

        // Rows in a random order, so children often come before parents,
        // plus cycles of 2–4 rows.
        let mut ids: Vec<u32> = (0..keys).collect();
        rng.shuffle(&mut ids);
        let mut ops: Vec<Op<Node>> = ids
            .iter()
            .map(|&id| Op::UpsertSilent(Arc::new(random_node(&mut rng, id, keys))))
            .collect();
        if keys >= 4 {
            for _ in 0..rng.usize(0..3) {
                let ring: Vec<u32> = (0..rng.usize(2..=4)).map(|_| rng.u32(0..keys)).collect();
                for (ix, &id) in ring.iter().enumerate() {
                    let mut row = random_node(&mut rng, id, keys);
                    row.parent = Some(ring[(ix + 1) % ring.len()]);
                    ops.push(Op::UpsertSilent(Arc::new(row)));
                }
            }
        }
        write(&mut ingest, &mut reference, ops);

        for phase in 0..3 {
            let snapshot = ingest.snapshot();
            for _ in 0..4 {
                let view = TreeView::random(&mut rng);
                let expected = reference.expected(&view);
                cycles += usize::from(expected.cycles > 0);
                collapsed += usize::from(expected.lines.len() < expected.order.len());
                deep += usize::from(expected.lines.iter().any(|&(_, depth)| depth >= 3));
                kept_for_context += usize::from(
                    view.is_filtered()
                        && expected.order.iter().any(|key| {
                            reference
                                .rows
                                .get(key)
                                .is_some_and(|(row, _)| !view.passes(row))
                        }),
                );
                fading += usize::from(expected.live < expected.order.len());
                for (name, engine) in engines() {
                    let context = format!("seed {seed}, phase {phase}, {keys} keys, {name}");
                    check_tree_engine(engine.as_ref(), &snapshot, &view, &expected, &context);
                }
            }
            // Removals reuse slots; new parents move rows and their subtrees.
            let mut ops: Vec<Op<Node>> = (0..keys / 5)
                .map(|_| Op::Remove(rng.u32(0..keys.max(1))))
                .collect();
            ops.extend((0..keys / 4).map(|_| {
                let id = rng.u32(0..keys.max(1));
                Op::UpsertSilent(Arc::new(random_node(&mut rng, id, keys)))
            }));
            write(&mut ingest, &mut reference, ops);
        }
    }
    assert!(
        cycles > 50 && collapsed > 50 && kept_for_context > 50 && deep > 30 && fading > 30,
        "cycles {cycles}, collapsed {collapsed}, kept for context {kept_for_context}, deep {deep}, fading {fading}"
    );
}

/// The rows of a tree in order, as (key, depth).
fn tree_of(projection: &Projection, snapshot: &StoreSnapshot<Node>) -> Vec<(u32, u16)> {
    let lines = actual(projection, snapshot).lines;
    assert_eq!(lines.len(), projection.rows().len(), "all expanded");
    lines
}

#[test]
fn test_tree_cycles_and_orphans() {
    let spec = ViewSpec::new().with_tree(TreeSpec::new());
    for (_, engine) in engines() {
        let engine = engine.as_ref();
        let rebuild = |ops: Vec<Op<Node>>| {
            let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
            ingest.apply(ops, Instant::now());
            let snapshot = ingest.snapshot();
            let projection = engine.rebuild(&snapshot, &spec);
            (tree_of(&projection, &snapshot), projection.tree_cycles())
        };
        let upsert = |id, parent| Op::UpsertSilent(Arc::new(node(id, parent, "")));

        // A → B → A: both are roots, both counted.
        assert_eq!(
            rebuild(vec![upsert(1, Some(2)), upsert(2, Some(1))]),
            (vec![(1, 0), (2, 0)], 2)
        );
        // A → A is a root, not a cycle.
        assert_eq!(rebuild(vec![upsert(1, Some(1))]), (vec![(1, 0)], 0));
        // A child of a key no row has is a root.
        assert_eq!(
            rebuild(vec![upsert(1, None), upsert(2, Some(99))]),
            (vec![(1, 0), (2, 0)], 0)
        );
        // A cycle with a tail, C → A and A → B → A: A and B are roots, C a
        // child of A.
        assert_eq!(
            rebuild(vec![
                upsert(1, Some(2)),
                upsert(2, Some(1)),
                upsert(3, Some(1)),
            ]),
            (vec![(1, 0), (3, 1), (2, 0)], 2)
        );
        // A longer cycle below a root: its rows become roots, the rest of
        // the tree stays.
        assert_eq!(
            rebuild(vec![
                upsert(1, None),
                upsert(2, Some(1)),
                upsert(3, Some(5)),
                upsert(4, Some(3)),
                upsert(5, Some(4)),
                upsert(6, Some(4)),
            ]),
            (vec![(1, 0), (2, 1), (3, 0), (4, 0), (6, 1), (5, 0)], 3)
        );

        // A child before its parent: a root until the parent comes.
        let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
        ingest.apply([upsert(2, Some(1))], Instant::now());
        let snapshot = ingest.snapshot();
        assert_eq!(
            tree_of(&engine.rebuild(&snapshot, &spec), &snapshot),
            [(2, 0)]
        );
        ingest.apply([upsert(1, None)], Instant::now());
        let snapshot = ingest.snapshot();
        let projection = engine.rebuild(&snapshot, &spec);
        assert_eq!(tree_of(&projection, &snapshot), [(1, 0), (2, 1)]);
        let parent = snapshot.row_id(&1).unwrap();
        assert_eq!(
            projection.branch(parent).map(|branch| (
                branch.child_count(),
                branch.descendant_count(),
                branch.is_expanded()
            )),
            Some((1, 1, true))
        );
        assert_eq!(projection.branch(snapshot.row_id(&2).unwrap()), None);
    }
}

/// The 10-row tree of the filter tests; "m" marks the rows that pass:
///
/// ```text
/// 1 ─┬─ 2 ─┬─ 4
///    │     └─ 5 m
///    └─ 3 m ─┬─ 6 ─── 10 m
///            └─ 7 m
/// 8 m ─── 9
/// ```
fn filter_tree() -> Vec<Op<Node>> {
    [
        (1, None, "fund"),
        (2, Some(1), "desk"),
        (3, Some(1), "desk m"),
        (4, Some(2), "book"),
        (5, Some(2), "pos m"),
        (6, Some(3), "book"),
        (7, Some(3), "pos m"),
        (8, None, "fund m"),
        (9, Some(8), "desk"),
        (10, Some(6), "pos m"),
    ]
    .into_iter()
    .map(|(id, parent, name)| Op::UpsertSilent(Arc::new(node(id, parent, name))))
    .collect()
}

/// Rows that pass alone.
struct PassingOnly;

impl TreeFilterRule for PassingOnly {
    fn is_shown(&self, row: &TreeFilterContext) -> bool {
        row.passes()
    }
}

/// Fails if called: without a filter the rule is not consulted.
struct NeverCalled;

impl TreeFilterRule for NeverCalled {
    fn is_shown(&self, _: &TreeFilterContext) -> bool {
        panic!("the tree filter rule ran without a filter")
    }
}

#[test]
fn test_tree_filter_modes() {
    let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply(filter_tree(), Instant::now());
    let snapshot = ingest.snapshot();
    let mut name = ColumnSet::EMPTY;
    name.insert(NodeColumn::Name.ix());
    let spec = |filter: TreeFilter| {
        ViewSpec::new()
            .with_quick_filter(QuickFilter::new("m", name, &ContainsAllTerms))
            .with_tree(TreeSpec::new().with_filter(filter))
    };
    for (_, engine) in engines() {
        let tree = |spec: ViewSpec<Node>| tree_of(&engine.rebuild(&snapshot, &spec), &snapshot);

        // Matches keep their ancestors (1, 2, 6); rows below a match that
        // do not pass (9) and other rows (4) are hidden.
        assert_eq!(
            tree(spec(TreeFilter::KeepAncestors)),
            [
                (1, 0),
                (2, 1),
                (5, 2),
                (3, 1),
                (6, 2),
                (10, 3),
                (7, 2),
                (8, 0)
            ]
        );
        // A failing row hides its subtree: 1 takes 3, 5, 7 and 10 with it.
        assert_eq!(tree(spec(TreeFilter::HideSubtree)), [(8, 0)]);
        // The application's rule: a passing row shows its whole subtree.
        assert_eq!(
            tree(spec(TreeFilter::Custom(Arc::new(ShowsSubtree)))),
            [
                (1, 0),
                (2, 1),
                (5, 2),
                (3, 1),
                (6, 2),
                (10, 3),
                (7, 2),
                (8, 0),
                (9, 1)
            ]
        );
        // A shown row with a hidden parent hangs from its nearest shown
        // ancestor (10 from 3), or is a root (5).
        assert_eq!(
            tree(spec(TreeFilter::Custom(Arc::new(PassingOnly)))),
            [(3, 0), (7, 1), (10, 1), (5, 0), (8, 0)]
        );
        // Without a filter every row is shown and no rule runs.
        let all = tree(
            ViewSpec::new()
                .with_tree(TreeSpec::new().with_filter(TreeFilter::Custom(Arc::new(NeverCalled)))),
        );
        assert_eq!(all.len(), 10);

        // A row kept for context is an ordinary shown row: a branch.
        let projection = engine.rebuild(&snapshot, &spec(TreeFilter::KeepAncestors));
        let fund = projection.branch(snapshot.row_id(&1).unwrap()).unwrap();
        assert_eq!((fund.child_count(), fund.descendant_count()), (2, 6));
        assert_eq!(projection.row_count(), 8);
    }
}

/// Checks the shape of a fully expanded tree projection: depths step by
/// one, and each branch's subtree is the deeper rows right below it.
/// Returns the depths of `Projection::rows`.
fn check_shape(projection: &Projection, context: &str) -> Vec<u16> {
    let rows = projection.rows();
    assert_eq!(projection.len(), rows.len(), "{context}: all expanded");
    let depths: Vec<u16> = (0..rows.len())
        .map(|ix| projection.line(ix).unwrap().depth())
        .collect();
    for (order_ix, &id) in rows.iter().enumerate() {
        let depth = depths[order_ix];
        let previous = order_ix.checked_sub(1).map(|ix| depths[ix]);
        assert!(
            previous.map_or(depth == 0, |previous| depth <= previous + 1),
            "{context}: depth {depth} at {order_ix}"
        );
        let below = &depths[order_ix + 1..];
        let subtree = below.iter().take_while(|&&below| below > depth).count();
        let children = below[..subtree]
            .iter()
            .filter(|&&below| below == depth + 1)
            .count();
        assert_eq!(
            projection
                .branch(id)
                .map(|branch| (branch.child_count(), branch.descendant_count())),
            (subtree > 0).then_some((children, subtree)),
            "{context}: branch at {order_ix}"
        );
    }
    depths
}

/// Checks that the lines of `projection` are its rows whose ancestors are
/// all expanded, at `depths`.
fn check_lines(projection: &Projection, depths: &[u16], context: &str) {
    let rows = projection.rows();
    let mut expected = Vec::new();
    let mut order_ix = 0;
    while let Some(&id) = rows.get(order_ix) {
        expected.push((id, depths[order_ix]));
        order_ix += 1;
        if let Some(branch) = projection.branch(id).filter(|branch| !branch.is_expanded()) {
            order_ix += branch.descendant_count();
        }
    }
    let lines: Vec<(RowId, u16)> = (0..projection.len())
        .map(|ix| {
            let line = projection.line(ix).unwrap();
            (line.row().unwrap(), line.depth())
        })
        .collect();
    assert_eq!(lines, expected, "{context}: lines");
}

/// The reference of a snapshot's rows and fading rows, with their
/// insertion sequences.
fn reference_of(snapshot: &StoreSnapshot<Node>) -> Reference {
    let mut reference = Reference::default();
    for (id, row) in snapshot.iter() {
        reference
            .rows
            .insert(row.id, ((**row).clone(), snapshot.seq(id)));
    }
    for (_, row, _, seq) in snapshot.fading_rows() {
        reference.fading.insert(row.id, ((**row).clone(), seq));
    }
    reference
}

#[test]
fn test_tree_compact_and_expand_match_rebuild() {
    for seed in 0..48 {
        let mut rng = fastrand::Rng::with_seed(seed);
        // Removed rows fade in odd seeds: compaction keeps them where a
        // rebuild puts them.
        let fade = seed % 2 == 1;
        let keys = [5, 40, 150, 300][rng.usize(..4)];
        let mut ids: Vec<u32> = (0..keys).collect();
        rng.shuffle(&mut ids);
        let rows: Vec<Arc<Node>> = ids
            .iter()
            .map(|&id| Arc::new(random_node(&mut rng, id, keys)))
            .collect();
        // `compact` is exact for the built-in rules; the view rebuilds a
        // tree with a custom rule.
        let mut view = TreeView::random(&mut rng);
        view.rule = [Rule::KeepAncestors, Rule::HideSubtree][rng.usize(..2)];
        let expanded_view = TreeView {
            depth: ExpandDepth::All,
            toggled: Vec::new(),
            ..view.clone()
        };
        let picks = rng.u64(..);

        for (name, engine) in engines() {
            let context = format!("seed {seed}, {keys} keys, {name}, {view:?}");
            let engine = engine.as_ref();
            let mut pick = fastrand::Rng::with_seed(picks);
            let mut ingest = Ingest::<Node>::new(policy(fade), Instant::now());
            ingest.apply(rows.iter().cloned().map(Op::UpsertSilent), Instant::now());
            let snapshot = ingest.snapshot();
            let previous = engine.rebuild(&snapshot, &view.spec());

            // A new expansion: the same as a rebuild with it, both ways.
            let expanded = engine.expand(&previous, &snapshot, &expanded_view.spec());
            assert_eq!(
                actual(&expanded, &snapshot),
                actual(&engine.rebuild(&snapshot, &expanded_view.spec()), &snapshot),
                "{context}: expand"
            );
            assert_eq!(expanded.store_version(), previous.store_version());
            let depths = check_shape(&expanded, &context);
            check_lines(&previous, &depths, &context);
            let back = engine.expand(&expanded, &snapshot, &view.spec());
            assert_eq!(
                actual(&back, &snapshot),
                actual(&previous, &snapshot),
                "{context}: expand back"
            );

            // Removed leaves: the same as a rebuild of the new snapshot. A
            // removed row of a cycle changes other rows' parents, so the
            // view rebuilds a tree with cycles.
            let key = |id: RowId| snapshot.row(id).unwrap().id;
            let leaves: Vec<u32> = previous
                .rows()
                .iter()
                .filter(|&&id| previous.branch(id).is_none() && pick.u8(0..3) == 0)
                .map(|&id| key(id))
                .collect();
            ingest.apply(leaves.iter().map(|&key| Op::Remove(key)), Instant::now());
            let after = ingest.snapshot();
            let compacted = engine.compact(&previous, &after);
            if previous.tree_cycles() == 0 {
                assert_eq!(
                    actual(&compacted, &after),
                    actual(&engine.rebuild(&after, &view.spec()), &after),
                    "{context}: compact after removing leaves {leaves:?}"
                );
            }

            // Removed branches: their children become roots in a consistent
            // tree, laid out by the kept flags; a rebuild matches the
            // reference.
            let key = |id: RowId| key_of(&after, id);
            let branches: Vec<u32> = compacted
                .rows()
                .iter()
                .filter(|&&id| compacted.branch(id).is_some() && pick.u8(0..3) == 0)
                .map(|&id| key(id))
                .collect();
            ingest.apply(branches.iter().map(|&key| Op::Remove(key)), Instant::now());
            let last = ingest.snapshot();
            let fallback = engine.compact(&compacted, &last);
            let context = format!("{context}, removed branches {branches:?}");
            for &id in fallback.rows() {
                assert!(
                    compacted.contains(id)
                        && (last.row(id).is_some() || last.fading_row(id).is_some()),
                    "{context}"
                );
            }
            let all = engine.expand(&fallback, &last, &expanded_view.spec());
            let depths = check_shape(&all, &context);
            check_lines(&fallback, &depths, &context);
            assert_eq!(
                actual(&engine.rebuild(&last, &view.spec()), &last),
                reference_of(&last).expected(&view),
                "{context}"
            );
        }
    }
}

#[test]
fn test_tree_expansion_is_by_key() {
    let view = |toggled: Vec<u32>, direction| TreeView {
        sort: vec![(NodeColumn::Price, direction)],
        toggled,
        ..TreeView::new()
    };
    let priced = |id, parent, price| {
        Op::UpsertSilent(Arc::new(Node {
            price,
            ..node(id, parent, "")
        }))
    };
    for (_, engine) in engines() {
        let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
        ingest.apply(
            [
                priced(1, None, 1.0),
                priced(2, Some(1), 1.0),
                priced(3, Some(1), 2.0),
                priced(5, None, 2.0),
                priced(6, None, 3.0),
            ],
            Instant::now(),
        );
        let lines = |ingest: &Ingest<Node>, view: &TreeView| {
            let snapshot = ingest.snapshot();
            let projection = engine.rebuild(&snapshot, &view.spec());
            let lines: Vec<u32> = actual(&projection, &snapshot)
                .lines
                .into_iter()
                .map(|(key, _)| key)
                .collect();
            let branch = |key| projection.branch(snapshot.row_id(&key)?);
            (lines, branch(1), branch(6), branch(5))
        };

        // 1 collapsed by key; the toggled key of leaf 6 changes nothing.
        let ascending = view(vec![1, 6], SortDirection::Ascending);
        let (shown, one, six, _) = lines(&ingest, &ascending);
        assert_eq!(shown, [1, 5, 6]);
        assert_eq!(
            one.map(|b| (b.child_count(), b.is_expanded())),
            Some((2, false))
        );
        assert_eq!(six, None);

        // A new child does not expand it; neither do a new sort or a move.
        ingest.apply([priced(4, Some(1), 0.0)], Instant::now());
        let (shown, one, ..) = lines(&ingest, &ascending);
        assert_eq!(shown, [1, 5, 6]);
        assert_eq!(
            one.map(|b| (b.child_count(), b.is_expanded())),
            Some((3, false))
        );
        let descending = view(vec![1, 6], SortDirection::Descending);
        assert_eq!(lines(&ingest, &descending).0, [6, 5, 1]);
        ingest.apply([priced(1, None, 10.0)], Instant::now());
        let (shown, one, ..) = lines(&ingest, &ascending);
        assert_eq!(shown, [5, 6, 1]);
        assert_eq!(one.map(|b| b.is_expanded()), Some(false));

        // A leaf that becomes a branch: by its toggle (6, collapsed against
        // `All`) or by the default (5, expanded).
        ingest.apply(
            [priced(7, Some(6), 1.0), priced(8, Some(5), 1.0)],
            Instant::now(),
        );
        let (shown, _, six, five) = lines(&ingest, &ascending);
        assert_eq!(shown, [5, 8, 6, 1]);
        assert_eq!(six.map(|b| b.is_expanded()), Some(false));
        assert_eq!(five.map(|b| b.is_expanded()), Some(true));

        // The default depth: `Levels(1)` expands the roots only, a toggle
        // inverts it at any depth; a collapsed row's branches keep their
        // own flags.
        ingest.apply([priced(9, Some(8), 1.0)], Instant::now());
        let levels = TreeView {
            depth: ExpandDepth::Levels(1),
            ..view(vec![8, 1], SortDirection::Ascending)
        };
        let snapshot = ingest.snapshot();
        let projection = engine.rebuild(&snapshot, &levels.spec());
        // 5 and 6 expanded as roots; 8 toggled open at depth 1, 1 toggled
        // closed at depth 0.
        assert_eq!(
            actual(&projection, &snapshot).lines,
            [(5, 0), (8, 1), (9, 2), (6, 0), (7, 1), (1, 0)]
        );
        let eight = projection.branch(snapshot.row_id(&8).unwrap()).unwrap();
        assert!(eight.is_expanded(), "toggled against Levels(1) at depth 1");
        let one = projection.branch(snapshot.row_id(&1).unwrap()).unwrap();
        assert!(!one.is_expanded(), "toggled against Levels(1) at depth 0");
    }
}

#[test]
fn test_tree_dependencies() {
    // The sort and both filters, as in a flat view: the parent is no
    // column.
    let view = TreeView {
        sort: vec![(NodeColumn::Price, SortDirection::Ascending)],
        hidden_group: Some(1),
        quick_text: "a".to_owned(),
        ..TreeView::new()
    };
    let dependencies = view.spec().dependencies();
    for column in [NodeColumn::Price, NodeColumn::Group, NodeColumn::Name] {
        assert!(dependencies.contains(column.ix()), "{column:?}");
    }
    assert!(!dependencies.contains(NodeColumn::Id.ix()));

    // A new parent with every column the same: the row is published as
    // updated, with no column but a parent change, and a rebuild moves it
    // with its subtree.
    let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
    ingest.apply(
        [
            Op::UpsertSilent(Arc::new(node(1, None, ""))),
            Op::UpsertSilent(Arc::new(node(2, None, ""))),
            Op::UpsertSilent(Arc::new(node(3, Some(1), ""))),
            Op::UpsertSilent(Arc::new(node(4, Some(3), ""))),
        ],
        Instant::now(),
    );
    let spec = TreeView::new().spec();
    let engine = Rebuild::default();
    let snapshot = ingest.snapshot();
    assert_eq!(
        tree_of(&engine.rebuild(&snapshot, &spec), &snapshot),
        [(1, 0), (3, 1), (4, 2), (2, 0)]
    );
    let changes = ingest.apply([Op::Upsert(Arc::new(node(3, Some(2), "")))], Instant::now());
    let snapshot = ingest.snapshot();
    assert_eq!(changes.updated(), [snapshot.row_id(&3).unwrap()]);
    assert!(changes.columns().is_empty() && !changes.has_structure_change());
    assert!(changes.has_parent_change());
    assert_eq!(
        tree_of(&engine.rebuild(&snapshot, &spec), &snapshot),
        [(1, 0), (2, 0), (3, 1), (4, 2)]
    );
}

#[test]
fn test_tree_rebuild_allocations() {
    // Rebuild, expand and compact allocate the same on 100 and on 300 rows:
    // nothing per row.
    let view = TreeView {
        sort: vec![
            (NodeColumn::Price, SortDirection::Descending),
            (NodeColumn::Name, SortDirection::Ascending),
        ],
        hidden_group: Some(1),
        quick_text: "a".to_owned(),
        depth: ExpandDepth::Levels(2),
        toggled: vec![3, 5, 8],
        ..TreeView::new()
    };
    let spec = view.spec();
    let collapsed = TreeView {
        depth: ExpandDepth::Levels(0),
        ..view.clone()
    }
    .spec();
    let engine = Rebuild::default().with_max_threads(1);
    let counts: Vec<[u64; 3]> = [100, 300]
        .into_iter()
        .map(|keys| {
            let mut rng = fastrand::Rng::with_seed(7);
            let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
            ingest.apply(
                (0..keys).map(|id| Op::UpsertSilent(Arc::new(random_node(&mut rng, id, keys)))),
                Instant::now(),
            );
            let snapshot = ingest.snapshot();
            let before = allocations();
            let projection = engine.rebuild(&snapshot, &spec);
            let rebuild = allocations() - before;
            let before = allocations();
            let expanded = engine.expand(&projection, &snapshot, &collapsed);
            let expand = allocations() - before;
            ingest.apply((0..10).map(Op::Remove), Instant::now());
            let after = ingest.snapshot();
            let before = allocations();
            let compacted = engine.compact(&expanded, &after);
            let compact = allocations() - before;
            assert!(projection.row_count() > 10 && compacted.row_count() > 0);
            [rebuild, expand, compact]
        })
        .collect();
    assert_eq!(counts[0], counts[1], "rebuild, expand, compact");
}

/// The id of the fading row of `key`.
fn fading_id(snapshot: &StoreSnapshot<Node>, key: u32) -> RowId {
    snapshot
        .fading_rows()
        .find(|(_, row, ..)| row.id == key)
        .map(|(id, ..)| id)
        .expect("a fading row")
}

#[test]
fn test_fading_rows_in_tree() {
    // 1 ─┬─ 2
    //    └─ 3 ─── 4
    // 5 m
    // 6 ─── 7 m
    let view = TreeView::new();
    let filtered = TreeView {
        quick_text: "m".to_owned(),
        ..TreeView::new()
    };
    for (name, engine) in engines() {
        let engine = engine.as_ref();
        let epoch = Instant::now();
        let mut ingest = Ingest::<Node>::new(policy(true), epoch);
        ingest.apply(
            [
                (1, None, ""),
                (2, Some(1), ""),
                (3, Some(1), ""),
                (4, Some(3), ""),
                (5, None, "m"),
                (6, None, ""),
                (7, Some(6), "m"),
            ]
            .map(|(id, parent, name)| Op::UpsertSilent(Arc::new(node(id, parent, name)))),
            epoch,
        );
        let tree = |snapshot: &StoreSnapshot<Node>, view: &TreeView| {
            let projection = engine.rebuild(snapshot, &view.spec());
            let shown = actual(&projection, snapshot);
            assert_eq!(shown, reference_of(snapshot).expected(view), "{name}");
            (projection, shown)
        };
        let before = engine.rebuild(&ingest.snapshot(), &view.spec());
        let before_filtered = engine.rebuild(&ingest.snapshot(), &filtered.spec());

        // A fading leaf stays under its parent, a shown child of it, counted
        // nowhere; compaction keeps it there.
        ingest.apply([Op::Remove(2)], epoch);
        let snapshot = ingest.snapshot();
        let (projection, shown) = tree(&snapshot, &view);
        assert_eq!(
            shown.lines,
            [(1, 0), (2, 1), (3, 1), (4, 2), (5, 0), (6, 0), (7, 1)]
        );
        assert_eq!((shown.live, shown.branches[&1].0), (6, 2), "{name}");
        assert_eq!(projection.branch(fading_id(&snapshot, 2)), None);
        assert_eq!(
            actual(&engine.compact(&before, &snapshot), &snapshot),
            shown,
            "{name}: compact"
        );

        // A fading branch is a leaf: its child is an orphan, a root.
        ingest.apply([Op::Remove(3)], epoch);
        let snapshot = ingest.snapshot();
        let (_, shown) = tree(&snapshot, &view);
        assert_eq!(
            shown.lines,
            [(1, 0), (2, 1), (3, 1), (4, 0), (5, 0), (6, 0), (7, 1)]
        );
        assert_eq!(shown.live, 5);
        // Compaction, the fallback, gives the same here.
        let compacted = engine.compact(&before, &snapshot);
        assert_eq!(actual(&compacted, &snapshot), shown, "{name}: compact");

        // A fading root stays a root; fading rows under it go, as a fading
        // row is never a parent.
        ingest.apply([Op::Remove(1)], epoch);
        let snapshot = ingest.snapshot();
        let (_, shown) = tree(&snapshot, &view);
        assert_eq!(shown.lines, [(1, 0), (4, 0), (5, 0), (6, 0), (7, 1)]);
        assert_eq!(shown.live, 4);
        assert_eq!(
            actual(&engine.compact(&compacted, &snapshot), &snapshot),
            shown,
            "{name}: compact"
        );

        // Filtered: 7 kept 6 for context. Fading, it keeps nothing: 6 goes,
        // and 7 with it, in a rebuild and in compaction.
        let (_, shown) = tree(&snapshot, &filtered);
        assert_eq!(shown.lines, [(5, 0), (6, 0), (7, 1)]);
        ingest.apply([Op::Remove(7)], epoch);
        let snapshot = ingest.snapshot();
        let (_, shown) = tree(&snapshot, &filtered);
        assert_eq!(shown.lines, [(5, 0)], "{name}");
        assert_eq!(
            actual(&engine.compact(&before_filtered, &snapshot), &snapshot),
            shown,
            "{name}: compact"
        );
        // Unfiltered, it stays under 6.
        let (_, shown) = tree(&snapshot, &view);
        assert_eq!(shown.lines, [(1, 0), (4, 0), (5, 0), (6, 0), (7, 1)]);

        // Past the fade the store drops them, and so does compaction.
        let fading_projection = engine.rebuild(&snapshot, &view.spec());
        let changes = ingest.apply([], epoch + FADE + Duration::from_secs(1));
        assert_eq!(changes.faded().len(), 4, "{name}");
        let snapshot = ingest.snapshot();
        let compacted = engine.compact(&fading_projection, &snapshot);
        let (_, shown) = tree(&snapshot, &view);
        assert_eq!(shown.lines, [(4, 0), (5, 0), (6, 0)]);
        assert_eq!(actual(&compacted, &snapshot), shown, "{name}: compact");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum HoldingColumn {
    Qty,
    Price,
    Name,
    #[cfg(feature = "decimal")]
    Amount,
}

impl GridColumns for HoldingColumn {
    fn all() -> &'static [Self] {
        &[
            Self::Qty,
            Self::Price,
            Self::Name,
            #[cfg(feature = "decimal")]
            Self::Amount,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Qty => "qty",
            Self::Price => "price",
            Self::Name => "name",
            #[cfg(feature = "decimal")]
            Self::Amount => "amount",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A row of a portfolio: a fund, desk or book holds the sum of its
/// children's quantities, a position its own.
#[derive(Clone, Debug)]
struct Holding {
    id: u32,
    parent: Option<u32>,
    qty: i64,
    price: f64,
    name: &'static str,
}

impl Holding {
    /// The quantity in money with cents, exact as a `Decimal`.
    #[cfg(feature = "decimal")]
    fn amount(&self) -> rust_decimal::Decimal {
        rust_decimal::Decimal::new(self.qty * 100 + 10, 2)
    }
}

impl GridRow for Holding {
    type Key = u32;
    type Column = HoldingColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: HoldingColumn) -> CellValue<'_> {
        match column {
            HoldingColumn::Qty => CellValue::Int(self.qty),
            HoldingColumn::Price => CellValue::Float(self.price),
            HoldingColumn::Name => CellValue::Text(self.name),
            #[cfg(feature = "decimal")]
            HoldingColumn::Amount => CellValue::Decimal(self.amount()),
        }
    }

    fn parent_key(&self) -> Option<&u32> {
        self.parent.as_ref()
    }
}

/// Positions and the roots alone.
struct Roots;

impl TreeTotalsRule for Roots {
    fn is_counted(&self, row: &TreeTotalsContext) -> bool {
        row.depth() == 0
    }
}

/// Rows that pass the filters themselves and show no children.
struct PassingLeaves;

impl TreeTotalsRule for PassingLeaves {
    fn is_counted(&self, row: &TreeTotalsContext) -> bool {
        row.passes() && !row.is_branch()
    }
}

#[test]
fn test_tree_totals() {
    // 1 fund 60 ─┬─ 2 desk 30 ─┬─ 4 book 10 ─┬─ 8 pos 4
    //            │             │             └─ 9 pos 6
    //            │             └─ 5 book 20 ─── 10 pos 20
    //            └─ 3 desk 30 ─── 6 book 30 ─── 11 pos 30
    // 7 fund 5 ─── 12 pos 5
    let holding = |id, parent, qty, name| Holding {
        id,
        parent,
        qty,
        price: f64::from(id) / 4.0,
        name,
    };
    let rows = [
        holding(1, None, 60, "fund"),
        holding(2, Some(1), 30, "desk"),
        holding(3, Some(1), 30, "desk"),
        holding(4, Some(2), 10, "book"),
        holding(5, Some(2), 20, "book"),
        holding(6, Some(3), 30, "book"),
        holding(7, None, 5, "fund"),
        holding(8, Some(4), 4, "pos"),
        holding(9, Some(4), 6, "pos"),
        holding(10, Some(5), 20, "pos"),
        holding(11, Some(6), 30, ""),
        holding(12, Some(7), 5, "pos"),
    ];
    let spec = |totals: TreeTotals, quick: &str| {
        let mut name = ColumnSet::EMPTY;
        name.insert(HoldingColumn::Name.ix());
        ViewSpec::new()
            .with_quick_filter(QuickFilter::new(quick, name, &ContainsAllTerms))
            .with_tree(TreeSpec::new().with_totals(totals))
            .with_aggregates([
                (HoldingColumn::Qty, Aggregate::Sum),
                (HoldingColumn::Name, Aggregate::Count),
                (HoldingColumn::Price, Aggregate::Min),
                #[cfg(feature = "decimal")]
                (HoldingColumn::Amount, Aggregate::Sum),
            ])
    };
    // The totals of the rows of these keys, by definition.
    let expected = |keys: &[u32]| {
        let counted: Vec<&Holding> = keys.iter().map(|&key| &rows[key as usize - 1]).collect();
        let qty: i64 = counted.iter().map(|row| row.qty).sum();
        // Empty text is a value: `Count` counts every name.
        let names = counted.len() as i64;
        let price = counted
            .iter()
            .map(|row| row.price)
            .reduce(f64::min)
            .map_or(OwnedValue::Empty, OwnedValue::Float);
        #[cfg_attr(not(feature = "decimal"), expect(unused_mut))]
        let mut values = vec![OwnedValue::Int(qty), price, OwnedValue::Int(names)];
        #[cfg(feature = "decimal")]
        values.push(OwnedValue::Decimal(
            counted.iter().map(|row| row.amount()).sum(),
        ));
        values
    };
    let totals_of = |projection: &Projection| -> Vec<OwnedValue> {
        HoldingColumn::all()
            .iter()
            .map(|column| projection.totals().get(column.ix()).unwrap().clone())
            .collect()
    };
    let same = |actual: Vec<OwnedValue>, expected: Vec<OwnedValue>| {
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| std::mem::discriminant(a) == std::mem::discriminant(b) && a == b)
    };

    for (name, engine) in [
        ("Rebuild", Rebuild::default()),
        (
            "Rebuild on one thread",
            Rebuild::default().with_max_threads(1),
        ),
    ] {
        let epoch = Instant::now();
        let mut ingest = Ingest::<Holding>::new(policy(true), epoch);
        ingest.apply(
            rows.iter()
                .cloned()
                .map(|row| Op::UpsertSilent(Arc::new(row))),
            epoch,
        );
        let snapshot = ingest.snapshot();
        let cases: [(TreeTotals, &str, &[u32], usize); 6] = [
            // Leaves by default: the parents hold their children's sums.
            (TreeTotals::Leaves, "", &[8, 9, 10, 11, 12], 12),
            (
                TreeTotals::AllRows,
                "",
                &[1, 2, 4, 8, 9, 5, 10, 3, 6, 11, 7, 12],
                12,
            ),
            (TreeTotals::Custom(Arc::new(Roots)), "", &[1, 7], 12),
            // Filtered: rows kept for context are shown, so counted by
            // `AllRows`; a book whose positions are all filtered out is a
            // leaf.
            (TreeTotals::Leaves, "pos", &[8, 9, 10, 12], 9),
            (
                TreeTotals::AllRows,
                "pos",
                &[1, 2, 4, 8, 9, 5, 10, 7, 12],
                9,
            ),
            (TreeTotals::Leaves, "fund", &[1, 7], 2),
        ];
        for (totals, quick, keys, shown) in cases {
            let context = format!("{name}, {totals:?}, {quick:?}");
            let projection = engine.rebuild(&snapshot, &spec(totals, quick));
            assert!(
                same(totals_of(&projection), expected(keys)),
                "{context}: {:?}",
                totals_of(&projection)
            );
            assert_eq!(projection.totals().row_count(), shown, "{context}");
            assert_eq!(projection.row_count(), shown, "{context}");
        }
        let passing = engine.rebuild(
            &snapshot,
            &spec(TreeTotals::Custom(Arc::new(PassingLeaves)), "book"),
        );
        assert!(same(totals_of(&passing), expected(&[4, 5, 6])), "{name}");

        // Fading rows count for nothing: a book whose position fades is a
        // leaf, so the default counts it.
        let leaves = spec(TreeTotals::Leaves, "");
        let built = engine.rebuild(&snapshot, &leaves);
        ingest.apply([Op::Remove(12), Op::Remove(9)], epoch);
        let removed = ingest.snapshot();
        let projection = engine.rebuild(&removed, &leaves);
        assert!(
            same(totals_of(&projection), expected(&[8, 10, 11, 7])),
            "{name}: {:?}",
            totals_of(&projection)
        );
        assert_eq!((projection.rows().len(), projection.row_count()), (12, 10));
        assert_eq!(projection.totals().row_count(), 10);

        // The aggregate job recounts what a rebuild counts: after the
        // compaction that keeps the fading rows, and after an update.
        let compacted = engine.compact(&built, &removed);
        let refreshed = engine.aggregate(&compacted, &removed, &leaves);
        assert!(
            same(totals_of(&refreshed), totals_of(&projection)),
            "{name}"
        );
        assert_eq!(refreshed.row_count(), 10);
        ingest.apply(
            [Op::Upsert(Arc::new(holding(10, Some(5), 25, "pos")))],
            epoch,
        );
        let updated = ingest.snapshot();
        let refreshed = engine.aggregate(&refreshed, &updated, &leaves);
        let rebuilt = engine.rebuild(&updated, &leaves);
        assert!(same(totals_of(&refreshed), totals_of(&rebuilt)), "{name}");
        assert_eq!(
            refreshed.totals().get(HoldingColumn::Qty.ix()),
            Some(&OwnedValue::Int(4 + 25 + 30 + 5))
        );
        assert_eq!(refreshed.store_version(), updated.version());
        // Without aggregates a tree has no totals, but its count.
        let bare = engine.aggregate(
            &rebuilt,
            &updated,
            &ViewSpec::new().with_tree(TreeSpec::new()),
        );
        assert!(bare.totals().is_empty());
        assert_eq!(bare.row_count(), 10);
    }
}

/// S19, background part: a tree of 100k rows, depth 5, about 20k branches; 10k
/// updates a second to the sort column between rebuilds at 10 Hz; a quick
/// filter with `KeepAncestors`. Prints p50, p99 and max; run with `cargo test
/// --release -p gpui-kit-grid --features test-support --test engine_tree --
/// --ignored --nocapture`.
#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
#[cfg(not(target_family = "wasm"))]
fn test_tree_rebuild_timing() {
    use std::time::Duration;

    const WORDS: [&str; 8] = [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    ];
    // Funds, desks, books, sub-books and positions: 20k branches.
    const LEVELS: [u32; 5] = [200, 800, 3_000, 16_000, 80_000];
    let mut rng = fastrand::Rng::with_seed(19);
    let mut rows: Vec<Node> = Vec::new();
    let mut above = 0..0;
    for count in LEVELS {
        let level = rows.len() as u32..rows.len() as u32 + count;
        for id in level.clone() {
            rows.push(Node {
                id,
                parent: (!above.is_empty()).then(|| rng.u32(above.clone())),
                group: rng.i64(0..4),
                price: rng.f64() * 100.0,
                name: format!("{} {}", WORDS[rng.usize(..WORDS.len())], rng.u16(..1_000)),
            });
        }
        above = level;
    }
    let mut arrival: Vec<Arc<Node>> = rows.iter().cloned().map(Arc::new).collect();
    rng.shuffle(&mut arrival);

    let mut name = ColumnSet::EMPTY;
    name.insert(NodeColumn::Name.ix());
    let view = |text: &str, depth, totals: bool| {
        let spec = ViewSpec::new()
            .with_sort([(NodeColumn::Price, SortDirection::Descending)])
            .with_quick_filter(QuickFilter::new(text, name, &ContainsAllTerms))
            .with_tree(TreeSpec::new())
            .with_tree_expansion(depth, []);
        if totals {
            spec.with_aggregates([
                (NodeColumn::Price, Aggregate::Sum),
                (NodeColumn::Name, Aggregate::Count),
            ])
        } else {
            spec
        }
    };
    let percentiles = |samples: &mut Vec<Duration>| {
        samples.sort();
        let at = |fraction: f64| {
            let ix = ((samples.len() - 1) as f64 * fraction).round() as usize;
            samples[ix].as_secs_f64() * 1e3
        };
        format!("{:.1} ms | {:.1} ms | {:.1} ms", at(0.5), at(0.99), at(1.0))
    };
    println!("| S19 tree, 100000 rows | Shown | p50 | p99 | max |");
    println!("|---|---|---|---|---|");
    for (filter, text, totals) in [
        ("quick filter, KeepAncestors", "alpha", false),
        ("no filter", "", false),
        (
            "quick filter, KeepAncestors, Sum and Count over leaves",
            "alpha",
            true,
        ),
    ] {
        let spec = view(text, ExpandDepth::All, totals);
        let collapsed = view(text, ExpandDepth::Levels(0), totals);
        for (threads, engine) in [
            ("", Rebuild::default()),
            (", one thread", Rebuild::default().with_max_threads(1)),
        ] {
            let mut ingest = Ingest::<Node>::new(HighlightPolicy::default(), Instant::now());
            ingest.apply(
                arrival.iter().cloned().map(Op::UpsertSilent),
                Instant::now(),
            );
            let mut rebuilds = Vec::new();
            let mut expands = Vec::new();
            let mut compacts = Vec::new();
            let mut aggregates = Vec::new();
            let mut shown = 0;
            for _ in 0..100 {
                // 100 ms of 10k updates a second, to the sort column.
                ingest.apply(
                    (0..1_000).map(|_| {
                        let ix = rng.usize(..rows.len());
                        let row = &mut rows[ix];
                        row.price = rng.f64() * 100.0;
                        Op::Upsert(Arc::new(row.clone()))
                    }),
                    Instant::now(),
                );
                let snapshot = ingest.snapshot();
                let started = Instant::now();
                let projection = engine.rebuild(&snapshot, &spec);
                rebuilds.push(started.elapsed());
                shown = projection.row_count();
                let started = Instant::now();
                let laid_out = engine.expand(&projection, &snapshot, &collapsed);
                expands.push(started.elapsed());
                assert!(laid_out.len() < projection.len());
                let started = Instant::now();
                let compacted = engine.compact(&projection, &snapshot);
                compacts.push(started.elapsed());
                assert_eq!(compacted.row_count(), shown);
                let started = Instant::now();
                let refreshed = engine.aggregate(&projection, &snapshot, &spec);
                aggregates.push(started.elapsed());
                assert_eq!(refreshed.totals().is_empty(), !totals);
            }
            println!(
                "| rebuild, {filter}{threads} | {shown} | {} |",
                percentiles(&mut rebuilds)
            );
            println!(
                "| expand all ↔ none, {filter}{threads} | {shown} | {} |",
                percentiles(&mut expands)
            );
            println!(
                "| compact, {filter}{threads} | {shown} | {} |",
                percentiles(&mut compacts)
            );
            println!(
                "| aggregate, {filter}{threads} | {shown} | {} |",
                percentiles(&mut aggregates)
            );
        }
    }
}
