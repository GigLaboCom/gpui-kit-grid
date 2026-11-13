//! Headless UI tests of `LiveGridHandle` (`.agents/COMPONENT_TEST_RULES.md`): a
//! grid without its delegate type reads its columns, layout, sort, grouping and
//! counts by column id, commands them with the same effect and the same events
//! as the typed state, ignores unknown ids, copies and exports, translates the
//! grid's events, and lets go of the grid through a weak handle.

mod common;

use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};

use gpui_kit::component::{IconName, IconNamed as _};
use gpui_kit::{AppContext as _, TestAppContext, px};
use gpui_kit_grid::{
    BandInfo, CellValue, ColumnBand, ColumnSet, ColumnZone, CsvFormat, ExpandDepth, ExportRequest,
    ExportScope, GridColumn, GridColumns as _, GridHandleEvent, GridSchema, GroupBy, GroupKey,
    GroupOrigin, HeaderLabel, LayoutGroup, LayoutOrigin, LayoutPart, LayoutParts, LayoutSort,
    LiveGridEvent, LiveGridHandle, ManualReorder, SelectionMode, SelectionOrigin, SortDirection,
    SortOrigin, ViewSpec,
};

use crate::common::{EditFixture, Order, OrderColumn, Orders, open_orders_in, orders, unthrottled};

use OrderColumn::{Active, Id, Instrument, Note, Placed, Price, Quantity, Side, Status};
use SortDirection::{Ascending, Descending};

/// Id frozen and not hideable; a band over Side, Qty and Price (not
/// resizable); Status shows an icon alone; Placed hidden; Active neither
/// sortable nor groupable; Note not movable.
fn schema() -> GridSchema<OrderColumn> {
    GridSchema::new()
        .column(
            GridColumn::new(Id, "Id")
                .with_width(px(50.))
                .with_hideable(false),
        )
        .column(GridColumn::new(Instrument, "Instrument").with_width(px(90.)))
        .band(
            ColumnBand::new("trade", "Trade")
                .column(GridColumn::new(Side, "Side").with_width(px(50.)))
                .column(GridColumn::new(Quantity, "Qty").with_width(px(70.)))
                .column(
                    GridColumn::new(Price, "Price")
                        .with_width(px(80.))
                        .with_resizable(false),
                ),
        )
        .column(
            GridColumn::new(Status, "Status")
                .with_width(px(80.))
                .with_icon_only(IconName::Check),
        )
        .column(
            GridColumn::new(Placed, "Placed")
                .with_width(px(150.))
                .with_visible(false),
        )
        .column(
            GridColumn::new(Active, "Active")
                .with_width(px(60.))
                .with_sortable(false)
                .with_groupable(false),
        )
        .column(
            GridColumn::new(Note, "Note")
                .with_width(px(120.))
                .with_movable(false),
        )
        .with_frozen(1)
}

/// Twelve orders over [`schema`], sorted by price (highest first), then id.
fn open(cx: &mut TestAppContext) -> (EditFixture, LiveGridHandle) {
    let fixture = open_orders_in(cx, schema(), orders(12), Orders, |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Price, Descending), (Id, Ascending)]))
            .with_view_id("orders")
            .with_selection_mode(SelectionMode::RowSet)
            .with_tuning(unthrottled())
    });
    let handle = LiveGridHandle::new(&fixture.grid);
    (fixture, handle)
}

