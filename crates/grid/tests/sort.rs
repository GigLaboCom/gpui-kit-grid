//! Headless UI tests of sorting from the header and of the quick filter
//! (`.agents/COMPONENT_TEST_RULES.md`): the click cycle, multi-column sorts,
//! the sort indicators, the sort and quick filter APIs, user actions over a
//! held order and the throttle, and the replaceable sort cycle.

mod common;

use std::{cell::RefCell, cmp::Ordering, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::Sizable as _;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, Modifiers, ParentElement as _, Pixels,
    Render, Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, px, size,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    ColumnAlign, ColumnIx, GridColumn, GridColumns as _, GridSchema, LiveGrid, LiveGridEvent,
    LiveGridState, ManualReorder, SortCycle, SortCycleContext, SortDirection, SortOrigin,
    StoreOptions, ViewSpec,
};

use crate::common::{
    Fixture, Item, ItemColumn, Items, WINDOW_HEIGHT, by_price, click_with, open, open_configured,
    unthrottled,
};

use ItemColumn::{Id, Price};
use SortDirection::{Ascending, Descending};

type Sort = Vec<(ItemColumn, SortDirection)>;

/// The events a sort test follows, in order.
#[derive(Debug, PartialEq)]
enum Seen {
    Sort(Sort, SortOrigin),
    Projection,
    Held(bool),
    Layout,
}

/// Every followed event so far, then forgets them.
fn take_events(fixture: &Fixture) -> Vec<Seen> {
    std::mem::take(&mut *fixture.events.borrow_mut())
        .into_iter()
        .filter_map(|event| match event {
            LiveGridEvent::SortChange { sort, origin } => Some(Seen::Sort(sort.to_vec(), origin)),
            LiveGridEvent::ProjectionChange { .. } => Some(Seen::Projection),
            LiveGridEvent::OrderHeldChange { held } => Some(Seen::Held(held)),
            LiveGridEvent::LayoutChange { .. } => Some(Seen::Layout),
            _ => None,
        })
        .collect()
}

fn user(sort: &[(ItemColumn, SortDirection)]) -> Seen {
    Seen::Sort(sort.to_vec(), SortOrigin::User)
}

fn programmatic(sort: &[(ItemColumn, SortDirection)]) -> Seen {
    Seen::Sort(sort.to_vec(), SortOrigin::Programmatic)
}

fn sort(fixture: &Fixture, cx: &mut TestAppContext) -> Sort {
    fixture.grid.read_with(cx, |grid, _| grid.sort().to_vec())
}

fn click(fixture: &Fixture, cx: &mut TestAppContext, cell: &'static str) {
    cx.update_window(fixture.window, |_, window, cx| {
        window.within("live-grid-header").click(cell, cx)
    })
    .expect("window is open");
}

fn shift_click(fixture: &Fixture, cx: &mut TestAppContext, cell: &'static str) {
    cx.update_window(fixture.window, |_, window, cx| {
        let center = window
            .within("live-grid-header")
            .find(cell)
            .bounds()
            .center();
        click_with(window, center, Modifiers::shift(), cx);
    })
    .expect("window is open");
}

/// The bounds of `id` inside the header cell `cell`, if it is there.
fn part(
    fixture: &Fixture,
    cx: &mut TestAppContext,
    cell: &'static str,
    id: &'static str,
) -> Option<Bounds<Pixels>> {
    cx.update_window(fixture.window, |_, window, _| {
        window
            .within("live-grid-header")
            .within(cell)
            .try_find(id)
            .map(|found| found.bounds())
    })
    .expect("window is open")
}

fn cell(fixture: &Fixture, cx: &mut TestAppContext, cell: &'static str) -> Bounds<Pixels> {
    cx.update_window(fixture.window, |_, window, _| {
        window.within("live-grid-header").find(cell).bounds()
    })
    .expect("window is open")
}

/// Which header cells show a direction icon and a priority number.
fn indicators(fixture: &Fixture, cx: &mut TestAppContext) -> [(bool, bool); 2] {
    ["id", "price"].map(|cell| {
        (
            part(fixture, cx, cell, "sort-indicator").is_some(),
            part(fixture, cx, cell, "sort-priority").is_some(),
        )
    })
}

