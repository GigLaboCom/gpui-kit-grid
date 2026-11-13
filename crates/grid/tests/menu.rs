//! Headless UI tests of the context menu, the primary command and tooltips
//! (`.agents/COMPONENT_TEST_RULES.md`): the menu target captured at the press,
//! the order held while the menu is open, the menu closing with its target row,
//! the keyboard menu and its sort items, the header menu's column items, the
//! primary command's `Confirm`, and cell and header tooltips under live data;
//! the menu, tooltips, zones and highlight of pinned rows; the menus of the
//! totals row and group footers. The test window builds menus and tooltips but
//! shows nothing: assertions read the grid, its events, the delegate's hook
//! calls and the menu items' accessibility labels, not pixels.

mod common;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{Sizable as _, menu::PopupMenu};
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _,
    InputEvent, IntoElement, KeyBinding, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, Point, Render, Role, SharedString, Styled as _, TestAppContext,
    Window, WindowBounds, WindowOptions, div, point, px, size,
};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, CellTooltip, CellTooltipContext, CellValue, ColumnIx, ColumnZone,
    GridColumn, GridColumns, GridMenuContext, GridRow, GridSchema, GridTarget, GridTuning,
    HeaderTooltipContext, LayoutOrigin, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState,
    ManualReorder, PinnedEdge, RowChange, SortDirection, SortOrigin, StoreOptions, ViewSpec,
    actions::{ClearAllSorting, KEY_CONTEXT, ShowContextMenu, SortAscending},
};

use crate::common::{
    GroupedColumn, GroupedRow, ROW as GROUPED_ROW, group_path, grouped_cell, grouped_rows,
    menu_labels, mouse_down_at, mouse_up_at, open_grouped_with, press_at,
};

const ROW: f32 = 26.;
/// Wide and tall enough for the header menu and its Sort submenu on the
/// right (`PopupMenu` opens a submenu leftwards when its 500 px maximum
/// width does not fit): a header and 22 `xsmall` rows.
const WIDTH: f32 = 800.;
const HEIGHT: f32 = ROW * 23.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Col {
    Id,
    Price,
    Size,
    Note,
}

impl GridColumns for Col {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Price, Self::Size, Self::Note]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Price => "price",
            Self::Size => "size",
            Self::Note => "note",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A note too long for the 80 px Note column.
const LONG_NOTE: &str = "A note far too long for its column";

#[derive(Clone, Debug)]
struct Item {
    id: u32,
    price: i64,
}

impl GridRow for Item {
    type Key = u32;
    type Column = Col;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: Col) -> CellValue<'_> {
        match column {
            Col::Id => CellValue::Int(self.id.into()),
            Col::Price => CellValue::Int(self.price),
            Col::Size => CellValue::Int(100),
            Col::Note if self.id % 2 == 0 => CellValue::Text(LONG_NOTE),
            Col::Note => CellValue::Text("ok"),
        }
    }
}

/// What the delegate's hooks were called with.
#[derive(Default)]
struct Log {
    /// `context_menu`: the target and the command scope.
    menus: Vec<(GridTarget<u32, Col>, Vec<u32>)>,
    /// `primary_command` calls.
    primaries: usize,
    /// `tooltip`: the row, the column, the painted text, truncated.
    tooltips: Vec<(u32, Col, String, bool)>,
    /// Text drawn by shown tooltips.
    drawn: Vec<String>,
    /// `header_tooltip`: the column, truncated.
    headers: Vec<(Col, bool)>,
    /// `cell_style` of pinned cells: the band, the row, the column, the
    /// position in the band, the highlighted change, selected.
    pinned_styles: Vec<(PinnedEdge, u32, Col, usize, Option<RowChange>, bool)>,
}

/// A delegate that records its hooks. Price cells get a tooltip with the
/// price, drawn through the slot, so a test sees a shown tooltip draw;
/// other cells get the default (the full text when truncated).
struct Items {
    log: Rc<RefCell<Log>>,
    primary: Rc<Cell<Option<&'static str>>>,
}

impl LiveGridDelegate for Items {
    type Row = Item;

    fn cell_style(&self, cell: &CellStyleContext<'_, Item>, _: &mut CellStyle, _: &App) {
        if let Some(edge) = cell.pinned_edge() {
            self.log.borrow_mut().pinned_styles.push((
                edge,
                cell.row().id,
                cell.column(),
                cell.display_ix(),
                cell.change().map(|change| change.change()),
                cell.is_selected(),
            ));
        }
    }

    fn primary_command(&self, _: &GridMenuContext<'_, Item>, _: &App) -> Option<SharedString> {
        self.log.borrow_mut().primaries += 1;
        self.primary.get().map(SharedString::new_static)
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Item>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        if let Some(key) = menu_context.target().key() {
            assert!(menu_context.row(key).is_some(), "the target row is shown");
        }
        if let GridTarget::Pinned { key, .. } = menu_context.target() {
            assert_eq!(
                menu_context.target_row().map(|row| row.id),
                Some(*key),
                "the pinned row, not a data row with its key"
            );
        }
        self.log.borrow_mut().menus.push((
            menu_context.target().clone(),
            menu_context.selection().keys().to_vec(),
        ));
        menu_context.standard_items(menu, window, cx)
    }

    fn tooltip(&self, cell: &CellTooltipContext<'_, Item>, _: &App) -> Option<CellTooltip> {
        self.log.borrow_mut().tooltips.push((
            cell.row().id,
            cell.column(),
            cell.text().to_owned(),
            cell.is_truncated(),
        ));
        if cell.column() == Col::Price {
            let text = format!("Price {}", cell.text());
            let log = self.log.clone();
            return Some(CellTooltip::element(move |_, _| {
                log.borrow_mut().drawn.push(text.clone());
                div().child(text.clone()).into_any_element()
            }));
        }
        cell.default_tooltip()
    }

    fn header_tooltip(
        &self,
        header: &HeaderTooltipContext<'_, Col>,
        _: &App,
    ) -> Option<CellTooltip> {
        self.log
            .borrow_mut()
            .headers
            .push((header.column(), header.is_truncated()));
        header.default_tooltip()
    }
}

/// Id 60 px, Price 100, Size 100 (not sortable), Note 80 with a title too
/// long for it: x 0..60, 60..160, 160..260, 260..340.
fn schema() -> GridSchema<Col> {
    GridSchema::new()
        .column(GridColumn::new(Col::Id, "Id").with_width(px(60.)))
        .column(GridColumn::new(Col::Price, "Price").with_width(px(100.)))
        .column(
            GridColumn::new(Col::Size, "Size")
                .with_width(px(100.))
                .with_sortable(false),
        )
        .column(GridColumn::new(Col::Note, "Notes for the trader").with_width(px(80.)))
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

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Items>>,
    events: Rc<RefCell<Vec<LiveGridEvent<u32, Col>>>>,
    log: Rc<RefCell<Log>>,
    primary: Rc<Cell<Option<&'static str>>>,
}

