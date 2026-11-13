//! Headless UI tests of grouping and expansion
//! (`.agents/COMPONENT_TEST_RULES.md`).
//!
//! In the grid state: the grouping and expansion APIs and their events, the set
//! of projection jobs (rebuild, compaction, aggregates, expansion) and when
//! each runs, the held order, and a tree rebuilt for a new parent. These tests
//! follow the jobs the engine is asked for (`Recorder`), the state's readers
//! and the lines the default engine shows.
//!
//! On screen: group captions in the frame, their chevrons, the mouse and the
//! keys that expand and collapse, the focus on a caption without the selection,
//! the scope of a command over a caption, the caption's menu and click, the
//! scroll anchor, and the accessibility tree of a grouped view (the window's
//! accessibility is off, so the tree is planned with `set_a11y_forced`).

mod common;

use std::{
    cell::RefCell,
    cmp::Ordering,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui_kit::accesskit::Action as A11yAction;
use gpui_kit::{
    App, Context, ElementId, Focusable as _, Modifiers, MouseButton, Role, TestAppContext,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    Aggregate, AggregateFn, AggregateLine, AggregateOrder, AggregateStyleContext, CaptionTag,
    CellStyle, ExpandArea, ExpandDepth, ExpandEdit, ExpandGesture, ExpandGestureContext,
    ExpandTarget, Expansion, FlushOutcome, FocusCaptions, GridColumns as _, GridMenuContext,
    GridTarget, GridTuning, GroupBy, GroupCaptionContext, GroupOrigin, GroupPath, HoldReorder,
    LineScopeContext, LiveGridDelegate, LiveGridEvent, LiveGridState, ManualReorder, NumberEditor,
    OwnedValue, Projection, ProjectionEngine, Rebuild, ScopedSelection, SelectableLine,
    SelectionMode, SelectionOrigin, SelectionPolicy, SortDirection, SortOrigin,
    StandardExpandGesture, StoreSnapshot, TreeSpec, ViewSpec,
};

use crate::common::{
    GroupSeen, Grouped, GroupedColumn, GroupedFixture, GroupedRow, LineKind, PausableRunner, ROW,
    caption_point, chevron_point, group_path, grouped_cell, grouped_rows, move_pointer_to,
    open_grouped, open_grouped_with, press_at, press_chevron, unthrottled,
};

use GroupOrigin::Programmatic;
use GroupedColumn::{Id, Instrument, Price, Qty, Side, Trader};
use SortDirection::{Ascending, Descending};

/// A job the engine was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Rebuild,
    Compact,
    Aggregate,
    Expand,
}

/// One job the engine ran: the store version of the snapshot it was given
/// and of the projection it started from, and the view's expansion.
#[derive(Clone, Debug)]
struct Record {
    call: Call,
    snapshot: u64,
    previous: Option<u64>,
    expansion: Option<Expansion<u32>>,
}

/// The default engine, recording every job it runs.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Record>>>);

impl Recorder {
    fn record(
        &self,
        call: Call,
        snapshot: &StoreSnapshot<GroupedRow>,
        previous: Option<&Projection>,
        view: Option<&ViewSpec<GroupedRow>>,
    ) {
        self.0.lock().unwrap().push(Record {
            call,
            snapshot: snapshot.version(),
            previous: previous.map(Projection::store_version),
            expansion: view.map(|view| view.expansion().clone()),
        });
    }

    /// The jobs run since the last call.
    fn take(&self) -> Vec<Record> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// The kinds of the jobs run since the last call.
    fn calls(&self) -> Vec<Call> {
        self.take().into_iter().map(|record| record.call).collect()
    }
}

impl ProjectionEngine<GroupedRow> for Recorder {
    fn rebuild(
        &self,
        snapshot: &StoreSnapshot<GroupedRow>,
        spec: &ViewSpec<GroupedRow>,
    ) -> Projection {
        self.record(Call::Rebuild, snapshot, None, Some(spec));
        Rebuild::default().rebuild(snapshot, spec)
    }

    fn compact(&self, previous: &Projection, snapshot: &StoreSnapshot<GroupedRow>) -> Projection {
        self.record(Call::Compact, snapshot, Some(previous), None);
        Rebuild::default().compact(previous, snapshot)
    }

    fn aggregate(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<GroupedRow>,
        spec: &ViewSpec<GroupedRow>,
    ) -> Projection {
        self.record(Call::Aggregate, snapshot, Some(previous), Some(spec));
        Rebuild::default().aggregate(previous, snapshot, spec)
    }

    fn expand(
        &self,
        previous: &Projection,
        snapshot: &StoreSnapshot<GroupedRow>,
        spec: &ViewSpec<GroupedRow>,
    ) -> Projection {
        self.record(Call::Expand, snapshot, Some(previous), Some(spec));
        Rebuild::default().expand(previous, snapshot, spec)
    }
}

/// Longer than the throttle of [`throttled`].
const PAST_THROTTLE: Duration = Duration::from_millis(1_100);

/// Data-driven work waits a second.
fn throttled() -> GridTuning {
    GridTuning::default().with_min_rebuild_interval(Duration::from_secs(1))
}

fn by(column: GroupedColumn) -> GroupBy<GroupedRow> {
    GroupBy::column(column)
}

/// Grouped by instrument, sorted by price within the groups.
fn by_instrument() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(Price, Ascending)])
        .with_group_by([by(Instrument)])
}

fn aapl() -> GroupPath {
    group_path(&[(Instrument, "AAPL")])
}

fn group(path: GroupPath) -> ExpandTarget<u32> {
    ExpandTarget::Group(path)
}

/// Trade `id` of `grouped_rows(40)`.
fn trade(id: u32) -> GroupedRow {
    grouped_rows(40).swap_remove(id as usize)
}

fn write(fixture: &GroupedFixture, cx: &mut TestAppContext, row: &GroupedRow) {
    fixture.writer(cx).upsert(row.clone());
}

/// Lets data-driven work past the throttle of [`throttled`], then settles.
fn past_throttle(fixture: &GroupedFixture, cx: &mut TestAppContext) {
    fixture.frame(cx, PAST_THROTTLE);
    fixture.settle(cx);
}

fn update<T>(
    fixture: &GroupedFixture,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut LiveGridState<Grouped>, &mut Context<LiveGridState<Grouped>>) -> T,
) -> T {
    fixture.grid.update(cx, f)
}

fn read<T>(
    fixture: &GroupedFixture,
    cx: &mut TestAppContext,
    f: impl FnOnce(&LiveGridState<Grouped>) -> T,
) -> T {
    fixture.grid.read_with(cx, |grid, _| f(grid))
}

/// The ids of the grouping levels, the top one first.
fn level_ids(fixture: &GroupedFixture, cx: &mut TestAppContext) -> Vec<String> {
    read(fixture, cx, |grid| {
        grid.group_by()
            .iter()
            .map(|level| level.id().as_str().to_owned())
            .collect()
    })
}

fn is_grouped(fixture: &GroupedFixture, cx: &mut TestAppContext) -> bool {
    read(fixture, cx, |grid| grid.projection().is_grouped())
}

/// The shown line after the caption of `path`: the next caption when the
/// group is collapsed. For a grouped projection only.
fn line_after(fixture: &GroupedFixture, cx: &mut TestAppContext, path: &GroupPath) -> LineKind {
    let line = fixture
        .caption_line(cx, path)
        .expect("the caption is shown");
    fixture.lines(cx)[line + 1].clone()
}

/// The selected keys, sorted.
fn selected_keys(fixture: &GroupedFixture, cx: &mut TestAppContext) -> Vec<u32> {
    read(fixture, cx, |grid| {
        let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    })
}

/// The aggregate of `column` of the group at `path` in a projection.
fn group_aggregate(
    projection: &Projection,
    path: &GroupPath,
    column: GroupedColumn,
) -> Option<OwnedValue> {
    let group = projection.groups().find(path)?;
    group.aggregates().get(column.ix()).cloned()
}

/// The shown lines of a tree: the key and depth of each row, top to
/// bottom.
fn tree_lines(fixture: &GroupedFixture, cx: &mut TestAppContext) -> Vec<(u32, u16)> {
    read(fixture, cx, |grid| {
        let (projection, snapshot) = (grid.projection(), grid.snapshot());
        (0..projection.len())
            .filter_map(|ix| {
                let line = projection.line(ix)?;
                Some((snapshot.row(line.row()?)?.id, line.depth()))
            })
            .collect()
    })
}

/// A `flush` in flight: once it resolves, its outcome and the number of
/// `ProjectionChange` events that had come by then.
struct PendingFlush(Rc<RefCell<Option<(Option<FlushOutcome>, usize)>>>);

impl PendingFlush {
    fn start(fixture: &GroupedFixture, cx: &mut TestAppContext) -> Self {
        let task = update(fixture, cx, |grid, cx| grid.flush(cx));
        let slot = Rc::new(RefCell::new(None));
        let (done, events) = (slot.clone(), fixture.events.clone());
        cx.update(|cx| {
            cx.spawn(async move |_| {
                let outcome = task.await;
                let shown = events
                    .borrow()
                    .iter()
                    .filter(|event| {
                        matches!(event, gpui_kit_grid::LiveGridEvent::ProjectionChange { .. })
                    })
                    .count();
                *done.borrow_mut() = Some((outcome, shown));
            })
            .detach();
        });
        Self(slot)
    }

    fn result(&self) -> Option<(Option<FlushOutcome>, usize)> {
        *self.0.borrow()
    }
}

