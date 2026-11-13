//! Headless UI tests of the column chooser (`.agents/COMPONENT_TEST_RULES.md`):
//! the grid opens it from the Action, its header menu and its handle, closes it
//! on Escape and on a press outside; it lists the columns by zone and band,
//! shows and hides them, reorders them with the grip and the keyboard, searches
//! them, follows the grid's layout, hands the opening to a factory, and works
//! as the application's own view.

mod common;

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{IconName, Sizable as _};
use gpui_kit::{
    AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, DismissEvent, ElementId,
    Entity, Focusable as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    Styled as _, TestAppContext, Window, WindowBounds, WindowOptions, div, px, size,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    ColumnBand, ColumnChooserContext, ColumnChooserFactory, ColumnChooserPanel, ColumnSet,
    ColumnZone, GridColumn, GridColumns as _, GridLayout, GridSchema, LayoutOrigin, LayoutPart,
    LayoutParts, LiveGrid, LiveGridEvent, LiveGridHandle, LiveGridState, PopoverColumnChooser,
    ReorderContext, ReorderDecision, ReorderPolicy, SortDirection, StoreOptions, ViewSpec,
    actions::ShowColumnChooser,
};

use crate::common::{Order, OrderColumn, Orders, orders, press_at, unthrottled};

use OrderColumn::{Active, Id, Instrument, Note, Placed, Price, Quantity, Side, Status};

/// Id (not hideable) and Instrument frozen left; Side, a band "Trade" over
/// Qty, Price and Placed, Active and a hidden Note scrolling; Status, a
/// check icon alone, frozen right.
fn schema() -> GridSchema<OrderColumn> {
    schema_with(
        GridColumn::new(Note, "Note")
            .with_width(px(120.))
            .with_visible(false),
    )
}

/// [`schema`] with `note` in Note's place.
fn schema_with(note: GridColumn<OrderColumn>) -> GridSchema<OrderColumn> {
    GridSchema::new()
        .column(
            GridColumn::new(Id, "Id")
                .with_width(px(50.))
                .with_hideable(false),
        )
        .column(GridColumn::new(Instrument, "Instrument").with_width(px(90.)))
        .column(GridColumn::new(Side, "Side").with_width(px(50.)))
        .band(
            ColumnBand::new("trade", "Trade")
                .column(GridColumn::new(Quantity, "Qty").with_width(px(70.)))
                .column(GridColumn::new(Price, "Price").with_width(px(80.)))
                .column(GridColumn::new(Placed, "Placed").with_width(px(150.))),
        )
        .column(GridColumn::new(Active, "Active").with_width(px(60.)))
        .column(note)
        .column(
            GridColumn::new(Status, "Status")
                .with_width(px(80.))
                .with_zone(ColumnZone::FrozenRight)
                .with_icon_only(IconName::Check),
        )
        .with_frozen(2)
}

const WIDTH: f32 = 900.;
const HEIGHT: f32 = 700.;

/// The grid, and beside it an application's own chooser if it has one.
struct Root {
    grid: Entity<LiveGridState<Orders>>,
    panel: Option<Entity<ColumnChooserPanel>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .child(LiveGrid::new(&self.grid).xsmall().bordered(false)),
            )
            .children(self.panel.clone())
    }
}

type Events = Rc<RefCell<Vec<LiveGridEvent<u32, OrderColumn>>>>;

struct Fixture {
    window: AnyWindowHandle,
    grid: Entity<LiveGridState<Orders>>,
    root: Entity<Root>,
    events: Events,
}

/// Twelve orders over [`schema`] in a window with room for the chooser,
/// configured by `configure`.
fn open(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Orders>) -> LiveGridState<Orders> + 'static,
) -> Fixture {
    open_with(cx, schema(), configure)
}