/// `count` rows (`price = id * 10`) in a window of a header and ten
/// `xsmall` rows, configured by `configure`.
fn open(
    cx: &mut TestAppContext,
    count: u32,
    configure: impl FnOnce(LiveGridState<Items>) -> LiveGridState<Items> + 'static,
) -> Fixture {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let log = Rc::new(RefCell::new(Log::default()));
    let primary = Rc::new(Cell::new(None));
    let (window, root) = cx
        .update(|cx| {
            let (log, primary) = (log.clone(), primary.clone());
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                move |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(
                                LiveGridState::new_standalone(
                                    schema(),
                                    Items { log, primary },
                                    StoreOptions::default(),
                                    cx,
                                )
                                .with_tuning(
                                    GridTuning::default().with_min_rebuild_interval(Duration::ZERO),
                                ),
                            )
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
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<u32, Col>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all((0..count).map(|id| {
        Arc::new(Item {
            id,
            price: i64::from(id) * 10,
        })
    }));
    let fixture = Fixture {
        window,
        grid,
        events,
        log,
        primary,
    };
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    fixture
}

fn by_price(direction: SortDirection) -> ViewSpec<Item> {
    ViewSpec::new().with_sort([(Col::Price, direction)])
}

/// The center of a body cell on screen line `line`.
fn cell(line: usize, column: Col) -> Point<Pixels> {
    point(column_x(column), px(ROW * (line as f32 + 1.5)))
}

/// The center of a header cell.
fn header(column: Col) -> Point<Pixels> {
    point(column_x(column), px(ROW / 2.))
}

fn column_x(column: Col) -> Pixels {
    px(match column {
        Col::Id => 30.,
        Col::Price => 110.,
        Col::Size => 210.,
        Col::Note => 300.,
    })
}

/// Sends one event without drawing: the test window draws only when
/// something became dirty, so a frame count shows whether it did.
fn send(window: &mut Window, event: impl InputEvent, cx: &mut App) {
    window.dispatch_event(event.to_platform_input(), cx);
}

fn move_event(position: Point<Pixels>) -> MouseMoveEvent {
    MouseMoveEvent {
        position,
        pressed_button: None,
        modifiers: Modifiers::default(),
    }
}

impl Fixture {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            self.frame(cx, Duration::from_millis(20));
        }
    }

    fn frame(&self, cx: &mut TestAppContext, elapsed: Duration) {
        cx.executor().advance_clock(elapsed);
        cx.run_until_parked();
        self.input(cx, |window, cx| window.render_frame(cx));
    }

    fn input<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    fn focus(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.render_frame(cx);
        });
    }

    fn is_grid_focused(&self, cx: &mut TestAppContext) -> bool {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| grid.focus_handle(cx).is_focused(window))
    }

    /// Presses keys one by one, each in its own update, and settles.
    fn press(&self, cx: &mut TestAppContext, keys: &str) {
        for key in keys.split(' ') {
            self.input(cx, |window, cx| window.press(key, cx));
            self.settle(cx);
        }
    }

    fn click(&self, cx: &mut TestAppContext, position: Point<Pixels>, button: MouseButton) {
        self.input(cx, |window, cx| {
            press_at(window, position, button, Modifiers::default(), 1, cx)
        });
        self.settle(cx);
    }

    fn right_click(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
        self.click(cx, position, MouseButton::Right);
    }

    /// Moves the pointer to `position`, draws, and moves it by a pixel
    /// within the same cell: GPUI schedules a tooltip on a move over an
    /// element that has one, and the first move is what plans that element.
    fn hover(&self, cx: &mut TestAppContext, position: Point<Pixels>) {
        self.input(cx, |window, cx| {
            send(window, move_event(position), cx);
            window.render_frame(cx);
            send(window, move_event(position + point(px(1.), px(0.))), cx);
            window.render_frame(cx);
        });
    }

    fn is_menu_open(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_menu_open())
    }

    fn is_tooltip_shown(&self, cx: &mut TestAppContext) -> bool {
        self.grid.read_with(cx, |grid, _| grid.is_tooltip_shown())
    }

    fn visible_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| grid.visible_keys())
    }

    fn selected_keys(&self, cx: &mut TestAppContext) -> Vec<u32> {
        self.grid.read_with(cx, |grid, _| {
            let mut keys: Vec<u32> = grid.selection().iter().copied().collect();
            keys.sort_unstable();
            keys
        })
    }

    fn last_menu(&self) -> (GridTarget<u32, Col>, Vec<u32>) {
        self.log
            .borrow()
            .menus
            .last()
            .cloned()
            .expect("a menu was built")
    }

    /// The items of the open menus, the top-level menu first, each top to
    /// bottom (separators and custom items have no label).
    fn menu_items(&self, cx: &mut TestAppContext) -> Vec<ElementSnapshot> {
        self.input(cx, |window, _| {
            let mut items: Vec<ElementSnapshot> = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
                .collect();
            // A submenu's items have the longer element path.
            items.sort_by(|a, b| {
                (a.path().len(), a.bounds().origin.y)
                    .partial_cmp(&(b.path().len(), b.bounds().origin.y))
                    .expect("finite")
            });
            items
        })
    }

    /// The labels of the top-level menu's items.
    fn menu_labels(&self, cx: &mut TestAppContext) -> Vec<String> {
        let submenu = ElementId::Name("submenu".into());
        self.menu_items(cx)
            .iter()
            .filter(|item| !item.path().contains(&submenu))
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Clicks the menu item labelled `label`.
    fn click_item(&self, cx: &mut TestAppContext, label: &str) {
        let item = self
            .menu_items(cx)
            .into_iter()
            .find(|item| item.label() == Some(label))
            .unwrap_or_else(|| panic!("no menu item {label:?}"));
        self.click(cx, item.bounds().center(), MouseButton::Left);
    }

    fn sort_changes(&self) -> Vec<(Vec<(Col, SortDirection)>, SortOrigin)> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::SortChange { sort, origin } => Some((sort.to_vec(), *origin)),
                _ => None,
            })
            .collect()
    }

    fn confirms(&self) -> Vec<(Vec<u32>, bool)> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::Confirm { keys, secondary } => Some((keys.to_vec(), *secondary)),
                _ => None,
            })
            .collect()
    }

    fn held_events(&self) -> Vec<bool> {
        self.events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::OrderHeldChange { held } => Some(*held),
                _ => None,
            })
            .collect()
    }

    /// Writes `price = sign * id * 10` to rows `0..count`: flipping the sign
    /// reverses the price order.
    fn write_prices(&self, cx: &mut TestAppContext, count: u32, sign: i64) {
        let writer = self.grid.read_with(cx, |grid, _| grid.writer());
        for id in 0..count {
            writer.upsert(Item {
                id,
                price: sign * i64::from(id) * 10,
            });
        }
    }
}