#[gpui_kit::test]
fn test_group_by_api_and_events(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Price, Ascending)]))
            .with_tuning(unthrottled())
            .with_reorder_policy(ManualReorder)
    });
    assert!(level_ids(&fixture, cx).is_empty());
    update(&fixture, cx, |grid, cx| grid.scroll_to_row(20, cx));
    fixture.settle(cx);
    assert!(read(&fixture, cx, |grid| grid.visible_rows().start) > 0);
    fixture.take_seen();

    // Repeated levels are dropped, the first one stays; the event comes at
    // once, the order when it is shown, from the top.
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by(
            [
                by(Instrument),
                by(Side),
                by(Instrument).with_direction(Descending),
            ],
            cx,
        )
    });
    assert_eq!(level_ids(&fixture, cx), ["instrument", "side"]);
    assert_eq!(
        read(&fixture, cx, |grid| grid.group_by()[0].direction()),
        Ascending
    );
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    assert_eq!(read(&fixture, cx, |grid| grid.visible_rows().start), 0);

    // The same grouping again does nothing; another direction is another
    // grouping.
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side)], cx)
    });
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), []);
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side).with_direction(Descending)], cx)
    });
    fixture.settle(cx);
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Group(Programmatic), GroupSeen::Projection]
    );

    // A held order is dropped first.
    let mut row = trade(5);
    row.price = 0.0;
    write(&fixture, cx, &row);
    fixture.settle(cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    fixture.take_seen();
    update(&fixture, cx, |grid, cx| grid.set_group_by([by(Side)], cx));
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Held(false), GroupSeen::Group(Programmatic)]
    );
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);

    // `set_view`: the sort, then the grouping; the grid keeps its quick
    // filter and expansion.
    update(&fixture, cx, |grid, cx| {
        grid.set_quick_filter("AAPL", cx);
        grid.set_expanded(group(aapl()), false, cx);
    });
    fixture.settle(cx);
    fixture.take_seen();
    let view = || {
        ViewSpec::new()
            .with_sort([(Qty, Descending)])
            .with_group_by([by(Trader)])
    };
    update(&fixture, cx, |grid, cx| grid.set_view(view(), cx));
    assert_eq!(
        fixture.take_seen(),
        [
            GroupSeen::Sort(SortOrigin::Programmatic),
            GroupSeen::Group(Programmatic)
        ]
    );
    assert_eq!(level_ids(&fixture, cx), ["trader"]);
    assert_eq!(
        read(&fixture, cx, |grid| grid.quick_filter().clone()),
        "AAPL"
    );
    assert!(!read(&fixture, cx, |grid| grid.is_expanded(&group(aapl()))));
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    // The same sort and grouping: only the new order.
    update(&fixture, cx, |grid, cx| grid.set_view(view(), cx));
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);

    // A tree replaces the grouping; no levels leave the tree; levels
    // replace it. The two exclude each other, as in AG Grid and DevExpress.
    update(&fixture, cx, |grid, cx| {
        grid.set_view(
            ViewSpec::new()
                .with_sort([(Qty, Descending)])
                .with_tree(TreeSpec::new()),
            cx,
        )
    });
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    assert!(read(&fixture, cx, |grid| grid.tree().is_some()));
    assert!(level_ids(&fixture, cx).is_empty());
    update(&fixture, cx, |grid, cx| grid.set_group_by([], cx));
    assert_eq!(fixture.take_seen(), []);
    assert!(read(&fixture, cx, |grid| grid.tree().is_some()));
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument)], cx)
    });
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    assert!(read(&fixture, cx, |grid| grid.tree().is_none()));
    assert_eq!(level_ids(&fixture, cx), ["instrument"]);
    update(&fixture, cx, |grid, cx| grid.set_group_by([], cx));
    assert_eq!(fixture.take_seen(), [GroupSeen::Group(Programmatic)]);
    fixture.settle(cx);
    assert!(!is_grouped(&fixture, cx));
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
}

#[gpui_kit::test]
fn test_expansion_api(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(ViewSpec::new().with_group_by([by(Instrument), by(Side)]))
            .with_expand_depth(ExpandDepth::Levels(1))
            .with_tuning(unthrottled())
    });
    let aapl_buy = group_path(&[(Instrument, "AAPL"), (Side, "Buy")]);
    let is_expanded = |fixture: &GroupedFixture, cx: &mut TestAppContext, path: &GroupPath| {
        read(fixture, cx, |grid| grid.is_expanded(&group(path.clone())))
    };
    assert_eq!(
        read(&fixture, cx, LiveGridState::expand_depth),
        ExpandDepth::Levels(1)
    );
    assert!(is_expanded(&fixture, cx, &aapl()));
    assert!(!is_expanded(&fixture, cx, &aapl_buy));
    assert!(is_expanded(&fixture, cx, &GroupPath::default()));
    fixture.take_seen();

    // Expanded already: nothing. The root and tree rows of a view that is
    // not a tree cannot be toggled.
    assert!(!update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        true,
        cx
    )));
    assert!(!update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(GroupPath::default()),
        false,
        cx
    )));
    assert!(!update(&fixture, cx, |grid, cx| grid.set_expanded(
        ExpandTarget::Row(4),
        false,
        cx
    )));
    assert!(!read(&fixture, cx, |grid| grid.is_expanded(&ExpandTarget::Row(4))));
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), []);

    // A toggle against the default depth: the event at once, the lines when
    // laid out.
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl_buy.clone()),
        true,
        cx
    )));
    assert!(is_expanded(&fixture, cx, &aapl_buy));
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Expansion(
            Some(group(aapl_buy.clone())),
            Programmatic
        )]
    );
    fixture.settle(cx);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);

    // A new default depth clears every toggle; the same one again does
    // nothing.
    update(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::All, cx)
    });
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Expansion(None, Programmatic)]
    );
    assert_eq!(
        read(&fixture, cx, LiveGridState::expand_depth),
        ExpandDepth::All
    );
    update(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::All, cx)
    });
    assert_eq!(fixture.take_seen(), []);
    update(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::Levels(1), cx)
    });
    assert!(!is_expanded(&fixture, cx, &aapl_buy), "the toggle is gone");
    update(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::Captions, cx)
    });
    assert!(is_expanded(&fixture, cx, &aapl()));
    assert!(!is_expanded(&fixture, cx, &aapl_buy), "captions only");
    update(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::All, cx)
    });
    fixture.settle(cx);
    fixture.take_seen();

    // Toggles are kept by path: another grouping silences them, going back
    // brings them back.
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl()), false, cx)
    });
    fixture.settle(cx);
    assert!(is_grouped(&fixture, cx));
    let msft = group_path(&[(Instrument, "MSFT")]);
    assert_eq!(line_after(&fixture, cx, &aapl()), LineKind::Caption(msft));
    update(&fixture, cx, |grid, cx| grid.set_group_by([by(Side)], cx));
    fixture.settle(cx);
    assert!(!is_expanded(&fixture, cx, &aapl()));
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side)], cx)
    });
    fixture.settle(cx);
    assert!(!is_expanded(&fixture, cx, &aapl()));
    assert!(is_expanded(
        &fixture,
        cx,
        &group_path(&[(Instrument, "MSFT")])
    ));
    assert!(is_grouped(&fixture, cx));
    assert_eq!(fixture.caption_line(cx, &aapl_buy), None, "under AAPL");
}

#[gpui_kit::test]
fn test_pending_work_kinds(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let fixture = open_grouped(cx, grouped_rows(40), {
        let recorder = recorder.clone();
        move |grid| {
            grid.with_engine(recorder)
                .with_view(by_instrument().with_aggregates([(Qty, Aggregate::Sum)]))
                .with_tuning(throttled())
                .with_reorder_policy(ManualReorder)
        }
    });
    recorder.take();
    fixture.take_seen();

    // A column outside the sort, the grouping and the aggregates: no job.
    let mut row = trade(1);
    row.trader = "Dan";
    write(&fixture, cx, &row);
    past_throttle(&fixture, cx);
    assert_eq!(recorder.calls(), []);

    // An aggregated column: the aggregates alone, values without a
    // `ProjectionChange`.
    row.qty += 1;
    write(&fixture, cx, &row);
    past_throttle(&fixture, cx);
    assert_eq!(recorder.calls(), [Call::Aggregate]);
    assert_eq!(fixture.take_seen(), []);

    // The sort column: a rebuild, which `ManualReorder` holds.
    row.price = 0.0;
    write(&fixture, cx, &row);
    past_throttle(&fixture, cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    assert_eq!(fixture.take_seen(), [GroupSeen::Held(true)]);
    update(&fixture, cx, LiveGridState::apply_held_order);
    fixture.settle(cx);
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Held(false), GroupSeen::Projection]
    );
    assert_eq!(recorder.calls(), [], "laid out for the expansion already");

    // A removal: a compaction, then the aggregates, in one job.
    fixture.writer(cx).remove(2);
    past_throttle(&fixture, cx);
    assert_eq!(recorder.calls(), [Call::Compact, Call::Aggregate]);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);

    // An expansion: at once, within the throttle the compaction started,
    // and shown under `ManualReorder`.
    let version = read(&fixture, cx, |grid| grid.snapshot().version());
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        false,
        cx
    )));
    fixture.settle(cx);
    let records = recorder.take();
    assert_eq!(
        records.iter().map(|record| record.call).collect::<Vec<_>>(),
        [Call::Expand]
    );
    assert_eq!(records[0].snapshot, version, "expand gets the snapshot");
    let expansion = records[0].expansion.as_ref().expect("a view");
    assert!(!expansion.is_group_expanded(&aapl(), 1));
    assert_eq!(
        fixture.take_seen(),
        [
            GroupSeen::Expansion(Some(group(aapl())), Programmatic),
            GroupSeen::Projection
        ]
    );
    assert!(!read(&fixture, cx, LiveGridState::is_order_held));

    // Removals under groups ordered by an aggregate reorder groups: a rebuild.
    update(&fixture, cx, |grid, cx| {
        grid.set_view(
            by_instrument()
                .with_aggregates([(Qty, Aggregate::Sum)])
                .with_group_order(AggregateOrder::new(Qty, Descending)),
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    fixture.writer(cx).remove(3);
    past_throttle(&fixture, cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);

    // A flat view has nothing to lay out: the event alone.
    update(&fixture, cx, |grid, cx| grid.set_group_by([], cx));
    fixture.settle(cx);
    recorder.take();
    fixture.take_seen();
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        true,
        cx
    )));
    fixture.settle(cx);
    assert_eq!(recorder.calls(), []);
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Expansion(Some(group(aapl())), Programmatic)]
    );
}