/// The `LayoutChange`s of the typed log: parts, columns and origin.
fn layout_changes(fixture: &EditFixture) -> Vec<(LayoutParts, ColumnSet, LayoutOrigin)> {
    fixture
        .events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::LayoutChange {
                parts,
                columns,
                origin,
            } => Some((*parts, *columns, *origin)),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn test_handle_columns(cx: &mut TestAppContext) {
    let (fixture, handle) = open(cx);
    let from_state = fixture.grid.update(cx, |grid, cx| grid.handle(cx));
    assert_eq!(from_state.entity_id(), handle.entity_id());
    assert_eq!(
        LiveGridHandle::from(&fixture.grid).entity_id(),
        fixture.grid.entity_id()
    );

    // Every schema column in display order, the hidden one at its place.
    let columns = cx.update(|cx| handle.columns(cx));
    let ids: Vec<&str> = columns.iter().map(|column| column.id().as_ref()).collect();
    assert_eq!(
        ids,
        [
            "id",
            "instrument",
            "side",
            "quantity",
            "price",
            "status",
            "placed",
            "active",
            "note"
        ]
    );
    let column = |id: &str| {
        columns
            .iter()
            .find(|column| column.id().as_ref() == id)
            .expect("the column")
    };

    let id = column("id");
    assert_eq!(id.zone(), ColumnZone::FrozenLeft);
    assert_eq!(id.width(), px(50.));
    assert!(!id.is_hideable() && id.is_visible());
    assert_eq!((id.sort(), id.sort_priority()), (Some(Ascending), Some(1)));
    assert!(id.bands().is_empty());

    let price = column("price");
    assert_eq!(price.title(), "Price");
    assert_eq!(price.zone(), ColumnZone::Scrolling);
    assert!(!price.is_resizable() && price.is_hideable());
    assert_eq!(
        (price.sort(), price.sort_priority()),
        (Some(Descending), Some(0))
    );
    assert_eq!(price.bands().len(), 1);
    let band: &BandInfo = &price.bands()[0];
    assert_eq!((band.id(), band.title().as_ref()), ("trade", "Trade"));
    assert_eq!(column("quantity").title(), "Qty");
    assert_eq!(column("side").bands(), price.bands());

    // The icon of a header that shows it alone; the title stays its name.
    let status = column("status");
    assert_eq!(status.icon(), Some(&IconName::Check.path()));
    assert_eq!(
        status.header_label(),
        &HeaderLabel::Icon(IconName::Check.path())
    );
    assert_eq!(status.title(), "Status");
    assert_eq!(column("instrument").icon(), None);

    assert!(!column("placed").is_visible());
    assert_eq!(column("placed").width(), px(150.));
    let active = column("active");
    assert!(!active.is_sortable() && !active.is_groupable() && active.is_movable());
    assert_eq!(active.sort(), None);
    assert!(!column("note").is_movable());

    // One column by id; an unknown id is none.
    cx.update(|cx| {
        let price = handle.column("price", cx).expect("the column");
        assert_eq!(price.sort_priority(), Some(0));
        assert!(handle.column("nope", cx).is_none());
        assert_eq!(handle.view_id(cx).as_deref(), Some("orders"));
    });
}

/// The commands a toolbar sends: through the handle on one grid, through
/// the typed state on the other.
#[gpui_kit::test]
fn test_handle_commands(cx: &mut TestAppContext) {
    let (by_handle, handle) = open(cx);
    let (typed, _) = open(cx);
    by_handle.events.borrow_mut().clear();
    typed.events.borrow_mut().clear();

    cx.update(|cx| {
        assert!(handle.set_column_visible("note", false, cx));
        assert!(handle.move_column("price", Some("side"), cx));
        assert!(handle.move_band("trade", None, cx));
        assert!(handle.set_column_zone("active", ColumnZone::FrozenRight, cx));
        assert!(handle.set_column_width("instrument", px(120.), cx));
        handle.show_all_columns(cx);
    });
    typed.grid.update(cx, |grid, cx| {
        assert!(grid.set_column_visible(Note, false, cx));
        assert!(grid.move_column(Price, Some(Side), cx));
        assert!(grid.move_band("trade", None, cx));
        grid.set_column_zone(Active, ColumnZone::FrozenRight, cx);
        grid.set_column_width(Instrument, px(120.), cx);
        grid.show_all_columns(cx);
    });
    by_handle.settle(cx);
    typed.settle(cx);

    // The same layout, and the same `LayoutChange`s, all `Programmatic`.
    let changes = layout_changes(&by_handle);
    assert_eq!(changes.len(), 6);
    assert_eq!(changes, layout_changes(&typed));
    assert!(
        changes
            .iter()
            .all(|(_, _, origin)| *origin == LayoutOrigin::Programmatic)
    );
    let layout = |fixture: &EditFixture, cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| grid.layout())
    };
    assert_eq!(layout(&by_handle, cx), layout(&typed, cx));
    assert_eq!(cx.update(|cx| handle.layout(cx)), layout(&typed, cx));
    let order = |fixture: &EditFixture, cx: &mut TestAppContext| -> Vec<OrderColumn> {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.columns().collect())
    };
    assert_eq!(order(&by_handle, cx), order(&typed, cx));
    assert_eq!(order(&by_handle, cx).last(), Some(&Active));

    // An unknown id, or a refused command: false, nothing changes, no event.
    by_handle.events.borrow_mut().clear();
    cx.update(|cx| {
        assert!(!handle.set_column_visible("nope", false, cx));
        assert!(!handle.move_column("nope", None, cx));
        assert!(!handle.move_column("price", Some("nope"), cx));
        assert!(!handle.move_band("nope", None, cx));
        assert!(!handle.move_band("trade", Some("nope"), cx));
        assert!(!handle.set_column_zone("nope", ColumnZone::FrozenLeft, cx));
        assert!(!handle.set_column_width("nope", px(10.), cx));
        // Across zones: the move policy refuses.
        assert!(!handle.move_column("note", Some("id"), cx));
    });
    by_handle.settle(cx);
    assert!(layout_changes(&by_handle).is_empty());
    assert_eq!(layout(&by_handle, cx), layout(&typed, cx));

    // Autofit through the handle fits the visible columns in the next frame.
    cx.update(|cx| handle.autofit_columns(cx));
    by_handle.settle(cx);
    let autofit = layout_changes(&by_handle);
    assert_eq!(autofit.len(), 1);
    assert!(autofit[0].0.contains(LayoutPart::Width));
}