/// Writes `items` and waits until the grid shows them.
fn write(fixture: &Fixture, cx: &mut TestAppContext, items: &[Item]) {
    let writer = fixture.writer(cx);
    for item in items {
        writer.upsert(item.clone());
    }
    fixture.settle(cx);
}

/// 30 rows whose prices are a permutation of `0, 10, … 290`, so ascending,
/// descending and insertion order all differ.
fn permuted() -> Vec<Item> {
    (0..30)
        .map(|id| Item {
            id,
            price: i64::from((id * 7) % 30) * 10,
        })
        .collect()
}

/// The first ten keys of `items` (in insertion order) under `sort`, as the
/// engine orders them: ties keep insertion order.
fn reference(items: &[Item], sort: &[(ItemColumn, SortDirection)]) -> Vec<u32> {
    let value = |item: &Item, column: ItemColumn| match column {
        Id => i64::from(item.id),
        Price => item.price,
    };
    let mut rows = items.to_vec();
    rows.sort_by(|a, b| {
        sort.iter()
            .map(|(column, direction)| {
                let ordering = value(a, *column).cmp(&value(b, *column));
                match direction {
                    Ascending => ordering,
                    Descending => ordering.reverse(),
                }
            })
            .find(|ordering| ordering.is_ne())
            .unwrap_or(Ordering::Equal)
    });
    rows.iter().take(10).map(|item| item.id).collect()
}

#[gpui_kit::test]
fn test_header_click_cycles_sort(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, ViewSpec::new(), Duration::ZERO);
    let items = permuted();
    write(&fixture, cx, &items);
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(15, cx));
    fixture.settle(cx);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.visible_rows()),
        15..25
    );
    take_events(&fixture);

    for expected in [vec![(Price, Ascending)], vec![(Price, Descending)], vec![]] {
        click(&fixture, cx, "price");
        // The sort and its event change at once; the order follows the
        // rebuild.
        assert_eq!(sort(&fixture, cx), expected);
        assert_eq!(take_events(&fixture), [user(&expected)]);
        fixture.settle(cx);
        assert_eq!(take_events(&fixture), [Seen::Projection]);
        assert_eq!(fixture.visible_keys(cx), reference(&items, &expected));
        // A new order is shown from the top.
        assert_eq!(
            fixture.grid.read_with(cx, |grid, _| grid.visible_rows()),
            0..10
        );
        let sorted = !expected.is_empty();
        assert_eq!(indicators(&fixture, cx), [(false, false), (sorted, false)]);
    }
    assert_eq!(
        fixture.visible_keys(cx),
        (0..10).collect::<Vec<_>>(),
        "unsorted is the insertion order"
    );

    // A double click is two clicks: two steps of the cycle.
    cx.update_window(fixture.window, |_, window, cx| {
        window.within("live-grid-header").double_click("price", cx)
    })
    .expect("window is open");
    fixture.settle(cx);
    assert_eq!(sort(&fixture, cx), [(Price, Descending)]);
    let events = take_events(&fixture);
    assert_eq!(
        events[..2],
        [user(&[(Price, Ascending)]), user(&[(Price, Descending)])]
    );
    assert!(
        !events.contains(&Seen::Layout),
        "sorting is not a layout change"
    );

    // The quick filter is a new view too: shown from the top.
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_row(5, cx));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("1", cx));
    fixture.settle(cx);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.visible_rows()),
        0..10
    );
}