#[gpui_kit::test]
fn test_aggregate_job_refreshes_held_order(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let fixture = open_grouped(cx, grouped_rows(40), {
        let recorder = recorder.clone();
        move |grid| {
            grid.with_engine(recorder)
                .with_view(by_instrument().with_aggregates([(Qty, Aggregate::Sum)]))
                .with_tuning(throttled())
                .with_reorder_policy(ManualReorder)
        }
    });
    let shown_version = read(&fixture, cx, |grid| grid.projection().store_version());
    // An AAPL trade moves: the new order is held.
    let mut row = trade(4);
    row.price = 0.0;
    write(&fixture, cx, &row);
    past_throttle(&fixture, cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    let held_version = read(&fixture, cx, |grid| grid.snapshot().version());
    recorder.take();
    fixture.take_seen();

    // Its quantity changes within the throttle; a flush lifts it. One job
    // recounts the shown order and the held one.
    row.qty = 10_000;
    write(&fixture, cx, &row);
    let flush = PendingFlush::start(&fixture, cx);
    fixture.settle(cx);
    let records = recorder.take();
    assert_eq!(
        records
            .iter()
            .map(|record| (record.call, record.previous))
            .collect::<Vec<_>>(),
        [
            (Call::Aggregate, Some(shown_version)),
            (Call::Aggregate, Some(held_version))
        ]
    );
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    let (outcome, shown) = flush.result().expect("the flush resolved");
    let outcome = outcome.expect("the grid is alive");
    assert!(outcome.is_order_held());
    assert_eq!(
        outcome.store_version(),
        read(&fixture, cx, |grid| grid.snapshot().version())
    );
    assert_eq!(shown, 0, "values: no `ProjectionChange`");
    assert_eq!(fixture.take_seen(), []);

    // AAPL's sum with the new quantity, in the shown order and, once shown,
    // in the held one, which needs no other job.
    let expected: i64 = grouped_rows(40)
        .into_iter()
        .filter(|trade| trade.instrument == "AAPL")
        .map(|trade| if trade.id == 4 { 10_000 } else { trade.qty })
        .sum();
    let check_sum = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        assert!(is_grouped(fixture, cx));
        let sum = read(fixture, cx, |grid| {
            group_aggregate(grid.projection(), &aapl(), Qty)
        })
        .expect("AAPL has a sum");
        assert_eq!(
            sum.cmp_total(&OwnedValue::Int(expected)),
            Ordering::Equal,
            "{sum:?}"
        );
    };
    check_sum(&fixture, cx);
    update(&fixture, cx, LiveGridState::apply_held_order);
    fixture.settle(cx);
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Held(false), GroupSeen::Projection]
    );
    assert_eq!(recorder.calls(), []);
    check_sum(&fixture, cx);
}

#[gpui_kit::test]
fn test_held_order_gets_expansion(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let fixture = open_grouped(cx, grouped_rows(40), {
        let recorder = recorder.clone();
        move |grid| {
            grid.with_engine(recorder)
                .with_view(by_instrument())
                .with_tuning(unthrottled())
                .with_reorder_policy(ManualReorder)
        }
    });
    let shown_version = read(&fixture, cx, |grid| grid.projection().store_version());
    recorder.take();
    let mut row = trade(5);
    row.price = 0.0;
    write(&fixture, cx, &row);
    fixture.settle(cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    let held_version = read(&fixture, cx, |grid| grid.snapshot().version());
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    fixture.take_seen();
    let collapsed = |record: &Record| {
        let expansion = record.expansion.as_ref().expect("a view");
        !expansion.is_group_expanded(&aapl(), 1)
    };
    let msft = || LineKind::Caption(group_path(&[(Instrument, "MSFT")]));

    // The shown order is laid out at once; the held one stays held.
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        false,
        cx
    )));
    fixture.settle(cx);
    let records = recorder.take();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].call, Call::Expand);
    assert_eq!(records[0].previous, Some(shown_version));
    assert!(collapsed(&records[0]));
    assert_eq!(
        fixture.take_seen(),
        [
            GroupSeen::Expansion(Some(group(aapl())), Programmatic),
            GroupSeen::Projection
        ]
    );
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    assert!(is_grouped(&fixture, cx));
    assert_eq!(line_after(&fixture, cx, &aapl()), msft());

    // Shown, the held order was laid out for the old expansion: an
    // expansion job lays it out for the new one.
    update(&fixture, cx, LiveGridState::apply_held_order);
    assert_eq!(
        fixture.take_seen(),
        [GroupSeen::Held(false), GroupSeen::Projection]
    );
    fixture.settle(cx);
    let records = recorder.take();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].call, Call::Expand);
    assert_eq!(records[0].previous, Some(held_version));
    assert!(collapsed(&records[0]));
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    assert!(is_grouped(&fixture, cx));
    assert_eq!(line_after(&fixture, cx, &aapl()), msft());
}

#[gpui_kit::test]
fn test_expansion_waits_for_job_in_flight(cx: &mut TestAppContext) {
    let (recorder, runner) = (Recorder::default(), PausableRunner::default());
    let fixture = open_grouped(cx, grouped_rows(40), {
        let (recorder, runner) = (recorder.clone(), runner.clone());
        move |grid| {
            grid.with_engine(recorder)
                .with_task_runner(runner)
                .with_view(by_instrument())
                .with_tuning(unthrottled())
        }
    });
    recorder.take();
    fixture.take_seen();

    // One job at a time: the expansion waits for the rebuild in flight,
    // then starts without the throttle.
    runner.pause();
    let mut row = trade(6);
    row.price = 0.0;
    write(&fixture, cx, &row);
    fixture.settle(cx);
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        false,
        cx
    )));
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [], "the rebuild is queued");
    runner.resume();
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild, Call::Expand]);
    assert_eq!(
        fixture.take_seen(),
        [
            GroupSeen::Expansion(Some(group(aapl())), Programmatic),
            GroupSeen::Projection,
            GroupSeen::Projection
        ]
    );
}

#[gpui_kit::test]
fn test_expansion_ahead_of_throttled_rebuild(cx: &mut TestAppContext) {
    let (recorder, runner) = (Recorder::default(), PausableRunner::default());
    let fixture = open_grouped(cx, grouped_rows(40), {
        let (recorder, runner) = (recorder.clone(), runner.clone());
        move |grid| {
            grid.with_engine(recorder)
                .with_task_runner(runner)
                .with_view(by_instrument())
                .with_tuning(
                    GridTuning::default().with_min_rebuild_interval(Duration::from_secs(10)),
                )
        }
    });
    recorder.take();
    fixture.take_seen();

    // A data-driven rebuild waits for the throttle; the expansion is laid
    // out first, at once.
    let mut row = trade(7);
    row.price = 0.0;
    write(&fixture, cx, &row);
    fixture.settle(cx);
    assert_eq!(recorder.calls(), []);
    runner.pause();
    assert!(update(&fixture, cx, |grid, cx| grid.set_expanded(
        group(aapl()),
        false,
        cx
    )));
    fixture.settle(cx);

    // A flush while the layout is in flight: the layout does not cover the
    // rebuild's changes, so the flush waits for the rebuild, which it
    // starts past the throttle.
    let flush = PendingFlush::start(&fixture, cx);
    fixture.settle(cx);
    assert!(flush.result().is_none());
    runner.resume();
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Expand, Call::Rebuild]);
    let (outcome, shown) = flush.result().expect("the flush resolved");
    let outcome = outcome.expect("the grid is alive");
    assert_eq!(shown, 2, "after the layout and the rebuild");
    assert_eq!(
        outcome.store_version(),
        read(&fixture, cx, |grid| grid.projection().store_version())
    );
    assert_eq!(
        fixture.take_seen(),
        [
            GroupSeen::Expansion(Some(group(aapl())), Programmatic),
            GroupSeen::Projection,
            GroupSeen::Projection
        ]
    );
}

#[gpui_kit::test]
fn test_collapsed_selection_scope(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument()).with_tuning(unthrottled())
    });
    // Trade 0 is AAPL's.
    update(&fixture, cx, |grid, cx| grid.select_key(Some(0), cx));
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl()), false, cx)
    });
    fixture.settle(cx);

    // A selected row of a collapsed group passes the view and stays in the
    // command scope, though it is not on screen.
    let (scoped, contains, shown) = read(&fixture, cx, |grid| {
        let id = grid.snapshot().row_id(&0).expect("a live row");
        (
            grid.scoped_selection().keys().to_vec(),
            grid.projection().contains(id),
            grid.projection().display_ix(id),
        )
    });
    assert_eq!(scoped, [0]);
    assert!(contains);
    assert_eq!(selected_keys(&fixture, cx), [0]);
    assert!(is_grouped(&fixture, cx));
    assert_eq!(shown, None, "in a collapsed group");

    // Expanded again, the row is on screen and still selected.
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl()), true, cx)
    });
    fixture.settle(cx);
    let shown = read(&fixture, cx, |grid| {
        let id = grid.snapshot().row_id(&0).expect("a live row");
        grid.projection().display_ix(id)
    });
    assert!(shown.is_some());
    assert_eq!(selected_keys(&fixture, cx), [0]);

    // The collapse moved the focus from the row to the caption, and a
    // command there acts on the group's rows, the selected one too.
    update(&fixture, cx, |grid, cx| grid.set_focused_cell(&0, None, cx));
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl()), false, cx)
    });
    fixture.settle(cx);
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    assert_eq!(selected_keys(&fixture, cx), [0]);
    fixture.take_seen();
    type_key(&fixture, cx, "enter");
    let confirmed = confirms(&take_events(&fixture));
    assert_eq!(confirmed.len(), 1);
    assert!(confirmed[0].contains(&0));
    assert_eq!(confirmed[0].len(), 10);
}