/// [`open`] over another schema.
fn open_with(
    cx: &mut TestAppContext,
    schema: GridSchema<OrderColumn>,
    configure: impl FnOnce(LiveGridState<Orders>) -> LiveGridState<Orders> + 'static,
) -> Fixture {
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
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            configure(
                                LiveGridState::new_standalone(
                                    schema,
                                    Orders,
                                    StoreOptions::default(),
                                    cx,
                                )
                                .with_tuning(unthrottled()),
                            )
                        }),
                        panel: None,
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Events::default();
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(
            &grid,
            move |_, event: &LiveGridEvent<u32, OrderColumn>, _| {
                events.borrow_mut().push(event.clone());
            },
        )
        .detach();
    });
    grid.read_with(cx, |grid, _| grid.writer())
        .replace_all(orders(12).into_iter().map(Arc::new));
    let fixture = Fixture {
        window,
        grid,
        root,
        events,
    };
    fixture.settle(cx);
    fixture
}

const CHOOSER: &str = "live-grid-column-chooser";

fn column_row(id: &'static str) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name("chooser-column".into())),
        id.into(),
    )
}

fn band_row(id: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Name("chooser-band".into())), id.into())
}

fn zone_header(id: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Name("chooser-zone".into())), id.into())
}

impl Fixture {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            self.input(cx, |window, cx| window.render_frame(cx));
        }
    }

    fn input<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .expect("window is open")
    }

    /// Focuses the grid and dispatches `ShowColumnChooser`, as a key would.
    fn show(&self, cx: &mut TestAppContext) {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.dispatch_action(Box::new(ShowColumnChooser), cx);
        });
        self.settle(cx);
    }

    fn press(&self, cx: &mut TestAppContext, key: &str) {
        self.input(cx, |window, cx| window.press(key, cx));
        self.settle(cx);
    }

    fn is_open(&self, cx: &mut TestAppContext) -> bool {
        self.grid
            .read_with(cx, |grid, _| grid.is_column_chooser_open())
    }

    fn has_panel(&self, cx: &mut TestAppContext) -> bool {
        self.input(cx, |window, _| window.try_find(CHOOSER).is_some())
    }

    fn is_grid_focused(&self, cx: &mut TestAppContext) -> bool {
        let grid = self.grid.clone();
        self.input(cx, |window, cx| grid.focus_handle(cx).is_focused(window))
    }

    /// The chooser's rows top to bottom: `zone:<id>`, `band:<id>` or the
    /// column id, each with its left edge.
    fn rows(&self, cx: &mut TestAppContext) -> Vec<(String, Pixels)> {
        let mut rows: Vec<(Pixels, String, Pixels)> = self.input(cx, |window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|item| item.visible())
                .filter(|item| {
                    item.path()
                        .iter()
                        .any(|id| *id == ElementId::Name(CHOOSER.into()))
                })
                .filter_map(|item| {
                    let ElementId::NamedChild(kind, id) = item.path().last()? else {
                        return None;
                    };
                    let name = match &**kind {
                        ElementId::Name(name) if name == "chooser-zone" => format!("zone:{id}"),
                        ElementId::Name(name) if name == "chooser-band" => format!("band:{id}"),
                        ElementId::Name(name) if name == "chooser-column" => id.to_string(),
                        _ => return None,
                    };
                    Some((item.bounds().top(), name, item.bounds().left()))
                })
                .collect()
        });
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("finite"));
        rows.into_iter()
            .map(|(_, name, left)| (name, left))
            .collect()
    }

    fn row_names(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.rows(cx).into_iter().map(|(name, _)| name).collect()
    }

    /// The checkbox of a row: its bounds and whether it is checked.
    fn checkbox(
        &self,
        cx: &mut TestAppContext,
        row: ElementId,
    ) -> (Bounds<Pixels>, Option<bool>, Option<String>) {
        self.input(cx, |window, _| {
            let check = window.within(CHOOSER).within(row).find("chooser-check");
            (
                check.bounds(),
                check.checked(),
                check.label().map(str::to_owned),
            )
        })
    }

    fn click_row(&self, cx: &mut TestAppContext, row: ElementId) {
        let center = self.input(cx, |window, _| {
            window.within(CHOOSER).find(row).bounds().center()
        });
        self.click_at(cx, center, MouseButton::Left);
    }

    fn click_at(&self, cx: &mut TestAppContext, position: Point<Pixels>, button: MouseButton) {
        self.input(cx, |window, cx| {
            press_at(window, position, button, Default::default(), 1, cx)
        });
        self.settle(cx);
    }

    fn grip(&self, cx: &mut TestAppContext, row: ElementId) -> Point<Pixels> {
        self.input(cx, |window, _| {
            window
                .within(CHOOSER)
                .within(row)
                .find("chooser-grip")
                .bounds()
                .center()
        })
    }

    fn drag(&self, cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>) {
        self.input(cx, |window, cx| window.drag(from, to, cx));
        self.settle(cx);
    }

    /// The `LayoutChange`s so far, then forgets every event.
    fn take_layout_events(&self) -> Vec<(LayoutParts, ColumnSet, LayoutOrigin)> {
        std::mem::take(&mut *self.events.borrow_mut())
            .into_iter()
            .filter_map(|event| match event {
                LiveGridEvent::LayoutChange {
                    parts,
                    columns,
                    origin,
                } => Some((parts, columns, origin)),
                _ => None,
            })
            .collect()
    }

    fn visible(&self, cx: &mut TestAppContext, column: OrderColumn) -> bool {
        self.grid
            .read_with(cx, |grid, _| grid.is_column_visible(column))
    }

    fn order(&self, cx: &mut TestAppContext) -> Vec<OrderColumn> {
        self.grid.read_with(cx, |grid, _| grid.columns().collect())
    }
}