#[gpui_kit::test]
fn test_shift_click_multi_sort(cx: &mut TestAppContext) {
    let fixture = open(cx, 30, ViewSpec::new(), Duration::ZERO);
    // Three prices, so the second sort column breaks ties.
    let items: Vec<Item> = (0..30)
        .map(|id| Item {
            id,
            price: i64::from(id % 3) * 10,
        })
        .collect();
    write(&fixture, cx, &items);
    take_events(&fixture);

    let steps: [(&'static str, bool, Sort); 5] = [
        ("price", false, vec![(Price, Ascending)]),
        // Shift adds the column last.
        ("id", true, vec![(Price, Ascending), (Id, Ascending)]),
        // Shift flips it in place: the priority stays.
        ("id", true, vec![(Price, Ascending), (Id, Descending)]),
        ("price", true, vec![(Price, Descending), (Id, Descending)]),
        // Shift on a descending column removes it; the rest move up.
        ("price", true, vec![(Id, Descending)]),
    ];
    for (cell, shift, expected) in steps {
        if shift {
            shift_click(&fixture, cx, cell);
        } else {
            click(&fixture, cx, cell);
        }
        fixture.settle(cx);
        assert_eq!(sort(&fixture, cx), expected, "{cell}");
        assert_eq!(fixture.visible_keys(cx), reference(&items, &expected));
        assert_eq!(take_events(&fixture), [user(&expected), Seen::Projection]);
        // Priority numbers appear only with two or more sorted columns.
        let multi = expected.len() >= 2;
        let sorted = |column| expected.iter().any(|(sorted, _)| *sorted == column);
        assert_eq!(
            indicators(&fixture, cx),
            [
                (sorted(Id), sorted(Id) && multi),
                (sorted(Price), sorted(Price) && multi)
            ],
            "{expected:?}"
        );
    }

    // A click without Shift on a column of a multi-column sort drops the
    // others and steps that column from its own direction.
    shift_click(&fixture, cx, "price");
    fixture.settle(cx);
    assert_eq!(sort(&fixture, cx), [(Id, Descending), (Price, Ascending)]);
    click(&fixture, cx, "price");
    fixture.settle(cx);
    assert_eq!(sort(&fixture, cx), [(Price, Descending)]);
}

#[gpui_kit::test]
fn test_sort_applies_while_order_held(cx: &mut TestAppContext) {
    let fixture = open_configured(cx, 30, Some(gpui_kit::component::Size::XSmall), |grid| {
        grid.with_view(by_price(Descending))
            .with_tuning(unthrottled())
            .with_reorder_policy(ManualReorder)
    });
    let shown: Vec<u32> = (20..30).rev().collect();
    fixture.write_prices(cx, 30, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), shown);
    assert!(fixture.is_order_held(cx));
    take_events(&fixture);

    // A header click is a user action: the held order goes at once, then
    // the sort changes, and the new order is shown without
    // `apply_held_order`.
    click(&fixture, cx, "id");
    assert!(!fixture.is_order_held(cx));
    assert_eq!(
        take_events(&fixture),
        [Seen::Held(false), user(&[(Id, Ascending)])]
    );
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert_eq!(take_events(&fixture), [Seen::Projection]);
    assert!(!fixture.is_order_held(cx));
}

#[gpui_kit::test]
fn test_sort_ignores_throttle(cx: &mut TestAppContext) {
    // Data-driven rebuilds wait 10 s; the settle below lasts 60 ms.
    let fixture = open(cx, 30, ViewSpec::new(), Duration::from_secs(10));
    let items = permuted();
    write(&fixture, cx, &items);
    click(&fixture, cx, "price");
    fixture.settle(cx);
    assert_eq!(
        fixture.visible_keys(cx),
        reference(&items, &[(Price, Ascending)])
    );
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort([(Price, Descending)], cx));
    fixture.settle(cx);
    assert_eq!(
        fixture.visible_keys(cx),
        reference(&items, &[(Price, Descending)])
    );
}

struct Root {
    grid: Entity<LiveGridState<Items>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(LiveGrid::new(&self.grid).xsmall().bordered(false))
    }
}

/// [`open`] with another schema: 30 rows (`price = id * 10`), unthrottled.
fn open_with_schema(cx: &mut TestAppContext, schema: GridSchema<ItemColumn>) -> Fixture {
    cx.update(gpui_kit::init);
    let (window, root) = cx
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
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            LiveGridState::new_standalone(
                                schema,
                                Items,
                                StoreOptions::default(),
                                cx,
                            )
                            .with_tuning(unthrottled())
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(
            &grid,
            move |_, event: &LiveGridEvent<u32, ItemColumn>, _| {
                events.borrow_mut().push(event.clone());
            },
        )
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..30).map(|id| {
        Arc::new(Item {
            id,
            price: i64::from(id) * 10,
        })
    }));
    let fixture = Fixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    fixture
}

