//! Headless UI tests of the grid view (`.agents/COMPONENT_TEST_RULES.md`,
//! "UI tests"): identity by key under removals, re-sorting and scrolling;
//! reorder policies, reloads, the store clock, sizes and task runners; pinned
//! rows, progress, `flush` and `ProjectionChange`; the accessibility tree of
//! the frame. The test window keeps accessibility off, so the tree is checked
//! through `set_a11y_forced`, the counters and the observed elements' roles,
//! labels and bounds. The frame's mechanisms: change paint, trend arrows, bold
//! text, cell starts. Cell hosts in the body's rows, the edit builders and the
//! key context of the focused cell. The latency of a write from the producer to
//! the grid.

mod common;

use std::{
    cell::{Cell, RefCell},
    hash::{Hash as _, Hasher as _},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::component::{ActiveTheme as _, IconName, IconNamed as _, Sizable as _, Size};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Div, ElementId, Entity, Focusable as _,
    FontWeight, Hsla, InputEvent as _, InteractiveElement as _, IntoElement, Modifiers,
    MouseButton, MouseMoveEvent, ParentElement as _, Pixels, Render, Role, ScrollDelta,
    ScrollWheelEvent, Stateful, Styled as _, TestAppContext, TouchPhase, Window, WindowBounds,
    WindowOptions, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    AfterCommit, CellChange, CellContent, CellEditor, CellStyle, CellStyleContext, CellTooltip,
    CellTooltipContext, ChangeDirection, ChangePaint, ChangePainter, ChangePalette, Clock,
    ColumnIx, DirectionalChanges, EditCommitKey, EditGesture, EditGestureContext, EditorOpen,
    EditorStart, FlushOutcome, GridColumn, GridColumns as _, GridProgress, GridSchema, GridStore,
    GridTuning, HeaderLabel, HoldReorder, InlineRunner, LiveGrid, LiveGridDelegate, LiveGridEvent,
    LiveGridState, ManualReorder, Op, PinnedEdge, ReloadPolicy, RowChange, SelectionOrigin,
    SortDirection, StandardEditPolicy, StoreOptions, UniformChanges, ViewSpec,
    actions::{ActivateCell, CELL_CONTENT_KEY, KEY_CONTEXT, ShowContextMenu},
};

use crate::common::{
    Fixture, GroupedColumn, GroupedRow, Item, ItemColumn, Items, LineKind, ORDER_COLUMNS, Order,
    OrderColumn, Orders, PausableRunner, ROW, WINDOW_HEIGHT, a11y_row_tops, by_price, cell_host_id,
    grouped_cell, grouped_rows, move_pointer_to, open, open_configured, open_grouped_with,
    open_orders, open_orders_in, open_with, order_cell, order_row_id, orders, press_at,
    unthrottled,
};

#[gpui_kit::test]
fn test_removed_row_leaves_screen_before_compaction(cx: &mut TestAppContext) {
    // A long rebuild interval holds the compaction back, so the test sees
    // what the screen does while the projection still has the removed row.
    let fixture = open(cx, 30, ViewSpec::new(), Duration::from_secs(10));
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(2), cx));

    fixture.writer(cx).remove(2);
    fixture.frame(cx, Duration::from_millis(20));

    let mut expected: Vec<u32> = (0..11).collect();
    expected.retain(|id| *id != 2);
    assert_eq!(fixture.visible_keys(cx), expected, "gone on the next frame");
    fixture.grid.read_with(cx, |grid, _| {
        assert_eq!(grid.projection().len(), 30, "compaction not run yet");
        assert_eq!(grid.stats().tombstones(), 1);
        assert!(grid.selection().is_empty());
    });
    assert!(fixture.events.borrow().iter().any(|event| matches!(
        event,
        LiveGridEvent::SelectionChange {
            selection,
            origin: SelectionOrigin::DataRemoved
        } if selection.is_empty()
    )));

    // Once the interval passes, a compaction (no sort) drops the row.
    fixture.frame(cx, Duration::from_secs(10));
    fixture.settle(cx);
    fixture.grid.read_with(cx, |grid, _| {
        assert_eq!(grid.projection().len(), 29);
        assert_eq!(grid.stats().compactions(), 1);
        assert_eq!(grid.stats().tombstones(), 0);
    });
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());

    // Removing a row the projection does not show needs no work at all.
    let filtered = open(
        cx,
        30,
        ViewSpec::new().with_filter(gpui_kit_grid::ColumnSet::first(2), |item: &Item| {
            item.id % 2 == 0
        }),
        Duration::ZERO,
    );
    let before = filtered.grid.read_with(cx, |grid, _| grid.stats());
    filtered.writer(cx).remove(3);
    filtered.settle(cx);
    let after = filtered.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(after.rebuilds(), before.rebuilds());
    assert_eq!(after.compactions(), before.compactions());
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds
    );
}

#[gpui_kit::test]
fn test_replacement_without_changed_column_repaints(cx: &mut TestAppContext) {
    // Sorted by price, so a changed price would rebuild the order.
    let fixture = open(cx, 30, by_price(SortDirection::Ascending), Duration::ZERO);
    let notified = Rc::new(Cell::new(0));
    cx.update(|cx| {
        let notified = notified.clone();
        cx.observe(&fixture.grid, move |_, _| notified.set(notified.get() + 1))
            .detach();
    });
    // Ingest and pull, without drawing a frame.
    let pull = |cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
    };
    let read = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            (grid.snapshot().clone(), grid.stats().rebuilds())
        })
    };
    pull(cx);
    assert_eq!(notified.get(), 0, "idle");
    let (before, rebuilds) = read(cx);
    fixture.events.borrow_mut().clear();

    // A painted row, replaced with the same values: the view takes the new
    // row and repaints, without a highlight and without a rebuild.
    let replacement = Arc::new(Item { id: 3, price: 30 });
    fixture.writer(cx).upsert(replacement.clone());
    pull(cx);
    assert!(notified.get() > 0, "a painted row repaints");
    fixture.settle(cx);
    let (snapshot, after) = read(cx);
    assert_eq!(snapshot.version(), before.version() + 1);
    assert!(Arc::ptr_eq(snapshot.get(&3).expect("row 3"), &replacement));
    assert_eq!(snapshot.stamps(snapshot.row_id(&3).expect("row 3")), None);
    assert_eq!(after, rebuilds, "no rebuild");
    assert!(
        fixture
            .events
            .borrow()
            .iter()
            .all(|event| !matches!(event, LiveGridEvent::ProjectionChange { .. }))
    );

    // A row off screen is published too, but repaints nothing.
    notified.set(0);
    fixture.writer(cx).upsert(Item { id: 25, price: 250 });
    pull(cx);
    assert_eq!(notified.get(), 0, "row 25 is not painted");
    assert_eq!(read(cx).0.version(), before.version() + 2);

    // The same `Arc` again publishes nothing.
    fixture.writer(cx).upsert(replacement);
    pull(cx);
    assert_eq!(notified.get(), 0);
    let (snapshot, after) = read(cx);
    assert_eq!(snapshot.version(), before.version() + 2);
    assert_eq!(after, rebuilds);
}

#[gpui_kit::test]
fn test_click_selects_by_key_and_selection_survives_resort(cx: &mut TestAppContext) {
    let fixture = open(
        cx,
        30,
        ViewSpec::new().with_sort([(ItemColumn::Price, SortDirection::Descending)]),
        Duration::ZERO,
    );
    assert_eq!(fixture.visible_keys(cx)[..3], [29, 28, 27]);

    // Third line of the body (below the header row).
    cx.update_window(fixture.window, |_, window, cx| {
        window.click_at("live-grid", point(px(50.), px(ROW * 3. + 5.)), cx);
    })
    .expect("window is open");
    assert_eq!(fixture.selected_keys(cx), [27]);

    // Reverse the order: the selection follows the key, not the line, and
    // the focused row stays on screen (the scroll anchor keeps its line as
    // far as the end of the rows allows).
    let writer = fixture.writer(cx);
    for id in 0..30u32 {
        writer.upsert(Item {
            id,
            price: -i64::from(id) * 10,
        });
    }
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).collect::<Vec<_>>());
    assert_eq!(fixture.selected_keys(cx), [27]);

    // The selected row is found by key in O(1) and scrolled into view.
    let found = fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_key(&27, cx));
    assert!(found);
    fixture.settle(cx);
    assert!(fixture.visible_keys(cx).contains(&27));
    assert!(
        !fixture
            .grid
            .update(cx, |grid, cx| grid.scroll_to_key(&999, cx))
    );

    // Producers read current rows by key from the published snapshot.
    let snapshot = fixture.writer(cx).snapshot();
    assert_eq!(snapshot.get(&27).map(|item| item.price), Some(-270));
    assert!(snapshot.get(&999).is_none());
}

#[gpui_kit::test]
fn test_subscription_closes_when_store_is_dropped(cx: &mut TestAppContext) {
    let store = cx.update(|cx| gpui_kit_grid::GridStore::<Item>::new(StoreOptions::default(), cx));
    let writer = store.writer();
    let subscription = store.subscribe();
    writer.upsert(Item { id: 1, price: 10 });
    cx.executor().advance_clock(Duration::from_millis(10));
    cx.run_until_parked();
    assert!(smol::block_on(subscription.changed()));
    assert_eq!(
        subscription.take().0.get(&1).map(|item| item.price),
        Some(10)
    );

    drop(store);
    cx.run_until_parked();
    assert!(writer.is_closed());
    assert!(
        !smol::block_on(subscription.changed()),
        "no snapshots will come"
    );
}