#[gpui_kit::test]
fn test_menu_target_captured_at_mouse_down(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| {
        grid.with_view(by_price(SortDirection::Ascending))
            .with_reorder_policy(ManualReorder)
    });
    // A reversed order is ready and held: line 2 still shows key 2.
    fixture.write_prices(cx, 10, -1);
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(fixture.visible_keys(cx)[2], 2);

    // The press captures key 2; before the menu is built (after the event)
    // the order flips, and key 7 is on line 2.
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        send(window, move_event(cell(2, Col::Price)), cx);
        send(
            window,
            MouseDownEvent {
                button: MouseButton::Right,
                position: cell(2, Col::Price),
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            },
            cx,
        );
        grid.update(cx, |grid, cx| grid.apply_held_order(cx));
    });
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[2], 7);
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.last_menu(),
        (
            GridTarget::Cell {
                key: 2,
                column: Col::Price
            },
            vec![2]
        ),
        "the target and the selection of the press, not the row now under the pointer"
    );
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Price)
    );

    // A header cell: `Header`, and the selection does not change.
    fixture.right_click(cx, header(Col::Note));
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Header { column: Col::Note }
    );
    assert_eq!(fixture.selected_keys(cx), [2]);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Note)
    );
    // Right of the columns: `Empty`.
    fixture.press(cx, "escape");
    fixture.right_click(cx, point(px(380.), px(ROW / 2.)));
    assert_eq!(fixture.last_menu().0, GridTarget::Empty);

    // Below the rows: `Empty`.
    let short = open(cx, 3, |grid| grid);
    short.right_click(cx, cell(6, Col::Price));
    assert!(short.is_menu_open(cx));
    assert_eq!(short.last_menu(), (GridTarget::Empty, vec![]));
    assert_eq!(GridTarget::<u32, Col>::Empty.key(), None);
    assert_eq!(
        GridTarget::<u32, Col>::Cell {
            key: 1,
            column: Col::Id
        }
        .column(),
        Some(Col::Id)
    );
}

#[gpui_kit::test]
fn test_menu_holds_order_and_closes_with_target(cx: &mut TestAppContext) {
    // `LiveReorder`, the default: no hold without the menu.
    let fixture = open(cx, 10, |grid| {
        grid.with_view(by_price(SortDirection::Ascending))
    });
    fixture.right_click(cx, cell(2, Col::Price));
    assert!(fixture.is_menu_open(cx));
    assert!(!fixture.is_grid_focused(cx), "the menu has the focus");

    // A new order arrives while the menu is open: held.
    fixture.write_prices(cx, 10, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx), (0..10).collect::<Vec<_>>());
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    assert_eq!(fixture.held_events(), [true]);

    // Escape closes the menu: the order applies, the grid has the focus.
    fixture.press(cx, "escape");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.held_events(), [true, false]);
    assert_eq!(fixture.visible_keys(cx), (0..10).rev().collect::<Vec<_>>());
    assert!(fixture.is_grid_focused(cx));

    // A menu on key 9 (line 0); a press on another row, below the menu,
    // closes it and changes nothing else.
    fixture.right_click(cx, cell(0, Col::Price));
    assert_eq!(fixture.selected_keys(cx), [9]);
    assert!(fixture.is_menu_open(cx));
    fixture.click(cx, cell(9, Col::Price), MouseButton::Left);
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(
        fixture.selected_keys(cx),
        [9],
        "the press only closed the menu"
    );
    assert!(fixture.is_grid_focused(cx));
    // The next press selects as usual.
    fixture.click(cx, cell(4, Col::Price), MouseButton::Left);
    assert_eq!(fixture.selected_keys(cx), [5]);

    // A second right press (left of the first menu) replaces the menu with
    // one for its own target.
    fixture.right_click(cx, cell(0, Col::Price));
    fixture.right_click(cx, cell(3, Col::Id));
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Cell {
            key: 6,
            column: Col::Id
        }
    );

    // The target row leaves the store: the menu closes in the next frame
    // and the grid gets the focus back.
    fixture
        .grid
        .read_with(cx, |grid, _| grid.writer())
        .remove(6);
    fixture.settle(cx);
    assert!(!fixture.is_menu_open(cx));
    assert!(fixture.is_grid_focused(cx));

    // Another row of the scope leaving keeps the menu.
    fixture.right_click(cx, cell(0, Col::Price));
    fixture
        .grid
        .read_with(cx, |grid, _| grid.writer())
        .remove(3);
    fixture.settle(cx);
    assert!(fixture.is_menu_open(cx));
}

