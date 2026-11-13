//! Headless UI tests of what cells show. Elements in cells: which cells get an
//! element, element ids by key, presses on elements against the selection, the
//! keyboard focus after a press, Space on a control, the accessibility of a
//! host, clipping, and elements under a live stream, on the `EditFixture` of 60
//! orders with a recording delegate of its own. Values on screen: icons, times
//! and values of the application's own type, on a grid of events.

mod common;

use std::{
    cell::{Cell, RefCell},
    cmp::Ordering as ValueOrdering,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, TimeZone as _, Utc};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity,
    Focusable as _, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Pixels, Render, RenderOnce, Role, Styled as _, TestAppContext, WeakEntity,
    Window, WindowBounds, WindowOptions, div, point, prelude::FluentBuilder as _, px, size,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellContent, CellElementContext, CellStyle, CellStyleContext, CellValue, ColumnAlign, ColumnIx,
    GridColumn, GridColumns as _, GridMenuContext, GridRow, GridSchema, GridTarget, GridValue,
    GroupBy, HighlightPolicy, IconValue, LinkOpener, LinkValue, LiveGrid, LiveGridDelegate,
    LiveGridEvent, LiveGridState, PinnedEdge, SelectionMode, SortDirection, StoreOptions,
};

use crate::common::{
    EditFixture, ORDER_COLUMNS, ORDER_WINDOW_WIDTH, Order, OrderColumn, ROW, WINDOW_HEIGHT,
    cell_host_id, drag_move_to, fading_row_id, mouse_down_at, mouse_up_at, move_pointer_to,
    open_orders_in, order_cell, order_row_id, orders, press_at,
};

/// What the test delegate does and what it saw: shared with the test.
#[derive(Clone, Default)]
struct Hooks {
    /// Every `render_cell` call since the last `take`: key and column.
    calls: Rc<RefCell<Vec<(u32, OrderColumn)>>>,
    /// The quantity each `Quantity` element was built with.
    quantities: Rc<RefCell<Vec<(u32, i64)>>>,
    /// `render_cell` gives no element at all: every cell is text.
    none: Rc<Cell<bool>>,
    /// The element of `Quantity` stops the presses of the left button.
    stop: Rc<Cell<bool>>,
    /// Presses the stopping element handled.
    stopped: Rc<Cell<usize>>,
    /// The checkbox of `Active` registers its handler of Space.
    activate: Rc<Cell<bool>>,
    /// The orders whose checkbox was pressed, by mouse or Space.
    toggles: Rc<RefCell<Vec<u32>>>,
    /// The grid's focused row as the handler of Space read it.
    seen: Rc<RefCell<Vec<Option<u32>>>>,
    /// The grid, which the handler of Space reads.
    grid: Rc<RefCell<Option<WeakEntity<LiveGridState<Widgets>>>>>,
    /// The targets of the context menus built.
    menus: Rc<RefCell<Vec<GridTarget<u32, OrderColumn>>>>,
    /// The serial each `Price` element read from its keyed state.
    serials: Rc<RefCell<Vec<(u32, u64)>>>,
    /// The removals `render_cell` was told of: key and intensity.
    removed: Rc<RefCell<Vec<(u32, f32)>>>,
}

impl Hooks {
    fn take_calls(&self) -> Vec<(u32, OrderColumn)> {
        self.calls.take()
    }
}

/// A delegate with an element in `Quantity` (a progress bar), `Active` (a
/// checkbox, a control) and `Price` (an element with keyed state), and none
/// in `Note`: its cells stay text.
struct Widgets {
    hooks: Hooks,
}

/// Serials of keyed state, unique within the test binary.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// An element that keeps a serial in keyed state and logs it with its row:
/// the same serial for a key means the same element state.
#[derive(IntoElement)]
struct Serial {
    key: u32,
    log: Rc<RefCell<Vec<(u32, u64)>>>,
}

impl RenderOnce for Serial {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let serial = *window
            .use_keyed_state("serial", cx, |_, _| {
                NEXT_SERIAL.fetch_add(1, Ordering::Relaxed)
            })
            .read(cx);
        self.log.borrow_mut().push((self.key, serial));
        div().size_full()
    }
}

impl LiveGridDelegate for Widgets {
    type Row = Order;

    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Order>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let key = *cell.key();
        self.hooks.calls.borrow_mut().push((key, cell.column()));
        if let Some(change) = cell
            .change()
            .filter(|change| change.change() == gpui_kit_grid::RowChange::Removed)
        {
            self.hooks
                .removed
                .borrow_mut()
                .push((key, change.intensity()));
        }
        if self.hooks.none.get() {
            return None;
        }
        match cell.column() {
            OrderColumn::Quantity => {
                let row = cell.row();
                self.hooks.quantities.borrow_mut().push((key, row.quantity));
                let stopped = self.hooks.stopped.clone();
                Some(
                    div()
                        .id("bar")
                        .size_full()
                        .flex()
                        .items_center()
                        .when(self.hooks.stop.get(), |this| {
                            this.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                stopped.set(stopped.get() + 1);
                                cx.stop_propagation();
                            })
                        })
                        .child(
                            Progress::new("filled")
                                .with_size(cell.size())
                                .value(row.quantity as f32 / 60.),
                        )
                        .into_any_element(),
                )
            }
            OrderColumn::Active => {
                let toggles = self.hooks.toggles.clone();
                if self.hooks.activate.get() {
                    let (grid, seen) = (self.hooks.grid.borrow().clone(), self.hooks.seen.clone());
                    let toggles = toggles.clone();
                    cell.on_activate(move |_, cx| {
                        // Reads the grid: it runs outside the grid's update.
                        let focused =
                            grid.as_ref()
                                .and_then(WeakEntity::upgrade)
                                .and_then(|grid| {
                                    grid.read(cx)
                                        .focused_cell()
                                        .and_then(|cell| cell.row().copied())
                                });
                        seen.borrow_mut().push(focused);
                        toggles.borrow_mut().push(key);
                    });
                }
                Some(
                    Checkbox::new("active")
                        .checked(cell.row().active)
                        .with_size(cell.size())
                        .tab_stop(false)
                        .accessibility_label("Active")
                        .on_click(move |_, _, _| toggles.borrow_mut().push(key))
                        .into_any_element(),
                )
            }
            OrderColumn::Price => Some(
                Serial {
                    key,
                    log: self.hooks.serials.clone(),
                }
                .into_any_element(),
            ),
            _ => None,
        }
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Order>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        self.hooks
            .menus
            .borrow_mut()
            .push(menu_context.target().clone());
        menu_context.standard_items(menu, window, cx)
    }
}