#[gpui_kit::test]
fn test_hold_reorder_keeps_order_under_pointer(cx: &mut TestAppContext) {
    const HOLD: Duration = Duration::from_millis(500);
    let fixture = open_configured(cx, 30, Some(Size::XSmall), |grid| {
        grid.with_view(by_price(SortDirection::Descending))
            .with_tuning(unthrottled())
            .with_reorder_policy(HoldReorder::new(HOLD))
    });
    let shown: Vec<u32> = (20..30).rev().collect();
    assert_eq!(
        fixture.visible_keys(cx),
        shown,
        "first data set applies at once"
    );

    // Streaming data reverses the order under the pointer: it is held.
    fixture.hover(cx, "live-grid");
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), shown);
    assert!(fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true]);

    // At most `HOLD`, even with the pointer still inside.
    fixture.frame(cx, HOLD);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert!(!fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false]);

    // A user action applies at once with the pointer inside.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_view(by_price(SortDirection::Ascending), cx)
    });
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), shown);
    assert!(!fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false]);

    // Held again; moving the pointer off the rows applies it before `HOLD`.
    fixture.write_prices(cx, 30, 1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), shown);
    assert_eq!(fixture.held_events(), [true, false, true]);
    fixture.hover(cx, "away");
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert!(!fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true, false, true, false]);
}

#[gpui_kit::test]
fn test_manual_reorder_waits_for_apply_held_order(cx: &mut TestAppContext) {
    let runner = PausableRunner::default();
    let fixture = open_configured(cx, 30, Some(Size::XSmall), {
        let runner = runner.clone();
        |grid| {
            grid.with_view(by_price(SortDirection::Descending))
                .with_tuning(unthrottled())
                .with_reorder_policy(ManualReorder)
                .with_task_runner(runner)
        }
    });
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());

    // No deadline: the reversed order waits however long it takes.
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    fixture.frame(cx, Duration::from_secs(60));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    assert!(fixture.is_order_held(cx));
    assert_eq!(fixture.held_events(), [true]);

    // A removal compacts the shown order; the newer order stays held.
    fixture.writer(cx).remove(29);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (19..29).rev().collect::<Vec<_>>());
    fixture.grid.read_with(cx, |grid, _| {
        assert_eq!(grid.projection().len(), 29);
        assert_eq!(grid.stats().compactions(), 1);
        assert!(grid.is_order_held());
    });
    assert_eq!(fixture.held_events(), [true]);

    // Applied while the compaction of another removal is in flight: that
    // compaction of the old order must not replace the applied one.
    runner.pause();
    fixture.writer(cx).remove(28);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (18..28).rev().collect::<Vec<_>>());
    fixture
        .grid
        .update(cx, |grid, cx| grid.apply_held_order(cx));
    runner.resume();
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    fixture.grid.read_with(cx, |grid, _| {
        assert!(!grid.is_order_held());
        assert_eq!(grid.projection().len(), 28, "removed rows compacted away");
    });
    assert_eq!(fixture.held_events(), [true, false]);
}

#[gpui_kit::test]
fn test_reload_replays_live_writes_over_snapshot(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, ViewSpec::new(), Duration::ZERO);
    let writer = fixture.writer(cx);
    let price = |cx: &mut TestAppContext, id: u32| {
        fixture
            .writer(cx)
            .snapshot()
            .get(&id)
            .map(|item| item.price)
    };

    // Writes while the reload runs are buffered, from every writer.
    let reload = writer.begin_reload();
    writer.upsert(Item { id: 1, price: 111 });
    fixture.writer(cx).remove(2);
    fixture.settle(cx);
    assert_eq!(price(cx, 1), Some(10));
    assert_eq!(price(cx, 2), Some(20));

    // The snapshot is older than the live writes: row 1 has its old price
    // and row 2 is still there. Row 3 is gone, row 100 is new.
    reload.finish(
        (0..30)
            .filter(|id| *id != 3)
            .chain([100])
            .map(|id| Arc::new(Item { id, price: 5 })),
    );
    fixture.settle(cx);
    assert_eq!(price(cx, 1), Some(111), "the live write wins");
    assert_eq!(price(cx, 2), None, "the live removal wins");
    assert_eq!(price(cx, 3), None);
    assert_eq!(price(cx, 0), Some(5));
    assert_eq!(price(cx, 100), Some(5));
    assert_eq!(
        fixture.visible_keys(cx),
        [0, 1, 4, 5, 6, 7, 8, 9, 10, 11],
        "a reload is a new data set: shown at once"
    );
    fixture
        .grid
        .read_with(cx, |grid, _| assert_eq!(grid.projection().len(), 29));

    // Dropping the guard cancels the reload and applies the buffered writes.
    let reload = writer.begin_reload();
    writer.upsert(Item { id: 0, price: 7 });
    fixture.settle(cx);
    assert_eq!(price(cx, 0), Some(5));
    drop(reload);
    fixture.settle(cx);
    assert_eq!(price(cx, 0), Some(7));

    // A newer reload supersedes an older one, keeping its buffered writes.
    let older = writer.begin_reload();
    writer.upsert(Item { id: 4, price: 44 });
    let newer = writer.begin_reload();
    older.finish([Arc::new(Item { id: 200, price: 1 })]);
    fixture.settle(cx);
    assert_eq!(price(cx, 200), None, "the older finish is ignored");
    assert_eq!(price(cx, 4), Some(5), "still buffered");
    newer.finish([4, 300].map(|id| Arc::new(Item { id, price: 3 })));
    fixture.settle(cx);
    assert_eq!(price(cx, 200), None);
    assert_eq!(price(cx, 300), Some(3));
    assert_eq!(price(cx, 4), Some(44));
    assert_eq!(fixture.writer(cx).snapshot().len(), 2);

    // Writes apply directly again.
    writer.upsert(Item { id: 300, price: 30 });
    fixture.settle(cx);
    assert_eq!(price(cx, 300), Some(30));
}

/// A clock that never moves.
struct FixedClock(Instant);

impl Clock for FixedClock {
    fn now(&self) -> Instant {
        self.0
    }
}

/// Keeps the reloaded snapshot and drops live writes made meanwhile.
struct SnapshotWins;

impl ReloadPolicy<Item> for SnapshotWins {
    fn resolve(&mut self, snapshot: Vec<Arc<Item>>, _live: Vec<Op<Item>>) -> Vec<Op<Item>> {
        vec![Op::ReplaceAll(snapshot)]
    }
}

#[gpui_kit::test]
fn test_grid_store_builder(cx: &mut TestAppContext) {
    let settle = |cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
    };

    // The default store reads the executor clock.
    let store = cx.update(|cx| GridStore::<Item>::new(StoreOptions::default(), cx));
    assert_eq!(store.now(), cx.executor().now());

    let at = cx.executor().now() + Duration::from_secs(3600);
    let store = cx.update(|cx| {
        GridStore::<Item>::builder(StoreOptions::default())
            .with_clock(Arc::new(FixedClock(at)))
            .with_reload_policy(SnapshotWins)
            .build(cx)
    });
    assert_eq!(store.now(), at);
    assert_eq!(store.snapshot().epoch(), at, "stamps measured from it");
    settle(cx);
    assert_eq!(store.now(), at);

    let writer = store.writer();
    writer.upsert(Item { id: 1, price: 10 });
    settle(cx);
    let reload = writer.begin_reload();
    writer.upsert(Item { id: 1, price: 11 });
    writer.upsert(Item { id: 2, price: 20 });
    reload.finish([Arc::new(Item { id: 1, price: 9 })]);
    settle(cx);
    let snapshot = store.snapshot();
    assert_eq!(snapshot.get(&1).map(|item| item.price), Some(9));
    assert!(
        snapshot.get(&2).is_none(),
        "live writes dropped by the policy"
    );
}

#[gpui_kit::test]
fn test_grid_size_and_task_runner(cx: &mut TestAppContext) {
    // The header is one row tall; the body paints every row it intersects.
    let painted = |row_height: f32| ((WINDOW_HEIGHT - row_height) / row_height).ceil() as usize;
    let mut counts = Vec::new();
    for (size, row_height) in [
        (Some(Size::XSmall), 26.),
        (Some(Size::Small), 30.),
        (Some(Size::Medium), 32.),
        (Some(Size::Large), 40.),
        (Some(Size::Size(px(50.))), 50.),
        // The state view embedded without `LiveGrid` uses the default size.
        (None, 32.),
    ] {
        let fixture = open_configured(cx, 30, size, |grid| grid);
        let count = fixture.visible_keys(cx).len();
        assert_eq!(count, painted(row_height), "{size:?}");
        counts.push(count);
    }
    assert_eq!(counts[..4], [10, 9, 8, 7]);

    // Jobs run inline on the UI thread: rebuilds and compactions alike.
    let fixture = open_configured(cx, 30, Some(Size::XSmall), |grid| {
        grid.with_view(by_price(SortDirection::Descending))
            .with_tuning(unthrottled())
            .with_task_runner(InlineRunner)
    });
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    fixture.writer(cx).remove(0);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (1..11).collect::<Vec<_>>());
    fixture.grid.read_with(cx, |grid, _| {
        assert_eq!(grid.projection().len(), 29);
        assert!(grid.stats().rebuilds() >= 2);
        assert_eq!(grid.stats().compactions(), 1);
    });
}