#[gpui_kit::test]
fn test_pinned_rows_menu_tooltip_and_zones(cx: &mut TestAppContext) {
    // Id frozen left; Note 800 px wide, so the scrolling zone (Price, Size,
    // Note from x 60) scrolls sideways.
    let fixture = open(cx, 30, |grid| grid);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_zone(Col::Id, ColumnZone::FrozenLeft, cx);
        grid.set_column_width(Col::Note, px(800.), cx);
    });
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    // The top band reuses data key 0: pinned keys are a space of their own.
    let pin_top = |id: u32, price: i64| {
        writer.set_pinned_rows(PinnedEdge::Top, [Arc::new(Item { id, price })]);
    };
    pin_top(0, -1);
    writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(Item { id: 1000, price: 5 })]);
    fixture.settle(cx);
    // The top band is screen line 0; the data rows start on line 1.
    let top = |column: Col| cell(0, column);
    let bottom = |x: f32| point(px(x), px(HEIGHT - ROW / 2.));
    assert_eq!(fixture.visible_keys(cx)[..2], [0, 1]);
    assert_eq!(fixture.visible_keys(cx).len(), 20);

    // A pinned cell's tooltip comes from the pinned row.
    fixture.hover(cx, top(Col::Price));
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().tooltips.last(),
        Some(&(0, Col::Price, "-1".to_owned(), false))
    );
    fixture.frame(cx, Duration::from_millis(600));
    assert!(fixture.is_tooltip_shown(cx));
    assert_eq!(
        fixture.log.borrow().drawn.last().map(String::as_str),
        Some("Price -1")
    );
    // The band goes: data row 0 moves under the pointer. The same key and
    // column, but another row: a new element, and the tooltip is let go.
    writer.set_pinned_rows(PinnedEdge::Top, []);
    fixture.settle(cx);
    assert!(!fixture.is_tooltip_shown(cx));
    assert_eq!(
        fixture.log.borrow().tooltips.last(),
        Some(&(0, Col::Price, "0".to_owned(), false))
    );
    pin_top(0, -1);
    fixture.settle(cx);

    // A right press on a pinned row: `GridTarget::Pinned`, the selection
    // stays, no primary command; the standard items are the copy items.
    fixture.right_click(cx, bottom(110.));
    assert!(fixture.is_menu_open(cx));
    let target = GridTarget::Pinned {
        edge: PinnedEdge::Bottom,
        key: 1000,
        column: Col::Price,
    };
    assert_eq!(fixture.last_menu(), (target.clone(), vec![]));
    assert_eq!(target.key(), None, "not a data row's key");
    assert_eq!(target.column(), Some(Col::Price));
    assert_eq!(fixture.log.borrow().primaries, 0);
    assert_eq!(fixture.menu_labels(cx), ["Copy", "Copy all"]);
    assert!(fixture.selected_keys(cx).is_empty());
    fixture.press(cx, "escape");

    // A left press on a pinned row focuses the grid and changes neither the
    // selection nor the focused cell; a double click confirms nothing.
    fixture.click(cx, cell(1, Col::Price), MouseButton::Left);
    assert_eq!(fixture.selected_keys(cx), [0]);
    fixture.input(cx, |window, cx| window.blur(cx));
    fixture.click(cx, bottom(110.), MouseButton::Left);
    assert!(fixture.is_grid_focused(cx));
    assert_eq!(fixture.selected_keys(cx), [0]);
    let focused = |cx: &mut TestAppContext| {
        fixture.grid.read_with(cx, |grid, _| {
            grid.focused_cell()
                .and_then(|cell| Some((*cell.row()?, cell.column())))
        })
    };
    assert_eq!(focused(cx), Some((0, Col::Price)));
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            top(Col::Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
        press_at(
            window,
            top(Col::Price),
            MouseButton::Left,
            Modifiers::default(),
            2,
            cx,
        );
    });
    fixture.settle(cx);
    assert!(fixture.confirms().is_empty());
    assert_eq!(fixture.selected_keys(cx), [0]);

    // The menu stays while its pinned row stays in its band, and closes
    // when the row leaves it.
    fixture.right_click(cx, top(Col::Id));
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Pinned {
            edge: PinnedEdge::Top,
            key: 0,
            column: Col::Id
        }
    );
    assert_eq!(fixture.selected_keys(cx), [0]);
    pin_top(0, -2);
    fixture.settle(cx);
    assert!(fixture.is_menu_open(cx));
    pin_top(7, -2);
    fixture.settle(cx);
    assert!(!fixture.is_menu_open(cx));
    assert!(fixture.is_grid_focused(cx));

    // Zones: the frozen cell of a band stays, its scrolling cells move with
    // the data rows' columns.
    let target_at = |cx: &mut TestAppContext, x: f32| {
        fixture.right_click(cx, bottom(x));
        let target = fixture.last_menu().0;
        fixture.press(cx, "escape");
        target.column()
    };
    assert_eq!(target_at(cx, 30.), Some(Col::Id));
    assert_eq!(target_at(cx, 110.), Some(Col::Price));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(200.), cx));
    fixture.settle(cx);
    assert_eq!(target_at(cx, 30.), Some(Col::Id));
    assert_eq!(target_at(cx, 110.), Some(Col::Note));

    // A live change of the total: highlighted on its changed column, with
    // the band in the style context; pinned cells are never selected.
    fixture.log.borrow_mut().pinned_styles.clear();
    writer.set_pinned_rows(PinnedEdge::Bottom, [Arc::new(Item { id: 1000, price: 6 })]);
    fixture.settle(cx);
    let log = fixture.log.borrow();
    let bottom_cells: Vec<_> = log
        .pinned_styles
        .iter()
        .filter(|style| style.0 == PinnedEdge::Bottom)
        .collect();
    assert!(!bottom_cells.is_empty());
    assert!(
        bottom_cells
            .iter()
            .all(|&&(_, key, _, display_ix, _, selected)| key == 1000
                && display_ix == 0
                && !selected)
    );
    assert!(
        bottom_cells
            .iter()
            .any(|style| style.2 == Col::Id && style.4.is_none())
    );
    assert!(
        log.pinned_styles
            .iter()
            .any(|style| style.0 == PinnedEdge::Top && style.1 == 7)
    );
    drop(log);
    // Price scrolled out of view; back in view it shows the highlight.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_scroll_x(px(0.), cx));
    fixture.settle(cx);
    assert!(fixture.log.borrow().pinned_styles.iter().any(|style| {
        *style
            == (
                PinnedEdge::Bottom,
                1000,
                Col::Price,
                0,
                Some(RowChange::Updated),
                false,
            )
    }));
}

