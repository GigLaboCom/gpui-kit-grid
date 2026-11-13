//! The ships example itself: the vessels table, the map, the feed's rate,
//! the selected vessel and the status bar, over the driver. The native
//! `main` (`main.rs`) opens it in a window; the browser build
//! (`crates/grid-web/apps/ships`, the page `ships/`) in the page's canvas.
//! Both call `setup` once, then `open` with their [`Settings`].

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    tag::Tag,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{
    GridStore, LiveGrid, LiveGridEvent, LiveGridState, SelectionMode, SortDirection,
};

mod ais;
mod detail;
pub mod driver;
pub mod fleet;
pub mod live;
mod map;
// Shared with the transit example's Helsinki feed.
#[path = "../mqtt/mod.rs"]
pub mod mqtt;
#[cfg(not(target_family = "wasm"))]
pub mod native;
mod rate;
pub mod rows;
#[cfg(feature = "test-support")]
pub mod snapshot;
pub mod source;
pub mod speed;
pub mod table;
mod theme;
mod ui;

pub use self::theme::ShipsTheme;
use self::{
    driver::{Control, Driver, EventSink, Fallback, FleetFrame},
    live::{Connector, LiveSource},
    map::AREAS,
    rows::{Clock, ShipColumn},
    source::{Connection, Recording, ReplaySource, VesselSource},
    speed::SpeedLimits,
    table::{Grouping, ShipTable},
    ui::{GAP, NARROW, PAGE_PADDING, ROW_HEIGHT, SHORT},
};
use crate::trading::theme::TradingTheme as _;

gpui_kit::actions!(
    ships,
    [
        /// Flat, grouped by type, grouped by status, flat again.
        CycleGrouping,
        /// Shows only the moving vessels, or every vessel.
        ToggleMoving,
        /// The next area of the map.
        CycleArea,
        /// Moves the focus to the search box.
        FocusSearch,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Ships";
/// Around the table: its letter keys work there, never in the search box.
const TABLE_CONTEXT: &str = "ShipsTable";
const TABLE_KEYS: &str = "ShipsTable > LiveGrid";

/// The keys, as data: the bindings and the app bar's hints are both made
/// from this table. The letters act in the table, which has the keyboard
/// at start; Enter in the search box gives it back.
const KEYS: [(&str, Command); 4] = [
    ("g", Command::Grouping),
    ("m", Command::Moving),
    ("a", Command::Area),
    ("secondary-f", Command::FocusSearch),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Grouping,
    Moving,
    Area,
    FocusSearch,
}

impl Command {
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let table = Some(TABLE_KEYS);
        match self {
            Self::Grouping => KeyBinding::new(keystrokes, CycleGrouping, table),
            Self::Moving => KeyBinding::new(keystrokes, ToggleMoving, table),
            Self::Area => KeyBinding::new(keystrokes, CycleArea, table),
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, Some(KEY_CONTEXT)),
        }
    }

    /// The app bar's word for it.
    fn hint(self) -> &'static str {
        match self {
            Self::Grouping => "Group",
            Self::Moving => "Moving",
            Self::Area => "Area",
            Self::FocusSearch => "Search",
        }
    }
}

/// Every binding of `KEYS`.
fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// The attribution Fintraffic asks for, wherever the data shows.
pub const ATTRIBUTION: &str = "Source: Fintraffic / digitraffic.fi, license CC 4.0 BY";

/// Where the vessels come from.
pub enum SourcePlan {
    /// Live from Digitraffic over `connector`; `fallback` replays a
    /// recording if Digitraffic, or its MQTT feed, cannot be reached.
    Live {
        connector: Box<dyn Connector>,
        fallback: Option<Fallback>,
    },
    /// A recording at `speed`, or why there is none.
    Replay {
        recording: Result<Arc<Recording>, String>,
        speed: f64,
    },
}

/// What the example starts with: natively from the environment
/// (`native::settings_from_env`), in the browser from the page.
pub struct Settings {
    source: SourcePlan,
    sink: Option<Box<dyn EventSink>>,
    notice: Option<SharedString>,
    seed: u64,
}

impl Settings {
    pub fn new(source: SourcePlan, seed: u64) -> Self {
        Self {
            source,
            sink: None,
            notice: None,
            seed,
        }
    }