#[gpui_kit::test]
fn test_handle_view(cx: &mut TestAppContext) {
    let (fixture, handle) = open(cx);
    cx.update(|cx| {
        assert_eq!(
            handle.sort(cx),
            [
                LayoutSort::new("price", Descending),
                LayoutSort::new("id", Ascending)
            ]
        );
        // A new sort by id; an unknown column changes nothing.
        assert!(handle.set_sort([LayoutSort::new("quantity", Ascending)], cx));
        assert!(!handle.set_sort([LayoutSort::new("nope", Ascending)], cx));
        assert_eq!(handle.sort(cx), [LayoutSort::new("quantity", Ascending)]);
    });
    fixture.settle(cx);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.sort().to_vec()),
        [(Quantity, Ascending)]
    );

    // Grouping by column levels; an unknown column changes nothing.
    cx.update(|cx| {
        assert!(handle.set_group_by([LayoutGroup::new("instrument", Descending)], cx));
        assert!(!handle.set_group_by([LayoutGroup::new("nope", Ascending)], cx));
        assert_eq!(
            handle.group_by(cx),
            [LayoutGroup::new("instrument", Descending)]
        );
        handle.set_expand_depth(ExpandDepth::Levels(0), cx);
        assert_eq!(handle.expand_depth(cx), ExpandDepth::Levels(0));
    });
    fixture.settle(cx);
    fixture.grid.read_with(cx, |grid, _| {
        assert_eq!(grid.group_by().len(), 1);
        assert_eq!(grid.group_by()[0].as_column(), Some(Instrument));
        assert_eq!(grid.group_by()[0].direction(), Descending);
        assert_eq!(grid.projection().len(), 4);
    });

    // A level by a function of the row reads back by its own id.
    let mut instrument = ColumnSet::EMPTY;
    instrument.insert(Instrument.ix());
    fixture.grid.update(cx, |grid, cx| {
        grid.set_group_by(
            [GroupBy::key(GroupKey::new(
                "desk",
                instrument,
                |row: &Order| CellValue::Text(row.instrument),
            ))],
            cx,
        )
    });
    assert_eq!(
        cx.update(|cx| handle.group_by(cx)),
        [LayoutGroup::new("desk", Ascending)]
    );
    cx.update(|cx| assert!(handle.set_group_by([], cx)));
    fixture.settle(cx);
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.group_by().is_empty())
    );

    // The quick filter.
    cx.update(|cx| handle.set_quick_filter("AAPL", cx));
    fixture.settle(cx);
    cx.update(|cx| {
        assert_eq!(handle.quick_filter(cx), "AAPL");
        assert_eq!(handle.row_count(cx), 3);
        handle.set_quick_filter("", cx);
    });
    fixture.settle(cx);

    // A layout read, changed and set back; then the default layout.
    let saved = cx.update(|cx| handle.layout(cx));
    cx.update(|cx| {
        assert!(handle.set_column_visible("note", false, cx));
        assert!(handle.set_layout(&saved, cx).is_ok());
        assert_eq!(handle.layout(cx), saved);
        handle.reset_layout(cx);
        assert_eq!(
            handle.sort(cx),
            [
                LayoutSort::new("price", Descending),
                LayoutSort::new("id", Ascending)
            ]
        );
    });
    assert_eq!(
        cx.update(|cx| handle.layout(cx)),
        fixture.grid.read_with(cx, |grid, _| grid.default_layout())
    );
}