#[gpui_kit::test]
fn test_wheel_line_is_one_row_at_every_size(cx: &mut TestAppContext) {
    // Eight wheel clicks of three lines, as X11 and Windows send them, under
    // a top band: whatever the row height, they scroll 24 rows and stop on a
    // whole row, so the middle of the first row under the band holds the
    // first visible key. A fixed line height of the `xsmall` row stopped
    // 30 px rows 24 px into a row, and that point hit the next row.
    for (size, row_height) in [
        (Size::XSmall, 26.),
        (Size::Small, 30.),
        (Size::Large, 40.),
        (Size::Size(px(24.)), 24.),
    ] {
        let fixture = open_configured(cx, 100, Some(size), |grid| grid);
        fixture
            .writer(cx)
            .set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 1000, price: 0 })]);
        fixture.settle(cx);
        // The header, then the top band, then the data rows.
        let rows_top = row_height * 2.;
        cx.update_window(fixture.window, |_, window, cx| {
            for _ in 0..8 {
                let wheel = ScrollWheelEvent {
                    position: point(px(50.), px(rows_top + row_height * 2.5)),
                    delta: ScrollDelta::Lines(point(0., -3.)),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                };
                window.dispatch_event(wheel.to_platform_input(), cx);
            }
        })
        .expect("window is open");
        fixture.settle(cx);
        let visible = fixture.visible_keys(cx);
        assert_eq!(visible.first(), Some(&24), "{size:?}");
        let press = |y: f32, cx: &mut TestAppContext| {
            cx.update_window(fixture.window, |_, window, cx| {
                let at = point(px(50.), px(y));
                press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
            })
            .expect("window is open");
        };
        press(rows_top + row_height / 2., cx);
        assert_eq!(fixture.selected_keys(cx), [visible[0]], "{size:?}");

        // Half a row of pixels (a trackpad): the first visible key is the
        // row cut by the band, and a press hits the row painted under it.
        cx.update_window(fixture.window, |_, window, cx| {
            let wheel = ScrollWheelEvent {
                position: point(px(50.), px(rows_top + row_height * 2.5)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-row_height / 2.))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            };
            window.dispatch_event(wheel.to_platform_input(), cx);
        })
        .expect("window is open");
        fixture.settle(cx);
        let visible = fixture.visible_keys(cx);
        assert_eq!(visible.first(), Some(&24), "{size:?}");
        press(rows_top + row_height / 2. + 1., cx);
        assert_eq!(fixture.selected_keys(cx), [visible[1]], "{size:?}");
        press(rows_top + 1., cx);
        assert_eq!(fixture.selected_keys(cx), [visible[0]], "{size:?}");
    }
}

type Events = Rc<RefCell<Vec<LiveGridEvent<u32, ItemColumn>>>>;

/// The `ProjectionChange` events so far: row count and store version.
fn projection_changes(events: &Events) -> Vec<(usize, u64)> {
    events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::ProjectionChange {
                row_count,
                store_version,
            } => Some((*row_count, *store_version)),
            _ => None,
        })
        .collect()
}

/// A `flush` in flight: once it resolves, its outcome and the number of
/// `ProjectionChange` events that had come by then.
struct PendingFlush(Rc<RefCell<Option<(Option<FlushOutcome>, usize)>>>);

impl PendingFlush {
    fn start<D: LiveGridDelegate<Row = Item>>(
        cx: &mut TestAppContext,
        grid: &Entity<LiveGridState<D>>,
        events: &Events,
    ) -> Self {
        let task = grid.update(cx, |grid, cx| grid.flush(cx));
        let slot = Rc::new(RefCell::new(None));
        let (done, events) = (slot.clone(), events.clone());
        cx.update(|cx| {
            cx.spawn(async move |_| {
                let outcome = task.await;
                let shown = projection_changes(&events).len();
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

/// Items `0..count` with `price = id * 10`.
fn items(count: u32) -> impl Iterator<Item = Arc<Item>> {
    (0..count).map(|id| {
        Arc::new(Item {
            id,
            price: i64::from(id) * 10,
        })
    })
}

fn pinned_keys<D: LiveGridDelegate<Row = Item>>(
    fixture: &Fixture<D>,
    cx: &mut TestAppContext,
) -> (Vec<u32>, Vec<u32>) {
    fixture.grid.read_with(cx, |grid, _| {
        (
            grid.pinned_keys(PinnedEdge::Top),
            grid.pinned_keys(PinnedEdge::Bottom),
        )
    })
}

#[gpui_kit::test]
fn test_pinned_rows_stay_in_place(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, ViewSpec::new(), Duration::ZERO);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    // The top band reuses key 5 of the data rows: pinned keys are a space
    // of their own. The bottom band is a total.
    let writer = fixture.writer(cx);
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 5, price: -1 })]);
    writer.set_pinned_rows(
        PinnedEdge::Bottom,
        [Arc::new(Item {
            id: 1000,
            price: 4350,
        })],
    );
    fixture.settle(cx);
    assert_eq!(pinned_keys(&fixture, cx), (vec![5], vec![1000]));
    // The body is the header's row shorter: 26 px of top band, eight data
    // rows instead of ten, 26 px of bottom band.
    assert_eq!(fixture.visible_keys(cx), (0..8).collect::<Vec<_>>());
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.visible_rows()),
        0..8
    );

    // Scrolled to the end and back by the wheel over a band: the bands
    // stay, the data rows scroll.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(100, cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (22..30).collect::<Vec<_>>());
    assert_eq!(pinned_keys(&fixture, cx), (vec![5], vec![1000]));
    cx.update_window(fixture.window, |_, window, cx| {
        let wheel = ScrollWheelEvent {
            position: point(px(50.), px(WINDOW_HEIGHT - ROW / 2.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(ROW * 2.))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        };
        window.dispatch_event(wheel.to_platform_input(), cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..28).collect::<Vec<_>>());
    assert_eq!(pinned_keys(&fixture, cx), (vec![5], vec![1000]));

    // A row scrolled into view lands above the bottom band, not under it.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(0, cx));
    assert!(
        fixture
            .grid
            .update(cx, |grid, cx| grid.scroll_to_key(&29, cx))
    );
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx).last(), Some(&29));
    assert_eq!(fixture.visible_keys(cx).len(), 8);

    // A press on a data row selects it; a press or a double click on a
    // band changes neither the selection nor the focus.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(0, cx));
    fixture.settle(cx);
    // The middle of a window line: 0 is the header, 1 the top band, 2 to 9
    // the data rows, 10 the bottom band.
    let at = |line: f32| point(px(50.), px(ROW * line + ROW / 2.));
    let focused = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.focused_cell().and_then(|cell| cell.row().copied())
        })
    };
    cx.update_window(fixture.window, |_, window, cx| {
        press_at(
            window,
            at(2.),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    })
    .expect("window is open");
    assert_eq!(fixture.selected_keys(cx), [0]);
    for band in [at(1.), at(10.)] {
        cx.update_window(fixture.window, |_, window, cx| {
            press_at(window, band, MouseButton::Left, Modifiers::default(), 1, cx);
            press_at(window, band, MouseButton::Left, Modifiers::default(), 2, cx);
        })
        .expect("window is open");
        fixture.settle(cx);
        assert_eq!(fixture.selected_keys(cx), [0]);
        assert_eq!(focused(cx), Some(0));
    }
    assert!(
        !fixture
            .events
            .borrow()
            .iter()
            .any(|event| matches!(event, LiveGridEvent::Confirm { .. }))
    );

    // Page Down moves by the eight rows between the bands.
    cx.update_window(fixture.window, |_, window, cx| window.press("pagedown", cx))
        .expect("window is open");
    fixture.settle(cx);
    assert_eq!(focused(cx), Some(8));
    assert_eq!(fixture.visible_keys(cx), (8..16).collect::<Vec<_>>());

    // Bands longer than the body: the top band first, then the bottom
    // band, and no room for data rows. A band cut short loses the rows next
    // to the data rows, so the bottom band shows its last rows, at the
    // body's edge.
    writer.set_pinned_rows(
        PinnedEdge::Top,
        (100..108).map(|id| Arc::new(Item { id, price: 0 })),
    );
    writer.set_pinned_rows(
        PinnedEdge::Bottom,
        (200..204).map(|id| Arc::new(Item { id, price: 0 })),
    );
    fixture.settle(cx);
    assert_eq!(
        pinned_keys(&fixture, cx),
        ((100..108).collect(), (202..204).collect())
    );
    assert!(fixture.visible_keys(cx).is_empty());
    // Cleared bands give the rows their room back.
    writer.set_pinned_rows(PinnedEdge::Top, []);
    writer.set_pinned_rows(PinnedEdge::Bottom, []);
    fixture.settle(cx);
    assert_eq!(pinned_keys(&fixture, cx), (vec![], vec![]));
    assert_eq!(fixture.visible_keys(cx).len(), 10);
}

/// Whether the window shows an element with this id.
fn shows(fixture: &Fixture, cx: &mut TestAppContext, id: &'static str) -> bool {
    cx.update_window(fixture.window, |_, window, _| window.try_find(id).is_some())
        .expect("window is open")
}

#[gpui_kit::test]
fn test_progress_from_writer_until_caught_up(cx: &mut TestAppContext) {
    let runner = PausableRunner::default();
    let fixture = open_configured(cx, 0, Some(Size::XSmall), {
        let runner = runner.clone();
        |grid| grid.with_tuning(unthrottled()).with_task_runner(runner)
    });
    let progress = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.progress().cloned())
    };
    assert_eq!(progress(cx), None);
    assert!(!shows(&fixture, cx, "live-grid-progress"));

    // A first load: the progress shows over the empty grid, with its label.
    runner.pause();
    let writer = fixture.writer(cx);
    let loading = GridProgress::indeterminate().with_label("Loading items");
    writer.set_progress(Some(loading.clone()));
    writer.replace_all(items(30));
    fixture.settle(cx);
    assert_eq!(progress(cx), Some(loading.clone()));
    assert!(shows(&fixture, cx, "live-grid-progress"));
    assert!(shows(&fixture, cx, "live-grid-progress-label"));
    fixture.events.borrow_mut().clear();

    // The producer clears it after the data, but the rebuild still waits:
    // the store has no progress any more, and the grid still shows it.
    writer.set_progress(None);
    fixture.settle(cx);
    assert_eq!(writer.snapshot().progress(), None);
    assert_eq!(progress(cx), Some(loading));
    assert!(shows(&fixture, cx, "live-grid-progress"));
    assert!(fixture.visible_keys(cx).is_empty());

    // The new rows show, and the progress leaves with the same frame.
    runner.resume();
    fixture.settle(cx);
    assert_eq!(
        projection_changes(&fixture.events),
        [(30, writer.snapshot().version() - 1)]
    );
    assert_eq!(progress(cx), None);
    assert!(!shows(&fixture, cx, "live-grid-progress"));
    assert_eq!(fixture.visible_keys(cx).len(), 10);

    // Progress cleared after writes that need no rebuild leaves at once.
    let half = GridProgress::new(0.5);
    writer.set_progress(Some(half.clone()));
    fixture.settle(cx);
    assert_eq!(progress(cx), Some(half));
    assert!(shows(&fixture, cx, "live-grid-progress"));
    assert!(
        !shows(&fixture, cx, "live-grid-progress-label"),
        "no label over data"
    );
    runner.pause();
    writer.set_progress(None);
    fixture.settle(cx);
    assert_eq!(progress(cx), None);
    runner.resume();
}