/// The order columns with `Quantity` and `Note` elements and `Active` a
/// control; `widths` replaces column widths, `frozen` freezes columns at
/// the left edge.
fn widget_schema(
    widths: &[(OrderColumn, f32)],
    price: bool,
    frozen: usize,
) -> GridSchema<OrderColumn> {
    ORDER_COLUMNS
        .into_iter()
        .fold(GridSchema::new(), |schema, (column, width)| {
            let width = widths
                .iter()
                .find(|(each, _)| *each == column)
                .map_or(width, |(_, width)| *width);
            let content = match column {
                OrderColumn::Quantity | OrderColumn::Note => CellContent::Element,
                OrderColumn::Price if price => CellContent::Element,
                OrderColumn::Active => CellContent::Control,
                _ => CellContent::Text,
            };
            schema.column(
                GridColumn::new(column, column.id())
                    .with_width(px(width))
                    .with_content(content),
            )
        })
        .with_frozen(frozen)
}

/// The fixture of 60 orders over [`widget_schema`], and its hooks.
fn open_widgets(
    cx: &mut TestAppContext,
    schema: GridSchema<OrderColumn>,
) -> (EditFixture<Widgets>, Hooks) {
    let hooks = Hooks::default();
    let fixture = open_orders_in(
        cx,
        schema,
        orders(60),
        Widgets {
            hooks: hooks.clone(),
        },
        |grid| grid,
    );
    *hooks.grid.borrow_mut() = Some(fixture.grid.downgrade());
    (fixture, hooks)
}

/// Draws a frame after a repaint request: every visible cell is planned.
/// Returns how many frames the grid planned.
fn repaint<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
) -> usize {
    let frames =
        |cx: &mut TestAppContext| fixture.grid.read_with(cx, |grid, _| grid.stats().frames());
    let before = frames(cx);
    fixture.grid.update(cx, |_, cx| cx.notify());
    fixture.frame(cx, Duration::from_millis(20));
    (frames(cx) - before) as usize
}

/// The calls of `frames` frames, each cell once: panics unless every
/// frame made the same calls.
fn calls_per_frame(hooks: &Hooks, frames: usize) -> Vec<(u32, OrderColumn)> {
    let mut calls = hooks.take_calls();
    assert!(
        frames > 0 && calls.len() % frames == 0,
        "{frames} frames: {calls:?}"
    );
    let per_frame = calls.len() / frames;
    calls.sort_unstable_by_key(|(key, column)| (*key, column.ix()));
    calls.dedup();
    assert_eq!(calls.len(), per_frame, "one call per cell and frame");
    calls
}

/// A cell host of the last frame: its row's element id, its own id, its
/// bounds, role and label.
#[derive(Debug)]
struct HostSnapshot {
    row: ElementId,
    id: ElementId,
    bounds: Bounds<Pixels>,
    role: Option<Role>,
    label: Option<String>,
}

/// The cell hosts of the last frame of `window`, top to bottom, left to
/// right.
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
                    role: element.role(),
                    label: element.label().map(str::to_owned),
                }
            })
            .collect();
        hosts.sort_by(|a, b| {
            (a.bounds.origin.y, a.bounds.origin.x)
                .partial_cmp(&(b.bounds.origin.y, b.bounds.origin.x))
                .expect("finite")
        });
        hosts
    })
    .expect("window is open")
}

/// The host of `column` in the row of `key`, if the last frame had one.
fn host_of(hosts: &[HostSnapshot], key: u32, column: OrderColumn) -> Option<&HostSnapshot> {
    let (row, id) = (order_row_id(key), cell_host_id("live-grid-cell", column));
    hosts.iter().find(|host| host.row == row && host.id == id)
}

/// Presses `button` on the cell of `column` on shown line `line`.
fn press<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
    line: usize,
    column: OrderColumn,
    button: MouseButton,
    click_count: usize,
) {
    cx.update_window(fixture.window, |_, window, cx| {
        let at = order_cell(line, column);
        press_at(window, at, button, Modifiers::default(), click_count, cx);
    })
    .expect("window is open");
}

/// Gives the grid the keyboard focus.
fn focus_grid<D: LiveGridDelegate<Row = Order>>(fixture: &EditFixture<D>, cx: &mut TestAppContext) {
    cx.update_window(fixture.window, |_, window, cx| {
        let handle = fixture.grid.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .expect("window is open");
}

/// Returns true if the grid's root holds the keyboard focus.
fn is_root_focused<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
) -> bool {
    cx.update_window(fixture.window, |_, window, cx| {
        fixture.grid.focus_handle(cx).is_focused(window)
    })
    .expect("window is open")
}

fn focused_row<D: LiveGridDelegate<Row = Order>>(
    fixture: &EditFixture<D>,
    cx: &mut TestAppContext,
) -> Option<(u32, OrderColumn)> {
    fixture.grid.read_with(cx, |grid, _| {
        grid.focused_cell()
            .and_then(|cell| Some((*cell.row()?, cell.column())))
    })
}

fn has_click(events: &[LiveGridEvent<u32, OrderColumn>]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, LiveGridEvent::Click { .. }))
}

#[gpui_kit::test]
fn test_element_cells_visible_only(cx: &mut TestAppContext) {
    // `Note` wide enough to scroll the rows sideways.
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[(OrderColumn::Note, 600.)], false, 0));
    hooks.take_calls();
    let frames = repaint(&fixture, cx);
    // One call per visible cell of the three element columns, nothing else.
    let visible = fixture.visible_keys(cx);
    assert_eq!(visible.len(), 10);
    let calls = calls_per_frame(&hooks, frames);
    let expected: Vec<(u32, OrderColumn)> = visible
        .iter()
        .flat_map(|key| {
            [
                OrderColumn::Quantity,
                OrderColumn::Active,
                OrderColumn::Note,
            ]
            .map(|column| (*key, column))
        })
        .collect();
    assert_eq!(calls, expected);
    // `None` from the hook paints text: `Note` has no host.
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.cell_hosts(), visible.len() * 2);
    let hosts = cell_hosts(cx, fixture.window);
    assert_eq!(hosts.len(), visible.len() * 2);
    assert!(host_of(&hosts, visible[0], OrderColumn::Note).is_none());
    assert!(host_of(&hosts, visible[0], OrderColumn::Quantity).is_some());

    // Scrolled sideways past `Quantity`: only the columns on screen.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(300.), cx));
    fixture.settle(cx);
    hooks.take_calls();
    let frames = repaint(&fixture, cx);
    let calls = calls_per_frame(&hooks, frames);
    assert_eq!(calls.len(), visible.len() * 2);
    assert!(
        calls
            .iter()
            .all(|(_, column)| *column != OrderColumn::Quantity)
    );

    // Grouped: data rows only, never a caption.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_scroll_x(px(0.), cx);
        grid.set_group_by([GroupBy::column(OrderColumn::Instrument)], cx);
    });
    fixture.settle(cx);
    hooks.take_calls();
    let frames = repaint(&fixture, cx);
    let visible = fixture.visible_keys(cx);
    let captions = fixture
        .grid
        .read_with(cx, |grid, _| grid.stats().painted_captions());
    assert!(captions > 0 && !visible.is_empty());
    assert_eq!(visible.len() + captions, 10);
    let calls = calls_per_frame(&hooks, frames);
    assert_eq!(calls.len(), visible.len() * 3);
    assert!(calls.iter().all(|(key, _)| visible.contains(key)));
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.cell_hosts(), visible.len() * 2);
}

