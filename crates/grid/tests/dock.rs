//! The dock recipe of saved layouts as a test: a grid panel saves its layout in
//! `Panel::dump`, and its `register_panel` builder restores it with
//! `with_layout`. The grid itself knows nothing of the dock; this file uses
//! only the public API of `gpui_kit::component::dock`, as gpui-kit's own
//! `crates/kit/tests/dock.rs` does.

use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::Sizable as _;
use gpui_kit::component::dock::{
    BasePanel, DockArea, DockAreaState, DockLayout, DockSkin, Panel, PanelEvent, PanelInfo,
    PanelState, PanelStyle, panel_handle, register_panel,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window,
    WindowBounds, WindowOptions, div, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    CellValue, ColumnIx, ColumnZone, ExpandDepth, ExpandTarget, GridColumn, GridColumns,
    GridLayout, GridRow, GridSchema, GroupBy, GroupPath, LiveGrid, LiveGridDelegate, LiveGridState,
    OwnedValue, SortDirection, StoreOptions,
};

const TRADES: &str = "trades";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TradeColumn {
    Id,
    Instrument,
    Price,
    Fee,
}

impl GridColumns for TradeColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Instrument, Self::Price, Self::Fee]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Price => "price",
            Self::Fee => "fee",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

#[derive(Clone)]
struct Trade {
    id: u64,
}

impl GridRow for Trade {
    type Key = u64;
    type Column = TradeColumn;

    fn key(&self) -> &u64 {
        &self.id
    }

    fn value(&self, column: TradeColumn) -> CellValue<'_> {
        match column {
            TradeColumn::Id => CellValue::Int(self.id as i64),
            TradeColumn::Instrument => CellValue::Text("SBER"),
            TradeColumn::Price => CellValue::Int(self.id as i64 * 10),
            TradeColumn::Fee => CellValue::Int(1),
        }
    }
}

struct Trades;

impl LiveGridDelegate for Trades {
    type Row = Trade;
}

fn schema() -> GridSchema<TradeColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(TradeColumn::Id, "Id").with_width(px(80.)))
        .column(GridColumn::new(TradeColumn::Instrument, "Instrument").with_width(px(140.)))
        .column(GridColumn::new(TradeColumn::Price, "Price"))
        .column(GridColumn::new(TradeColumn::Fee, "Fee").with_width(px(90.)))
}

/// The recipe's panel: a grid of one view, its layout under `"grid"`.
struct TradesPanel {
    grid: Entity<LiveGridState<Trades>>,
    focus_handle: FocusHandle,
}

impl TradesPanel {
    fn new(layout: Option<GridLayout>, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| {
            let grid = LiveGridState::new_standalone(schema(), Trades, StoreOptions::default(), cx)
                .with_view_id(TRADES);
            match layout {
                Some(layout) => grid.with_layout(layout),
                None => grid,
            }
        });
        let writer = grid.read(cx).writer();
        writer.replace_all((0..30).map(|id| Arc::new(Trade { id })));
        Self {
            grid,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl BasePanel for TradesPanel {
    fn panel_name(&self) -> &'static str {
        TRADES
    }

    /// The dock pulls the layout when it saves; the grid needs no render.
    fn dump(&self, cx: &App) -> PanelState {
        let mut state = PanelState::new(TRADES);
        let layout = self.grid.read(cx).layout();
        state.info = PanelInfo::panel(serde_json::json!({ "grid": layout }));
        state
    }
}

impl Panel for TradesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Trades"
    }
}

impl EventEmitter<PanelEvent> for TradesPanel {}

impl Focusable for TradesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TradesPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(LiveGrid::new(&self.grid).xsmall().bordered(false))
    }
}

/// The saved grid layout of a panel, if it is one this build can read. A
/// bad layout must not fail `DockArea::load`: the panel opens with the
/// schema's layout instead (and `with_layout` ignores a layout of a newer
/// version).
fn saved_layout(info: &PanelInfo) -> Option<GridLayout> {
    match info {
        PanelInfo::Panel(value) => value
            .get("grid")
            .and_then(|grid| serde_json::from_value::<GridLayout>(grid.clone()).ok()),
        _ => None,
    }
}

/// Registers the trades panel; every panel the registry builds is recorded.
fn register(cx: &mut TestAppContext) -> Rc<RefCell<Vec<Entity<TradesPanel>>>> {
    let built: Rc<RefCell<Vec<Entity<TradesPanel>>>> = Rc::default();
    cx.update(|cx| {
        gpui_kit::init(cx);
        let built = built.clone();
        register_panel(cx, TRADES, move |context, _window, cx| {
            let layout = saved_layout(context.info());
            let panel = cx.new(|cx| TradesPanel::new(layout, cx));
            built.borrow_mut().push(panel.clone());
            panel_handle(panel)
        });
    });
    built
}