#[gpui_kit::test]
fn test_tree_rebuilds_for_new_parent(cx: &mut TestAppContext) {
    // Trades 1 and 2 under trade 0, trade 3 under trade 1.
    let parent_of = |id| match id {
        1 | 2 => Some(0),
        3 => Some(1),
        _ => None,
    };
    let rows = grouped_rows(4)
        .into_iter()
        .map(|row| GroupedRow {
            parent: parent_of(row.id),
            ..row
        })
        .collect();
    let recorder = Recorder::default();
    let fixture = open_grouped(cx, rows, {
        let recorder = recorder.clone();
        move |grid| {
            grid.with_engine(recorder)
                .with_view(
                    ViewSpec::new()
                        .with_sort([(Id, Ascending)])
                        .with_tree(TreeSpec::new()),
                )
                .with_tuning(unthrottled())
        }
    });
    assert_eq!(tree_lines(&fixture, cx), [(0, 0), (1, 1), (3, 2), (2, 1)]);
    recorder.take();
    fixture.take_seen();

    // Only the parent changes, no column: a rebuild moves trade 3 under
    // trade 2.
    let moved = GroupedRow {
        parent: Some(2),
        ..trade(3)
    };
    write(&fixture, cx, &moved);
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    assert_eq!(tree_lines(&fixture, cx), [(0, 0), (1, 1), (2, 1), (3, 2)]);

    // A view that is not a tree has no work for a new parent.
    update(&fixture, cx, |grid, cx| grid.set_view(by_instrument(), cx));
    fixture.settle(cx);
    recorder.take();
    let back = GroupedRow {
        parent: parent_of(3),
        ..trade(3)
    };
    write(&fixture, cx, &back);
    fixture.settle(cx);
    assert_eq!(recorder.calls(), []);
}

/// The number of a group's rows: a custom aggregate.
struct RowCount;

impl AggregateFn<GroupedRow> for RowCount {
    fn aggregate(
        &self,
        _: GroupedColumn,
        rows: &mut dyn Iterator<Item = &GroupedRow>,
    ) -> OwnedValue {
        OwnedValue::Int(rows.count() as i64)
    }
}

#[gpui_kit::test]
fn test_set_aggregates(cx: &mut TestAppContext) {
    let recorder = Recorder::default();
    let fixture = open_grouped(cx, grouped_rows(40), {
        let recorder = recorder.clone();
        move |grid| {
            grid.with_engine(recorder)
                .with_view(by_instrument().with_aggregates([(Qty, Aggregate::Sum)]))
                .with_tuning(unthrottled())
        }
    });
    update(&fixture, cx, |grid, cx| grid.scroll_to_row(10, cx));
    fixture.settle(cx);
    let first = read(&fixture, cx, |grid| grid.visible_rows().start);
    assert!(first > 0);
    recorder.take();
    fixture.take_seen();

    // The same aggregates: nothing.
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates([(Qty, Aggregate::Sum)], cx)
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), []);

    // Other aggregates: a rebuild as a user action, no event, the scroll
    // position kept.
    let count: Arc<dyn AggregateFn<GroupedRow>> = Arc::new(RowCount);
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates(
            [
                (Qty, Aggregate::Sum),
                (Price, Aggregate::Custom(count.clone())),
            ],
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    assert_eq!(fixture.take_seen(), [GroupSeen::Projection]);
    assert_eq!(read(&fixture, cx, |grid| grid.visible_rows().start), first);
    assert_eq!(
        read(&fixture, cx, |grid| grid
            .aggregates()
            .iter()
            .map(|(column, _)| *column)
            .collect::<Vec<_>>()),
        [Qty, Price]
    );

    // In another order, the same custom aggregate: the same set.
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates(
            [
                (Price, Aggregate::Custom(count.clone())),
                (Qty, Aggregate::Sum),
            ],
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), []);

    // Another custom aggregate is another set, as is another built-in one.
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates(
            [
                (Qty, Aggregate::Sum),
                (Price, Aggregate::Custom(Arc::new(RowCount))),
            ],
            cx,
        )
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
    update(&fixture, cx, |grid, cx| {
        grid.set_aggregates([(Qty, Aggregate::Max)], cx)
    });
    fixture.settle(cx);
    assert_eq!(recorder.calls(), [Call::Rebuild]);
}

// Groups on screen.

/// What the delegate's hooks were asked: every caption text written, every
/// aggregate cell styled, every menu built.
#[derive(Clone, Default)]
struct Hooks {
    captions: Rc<RefCell<Vec<String>>>,
    /// The group, the column and the value of each aggregate cell styled.
    aggregates: Rc<RefCell<Vec<(GroupPath, GroupedColumn, Option<String>)>>>,
    /// The target of each menu built, the command scope it was given, and
    /// the path of its group.
    menus: Rc<RefCell<Vec<MenuSeen>>>,
}

#[derive(Clone, Debug)]
struct MenuSeen {
    target: GridTarget<u32, GroupedColumn>,
    scope: Vec<u32>,
    group: Option<GroupPath>,
}

impl Hooks {
    fn last_menu(&self) -> MenuSeen {
        self.menus
            .borrow()
            .last()
            .cloned()
            .expect("a menu was built")
    }
}

/// A delegate recording its hooks, with a primary command.
struct Recording(Hooks);

impl LiveGridDelegate for Recording {
    type Row = GroupedRow;

    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, GroupedRow>,
        out: &mut String,
        _: &App,
    ) {
        caption.default_caption(out);
        self.0.captions.borrow_mut().push(out.clone());
    }

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, GroupedRow>,
        _: &mut CellStyle,
        _: &App,
    ) {
        let AggregateLine::Caption(group) = cell.line() else {
            panic!("only captions have aggregate cells in these tests");
        };
        self.0.aggregates.borrow_mut().push((
            group.path().clone(),
            cell.column(),
            cell.value().map(|value| format!("{value:?}")),
        ));
    }

    fn primary_command(
        &self,
        menu: &GridMenuContext<'_, GroupedRow>,
        _: &App,
    ) -> Option<gpui_kit::SharedString> {
        Some(format!("Open {} trades", menu.selection().len()).into())
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, GroupedRow>,
        menu: gpui_kit::component::menu::PopupMenu,
        window: &mut gpui_kit::Window,
        cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
    ) -> gpui_kit::component::menu::PopupMenu {
        self.0.menus.borrow_mut().push(MenuSeen {
            target: menu_context.target().clone(),
            scope: menu_context.selection().keys().to_vec(),
            group: menu_context.group().map(|group| group.path().clone()),
        });
        menu_context.standard_items(menu, window, cx)
    }
}

/// A grid of `rows` with the recording delegate, configured by `configure`.
fn open_recording(
    cx: &mut TestAppContext,
    rows: Vec<GroupedRow>,
    configure: impl FnOnce(LiveGridState<Recording>) -> LiveGridState<Recording> + 'static,
) -> (GroupedFixture<Recording>, Hooks) {
    let hooks = Hooks::default();
    let fixture = open_grouped_with(cx, rows, Recording(hooks.clone()), configure);
    (fixture, hooks)
}

fn by_instrument_and_side() -> ViewSpec<GroupedRow> {
    ViewSpec::new()
        .with_sort([(Id, Ascending)])
        .with_group_by([by(Instrument), by(Side)])
}

fn aapl_side(side: &str) -> GroupPath {
    group_path(&[(Instrument, "AAPL"), (Side, side)])
}

/// The focused row and caption.
fn focus_of<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> (Option<u32>, Option<GroupPath>) {
    fixture.grid.read_with(cx, |grid, _| {
        grid.focused_cell().map_or((None, None), |cell| {
            (cell.row().copied(), cell.group().cloned())
        })
    })
}

fn expanded<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    path: &GroupPath,
) -> bool {
    fixture
        .grid
        .read_with(cx, |grid, _| grid.is_expanded(&group(path.clone())))
}

/// Gives the grid the keyboard focus and types `key` (a GPUI keystroke;
/// `shift-=->+` is Shift+= typing "+" on a US layout), then settles.
fn type_key<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    key: &str,
) {
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
        window.press(key, cx);
    });
    fixture.settle(cx);
}

/// Dispatches an Action to the focused grid, then settles.
fn dispatch<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    action: &dyn gpui_kit::Action,
) {
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
        window.dispatch_action(action.boxed_clone(), cx);
    });
    fixture.settle(cx);
}

/// A left press (and release) at a window position, `count`th of a series.
fn left_press<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    modifiers: Modifiers,
    count: usize,
) {
    fixture.input(cx, |window, cx| {
        press_at(window, position, MouseButton::Left, modifiers, count, cx)
    });
}

/// Every event so far, forgetting them.
fn take_events<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
) -> Vec<LiveGridEvent<u32, GroupedColumn>> {
    std::mem::take(&mut *fixture.events.borrow_mut())
}

/// The targets and click counts of the `Click` events among `events`.
fn clicks(
    events: &[LiveGridEvent<u32, GroupedColumn>],
) -> Vec<(GridTarget<u32, GroupedColumn>, usize)> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::Click {
                target,
                click_count,
                ..
            } => Some((target.clone(), *click_count)),
            _ => None,
        })
        .collect()
}

/// The keys of the `Confirm` events among `events`.
fn confirms(events: &[LiveGridEvent<u32, GroupedColumn>]) -> Vec<Vec<u32>> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::Confirm { keys, .. } => Some(keys.to_vec()),
            _ => None,
        })
        .collect()
}

/// The `ExpansionChange` and `FocusChange` events among `events`, in order.
fn expansion_and_focus(events: &[LiveGridEvent<u32, GroupedColumn>]) -> Vec<GroupSeen> {
    events
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::ExpansionChange { target, origin } => {
                Some(GroupSeen::Expansion(target.clone(), *origin))
            }
            LiveGridEvent::FocusChange { origin, .. } => Some(GroupSeen::Focus(*origin)),
            _ => None,
        })
        .collect()
}

fn caption(path: GroupPath) -> GridTarget<u32, GroupedColumn> {
    GridTarget::GroupCaption {
        group: path,
        column: None,
    }
}