#[gpui_kit::test]
fn test_element_ids_follow_keys(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], true, 0));
    let serials = |cx: &mut TestAppContext| {
        hooks.serials.take();
        repaint(&fixture, cx);
        let mut serials = hooks.serials.take();
        serials.sort_unstable();
        serials.dedup();
        serials
    };
    let ascending = serials(cx);
    assert_eq!(ascending.len(), 10);
    assert_eq!(ascending[0].0, 1);

    // A re-sort by instrument: orders 4 and 8 (AAPL) stay on screen at
    // other lines and keep their element state; the others are new.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_sort([(OrderColumn::Instrument, SortDirection::Ascending)], cx);
    });
    fixture.settle(cx);
    let resorted = serials(cx);
    assert_eq!(&fixture.visible_keys(cx)[..3], [4, 8, 12]);
    for (key, serial) in &resorted {
        let before = ascending.iter().find(|(each, _)| each == key);
        match key {
            4 | 8 => assert_eq!(before, Some(&(*key, *serial)), "order {key}"),
            _ => assert!(before.is_none(), "order {key}"),
        }
    }
    // Host ids are by key and column.
    let hosts = cell_hosts(cx, fixture.window);
    for key in fixture.visible_keys(cx) {
        assert!(host_of(&hosts, key, OrderColumn::Price).is_some(), "{key}");
    }

    // A removed row has no host; back again, its element starts anew.
    let removed = fixture.visible_keys(cx)[0];
    let old = resorted
        .iter()
        .find(|(key, _)| *key == removed)
        .map(|(_, serial)| *serial);
    let writer = fixture.writer(cx);
    writer.remove(removed);
    fixture.settle(cx);
    let hosts = cell_hosts(cx, fixture.window);
    assert!(host_of(&hosts, removed, OrderColumn::Price).is_none());
    assert!(hosts.iter().all(|host| host.row != order_row_id(removed)));
    let order = orders(60)
        .into_iter()
        .find(|order| order.id == removed)
        .expect("an order");
    writer.upsert(order);
    fixture.settle(cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.scroll_to_key(&removed, cx));
    fixture.settle(cx);
    let again = serials(cx);
    let new = again
        .iter()
        .find(|(key, _)| *key == removed)
        .map(|(_, serial)| *serial);
    assert!(new.is_some() && new != old, "{old:?} → {new:?}");
}

#[gpui_kit::test]
fn test_element_press_selects(cx: &mut TestAppContext) {
    let (fixture, _) = open_widgets(cx, widget_schema(&[], false, 0));
    fixture.take_events();
    // A press the progress bar does not stop is a press on its cell.
    press(&fixture, cx, 2, OrderColumn::Quantity, MouseButton::Left, 1);
    assert_eq!(fixture.selected_keys(cx), [3]);
    assert_eq!(focused_row(&fixture, cx), Some((3, OrderColumn::Quantity)));
    let events = fixture.take_events();
    let focus_change = events
        .iter()
        .position(|event| matches!(event, LiveGridEvent::FocusChange { .. }));
    let click = events.iter().position(|event| {
        matches!(
            event,
            LiveGridEvent::Click {
                target: GridTarget::Cell {
                    key: 3,
                    column: OrderColumn::Quantity
                },
                button: MouseButton::Left,
                ..
            }
        )
    });
    assert!(
        matches!((focus_change, click), (Some(focus), Some(click)) if focus < click),
        "{events:?}"
    );

    // The second press of a series confirms the row (no editor).
    press(&fixture, cx, 2, OrderColumn::Quantity, MouseButton::Left, 2);
    let events = fixture.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            LiveGridEvent::Confirm { keys, .. } if keys[..] == [3]
        )),
        "{events:?}"
    );

    // A drag from a cell with an element selects a range.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_selection_mode(SelectionMode::RowSet, cx);
    });
    cx.update_window(fixture.window, |_, window, cx| {
        let modifiers = Modifiers::default();
        let from = order_cell(4, OrderColumn::Quantity);
        let to = order_cell(7, OrderColumn::Quantity);
        mouse_down_at(window, from, MouseButton::Left, modifiers, 1, cx);
        drag_move_to(window, to, cx);
        mouse_up_at(window, to, MouseButton::Left, modifiers, 1, cx);
    })
    .expect("window is open");
    assert_eq!(fixture.selected_keys(cx), [5, 6, 7, 8]);
}

#[gpui_kit::test]
fn test_element_press_stopped(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], false, 0));
    hooks.stop.set(true);
    repaint(&fixture, cx);
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(1), cx));
    fixture.take_events();

    // The element stops the press: only the focus moves, on release.
    cx.update_window(fixture.window, |_, window, cx| {
        let at = order_cell(3, OrderColumn::Quantity);
        mouse_down_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
    })
    .expect("window is open");
    assert!(fixture.take_events().is_empty());
    cx.update_window(fixture.window, |_, window, cx| {
        let at = order_cell(3, OrderColumn::Quantity);
        mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
    })
    .expect("window is open");
    assert_eq!(hooks.stopped.get(), 1);
    assert_eq!(fixture.selected_keys(cx), [1]);
    assert_eq!(focused_row(&fixture, cx), Some((4, OrderColumn::Quantity)));
    let events = fixture.take_events();
    assert!(
        matches!(&events[..], [LiveGridEvent::FocusChange { .. }]),
        "{events:?}"
    );

    // A stopped double click neither selects nor confirms.
    press(&fixture, cx, 5, OrderColumn::Quantity, MouseButton::Left, 1);
    press(&fixture, cx, 5, OrderColumn::Quantity, MouseButton::Left, 2);
    assert_eq!(hooks.stopped.get(), 3);
    let events = fixture.take_events();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            LiveGridEvent::Confirm { .. } | LiveGridEvent::SelectionChange { .. }
        )) && !has_click(&events),
        "{events:?}"
    );
    assert_eq!(fixture.selected_keys(cx), [1]);
    // The keyboard focus is the grid's.
    assert!(is_root_focused(&fixture, cx));
}