fn set(columns: impl IntoIterator<Item = OrderColumn>) -> ColumnSet {
    columns
        .into_iter()
        .fold(ColumnSet::EMPTY, |mut set, column| {
            set.insert(column.ix());
            set
        })
}

fn visibility() -> LayoutParts {
    LayoutPart::Visibility.into()
}

#[gpui_kit::test]
fn test_chooser_open_close(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    let handle = LiveGridHandle::new(&fixture.grid);

    // The Action opens it, focused (its list: nine columns, no search).
    fixture.show(cx);
    assert!(fixture.is_open(cx));
    assert!(cx.update(|cx| handle.is_column_chooser_open(cx)));
    assert!(fixture.has_panel(cx));
    assert!(!fixture.is_grid_focused(cx));
    // Below the right end of the header, its right edge at the header's.
    let (header, panel) = fixture.input(cx, |window, _| {
        (
            window.find("live-grid-header").bounds(),
            window.find(CHOOSER).bounds(),
        )
    });
    // The body's top: the header's bottom and its rule.
    assert!((panel.top() - header.bottom()).abs() <= px(1.));
    assert_eq!(panel.right(), header.right());

    // Escape closes it; the grid has the focus back.
    fixture.press(cx, "escape");
    assert!(!fixture.is_open(cx));
    assert!(!fixture.has_panel(cx));
    assert!(fixture.is_grid_focused(cx));

    // The header menu's "Choose columns…" opens it where the menu was.
    let header_cell = fixture.input(cx, |window, _| {
        window
            .within("live-grid-header")
            .find("instrument")
            .bounds()
            .center()
    });
    fixture.click_at(cx, header_cell, MouseButton::Right);
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()));
    let item = fixture.input(cx, |window, _| {
        gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| item.visible() && item.label() == Some("Choose columns…"))
            .expect("the menu item")
            .bounds()
            .center()
    });
    fixture.click_at(cx, item, MouseButton::Left);
    assert!(fixture.is_open(cx));
    assert!(!fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()));
    let panel = fixture.input(cx, |window, _| window.find(CHOOSER).bounds());
    assert_eq!(panel.origin, header_cell);

    // A press outside closes it and does nothing else: the selection stays.
    fixture.events.borrow_mut().clear();
    let cell = point_in_body(&fixture, cx);
    fixture.click_at(cx, cell, MouseButton::Left);
    assert!(!fixture.is_open(cx));
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.selection().is_empty())
    );
    assert!(fixture.events.borrow().is_empty());

    // The handle opens it; a context menu closes it.
    fixture.input(cx, |window, cx| handle.show_column_chooser(window, cx));
    fixture.settle(cx);
    assert!(fixture.is_open(cx));
    let grid = fixture.grid.clone();
    fixture.input(cx, |window, cx| {
        grid.update(cx, |grid, cx| grid.show_context_menu(window, cx))
    });
    fixture.settle(cx);
    assert!(!fixture.is_open(cx));
    assert!(fixture.grid.read_with(cx, |grid, _| grid.is_menu_open()));
}