#[gpui_kit::test]
fn test_unsortable_column(cx: &mut TestAppContext) {
    let fixture = open_with_schema(
        cx,
        GridSchema::new()
            .column(GridColumn::new(Id, "Id").with_sortable(false))
            .column(GridColumn::new(Price, "Price").with_align(ColumnAlign::End)),
    );
    take_events(&fixture);

    // A click on a column that is not sortable changes nothing.
    click(&fixture, cx, "id");
    fixture.settle(cx);
    assert!(sort(&fixture, cx).is_empty());
    assert_eq!(take_events(&fixture), []);
    assert_eq!(indicators(&fixture, cx), [(false, false); 2]);

    // The API sorts by it anyway. Repeated columns are dropped, the first
    // one stays.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_sort([(Id, Descending), (Price, Ascending), (Id, Ascending)], cx)
    });
    let expected = [(Id, Descending), (Price, Ascending)];
    assert_eq!(sort(&fixture, cx), expected);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    assert_eq!(
        take_events(&fixture),
        [programmatic(&expected), Seen::Projection]
    );
    assert_eq!(indicators(&fixture, cx), [(true, true); 2]);
    // The same sort again does nothing.
    let rebuilds = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().rebuilds());
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort(expected, cx));
    fixture.settle(cx);
    assert_eq!(take_events(&fixture), []);
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds
    );
    click(&fixture, cx, "id");
    assert_eq!(sort(&fixture, cx), expected, "still not by a click");

    // The indicator follows a start-aligned title and precedes an
    // end-aligned one, inside the padding (clear of the resize handles).
    let id = cell(&fixture, cx, "id");
    let icon = part(&fixture, cx, "id", "sort-indicator").expect("sorted");
    assert!(
        icon.left() > id.left() + px(10.),
        "after the title: {icon:?}"
    );
    assert!(icon.right() <= id.right() - px(4.));
    let price = cell(&fixture, cx, "price");
    let icon = part(&fixture, cx, "price", "sort-indicator").expect("sorted");
    let priority = part(&fixture, cx, "price", "sort-priority").expect("two sorted");
    assert!(icon.left() >= price.left() + px(4.));
    assert!(
        priority.right() < price.right() - px(10.),
        "before the title: {priority:?}"
    );
}

#[gpui_kit::test]
fn test_quick_filter(cx: &mut TestAppContext) {
    // A data-driven rebuild would wait 10 s: the quick filter does not.
    let fixture = open(cx, 30, ViewSpec::new(), Duration::from_secs(10));
    let quick_filter = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.quick_filter().to_string())
    };
    let rebuilds = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.stats().rebuilds())
    };

    // "7" in the Id or the Price text (`price = id * 10`).
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("7", cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [7, 17, 27]);
    assert_eq!(quick_filter(cx), "7");

    // The same text, trimmed, rebuilds nothing.
    let before = rebuilds(cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter(" 7 ", cx));
    fixture.settle(cx);
    assert_eq!(rebuilds(cx), before);
    assert_eq!(quick_filter(cx), "7");

    // Every term is required, in any column.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("7 2", cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [27]);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("7", cx));

    // A new view keeps the quick filter.
    take_events(&fixture);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_view(by_price(Descending), cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [27, 17, 7]);
    assert_eq!(quick_filter(cx), "7");
    assert_eq!(
        take_events(&fixture),
        [programmatic(&[(Price, Descending)]), Seen::Projection]
    );
    // The same sort in a new view rebuilds without a `SortChange`.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_view(by_price(Descending), cx));
    fixture.settle(cx);
    assert_eq!(take_events(&fixture), [Seen::Projection]);

    // Blank text turns it off.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter(" \t", cx));
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (20..30).rev().collect::<Vec<_>>());
    assert_eq!(quick_filter(cx), "");
    let before = rebuilds(cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_quick_filter("", cx));
    fixture.settle(cx);
    assert_eq!(rebuilds(cx), before, "already off");
}