#[gpui_kit::test]
fn test_view_progress_overrides_store(cx: &mut TestAppContext) {
    let fixture = open(cx, 0, ViewSpec::new(), Duration::ZERO);
    let progress = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.progress().cloned())
    };
    let writer = fixture.writer(cx);
    let store = GridProgress::new(0.25).with_label("Store work");
    let view = GridProgress::indeterminate().with_label("View work");

    writer.set_progress(Some(store.clone()));
    fixture.settle(cx);
    assert_eq!(progress(cx), Some(store.clone()));
    assert!(
        shows(&fixture, cx, "live-grid-progress-label"),
        "empty grid"
    );

    // The view's progress wins at once; cleared, the store's shows again.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_progress(Some(view.clone()), cx));
    assert_eq!(progress(cx), Some(view));
    fixture.settle(cx);
    assert!(shows(&fixture, cx, "live-grid-progress"));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_progress(None, cx));
    assert_eq!(progress(cx), Some(store));

    // With data shown there is no label, and the rows stay readable.
    writer.replace_all(items(30));
    fixture.settle(cx);
    assert!(shows(&fixture, cx, "live-grid-progress"));
    assert!(!shows(&fixture, cx, "live-grid-progress-label"));
    assert_eq!(fixture.visible_keys(cx).len(), 10);

    writer.set_progress(None);
    fixture.settle(cx);
    assert_eq!(progress(cx), None);
    assert!(!shows(&fixture, cx, "live-grid-progress"));
}

#[gpui_kit::test]
fn test_flush_and_projection_change(cx: &mut TestAppContext) {
    // A long throttle: only the flush starts data-driven rebuilds at once.
    let fixture = open_configured(cx, 0, Some(Size::XSmall), |grid| {
        grid.with_view(by_price(SortDirection::Descending))
            .with_tuning(GridTuning::default().with_min_rebuild_interval(Duration::from_secs(10)))
            .with_reorder_policy(ManualReorder)
    });
    fixture.events.borrow_mut().clear();
    let writer = fixture.writer(cx);

    // A new data set: shown under any policy; `ProjectionChange` first.
    writer.replace_all(items(30));
    let flush = PendingFlush::start(cx, &fixture.grid, &fixture.events);
    fixture.settle(cx);
    let version = writer.snapshot().version();
    let (outcome, shown) = flush.result().expect("resolved");
    let outcome = outcome.expect("the grid is alive");
    assert_eq!(outcome.store_version(), version);
    assert_eq!(outcome.row_count(), 30);
    assert!(!outcome.is_order_held());
    assert_eq!(shown, 1, "ProjectionChange came before the flush resolved");
    assert_eq!(projection_changes(&fixture.events), [(30, version)]);
    assert_eq!(fixture.visible_keys(cx)[0], 29);

    // The stream reverses the order: the flush resolves without waiting for
    // the throttle, with the order held by `ManualReorder`.
    fixture.write_prices(cx, 30, -1);
    let flush = PendingFlush::start(cx, &fixture.grid, &fixture.events);
    fixture.settle(cx);
    let (outcome, _) = flush.result().expect("resolved within the throttle");
    let outcome = outcome.expect("the grid is alive");
    assert_eq!(outcome.store_version(), writer.snapshot().version());
    assert!(outcome.is_order_held());
    assert_eq!(outcome.row_count(), 30);
    assert_eq!(
        projection_changes(&fixture.events).len(),
        1,
        "held: no event"
    );
    assert_eq!(fixture.visible_keys(cx)[0], 29);
    // Applied: exactly one more event, for the one order shown.
    fixture
        .grid
        .update(cx, |grid, cx| grid.apply_held_order(cx));
    fixture.settle(cx);
    assert_eq!(projection_changes(&fixture.events).len(), 2);
    assert_eq!(fixture.visible_keys(cx)[0], 0);

    // A flush with nothing written resolves with the current version.
    let flush = PendingFlush::start(cx, &fixture.grid, &fixture.events);
    fixture.settle(cx);
    let (outcome, _) = flush.result().expect("resolved");
    assert_eq!(
        outcome.map(|outcome| outcome.store_version()),
        Some(writer.snapshot().version())
    );
    assert_eq!(projection_changes(&fixture.events).len(), 2);

    // A grid that is never painted still resolves its flush.
    let schema = || {
        GridSchema::new()
            .column(GridColumn::new(ItemColumn::Id, "Id"))
            .column(GridColumn::new(ItemColumn::Price, "Price"))
    };
    let hidden = cx.new(|cx| {
        LiveGridState::new_standalone(schema(), Items, StoreOptions::default(), cx)
            .with_view(by_price(SortDirection::Ascending))
    });
    hidden
        .read_with(cx, |grid, _| grid.writer())
        .replace_all(items(5));
    let none: Events = Rc::default();
    let flush = PendingFlush::start(cx, &hidden, &none);
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
    }
    let (outcome, _) = flush.result().expect("resolved without a render");
    assert_eq!(outcome.map(|outcome| outcome.row_count()), Some(5));
    hidden.read_with(cx, |grid, _| {
        assert_eq!(grid.projection().len(), 5);
        assert_eq!(grid.stats().frames(), 0);
    });

    // A grid dropped before its flush resolves: `None`.
    let dropped =
        cx.new(|cx| LiveGridState::new_standalone(schema(), Items, StoreOptions::default(), cx));
    dropped
        .read_with(cx, |grid, _| grid.writer())
        .replace_all(items(5));
    let flush = PendingFlush::start(cx, &dropped, &none);
    drop(dropped);
    cx.run_until_parked();
    assert_eq!(flush.result(), Some((None, 0)));
}

#[gpui_kit::test]
fn test_flush_under_live_stream(cx: &mut TestAppContext) {
    const INTERVAL: Duration = Duration::from_millis(100);
    const FRAME: Duration = Duration::from_millis(16);
    let fixture = open_configured(cx, 30, Some(Size::XSmall), |grid| {
        grid.with_view(by_price(SortDirection::Descending))
            .with_tuning(GridTuning::default().with_min_rebuild_interval(INTERVAL))
    });
    let writer = fixture.writer(cx);
    // Every frame writes a price, and the sort is by price: the work never
    // ends.
    let mut step = 0u32;
    let mut write_frame = |cx: &mut TestAppContext| {
        step += 1;
        writer.upsert(Item {
            id: step % 30,
            price: i64::from(step * 7_919 % 1_000),
        });
        fixture.frame(cx, FRAME);
    };
    for _ in 0..20 {
        write_frame(cx);
    }

    let started = cx.executor().now();
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());
    let flush = PendingFlush::start(cx, &fixture.grid, &fixture.events);
    let written = writer.snapshot().version();
    let mut frames = 0;
    while flush.result().is_none() {
        write_frame(cx);
        frames += 1;
        assert!(frames < 30, "the flush never resolved");
    }
    let waited = cx.executor().now() - started;
    assert!(waited <= INTERVAL + FRAME, "waited {waited:?}");
    let (outcome, _) = flush.result().expect("resolved");
    let outcome = outcome.expect("the grid is alive");
    assert!(outcome.store_version() >= written);
    assert!(!outcome.is_order_held());
    fixture.grid.read_with(cx, |grid, _| {
        assert!(grid.projection().store_version() >= outcome.store_version());
        assert!(grid.stats().rebuilds() > rebuilds);
    });

    // After the flush the throttle holds again: about one rebuild per
    // interval, not one per frame.
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());
    for _ in 0..50 {
        write_frame(cx);
    }
    let more = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds())
        - rebuilds;
    assert!((4..=10).contains(&more), "{more} rebuilds in 800 ms");
}

#[gpui_kit::test]
fn test_copy_all_with_pinned_rows(cx: &mut TestAppContext) {
    let fixture = open(cx, 3, ViewSpec::new(), Duration::ZERO);
    let writer = fixture.writer(cx);
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 100, price: 1 })]);
    writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(Item { id: 200, price: 2 })]);
    fixture.settle(cx);
    let copied = |cx: &mut TestAppContext| {
        assert!(fixture.grid.update(cx, |grid, cx| grid.copy_all(cx)));
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("copied");
        text.lines().map(str::to_owned).collect::<Vec<_>>()
    };
    // The top band before the data rows, the bottom band after them.
    assert_eq!(
        copied(cx),
        ["Id\tPrice", "100\t1", "0\t0", "1\t10", "2\t20", "200\t2"]
    );
    // With no data rows, "Copy all" still copies the bands.
    writer.clear();
    fixture.settle(cx);
    assert_eq!(copied(cx), ["Id\tPrice", "100\t1", "200\t2"]);
}

/// The element id of a data row's accessibility row: `NamedInteger(
/// "live-grid-row", key hash)`, the key hashed with `FxHasher`.
fn a11y_row_id(key: u32) -> ElementId {
    let mut hasher = rustc_hash::FxHasher::default();
    key.hash(&mut hasher);
    ElementId::NamedInteger("live-grid-row".into(), hasher.finish())
}

/// The element id of the focused cell in `column`, in its row's id scope.
fn a11y_focus_id(column: &str) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name("live-grid-focus".into())),
        column.to_owned().into(),
    )
}