/// A point on the third row, right of the panel opened at the Instrument
/// header (x 95 to 375).
fn point_in_body(fixture: &Fixture, cx: &mut TestAppContext) -> Point<Pixels> {
    let header = fixture.input(cx, |window, _| window.find("live-grid-header").bounds());
    Point::new(px(450.), header.bottom() + px(26. * 2.5))
}

#[gpui_kit::test]
fn test_chooser_lists_columns(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.show(cx);

    // Sections by zone in screen order; the band before its members. The
    // list scrolls past 360 px: the last row shows once it is highlighted.
    assert_eq!(
        fixture.row_names(cx)[..12],
        [
            "zone:frozen-left",
            "id",
            "instrument",
            "zone:scrolling",
            "side",
            "band:trade",
            "quantity",
            "price",
            "placed",
            "active",
            "note",
            "zone:frozen-right",
        ]
    );
    for _ in 0..10 {
        fixture.press(cx, "down");
    }
    assert_eq!(
        fixture.row_names(cx).last().map(String::as_str),
        Some("status")
    );
    // The band's members are indented a level.
    let side = fixture.checkbox(cx, column_row("side")).0;
    let qty = fixture.checkbox(cx, column_row("quantity")).0;
    assert_eq!(qty.left() - side.left(), px(16.));

    // Checked by visibility; named by the column's title, the icon column
    // too.
    assert_eq!(fixture.checkbox(cx, column_row("note")).1, Some(false));
    assert_eq!(fixture.checkbox(cx, column_row("price")).1, Some(true));
    assert_eq!(fixture.checkbox(cx, band_row("trade")).1, Some(true));
    assert_eq!(
        fixture.checkbox(cx, column_row("status")).2.as_deref(),
        Some("Status")
    );
    assert!(fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .try_find(zone_header("frozen-right"))
            .is_some()
    }));
}

/// An internal column (`with_internal`) is never shown or offered: not by
/// the schema, the chooser, Show all, `set_column_visible` or a layout. The
/// handle lists it, marked.
#[gpui_kit::test]
fn test_chooser_leaves_out_internal_columns(cx: &mut TestAppContext) {
    // Shown by its declaration, but internal.
    let schema = schema_with(GridColumn::new(Note, "Rank").with_internal(true));
    let fixture = open_with(cx, schema, |grid| grid);
    let visible = |cx: &mut TestAppContext| {
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Note))
    };
    assert!(!visible(cx));
    fixture.show(cx);
    let names = fixture.row_names(cx);
    assert!(!names.iter().any(|name| name == "note"), "{names:?}");
    assert!(names.iter().any(|name| name == "active"), "{names:?}");

    let info = cx.update(|cx| {
        LiveGridHandle::new(&fixture.grid)
            .columns(cx)
            .into_iter()
            .find(|info| info.id().as_str() == "note")
    });
    let info = info.expect("the handle lists it");
    assert!(info.is_internal() && !info.is_visible());

    fixture.grid.update(cx, |grid, cx| {
        assert!(!grid.set_column_visible(Note, true, cx));
        grid.show_all_columns(cx);
    });
    fixture.settle(cx);
    assert!(!visible(cx));
    let layout = fixture.grid.read_with(cx, |grid, _| grid.layout());
    let shown = layout
        .columns()
        .iter()
        .cloned()
        .map(|column| column.with_visible(true))
        .fold(GridLayout::new(), GridLayout::with_column);
    fixture.grid.update(cx, |grid, cx| {
        grid.set_layout(&shown, cx).expect("a layout of this grid");
    });
    fixture.settle(cx);
    assert!(!visible(cx));
    assert!(
        fixture
            .grid
            .read_with(cx, |grid, _| grid.is_column_visible(Active))
    );
}