#[gpui_kit::test]
fn test_menu_from_keyboard_and_sort_items(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| grid);
    fixture.focus(cx);
    // The focus on row 0, column Id.
    fixture.press(cx, "down");
    let focused = fixture.grid.read_with(cx, |grid, _| {
        grid.focused_cell()
            .map(|cell| (cell.row().copied(), cell.column()))
    });
    assert_eq!(focused, Some((Some(0), Col::Id)));

    fixture.press(cx, "shift-f10");
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.last_menu(),
        (
            GridTarget::Cell {
                key: 0,
                column: Col::Id
            },
            vec![0]
        )
    );
    assert_eq!(
        fixture.menu_labels(cx),
        ["Copy", "Copy all", "Sort", "Column"]
    );

    // Down to Sort (past the separator), into it, Sort descending.
    fixture.press(cx, "down down down right down enter");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(
        fixture.sort_changes(),
        [(vec![(Col::Id, SortDirection::Descending)], SortOrigin::User)]
    );
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Id)
    );
    assert!(fixture.is_grid_focused(cx));

    // The Menu key opens the same menu.
    fixture.press(cx, "menu");
    assert!(fixture.is_menu_open(cx));
    assert_eq!(fixture.last_menu().0.column(), Some(Col::Id));
    fixture.press(cx, "escape");

    // Clear sort takes the column out; the menu items of a column that is
    // not sortable do nothing, even dispatched directly.
    fixture.right_click(cx, header(Col::Id));
    fixture.click_item(cx, "Clear sort");
    assert_eq!(
        fixture.sort_changes().last(),
        Some(&(vec![], SortOrigin::User))
    );
    fixture.events.borrow_mut().clear();
    fixture.press(cx, "right right");
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.current_column()),
        Some(Col::Size)
    );
    fixture.input(cx, |window, cx| {
        window.dispatch_action(Box::new(SortAscending), cx)
    });
    fixture.settle(cx);
    assert!(fixture.sort_changes().is_empty());
    // Its header menu has the sort items, disabled: clicking does nothing
    // and the menu stays.
    fixture.right_click(cx, header(Col::Size));
    fixture.click_item(cx, "Sort ascending");
    assert!(fixture.is_menu_open(cx));
    assert!(fixture.sort_changes().is_empty());

    // With no focused cell, the keyboard menu is the grid's.
    let unfocused = open(cx, 3, |grid| grid);
    unfocused.focus(cx);
    unfocused.input(cx, |window, cx| {
        window.dispatch_action(Box::new(ShowContextMenu), cx)
    });
    unfocused.settle(cx);
    assert_eq!(unfocused.last_menu(), (GridTarget::Empty, vec![]));
}

#[gpui_kit::test]
fn test_header_menu_column_items(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| grid);
    let layout_origins = |fixture: &Fixture| -> Vec<LayoutOrigin> {
        fixture
            .events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                LiveGridEvent::LayoutChange { origin, .. } => Some(*origin),
                _ => None,
            })
            .collect()
    };

    fixture.right_click(cx, header(Col::Price));
    assert_eq!(
        fixture.menu_labels(cx),
        [
            "Sort ascending",
            "Sort descending",
            "Clear sort",
            "Clear all sorting",
            "Autofit column",
            "Autofit all columns",
            "Hide column",
            "Show all columns",
            // The column chooser.
            "Choose columns…",
            "Freeze left",
            "Freeze right",
            "Reset columns",
            // The grouping items.
            "Group by this column",
            "Ungroup",
            "Ungroup all",
            "Copy",
            "Copy all",
        ]
    );
    fixture.click_item(cx, "Hide column");
    assert!(!fixture.is_menu_open(cx));
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Price))
    );
    assert_eq!(layout_origins(&fixture), [LayoutOrigin::User]);

    // Autofit widens the Note column to its long text.
    fixture.right_click(cx, header(Col::Note) - point(px(100.), px(0.)));
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Header { column: Col::Note }
    );
    fixture.click_item(cx, "Autofit column");
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Col::Note))
            > px(80.)
    );

    // Freeze left, then the frozen column offers Freeze right and Unfreeze.
    fixture.right_click(cx, header(Col::Id));
    fixture.click_item(cx, "Freeze left");
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_zone(Col::Id)),
        ColumnZone::FrozenLeft
    );
    fixture.right_click(cx, header(Col::Id));
    let labels = fixture.menu_labels(cx);
    assert!(labels.contains(&"Unfreeze".to_owned()));
    assert!(!labels.contains(&"Freeze left".to_owned()));
    fixture.press(cx, "escape");

    // Reset columns restores the schema's layout (`ResetLayout`, `User`).
    fixture.right_click(cx, header(Col::Id));
    fixture.click_item(cx, "Reset columns");
    fixture.grid.read_with(cx, |grid, _| {
        assert!(grid.is_column_visible(Col::Price));
        assert_eq!(grid.column_zone(Col::Id), ColumnZone::Scrolling);
        assert_eq!(grid.column_width(Col::Note), px(80.));
    });
    assert_eq!(layout_origins(&fixture).last(), Some(&LayoutOrigin::User));

    // The last visible column cannot be hidden: the item does nothing.
    fixture.grid.update(cx, |grid, cx| {
        for column in [Col::Price, Col::Size, Col::Note] {
            grid.set_column_visible(column, false, cx);
        }
    });
    fixture.settle(cx);
    fixture.right_click(cx, header(Col::Id));
    fixture.click_item(cx, "Hide column");
    assert!(
        fixture.is_menu_open(cx),
        "a disabled item does not close the menu"
    );
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Id))
    );
    fixture.click_item(cx, "Show all columns");
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Note))
    );
}

#[gpui_kit::test]
fn test_primary_command_dispatches_confirm(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| grid);
    fixture.primary.set(Some("Open"));
    fixture.right_click(cx, cell(3, Col::Price));
    // The primary command and its separator come first. A custom element
    // item has no label of its own; AccessKit names it after the value of
    // its `Label` descendant.
    let items = fixture.menu_items(cx);
    assert_eq!(
        items.len(),
        7,
        "primary, separator, Copy, Copy all, separator, Sort, Column"
    );
    assert_eq!(items[0].label(), None);
    let name = fixture.input(cx, |window, _| window.find("live-grid-primary-command"));
    assert_eq!(name.role(), Some(Role::Label));
    assert_eq!(name.value(), Some("Open"));
    assert!(name.path().starts_with(items[0].path()));
    assert_eq!(
        fixture.menu_labels(cx),
        ["Copy", "Copy all", "Sort", "Column"]
    );

    fixture.press(cx, "down enter");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.confirms(), [(vec![3], false)]);
    // Enter on the grid: the same event, from the same handler.
    fixture.press(cx, "enter");
    assert_eq!(fixture.confirms(), [(vec![3], false), (vec![3], false)]);

    // Without a primary command there is no such item; a double click
    // still confirms.
    fixture.events.borrow_mut().clear();
    fixture.primary.set(None);
    fixture.right_click(cx, cell(3, Col::Price));
    assert_eq!(fixture.menu_items(cx).len(), 5);
    fixture.press(cx, "escape");
    assert!(fixture.confirms().is_empty());
    fixture.input(cx, |window, cx| {
        press_at(
            window,
            cell(3, Col::Price),
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
        press_at(
            window,
            cell(3, Col::Price),
            MouseButton::Left,
            Modifiers::default(),
            2,
            cx,
        );
    });
    fixture.settle(cx);
    assert_eq!(fixture.confirms(), [(vec![3], false)]);

    // No primary command on a header, below the rows, or with an empty
    // scope: the hook is not even asked.
    let asked = fixture.log.borrow().primaries;
    fixture.primary.set(Some("Open"));
    fixture.right_click(cx, header(Col::Price));
    assert_eq!(fixture.log.borrow().primaries, asked);
}