/// The observed accessibility rows of the last frame, header included: the
/// last part of their element ids and their selection.
fn a11y_rows<D: LiveGridDelegate<Row = Item>>(
    fixture: &Fixture<D>,
    cx: &mut TestAppContext,
) -> Vec<(ElementId, Option<bool>)> {
    cx.update_window(fixture.window, |_, window, _| {
        gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|element| element.role() == Some(Role::Row))
            .map(|element| {
                (
                    element.path().last().cloned().expect("an id"),
                    element.selected(),
                )
            })
            .collect()
    })
    .expect("window is open")
}

fn find<D: LiveGridDelegate<Row = Item>>(
    fixture: &Fixture<D>,
    cx: &mut TestAppContext,
    id: ElementId,
) -> Option<gpui_kit::test::ElementSnapshot> {
    cx.update_window(fixture.window, |_, window, _| window.try_find(id))
        .expect("window is open")
}

/// Gives the grid the keyboard focus.
fn focus_grid<D: LiveGridDelegate<Row = Item>>(fixture: &Fixture<D>, cx: &mut TestAppContext) {
    let grid = fixture.grid.clone();
    cx.update_window(fixture.window, |_, window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .expect("window is open");
}

/// A body cell's bounds on screen line `line` (no pinned rows), the header
/// one `xsmall` row and both columns 100 px wide.
fn cell_bounds(line: usize, column: ItemColumn) -> Bounds<Pixels> {
    let x = match column {
        ItemColumn::Id => 0.,
        ItemColumn::Price => 100.,
    };
    Bounds::new(
        point(px(x), px(ROW * (line as f32 + 1.))),
        size(px(100.), px(ROW)),
    )
}

/// Records the cells the delegate's `tooltip` is asked for, and gives each
/// a text tooltip.
struct TooltipCalls(Rc<RefCell<Vec<(u32, ItemColumn)>>>);

impl LiveGridDelegate for TooltipCalls {
    type Row = Item;

    fn tooltip(&self, cell: &CellTooltipContext<'_, Item>, _: &App) -> Option<CellTooltip> {
        self.0.borrow_mut().push((cell.row().id, cell.column()));
        Some(CellTooltip::text(format!("Last trade {}", cell.row().id)))
    }
}

#[gpui_kit::test]
fn test_a11y_costs_nothing_when_off(cx: &mut TestAppContext) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let fixture = open_with(
        cx,
        30,
        Some(Size::XSmall),
        TooltipCalls(calls.clone()),
        |grid| grid.with_tuning(unthrottled()),
    );
    focus_grid(&fixture, cx);
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&3, Some(ItemColumn::Price), cx));
    });
    let frames =
        |cx: &mut TestAppContext| fixture.grid.read_with(cx, |grid, _| grid.stats().frames());

    // Accessibility off: no plan, no row elements, no tooltip for the
    // focused cell, frame after frame of a live stream.
    for sign in [-1, 1, -1, 1] {
        fixture.write_prices(cx, 30, sign);
        fixture.frame(cx, Duration::from_millis(20));
        fixture.grid.read_with(cx, |grid, _| {
            let stats = grid.stats();
            assert_eq!(stats.a11y_rows(), 0);
            assert_eq!(stats.a11y_cells(), 0);
            assert_eq!(stats.last_a11y_plan(), Duration::ZERO);
            assert_eq!(stats.visible_cells(), 20);
        });
    }
    assert!(calls.borrow().is_empty());
    assert_eq!(a11y_rows(&fixture, cx).len(), 1, "the header row only");
    assert!(find(&fixture, cx, a11y_focus_id("price")).is_none());
    // The root and the header carry their roles all the same: fields of
    // divs that are there anyway.
    let root = find(&fixture, cx, "live-grid".into()).expect("the root");
    assert_eq!(root.role(), Some(Role::Grid));
    assert_eq!(root.label(), None, "no name without `accessibility_label`");
    let header = find(&fixture, cx, "live-grid-header".into()).expect("the header");
    assert_eq!(header.role(), Some(Role::Row));
    let title = cx
        .update_window(fixture.window, |_, window, _| {
            window.within("live-grid-header").find("price")
        })
        .expect("window is open");
    assert_eq!(title.role(), Some(Role::ColumnHeader));
    assert_eq!(title.label(), Some("Price"));

    // Planned: the focused cell's tooltip is asked once a frame.
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    calls.borrow_mut().clear();
    let before = frames(cx);
    fixture.frame(cx, Duration::from_millis(20));
    let drawn = (frames(cx) - before) as usize;
    assert!(drawn >= 1);
    assert_eq!(*calls.borrow(), vec![(3, ItemColumn::Price); drawn]);

    // Hovered and focused: still one call a frame; another cell hovered: two.
    let hover = |cx: &mut TestAppContext, line: usize| {
        cx.update_window(fixture.window, |_, window, cx| {
            let event = MouseMoveEvent {
                position: cell_bounds(line, ItemColumn::Price).center(),
                pressed_button: None,
                modifiers: Modifiers::default(),
            };
            window.dispatch_event(event.to_platform_input(), cx);
        })
        .expect("window is open");
        fixture.settle(cx);
        calls.borrow_mut().clear();
        let before = frames(cx);
        fixture.frame(cx, Duration::from_millis(20));
        (frames(cx) - before) as usize
    };
    let drawn = hover(cx, 3);
    assert_eq!(*calls.borrow(), vec![(3, ItemColumn::Price); drawn]);
    let drawn = hover(cx, 5);
    assert_eq!(calls.borrow().len(), 2 * drawn);
    assert!(calls.borrow().contains(&(5, ItemColumn::Price)));
    assert!(calls.borrow().contains(&(3, ItemColumn::Price)));
}

#[gpui_kit::test]
fn test_a11y_planned_rows_follow_frame(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, ViewSpec::new(), Duration::ZERO);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    fixture.settle(cx);
    let counts = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            let stats = grid.stats();
            (stats.a11y_rows(), stats.a11y_cells(), stats.visible_cells())
        })
    };
    // Ten data rows of two cells; no focused cell: every cell is synthetic.
    assert_eq!(counts(cx), (10, 20, 20));
    // One element per row: `Row`, by key, at its line across the body; in
    // the row modes an unselected row says so (`Some(false)`).
    let rows = a11y_rows(&fixture, cx);
    assert_eq!(rows.len(), 11, "the header and ten data rows");
    for key in 0..10 {
        assert!(rows.contains(&(a11y_row_id(key), Some(false))), "{key}");
    }
    let row = find(&fixture, cx, a11y_row_id(4)).expect("row 4");
    assert_eq!(
        row.bounds(),
        Bounds::new(point(px(0.), px(ROW * 5.)), size(px(400.), px(ROW)))
    );

    // Pinned rows: rows of their own band, numbered around the data rows;
    // they are never selected. Key 5 is in both spaces: two rows.
    let writer = fixture.writer(cx);
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id: 5, price: -1 })]);
    writer.set_pinned_rows(
        PinnedEdge::Bottom,
        [Arc::new(Item {
            id: 1000,
            price: 4350,
        })],
    );
    fixture.settle(cx);
    assert_eq!(counts(cx), (10, 20, 20), "eight data rows and two bands");
    let rows = a11y_rows(&fixture, cx);
    let band = |name: &str| {
        rows.iter()
            .filter(|(id, selected)| {
                matches!(id, ElementId::NamedInteger(band, _) if band == name) && selected.is_none()
            })
            .count()
    };
    assert_eq!(band("live-grid-pinned-top"), 1);
    assert_eq!(band("live-grid-pinned-bottom"), 1);
    assert!(rows.contains(&(a11y_row_id(5), Some(false))));

    // The selection, by key.
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(6), cx));
    fixture.settle(cx);
    let rows = a11y_rows(&fixture, cx);
    assert!(rows.contains(&(a11y_row_id(6), Some(true))));
    assert!(rows.contains(&(a11y_row_id(7), Some(false))));

    // Scrolled: the rows on screen, nothing else.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(10, cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (10..18).collect::<Vec<_>>());
    assert!(find(&fixture, cx, a11y_row_id(10)).is_some());
    assert!(find(&fixture, cx, a11y_row_id(6)).is_none());

    // The focused cell is apart from the synthetic cells.
    focus_grid(&fixture, cx);
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&12, Some(ItemColumn::Price), cx));
    });
    fixture.settle(cx);
    assert_eq!(counts(cx), (10, 19, 20));

    // A painted row removed: its element is gone with the next frame, the
    // next row takes its line.
    writer.remove(14);
    fixture.frame(cx, Duration::from_millis(20));
    assert!(find(&fixture, cx, a11y_row_id(14)).is_none());
    assert!(find(&fixture, cx, a11y_row_id(18)).is_some());
    assert_eq!(counts(cx), (10, 19, 20));

    // Off again: nothing planned.
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(false));
    fixture.settle(cx);
    assert_eq!(counts(cx), (0, 0, 20));
    assert_eq!(a11y_rows(&fixture, cx).len(), 1);
}