#[gpui_kit::test]
fn test_handle_counts(cx: &mut TestAppContext) {
    let fixture = open_orders_in(cx, schema(), orders(12), Orders, |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Price, Descending)]))
            .with_selection_mode(SelectionMode::RowSet)
            .with_reorder_policy(ManualReorder)
            .with_tuning(unthrottled())
    });
    let handle = LiveGridHandle::new(&fixture.grid);
    cx.update(|cx| {
        assert_eq!(handle.row_count(cx), 12);
        assert_eq!(handle.total_row_count(cx), 12);
        assert_eq!(handle.selected_count(cx), 0);
    });

    // A filter shows fewer rows of the same total; selected rows it hides
    // still count.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_selected_keys([1, 4, 8], cx));
    cx.update(|cx| handle.set_quick_filter("MSFT", cx));
    fixture.settle(cx);
    cx.update(|cx| {
        assert_eq!(handle.row_count(cx), 3);
        assert_eq!(handle.total_row_count(cx), 12);
        assert_eq!(handle.selected_count(cx), 3);
        handle.set_quick_filter("", cx);
    });
    fixture.settle(cx);

    // New prices reverse the order: held until applied.
    let writer = fixture.writer(cx);
    for order in orders(12) {
        writer.upsert(Order {
            price: -order.price,
            ..order
        });
    }
    fixture.settle(cx);
    assert!(cx.update(|cx| handle.is_order_held(cx)));
    cx.update(|cx| handle.apply_held_order(cx));
    fixture.settle(cx);
    assert!(!cx.update(|cx| handle.is_order_held(cx)));
    assert_eq!(fixture.visible_keys(cx)[0], 1);
}

#[gpui_kit::test]
fn test_handle_events(cx: &mut TestAppContext) {
    let (fixture, handle) = open(cx);
    let seen: Rc<RefCell<Vec<GridHandleEvent>>> = Rc::default();
    let notified = Rc::new(RefCell::new(0));
    let (subscription, observation) = cx.update(|cx| {
        let seen = seen.clone();
        let notified = notified.clone();
        (
            handle.subscribe(cx, move |event, _| seen.borrow_mut().push(event.clone())),
            handle.observe(cx, move |_| *notified.borrow_mut() += 1),
        )
    });

    cx.update(|cx| {
        assert!(handle.set_column_visible("note", false, cx));
        assert!(handle.set_sort([LayoutSort::new("id", Ascending)], cx));
    });
    fixture.settle(cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_selected_keys([2, 3], cx));
    fixture.settle(cx);

    // What the typed events say, in their order, without keys and columns.
    let version = fixture
        .grid
        .read_with(cx, |grid, _| grid.snapshot().version());
    assert_eq!(
        *seen.borrow(),
        [
            GridHandleEvent::LayoutChange {
                parts: LayoutPart::Visibility.into(),
                origin: LayoutOrigin::Programmatic
            },
            GridHandleEvent::SortChange {
                origin: SortOrigin::Programmatic
            },
            GridHandleEvent::ProjectionChange {
                row_count: 12,
                store_version: version
            },
            GridHandleEvent::SelectionChange {
                count: 2,
                origin: SelectionOrigin::Programmatic
            },
        ]
    );
    assert!(*notified.borrow() > 0);

    // Collapsing groups and grouping translate too.
    cx.update(|cx| {
        assert!(handle.set_group_by([LayoutGroup::new("side", Ascending)], cx));
        handle.set_expand_depth(ExpandDepth::Levels(0), cx);
    });
    fixture.settle(cx);
    assert!(seen.borrow().contains(&GridHandleEvent::GroupChange {
        origin: GroupOrigin::Programmatic
    }));
    assert!(seen.borrow().contains(&GridHandleEvent::ExpansionChange {
        origin: GroupOrigin::Programmatic
    }));

    // Dropped: silence.
    drop((subscription, observation));
    let (events, notifications) = (seen.borrow().len(), *notified.borrow());
    cx.update(|cx| assert!(handle.set_column_visible("note", true, cx)));
    fixture.settle(cx);
    assert_eq!(seen.borrow().len(), events);
    assert_eq!(*notified.borrow(), notifications);
}