/// The hash of a group path in element ids: `FxHasher` over
/// `("live-grid-group", path)`.
fn group_hash(path: &GroupPath) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = rustc_hash::FxHasher::default();
    ("live-grid-group", path).hash(&mut hasher);
    hasher.finish()
}

#[gpui_kit::test]
fn test_captions_on_screen(cx: &mut TestAppContext) {
    // Two trades of each instrument, and one with no instrument.
    let mut rows = grouped_rows(8);
    rows.push(GroupedRow {
        id: 8,
        instrument: "",
        ..trade(0)
    });
    let (fixture, hooks) = open_recording(cx, rows, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Id, Ascending)])
                .with_group_by([by(Instrument)])
                .with_aggregates([(Qty, Aggregate::Sum)]),
        )
        .with_tuning(unthrottled())
    });
    let blank = group_path(&[(Instrument, "")]);
    let path = |instrument| group_path(&[(Instrument, instrument)]);
    assert_eq!(
        fixture.lines(cx),
        [
            LineKind::Caption(blank.clone()),
            LineKind::Data(8),
            LineKind::Caption(path("AAPL")),
            LineKind::Data(0),
            LineKind::Data(4),
            LineKind::Caption(path("MSFT")),
            LineKind::Data(1),
            LineKind::Data(5),
            LineKind::Caption(path("NVDA")),
            LineKind::Data(2),
            LineKind::Data(6),
            LineKind::Caption(path("TSLA")),
            LineKind::Data(3),
            LineKind::Data(7),
        ]
    );
    // Ten lines fit: four captions and six rows; captions are no rows.
    assert_eq!(fixture.visible_keys(cx), [8, 0, 4, 1, 5, 2]);
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.painted_captions(), 4);
    // The default caption: the level's title, the key, the live rows.
    let captions = hooks.captions.borrow().clone();
    assert!(
        captions.contains(&"instrument: AAPL (2)".to_owned()),
        "{captions:?}"
    );
    assert!(
        captions.contains(&"instrument: (Blank) (1)".to_owned()),
        "{captions:?}"
    );
    assert!(
        !captions.iter().any(|text| text.contains("TSLA")),
        "off screen"
    );
    // The aggregate of Qty under its column: AAPL's 100 + 500.
    let aggregates = hooks.aggregates.borrow().clone();
    assert!(aggregates.contains(&(path("AAPL"), Qty, Some("Int(600)".to_owned()))));
    assert!(aggregates.iter().all(|(_, column, _)| *column == Qty));

    // Two levels: a caption per level, the subgroup's below its group's.
    update_recording(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side)], cx)
    });
    fixture.settle(cx);
    let lines = fixture.lines(cx);
    assert_eq!(
        lines[3..7],
        [
            LineKind::Caption(path("AAPL")),
            LineKind::Caption(aapl_side("Buy")),
            LineKind::Data(0),
            LineKind::Caption(aapl_side("Sell")),
        ]
    );
    assert!(
        hooks
            .captions
            .borrow()
            .contains(&"side: Buy (1)".to_owned())
    );
}

fn update_recording<T>(
    fixture: &GroupedFixture<Recording>,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut LiveGridState<Recording>, &mut Context<LiveGridState<Recording>>) -> T,
) -> T {
    fixture.grid.update(cx, f)
}

#[gpui_kit::test]
fn test_caption_clip_and_horizontal_scroll(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(
            by_instrument().with_aggregates([(Qty, Aggregate::Sum), (Trader, Aggregate::Count)]),
        )
        .with_tuning(unthrottled())
    });
    // A wide Trader column: the body scrolls sideways.
    update(&fixture, cx, |grid, cx| {
        grid.set_column_width(Trader, gpui_kit::px(400.), cx);
        grid.set_a11y_forced(true);
        grid.set_focused_group(&aapl(), cx)
    });
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
    });
    fixture.settle(cx);
    let caption_cell = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        let scope = ElementId::NamedInteger("live-grid-group-row".into(), group_hash(&aapl()));
        let mut bounds = None;
        fixture.input(cx, |window, _| {
            bounds = window
                .within(scope)
                .try_find(focus_id())
                .map(|element| element.bounds());
        });
        bounds.expect("the focused caption's cell")
    };
    // The tooltip of the aggregate cell under the pointer, after GPUI's
    // delay: only a painted cell has one.
    let hover_qty = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        // Qty spans x 190..260 at scroll 0; the caption is on line 0.
        let point = grouped_cell(0, Qty);
        fixture.input(cx, |window, cx| {
            move_pointer_to(window, point, cx);
            move_pointer_to(
                window,
                point + gpui_kit::point(gpui_kit::px(1.), gpui_kit::px(0.)),
                cx,
            );
        });
        fixture.frame(cx, Duration::from_millis(600));
        let shown = fixture
            .grid
            .read_with(cx, |grid, _| grid.is_tooltip_shown());
        // Away again, so the next hover starts over.
        fixture.input(cx, |window, cx| {
            move_pointer_to(
                window,
                gpui_kit::point(gpui_kit::px(470.), gpui_kit::px(4.)),
                cx,
            )
        });
        fixture.settle(cx);
        shown
    };

    // The text runs from the body's left edge to Qty's aggregate, 190 px
    // in; the aggregate cell is painted, its tooltip slot under the pointer.
    let bounds = caption_cell(&fixture, cx);
    assert_eq!(bounds.origin.x, gpui_kit::px(0.));
    assert_eq!(bounds.size.width, gpui_kit::px(190.));
    assert!(hover_qty(&fixture, cx));

    // Scrolled 100 px: the text stays; Qty's aggregate starts at 90, left
    // of the text's least width (26 + 120), so it is not painted, and the
    // text runs to Trader's aggregate, at 240.
    update(&fixture, cx, |grid, cx| {
        grid.set_scroll_x(gpui_kit::px(100.), cx)
    });
    fixture.settle(cx);
    let bounds = caption_cell(&fixture, cx);
    assert_eq!(bounds.origin.x, gpui_kit::px(0.));
    assert_eq!(bounds.size.width, gpui_kit::px(240.));
    assert!(!hover_qty(&fixture, cx));
}

/// The id of a focused caption's cell element, in its row's id scope.
fn focus_id() -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name("live-grid-focus".into())),
        "group".into(),
    )
}

#[gpui_kit::test]
fn test_expand_by_mouse(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
    });
    assert_eq!(fixture.caption_line(cx, &aapl()), Some(0));
    take_events(&fixture);

    // The chevron toggles at once, in the same frame (the chevron reads the
    // view's expansion); the lines follow. No click: it is a control.
    fixture.input(cx, |window, cx| {
        press_chevron(window, 0, 0, Modifiers::default(), cx)
    });
    assert!(!expanded(&fixture, cx, &aapl()));
    fixture.settle(cx);
    assert_eq!(
        line_after(&fixture, cx, &aapl()),
        LineKind::Caption(group_path(&[(Instrument, "MSFT")]))
    );
    let events = take_events(&fixture);
    assert_eq!(
        expansion_and_focus(&events),
        [
            GroupSeen::Expansion(Some(group(aapl())), GroupOrigin::User),
            GroupSeen::Focus(SelectionOrigin::User),
        ]
    );
    assert!(clicks(&events).is_empty());
    // The press focused the caption, and selected nothing.
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    assert!(selected_keys_of(&fixture, cx).is_empty());
    fixture.input(cx, |window, cx| {
        press_chevron(window, 0, 0, Modifiers::default(), cx)
    });
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, &aapl()));

    // Alt (Option): the group and every subgroup, one event.
    take_events(&fixture);
    fixture.input(cx, |window, cx| {
        press_chevron(window, 0, 0, Modifiers::alt(), cx)
    });
    fixture.settle(cx);
    for path in [aapl(), aapl_side("Buy"), aapl_side("Sell")] {
        assert!(!expanded(&fixture, cx, &path), "{path:?}");
    }
    assert_eq!(
        expansion_and_focus(&take_events(&fixture)),
        [GroupSeen::Expansion(Some(group(aapl())), GroupOrigin::User)]
    );
    fixture.input(cx, |window, cx| {
        press_chevron(window, 0, 0, Modifiers::alt(), cx)
    });
    fixture.settle(cx);
    for path in [aapl(), aapl_side("Buy"), aapl_side("Sell")] {
        assert!(expanded(&fixture, cx, &path), "{path:?}");
    }
    assert_eq!(
        line_after(&fixture, cx, &aapl()),
        LineKind::Caption(aapl_side("Buy"))
    );

    // A double click on the caption's text toggles once, and confirms
    // nothing; both presses are clicks on the caption.
    take_events(&fixture);
    left_press(&fixture, cx, caption_point(0), Modifiers::default(), 1);
    left_press(&fixture, cx, caption_point(0), Modifiers::default(), 2);
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, &aapl()));
    let events = take_events(&fixture);
    assert!(confirms(&events).is_empty());
    assert_eq!(
        clicks(&events),
        [(caption(aapl()), 1), (caption(aapl()), 2)]
    );

    // A double click on the chevron toggles on its first press only.
    left_press(&fixture, cx, chevron_point(0, 0), Modifiers::default(), 1);
    left_press(&fixture, cx, chevron_point(0, 0), Modifiers::default(), 2);
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, &aapl()));
    let events = take_events(&fixture);
    assert!(clicks(&events).is_empty());
    assert_eq!(
        expansion_and_focus(&events)
            .iter()
            .filter(|seen| matches!(seen, GroupSeen::Expansion(..)))
            .count(),
        1
    );
}