#[gpui_kit::test]
fn test_a11y_focus_cell_element(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, by_price(SortDirection::Ascending), Duration::ZERO);
    fixture
        .grid
        .update(cx, |grid, _| grid.set_a11y_forced(true));
    focus_grid(&fixture, cx);
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&2, Some(ItemColumn::Price), cx));
    });
    fixture.settle(cx);

    // A `GridCell` over the cell, in the id scope of its row.
    let cell = find(&fixture, cx, a11y_focus_id("price")).expect("the focused cell");
    assert_eq!(cell.role(), Some(Role::GridCell));
    assert_eq!(cell.bounds(), cell_bounds(2, ItemColumn::Price));
    assert!(cell.path().contains(&a11y_row_id(2)));
    assert!(find(&fixture, cx, a11y_focus_id("id")).is_none());

    // Re-sorted by the stream: the same element, wherever the row is now.
    // (The focused row keeps its line: the body scrolls with it.)
    fixture.writer(cx).upsert(Item { id: 2, price: 45 });
    fixture.settle(cx);
    let line = fixture
        .visible_keys(cx)
        .iter()
        .position(|key| *key == 2)
        .expect("row 2 is on screen");
    let first = fixture
        .grid
        .read_with(cx, |grid, _| grid.visible_rows().start);
    assert_eq!(first + line, 4, "between prices 40 and 50");
    let cell = find(&fixture, cx, a11y_focus_id("price")).expect("the focused cell");
    assert_eq!(cell.bounds(), cell_bounds(line, ItemColumn::Price));
    assert!(cell.path().contains(&a11y_row_id(2)));

    // The menu has the focus: the grid claims no active descendant.
    cx.update_window(fixture.window, |_, window, cx| {
        window.dispatch_action(Box::new(ShowContextMenu), cx)
    })
    .expect("window is open");
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()));
    assert!(find(&fixture, cx, a11y_focus_id("price")).is_none());
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().a11y_cells()),
        20,
        "every painted cell synthetic"
    );
    cx.update_window(fixture.window, |_, window, cx| window.press("escape", cx))
        .expect("window is open");
    fixture.settle(cx);
    assert!(find(&fixture, cx, a11y_focus_id("price")).is_some());

    // Scrolled off screen: no element; the focus stays on the grid node.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(20, cx));
    fixture.settle(cx);
    assert!(!fixture.visible_keys(cx).contains(&2));
    assert!(find(&fixture, cx, a11y_focus_id("price")).is_none());
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().a11y_cells()),
        20
    );
}

#[gpui_kit::test]
fn test_live_grid_accessibility_label(cx: &mut TestAppContext) {
    struct Labelled {
        grid: Entity<LiveGridState<Items>>,
    }

    impl Render for Labelled {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                LiveGrid::new(&self.grid)
                    .accessibility_label("Trades")
                    .xsmall(),
            )
        }
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let (window, _) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(400.), px(WINDOW_HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| Labelled {
                        grid: cx.new(|cx| {
                            LiveGridState::new_standalone(
                                GridSchema::new()
                                    .column(GridColumn::new(ItemColumn::Id, "Id"))
                                    .column(GridColumn::new(ItemColumn::Price, "Price")),
                                Items,
                                StoreOptions::default(),
                                cx,
                            )
                        }),
                    })
                },
            )
        })
        .expect("open window");
    cx.run_until_parked();
    let root = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("live-grid")
        })
        .expect("window is open");
    assert_eq!(root.role(), Some(Role::Grid));
    assert_eq!(root.label(), Some("Trades"));
}

/// A cell's default style as `cell_style` saw it: the key, the column, the
/// change, the background and the trend.
type StyleEntry = (
    u32,
    ItemColumn,
    Option<CellChange>,
    Option<Hsla>,
    Option<ChangeDirection>,
);

/// Records the style each cell comes to `cell_style` with.
#[derive(Clone, Default)]
struct StyleRecorder(Rc<RefCell<Vec<StyleEntry>>>);

impl StyleRecorder {
    /// The last style recorded for a cell.
    fn last(&self, key: u32, column: ItemColumn) -> Option<StyleEntry> {
        self.0
            .borrow()
            .iter()
            .rev()
            .find(|entry| (entry.0, entry.1) == (key, column))
            .copied()
    }
}

impl LiveGridDelegate for StyleRecorder {
    type Row = Item;

    fn cell_style(&self, cell: &CellStyleContext<'_, Item>, style: &mut CellStyle, _: &App) {
        self.0.borrow_mut().push((
            cell.row().id,
            cell.column(),
            cell.change(),
            style.background(),
            style.trend(),
        ));
    }
}

/// Paints every change as a rise, with an arrow.
struct AlwaysUp;

impl ChangePainter for AlwaysUp {
    fn paint(&self, _: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        style.set_background(palette.up());
        style.set_trend(Some(ChangeDirection::Up));
    }
}

/// An update of row 1's price and an inserted row 7, after the load.
fn write_changes(fixture: &Fixture<StyleRecorder>, cx: &mut TestAppContext) {
    let writer = fixture.writer(cx);
    writer.upsert(Item { id: 1, price: 999 });
    writer.upsert(Item { id: 7, price: 70 });
    fixture.settle(cx);
}

#[gpui_kit::test]
fn test_uniform_changes_paint_stage1_colors(cx: &mut TestAppContext) {
    let recorder = StyleRecorder::default();
    let fixture = open_with(cx, 5, Some(Size::XSmall), recorder.clone(), |grid| {
        grid.with_tuning(unthrottled())
    });
    assert!(fixture.grid.read_with(cx, |grid, _| matches!(
        grid.change_paint(),
        ChangePaint::Uniform(_)
    )));
    write_changes(&fixture, cx);
    let (success, warning) = cx.update(|cx| (cx.theme().success, cx.theme().warning));

    // As in stage 1: background only, at 0.45 of the fading intensity; an
    // insertion in `success`, an update in `warning`; no arrow.
    let (_, _, change, background, trend) = recorder.last(1, ItemColumn::Price).unwrap();
    let change = change.expect("the price changed");
    assert_eq!(change.change(), RowChange::Updated);
    assert_eq!(
        change.direction(),
        Some(ChangeDirection::Up),
        "recorded, not painted"
    );
    assert_eq!(background, Some(warning.opacity(0.45 * change.intensity())));
    assert_eq!(trend, None);
    let (_, _, change, background, _) = recorder.last(7, ItemColumn::Id).unwrap();
    let change = change.expect("the row is new");
    assert_eq!(change.change(), RowChange::Inserted);
    assert_eq!(background, Some(success.opacity(0.45 * change.intensity())));
    // Every recorded cell: a background exactly when it changed.
    for (key, column, change, background, trend) in recorder.0.borrow().iter().copied() {
        let expected = change.map(|change| {
            let color = match change.change() {
                RowChange::Inserted => success,
                _ => warning,
            };
            color.opacity(0.45 * change.intensity())
        });
        assert_eq!(background, expected, "{key} {column:?}");
        assert_eq!(trend, None);
    }
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.stats().icons()),
        0
    );
}

#[gpui_kit::test]
fn test_directional_changes(cx: &mut TestAppContext) {
    let recorder = StyleRecorder::default();
    let fixture = open_with(cx, 5, Some(Size::XSmall), recorder.clone(), |grid| {
        grid.with_tuning(unthrottled())
            .with_change_paint(ChangePaint::Directional(DirectionalChanges))
    });
    write_changes(&fixture, cx);
    let success = cx.update(|cx| cx.theme().success);

    // The store records the direction: row 1's price rose, in `success`
    // with an arrow; the inserted row 7 has none.
    let (_, _, change, background, trend) = recorder.last(1, ItemColumn::Price).unwrap();
    let change = change.unwrap();
    assert_eq!(change.direction(), Some(ChangeDirection::Up));
    assert_eq!(background, Some(success.opacity(0.45 * change.intensity())));
    assert_eq!(trend, Some(ChangeDirection::Up));
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.icons(), 1);

    // A painter that sets a trend gets an arrow per hot cell; switching
    // painters repaints without rebuilding.
    let up = cx.update(|cx| cx.theme().success);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::Custom(Arc::new(AlwaysUp)), cx)
    });
    fixture.frame(cx, Duration::ZERO);
    let after = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert!(after.frames() > stats.frames(), "repainted");
    assert_eq!(after.rebuilds(), stats.rebuilds(), "not rebuilt");
    let (_, _, _, background, trend) = recorder.last(1, ItemColumn::Price).unwrap();
    assert_eq!((background, trend), (Some(up), Some(ChangeDirection::Up)));
    // Row 1's price and both cells of row 7 are hot.
    assert_eq!(after.icons(), 3);

    fixture.grid.update(cx, |grid, cx| {
        grid.set_change_paint(ChangePaint::default(), cx)
    });
    fixture.frame(cx, Duration::ZERO);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.stats().icons()),
        0
    );
    assert_eq!(recorder.last(1, ItemColumn::Price).unwrap().4, None);
}

/// Paints the changes of one column only, as [`UniformChanges`].
struct OneColumn(ColumnIx);

impl ChangePainter for OneColumn {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        if change.column_ix() == Some(self.0) {
            UniformChanges.paint(change, palette, style);
        }
    }
}

/// A painter knows the cell's column: one that paints the id column only
/// leaves the updated price alone and paints the inserted row's id.
#[gpui_kit::test]
fn test_change_painter_knows_the_column(cx: &mut TestAppContext) {
    let recorder = StyleRecorder::default();
    let fixture = open_with(cx, 5, Some(Size::XSmall), recorder.clone(), |grid| {
        grid.with_tuning(unthrottled())
            .with_change_paint(ChangePaint::Custom(Arc::new(OneColumn(
                ItemColumn::Id.ix(),
            ))))
    });
    write_changes(&fixture, cx);
    let success = cx.update(|cx| cx.theme().success);
    let (_, _, change, background, _) = recorder.last(1, ItemColumn::Price).unwrap();
    assert_eq!(change.unwrap().column_ix(), Some(ItemColumn::Price.ix()));
    assert_eq!(background, None);
    let (_, _, change, background, _) = recorder.last(7, ItemColumn::Id).unwrap();
    let change = change.unwrap();
    assert_eq!(change.column_ix(), Some(ItemColumn::Id.ix()));
    assert_eq!(background, Some(success.opacity(0.45 * change.intensity())));
    let (_, _, change, background, _) = recorder.last(7, ItemColumn::Price).unwrap();
    assert_eq!(change.unwrap().column_ix(), Some(ItemColumn::Price.ix()));
    assert_eq!(background, None);
}

/// Sets one column's text bold and records the truncation of hovered cells.
#[derive(Clone, Default)]
struct GroupedRecorder {
    bold: Option<GroupedColumn>,
    tooltips: Rc<RefCell<Vec<(u32, GroupedColumn, bool)>>>,
}