#[gpui_kit::test]
fn test_element_context_press(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], false, 0));
    fixture
        .grid
        .update(cx, |grid, cx| grid.select_key(Some(1), cx));
    fixture.take_events();
    // A right press on the checkbox: the menu of the cell; the row, not
    // selected, is selected first (Windows convention).
    press(&fixture, cx, 3, OrderColumn::Active, MouseButton::Right, 1);
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()));
    assert_eq!(
        hooks.menus.borrow().last(),
        Some(&GridTarget::Cell {
            key: 4,
            column: OrderColumn::Active
        })
    );
    assert_eq!(fixture.selected_keys(cx), [4]);
    assert!(hooks.toggles.borrow().is_empty());
    assert!(!has_click(&fixture.take_events()));
}

#[gpui_kit::test]
fn test_element_returns_focus(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], false, 0));
    assert!(!is_root_focused(&fixture, cx));
    // A click on a checkbox presses it once and selects its row; the
    // keyboard focus goes to the grid, not to the checkbox.
    press(&fixture, cx, 1, OrderColumn::Active, MouseButton::Left, 1);
    assert_eq!(*hooks.toggles.borrow(), [2]);
    assert_eq!(fixture.selected_keys(cx), [2]);
    assert!(is_root_focused(&fixture, cx));
    // The arrows go on from the cell.
    fixture.press_key(cx, "down");
    assert_eq!(focused_row(&fixture, cx), Some((3, OrderColumn::Active)));
    // The grid is one Tab stop: the next stop is not a checkbox.
    cx.update_window(fixture.window, |_, window, cx| window.focus_next(cx))
        .expect("window is open");
    assert!(is_root_focused(&fixture, cx));
}

#[gpui_kit::test]
fn test_activate_cell(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], false, 0));
    hooks.activate.set(true);
    focus_grid(&fixture, cx);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_focused_cell(&3, Some(OrderColumn::Active), cx)
    });
    fixture.settle(cx);
    fixture.take_events();
    // Space presses the focused checkbox once, after the grid's update.
    fixture.press_key(cx, "space");
    assert_eq!(*hooks.toggles.borrow(), [3]);
    assert_eq!(*hooks.seen.borrow(), [Some(3)]);
    assert!(fixture.selected_keys(cx).is_empty());
    assert!(fixture.take_events().is_empty());

    // Without a handler, Space toggles the selection, as on any cell.
    hooks.activate.set(false);
    repaint(&fixture, cx);
    fixture.press_key(cx, "space");
    assert_eq!(hooks.toggles.borrow().len(), 1);
    assert_eq!(fixture.selected_keys(cx), [3]);

    // On an `Element` cell too.
    hooks.activate.set(true);
    fixture.grid.update(cx, |grid, cx| {
        grid.select_key(None, cx);
        grid.set_focused_cell(&5, Some(OrderColumn::Quantity), cx);
    });
    fixture.settle(cx);
    fixture.press_key(cx, "space");
    assert_eq!(hooks.toggles.borrow().len(), 1);
    assert_eq!(fixture.selected_keys(cx), [5]);
}

#[gpui_kit::test]
fn test_element_cells_a11y(cx: &mut TestAppContext) {
    let (fixture, hooks) = open_widgets(cx, widget_schema(&[], false, 0));
    focus_grid(&fixture, cx);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_a11y_forced(true);
        grid.set_focused_cell(&2, Some(OrderColumn::Active), cx);
    });
    let focus_id = cell_host_id("live-grid-focus", OrderColumn::Active);
    let has_focus_element = |cx: &mut TestAppContext| {
        cx.update_window(fixture.window, |_, window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|element| element.path().last() == Some(&focus_id))
        })
        .expect("window is open")
    };

    // Without elements: synthetic cells only, the focus element of its own.
    hooks.none.set(true);
    fixture.settle(cx);
    repaint(&fixture, cx);
    let rows = fixture.visible_keys(cx).len();
    let plain = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert!(has_focus_element(cx));
    assert_eq!(plain.cell_hosts(), 0);

    // With elements: each host is its cell's node; the synthetic cells of
    // `Quantity` and `Active` are gone (the focused one was apart anyway).
    hooks.none.set(false);
    repaint(&fixture, cx);
    let stats = fixture.grid.read_with(cx, |grid, _| grid.stats());
    assert_eq!(stats.a11y_rows(), plain.a11y_rows());
    assert_eq!(stats.a11y_cells(), plain.a11y_cells() - (rows * 2 - 1));
    let hosts = cell_hosts(cx, fixture.window);
    let quantity = host_of(&hosts, 3, OrderColumn::Quantity).expect("a host");
    assert_eq!(quantity.role, Some(Role::GridCell));
    assert_eq!(quantity.label.as_deref(), Some("300"));
    let active = host_of(&hosts, 3, OrderColumn::Active).expect("a host");
    assert_eq!(active.role, Some(Role::GridCell));
    assert_eq!(active.label, None, "a control names itself");
    // The focused cell's host claims the active descendant.
    assert!(!has_focus_element(cx));
    let focused = host_of(&hosts, 2, OrderColumn::Active).expect("a host");
    assert_eq!(focused.role, Some(Role::GridCell));
}