/// On a cell that typing edits, `-` and `+` are the draft's text (a group
/// collapsed while typing `-5` into a quantity surprises more than a lost
/// collapse key on one column); on any other line they collapse and expand.
#[gpui_kit::test]
fn test_type_to_edit_in_groups(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
            .with_editor(Qty, NumberEditor::new())
    });
    // Trade 0 is the first row of AAPL / Buy.
    assert!(update(&fixture, cx, |grid, cx| grid.set_focused_cell(
        &0,
        Some(Qty),
        cx
    )));
    fixture.settle(cx);
    take_events(&fixture);
    type_key(&fixture, cx, "-");
    fixture.input(cx, |window, cx| {
        window.press("5", cx);
        window.press("enter", cx);
    });
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, &aapl_side("Buy")));
    let commits: Vec<(u32, GroupedColumn, OwnedValue)> = take_events(&fixture)
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::EditCommit {
                key, column, value, ..
            } => Some((key, column, value)),
            _ => None,
        })
        .collect();
    assert_eq!(commits, [(0, Qty, OwnedValue::Int(-5))]);

    // A cell without an editor: `-` collapses the row's group.
    assert!(update(&fixture, cx, |grid, cx| grid.set_focused_cell(
        &0,
        Some(Instrument),
        cx
    )));
    fixture.settle(cx);
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, &aapl_side("Buy")));
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Buy"))));
    let edits = take_events(&fixture)
        .into_iter()
        .filter(|event| matches!(event, LiveGridEvent::EditStart { .. }))
        .count();
    assert_eq!(edits, 0);
}

fn selected_keys_of<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<u32> {
    fixture.grid.read_with(cx, |grid, _| {
        let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
        keys.sort_unstable();
        keys
    })
}

#[gpui_kit::test]
fn test_expand_by_keyboard(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
    });
    assert!(update(&fixture, cx, |grid, cx| grid.set_focused_group(&aapl(), cx)));
    fixture.settle(cx);

    // ← collapses an expanded caption; on a collapsed top-level one it does
    // nothing.
    type_key(&fixture, cx, "left");
    assert!(!expanded(&fixture, cx, &aapl()));
    type_key(&fixture, cx, "left");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    // → expands a collapsed caption, then moves to the group's first line.
    type_key(&fixture, cx, "right");
    assert!(expanded(&fixture, cx, &aapl()));
    type_key(&fixture, cx, "right");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Buy"))));
    // ← on the subgroup: collapse, then to the enclosing caption.
    type_key(&fixture, cx, "left");
    assert!(!expanded(&fixture, cx, &aapl_side("Buy")));
    type_key(&fixture, cx, "left");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));

    // `+` (Shift+= on a US layout) expands, `-` collapses.
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Buy"))));
    type_key(&fixture, cx, "shift-=->+");
    assert!(expanded(&fixture, cx, &aapl_side("Buy")));
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, &aapl_side("Buy")));
    type_key(&fixture, cx, "+");
    assert!(expanded(&fixture, cx, &aapl_side("Buy")));
    // `+` on an expanded caption goes on.
    type_key(&fixture, cx, "+");
    assert!(expanded(&fixture, cx, &aapl_side("Buy")));

    // `-` on a data row collapses its group and focuses its caption:
    // `ExpansionChange`, then `FocusChange`.
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), (Some(0), None));
    take_events(&fixture);
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, &aapl_side("Buy")));
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Buy"))));
    assert_eq!(
        expansion_and_focus(&take_events(&fixture)),
        [
            GroupSeen::Expansion(Some(group(aapl_side("Buy"))), GroupOrigin::User),
            GroupSeen::Focus(SelectionOrigin::User),
        ]
    );
    // ↓ and ↑ walk the shown lines, captions too, keeping the column.
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Sell"))));
    type_key(&fixture, cx, "down");
    assert_eq!(focus_of(&fixture, cx), (Some(4), None));
    type_key(&fixture, cx, "up");
    type_key(&fixture, cx, "up");
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl_side("Buy"))));
}

#[gpui_kit::test]
fn test_caption_focus_not_selection(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(8), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Id, Ascending)])
                .with_group_by([by(Instrument)]),
        )
        .with_tuning(unthrottled())
        .with_selection_mode(SelectionMode::RowSet)
    });
    let msft = group_path(&[(Instrument, "MSFT")]);
    // Lines: AAPL, 0, 4, MSFT, 1, 5, NVDA, 2, 6, TSLA, 3, 7.
    left_press(&fixture, cx, grouped_cell(2, Id), Modifiers::default(), 1);
    assert_eq!(selected_keys(&fixture, cx), [4]);
    take_events(&fixture);
    // A press on a caption focuses it; the selection stays.
    left_press(&fixture, cx, caption_point(0), Modifiers::default(), 1);
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    assert_eq!(selected_keys(&fixture, cx), [4]);
    let events = take_events(&fixture);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, LiveGridEvent::SelectionChange { .. }))
    );
    // Space on a caption does nothing.
    type_key(&fixture, cx, "space");
    assert_eq!(selected_keys(&fixture, cx), [4]);

    // A range from row 4 through MSFT's caption to row 1: the caption is
    // not selected; with the focus on the caption, the range stops there.
    left_press(&fixture, cx, grouped_cell(2, Id), Modifiers::default(), 1);
    type_key(&fixture, cx, "shift-down");
    assert_eq!(focus_of(&fixture, cx), (None, Some(msft.clone())));
    assert_eq!(selected_keys(&fixture, cx), [4]);
    type_key(&fixture, cx, "shift-down");
    assert_eq!(focus_of(&fixture, cx), (Some(1), None));
    assert_eq!(selected_keys(&fixture, cx), [1, 4]);

    // Cmd/Ctrl+A selects the rows on screen: not those of a collapsed group.
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(msft.clone()), false, cx)
    });
    fixture.settle(cx);
    type_key(&fixture, cx, "secondary-a");
    assert_eq!(selected_keys(&fixture, cx), [0, 2, 3, 4, 6, 7]);
}

#[gpui_kit::test]
fn test_caption_command_scope(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_recording(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
    });
    // AAPL collapsed, with a collapsed subgroup; a MSFT trade selected.
    update_recording(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl_side("Buy")), false, cx);
        grid.set_expanded(group(aapl()), false, cx);
        grid.select_key(Some(1), cx);
        grid.set_focused_group(&aapl(), cx)
    });
    fixture.settle(cx);
    // Every AAPL trade, Buy then Sell, in the order of `rows()`.
    let aapl_trades = [0, 8, 16, 24, 32, 4, 12, 20, 28, 36];
    take_events(&fixture);
    type_key(&fixture, cx, "enter");
    assert_eq!(confirms(&take_events(&fixture)), [aapl_trades.to_vec()]);
    // The menu of the caption has the same scope.
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            caption_point(0),
            MouseButton::Right,
            Modifiers::default(),
            1,
            cx,
        )
    });
    fixture.settle(cx);
    let menu = hooks.last_menu();
    assert_eq!(menu.target, caption(aapl()));
    assert_eq!(menu.scope, aapl_trades);
    assert_eq!(menu.group, Some(aapl()));
    // Without a caption, a command acts on the scoped selection.
    update_recording(&fixture, cx, |grid, cx| grid.set_focused_cell(&1, None, cx));
    take_events(&fixture);
    type_key(&fixture, cx, "enter");
    assert_eq!(confirms(&take_events(&fixture)), [vec![1]]);
}

#[gpui_kit::test]
fn test_expansion_under_live_data(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument()).with_tuning(unthrottled())
    });
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(aapl()), false, cx)
    });
    fixture.settle(cx);
    let count = |fixture: &GroupedFixture, cx: &mut TestAppContext, path: &GroupPath| {
        read(fixture, cx, |grid| {
            grid.projection()
                .groups()
                .find(path)
                .map(|group| group.row_count())
        })
    };
    assert_eq!(count(&fixture, cx, &aapl()), Some(10));

    // A new trade of a collapsed group does not expand it; its count
    // grows. A new group comes expanded, by the default depth.
    write(
        &fixture,
        cx,
        &GroupedRow {
            id: 100,
            ..trade(0)
        },
    );
    let amzn = group_path(&[(Instrument, "AMZN")]);
    write(
        &fixture,
        cx,
        &GroupedRow {
            id: 101,
            instrument: "AMZN",
            ..trade(1)
        },
    );
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, &aapl()));
    assert_eq!(count(&fixture, cx, &aapl()), Some(11));
    assert_eq!(
        line_after(&fixture, cx, &aapl()),
        LineKind::Caption(amzn.clone())
    );
    assert!(expanded(&fixture, cx, &amzn));
    assert_eq!(line_after(&fixture, cx, &amzn), LineKind::Data(101));

    // Another grouping and back: the collapsed group is collapsed again.
    update(&fixture, cx, |grid, cx| grid.set_group_by([by(Side)], cx));
    fixture.settle(cx);
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument)], cx)
    });
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, &aapl()));
    assert_eq!(line_after(&fixture, cx, &aapl()), LineKind::Caption(amzn));
}

#[gpui_kit::test]
fn test_focus_follows_collapse_and_regroup(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument()).with_tuning(unthrottled())
    });
    let msft = group_path(&[(Instrument, "MSFT")]);
    // The focus on an AAPL trade; collapse all: the focus goes to AAPL's
    // caption at once, after the expansion's event.
    update(&fixture, cx, |grid, cx| grid.set_focused_cell(&0, None, cx));
    fixture.settle(cx);
    take_events(&fixture);
    dispatch(&fixture, cx, &gpui_kit_grid::actions::CollapseAll);
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    assert_eq!(
        expansion_and_focus(&take_events(&fixture)),
        [
            GroupSeen::Expansion(None, GroupOrigin::User),
            GroupSeen::Focus(SelectionOrigin::User),
        ]
    );
    // Lines: AAPL, MSFT, NVDA, TSLA.
    assert_eq!(fixture.caption_line(cx, &msft), Some(1));

    // The focus on MSFT's caption; its group empties: the focus goes to
    // the line at its place, NVDA's caption, after `ProjectionChange`.
    assert!(update(&fixture, cx, |grid, cx| grid.set_focused_group(&msft, cx)));
    fixture.settle(cx);
    fixture.take_seen();
    let writer = fixture.writer(cx);
    for id in (1..40).step_by(4) {
        writer.remove(id);
    }
    fixture.settle(cx);
    let nvda = group_path(&[(Instrument, "NVDA")]);
    assert_eq!(focus_of(&fixture, cx), (None, Some(nvda.clone())));
    let seen = fixture.take_seen();
    let projection = seen
        .iter()
        .position(|seen| *seen == GroupSeen::Projection)
        .expect("a new order");
    assert_eq!(
        seen[projection + 1..],
        [GroupSeen::Focus(SelectionOrigin::Filtered)]
    );

    // Another grouping: the caption is gone, the focus goes to the line at
    // its place (by side, collapsed: Buy, Sell).
    update(&fixture, cx, |grid, cx| grid.set_group_by([by(Side)], cx));
    fixture.settle(cx);
    assert_eq!(
        focus_of(&fixture, cx),
        (None, Some(group_path(&[(Side, "Sell")])))
    );
    // The last line, then nothing below it: the nearest line above.
    update(&fixture, cx, |grid, cx| grid.set_group_by([by(Trader)], cx));
    fixture.settle(cx);
    assert_eq!(
        focus_of(&fixture, cx),
        (None, Some(group_path(&[(Trader, "Bob")])))
    );
}