#[gpui_kit::test]
fn test_cell_tooltip_follows_row_under_pointer(cx: &mut TestAppContext) {
    let fixture = open(cx, 10, |grid| {
        grid.with_view(by_price(SortDirection::Ascending))
    });
    fixture.hover(cx, cell(1, Col::Price));
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().tooltips.last(),
        Some(&(1, Col::Price, "10".to_owned(), false))
    );
    // Moving within the cell asks for no frame.
    let frames = fixture.grid.read_with(cx, |grid, _| grid.stats().frames());
    fixture.input(cx, |window, cx| {
        send(
            window,
            move_event(cell(1, Col::Price) + point(px(8.), px(3.))),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.stats().frames()),
        frames
    );

    // After GPUI's delay the tooltip shows and draws the price.
    fixture.frame(cx, Duration::from_millis(600));
    assert!(fixture.is_tooltip_shown(cx));
    assert_eq!(
        fixture.log.borrow().drawn.last().map(String::as_str),
        Some("Price 10")
    );

    // The price changes: the shown tooltip draws the new one in place.
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    writer.upsert(Item { id: 1, price: 15 });
    fixture.settle(cx);
    assert!(fixture.is_tooltip_shown(cx));
    assert_eq!(
        fixture.log.borrow().drawn.last().map(String::as_str),
        Some("Price 15")
    );

    // Another row moves under the pointer: a new element, the old tooltip
    // is let go, and the new one waits for its delay.
    fixture.write_prices(cx, 10, -1);
    fixture.settle(cx);
    assert_eq!(fixture.visible_keys(cx)[1], 8);
    assert!(!fixture.is_tooltip_shown(cx));
    assert_eq!(
        fixture.log.borrow().tooltips.last(),
        Some(&(8, Col::Price, "-80".to_owned(), false))
    );
    let drawn = fixture.log.borrow().drawn.len();
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().drawn.len(),
        drawn,
        "nothing of row 1 is drawn"
    );

    // A press hides it, as GPUI does.
    fixture.hover(cx, cell(1, Col::Price) + point(px(2.), px(0.)));
    fixture.frame(cx, Duration::from_millis(600));
    assert!(fixture.is_tooltip_shown(cx));
    fixture.input(cx, |window, cx| {
        send(
            window,
            MouseDownEvent {
                button: MouseButton::Left,
                position: cell(1, Col::Price),
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            },
            cx,
        );
    });
    assert!(!fixture.is_tooltip_shown(cx));

    // Automatic tooltips: a truncated cell shows its full text, a short one
    // none.
    let notes = open(cx, 3, |grid| grid);
    notes.hover(cx, cell(0, Col::Note));
    notes.settle(cx);
    assert_eq!(
        notes.log.borrow().tooltips.last(),
        Some(&(0, Col::Note, LONG_NOTE.to_owned(), true))
    );
    notes.frame(cx, Duration::from_millis(600));
    assert!(notes.is_tooltip_shown(cx));
    notes.hover(cx, cell(1, Col::Note));
    notes.frame(cx, Duration::from_millis(600));
    assert_eq!(
        notes.log.borrow().tooltips.last(),
        Some(&(1, Col::Note, "ok".to_owned(), false))
    );
    assert!(!notes.is_tooltip_shown(cx), "no tooltip for text that fits");

    // The row under the pointer is removed: no cell, no tooltip, no hook.
    notes.hover(cx, cell(2, Col::Note));
    notes.frame(cx, Duration::from_millis(600));
    assert!(notes.is_tooltip_shown(cx));
    notes.grid.read_with(cx, |grid, _| grid.writer()).remove(2);
    notes.settle(cx);
    assert!(!notes.is_tooltip_shown(cx));
    let calls = notes.log.borrow().tooltips.len();
    notes.settle(cx);
    assert_eq!(notes.log.borrow().tooltips.len(), calls);
}

#[gpui_kit::test]
fn test_header_tooltip_for_truncated_title(cx: &mut TestAppContext) {
    let fixture = open(cx, 3, |grid| grid);
    fixture.hover(cx, header(Col::Note));
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().headers.last(),
        Some(&(Col::Note, true))
    );
    fixture.hover(cx, header(Col::Price));
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().headers.last(),
        Some(&(Col::Price, false))
    );
    // Only the hovered cell is asked, once per render.
    // Widened, the title fits.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_width(Col::Note, px(240.), cx)
    });
    fixture.hover(cx, header(Col::Note));
    fixture.settle(cx);
    assert_eq!(
        fixture.log.borrow().headers.last(),
        Some(&(Col::Note, false))
    );
}