#[gpui_kit::test]
fn test_chooser_toggle(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.show(cx);
    fixture.take_layout_events();

    // A click on a column's row hides it: one `User` change.
    fixture.click_row(cx, column_row("price"));
    assert!(!fixture.visible(cx, Price));
    assert_eq!(
        fixture.take_layout_events(),
        [(visibility(), set([Price]), LayoutOrigin::User)]
    );
    assert_eq!(fixture.checkbox(cx, column_row("price")).1, Some(false));

    // Space on the highlighted row shows it again.
    fixture.press(cx, "space");
    assert!(fixture.visible(cx, Price));
    assert_eq!(
        fixture.take_layout_events(),
        [(visibility(), set([Price]), LayoutOrigin::User)]
    );

    // The band's checkbox: every member in one change, and back.
    fixture.click_row(cx, band_row("trade"));
    assert!(!fixture.visible(cx, Quantity) && !fixture.visible(cx, Placed));
    assert_eq!(
        fixture.take_layout_events(),
        [(
            visibility(),
            set([Quantity, Price, Placed]),
            LayoutOrigin::User
        )]
    );
    fixture.click_row(cx, band_row("trade"));
    assert!(fixture.visible(cx, Quantity) && fixture.visible(cx, Placed));
    assert_eq!(fixture.take_layout_events().len(), 1);

    // A column the user may not hide stays.
    fixture.click_row(cx, column_row("id"));
    assert!(fixture.visible(cx, Id));
    assert!(fixture.take_layout_events().is_empty());

    // "Show all" shows the hidden Note.
    let show_all = fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .find("chooser-show-all")
            .bounds()
            .center()
    });
    fixture.click_at(cx, show_all, MouseButton::Left);
    assert!(fixture.visible(cx, Note));
    assert_eq!(
        fixture.take_layout_events(),
        [(visibility(), set([Note]), LayoutOrigin::User)]
    );
}

#[gpui_kit::test]
fn test_chooser_last_visible(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    // Every column but Id hidden, then Id made hideable is still the last.
    fixture.grid.update(cx, |grid, cx| {
        for column in [Instrument, Side, Quantity, Price, Placed, Active, Status] {
            grid.set_column_visible(column, false, cx);
        }
    });
    fixture.show(cx);
    fixture.take_layout_events();
    fixture.click_row(cx, column_row("id"));
    assert!(fixture.visible(cx, Id));
    assert!(fixture.take_layout_events().is_empty());
}