#[gpui_kit::test]
fn test_handle_weak(cx: &mut TestAppContext) {
    let (fixture, handle) = open(cx);
    let weak = handle.downgrade();
    assert_eq!(weak.entity_id(), handle.entity_id());
    let upgraded = weak.upgrade().expect("the grid is alive");
    assert_eq!(upgraded.entity_id(), handle.entity_id());
    drop(upgraded);

    // The handle keeps the grid alive; the weak one does not.
    let window = fixture.window;
    drop(fixture);
    cx.update_window(window, |_, window, _| window.remove_window())
        .expect("window is open");
    cx.run_until_parked();
    assert!(weak.upgrade().is_some());
    assert_eq!(cx.update(|cx| handle.row_count(cx)), 12);
    drop(handle);
    cx.run_until_parked();
    assert!(weak.upgrade().is_none());
}

/// A directory of the test's files, removed at the end.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gpui-kit-grid-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a temporary directory");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[gpui_kit::test]
fn test_handle_copy_and_export(cx: &mut TestAppContext) {
    let (fixture, handle) = open(cx);

    // The same text as the typed "Copy all".
    assert!(cx.update(|cx| handle.copy_scope(ExportScope::Visible, cx)));
    let by_handle = cx.read_from_clipboard().and_then(|item| item.text());
    assert!(fixture.grid.update(cx, |grid, cx| grid.copy_all(cx)));
    let typed = cx.read_from_clipboard().and_then(|item| item.text());
    assert!(by_handle.is_some());
    assert_eq!(by_handle, typed);

    // The same file as the typed export.
    let dir = TempDir::new("handle-export");
    let (by_handle, typed) = (dir.0.join("handle.csv"), dir.0.join("typed.csv"));
    assert!(!cx.update(|cx| handle.is_export_empty(ExportScope::Visible, cx)));
    let job = cx
        .update(|cx| handle.export_to_path(ExportScope::Visible, CsvFormat::new(), &by_handle, cx))
        .expect("something to export");
    cx.run_until_parked();
    assert_eq!(smol::block_on(job).expect("written").output(), &by_handle);
    let job = fixture
        .grid
        .update(cx, |grid, cx| {
            grid.export_to_path(
                ExportRequest::new(ExportScope::Visible),
                CsvFormat::new(),
                &typed,
                cx,
            )
        })
        .expect("something to export");
    cx.run_until_parked();
    smol::block_on(job).expect("written");
    let text = fs::read_to_string(&by_handle).expect("the file");
    assert!(text.starts_with("\u{feff}Id,Instrument,Side,Qty,Price,Status,Active,Note\r\n"));
    assert_eq!(text, fs::read_to_string(&typed).expect("the file"));

    // Nothing to export once a filter hides every row.
    cx.update(|cx| handle.set_quick_filter("no such order", cx));
    fixture.settle(cx);
    cx.update(|cx| {
        assert!(handle.is_export_empty(ExportScope::Visible, cx));
        assert!(!handle.copy_scope(ExportScope::Visible, cx));
    });
}