#[gpui_kit::test]
fn test_context_menu_gesture_decides_which_press_opens(cx: &mut TestAppContext) {
    use gpui_kit_grid::{ContextMenuGesture, ContextMenuGestureContext, GridArea, SelectionMode};

    /// Ctrl+click opens the menu on every platform; the right button opens
    /// nothing. Records where it was last asked.
    struct ControlClick(Rc<Cell<Option<GridArea>>>);

    impl ContextMenuGesture for ControlClick {
        fn opens_menu(&self, context: &ContextMenuGestureContext) -> bool {
            self.0.set(Some(context.area()));
            context.button() == MouseButton::Left && context.modifiers().control
        }
    }

    let area = Rc::new(Cell::new(None));
    let fixture = open(cx, 10, {
        let area = area.clone();
        |grid| {
            grid.with_selection_mode(SelectionMode::RowSet)
                .with_context_menu_gesture(ControlClick(area))
        }
    });
    let press = |fixture: &Fixture, cx: &mut TestAppContext, position, modifiers| {
        fixture.input(cx, |window, cx| {
            press_at(window, position, MouseButton::Left, modifiers, 1, cx)
        });
        fixture.settle(cx);
    };
    let menus = |fixture: &Fixture| fixture.log.borrow().menus.len();

    fixture.click(cx, cell(2, Col::Price), MouseButton::Left);
    press(&fixture, cx, cell(3, Col::Price), Modifiers::shift());
    assert_eq!(fixture.selected_keys(cx), [2, 3]);

    // The right button opens nothing and is no press of the selection.
    fixture.right_click(cx, cell(5, Col::Price));
    assert_eq!(area.get(), Some(GridArea::Body));
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(menus(&fixture), 0);
    assert_eq!(fixture.selected_keys(cx), [2, 3]);

    // Ctrl+click is the context press, not a toggle (the standard Ctrl+click
    // on Windows and Linux): a selected row keeps the selection...
    press(&fixture, cx, cell(3, Col::Price), Modifiers::control());
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.last_menu(),
        (
            GridTarget::Cell {
                key: 3,
                column: Col::Price
            },
            vec![2, 3]
        )
    );
    assert_eq!(fixture.selected_keys(cx), [2, 3]);
    fixture.press(cx, "escape");
    // ...and an unselected one is selected alone before the menu is built.
    press(&fixture, cx, cell(6, Col::Id), Modifiers::control());
    assert_eq!(
        fixture.last_menu(),
        (
            GridTarget::Cell {
                key: 6,
                column: Col::Id
            },
            vec![6]
        )
    );
    assert_eq!(fixture.selected_keys(cx), [6]);
    fixture.press(cx, "escape");

    // The header asks the same gesture: the right button opens nothing;
    // Ctrl+click opens the column's menu and does not sort.
    let built = menus(&fixture);
    fixture.right_click(cx, header(Col::Price));
    assert_eq!(area.get(), Some(GridArea::Header));
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(menus(&fixture), built);
    press(&fixture, cx, header(Col::Price), Modifiers::control());
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Header { column: Col::Price }
    );
    assert!(fixture.sort_changes().is_empty());
    assert_eq!(fixture.selected_keys(cx), [6]);
    fixture.press(cx, "escape");
    // Ctrl let go before the button: the press opened the menu, so its
    // click does not sort, whatever the modifiers when the button goes up.
    fixture.input(cx, |window, cx| {
        let position = header(Col::Id);
        mouse_down_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::control(),
            1,
            cx,
        );
        mouse_up_at(
            window,
            position,
            MouseButton::Left,
            Modifiers::default(),
            1,
            cx,
        );
    });
    fixture.settle(cx);
    assert!(fixture.is_menu_open(cx));
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Header { column: Col::Id }
    );
    assert!(fixture.sort_changes().is_empty());
    fixture.press(cx, "escape");
    // A plain click still sorts.
    fixture.click(cx, header(Col::Price), MouseButton::Left);
    assert_eq!(
        fixture.sort_changes(),
        [(
            vec![(Col::Price, SortDirection::Ascending)],
            SortOrigin::User
        )]
    );
}

#[gpui_kit::test]
fn test_cell_menu_column_items_from_keyboard(cx: &mut TestAppContext) {
    // The header menu's column commands, from the keyboard: Shift+F10 on
    // the focused cell, then Column. They act on the focused cell's
    // column (`current_column`).
    let fixture = open(cx, 10, |grid| grid);
    let submenu = ElementId::Name("submenu".into());
    let submenu_labels = |fixture: &Fixture, cx: &mut TestAppContext| -> Vec<String> {
        fixture
            .menu_items(cx)
            .iter()
            .filter(|item| item.path().contains(&submenu))
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    };
    fixture.focus(cx);
    // The focus on row 0, column Note: its long text is cut at 80 px.
    fixture.press(cx, "down right right right");
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.focused_cell().map(|cell| cell.column())),
        Some(Col::Note)
    );

    // Down to Column (past the separator and Sort), into it: its first
    // item, Autofit column.
    fixture.press(cx, "shift-f10");
    assert_eq!(
        fixture.last_menu().0,
        GridTarget::Cell {
            key: 0,
            column: Col::Note
        }
    );
    fixture.press(cx, "down down down down right");
    assert_eq!(
        submenu_labels(&fixture, cx),
        [
            "Autofit column",
            "Autofit all columns",
            "Hide column",
            "Show all columns",
            // The column chooser.
            "Choose columns…",
            "Freeze left",
            "Freeze right",
            "Reset columns",
            // The grouping items.
            "Group by this column",
            "Ungroup",
            "Ungroup all",
        ]
    );
    fixture.press(cx, "enter");
    assert!(!fixture.is_menu_open(cx));
    assert!(fixture.is_grid_focused(cx));
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.column_width(Col::Note))
            > px(80.)
    );
    let layout_changes = |fixture: &Fixture| {
        fixture
            .events
            .borrow()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    LiveGridEvent::LayoutChange {
                        origin: LayoutOrigin::User,
                        ..
                    }
                )
            })
            .count()
    };
    assert_eq!(layout_changes(&fixture), 1);

    // Again, down to Hide column (past a separator): Note is hidden.
    fixture.press(cx, "shift-f10");
    fixture.press(cx, "down down down down right down down enter");
    assert!(
        !fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Col::Note))
    );
    assert_eq!(layout_changes(&fixture), 2);
}

#[gpui_kit::test]
fn test_clear_all_sorting(cx: &mut TestAppContext) {
    // A multi-column sort and a newer order held back.
    let fixture = open(cx, 3, |grid| {
        grid.with_view(by_price(SortDirection::Ascending))
            .with_reorder_policy(ManualReorder)
    });
    let two_columns = [
        (Col::Price, SortDirection::Ascending),
        (Col::Id, SortDirection::Descending),
    ];
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort(two_columns, cx));
    fixture.settle(cx);
    fixture.write_prices(cx, 3, -1);
    fixture.settle(cx);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_order_held()));
    fixture.events.borrow_mut().clear();

    // In the header menu of a column outside the sort, after Clear sort:
    // every column leaves the sort, as a user action. The held order is
    // dropped first, then the sort changes.
    fixture.right_click(cx, header(Col::Note));
    let labels = fixture.menu_labels(cx);
    let clear = labels
        .iter()
        .position(|label| label == "Clear sort")
        .expect("Clear sort");
    assert_eq!(labels[clear + 1], "Clear all sorting");
    fixture.click_item(cx, "Clear all sorting");
    assert!(!fixture.is_menu_open(cx));
    assert!(fixture.grid.read_with(cx, |grid, _| grid.sort().is_empty()));
    let followed: Vec<String> = fixture
        .events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            LiveGridEvent::OrderHeldChange { held } => Some(format!("held {held}")),
            LiveGridEvent::SortChange { sort, origin } => {
                Some(format!("sort {:?} {origin:?}", sort.to_vec()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(followed, ["held false", "sort [] User"]);
    fixture.events.borrow_mut().clear();

    // Nothing sorted: the item is disabled, and the Action does nothing.
    fixture.right_click(cx, header(Col::Price));
    fixture.click_item(cx, "Clear all sorting");
    assert!(
        fixture.is_menu_open(cx),
        "a disabled item does not close the menu"
    );
    fixture.press(cx, "escape");
    fixture.input(cx, |window, cx| {
        window.dispatch_action(Box::new(ClearAllSorting), cx)
    });
    fixture.settle(cx);
    assert!(fixture.sort_changes().is_empty());

    // The cell menu's Sort submenu has it too, even on a column that is
    // not sortable (Size): the submenu is enabled while something is
    // sorted, with only this item enabled.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort(two_columns, cx));
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    fixture.focus(cx);
    fixture.press(cx, "down right right");
    assert_eq!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.focused_cell().map(|cell| cell.column())),
        Some(Col::Size)
    );
    fixture.press(cx, "shift-f10");
    fixture.press(cx, "down down down right");
    let submenu = ElementId::Name("submenu".into());
    let submenu_labels: Vec<String> = fixture
        .menu_items(cx)
        .iter()
        .filter(|item| item.path().contains(&submenu))
        .filter_map(|item| item.label().map(str::to_owned))
        .collect();
    assert_eq!(
        submenu_labels,
        [
            "Sort ascending",
            "Sort descending",
            "Clear sort",
            "Clear all sorting"
        ]
    );
    fixture.click_item(cx, "Clear all sorting");
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.sort_changes(), [(vec![], SortOrigin::User)]);
    fixture.events.borrow_mut().clear();

    // No default key: the application binds one in the grid's context.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_sort(two_columns, cx));
    fixture.settle(cx);
    fixture.events.borrow_mut().clear();
    cx.update(|cx| cx.bind_keys([KeyBinding::new("alt-s", ClearAllSorting, Some(KEY_CONTEXT))]));
    fixture.press(cx, "alt-s");
    assert_eq!(fixture.sort_changes(), [(vec![], SortOrigin::User)]);
}