#[gpui_kit::test]
fn test_caption_anchor(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Id, Ascending)])
                .with_group_by([by(Instrument)]),
        )
        .with_tuning(unthrottled())
    });
    let msft = group_path(&[(Instrument, "MSFT")]);
    // Lines: AAPL 0, its rows 1–10, MSFT 11. Scrolled to line 8, MSFT's
    // caption is on screen line 3.
    update(&fixture, cx, |grid, cx| grid.scroll_to_row(8, cx));
    fixture.settle(cx);
    assert_eq!(fixture.caption_line(cx, &msft), Some(11));
    fixture.input(cx, |window, cx| {
        press_chevron(window, 3, 0, Modifiers::default(), cx)
    });
    fixture.settle(cx);
    // The toggled caption stays on its screen line; rows below it went.
    assert!(!expanded(&fixture, cx, &msft));
    let start = read(&fixture, cx, |grid| grid.visible_rows().start);
    assert_eq!(fixture.caption_line(cx, &msft), Some(start + 3));
    assert_eq!(start, 8);
    // Expand all: the focused caption keeps its screen line too.
    dispatch(&fixture, cx, &gpui_kit_grid::actions::ExpandAll);
    assert!(expanded(&fixture, cx, &msft));
    let start = read(&fixture, cx, |grid| grid.visible_rows().start);
    assert_eq!(fixture.caption_line(cx, &msft), Some(start + 3));
    assert_eq!(line_after(&fixture, cx, &msft), LineKind::Data(1));
}

#[gpui_kit::test]
fn test_captions_and_reorder_policy(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument().with_aggregates([(Qty, Aggregate::Sum)]))
            .with_tuning(unthrottled())
            .with_reorder_policy(HoldReorder::new(Duration::from_secs(60)))
    });
    let msft = group_path(&[(Instrument, "MSFT")]);
    let sum = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        read(fixture, cx, |grid| {
            group_aggregate(grid.projection(), &aapl(), Qty)
        })
    };
    assert_eq!(sum(&fixture, cx), Some(OwnedValue::Int(3_000)));
    // The pointer rests over a caption: as over the rows.
    fixture.input(cx, |window, cx| {
        move_pointer_to(window, caption_point(0), cx)
    });
    let lines = fixture.lines(cx);
    // The stream moves a trade within its group and changes a quantity.
    write(
        &fixture,
        cx,
        &GroupedRow {
            price: 0.0,
            ..trade(36)
        },
    );
    write(
        &fixture,
        cx,
        &GroupedRow {
            qty: 1_100,
            ..trade(0)
        },
    );
    fixture.settle(cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    assert_eq!(fixture.lines(cx), lines, "the order is held");
    assert_eq!(
        sum(&fixture, cx),
        Some(OwnedValue::Int(4_000)),
        "values are not"
    );
    // An expansion applies at once, held order or not.
    fixture.input(cx, |window, cx| {
        press_chevron(window, 0, 0, Modifiers::default(), cx)
    });
    fixture.settle(cx);
    assert!(read(&fixture, cx, LiveGridState::is_order_held));
    assert_eq!(line_after(&fixture, cx, &aapl()), LineKind::Caption(msft));
}

#[gpui_kit::test]
fn test_caption_menu(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_recording(cx, grouped_rows(40), |grid| {
        grid.with_view(by_instrument().with_aggregates([(Qty, Aggregate::Sum)]))
            .with_tuning(unthrottled())
    });
    let right_press = |fixture: &GroupedFixture<Recording>, cx: &mut TestAppContext, point| {
        fixture.input(cx, |window, cx| {
            press_at(
                window,
                point,
                MouseButton::Right,
                Modifiers::default(),
                1,
                cx,
            )
        });
        fixture.settle(cx);
    };
    let labels = |fixture: &GroupedFixture<Recording>, cx: &mut TestAppContext| {
        let mut items = Vec::new();
        fixture.input(cx, |window, _| {
            let mut found: Vec<_> = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
                .collect();
            found.sort_by(|a, b| {
                a.bounds()
                    .origin
                    .y
                    .partial_cmp(&b.bounds().origin.y)
                    .expect("finite")
            });
            items = found
                .iter()
                .map(|item| (item.label().map(str::to_owned), item.checked()))
                .collect();
        });
        items
    };
    let is_open = |fixture: &GroupedFixture<Recording>, cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| grid.is_menu_open())
    };

    // A right press on the caption's text: the caption, no column.
    right_press(&fixture, cx, caption_point(0));
    assert!(is_open(&fixture, cx));
    assert_eq!(hooks.last_menu().target, caption(aapl()));
    assert_eq!(focus_of(&fixture, cx), (None, Some(aapl())));
    let items = labels(&fixture, cx);
    let names: Vec<_> = items
        .iter()
        .filter_map(|(label, _)| label.clone())
        .collect();
    assert_eq!(
        names,
        [
            "Collapse",
            "Expand all",
            "Collapse all",
            "Captions only",
            "Copy",
            "Copy all"
        ]
    );
    // (gpui-kit's menu items do not expose their check mark to
    // accessibility, so `checked` is not observed here.) The primary
    // command over the caption's ten trades comes first.
    let mut primary = None;
    fixture.input(cx, |window, _| {
        primary = window
            .try_find("live-grid-primary-command")
            .and_then(|label| label.value().map(str::to_owned));
    });
    assert_eq!(primary.as_deref(), Some("Open 10 trades"));

    // A right press on the aggregate cell: the caption with that column.
    fixture.input(cx, |window, cx| window.press("escape", cx));
    fixture.settle(cx);
    assert!(!is_open(&fixture, cx));
    right_press(&fixture, cx, grouped_cell(0, Qty));
    assert_eq!(
        hooks.last_menu().target,
        GridTarget::GroupCaption {
            group: aapl(),
            column: Some(Qty),
        }
    );
    // Collapse: its Action goes to the grid's focus, the caption.
    let mut item = None;
    fixture.input(cx, |window, _| {
        item = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| item.role() == Some(Role::MenuItem) && item.label() == Some("Collapse"))
            .map(|item| item.bounds().center());
    });
    let item = item.expect("the Collapse item");
    left_press(&fixture, cx, item, Modifiers::default(), 1);
    fixture.settle(cx);
    assert!(!is_open(&fixture, cx));
    assert!(!expanded(&fixture, cx, &aapl()));

    // Shift+F10 on the focused caption.
    update_recording(&fixture, cx, |grid, cx| {
        grid.set_expand_depth(ExpandDepth::Captions, cx);
        grid.set_focused_group(&aapl(), cx)
    });
    fixture.settle(cx);
    type_key(&fixture, cx, "shift-f10");
    assert!(is_open(&fixture, cx));
    assert_eq!(hooks.last_menu().target, caption(aapl()));
    assert!(
        labels(&fixture, cx)
            .iter()
            .any(|(label, _)| label.as_deref() == Some("Captions only"))
    );
    // The group leaves the projection while the menu is open: it closes.
    let writer = fixture.writer(cx);
    for id in (0..40).step_by(4) {
        writer.remove(id);
    }
    fixture.settle(cx);
    assert!(!is_open(&fixture, cx));
}

/// The observed accessibility rows of the last frame, header included: the
/// last part of their element ids and whether they are expanded.
fn a11y_rows<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Vec<(ElementId, Option<bool>)> {
    let mut rows = Vec::new();
    fixture.input(cx, |window, _| {
        let mut found: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|element| element.role() == Some(Role::Row))
            .collect();
        found.sort_by(|a, b| {
            a.bounds()
                .origin
                .y
                .partial_cmp(&b.bounds().origin.y)
                .expect("finite")
        });
        rows = found
            .iter()
            .map(|element| {
                (
                    element.path().last().cloned().expect("an id"),
                    element.expanded(),
                )
            })
            .collect();
    });
    rows
}

fn root_role<D: LiveGridDelegate<Row = GroupedRow>>(
    fixture: &GroupedFixture<D>,
    cx: &mut TestAppContext,
) -> Option<Role> {
    let mut role = None;
    fixture.input(cx, |window, _| {
        role = window.try_find("live-grid").and_then(|root| root.role())
    });
    role
}

fn caption_row_id(path: &GroupPath) -> ElementId {
    ElementId::NamedInteger("live-grid-group-row".into(), group_hash(path))
}

fn data_row_id(key: u32) -> ElementId {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = rustc_hash::FxHasher::default();
    key.hash(&mut hasher);
    ElementId::NamedInteger("live-grid-row".into(), hasher.finish())
}