#[gpui_kit::test]
fn test_element_cells_clip(cx: &mut TestAppContext) {
    // `Id` frozen at the left edge; `Note` wide enough to scroll sideways.
    let (fixture, _) = open_widgets(cx, widget_schema(&[(OrderColumn::Note, 400.)], false, 1));
    // `Quantity` (190 to 260 in the content) scrolled to 20 to 90 in the
    // window: its left part is under the frozen `Id` (0 to 50).
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(170.), cx));
    fixture.settle(cx);
    let hosts = cell_hosts(cx, fixture.window);
    let host = host_of(&hosts, 3, OrderColumn::Quantity).expect("a host");
    assert_eq!(host.bounds.origin.x, px(20.));
    let y = host.bounds.center().y;
    let click_column = |x: f32, cx: &mut TestAppContext| {
        fixture.take_events();
        cx.update_window(fixture.window, |_, window, cx| {
            press_at(
                window,
                point(px(x), y),
                MouseButton::Left,
                Modifiers::default(),
                1,
                cx,
            );
        })
        .expect("window is open");
        fixture
            .take_events()
            .into_iter()
            .find_map(|event| match event {
                LiveGridEvent::Click {
                    target: GridTarget::Cell { key, column },
                    ..
                } => Some((key, column)),
                _ => None,
            })
    };
    assert_eq!(click_column(40., cx), Some((3, OrderColumn::Id)));
    assert_eq!(click_column(70., cx), Some((3, OrderColumn::Quantity)));
}

/// A window with an `xsmall` grid in it.
struct GridRoot<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> Render for GridRoot<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .with_size(Size::XSmall)
                .bordered(false),
        )
    }
}

/// An `xsmall` grid of `schema` over a store with `options`, holding
/// `rows`, in a window [`ORDER_WINDOW_WIDTH`] wide: for rows other than the
/// orders, or a store of its own. Draws three frames.
fn open_rows<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    delegate: D,
    options: StoreOptions,
    rows: Vec<D::Row>,
) -> (AnyWindowHandle, Entity<LiveGridState<D>>) {
    open_rows_with(cx, schema, delegate, options, rows, |grid| grid)
}

/// [`open_rows`], the grid configured by `configure`.
fn open_rows_with<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    delegate: D,
    options: StoreOptions,
    rows: Vec<D::Row>,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> (AnyWindowHandle, Entity<LiveGridState<D>>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(ORDER_WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| GridRoot {
                        grid: cx.new(|cx| {
                            configure(LiveGridState::new_standalone(schema, delegate, options, cx))
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    for _ in 0..3 {
        draw(cx, window, &grid);
    }
    (window, grid)
}

/// Runs jobs and timers for 20 ms, then draws a frame of `grid`.
fn draw<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    grid: &Entity<LiveGridState<D>>,
) {
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    grid.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("window is open");
}

#[gpui_kit::test]
fn test_element_cells_live(cx: &mut TestAppContext) {
    let hooks = Hooks::default();
    let options = StoreOptions::default().with_highlight(
        HighlightPolicy::new(Duration::from_millis(800), 8)
            .with_removal_fade(Duration::from_millis(400)),
    );
    let delegate = Widgets {
        hooks: hooks.clone(),
    };
    let (window, grid) = open_rows(
        cx,
        widget_schema(&[], false, 0),
        delegate,
        options,
        orders(20),
    );
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    let frame = |cx: &mut TestAppContext| draw(cx, window, &grid);

    // A new value reaches the element in the next frame.
    let mut order = orders(3).pop().expect("an order");
    order.quantity = 4_200;
    writer.upsert(order);
    hooks.quantities.take();
    frame(cx);
    frame(cx);
    assert!(hooks.quantities.borrow().contains(&(3, 4_200)));

    // A removed row keeps its elements while it fades out: told of its
    // removal, in a row element of its own, and taking no press.
    writer.remove(2);
    frame(cx);
    frame(cx);
    let fading = grid.read_with(cx, |grid, _| grid.stats().fading_lines());
    assert_eq!(fading, 1);
    hooks.take_calls();
    hooks.removed.take();
    frame(cx);
    let calls = hooks.take_calls();
    for column in [OrderColumn::Quantity, OrderColumn::Active] {
        assert!(calls.contains(&(2, column)), "{column:?}: {calls:?}");
    }
    let removed = hooks.removed.take();
    assert!(!removed.is_empty() && removed.iter().all(|(key, _)| *key == 2));
    assert!(
        removed
            .iter()
            .all(|(_, intensity)| *intensity > 0. && *intensity <= 1.)
    );
    let hosts = cell_hosts(cx, window);
    assert!(hosts.iter().all(|host| host.row != order_row_id(2)));
    let fading_hosts: Vec<&HostSnapshot> = hosts
        .iter()
        .filter(|host| host.row == fading_row_id(2))
        .collect();
    // `Quantity` and `Active`; `Note` gives no element.
    assert_eq!(fading_hosts.len(), 2, "{hosts:?}");
    let checkbox = fading_hosts
        .iter()
        .find(|host| host.id == cell_host_id("live-grid-cell", OrderColumn::Active))
        .expect("the fading checkbox");
    let selection = |cx: &mut TestAppContext| {
        grid.read_with(cx, |grid, _| {
            (grid.selection().len(), grid.focused_cell().cloned())
        })
    };
    let selected = selection(cx);
    hooks.toggles.take();
    cx.update_window(window, |_, window, cx| {
        let at = checkbox.bounds.center();
        press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
    })
    .expect("window is open");
    frame(cx);
    assert!(hooks.toggles.borrow().is_empty());
    assert_eq!(selection(cx), selected);
    // The fade ends: the elements go with the row.
    for _ in 0..30 {
        frame(cx);
    }
    hooks.take_calls();
    frame(cx);
    assert!(hooks.take_calls().iter().all(|(key, _)| *key != 2));
    let hosts = cell_hosts(cx, window);
    assert!(hosts.iter().all(|host| host.row != fading_row_id(2)));
    assert!(host_of(&hosts, 3, OrderColumn::Quantity).is_some());
    // The cells of the element columns stay where the columns are.
    let quantity = host_of(&hosts, 3, OrderColumn::Quantity).expect("a host");
    assert_eq!(quantity.bounds.size, size(px(70.), px(ROW)));
}

// Icons, times and values of the application's own type on screen.

/// The columns of an [`Event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum EventColumn {
    /// `Int`.
    Id,
    /// `Icon`.
    Status,
    /// `Timestamp`.
    Placed,
    /// `Custom` ([`Tenor`]).
    Tenor,
}

impl gpui_kit_grid::GridColumns for EventColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Status, Self::Placed, Self::Tenor]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Status => "status",
            Self::Placed => "placed",
            Self::Tenor => "tenor",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A tenor in months, shown "3M" or "2Y" and sorted by length: a value of
/// the application's own type.
#[derive(Clone, Copy, Debug)]
struct Tenor(u32);

impl GridValue for Tenor {
    fn cmp_value(&self, other: &dyn GridValue) -> ValueOrdering {
        match other.downcast_ref::<Tenor>() {
            Some(other) => self.0.cmp(&other.0),
            None => std::any::TypeId::of::<Self>().cmp(&other.as_any().type_id()),
        }
    }

    fn write_text(&self, out: &mut String) {
        use std::fmt::Write as _;
        let _ = if self.0 % 12 == 0 {
            write!(out, "{}Y", self.0 / 12)
        } else {
            write!(out, "{}M", self.0)
        };
    }
}

/// An event: an icon with its text, a time and a tenor.
#[derive(Clone, Debug)]
struct Event {
    id: u32,
    icon: &'static str,
    status: &'static str,
    placed: DateTime<Utc>,
    tenor: Tenor,
}

impl GridRow for Event {
    type Key = u32;
    type Column = EventColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: EventColumn) -> CellValue<'_> {
        match column {
            EventColumn::Id => CellValue::Int(self.id.into()),
            EventColumn::Status => CellValue::Icon(IconValue::new(self.icon, self.status)),
            EventColumn::Placed => CellValue::Timestamp(self.placed),
            EventColumn::Tenor => CellValue::Custom(&self.tenor),
        }
    }
}

