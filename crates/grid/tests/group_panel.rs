//! Headless UI tests of the group panel (`.agents/COMPONENT_TEST_RULES.md`):
//! `LiveGrid::group_panel` and the body under it, headers dragged into the
//! panel and refused there, chip clicks, drags and remove buttons, the grouping
//! items of the column menu and the grouping Actions, the chips from the
//! keyboard, and "captions only" from the panel's menu. Assertions read the
//! grid, its events, the projection's lines and the elements' accessibility
//! facts, not pixels.

mod common;

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui_kit::component::Sizable as _;
use gpui_kit::test::{ElementSnapshot, TestWindowExt as _};
use gpui_kit::{
    Action, App, AppContext as _, Bounds, Context, ElementId, Entity, Focusable as _, IntoElement,
    Modifiers, MouseButton, ParentElement as _, Pixels, Point, Render, Role, Styled as _,
    TestAppContext, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use gpui_kit_grid::{
    Aggregate, ColumnBand, ExpandDepth, GridColumn, GridColumns as _, GridSchema, GroupBy,
    GroupOrigin, LiveGrid, LiveGridEvent, LiveGridState, SortDirection, StoreOptions, TreeSpec,
    ViewSpec,
    actions::{ClearGrouping, GroupByColumn, UngroupColumn},
};

use crate::common::{
    GROUPED_COLUMNS, GroupSeen, Grouped, GroupedColumn, GroupedFixture, LineKind, ROW,
    drag_move_to, grouped_rows, mouse_down_at, mouse_up_at, open_grouped, press_at,
};

use GroupOrigin::{Programmatic, User};
use GroupedColumn::{Id, Instrument, Qty, Side, Trader};
use SortDirection::{Ascending, Descending};

/// Wide and tall enough for the header menu: the panel, the header and 22
/// `xsmall` rows.
const WIDTH: f32 = 600.;
const HEIGHT: f32 = ROW * 24.;
/// The vertical centre of the panel (the top row).
const PANEL_Y: f32 = ROW / 2.;

/// The window root: a `LiveGrid` whose panel follows `panel`.
struct PanelRoot {
    grid: Entity<LiveGridState<Grouped>>,
    panel: Rc<Cell<bool>>,
}

impl Render for PanelRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .xsmall()
                .bordered(false)
                .group_panel(self.panel.get()),
        )
    }
}

/// A grid of trades with the group panel shown, and the switch of it.
struct Panel {
    fixture: GroupedFixture,
    root: Entity<PanelRoot>,
    panel: Rc<Cell<bool>>,
}

/// The fixture's columns, one of them not groupable, Qty and Price under a
/// band if `band`, Id not movable if `fixed_id`.
fn schema(
    ungroupable: Option<GroupedColumn>,
    band: bool,
    fixed_id: bool,
) -> GridSchema<GroupedColumn> {
    let column = |column: GroupedColumn| {
        let width = GROUPED_COLUMNS
            .iter()
            .find(|(each, _)| *each == column)
            .map(|(_, width)| *width)
            .expect("a fixture column");
        GridColumn::new(column, column.id())
            .with_width(px(width))
            .with_groupable(Some(column) != ungroupable)
            .with_movable(!(fixed_id && column == Id))
    };
    let schema = GridSchema::new()
        .column(column(Id))
        .column(column(Instrument))
        .column(column(Side));
    let schema = if band {
        schema.band(
            ColumnBand::new("trade", "Trade")
                .column(column(Qty))
                .column(column(GroupedColumn::Price)),
        )
    } else {
        schema
            .column(column(Qty))
            .column(column(GroupedColumn::Price))
    };
    schema.column(column(Trader))
}

/// A grid of 24 trades over `schema`, with the group panel shown,
/// configured by `configure`.
fn open_panel(
    cx: &mut TestAppContext,
    schema: GridSchema<GroupedColumn>,
    configure: impl FnOnce(LiveGridState<Grouped>) -> LiveGridState<Grouped> + 'static,
) -> Panel {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let panel = Rc::new(Cell::new(true));
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
                {
                    let panel = panel.clone();
                    move |_, cx| {
                        cx.new(|cx| PanelRoot {
                            grid: cx.new(|cx| {
                                configure(LiveGridState::new_standalone(
                                    schema,
                                    Grouped,
                                    StoreOptions::default(),
                                    cx,
                                ))
                            }),
                            panel,
                        })
                    }
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&grid, move |_, event: &LiveGridEvent<_, _>, _| {
            events.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(grouped_rows(24).into_iter().map(Arc::new));
    let fixture = GroupedFixture {
        window,
        grid,
        events,
    };
    fixture.settle(cx);
    Panel {
        fixture,
        root,
        panel,
    }
}

/// The panel over the plain fixture columns.
fn open(
    cx: &mut TestAppContext,
    configure: impl FnOnce(LiveGridState<Grouped>) -> LiveGridState<Grouped> + 'static,
) -> Panel {
    open_panel(cx, schema(None, false, false), configure)
}

fn chip_id(column: GroupedColumn) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name("live-grid-group-chip".into())),
        column.id().into(),
    )
}