#[gpui_kit::test]
fn test_group_caption_a11y_plan(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(30), |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Id, Ascending)]))
            .with_tuning(unthrottled())
    });
    update(&fixture, cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    // Flat: a grid of data rows.
    assert_eq!(root_role(&fixture, cx), Some(Role::Grid));

    // Grouped by two columns: a tree grid; caption rows with their state,
    // data rows of the shown lines; a collapsed caption brings no rows.
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side)], cx);
        grid.set_expanded(group(aapl_side("Buy")), false, cx);
    });
    fixture.settle(cx);
    assert_eq!(root_role(&fixture, cx), Some(Role::TreeGrid));
    let rows = a11y_rows(&fixture, cx);
    // The header row, then the ten lines on screen: AAPL, AAPL/Buy
    // (collapsed), AAPL/Sell, its four trades, MSFT, MSFT/Buy, trade 1.
    assert_eq!(
        rows[1..],
        [
            (caption_row_id(&aapl()), Some(true)),
            (caption_row_id(&aapl_side("Buy")), Some(false)),
            (caption_row_id(&aapl_side("Sell")), Some(true)),
            (data_row_id(4), None),
            (data_row_id(12), None),
            (data_row_id(20), None),
            (data_row_id(28), None),
            (
                caption_row_id(&group_path(&[(Instrument, "MSFT")])),
                Some(true)
            ),
            (
                caption_row_id(&group_path(&[(Instrument, "MSFT"), (Side, "Buy")])),
                Some(true)
            ),
            (data_row_id(1), None),
        ]
    );
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.a11y_rows(), 10);

    // The focus on a caption: its cell is the real focus element, in the
    // caption row's id scope; after a collapse it is the same element.
    update(&fixture, cx, |grid, cx| {
        grid.set_focused_group(&aapl_side("Sell"), cx)
    });
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
    });
    fixture.settle(cx);
    let focus_bounds = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        let mut bounds = None;
        fixture.input(cx, |window, _| {
            bounds = window
                .within(caption_row_id(&aapl_side("Sell")))
                .try_find(focus_id())
                .map(|element| element.bounds());
        });
        bounds
    };
    let bounds = focus_bounds(&fixture, cx).expect("the caption's focus element");
    // Screen line 2; the caption's text runs over the whole body (no
    // aggregates).
    assert_eq!(bounds.origin.y, gpui_kit::px(ROW * 3.));
    assert_eq!(bounds.origin.x, gpui_kit::px(0.));
    type_key(&fixture, cx, "-");
    assert!(!expanded(&fixture, cx, &aapl_side("Sell")));
    assert_eq!(focus_bounds(&fixture, cx), Some(bounds));
    assert!(a11y_rows(&fixture, cx).contains(&(caption_row_id(&aapl_side("Sell")), Some(false))));

    // A screen reader's Expand on the caption row expands it, as the
    // chevron does (`GroupOrigin::User`).
    take_events(&fixture);
    update(&fixture, cx, |grid, cx| {
        grid.send_a11y_action(&caption_row_id(&aapl_side("Sell")), A11yAction::Expand, cx)
    });
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, &aapl_side("Sell")));
    assert_eq!(
        expansion_and_focus(&take_events(&fixture)),
        [GroupSeen::Expansion(
            Some(group(aapl_side("Sell"))),
            GroupOrigin::User
        )]
    );

    // Ungrouped: a grid again.
    update(&fixture, cx, |grid, cx| grid.set_group_by([], cx));
    fixture.settle(cx);
    assert_eq!(root_role(&fixture, cx), Some(Role::Grid));
}

/// Captions take the selection: their groups' rows join a range (AG Grid's
/// `groupSelectsChildren`).
struct SelectableCaptions;

impl SelectionPolicy<GroupedRow> for SelectableCaptions {
    fn is_selectable(&self, _: &SelectableLine<'_, GroupedRow>) -> bool {
        true
    }

    fn line_scope(
        &self,
        context: &LineScopeContext<'_, GroupedRow>,
    ) -> Option<ScopedSelection<u32>> {
        FocusCaptions.line_scope(context)
    }
}

/// A single press on a caption toggles it (Syncfusion).
struct SinglePress;

impl ExpandGesture for SinglePress {
    fn edit(&self, context: &ExpandGestureContext) -> Option<ExpandEdit> {
        (context.area() != ExpandArea::TreeCell && context.click_count() == 1)
            .then_some(ExpandEdit::Toggle)
    }
}

#[gpui_kit::test]
fn test_live_grid_state_builder(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(8), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(Id, Ascending)])
                .with_group_by([by(Instrument)]),
        )
        .with_tuning(unthrottled())
        .with_expand_depth(ExpandDepth::Levels(1))
        .with_selection_mode(SelectionMode::RowSet)
        .with_selection_policy(SelectableCaptions)
        .with_expand_gesture(SinglePress)
    });
    assert_eq!(
        read(&fixture, cx, |grid| grid.expand_depth()),
        ExpandDepth::Levels(1)
    );
    // A single press on a caption's text toggles it.
    left_press(&fixture, cx, caption_point(0), Modifiers::default(), 1);
    fixture.settle(cx);
    assert!(!expanded(&fixture, cx, &aapl()));
    left_press(&fixture, cx, caption_point(0), Modifiers::default(), 1);
    fixture.settle(cx);
    assert!(expanded(&fixture, cx, &aapl()));
    // A selectable caption brings its group's rows into a range; Cmd/Ctrl+A
    // takes a collapsed group's rows through its caption.
    update(&fixture, cx, |grid, cx| {
        grid.set_expanded(group(group_path(&[(Instrument, "MSFT")])), false, cx)
    });
    fixture.settle(cx);
    type_key(&fixture, cx, "secondary-a");
    assert_eq!(selected_keys(&fixture, cx), [0, 1, 2, 3, 4, 5, 6, 7]);

    // The defaults.
    let defaults = open_grouped(cx, grouped_rows(8), |grid| grid);
    assert_eq!(
        read(&defaults, cx, |grid| grid.expand_depth()),
        ExpandDepth::All
    );
    let _ = StandardExpandGesture;
}

/// Tags each instrument's caption in its own colour, with the instrument
/// as the tag's text, and writes the caption without it.
struct Tagged;

impl LiveGridDelegate for Tagged {
    type Row = GroupedRow;

    fn group_caption(
        &self,
        caption: &GroupCaptionContext<'_, GroupedRow>,
        out: &mut String,
        _: &App,
    ) {
        use std::fmt::Write as _;
        if caption.group().depth() == 0 {
            let _ = write!(out, "{} trades", caption.row_count());
        } else {
            caption.default_caption(out);
        }
    }

    fn group_caption_tag(
        &self,
        caption: &GroupCaptionContext<'_, GroupedRow>,
        out: &mut String,
        _: &App,
    ) -> Option<CaptionTag> {
        (caption.group().depth() == 0).then(|| {
            out.push_str(caption.value_text());
            CaptionTag::new(gpui_kit::red(), gpui_kit::white())
                .with_font_weight(gpui_kit::FontWeight::BOLD)
                .with_min_width(gpui_kit::px(60.))
        })
    }
}

/// A caption tag is a pill before the caption's text, on the top level
/// only here; a screen reader hears it first; it costs no allocation per
/// frame (the hook writes into a reused buffer).
#[gpui_kit::test]
fn test_group_caption_tag(cx: &mut TestAppContext) {
    let fixture = open_grouped_with(cx, grouped_rows(30), Tagged, |grid| {
        grid.with_view(by_instrument_and_side())
            .with_tuning(unthrottled())
    });
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    let tags = fixture
        .grid
        .read_with(cx, |grid, _| grid.painted_caption_tags().to_vec());
    // AAPL on screen; its sides have no tag.
    assert_eq!(tags.len(), 1, "{tags:?}");
    for (bounds, color) in &tags {
        assert_eq!(*color, gpui_kit::red());
        assert_eq!(bounds.size.width, gpui_kit::px(60.));
        assert!(bounds.size.height < gpui_kit::px(ROW));
    }
    // The first caption is on the first line.
    assert!(tags[0].0.origin.y > gpui_kit::px(ROW) && tags[0].0.origin.y < gpui_kit::px(ROW * 2.));
    let cells = fixture
        .grid
        .read_with(cx, |grid, _| grid.a11y_row_cells(&caption_row_id(&aapl())));
    assert!(
        cells
            .first()
            .is_some_and(|(label, _)| label.starts_with("AAPL ") && label.ends_with(" trades")),
        "{cells:?}"
    );
}

/// A new grouping at the top shows its first caption at the top, even with
/// the focused row on the first line; a scrolled body keeps the focused
/// row on its screen line, by key.
#[gpui_kit::test]
fn test_regroup_scroll_anchor(cx: &mut TestAppContext) {
    let fixture = open_grouped(cx, grouped_rows(40), |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Id, Ascending)]))
            .with_tuning(unthrottled())
    });
    let start = |fixture: &GroupedFixture, cx: &mut TestAppContext| {
        read(fixture, cx, |grid| grid.visible_rows().start)
    };
    let line_of = |fixture: &GroupedFixture, cx: &mut TestAppContext, key: u32| {
        fixture
            .lines(cx)
            .iter()
            .position(|line| *line == LineKind::Data(key))
    };
    assert_eq!(fixture.lines(cx)[0], LineKind::Data(0));
    update(&fixture, cx, |grid, cx| {
        assert!(grid.set_focused_cell(&0, None, cx));
    });
    fixture.settle(cx);
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument)], cx)
    });
    fixture.settle(cx);
    assert_eq!(start(&fixture, cx), 0);
    assert_eq!(fixture.caption_line(cx, &aapl()), Some(0));

    // Scrolled: the focused row stays on screen line 2 under the new
    // grouping.
    update(&fixture, cx, |grid, cx| grid.scroll_to_row(10, cx));
    fixture.settle(cx);
    let LineKind::Data(key) = fixture.lines(cx)[12].clone() else {
        panic!("line 12 is a row");
    };
    update(&fixture, cx, |grid, cx| {
        assert!(grid.set_focused_cell(&key, None, cx));
    });
    fixture.settle(cx);
    assert_eq!(start(&fixture, cx), 10);
    update(&fixture, cx, |grid, cx| {
        grid.set_group_by([by(Instrument), by(Side)], cx)
    });
    fixture.settle(cx);
    let start = start(&fixture, cx);
    assert!(start > 0);
    assert_eq!(line_of(&fixture, cx, key), Some(start + 2));
}