/// The icons of the events: one asset that exists, one that does not.
const CHECK: &str = "icons/check.svg";
const MISSING: &str = "icons/no-such-icon.svg";

/// `count` events from 1: "Filled" with a check every other row, else a
/// "Rejected" icon whose asset is missing; placed a minute apart from
/// 2026-11-04 12:34:56 UTC, the first event latest; tenors 1 to 24 months.
fn events(count: u32) -> Vec<Event> {
    let open = Utc
        .with_ymd_and_hms(2026, 11, 4, 12, 34, 56)
        .single()
        .expect("a valid time");
    (1..=count)
        .map(|id| Event {
            id,
            icon: if id % 2 == 1 { CHECK } else { MISSING },
            status: if id % 2 == 1 { "Filled" } else { "Rejected" },
            placed: open - chrono::Duration::minutes(i64::from(id - 1)),
            tenor: Tenor(1 + (id * 7) % 24),
        })
        .collect()
}

/// A delegate that paints the icon of "Filled" in `color`, if any.
#[derive(Default)]
struct Events {
    color: Option<gpui_kit::Hsla>,
}

impl LiveGridDelegate for Events {
    type Row = Event;

    fn cell_style(&self, cell: &CellStyleContext<'_, Event>, style: &mut CellStyle, _: &App) {
        if let (EventColumn::Status, Some(color)) = (cell.column(), self.color)
            && cell.row().status == "Filled"
        {
            style.set_foreground(color);
        }
    }
}

/// The event columns, 60 px for the status and wide enough for a time in
/// `Placed`, with the status aligned by `align`.
fn event_schema(align: ColumnAlign) -> GridSchema<EventColumn> {
    GridSchema::new()
        .column(GridColumn::new(EventColumn::Id, "Id").with_width(px(50.)))
        .column(
            GridColumn::new(EventColumn::Status, "Status")
                .with_width(px(60.))
                .with_align(align),
        )
        .column(GridColumn::new(EventColumn::Placed, "Placed").with_width(px(200.)))
        .column(GridColumn::new(EventColumn::Tenor, "Tenor").with_width(px(60.)))
}

/// The grid of 30 [`events`].
fn open_events(
    cx: &mut TestAppContext,
    schema: GridSchema<EventColumn>,
    delegate: Events,
) -> (AnyWindowHandle, Entity<LiveGridState<Events>>) {
    open_rows(cx, schema, delegate, StoreOptions::default(), events(30))
}

/// The icons of icon values the last frame painted: bounds and path, top
/// to bottom.
fn status_icons(
    cx: &mut TestAppContext,
    grid: &Entity<LiveGridState<Events>>,
) -> Vec<(Bounds<Pixels>, String, gpui_kit::Hsla)> {
    grid.read_with(cx, |grid, _| {
        let mut icons: Vec<_> = grid
            .painted_icons()
            .iter()
            .filter(|(_, path, _)| [CHECK, MISSING].contains(&path.as_ref()))
            .map(|(bounds, path, color)| (*bounds, path.to_string(), *color))
            .collect();
        icons.sort_by(|a, b| a.0.origin.y.partial_cmp(&b.0.origin.y).expect("finite"));
        icons
    })
}

/// The centre of the cell of `column` on shown line `line` of the event
/// grid (`Id` 50 px, `Status` 60, `Placed` 200, `Tenor` 60).
fn event_cell(line: usize, column: EventColumn) -> gpui_kit::Point<Pixels> {
    let x = match column {
        EventColumn::Id => 25.,
        EventColumn::Status => 80.,
        EventColumn::Placed => 210.,
        EventColumn::Tenor => 340.,
    };
    point(px(x), px(ROW * (line as f32 + 1.5)))
}

/// The tooltip text the grid plans for the cell under the pointer at
/// `position`.
fn tooltip_at(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    grid: &Entity<LiveGridState<Events>>,
    position: gpui_kit::Point<Pixels>,
) -> Option<String> {
    cx.update_window(window, |_, window, cx| {
        common::move_pointer_to(window, position, cx);
    })
    .expect("window is open");
    draw(cx, window, grid);
    grid.read_with(cx, |grid, _| {
        grid.hovered_tooltip_text().map(|text| text.to_string())
    })
}

#[gpui_kit::test]
fn test_icon_cell(cx: &mut TestAppContext) {
    let color = gpui_kit::hsla(0.3, 0.6, 0.4, 1.);
    let (window, grid) = open_events(
        cx,
        event_schema(ColumnAlign::Auto),
        Events { color: Some(color) },
    );
    // One 14 px icon per painted row (xsmall), in the middle of the cell
    // (`Auto`) and of the row, as no text.
    let icons = status_icons(cx, &grid);
    assert_eq!(icons.len(), 10);
    let (bounds, path, painted) = &icons[0];
    assert_eq!(path, CHECK);
    assert_eq!(bounds.size, size(px(14.), px(14.)));
    assert_eq!(bounds.origin, point(px(50. + 23.), px(ROW + 6.)));
    // `cell_style` paints a status in its colour; others take the text's.
    assert_eq!(*painted, color);
    assert_ne!(icons[1].2, color);
    let stats = grid.read_with(cx, |grid, _| grid.stats());
    assert!(stats.icons() >= 10, "{}", stats.icons());
    // The text is the tooltip of the icon, as of a truncated cell.
    assert_eq!(
        tooltip_at(cx, window, &grid, event_cell(0, EventColumn::Status)).as_deref(),
        Some("Filled")
    );
    // And a screen reader's label of the cell.
    grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    draw(cx, window, &grid);
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&event_row_id(2)));
    assert_eq!(cells[1].0, "Rejected");

    // At the start of the cell for `Start`.
    let (_, grid) = open_events(cx, event_schema(ColumnAlign::Start), Events::default());
    let icons = status_icons(cx, &grid);
    assert_eq!(icons[0].0.origin.x, px(50. + 6.));
}