fn remove_id(column: GroupedColumn) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::Name("live-grid-group-chip-remove".into())),
        column.id().into(),
    )
}

/// The centre of a column's header cell under the panel, for a header of
/// one row.
fn header(column: GroupedColumn) -> Point<Pixels> {
    let mut x = 0.;
    for (each, width) in GROUPED_COLUMNS {
        if each == column {
            return point(px(x + width / 2.), px(ROW * 1.5));
        }
        x += width;
    }
    unreachable!("every column is in the schema")
}

/// A point of the panel at `x`.
fn in_panel(x: f32) -> Point<Pixels> {
    point(px(x), px(PANEL_Y))
}

impl Panel {
    fn settle(&self, cx: &mut TestAppContext) {
        self.fixture.settle(cx);
    }

    fn input<T>(
        &self,
        cx: &mut TestAppContext,
        input: impl FnOnce(&mut Window, &mut App) -> T,
    ) -> T {
        cx.update_window(self.fixture.window, |_, window, cx| input(window, cx))
            .expect("window is open")
    }

    fn try_find(
        &self,
        cx: &mut TestAppContext,
        id: impl Into<ElementId>,
    ) -> Option<ElementSnapshot> {
        let id = id.into();
        self.input(cx, |window, _| window.try_find(id))
    }