impl LiveGridDelegate for GroupedRecorder {
    type Row = GroupedRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, GroupedRow>, style: &mut CellStyle, _: &App) {
        if Some(cell.column()) == self.bold {
            style.set_font_weight(FontWeight::BOLD);
        }
    }

    fn tooltip(&self, cell: &CellTooltipContext<'_, GroupedRow>, _: &App) -> Option<CellTooltip> {
        self.tooltips
            .borrow_mut()
            .push((cell.row().id, cell.column(), cell.is_truncated()));
        cell.default_tooltip()
    }
}

#[gpui_kit::test]
fn test_bold_text_cached_apart(cx: &mut TestAppContext) {
    let recorder = GroupedRecorder {
        bold: Some(GroupedColumn::Instrument),
        ..GroupedRecorder::default()
    };
    let fixture = open_grouped_with(cx, grouped_rows(24), recorder, |grid| grid);
    assert_eq!(fixture.visible_keys(cx).len(), 10);
    let stats = fixture.grid.read_with(cx, |grid, _| grid.text_stats());
    // The four instruments in bold, in a cache of their own; sides and
    // traders in the grid's font.
    assert_eq!(stats.bold_entries(), 4);
    assert!(stats.entries() > stats.bold_entries());

    // A repeated frame shapes nothing in either cache.
    fixture.frame(cx, Duration::from_millis(20));
    let again = fixture.grid.read_with(cx, |grid, _| grid.text_stats());
    assert_eq!(again.misses(), stats.misses());
    assert!(again.hits() > stats.hits());
    assert_eq!(again.bold_entries(), 4);
}

#[gpui_kit::test]
fn test_cell_lead_indents_text(cx: &mut TestAppContext) {
    let recorder = GroupedRecorder::default();
    let fixture = open_grouped_with(cx, grouped_rows(24), recorder.clone(), |grid| grid);
    assert_eq!(
        fixture.lines(cx)[..3],
        [LineKind::Data(0), LineKind::Data(1), LineKind::Data(2)]
    );
    // "AAPL" fits its 80 px column.
    let cell = grouped_cell(0, GroupedColumn::Instrument);
    fixture.input(cx, |window, cx| move_pointer_to(window, cell, cx));
    fixture.settle(cx);
    assert_eq!(
        recorder.tooltips.borrow().last(),
        Some(&(0, GroupedColumn::Instrument, false))
    );

    // An indent leaves it too little room: the text is cut and the cell
    // gets the automatic tooltip. The icon sits in the indent of every
    // painted row.
    fixture.grid.update(cx, |grid, _| {
        grid.set_test_cell_lead(
            GroupedColumn::Instrument,
            px(60.),
            Some(IconName::ChevronRight.path()),
        )
    });
    fixture.frame(cx, Duration::from_millis(20));
    assert_eq!(
        recorder.tooltips.borrow().last(),
        Some(&(0, GroupedColumn::Instrument, true))
    );
    let painted = fixture.visible_keys(cx).len();
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.stats().icons()),
        painted
    );
}

/// The latency of a write while the stream is busy: right after a publish,
/// the store waits out its publish interval, then the grid takes the
/// snapshot when its own pull interval allows. Both are 8 ms by default, so
/// a write reaches the screen within 16 ms.
#[gpui_kit::test]
fn test_write_reaches_grid_within_publish_and_pull_interval(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, ViewSpec::new(), Duration::ZERO);
    let writer = fixture.writer(cx);
    let price = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.snapshot().get(&3).map(|row| row.price))
    };
    let publish = StoreOptions::default().publish_interval();
    let pull = Duration::from_millis(8);

    // The first write after a quiet spell is published and taken at once.
    writer.upsert(Item { id: 3, price: 1 });
    cx.run_until_parked();
    assert_eq!(price(cx), Some(1));

    // The next one waits for the store's pause, then for the grid's.
    writer.upsert(Item { id: 3, price: 2 });
    let step = Duration::from_millis(1);
    let mut waited = Duration::ZERO;
    while price(cx) != Some(2) {
        assert!(waited < publish + pull, "not in the grid after {waited:?}");
        cx.executor().advance_clock(step);
        cx.run_until_parked();
        waited += step;
    }
    assert!(waited >= publish, "published before the pause: {waited:?}");
}

// Cell hosts, the edit seams and the key context of the focused cell.

/// A cell host of the last frame of `window`: its row's element id, its
/// own id, its bounds and whether any of it is painted.
#[derive(Debug)]
struct HostSnapshot {
    row: ElementId,
    id: ElementId,
    bounds: Bounds<Pixels>,
    visible: bool,
}

/// The cell hosts of the last frame of `window`, top to bottom.
fn cell_hosts(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<HostSnapshot> {
    let host_name = |id: &ElementId| {
        matches!(id, ElementId::NamedChild(name, _)
            if matches!(&**name, ElementId::Name(name) if name.as_ref() == "live-grid-cell"))
    };
    cx.update_window(window, |_, window, _| {
        let mut hosts: Vec<HostSnapshot> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|element| element.path().last().is_some_and(host_name))
            .map(|element| {
                let path = element.path();
                HostSnapshot {
                    row: path[path.len() - 2].clone(),
                    id: path[path.len() - 1].clone(),
                    bounds: element.bounds(),
                    visible: element.visible(),
                }
            })
            .collect();
        hosts.sort_by(|a, b| {
            a.bounds
                .origin
                .y
                .partial_cmp(&b.bounds.origin.y)
                .expect("finite")
        });
        hosts
    })
    .expect("window is open")
}

/// The order columns with `Id` frozen at the left edge, and a note wide
/// enough for the rows to scroll sideways.
fn frozen_order_schema() -> GridSchema<OrderColumn> {
    ORDER_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            let width = if column == OrderColumn::Note {
                400.
            } else {
                width
            };
            schema.column(GridColumn::new(column, column.id()).with_width(px(width)))
        })
        .with_frozen(1)
}

/// A widget host as the grid makes one for a cell's element: the body does
/// not see the presses on it; the wheel goes through.
fn widget_host(host: Stateful<Div>) -> Stateful<Div> {
    host.block_mouse_except_scroll()
}

#[gpui_kit::test]
fn test_cell_host_mechanism(cx: &mut TestAppContext) {
    let fixture = open_orders_in(cx, frozen_order_schema(), orders(60), Orders, |grid| grid);
    // No host, no row element.
    assert!(cell_hosts(cx, fixture.window).is_empty());
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.cell_hosts(), 0);
    assert_eq!(stats.last_hosts_prepaint(), Duration::ZERO);

    fixture.grid.update(cx, |grid, _| {
        grid.set_test_cell_host(Some(OrderColumn::Price), widget_host)
    });
    fixture.settle(cx);
    // One host per painted row, in the row's element, by key and column.
    let check = |cx: &mut TestAppContext| {
        let visible = fixture.visible_keys(cx);
        let hosts = cell_hosts(cx, fixture.window);
        assert_eq!(hosts.len(), visible.len());
        for (line, (key, host)) in visible.iter().zip(&hosts).enumerate() {
            assert_eq!(host.row, order_row_id(*key), "line {line}");
            assert_eq!(host.id, cell_host_id("live-grid-cell", OrderColumn::Price));
            assert!(host.bounds.contains(&order_cell(line, OrderColumn::Price)));
            assert_eq!(host.bounds.size, size(px(80.), px(ROW)));
            assert!(host.visible);
        }
        let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
        assert_eq!(stats.cell_hosts(), visible.len());
        visible
    };
    let ascending = check(cx);
    assert_eq!(ascending[..3], [1, 2, 3]);

    // A re-sort: every host keeps its id with its key, at the key's new line.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_sort([(OrderColumn::Price, SortDirection::Descending)], cx)
    });
    fixture.settle(cx);
    let descending = check(cx);
    assert_eq!(descending[..3], [60, 59, 58]);

    // Scrolled sideways under the frozen `Id` column: the host's left part
    // is clipped by its zone, so a press there reaches the frozen cell,
    // while a press on its shown part does not reach the body.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(230.), cx));
    fixture.settle(cx);
    let hosts = cell_hosts(cx, fixture.window);
    let host = &hosts[2];
    assert_eq!(host.bounds.origin.x, px(30.));
    let y = host.bounds.center().y;
    let press = |x: f32, cx: &mut TestAppContext| {
        cx.update_window(fixture.window, |_, window, cx| {
            let at = point(px(x), y);
            press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
        })
        .expect("window is open");
    };
    press(40., cx);
    let selected = fixture.selected_keys(cx);
    assert_eq!(selected, [descending[2]], "the frozen cell takes the press");
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(None, cx));
    press(90., cx);
    let selected = fixture.selected_keys(cx);
    assert!(
        selected.is_empty(),
        "the host takes the press: {selected:?}"
    );

    // Half a row scrolled under the header: the row's element is clipped to
    // the rows, so a press on the header above the row's host sorts.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_scroll_x(px(0.), cx);
        grid.scroll_by(point(px(0.), px(ROW / 2.)), cx);
    });
    fixture.settle(cx);
    let hosts = cell_hosts(cx, fixture.window);
    assert!(hosts[0].bounds.origin.y < px(ROW), "{:?}", hosts[0].bounds);
    fixture.take_events();
    cx.update_window(fixture.window, |_, window, cx| {
        let at = point(order_cell(0, OrderColumn::Price).x, px(ROW - 4.));
        press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
    })
    .expect("window is open");
    assert!(
        fixture
            .take_events()
            .iter()
            .any(|event| matches!(event, LiveGridEvent::SortChange { .. })),
        "the header takes the press"
    );

    // Taken away, the hosts leave with the next frame.
    fixture
        .grid
        .update(cx, |grid, _| grid.set_test_cell_host(None, widget_host));
    fixture.settle(cx);
    assert!(cell_hosts(cx, fixture.window).is_empty());
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.cell_hosts(), 0);
}