struct Workspace {
    area: Entity<DockArea>,
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.area.clone())
    }
}

/// A window with a dock area whose center is set by `build`.
fn open_dock(
    cx: &mut TestAppContext,
    build: impl FnOnce(&Entity<DockArea>, &mut Window, &mut App) + 'static,
) -> (AnyWindowHandle, Entity<DockArea>) {
    let (window, workspace) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(600.), px(400.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let (area, skin) = DockSkin::dock_area("trading", None, window, cx);
                    skin.set_panel_style(PanelStyle::TabBar, cx);
                    build(&area, window, cx);
                    cx.new(|_| Workspace { area })
                },
            )
        })
        .expect("open window");
    let area = workspace.read_with(cx, |workspace, _| workspace.area.clone());
    settle(cx, window);
    (window, area)
}

fn settle(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }
}

/// Loads a saved dock into a new window, through the panel registry.
fn load_dock(cx: &mut TestAppContext, text: String) -> (AnyWindowHandle, Entity<DockArea>) {
    open_dock(cx, move |area, window, cx| {
        let state: DockAreaState = serde_json::from_str(&text).expect("a dock state");
        area.update(cx, |area, cx| area.load(state, window, cx))
            .expect("the dock loads");
    })
}

fn grid_of(cx: &mut TestAppContext, panel: &Entity<TradesPanel>) -> Entity<LiveGridState<Trades>> {
    panel.read_with(cx, |panel, _| panel.grid.clone())
}

fn layout_of(cx: &mut TestAppContext, panel: &Entity<TradesPanel>) -> GridLayout {
    grid_of(cx, panel).read_with(cx, |grid, _| grid.layout())
}

#[gpui_kit::test]
fn test_dock_round_trip_restores_grid_layouts(cx: &mut TestAppContext) {
    let built = register(cx);
    let panels: Rc<RefCell<Vec<Entity<TradesPanel>>>> = Rc::default();
    let (_, area) = open_dock(cx, {
        let panels = panels.clone();
        move |area, window, cx| {
            let first = cx.new(|cx| TradesPanel::new(None, cx));
            let second = cx.new(|cx| TradesPanel::new(None, cx));
            panels.borrow_mut().extend([first.clone(), second.clone()]);
            let layout = DockLayout::tabs()
                .panel_view(panel_handle(first), cx)
                .panel_view(panel_handle(second), cx);
            area.update(cx, |area, cx| area.set_center(layout, window, cx));
        }
    });
    let (first, second) = {
        let panels = panels.borrow();
        (panels[0].clone(), panels[1].clone())
    };
    let second_grid = grid_of(cx, &second);
    assert_eq!(
        second_grid.read_with(cx, |grid, _| grid.stats().frames()),
        0,
        "the second tab was never shown"
    );

    // Both grids change through the API, the hidden one too.
    grid_of(cx, &first).update(cx, |grid, cx| {
        grid.set_column_width(TradeColumn::Instrument, px(200.), cx);
        grid.set_sort([(TradeColumn::Price, SortDirection::Descending)], cx);
    });
    second_grid.update(cx, |grid, cx| {
        grid.set_column_visible(TradeColumn::Fee, false, cx);
        grid.set_column_zone(TradeColumn::Price, ColumnZone::FrozenRight, cx);
        grid.set_column_width(TradeColumn::Price, px(130.), cx);
    });
    let expected = [layout_of(cx, &first), layout_of(cx, &second)];
    assert_eq!(expected[1].view_id(), Some(TRADES));
    assert_ne!(expected[0], expected[1]);

    // Dump to text, load into a new area through the registry.
    let text = serde_json::to_string(&area.read_with(cx, |area, cx| area.dump(cx))).unwrap();
    let (window, _) = load_dock(cx, text);
    let restored = built.borrow().clone();
    assert_eq!(restored.len(), 2);
    assert_eq!(
        [layout_of(cx, &restored[0]), layout_of(cx, &restored[1])],
        expected
    );

    // The second tab, shown for the first time, paints the saved widths.
    cx.update_window(window, |_, window, cx| {
        window.within("tab-bar").click(1usize, cx)
    })
    .expect("window is open");
    settle(cx, window);
    cx.update_window(window, |_, window, _| {
        let header = window.within("live-grid-header");
        let width = |id: &'static str| header.find(id).bounds().size.width;
        assert_eq!(
            [width("id"), width("instrument"), width("price")],
            [px(80.), px(140.), px(130.)]
        );
        assert!(header.try_find("fee").is_none(), "hidden");
    })
    .expect("window is open");
}

/// Replaces the value of every `"grid"` key in a dock state; returns how
/// many it replaced.
fn replace_grid(value: &mut serde_json::Value, with: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => map
            .iter_mut()
            .map(|(key, child)| {
                if key == "grid" {
                    *child = with.clone();
                    1
                } else {
                    replace_grid(child, with)
                }
            })
            .sum(),
        serde_json::Value::Array(items) => {
            items.iter_mut().map(|item| replace_grid(item, with)).sum()
        }
        _ => 0,
    }
}