    fn find(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> ElementSnapshot {
        let id = id.into();
        self.input(cx, |window, _| window.find(id))
    }

    /// Shows or hides the panel, as an application toggling it.
    fn show_panel(&self, cx: &mut TestAppContext, shown: bool) {
        self.panel.set(shown);
        cx.update(|cx| self.root.update(cx, |_, cx| cx.notify()));
        self.settle(cx);
    }

    /// The grouping as (column, direction), top level first.
    fn levels(&self, cx: &mut TestAppContext) -> Vec<(GroupedColumn, SortDirection)> {
        self.fixture.grid.read_with(cx, |grid, _| {
            grid.group_by()
                .iter()
                .map(|level| {
                    (
                        level.as_column().expect("a column level"),
                        level.direction(),
                    )
                })
                .collect()
        })
    }

    fn set_group_by(&self, cx: &mut TestAppContext, columns: &[GroupedColumn]) {
        self.fixture.grid.update(cx, |grid, cx| {
            grid.set_group_by(columns.iter().map(|column| GroupBy::column(*column)), cx)
        });
        self.settle(cx);
        self.fixture.take_seen();
    }

    /// The grouping events so far; forgets every event.
    fn group_events(&self) -> Vec<GroupSeen> {
        self.fixture
            .take_seen()
            .into_iter()
            .filter(|seen| matches!(seen, GroupSeen::Group(_)))
            .collect()
    }

    fn layout_events(&self) -> usize {
        self.fixture
            .events
            .borrow()
            .iter()
            .filter(|event| matches!(event, LiveGridEvent::LayoutChange { .. }))
            .count()
    }

    fn columns(&self, cx: &mut TestAppContext) -> Vec<GroupedColumn> {
        self.fixture
            .grid
            .read_with(cx, |grid, _| grid.columns().collect())
    }

    /// Presses at `from`, drags past the threshold and on through `path`,
    /// and keeps the button down.
    fn press_and_move(&self, cx: &mut TestAppContext, from: Point<Pixels>, path: &[Point<Pixels>]) {
        self.input(cx, |window, cx| {
            mouse_down_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
            drag_move_to(window, from + point(px(10.), px(0.)), cx);
            for position in path {
                drag_move_to(window, *position, cx);
            }
        });
    }

    fn release(&self, cx: &mut TestAppContext, at: Point<Pixels>) {
        self.input(cx, |window, cx| {
            mouse_up_at(window, at, MouseButton::Left, Modifiers::default(), 1, cx);
            window.render_frame(cx);
        });
        self.settle(cx);
    }

    /// Drags from `from` and drops at `to`.
    fn drag(&self, cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>) {
        self.press_and_move(cx, from, &[to]);
        self.release(cx, to);
    }

    fn click(&self, cx: &mut TestAppContext, at: Point<Pixels>, button: MouseButton) {
        self.input(cx, |window, cx| {
            press_at(window, at, button, Modifiers::default(), 1, cx)
        });
        self.settle(cx);
    }

    fn chip(&self, cx: &mut TestAppContext, column: GroupedColumn) -> Bounds<Pixels> {
        self.find(cx, chip_id(column)).bounds()
    }

    fn focus_grid(&self, cx: &mut TestAppContext) {
        let grid = self.fixture.grid.clone();
        self.input(cx, |window, cx| {
            let handle = grid.focus_handle(cx);
            window.focus(&handle, cx);
            window.render_frame(cx);
        });
    }

    fn press(&self, cx: &mut TestAppContext, key: &str) {
        self.input(cx, |window, cx| window.press(key, cx));
        self.settle(cx);
    }

    fn dispatch(&self, cx: &mut TestAppContext, action: impl Action) {
        self.input(cx, |window, cx| {
            window.dispatch_action(action.boxed_clone(), cx);
            window.render_frame(cx);
        });
        self.settle(cx);
    }

    fn is_focused(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> bool {
        self.find(cx, id).focused() == Some(true)
    }

    fn is_menu_open(&self, cx: &mut TestAppContext) -> bool {
        self.fixture
            .grid
            .read_with(cx, |grid, _| grid.is_menu_open())
    }

    /// The items of the open menu, top to bottom.
    fn menu_items(&self, cx: &mut TestAppContext) -> Vec<ElementSnapshot> {
        self.input(cx, |window, _| {
            let mut items: Vec<ElementSnapshot> = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .filter(|item| item.role() == Some(Role::MenuItem) && item.visible())
                .collect();
            items.sort_by(|a, b| {
                a.bounds()
                    .origin
                    .y
                    .partial_cmp(&b.bounds().origin.y)
                    .expect("finite")
            });
            items
        })
    }

    fn menu_labels(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.menu_items(cx)
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// The menu item whose label `matches`.
    fn menu_item(
        &self,
        cx: &mut TestAppContext,
        matches: impl Fn(&str) -> bool,
    ) -> ElementSnapshot {
        self.menu_items(cx)
            .into_iter()
            .find(|item| item.label().is_some_and(&matches))
            .unwrap_or_else(|| panic!("no such item in {:?}", self.menu_labels(cx)))
    }

    fn click_item(&self, cx: &mut TestAppContext, matches: impl Fn(&str) -> bool) {
        let item = self.menu_item(cx, matches);
        self.click(cx, item.bounds().center(), MouseButton::Left);
    }

    fn lines(&self, cx: &mut TestAppContext) -> Vec<LineKind> {
        self.fixture.lines(cx)
    }
}

/// An item labelled `label`.
fn labelled(label: &'static str) -> impl Fn(&str) -> bool {
    move |text| text == label
}

/// An expansion item of a menu: its English label, or its catalog key while
/// the catalog has no entry for it.
fn expansion_item(label: &'static str, key: &'static str) -> impl Fn(&str) -> bool {
    move |text| text == label || text == key
}

#[gpui_kit::test]
fn test_group_panel_builder(cx: &mut TestAppContext) {
    // Off by default: a grid that does not ask for the panel shows none.
    let plain = open_grouped(cx, grouped_rows(4), |grid| grid);
    assert!(
        !plain
            .grid
            .read_with(cx, |grid, _| grid.is_group_panel_shown())
    );
    let absent = cx
        .update_window(plain.window, |_, window, _| {
            window.try_find("live-grid-group-panel")
        })
        .expect("window is open");
    assert!(absent.is_none());

    let fixture = open(cx, |grid| grid);
    assert!(
        fixture
            .fixture
            .grid
            .read_with(cx, |grid, _| grid.is_group_panel_shown())
    );
    let panel = fixture.find(cx, "live-grid-group-panel");
    assert_eq!(panel.role(), Some(Role::Toolbar));
    assert_eq!(panel.label(), Some("Grouping"));
    assert_eq!(panel.bounds().origin.y, px(0.));
    assert_eq!(panel.bounds().size.height, px(ROW));
    assert_eq!(
        fixture.find(cx, "live-grid-group-panel-hint").label(),
        Some("Drag a column header here to group by it")
    );
    // The header and the rows start one header row lower: one row fewer.
    assert_eq!(
        fixture.find(cx, "live-grid-header").bounds().origin.y,
        px(ROW)
    );
    let shown = fixture.fixture.visible_keys(cx).len();
    assert_eq!(shown, 22);

    fixture.show_panel(cx, false);
    assert!(fixture.try_find(cx, "live-grid-group-panel").is_none());
    assert_eq!(
        fixture.find(cx, "live-grid-header").bounds().origin.y,
        px(0.)
    );
    assert_eq!(fixture.fixture.visible_keys(cx).len(), shown + 1);
    fixture.show_panel(cx, true);
    assert!(fixture.try_find(cx, "live-grid-group-panel").is_some());

    // A chip: a button named by its title and direction, numbered by its
    // level; its remove button named by the title.
    fixture.set_group_by(cx, &[Instrument, Side]);
    let chip = fixture.find(cx, chip_id(Side));
    assert_eq!(chip.role(), Some(Role::Button));
    assert_eq!(chip.label(), Some("side, Sorted ascending"));
    let remove = fixture.find(cx, remove_id(Instrument));
    assert_eq!(remove.role(), Some(Role::Button));
    assert_eq!(remove.label(), Some("Remove grouping by instrument"));
    assert!(fixture.try_find(cx, "live-grid-group-panel-hint").is_none());
    assert!(fixture.chip(cx, Instrument).right() < fixture.chip(cx, Side).left());

    // Columns are groupable unless they say otherwise.
    let column = GridColumn::new(Trader, "Trader");
    assert!(column.is_groupable());
    assert!(!column.with_groupable(false).is_groupable());
}

#[gpui_kit::test]
fn test_drag_header_into_panel(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    let columns = fixture.columns(cx);

    // Into the empty panel: the first level, ascending.
    fixture.drag(cx, header(Instrument), in_panel(300.));
    assert_eq!(fixture.levels(cx), [(Instrument, Ascending)]);
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // Side into the gap before the Instrument chip. While over the panel the
    // insertion line is the panel's: the header shows none, and the column
    // does not move.
    let instrument = fixture.chip(cx, Instrument);
    let before = point(instrument.left() + px(2.), px(PANEL_Y));
    fixture.press_and_move(cx, header(Side), &[before]);
    assert!(fixture.try_find(cx, "live-grid-group-panel-gap").is_some());
    assert!(fixture.try_find(cx, "live-grid-drop-gap").is_none());
    fixture.release(cx, before);
    assert!(fixture.try_find(cx, "live-grid-group-panel-gap").is_none());
    assert_eq!(
        fixture.levels(cx),
        [(Side, Ascending), (Instrument, Ascending)]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // Instrument again, after the last chip, where its level is: no second
    // level, no event, no line.
    fixture.press_and_move(cx, header(Instrument), &[in_panel(500.)]);
    assert!(fixture.try_find(cx, "live-grid-group-panel-gap").is_none());
    fixture.release(cx, in_panel(500.));
    assert_eq!(
        fixture.levels(cx),
        [(Side, Ascending), (Instrument, Ascending)]
    );
    assert!(fixture.group_events().is_empty());

    // Before the first chip: its level moves there.
    let side = fixture.chip(cx, Side);
    fixture.drag(
        cx,
        header(Instrument),
        point(side.left() + px(2.), px(PANEL_Y)),
    );
    assert_eq!(
        fixture.levels(cx),
        [(Instrument, Ascending), (Side, Ascending)]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // The columns stayed where they were.
    assert_eq!(fixture.columns(cx), columns);
    assert_eq!(fixture.layout_events(), 0);
}

#[gpui_kit::test]
fn test_panel_refuses_drops(cx: &mut TestAppContext) {
    // Trader is not groupable; Qty and Price are under a band (the header
    // is two rows); Id does not move but groups.
    let fixture = open_panel(cx, schema(Some(Trader), true, true), |grid| grid);
    let columns = fixture.columns(cx);
    // The column row of the two-row header, under the panel.
    let cell = |column: GroupedColumn| point(header(column).x, px(ROW * 2.5));

    fixture.press_and_move(cx, cell(Trader), &[in_panel(300.)]);
    assert!(fixture.try_find(cx, "live-grid-group-panel-gap").is_none());
    fixture.release(cx, in_panel(300.));
    // The band, by its top row.
    let band = point(header(Qty).x + px(40.), px(ROW * 1.5));
    fixture.drag(cx, band, in_panel(300.));
    assert!(fixture.levels(cx).is_empty());
    assert!(fixture.group_events().is_empty());
    assert_eq!(fixture.columns(cx), columns);

    // Id groups, though it does not move: dropped on the header, it stays.
    fixture.drag(cx, cell(Id), in_panel(300.));
    assert_eq!(fixture.levels(cx), [(Id, Ascending)]);
    fixture.drag(cx, cell(Id), cell(GroupedColumn::Price));
    assert_eq!(fixture.columns(cx), columns);
    assert_eq!(fixture.layout_events(), 0);

    // A tree view takes no grouping from the panel, and says so.
    let tree = open(cx, |grid| {
        grid.with_view(ViewSpec::new().with_tree(TreeSpec::new()))
    });
    assert_eq!(
        tree.find(cx, "live-grid-group-panel-hint").label(),
        Some("Grouping is off in the tree view")
    );
    tree.drag(cx, header(Instrument), in_panel(300.));
    assert!(tree.levels(cx).is_empty());
    assert!(tree.group_events().is_empty());
    assert!(
        tree.fixture
            .grid
            .read_with(cx, |grid, _| grid.tree().is_some())
    );
}

#[gpui_kit::test]
fn test_chip_gestures(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| grid);
    fixture.set_group_by(cx, &[Instrument, Side]);

    // A click reverses the level, and again.
    let instrument = fixture.chip(cx, Instrument).center();
    fixture.click(cx, instrument, MouseButton::Left);
    assert_eq!(
        fixture.levels(cx),
        [(Instrument, Descending), (Side, Ascending)]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    fixture.click(cx, instrument, MouseButton::Left);
    assert_eq!(fixture.levels(cx)[0], (Instrument, Ascending));
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // The remove button takes the level out.
    let remove = fixture.find(cx, remove_id(Side)).bounds().center();
    fixture.click(cx, remove, MouseButton::Left);
    assert_eq!(fixture.levels(cx), [(Instrument, Ascending)]);
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    assert!(fixture.try_find(cx, chip_id(Side)).is_none());

    // Instrument dragged past Side's centre (its chip ends with the remove
    // button): into the second gap.
    fixture.set_group_by(cx, &[Instrument, Side, Trader]);
    let side = fixture.find(cx, remove_id(Side)).bounds();
    let gap = point(side.right(), px(PANEL_Y));
    let instrument = fixture.chip(cx, Instrument).center();
    fixture.press_and_move(cx, instrument, &[gap]);
    assert!(fixture.try_find(cx, "live-grid-group-panel-gap").is_some());
    fixture.release(cx, gap);
    assert_eq!(
        fixture.levels(cx),
        [
            (Side, Ascending),
            (Instrument, Ascending),
            (Trader, Ascending)
        ]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // Onto its own place: nothing.
    let trader = fixture.chip(cx, Trader);
    fixture.drag(
        cx,
        trader.center(),
        point(trader.right() - px(2.), px(PANEL_Y)),
    );
    assert_eq!(fixture.levels(cx).len(), 3);
    assert!(fixture.group_events().is_empty());

    // Onto the body: the level goes.
    fixture.drag(cx, trader.center(), point(px(200.), px(ROW * 8.)));
    assert_eq!(
        fixture.levels(cx),
        [(Side, Ascending), (Instrument, Ascending)]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    // A right press opens the chip's menu: Move left of the first chip does
    // nothing, Move right moves it.
    let side = fixture.chip(cx, Side).center();
    fixture.click(cx, side, MouseButton::Right);
    assert!(fixture.is_menu_open(cx));
    fixture.click_item(cx, labelled("Move left"));
    assert!(
        fixture.is_menu_open(cx),
        "the first chip does not move left"
    );
    fixture.click_item(cx, labelled("Move right"));
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(
        fixture.levels(cx),
        [(Instrument, Ascending), (Side, Ascending)]
    );
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // And onto the header.
    let side = fixture.chip(cx, Side).center();
    fixture.drag(cx, side, header(Qty));
    assert_eq!(fixture.levels(cx), [(Instrument, Ascending)]);
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    assert_eq!(fixture.layout_events(), 0);
}

#[gpui_kit::test]
fn test_grouping_menu_items(cx: &mut TestAppContext) {
    let fixture = open_panel(cx, schema(Some(Trader), false, false), |grid| grid);
    // A disabled item does nothing and leaves the menu open (menu items do
    // not tell their state to accessibility).
    let assert_disabled = |fixture: &Panel, cx: &mut TestAppContext, label: &'static str| {
        let levels = fixture.levels(cx);
        fixture.click_item(cx, labelled(label));
        assert!(fixture.is_menu_open(cx), "{label} is enabled");
        assert_eq!(fixture.levels(cx), levels);
        assert!(fixture.group_events().is_empty());
    };

    // The header menu of Side: after the column items.
    fixture.click(cx, header(Side), MouseButton::Right);
    let labels = fixture.menu_labels(cx);
    let reset = labels
        .iter()
        .position(|label| label == "Reset columns")
        .expect("the column items");
    assert_eq!(
        labels[reset + 1..reset + 4],
        ["Group by this column", "Ungroup", "Ungroup all"]
    );
    assert_disabled(&fixture, cx, "Ungroup");
    assert_disabled(&fixture, cx, "Ungroup all");
    fixture.click_item(cx, labelled("Group by this column"));
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.levels(cx), [(Side, Ascending)]);
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);

    // The Actions act on the current column, Side; a repeat changes nothing.
    fixture.focus_grid(cx);
    fixture.dispatch(cx, GroupByColumn);
    assert!(fixture.group_events().is_empty());
    fixture.dispatch(cx, UngroupColumn);
    assert!(fixture.levels(cx).is_empty());
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    fixture.dispatch(cx, UngroupColumn);
    assert!(fixture.group_events().is_empty());
    fixture.dispatch(cx, GroupByColumn);
    assert_eq!(fixture.levels(cx), [(Side, Ascending)]);
    fixture.dispatch(cx, ClearGrouping);
    assert!(fixture.levels(cx).is_empty());
    assert_eq!(
        fixture.group_events(),
        [GroupSeen::Group(User), GroupSeen::Group(User)]
    );
    fixture.dispatch(cx, ClearGrouping);
    assert!(fixture.group_events().is_empty());

    // Trader is not groupable: its item is disabled, the Action does nothing.
    fixture.set_group_by(cx, &[Instrument]);
    fixture.click(cx, header(Trader), MouseButton::Right);
    assert_disabled(&fixture, cx, "Group by this column");
    assert_disabled(&fixture, cx, "Ungroup");
    fixture.press(cx, "escape");
    assert!(!fixture.is_menu_open(cx));
    fixture.focus_grid(cx);
    fixture.dispatch(cx, GroupByColumn);
    assert_eq!(fixture.levels(cx), [(Instrument, Ascending)]);
    assert!(fixture.group_events().is_empty());
    // `set_group_by` groups by any column.
    fixture.set_group_by(cx, &[Trader]);
    assert_eq!(fixture.levels(cx), [(Trader, Ascending)]);
}

#[gpui_kit::test]
fn test_chip_keyboard(cx: &mut TestAppContext) {
    // Sorted by quantity, so new quantities reorder the rows.
    let fixture = open(cx, |grid| {
        grid.with_view(ViewSpec::new().with_sort([(Qty, Descending)]))
    });
    fixture.set_group_by(cx, &[Instrument, Side]);

    // Tab from the grid to the first chip.
    fixture.focus_grid(cx);
    fixture.input(cx, |window, cx| {
        window.focus_next(cx);
        window.render_frame(cx);
    });
    assert!(fixture.is_focused(cx, chip_id(Instrument)));

    // Enter and Space reverse its level.
    fixture.press(cx, "enter");
    assert_eq!(fixture.levels(cx)[0], (Instrument, Descending));
    fixture.press(cx, "space");
    assert_eq!(fixture.levels(cx)[0], (Instrument, Ascending));
    assert_eq!(
        fixture.group_events(),
        [GroupSeen::Group(User), GroupSeen::Group(User)]
    );
    assert!(fixture.is_focused(cx, chip_id(Instrument)));

    // Delete removes it; the focus moves to the chip on the right.
    fixture.press(cx, "delete");
    assert_eq!(fixture.levels(cx), [(Side, Ascending)]);
    assert_eq!(fixture.group_events(), [GroupSeen::Group(User)]);
    assert!(fixture.is_focused(cx, chip_id(Side)));

    // Shift+F10 opens the chip's menu; the order holds while it is open.
    fixture.press(cx, "shift-f10");
    assert!(fixture.is_menu_open(cx));
    let labels = fixture.menu_labels(cx);
    assert_eq!(
        labels[..5],
        [
            "Sort ascending",
            "Sort descending",
            "Move left",
            "Move right",
            "Ungroup"
        ]
    );
    fixture.fixture.take_seen();
    let writer = fixture.fixture.writer(cx);
    for mut row in grouped_rows(24) {
        row.qty = -row.qty;
        writer.upsert(row);
    }
    fixture.settle(cx);
    assert!(fixture.fixture.take_seen().contains(&GroupSeen::Held(true)));
    // A menu item: the chip's direction.
    fixture.click_item(cx, labelled("Sort descending"));
    assert!(!fixture.is_menu_open(cx));
    assert_eq!(fixture.levels(cx), [(Side, Descending)]);

    // Escape closes a menu without a change.
    fixture.input(cx, |window, cx| {
        window.dispatch_action(Box::new(gpui_kit_grid::actions::ShowContextMenu), cx);
        window.render_frame(cx);
    });
    fixture.settle(cx);
    fixture.press(cx, "escape");
    assert!(!fixture.is_menu_open(cx));

    // Delete in the grid itself is unbound.
    fixture.focus_grid(cx);
    fixture.fixture.take_seen();
    fixture.press(cx, "delete");
    fixture.press(cx, "backspace");
    assert_eq!(fixture.levels(cx), [(Side, Descending)]);
    assert!(fixture.group_events().is_empty());
}

#[gpui_kit::test]
fn test_captions_only_from_panel_menu(cx: &mut TestAppContext) {
    let fixture = open(cx, |grid| {
        grid.with_view(
            ViewSpec::new()
                .with_group_by([GroupBy::column(Instrument), GroupBy::column(Side)])
                .with_aggregates([(Qty, Aggregate::Sum)]),
        )
    });
    fixture.fixture.take_seen();
    let data = |lines: &[LineKind]| {
        lines
            .iter()
            .filter(|line| matches!(line, LineKind::Data(_)))
            .count()
    };
    let captions = |lines: &[LineKind], depth: u16| {
        lines
            .iter()
            .filter(|line| matches!(line, LineKind::Caption(path) if path.depth() == depth))
            .count()
    };
    assert_eq!(data(&fixture.lines(cx)), 24);

    // The panel's own menu, right of the chips.
    fixture.click(cx, in_panel(WIDTH - 20.), MouseButton::Right);
    assert!(fixture.is_menu_open(cx));
    fixture.click_item(
        cx,
        expansion_item("Captions only", "live_grid.menu.captions_only"),
    );
    assert_eq!(
        fixture
            .fixture
            .grid
            .read_with(cx, |grid, _| grid.expand_depth()),
        ExpandDepth::Captions
    );
    assert!(
        fixture
            .fixture
            .take_seen()
            .contains(&GroupSeen::Expansion(None, User))
    );
    let lines = fixture.lines(cx);
    assert_eq!(data(&lines), 0);
    assert_eq!(captions(&lines, 0), 4);
    assert_eq!(captions(&lines, 1), 8);

    // A third level keeps the rows hidden: captions only follows the levels.
    fixture.drag(cx, header(Trader), in_panel(WIDTH - 20.));
    assert_eq!(fixture.levels(cx).len(), 3);
    let lines = fixture.lines(cx);
    assert_eq!(data(&lines), 0);
    assert_eq!(captions(&lines, 2), 24);

    // Expand all brings them back.
    fixture.click(cx, in_panel(WIDTH - 20.), MouseButton::Right);
    fixture.click_item(
        cx,
        expansion_item("Expand all", "live_grid.menu.expand_all"),
    );
    assert_eq!(
        fixture
            .fixture
            .grid
            .read_with(cx, |grid, _| grid.expand_depth()),
        ExpandDepth::All
    );
    assert_eq!(data(&fixture.lines(cx)), 24);

    // A programmatic grouping is `Programmatic`; only the user's are `User`.
    fixture.fixture.take_seen();
    fixture
        .fixture
        .grid
        .update(cx, |grid, cx| grid.set_group_by([], cx));
    assert_eq!(fixture.group_events(), [GroupSeen::Group(Programmatic)]);
}