#[gpui_kit::test]
fn test_quick_filter_live_and_columns(cx: &mut TestAppContext) {
    let interval = Duration::from_secs(1);
    let fixture = open(cx, 30, ViewSpec::new(), interval);
    let set_filter = |cx: &mut TestAppContext, fixture: &Fixture, text: &'static str| {
        fixture
            .grid
            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
        fixture.settle(cx);
    };
    set_filter(cx, &fixture, "777");
    assert_eq!(fixture.visible_keys(cx), Vec::<u32>::new());

    // A streamed value that starts to match shows after the throttle.
    fixture.writer(cx).upsert(Item { id: 3, price: 777 });
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), Vec::<u32>::new());
    fixture.frame(cx, interval);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), [3]);

    // Hiding a default quick filter column searches without it at once.
    take_events(&fixture);
    let hide = |cx: &mut TestAppContext, fixture: &Fixture, visible: bool| {
        fixture
            .grid
            .update(cx, |grid, cx| grid.set_column_visible(Price, visible, cx));
        fixture.settle(cx);
    };
    hide(cx, &fixture, false);
    assert_eq!(fixture.visible_keys(cx), Vec::<u32>::new());
    assert_eq!(take_events(&fixture), [Seen::Layout, Seen::Projection]);
    hide(cx, &fixture, true);
    assert_eq!(fixture.visible_keys(cx), [3]);

    // Explicit columns: Id only never matches a price.
    let ids = open_configured(cx, 30, Some(gpui_kit::component::Size::XSmall), |grid| {
        grid.with_quick_filter_columns([Id])
    });
    set_filter(cx, &ids, "70");
    assert_eq!(ids.visible_keys(cx), Vec::<u32>::new());
    set_filter(cx, &ids, "2");
    assert_eq!(
        ids.visible_keys(cx),
        [2, 12, 20, 21, 22, 23, 24, 25, 26, 27]
    );

    // An explicit set is kept when its column is hidden.
    let prices = open_configured(cx, 30, Some(gpui_kit::component::Size::XSmall), |grid| {
        grid.with_quick_filter_columns([Price])
    });
    set_filter(cx, &prices, "70");
    assert_eq!(prices.visible_keys(cx), [7, 17, 27]);
    let rebuilds = prices.grid.read_with(cx, |grid, _| grid.stats().rebuilds());
    hide(cx, &prices, false);
    assert_eq!(prices.visible_keys(cx), [7, 17, 27]);
    assert_eq!(
        prices.grid.read_with(cx, |grid, _| grid.stats().rebuilds()),
        rebuilds
    );
}

/// `DataTable`'s order: descending, ascending, unsorted.
struct DescendingFirst;

impl SortCycle for DescendingFirst {
    fn next_sort(&self, context: &SortCycleContext<'_>) -> Vec<(ColumnIx, SortDirection)> {
        let column = context.column();
        match context.direction() {
            None => vec![(column, Descending)],
            Some(Descending) => vec![(column, Ascending)],
            Some(Ascending) => Vec::new(),
        }
    }
}

/// Returns a repeated column and one outside the schema's column type.
struct Careless;

impl SortCycle for Careless {
    fn next_sort(&self, context: &SortCycleContext<'_>) -> Vec<(ColumnIx, SortDirection)> {
        vec![
            (context.column(), Descending),
            (ColumnIx::new(99), Ascending),
            (context.column(), Ascending),
            (Id.ix(), Ascending),
        ]
    }
}

#[gpui_kit::test]
fn test_sort_cycle_seam(cx: &mut TestAppContext) {
    let fixture = open_configured(cx, 30, Some(gpui_kit::component::Size::XSmall), |grid| {
        grid.with_sort_cycle(DescendingFirst)
            .with_tuning(unthrottled())
    });
    for expected in [vec![(Price, Descending)], vec![(Price, Ascending)], vec![]] {
        click(&fixture, cx, "price");
        assert_eq!(sort(&fixture, cx), expected);
    }

    // The grid drops repeats and columns the column type does not have.
    let fixture = open_configured(cx, 30, Some(gpui_kit::component::Size::XSmall), |grid| {
        grid.with_sort_cycle(Careless)
    });
    click(&fixture, cx, "price");
    assert_eq!(sort(&fixture, cx), [(Price, Descending), (Id, Ascending)]);
}