/// The element id of the row element of the event `key`.
fn event_row_id(key: u32) -> ElementId {
    order_row_id(key)
}

#[gpui_kit::test]
fn test_icon_cell_in_tree_and_pinned(cx: &mut TestAppContext) {
    let (window, grid) = open_events(cx, event_schema(ColumnAlign::Start), Events::default());
    // A tree row's start in the status column: the icon comes after the
    // indent and the chevron lane, which keeps its chevron.
    grid.update(cx, |grid, _| {
        grid.set_test_cell_lead(
            EventColumn::Status,
            px(20.),
            Some("icons/chevron-right.svg".into()),
        )
    });
    draw(cx, window, &grid);
    let icons = status_icons(cx, &grid);
    assert_eq!(icons[0].0.origin.x, px(50. + 20. + 6.));
    let chevrons = grid.read_with(cx, |grid, _| {
        grid.painted_icons()
            .iter()
            .filter(|(_, path, _)| path.as_ref() == "icons/chevron-right.svg")
            .count()
    });
    assert_eq!(chevrons, 10);

    // A pinned row paints its icon in its band, as a data row.
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    let mut pinned = events(1).pop().expect("an event");
    pinned.id = 100;
    writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(pinned)]);
    draw(cx, window, &grid);
    draw(cx, window, &grid);
    let icons = status_icons(cx, &grid);
    assert_eq!(icons.len(), 10);
    assert_eq!(icons[0].0.origin.y, px(ROW + 6.), "the band's row");
    assert_eq!(icons[1].0.origin.y, px(ROW * 2. + 6.), "the first data row");
}

#[gpui_kit::test]
fn test_icon_missing_asset(cx: &mut TestAppContext) {
    let (window, grid) = open_events(cx, event_schema(ColumnAlign::Auto), Events::default());
    // The frames drew the missing icon's cell without a panic; the icon is
    // planned and paints nothing. Its text is in the tooltip and the label.
    let icons = status_icons(cx, &grid);
    assert_eq!(icons[1].1, MISSING);
    assert_eq!(
        tooltip_at(cx, window, &grid, event_cell(1, EventColumn::Status)).as_deref(),
        Some("Rejected")
    );
    grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    draw(cx, window, &grid);
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&event_row_id(2)));
    assert_eq!(cells[1].0, "Rejected");
}

#[gpui_kit::test]
fn test_timestamp_and_custom_cells(cx: &mut TestAppContext) {
    let (window, grid) = open_events(cx, event_schema(ColumnAlign::Auto), Events::default());
    grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    draw(cx, window, &grid);
    // The time in UTC and the application's own text, as the cells show.
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&event_row_id(1)));
    assert_eq!(cells[2].0, "2026-11-04 12:34:56");
    assert_eq!(cells[3].0, "8M");
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&event_row_id(5)));
    assert_eq!(cells[3].0, "1Y");
    // Times fit their column: no tooltip.
    assert_eq!(
        tooltip_at(cx, window, &grid, event_cell(0, EventColumn::Placed)),
        None
    );

    // An unchanged time is laid out from the glyph table of numbers: a
    // repaint shapes nothing.
    let counters = |cx: &mut TestAppContext| {
        grid.read_with(cx, |grid, _| {
            let text = grid.text_stats();
            (grid.stats().frames(), text.misses(), text.numeric())
        })
    };
    let (frames, misses, numeric) = counters(cx);
    draw(cx, window, &grid);
    let (frames_after, misses_after, numeric_after) = counters(cx);
    assert_eq!(misses_after, misses);
    // Ten ids and ten times a frame.
    assert_eq!(numeric_after - numeric, (frames_after - frames) * 20);

    // Sorted by time, the earliest first; by tenor, the shortest first.
    let visible = |cx: &mut TestAppContext| grid.read_with(cx, |grid, _| grid.visible_keys());
    grid.update(cx, |grid, cx| {
        grid.set_sort([(EventColumn::Placed, SortDirection::Ascending)], cx)
    });
    draw(cx, window, &grid);
    assert_eq!(visible(cx)[..3], [30, 29, 28]);
    grid.update(cx, |grid, cx| {
        grid.set_sort([(EventColumn::Tenor, SortDirection::Ascending)], cx)
    });
    draw(cx, window, &grid);
    let tenors: Vec<u32> = visible(cx).iter().map(|key| 1 + (key * 7) % 24).collect();
    assert!(tenors.is_sorted(), "{tenors:?}");

    // The quick filter reads the time's text.
    grid.update(cx, |grid, cx| {
        grid.set_sort([(EventColumn::Id, SortDirection::Ascending)], cx);
        grid.set_quick_filter("12:3", cx);
    });
    draw(cx, window, &grid);
    draw(cx, window, &grid);
    // 12:34:56 down to 12:30:56: the first five events.
    assert_eq!(visible(cx), [1, 2, 3, 4, 5]);
}

#[gpui_kit::test]
fn test_icon_autofit(cx: &mut TestAppContext) {
    // Long texts behind the icons, and the same texts in a text column.
    let schema = event_schema(ColumnAlign::Auto);
    let (window, grid) = open_events(cx, schema, Events::default());
    grid.update(cx, |grid, cx| grid.autofit_column(EventColumn::Status, cx));
    draw(cx, window, &grid);
    let width = grid.read_with(cx, |grid, _| grid.column_width(EventColumn::Status));
    // The header's "Status" with its sort lane is wider than the icon, so
    // the title sets the floor; the texts ("Rejected") do not widen it.
    let header_only = {
        let schema = GridSchema::new()
            .column(GridColumn::new(EventColumn::Status, "Status").with_width(px(200.)));
        let (window, grid) = open_rows(
            cx,
            schema,
            Events::default(),
            StoreOptions::default(),
            Vec::new(),
        );
        grid.update(cx, |grid, cx| grid.autofit_column(EventColumn::Status, cx));
        draw(cx, window, &grid);
        grid.read_with(cx, |grid, _| grid.column_width(EventColumn::Status))
    };
    assert_eq!(width, header_only);

    // Without a title to keep room for, the column fits the icon and its
    // padding: 14 + 2 × 6 + 1.
    let schema = GridSchema::new().column(
        GridColumn::new(EventColumn::Status, "")
            .with_width(px(200.))
            .with_sortable(false),
    );
    let (window, grid) = open_rows(
        cx,
        schema,
        Events::default(),
        StoreOptions::default(),
        events(30),
    );
    grid.update(cx, |grid, cx| grid.autofit_column(EventColumn::Status, cx));
    draw(cx, window, &grid);
    let width = grid.read_with(cx, |grid, _| grid.column_width(EventColumn::Status));
    assert_eq!(width, px(27.));
}