#[gpui_kit::test]
fn test_dock_bad_grid_layout_falls_back(cx: &mut TestAppContext) {
    let built = register(cx);
    let (_, area) = open_dock(cx, |area, window, cx| {
        let panel = cx.new(|cx| TradesPanel::new(None, cx));
        let layout = DockLayout::tabs().panel_view(panel_handle(panel), cx);
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
    });
    let state = serde_json::to_value(area.read_with(cx, |area, cx| area.dump(cx))).unwrap();

    let newer = serde_json::json!({
        "version": 99,
        "columns": [{ "id": "instrument", "width": 300.0 }],
    });
    // Plain serde reads the newer layout; `with_layout` ignores it.
    assert!(saved_layout(&PanelInfo::panel(serde_json::json!({ "grid": newer }))).is_some());
    for bad in [
        serde_json::json!("not a layout"),
        serde_json::json!({ "version": 1, "columns": "by id" }),
        newer,
    ] {
        let mut state = state.clone();
        assert_eq!(replace_grid(&mut state, &bad), 1);
        built.borrow_mut().clear();
        load_dock(cx, state.to_string());
        let panels = built.borrow().clone();
        assert_eq!(panels.len(), 1, "the panel is built: {bad}");
        grid_of(cx, &panels[0]).read_with(cx, |grid, _| {
            assert_eq!(grid.layout(), grid.default_layout(), "{bad}");
            assert_eq!(grid.column_width(TradeColumn::Instrument), px(140.));
        });
    }
}

/// A dock of two trades panels in tabs; the second tab is never shown.
fn open_two_tabs(
    cx: &mut TestAppContext,
) -> (Entity<DockArea>, Entity<TradesPanel>, Entity<TradesPanel>) {
    let panels: Rc<RefCell<Vec<Entity<TradesPanel>>>> = Rc::default();
    let (_, area) = open_dock(cx, {
        let panels = panels.clone();
        move |area, window, cx| {
            let first = cx.new(|cx| TradesPanel::new(None, cx));
            let second = cx.new(|cx| TradesPanel::new(None, cx));
            panels.borrow_mut().extend([first.clone(), second.clone()]);
            let layout = DockLayout::tabs()
                .panel_view(panel_handle(first), cx)
                .panel_view(panel_handle(second), cx);
            area.update(cx, |area, cx| area.set_center(layout, window, cx));
        }
    });
    let panels = panels.borrow();
    (area, panels[0].clone(), panels[1].clone())
}

#[gpui_kit::test]
fn test_dock_round_trip_with_groups(cx: &mut TestAppContext) {
    let built = register(cx);
    let (area, _, hidden) = open_two_tabs(cx);
    let grid = grid_of(cx, &hidden);
    let sber = GroupPath::new([(
        TradeColumn::Instrument.column_id(),
        OwnedValue::Text("SBER".into()),
    )]);
    let collapsed = ExpandTarget::Group(sber);

    // The hidden grid is grouped by instrument and fee, captions only, with
    // its one instrument collapsed. Never painted, it gives its layout.
    grid.update(cx, |grid, cx| {
        grid.set_group_by(
            [
                GroupBy::column(TradeColumn::Instrument),
                GroupBy::column(TradeColumn::Fee).with_direction(SortDirection::Descending),
            ],
            cx,
        );
        grid.set_expand_depth(ExpandDepth::Captions, cx);
        grid.set_expanded(collapsed.clone(), false, cx);
    });
    assert_eq!(grid.read_with(cx, |grid, _| grid.stats().frames()), 0);
    let expected = layout_of(cx, &hidden);
    assert_eq!(expected.group_by().len(), 2);
    assert_eq!(
        expected
            .expansion()
            .map(|expansion| expansion.toggled().len()),
        Some(1)
    );

    // Dump to text, load into a new area: grouping and expansion are back.
    let text = serde_json::to_string(&area.read_with(cx, |area, cx| area.dump(cx))).unwrap();
    built.borrow_mut().clear();
    load_dock(cx, text);
    let restored = built.borrow()[1].clone();
    assert_eq!(layout_of(cx, &restored), expected);
    grid_of(cx, &restored).read_with(cx, |grid, _| {
        let levels: Vec<&str> = grid
            .group_by()
            .iter()
            .map(|level| level.as_column().map_or("", GridColumns::id))
            .collect();
        assert_eq!(levels, ["instrument", "fee"]);
        assert_eq!(grid.group_by()[1].direction(), SortDirection::Descending);
        assert_eq!(grid.expand_depth(), ExpandDepth::Captions);
        assert!(!grid.is_expanded(&collapsed));
    });
}