#[gpui_kit::test]
fn test_chooser_reorder_mouse(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.show(cx);
    fixture.take_layout_events();

    // Active onto Side: before it, one `Order` change.
    let from = fixture.grip(cx, column_row("active"));
    let to = fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .find(column_row("side"))
            .bounds()
            .center()
    });
    fixture.drag(cx, from, to);
    assert_eq!(
        fixture.order(cx),
        [
            Id, Instrument, Active, Side, Quantity, Price, Placed, Note, Status
        ]
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(LayoutPart::Order.into(), set([Active]), LayoutOrigin::User)]
    );

    // Side onto the "Frozen left" header: first in that zone, zone and
    // order in one change.
    let from = fixture.grip(cx, column_row("side"));
    let to = fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .find(zone_header("frozen-left"))
            .bounds()
            .center()
    });
    fixture.drag(cx, from, to);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.column_zone(Side)),
        ColumnZone::FrozenLeft
    );
    assert_eq!(fixture.order(cx)[0], Side);
    let changes = fixture.take_layout_events();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].0.contains(LayoutPart::Zone) && changes[0].0.contains(LayoutPart::Order));

    // A band member out of its band: refused, nothing changes.
    let order = fixture.order(cx);
    let from = fixture.grip(cx, column_row("quantity"));
    let to = fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .find(column_row("active"))
            .bounds()
            .center()
    });
    fixture.drag(cx, from, to);
    assert_eq!(fixture.order(cx), order);
    assert!(fixture.take_layout_events().is_empty());
}

#[gpui_kit::test]
fn test_chooser_reorder_keyboard(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.show(cx);
    fixture.take_layout_events();

    // Down to the band's row (the fourth), then Alt+↑: the band moves one
    // place up, before Side.
    for _ in 0..4 {
        fixture.press(cx, "down");
    }
    fixture.press(cx, "alt-up");
    assert_eq!(
        fixture.order(cx),
        [
            Id, Instrument, Quantity, Price, Placed, Side, Active, Note, Status
        ]
    );
    assert_eq!(
        fixture.take_layout_events(),
        [(
            LayoutPart::Order.into(),
            set([Quantity, Price, Placed]),
            LayoutOrigin::User
        )]
    );
    // First in its zone: nothing.
    fixture.press(cx, "alt-up");
    assert!(fixture.take_layout_events().is_empty());
}

#[gpui_kit::test]
fn test_chooser_search(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| {
        grid.with_column_chooser(PopoverColumnChooser::new().with_search_threshold(5))
    });
    fixture.show(cx);

    // The search field has the focus: typing filters the columns into a
    // flat list without grips.
    fixture.input(cx, |window, cx| window.input("ac", cx));
    fixture.settle(cx);
    assert_eq!(fixture.row_names(cx), ["placed", "active"]);
    let grip = fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .within(column_row("active"))
            .find("chooser-grip")
            .label()
            .map(str::to_owned)
    });
    assert_eq!(grip, None);

    // Nothing matches: a line says so.
    fixture.input(cx, |window, cx| window.input("zz", cx));
    fixture.settle(cx);
    assert!(fixture.row_names(cx).is_empty());
    assert!(fixture.input(cx, |window, _| {
        window
            .within(CHOOSER)
            .try_find("chooser-no-match")
            .is_some()
    }));
}

#[gpui_kit::test]
fn test_chooser_follows_grid(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.show(cx);
    // Highlight Price, the seventh row.
    for _ in 0..6 {
        fixture.press(cx, "down");
    }

    // The grid hides Active and moves Side: the rows follow at once.
    fixture.grid.update(cx, |grid, cx| {
        grid.set_column_visible(Active, false, cx);
        grid.move_column(Side, None, cx);
    });
    fixture.settle(cx);
    assert_eq!(fixture.checkbox(cx, column_row("active")).1, Some(false));
    let names = fixture.row_names(cx);
    assert_eq!(
        names[names.iter().position(|name| name == "note").unwrap() + 1],
        "side"
    );

    // The highlight stayed on Price: Space hides Price.
    fixture.take_layout_events();
    fixture.press(cx, "space");
    assert!(!fixture.visible(cx, Price));

    // A restored layout shows in the rows too.
    let default = fixture.grid.read_with(cx, |grid, _| grid.default_layout());
    fixture.grid.update(cx, |grid, cx| {
        grid.set_layout(&default, cx).expect("a layout")
    });
    fixture.settle(cx);
    assert_eq!(fixture.checkbox(cx, column_row("price")).1, Some(true));
    assert_eq!(fixture.checkbox(cx, column_row("active")).1, Some(true));
}