    /// Every event also goes to `sink` (natively, a recording file).
    pub fn with_sink(mut self, sink: Option<Box<dyn EventSink>>) -> Self {
        self.sink = sink;
        self
    }

    /// Why the source is what it is, shown above the table.
    #[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
    pub fn with_notice(mut self, notice: Option<SharedString>) -> Self {
        self.notice = notice;
        self
    }
}

/// gpui-kit, the grid's key bindings, then `configure`. Call once.
pub fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// fonts, and the keys.
pub fn configure(cx: &mut App) {
    crate::trading::theme::install::<ShipsTheme>(cx);
    ShipsTheme::Bridge.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The ships view in `window`, the keyboard in its table, its driver
/// started.
pub fn open(settings: Settings, window: &mut Window, cx: &mut App) -> Entity<Ships> {
    let ships = cx.new(|cx| Ships::new(settings, window, cx));
    let focus = ships.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    ships
}

/// The grid's counters, refreshed every second.
#[derive(Default)]
struct Status {
    rebuild: Duration,
    frame: Duration,
}

pub struct Ships {
    control: Arc<Control>,
    grid: Entity<LiveGridState<ShipTable>>,
    search: Entity<InputState>,
    grouping: Grouping,
    moving_only: bool,
    area_ix: usize,
    frame: Arc<FleetFrame>,
    status: Status,
    /// Where the map was painted last, for clicks on it, and its paths.
    map_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    map_cache: Rc<RefCell<map::MapCache>>,
    /// Bumped by every frame taken from the driver.
    frame_version: u64,
    /// What the vessels come from, for the status bar.
    source_label: SharedString,
    notice: Option<SharedString>,
    /// Why there is nothing to show, if there is not.
    error: Option<SharedString>,
    /// The vessel selected in the table once its row arrives, and whether
    /// the table scrolls to it; the first rows select the fastest vessel.
    pending_selection: Option<(u32, bool)>,
    selected_once: bool,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Ships {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            source,
            sink,
            notice,
            seed,
        } = settings;
        let control = Control::new();
        let store = GridStore::new(table::store_options(), cx);
        let grid = cx.new(|cx| {
            let mut grid = LiveGridState::new(table::schema(), ShipTable, store.clone(), cx)
                .with_view_id("vessels")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(crate::trading::paint::quiet_changes())
                .with_quick_filter_columns(table::SEARCHED)
                .with_view(table::view(Grouping::None, false, table::default_sort()));
            for (column, visible) in Grouping::None.column_visibility() {
                grid.set_column_visible(column, visible, cx);
            }
            grid
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search name, destination, call sign or MMSI")
        });
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe_in(&search, window, |ships, search, event, window, cx| {
                match event {
                    InputEvent::Change => {
                        let text = search.read(cx).value();
                        ships
                            .grid
                            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                    }
                    // Back to the table, where the letter keys work.
                    InputEvent::PressEnter { .. } => {
                        let focus = ships.grid.focus_handle(cx);
                        window.focus(&focus, cx);
                    }
                    _ => {}
                }
            }),
        ];
        let mut tasks = vec![Self::poll(cx)];
        let (source, fallback, source_label, error): (
            Option<Box<dyn VesselSource>>,
            Option<Fallback>,
            SharedString,
            Option<SharedString>,
        ) = match source {
            SourcePlan::Live {
                connector,
                fallback,
            } => (
                Some(Box::new(LiveSource::new(connector, seed))),
                fallback,
                "Digitraffic AIS over MQTT".into(),
                None,
            ),
            SourcePlan::Replay {
                recording: Ok(recording),
                speed,
            } => {
                let mut label = String::from("a recording of ");
                Clock::write(recording.start_ms(), &mut label);
                label.push_str(" UTC");
                if speed != 1. {
                    label.push_str(&format!(" at ×{speed}"));
                }
                (
                    Some(Box::new(ReplaySource::new(recording, speed))),
                    None,
                    label.into(),
                    None,
                )
            }
            SourcePlan::Replay {
                recording: Err(error),
                ..
            } => {
                err!("ships: no recording ({error})");
                (None, None, "no source".into(), Some(error.into()))
            }
        };
        if let Some(source) = source {
            let driver = Driver::new(source, store.writer(), control.clone())
                .with_fallback(fallback)
                .with_speed_rule(Arc::new(SpeedLimits::default()))
                .with_sink(sink)
                .with_notice(notice.as_ref().map(|notice| notice.as_ref().into()));
            tasks.push(driver::start(driver, cx));
        }
        Self {
            control,
            grid,
            search,
            grouping: Grouping::None,
            moving_only: false,
            area_ix: 0,
            frame: Arc::default(),
            status: Status::default(),
            map_bounds: Rc::default(),
            map_cache: Rc::default(),
            frame_version: 0,
            source_label,
            notice,
            error,
            pending_selection: None,
            selected_once: false,
            _tasks: tasks,
            _subscriptions: subscriptions,
        }
    }

    /// Four times a second: a new frame from the driver; the counters once
    /// a second.
    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut seen = 0;
            let mut ticks = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                ticks += 1;
                let Ok(()) = this.update(cx, |ships, cx| {
                    let version = ships.control.version();
                    if version != seen {
                        seen = version;
                        ships.frame = ships.control.frame();
                        ships.frame_version += 1;
                        if ships.notice.is_none() {
                            ships.notice = ships.frame.notice.as_deref().map(SharedString::from);
                        }
                    }
                    ships.select_first(cx);
                    ships.select_pending(cx);
                    if ticks % 4 == 0 {
                        let stats = ships.grid.read(cx).stats();
                        ships.status = Status {
                            rebuild: stats.last_rebuild(),
                            frame: stats.last_prepaint(),
                        };
                    }
                    cx.notify();
                }) else {
                    break;
                };
            }
        })
    }

    /// Once the first rows are in, selects the vessel on top (the fastest)
    /// so the card and the map show one.
    fn select_first(&mut self, cx: &mut Context<Self>) {
        if self.selected_once {
            return;
        }
        let grid = self.grid.read(cx);
        let snapshot = grid.snapshot();
        let first = grid
            .visible_keys()
            .iter()
            .copied()
            .find(|mmsi| snapshot.get(mmsi).is_some_and(|row| !row.stale));
        if let Some(mmsi) = first {
            self.selected_once = true;
            self.control.select(Some(mmsi));
            self.pending_selection = Some((mmsi, false));
        }
    }

    /// Selects the vessel asked for once its row is in the table, and
    /// scrolls to it if asked.
    fn select_pending(&mut self, cx: &mut Context<Self>) {
        let Some((mmsi, scroll)) = self.pending_selection else {
            return;
        };
        if self.grid.read(cx).snapshot().get(&mmsi).is_some() {
            self.pending_selection = None;
            self.grid.update(cx, |grid, cx| {
                grid.select_key(Some(mmsi), cx);
                if scroll {
                    grid.scroll_to_key(&mmsi, cx);
                }
            });
        }
    }

    /// Selects the vessel `mmsi` in the table, the card and the map.
    pub fn select(&mut self, mmsi: u32, cx: &mut Context<Self>) {
        self.selected_once = true;
        self.control.select(Some(mmsi));
        self.pending_selection = Some((mmsi, true));
        self.select_pending(cx);
        cx.notify();
    }

    /// Groups the table by `grouping`.
    pub fn set_grouping(&mut self, grouping: Grouping, cx: &mut Context<Self>) {
        if self.grouping != grouping {
            self.grouping = grouping;
            self.apply_view(cx);
        }
    }

    /// Shows only the moving vessels, or every vessel.
    pub fn set_moving_only(&mut self, moving_only: bool, cx: &mut Context<Self>) {
        if self.moving_only != moving_only {
            self.moving_only = moving_only;
            self.apply_view(cx);
        }
    }

    /// Shows the area `ix` of `map::AREAS` on the map.
    pub fn set_area(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.area_ix = ix % AREAS.len();
        cx.notify();
    }

    fn apply_view(&mut self, cx: &mut Context<Self>) {
        let (grouping, moving_only) = (self.grouping, self.moving_only);
        self.grid.update(cx, |grid, cx| {
            let sort = grid.sort().to_vec();
            grid.set_view(table::view(grouping, moving_only, sort), cx);
            for (column, visible) in grouping.column_visibility() {
                grid.set_column_visible(column, visible, cx);
            }
        });
        cx.notify();
    }

    /// A selected vessel goes to the card and the map.
    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<ShipTable>>,
        event: &LiveGridEvent<u32, ShipColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::SelectionChange { selection, .. } = event
            && let Some(mmsi) = selection.iter().next()
        {
            self.selected_once = true;
            self.control.select(Some(*mmsi));
            cx.notify();
        }
    }

    /// A click on the map selects the vessel nearest to it.
    fn on_map_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.map_bounds.get() else {
            return;
        };
        if !bounds.contains(&event.position) {
            return;
        }
        let area = AREAS[self.area_ix];
        if let Some(mmsi) = map::nearest(&self.frame.dots, area, bounds, event.position, 14.) {
            self.select(mmsi, cx);
        }
    }

    fn on_cycle_grouping(&mut self, _: &CycleGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.set_grouping(self.grouping.next(), cx);
    }

    fn on_toggle_moving(&mut self, _: &ToggleMoving, _: &mut Window, cx: &mut Context<Self>) {
        self.set_moving_only(!self.moving_only, cx);
    }

    fn on_cycle_area(&mut self, _: &CycleArea, _: &mut Window, cx: &mut Context<Self>) {
        self.set_area(self.area_ix + 1, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The connection as a tag: live in `success`, reconnecting in
    /// `warning`, unreachable in `danger`, a replay plain.
    fn connection_tag(&self) -> Tag {
        match &self.frame.connection {
            Connection::Live => Tag::success().small().child("LIVE"),
            Connection::Connecting => Tag::secondary().outline().small().child("connecting"),
            Connection::Reconnecting { in_s, .. } if *in_s > 0 => Tag::warning()
                .small()
                .child(format!("reconnecting in {in_s} s")),
            Connection::Reconnecting { .. } => Tag::warning().small().child("reconnecting"),
            Connection::Unreachable { .. } => Tag::danger().small().child("unreachable"),
            Connection::Replay { .. } => Tag::info().outline().small().child("REPLAY"),
        }
    }

    /// The connection in words, for the status bar and the feed panel.
    fn connection_text(&self) -> String {
        match &self.frame.connection {
            Connection::Live => "live from Digitraffic".into(),
            Connection::Connecting => "connecting to Digitraffic".into(),
            Connection::Reconnecting {
                attempt,
                in_s,
                reason,
            } => format!("reconnecting in {in_s} s (attempt {attempt}: {reason})"),
            Connection::Unreachable { reason } => format!("Digitraffic unreachable: {reason}"),
            Connection::Replay { loops } => match loops {
                0 => format!("replaying {}", self.source_label),
                _ => format!("replaying {}, loop {}", self.source_label, loops + 1),
            },
        }
    }

    /// The mark, the connection, the clock and the keys.
    fn render_app_bar(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let narrow = room == Room::Narrow;
        let theme = cx.theme();
        let mut clock = String::new();
        if self.frame.clock_ms > 0. {
            Clock::write(self.frame.clock_ms, &mut clock);
            clock.push_str(" UTC");
        }
        let hints = KEYS.iter().filter_map(|(keystrokes, command)| {
            let keystroke = Keystroke::parse(keystrokes).ok()?;
            Some(
                h_flex()
                    .gap(px(5.))
                    .child(Kbd::new(keystroke).outline())
                    .child(command.hint()),
            )
        });
        h_flex()
            .flex_none()
            .w_full()
            .h(px(44.))
            .px(PAGE_PADDING + px(6.))
            .gap(px(16.))
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .child(div().w(px(14.)).h(px(3.)).bg(theme.primary).rounded(px(1.)))
                    .child(
                        div()
                            .text_size(px(19.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("ships"),
                    )
                    .when(!narrow, |mark| {
                        mark.child(
                            div()
                                .text_color(theme.muted_foreground)
                                // The connection tag says live or replay.
                                .child("every vessel around Finland, from AIS"),
                        )
                    }),
            )
            .child(self.connection_tag())
            .when(!narrow, |bar| {
                bar.child(
                    ui::figures(cx)
                        .flex_none()
                        .text_color(theme.foreground)
                        .child(SharedString::from(clock)),
                )
            })
            .child(div().flex_1())
            .when(!narrow, |bar| {
                bar.child(
                    h_flex()
                        .flex_none()
                        .gap(px(12.))
                        .text_color(theme.muted_foreground)
                        .children(hints),
                )
            })
    }

    /// The search box, the grouping and the moving filter.
    fn render_toolbar(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let narrow = room == Room::Narrow;
        let moving_only = self.moving_only;
        let search = Input::new(&self.search)
            .prefix(Icon::new(IconName::Search).small())
            .cleanable(true)
            .small()
            .aria_label("Search vessels");
        let grouping = TabBar::new("grouping")
            .segmented()
            .small()
            .selected_index(self.grouping.ix())
            .children(Grouping::ALL.map(|grouping| {
                Tab::new().label(if narrow {
                    grouping.short_label()
                } else {
                    grouping.label()
                })
            }))
            .on_click(cx.listener(|ships, ix: &usize, _, cx| {
                if let Some(grouping) = Grouping::ALL.get(*ix) {
                    ships.set_grouping(*grouping, cx);
                }
            }));
        let moving = Button::new("moving-only")
            .label(if narrow { "Moving" } else { "Moving only" })
            .small()
            .toggled(moving_only)
            .map(|button| {
                if moving_only {
                    button.primary()
                } else {
                    button.outline()
                }
            })
            .tooltip("Show only the vessels making way (M)")
            .on_click(cx.listener(move |ships, _, _, cx| ships.set_moving_only(!moving_only, cx)));
        h_flex()
            .flex_none()
            .w_full()
            .flex_wrap()
            .px(ui::PANEL_PADDING)
            .py(px(7.))
            .gap(px(8.))
            // On a phone: the search box and the filter on one line, the
            // grouping on the next.
            .map(|toolbar| {
                if narrow {
                    toolbar
                        .child(
                            h_flex()
                                .w_full()
                                .gap(px(8.))
                                .child(div().flex_1().min_w_0().child(search))
                                .child(moving),
                        )
                        .child(grouping)
                } else {
                    toolbar
                        .child(search.w(px(300.)))
                        .child(grouping)
                        .child(moving)
                }
            })
    }

    /// The table, or why there is none.
    fn render_table(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let warning = theme.warning;
        let counts = self.frame.counts;
        let shown = self.grid.read(cx).projection().row_count();
        let detail = (counts.vessels > 0).then(|| {
            SharedString::from(format!(
                "{} vessels · {} moving · {} shown",
                counts.vessels, counts.moving, shown
            ))
        });
        let body = match &self.error {
            Some(error) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(24.))
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.danger)
                        .child("No vessels to show"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(error.clone()),
                )
                .child(div().child(
                    "Run it live, or record a few minutes first: SHIPS_RECORD=<file> (see main.rs).",
                ))
                .into_any_element(),
            None => ui::grid_frame(cx)
                .key_context(TABLE_CONTEXT)
                .child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Vessels"),
                )
                .into_any_element(),
        };
        ui::panel(cx)
            .size_full()
            .child(ui::panel_header("Vessels", detail, cx))
            .child(self.render_toolbar(room, cx))
            .when_some(self.notice.clone(), |panel, notice| {
                panel.child(
                    div()
                        .flex_none()
                        .px(ui::PANEL_PADDING)
                        .py(px(5.))
                        .bg(warning.opacity(0.14))
                        .text_color(warning)
                        .child(notice),
                )
            })
            .child(body)
    }

    /// The map with its areas and legend: the legend's types alone when
    /// short, no legend on a phone.
    fn render_map(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let frame = self.frame.clone();
        let area = AREAS[self.area_ix];
        let detail = (frame.counts.vessels > 0 && room != Room::Narrow).then(|| {
            SharedString::from(format!(
                "{} vessels · {} moving",
                frame.dots.len(),
                frame.counts.moving
            ))
        });
        let (muted, border) = (theme.muted_foreground, theme.border);
        let legend = ShipCategoryLegend::ITEMS.iter().map(|category| {
            let color = theme::category_color(*category, theme);
            h_flex()
                .gap(px(4.))
                .child(div().size(px(7.)).rounded_full().bg(color))
                .child(category.label())
        });
        ui::panel(cx)
            .size_full()
            .child(
                ui::panel_header("Sea", detail, cx)
                    .child(div().flex_1())
                    .child(
                        TabBar::new("area")
                            .segmented()
                            .xsmall()
                            .selected_index(self.area_ix)
                            .children(AREAS.map(|area| {
                                Tab::new().label(if room == Room::Narrow {
                                    area.short_label
                                } else {
                                    area.label
                                })
                            }))
                            .on_click(
                                cx.listener(|ships, ix: &usize, _, cx| ships.set_area(*ix, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_map_mouse_down))
                    .child(map::map(
                        frame,
                        self.frame_version,
                        area,
                        self.control.selected(),
                        self.map_bounds.clone(),
                        self.map_cache.clone(),
                        cx,
                    )),
            )
            .when(room != Room::Narrow, |panel| {
                panel.child(
                    h_flex()
                        .flex_none()
                        .flex_wrap()
                        .px(ui::PANEL_PADDING)
                        .py(px(6.))
                        .gap_x(px(12.))
                        .gap_y(px(2.))
                        .text_xs()
                        .text_color(muted)
                        .border_t_1()
                        .border_color(border)
                        .children(legend)
                        .when(room == Room::Roomy, |legend| {
                            legend.child("arrows moving, dots still")
                        }),
                )
            })
    }

    /// Messages a second and the connection; the chart alone when short.
    fn render_feed(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let short = room != Room::Roomy;
        let theme = cx.theme();
        let frame = self.frame.clone();
        let detail = SharedString::from(format!(
            "{:.1} msg/s · {} messages",
            frame.rate, frame.messages
        ));
        ui::panel(cx)
            .flex_none()
            .child(ui::panel_header("Feed", Some(detail), cx))
            .child(
                v_flex()
                    .px(ui::PANEL_PADDING)
                    .py(px(8.))
                    .gap(px(6.))
                    .child(rate::rate_chart(
                        frame.clone(),
                        if short {
                            rate::SHORT_HEIGHT
                        } else {
                            rate::HEIGHT
                        },
                        cx,
                    ))
                    .when(!short, |feed| {
                        feed.child(
                            h_flex()
                                .gap(px(12.))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(swatch(theme.chart_1, "locations"))
                                .child(swatch(theme.chart_3, "metadata"))
                                .child(div().flex_1())
                                .child("last 2 minutes"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(self.connection_text()),
                        )
                    }),
            )
    }

    /// The map, the feed and the selected vessel, beside the table.
    fn render_side(&self, room: Room, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let frame = self.frame.clone();
        let selected = self.control.selected();
        let detail = frame
            .selected
            .as_ref()
            .filter(|detail| Some(detail.row.mmsi) == selected);
        v_flex()
            .flex_none()
            .w(px(560.))
            .h_full()
            .gap(GAP)
            .child(div().flex_1().min_h_0().child(self.render_map(room, cx)))
            .child(self.render_feed(room, cx))
            .child(
                ui::panel(cx)
                    .flex_none()
                    .child(ui::panel_header("Vessel", None, cx))
                    .child(match room {
                        Room::Roomy => detail::render_detail(detail, frame.clock_ms, cx),
                        Room::Short | Room::Narrow => {
                            detail::render_strip(detail, frame.clock_ms, cx)
                        }
                    }),
            )
    }

    fn render_status(&self, room: Room, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let counts = self.frame.counts;
        let narrow = room == Room::Narrow;
        if narrow {
            // A phone: the attribution, whole, on two lines if it must.
            return div()
                .flex_none()
                .w_full()
                .px(PAGE_PADDING)
                .py(px(4.))
                .bg(theme.status_bar)
                .border_t_1()
                .border_color(theme.status_bar_border)
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child(ATTRIBUTION)
                .into_any_element();
        }
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .left(SharedString::from(format!(
                "{} vessels · {} moving",
                counts.vessels, counts.moving
            )))
            .left(SharedString::from(format!("{:.1} msg/s", self.frame.rate)))
            .when(!narrow, |bar| {
                bar.left(SharedString::from(format!(
                    "{} berthed · {} quiet",
                    counts.berthed, counts.stale
                )))
                .left(SharedString::from(self.connection_text()))
                .left(SharedString::from(format!(
                    "rebuild {:.1} ms · frame {:.2} ms",
                    ms(self.status.rebuild),
                    ms(self.status.frame),
                )))
            })
            .right(div().text_color(theme.muted_foreground).child(ATTRIBUTION))
            .into_any_element()
    }
}

/// A legend's entry: a square of `color` and its `label`, drawn rather
/// than a glyph the page's fonts may not have.
fn swatch(color: gpui_kit::Hsla, label: &'static str) -> impl IntoElement {
    h_flex()
        .gap(px(4.))
        .child(div().size(px(7.)).bg(color))
        .child(div().text_color(color).child(label))
}

/// How much room the window gives the panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Room {
    /// The table beside the map, the feed and the vessel's card.
    Roomy,
    /// Not tall enough for all three: the feed's chart alone and the
    /// vessel in a strip.
    Short,
    /// A phone: the map over the table, no side panels.
    Narrow,
}

impl Room {
    fn of(window: &Window) -> Self {
        let viewport = window.viewport_size();
        if viewport.width < NARROW {
            Self::Narrow
        } else if viewport.height < SHORT {
            Self::Short
        } else {
            Self::Roomy
        }
    }
}

/// The categories the map's legend names: every one a vessel can show.
struct ShipCategoryLegend;

impl ShipCategoryLegend {
    const ITEMS: [ais::ShipCategory; 9] = [
        ais::ShipCategory::Cargo,
        ais::ShipCategory::Tanker,
        ais::ShipCategory::Passenger,
        ais::ShipCategory::HighSpeed,
        ais::ShipCategory::Tug,
        ais::ShipCategory::Service,
        ais::ShipCategory::Leisure,
        ais::ShipCategory::Special,
        ais::ShipCategory::Other,
    ];
}

/// What the snapshot and the browser page drive and read the example
/// with: natively the snapshot calls part of it, with `test-support` only.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
impl Ships {
    pub fn grid(&self) -> &Entity<LiveGridState<ShipTable>> {
        &self.grid
    }

    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    pub fn is_moving_only(&self) -> bool {
        self.moving_only
    }

    pub fn area(&self) -> &'static str {
        AREAS[self.area_ix].label
    }

    pub fn frame(&self) -> &Arc<FleetFrame> {
        &self.frame
    }

    /// Sorts the table by `column`.
    pub fn set_sort(
        &mut self,
        column: ShipColumn,
        direction: SortDirection,
        cx: &mut Context<Self>,
    ) {
        self.grid
            .update(cx, |grid, cx| grid.set_sort([(column, direction)], cx));
    }

    /// Types `text` into the search box, as the keys would.
    pub fn type_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.grid.update(cx, |grid, cx| {
            grid.set_quick_filter(SharedString::from(text.to_owned()), cx)
        });
    }

    /// The status as one line, for the snapshot and the page.
    pub fn status_line(&self, cx: &App) -> String {
        let frame = &self.frame;
        let counts = frame.counts;
        let stats = self.grid.read(cx).stats();
        format!(
            "{} vessels ({} named) · {} moving · {} berthed · {} quiet · {} shown · {:.1} msg/s · {} messages · {} · first rows {} · frames {} · rebuilds {} · rebuild {:.2} ms · frame {:.2} ms · apply {:.3} ms",
            counts.vessels,
            counts.named,
            counts.moving,
            counts.berthed,
            counts.stale,
            self.grid.read(cx).projection().row_count(),
            frame.rate,
            frame.messages,
            self.connection_text(),
            frame.first_rows.map_or_else(
                || "none".into(),
                |at| format!("at {:.2} s", at.as_secs_f64())
            ),
            stats.frames(),
            stats.rebuilds(),
            ms(stats.last_rebuild()),
            ms(stats.last_prepaint()),
            ms(frame.apply),
        )
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

impl Focusable for Ships {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for Ships {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        let viewport = window.viewport_size();
        let room = Room::of(window);
        let body = if room == Room::Narrow {
            // Stacked: the map over the table.
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(PAGE_PADDING)
                .gap(GAP)
                .child(
                    div()
                        .flex_none()
                        .w_full()
                        .h(viewport.height * 0.3)
                        .child(self.render_map(room, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(self.render_table(room, cx)),
                )
                .into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_start()
                .p(PAGE_PADDING)
                .gap(GAP)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_table(room, cx)),
                )
                .child(self.render_side(room, cx))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_cycle_grouping))
            .on_action(cx.listener(Self::on_toggle_moving))
            .on_action(cx.listener(Self::on_cycle_area))
            .on_action(cx.listener(Self::on_focus_search))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(room, cx))
            .child(body)
            .child(self.render_status(room, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keys_parse_and_name_each_command_once() {
        let bindings = key_bindings();
        assert_eq!(bindings.len(), KEYS.len());
        for (ix, (keystrokes, command)) in KEYS.iter().enumerate() {
            assert!(Keystroke::parse(keystrokes).is_ok(), "{keystrokes}");
            assert!(
                KEYS[ix + 1..]
                    .iter()
                    .all(|(other, again)| other != keystrokes && again != command),
                "{keystrokes}"
            );
        }
    }
}