// Links in cells: painted by the body, opened by a click on the text and
// by Enter, copied as their text, read as links.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PageColumn {
    Id,
    Page,
}

impl gpui_kit_grid::GridColumns for PageColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Page]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Page => "page",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A page with a short title and its address.
#[derive(Clone, Debug)]
struct Page {
    id: u32,
    title: String,
    url: String,
}

impl GridRow for Page {
    type Key = u32;
    type Column = PageColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: PageColumn) -> CellValue<'_> {
        match column {
            PageColumn::Id => CellValue::Int(self.id.into()),
            PageColumn::Page => CellValue::Link(LinkValue::new(&self.title, &self.url)),
        }
    }
}

struct Pages;

impl LiveGridDelegate for Pages {
    type Row = Page;
}

/// Records the addresses it opens.
struct Recorded(Rc<RefCell<Vec<String>>>);

impl LinkOpener for Recorded {
    fn open(&self, url: &str, _: &mut App) {
        self.0.borrow_mut().push(url.to_owned());
    }
}

#[gpui_kit::test]
fn test_link_cells(cx: &mut TestAppContext) {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let pages = (1..=10)
        .map(|id| Page {
            id,
            title: format!("P{id}"),
            url: format!("https://example.org/{id}"),
        })
        .collect();
    let schema = GridSchema::new()
        .column(GridColumn::new(PageColumn::Id, "Id").with_width(px(50.)))
        .column(GridColumn::new(PageColumn::Page, "Page").with_width(px(300.)));
    let (window, grid) = open_rows_with(cx, schema, Pages, StoreOptions::default(), pages, {
        let opened = opened.clone();
        move |grid| grid.with_link_opener(Recorded(opened))
    });
    let frame = |cx: &mut TestAppContext| draw(cx, window, &grid);
    // Line 0 is page 1; its text "P1" starts after the cell's padding.
    let on_text = point(px(50. + 9.), px(ROW * 1.5));
    let past_text = point(px(50. + 280.), px(ROW * 1.5));
    let input = |cx: &mut TestAppContext, f: &dyn Fn(&mut Window, &mut App)| {
        cx.update_window(window, |_, window, cx| f(window, cx))
            .expect("window is open");
    };

    // The pointer over the text is over the link; past it, not.
    input(cx, &|window, cx| move_pointer_to(window, on_text, cx));
    frame(cx);
    assert!(grid.read_with(cx, |grid, _| grid.is_link_hovered()));
    input(cx, &|window, cx| move_pointer_to(window, past_text, cx));
    frame(cx);
    assert!(!grid.read_with(cx, |grid, _| grid.is_link_hovered()));

    // A click on the text opens the address; on the rest of the cell, or
    // with a modifier, it does not.
    input(cx, &|window, cx| {
        press_at(
            window,
            on_text,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    frame(cx);
    assert_eq!(*opened.borrow(), ["https://example.org/1"]);
    input(cx, &|window, cx| {
        press_at(
            window,
            past_text,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        )
    });
    input(cx, &|window, cx| {
        press_at(
            window,
            on_text,
            MouseButton::Left,
            Modifiers::shift(),
            1,
            cx,
        )
    });
    // The second press of a double click does not open it again.
    input(cx, &|window, cx| {
        press_at(
            window,
            on_text,
            MouseButton::Left,
            Modifiers::default(),
            2,
            cx,
        )
    });
    frame(cx);
    assert_eq!(opened.borrow().len(), 1);

    // Enter on the focused link opens it; on another cell it confirms.
    input(cx, &|window, cx| {
        let handle = grid.focus_handle(cx);
        window.focus(&handle, cx);
    });
    grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&3, Some(PageColumn::Page), cx));
    });
    frame(cx);
    input(cx, &|window, cx| window.press("enter", cx));
    frame(cx);
    assert_eq!(
        opened.borrow().last().map(String::as_str),
        Some("https://example.org/3")
    );
    let confirms = Rc::new(Cell::new(0));
    cx.update(|cx| {
        let confirms = confirms.clone();
        cx.subscribe(
            &grid,
            move |_, event: &LiveGridEvent<u32, PageColumn>, _| {
                if matches!(event, LiveGridEvent::Confirm { .. }) {
                    confirms.set(confirms.get() + 1);
                }
            },
        )
        .detach();
    });
    grid.update(cx, |grid, cx| {
        assert!(grid.set_focused_cell(&3, Some(PageColumn::Id), cx));
    });
    frame(cx);
    input(cx, &|window, cx| window.press("enter", cx));
    frame(cx);
    assert_eq!(opened.borrow().len(), 2);
    assert_eq!(confirms.get(), 1);

    // A double click on the link's text opens it once and confirms
    // nothing; on another cell of the row it confirms.
    let id_cell = point(px(25.), px(ROW * 1.5));
    for (at, opens, confirms_after) in [(on_text, 3, 1), (id_cell, 3, 2)] {
        input(cx, &|window, cx| {
            move_pointer_to(window, at, cx);
            press_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
            press_at(window, at, MouseButton::Left, Modifiers::default(), 2, cx);
        });
        frame(cx);
        assert_eq!(opened.borrow().len(), opens);
        assert_eq!(confirms.get(), confirms_after);
    }

    // A copy writes the text; a screen reader hears "link".
    grid.update(cx, |grid, cx| {
        grid.set_selected_keys([3], cx);
        assert!(grid.copy(cx));
    });
    frame(cx);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("3\tP3"));
    grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    frame(cx);
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&event_row_id(3)));
    assert!(
        cells.iter().any(|(label, description)| label == "P3"
            && description
                .as_deref()
                .is_some_and(|text| text.contains("link"))),
        "{cells:?}"
    );
}