#[gpui_kit::test]
fn test_cell_host_blocks_body_press(cx: &mut TestAppContext) {
    let fixture = open_orders(cx, orders(60), |grid| grid);
    fixture.grid.update(cx, |grid, _| {
        grid.set_test_cell_host(Some(OrderColumn::Price), widget_host)
    });
    fixture.settle(cx);
    fixture.take_events();

    // A press on a host: no selection, no focus, no click.
    fixture.press_cell(cx, 2, OrderColumn::Price, 1);
    fixture.press_cell(cx, 3, OrderColumn::Price, 2);
    assert!(fixture.take_events().is_empty());
    let selected = fixture.selected_keys(cx);
    let focused = fixture
        .grid
        .read_with(cx, |grid, _| grid.focused_cell().cloned());
    assert!(selected.is_empty() && focused.is_none());

    // A press on a cell of the row without a host, as before.
    fixture.press_cell(cx, 2, OrderColumn::Quantity, 1);
    let events = fixture.take_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LiveGridEvent::Click { .. }))
    );
    let selected = fixture.selected_keys(cx);
    assert_eq!(selected, [3]);

    // The wheel over a host scrolls the rows.
    cx.update_window(fixture.window, |_, window, cx| {
        let wheel = ScrollWheelEvent {
            position: order_cell(4, OrderColumn::Price),
            delta: ScrollDelta::Pixels(point(px(0.), px(-ROW * 2.))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        };
        window.dispatch_event(wheel.to_platform_input(), cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx).first(), Some(&3));
}

#[gpui_kit::test]
fn test_cell_host_outline_on_top(cx: &mut TestAppContext) {
    let fixture = open_orders(cx, orders(60), |grid| grid);
    let focus_with_keys = |cx: &mut TestAppContext| {
        cx.update_window(fixture.window, |_, window, cx| {
            let handle = fixture.grid.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .expect("window is open");
        fixture.grid.update(cx, |grid, cx| {
            grid.set_focused_cell(&4, Some(OrderColumn::Price), cx);
        });
        // A key makes the focus visible: the outline shows.
        fixture.press_key(cx, "up");
        fixture.settle(cx);
        fixture.grid.read_with(cx, |grid, _| {
            (
                grid.focused_cell().and_then(|cell| cell.row().copied()),
                grid.last_paint_order().to_vec(),
            )
        })
    };
    // Without hosts: no row element; the outline after the regions.
    let (focused, order) = focus_with_keys(cx);
    assert_eq!(focused, Some(3));
    assert_eq!(order, ["regions", "outlines"]);

    // With hosts, the row elements paint between the regions and the
    // outline: a host does not cover the focused cell's outline.
    fixture.grid.update(cx, |grid, _| {
        grid.set_test_cell_host(Some(OrderColumn::Price), widget_host)
    });
    let (focused, order) = focus_with_keys(cx);
    assert_eq!(focused, Some(3));
    assert_eq!(order, ["regions", "rows", "outlines"]);
}

#[gpui_kit::test]
fn test_cell_host_with_a11y(cx: &mut TestAppContext) {
    let fixture = open_orders(cx, orders(60), |grid| grid);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_a11y_forced(true);
        grid.set_focused_cell(&2, Some(OrderColumn::Price), cx);
    });
    cx.update_window(fixture.window, |_, window, cx| {
        let handle = fixture.grid.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .expect("window is open");
    fixture.settle(cx);
    let plain_rows = a11y_row_tops(cx, fixture.window);
    let plain_cells = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().a11y_cells());
    let focus_id = cell_host_id("live-grid-focus", OrderColumn::Price);
    let has_focus_element = |cx: &mut TestAppContext| {
        cx.update_window(fixture.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|element| element.path().last() == Some(&focus_id))
        })
        .expect("window is open")
    };
    assert!(has_focus_element(cx));

    // With hosts, each one is a child of its row's accessibility element:
    // one element per row, the same id; the plan is the same.
    fixture.grid.update(cx, |grid, _| {
        grid.set_test_cell_host(Some(OrderColumn::Price), widget_host)
    });
    fixture.settle(cx);
    let rows = a11y_row_tops(cx, fixture.window);
    assert_eq!(rows, plain_rows);
    let cells = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().a11y_cells());
    assert_eq!(cells, plain_cells);
    let hosts = cell_hosts(cx, fixture.window);
    assert!(!hosts.is_empty());
    for host in &hosts {
        assert!(
            rows.iter().any(|(id, _)| *id == host.row),
            "{:?} is in an accessibility row",
            host.row
        );
    }
    // The focused cell's host claims the active descendant: no element of
    // its own for the focus.
    assert!(!has_focus_element(cx));
    let focused_host = cx
        .update_window(fixture.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|element| {
                    element.path().ends_with(&[
                        order_row_id(2),
                        cell_host_id("live-grid-cell", OrderColumn::Price),
                    ])
                })
                .and_then(|element| element.role())
        })
        .expect("window is open");
    assert_eq!(focused_host, Some(Role::GridCell));
}

/// An editor that refuses every start: enough to give a column an editor.
struct RefusingEditor;

impl CellEditor<Order> for RefusingEditor {
    fn open(&self, _: &EditorStart<'_, Order>, _: &mut Window, _: &mut App) -> EditorOpen {
        EditorOpen::Refuse
    }
}

/// A gesture that opens no editor.
struct NoEditGesture;

impl EditGesture for NoEditGesture {
    fn starts_edit(&self, _: &EditGestureContext) -> bool {
        false
    }

    fn after_commit(&self, _: EditCommitKey) -> AfterCommit {
        AfterCommit::Stay
    }
}

#[gpui_kit::test]
fn test_edit_builders(cx: &mut TestAppContext) {
    let schema = ORDER_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            let content = match column {
                OrderColumn::Status => CellContent::Element,
                OrderColumn::Active => CellContent::Control,
                _ => CellContent::Text,
            };
            schema.column(
                GridColumn::new(column, column.id())
                    .with_width(px(width))
                    .with_content(content),
            )
        });
    let fixture = open_orders_in(cx, schema, orders(60), Orders, |grid| {
        grid.with_editor(OrderColumn::Price, RefusingEditor)
            .with_edit_gesture(NoEditGesture)
            .with_edit_policy(StandardEditPolicy)
    });
    fixture.grid.update(cx, |grid, cx| {
        assert!(grid.has_editor(OrderColumn::Price));
        assert!(!grid.has_editor(OrderColumn::Note));
        grid.set_editor(OrderColumn::Note, Some(Rc::new(RefusingEditor)), cx);
        assert!(grid.has_editor(OrderColumn::Note));
        grid.set_editor(OrderColumn::Price, None, cx);
        assert!(!grid.has_editor(OrderColumn::Price));
    });
    // Without `render_cell`, an `Element` or `Control` column paints its
    // value as text: no host.
    assert!(cell_hosts(cx, fixture.window).is_empty());
    let column = GridColumn::new(OrderColumn::Status, "Status");
    assert_eq!(column.content(), CellContent::Text);
    assert_eq!(column.header_label(), &HeaderLabel::Title);
    assert!(column.icon().is_none() && column.is_hideable());
    let column = column
        .with_content(CellContent::Control)
        .with_icon(IconName::Check)
        .with_hideable(false);
    assert_eq!(column.content(), CellContent::Control);
    assert_eq!(
        column.header_label(),
        &HeaderLabel::IconAndTitle(IconName::Check.path())
    );
    assert_eq!(column.icon(), Some(&IconName::Check.path()));
    assert!(!column.is_hideable());
    let column = column.with_icon_only(IconName::Check);
    assert_eq!(
        column.header_label(),
        &HeaderLabel::Icon(IconName::Check.path())
    );

    // The root names what the focused data cell's column paints, so Space
    // binds to `ActivateCell` on a control only.
    cx.update_window(fixture.window, |_, window, cx| {
        let handle = fixture.grid.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .expect("window is open");
    let focus_on = |column: OrderColumn, cx: &mut TestAppContext| {
        fixture.grid.update(cx, |grid, cx| {
            grid.select_key(None, cx);
            grid.set_focused_cell(&5, Some(column), cx);
        });
        fixture.settle(cx);
        cx.update_window(fixture.window, |_, window, _| {
            window
                .context_stack()
                .iter()
                .find(|context| context.contains(KEY_CONTEXT))
                .and_then(|root| root.get(CELL_CONTENT_KEY))
                .map(ToString::to_string)
        })
        .expect("window is open")
    };
    assert_eq!(focus_on(OrderColumn::Price, cx).as_deref(), Some("text"));
    assert_eq!(
        focus_on(OrderColumn::Status, cx).as_deref(),
        Some("element")
    );
    // With no element to handle it, `ActivateCell` lets Space go on to
    // `ToggleSelection`.
    assert_eq!(
        focus_on(OrderColumn::Active, cx).as_deref(),
        Some("control")
    );
    fixture.press_key(cx, "space");
    assert_eq!(
        fixture.selected_keys(cx),
        [5],
        "Space toggles the selection"
    );
    // Handled (here by a global listener), it takes Space on a control and
    // nowhere else.
    let activated = Rc::new(Cell::new(0));
    cx.update({
        let activated = activated.clone();
        move |cx| {
            cx.on_action(move |_: &ActivateCell, _| activated.set(activated.get() + 1));
        }
    });
    focus_on(OrderColumn::Active, cx);
    fixture.press_key(cx, "space");
    assert_eq!(activated.get(), 1);
    assert!(fixture.selected_keys(cx).is_empty());
    focus_on(OrderColumn::Price, cx);
    fixture.press_key(cx, "space");
    assert_eq!(activated.get(), 1);
    assert_eq!(fixture.selected_keys(cx), [5]);
    // F2, Backspace and Delete do nothing yet: no column takes an edit.
    fixture.take_events();
    for key in ["f2", "backspace", "delete"] {
        fixture.press_key(cx, key);
    }
    fixture.settle(cx);
    assert!(fixture.take_events().is_empty());
}