/// Shows nothing in the grid's layer; records each opening.
#[derive(Clone, Default)]
struct DialogChooser(Rc<RefCell<Vec<Point<Pixels>>>>);

impl ColumnChooserFactory for DialogChooser {
    fn open(
        &self,
        context: &ColumnChooserContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyView> {
        // The factory reads the grid through the handle.
        assert_eq!(context.grid().columns(cx).len(), 9);
        self.0.borrow_mut().push(context.position());
        None
    }
}

#[gpui_kit::test]
fn test_chooser_factory(cx: &mut TestAppContext) {
    let dialog = DialogChooser::default();
    let fixture = open(cx, {
        let dialog = dialog.clone();
        move |grid| grid.with_column_chooser(dialog)
    });
    fixture.show(cx);
    assert!(!fixture.is_open(cx));
    assert!(!fixture.has_panel(cx));
    let header = fixture.input(cx, |window, _| window.find("live-grid-header").bounds());
    let positions = dialog.0.borrow().clone();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].x, header.right() - px(280.));
    assert!((positions[0].y - header.bottom()).abs() <= px(1.));
}

#[gpui_kit::test]
fn test_chooser_in_app_popover(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    let dismissed = Rc::new(RefCell::new(0));
    // The application's own chooser over the handle, beside the grid.
    let handle = LiveGridHandle::new(&fixture.grid);
    let root = fixture.root.clone();
    let panel = fixture.input(cx, |window, cx| {
        let panel = cx.new(|cx| ColumnChooserPanel::new(&handle, window, cx));
        root.update(cx, |root, cx| {
            root.panel = Some(panel.clone());
            cx.notify();
        });
        panel
    });
    cx.update(|cx| {
        let dismissed = dismissed.clone();
        cx.subscribe(&panel, move |_, _: &DismissEvent, _| {
            *dismissed.borrow_mut() += 1
        })
        .detach();
    });
    fixture.settle(cx);
    assert!(!fixture.is_open(cx));

    // It works without the grid's layer.
    fixture.take_layout_events();
    fixture.click_row(cx, column_row("instrument"));
    assert!(!fixture.visible(cx, Instrument));
    assert_eq!(
        fixture.take_layout_events(),
        [(visibility(), set([Instrument]), LayoutOrigin::User)]
    );

    // Escape in it says it is done.
    fixture.input(cx, |window, cx| {
        let focus = panel.focus_handle(cx);
        window.focus(&focus, cx);
    });
    fixture.press(cx, "escape");
    assert_eq!(*dismissed.borrow(), 1);
}

/// Applies every order, and records whether the chooser was open.
#[derive(Clone, Default)]
struct SeesChooser(Rc<RefCell<Vec<bool>>>);

impl ReorderPolicy for SeesChooser {
    fn decide(&self, context: &ReorderContext) -> ReorderDecision {
        self.0.borrow_mut().push(context.is_column_chooser_open());
        ReorderDecision::Apply
    }
}

#[gpui_kit::test]
fn test_chooser_reorder_input(cx: &mut TestAppContext) {
    let seen = SeesChooser::default();
    let fixture = open(cx, {
        let seen = seen.clone();
        move |grid| {
            grid.with_view(ViewSpec::new().with_sort([(Price, SortDirection::Descending)]))
                .with_reorder_policy(seen)
        }
    });
    // An open chooser holds nothing by itself; a policy sees it open.
    fixture.show(cx);
    seen.0.borrow_mut().clear();
    let writer = fixture.grid.read_with(cx, |grid, _| grid.writer());
    for order in orders(12) {
        writer.upsert(Order {
            price: -order.price,
            ..order
        });
    }
    fixture.settle(cx);
    assert!(seen.0.borrow().contains(&true));
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.visible_keys())[0],
        1
    );
}