/// What the delegate's menu hooks saw over summary lines.
#[derive(Clone, Default)]
struct SummaryMenus {
    /// `context_menu`: the target, the group, whether there was a target
    /// row.
    menus: Rc<
        RefCell<
            Vec<(
                GridTarget<u32, GroupedColumn>,
                Option<gpui_kit_grid::GroupPath>,
                bool,
            )>,
        >,
    >,
    primaries: Rc<Cell<usize>>,
}

impl LiveGridDelegate for SummaryMenus {
    type Row = GroupedRow;

    fn primary_command(
        &self,
        _: &GridMenuContext<'_, GroupedRow>,
        _: &App,
    ) -> Option<SharedString> {
        self.primaries.set(self.primaries.get() + 1);
        Some("Open".into())
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, GroupedRow>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        self.menus.borrow_mut().push((
            menu_context.target().clone(),
            menu_context.group().map(|group| group.path().clone()),
            menu_context.target_row().is_some(),
        ));
        menu_context.standard_items(menu, window, cx)
    }
}

#[gpui_kit::test]
fn test_totals_and_footer_targets(cx: &mut TestAppContext) {
    use GroupedColumn::{Instrument, Qty};
    use gpui_kit_grid::{Aggregate, EveryGroupFooter, ExpandTarget, GroupBy};

    let hooks = SummaryMenus::default();
    // Twelve trades in four instruments of three; the bottom band holds the
    // totals, screen line 9. On screen: AAPL, 0, 4, 8, AAPL's footer (line
    // 4), MSFT, 1, 5, 9.
    let fixture = open_grouped_with(cx, grouped_rows(12), hooks.clone(), |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_sort([(GroupedColumn::Id, SortDirection::Ascending)])
                .with_group_by([GroupBy::column(Instrument)])
                .with_aggregates([(Qty, Aggregate::Sum)])
                .with_group_footer(EveryGroupFooter),
        )
        .with_tuning(GridTuning::default().with_min_rebuild_interval(Duration::ZERO))
        .with_totals(PinnedEdge::Bottom)
    });
    let aapl = group_path(&[(Instrument, "AAPL")]);
    let is_open =
        |cx: &mut TestAppContext| fixture.grid.read_with(cx, |grid, _| grid.is_menu_open());
    let grid_focused = |cx: &mut TestAppContext| {
        let grid = fixture.grid.clone();
        cx.update_window(fixture.window, |_, window, cx| {
            grid.focus_handle(cx).is_focused(window)
        })
        .expect("window is open")
    };
    let right_click = |cx: &mut TestAppContext, position: Point<Pixels>| {
        fixture.input(cx, |window, cx| {
            press_at(
                window,
                position,
                MouseButton::Right,
                Modifiers::default(),
                1,
                cx,
            )
        });
        fixture.settle(cx);
    };
    assert_eq!(GROUPED_ROW, ROW);

    // The totals: their band and column; the copy items; no primary
    // command, no group, no row.
    right_click(cx, grouped_cell(9, Qty));
    assert!(is_open(cx));
    let totals = GridTarget::Totals {
        edge: PinnedEdge::Bottom,
        column: Qty,
    };
    assert_eq!(
        hooks.menus.borrow().last(),
        Some(&(totals.clone(), None, false))
    );
    assert_eq!(
        (totals.key(), totals.column(), totals.group()),
        (None, Some(Qty), None)
    );
    assert_eq!(menu_labels(cx, fixture.window), ["Copy", "Copy all"]);
    assert_eq!(hooks.primaries.get(), 0);
    // The totals go while the menu is open: it closes, the grid has the
    // focus.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_totals(None, cx));
    fixture.settle(cx);
    assert!(!is_open(cx));
    assert!(grid_focused(cx));

    // Back at the bottom; the menu closes with its column hidden.
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_totals(Some(PinnedEdge::Bottom), cx));
    fixture.settle(cx);
    right_click(cx, grouped_cell(9, Qty));
    assert!(is_open(cx));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Qty, false, cx));
    fixture.settle(cx);
    assert!(!is_open(cx));
    fixture
        .grid
        .update(cx, |grid, cx| grid.set_column_visible(Qty, true, cx));
    fixture.settle(cx);

    // A group footer: its group and column; the menu context gives the
    // group.
    right_click(cx, grouped_cell(4, Qty));
    assert!(is_open(cx));
    let footer = GridTarget::GroupFooter {
        group: aapl.clone(),
        column: Qty,
    };
    assert_eq!(
        hooks.menus.borrow().last(),
        Some(&(footer.clone(), Some(aapl.clone()), false))
    );
    assert_eq!((footer.key(), footer.group()), (None, Some(&aapl)));
    assert_eq!(menu_labels(cx, fixture.window), ["Copy", "Copy all"]);
    assert_eq!(hooks.primaries.get(), 0);
    // Its group collapses: the footer goes, and the menu with it.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_expanded(ExpandTarget::Group(aapl.clone()), false, cx)
    });
    fixture.settle(cx);
    assert!(!is_open(cx));
    assert!(grid_focused(cx));
}
