//! The alert console: one row per alert key however often it fires, with
//! the count and the rate the simulator keeps for it, grouped by service or
//! by data center with each group's active alerts by severity in its
//! caption, the worst groups first, and within a group the worst and the
//! fastest firing first; severity chips and a search box over it; Enter,
//! `A`, a double click or the menu's first item acknowledges the selected
//! alerts, or on a caption the whole group. The order can be paused
//! (`ManualReorder`): values keep updating in place, rows stop moving.

use std::{
    fmt::Write as _,
    rc::Rc,
    sync::{Arc, mpsc::Sender},
    time::Duration,
};

use gpui_kit::base::{actions::Confirm, h_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    tab::{Tab, TabBar},
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, Keystroke, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, AggregateStyleContext, CellContent, CellElementContext, CellStyle,
    CellStyleContext, CellTooltip, ColumnSet, GridColumn, GridColumns as _, GridMenuContext,
    GridSchema, GridStore, GroupBy, GroupCaptionContext, HeaderTooltipContext, HighlightPolicy,
    HoldReorder, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, ManualReorder,
    OwnedValue, PinnedEdge, SelectionMode, SortDirection, StoreOptions, ViewSpec,
};

use super::{
    PauseOrder,
    rows::{Alert, AlertColumn, AlertKey, AlertState, Severity},
    sim::{Command, Control},
    theme::status_color,
    ui::{self, PANEL_PADDING, ROW_HEIGHT},
};

/// The key context around the console's grid: `A` acknowledges there.
pub const KEY_CONTEXT: &str = "OpsAlerts";

/// Where the console's grid layout is kept between runs: natively a file
/// (`OPS_LAYOUT`, `native.rs`); the browser keeps none.
pub trait LayoutStore {
    /// The layout saved last, `None` if there is none.
    fn load(&self) -> Option<String>;

    /// Keeps `layout` in place of the one saved last.
    fn save(&self, layout: &str) -> Result<(), String>;

    /// Where the layout is kept, for messages.
    fn name(&self) -> String;
}
/// How long a new or changed alert stays tinted.
const ALERT_FLASH: Duration = Duration::from_millis(1_500);
/// How long a removed alert fades out.
const REMOVAL_FADE: Duration = Duration::from_millis(1_200);
/// A handled alert's text: the foreground, quieter.
const HANDLED_OPACITY: f32 = 0.55;

/// The alert store's options: short flashes, removed alerts fading out.
pub fn store_options() -> StoreOptions {
    StoreOptions::default()
        .with_highlight(HighlightPolicy::new(ALERT_FLASH, 6).with_removal_fade(REMOVAL_FADE))
}

/// What the console groups by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    #[default]
    Service,
    Dc,
}

impl Grouping {
    pub const ALL: [Self; 2] = [Self::Service, Self::Dc];

    pub fn column(self) -> AlertColumn {
        match self {
            Self::Service => AlertColumn::Service,
            Self::Dc => AlertColumn::Dc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Service => "By service",
            Self::Dc => "By data center",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Service => Self::Dc,
            Self::Dc => Self::Service,
        }
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// The alert severities shown: a set over `Severity::ALERTS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeverityFilter(u8);

impl SeverityFilter {
    pub const ALL: Self = Self((1 << Severity::ALERTS.len()) - 1);

    pub fn contains(self, severity: Severity) -> bool {
        self.0 & (1 << severity.ix()) != 0
    }

    pub fn is_all(self) -> bool {
        self == Self::ALL
    }

    /// `severity` in or out; with none left, every one again.
    pub fn toggled(self, severity: Severity) -> Self {
        match self.0 ^ (1 << severity.ix()) {
            0 => Self::ALL,
            set => Self(set & Self::ALL.0),
        }
    }
}

/// The console grid's hooks, colours from the theme only: the severity a
/// pill in its status colour with its shape (`render_cell`; bold while a
/// critical fires, quieter once handled; the value is the `Severity`
/// itself, ranked worst first), the state by what it
/// means (firing in the foreground, acknowledged quieter, resolved in the
/// OK colour), a handled alert's other text quieter, places and times
/// muted. Captions name the group and count its active alerts by
/// severity, icon first. They take `&self` and never read the grid; the
/// styles never allocate (the pill, an element, does).
pub struct Alerts;

impl LiveGridDelegate for Alerts {
    type Row = Alert;

    fn cell_style(&self, cell: &CellStyleContext<'_, Alert>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let alert = cell.row();
        let quiet = theme.foreground.opacity(HANDLED_OPACITY);
        match cell.column() {
            AlertColumn::Severity => {
                let color = status_color(alert.severity, theme);
                style.set_foreground(if alert.state.is_active() {
                    color
                } else {
                    color.opacity(HANDLED_OPACITY)
                });
                if alert.severity == Severity::Critical && alert.state == AlertState::Firing {
                    style.set_font_weight(FontWeight::BOLD);
                }
            }
            AlertColumn::State => match alert.state {
                AlertState::Firing => {
                    style.set_foreground(theme.foreground);
                    style.set_font_weight(FontWeight::BOLD);
                }
                AlertState::Acknowledged => style.set_foreground(quiet),
                AlertState::Resolved => style.set_foreground(status_color(Severity::Ok, theme)),
            },
            AlertColumn::Alert | AlertColumn::Subject | AlertColumn::Count | AlertColumn::Rate
                if alert.state != AlertState::Firing =>
            {
                style.set_foreground(quiet)
            }
            AlertColumn::Service
            | AlertColumn::Dc
            | AlertColumn::FirstSeen
            | AlertColumn::LastSeen => style.set_foreground(theme.muted_foreground),
            _ => {}
        }
        // A removed alert fading out keeps the paint's faded text.
        if let Some(change) = cell.change()
            && change.change() == gpui_kit_grid::RowChange::Removed
        {
            style.set_foreground(change.fade(theme.muted_foreground));
        }
    }

    /// The severity's pill; a removed alert fading out shows its text.
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Alert>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        (cell.column() == AlertColumn::Severity).then(|| {
            let alert = cell.row();
            let opacity = if alert.state.is_active() {
                1.
            } else {
                HANDLED_OPACITY
            };
            let firing = alert.severity == Severity::Critical && alert.state == AlertState::Firing;
            ui::severity_pill(alert.severity, opacity, cell.size(), cx)
                .when(firing, |pill| pill.font_weight(FontWeight::BOLD))
                .into_any_element()
        })
    }

    fn aggregate_style(
        &self,
        cell: &AggregateStyleContext<'_, Alert>,
        style: &mut CellStyle,
        cx: &App,
    ) {
        if cell.column() == AlertColumn::LastSeen {
            style.set_foreground(cx.theme().muted_foreground);
        }
    }

    fn group_caption(&self, caption: &GroupCaptionContext<'_, Alert>, out: &mut String, _: &App) {
        let _ = write!(out, "{} ({})", caption.value_text(), caption.row_count());
        for severity in Severity::ALERTS {
            let count = AlertColumn::counter(severity)
                .and_then(|column| caption.aggregate(column))
                .map_or(0, |value| match value {
                    OwnedValue::Int(count) => *count,
                    _ => 0,
                });
            if count > 0 {
                let _ = write!(out, "   {} {count}", severity.icon());
            }
        }
    }

    /// The count and the rate say who keeps them: the simulator, not the
    /// grid.
    fn header_tooltip(
        &self,
        header: &HeaderTooltipContext<'_, AlertColumn>,
        _: &App,
    ) -> Option<CellTooltip> {
        match header.column() {
            AlertColumn::Count => Some(CellTooltip::text(
                "Firings since the alert first fired, counted by the simulator",
            )),
            AlertColumn::Rate => Some(CellTooltip::text(
                "Firings a second over the last 10 s, counted by the simulator",
            )),
            _ => header.default_tooltip(),
        }
    }

    /// "Acknowledge", or "Acknowledge 12 alerts": the selection, or a
    /// caption's group. First and bold in the menu, above the standard
    /// items (copy, sort, column, group).
    fn primary_command(&self, menu: &GridMenuContext<'_, Alert>, _: &App) -> Option<SharedString> {
        Some(match menu.selection().len() {
            1 => "Acknowledge".into(),
            count => format!("Acknowledge {count} alerts").into(),
        })
    }
}

fn column(column: AlertColumn, title: &'static str, width: f32) -> GridColumn<AlertColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// Severity, state and the alert frozen on the left; the per-severity
/// counters internal: they feed the captions, and no one sees them, the
/// column chooser included.
fn schema() -> GridSchema<AlertColumn> {
    let counters = Severity::ALERTS
        .iter()
        .filter_map(|severity| AlertColumn::counter(*severity))
        .map(|counter| column(counter, counter.id(), 40.).with_internal(true));
    let schema = GridSchema::new()
        .with_frozen(3)
        .column(
            column(AlertColumn::Severity, "Severity", 104.)
                .with_align(gpui_kit_grid::ColumnAlign::Start)
                .with_content(CellContent::Element),
        )
        .column(column(AlertColumn::State, "State", 76.))
        .column(column(AlertColumn::Alert, "Alert", 168.))
        .column(column(AlertColumn::Subject, "Host / service", 140.))
        .column(column(AlertColumn::Service, "Service", 96.))
        .column(column(AlertColumn::Dc, "DC", 56.))
        .column(column(AlertColumn::Count, "Firings", 66.))
        .column(column(AlertColumn::Rate, "Rate", 64.))
        .column(column(AlertColumn::FirstSeen, "First seen", 88.))
        .column(column(AlertColumn::LastSeen, "Last seen", 84.));
    counters.fold(schema, GridSchema::column)
}

/// The order within a group: the worst first, then the fastest firing.
/// Density reads as a sorted number, not as flashing cells.
pub const SORT: [(AlertColumn, SortDirection); 2] = [
    (AlertColumn::Severity, SortDirection::Ascending),
    (AlertColumn::Rate, SortDirection::Descending),
];

/// Grouped by `grouping`, the groups with the most critical alerts first;
/// within a group by [`SORT`]. Each caption sums the counts, the rates and
/// the per-severity counters, and shows the latest firing.
fn view(grouping: Grouping, filter: SeverityFilter) -> ViewSpec<Alert> {
    let counters = Severity::ALERTS
        .iter()
        .filter_map(|severity| AlertColumn::counter(*severity))
        .map(|counter| (counter, Aggregate::Sum));
    let view = ViewSpec::new()
        .with_sort(SORT)
        .with_group_by([GroupBy::column(grouping.column())])
        .with_group_order(AggregateOrder::new(
            AlertColumn::Critical,
            SortDirection::Descending,
        ))
        .with_aggregates(
            [
                (AlertColumn::Count, Aggregate::Sum),
                (AlertColumn::Rate, Aggregate::Sum),
                (AlertColumn::LastSeen, Aggregate::Max),
            ]
            .into_iter()
            .chain(counters),
        );
    if filter.is_all() {
        return view;
    }
    let mut depends_on = ColumnSet::EMPTY;
    depends_on.insert(AlertColumn::Severity.ix());
    view.with_filter(depends_on, move |alert: &Alert| {
        filter.contains(alert.severity)
    })
}

/// The alert console: its grid, the severity chips, the grouping switch,
/// the search box and the acknowledge command.
pub struct AlertConsole {
    grid: Entity<LiveGridState<Alerts>>,
    search: Entity<InputState>,
    grouping: Grouping,
    filter: SeverityFilter,
    commands: Sender<Command>,
    control: Arc<Control>,
    /// `OPS_LAYOUT`: where the grid's layout is saved.
    layout: Option<Rc<dyn LayoutStore>>,
    /// Alerts the console has asked to acknowledge.
    acked: usize,
    /// The order is paused: rows keep their places until resumed.
    paused: bool,
    _subscriptions: Vec<Subscription>,
}

impl AlertConsole {
    pub fn new(
        store: GridStore<Alert>,
        commands: Sender<Command>,
        control: Arc<Control>,
        layout: Option<Rc<dyn LayoutStore>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let grouping = Grouping::default();
        let grid = cx.new(|cx| {
            let mut grid = LiveGridState::new(schema(), Alerts, store, cx)
                .with_view_id("alerts")
                .with_selection_mode(SelectionMode::RowSet)
                .with_change_paint(ui::accent_changes(cx))
                .with_reorder_policy(HoldReorder::default())
                .with_quick_filter_columns([
                    AlertColumn::Severity,
                    AlertColumn::State,
                    AlertColumn::Alert,
                    AlertColumn::Subject,
                    AlertColumn::Service,
                    AlertColumn::Dc,
                ])
                .with_view(view(grouping, SeverityFilter::ALL))
                .with_totals(PinnedEdge::Bottom);
            grid.set_column_visible(grouping.column(), false, cx);
            grid
        });
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search alerts, hosts, services"));
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe(&search, |console, search, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = search.read(cx).value();
                    console.set_search(text, cx);
                }
            }),
        ];
        let mut console = Self {
            grid,
            search,
            grouping,
            filter: SeverityFilter::ALL,
            commands,
            control,
            layout,
            acked: 0,
            paused: false,
            _subscriptions: subscriptions,
        };
        console.restore_layout(cx);
        console
    }

    pub fn grid(&self) -> &Entity<LiveGridState<Alerts>> {
        &self.grid
    }

    pub fn grouping(&self) -> Grouping {
        self.grouping
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Pauses the order or resumes it (WCAG 2.2.2, pause, stop, hide). While
    /// paused the grid holds every new order (`ManualReorder`): values
    /// update in place, new alerts wait in the held order, and a sort,
    /// grouping or filter change still applies. Resuming shows the held
    /// order unless the pointer rests on the grid (`HoldReorder`).
    pub fn toggle_paused(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        let paused = self.paused;
        self.grid.update(cx, |grid, cx| {
            if paused {
                grid.set_reorder_policy(ManualReorder, cx);
            } else {
                grid.set_reorder_policy(HoldReorder::default(), cx);
            }
        });
        cx.notify();
    }

    /// Groups by `grouping` and hides its column, which the captions name.
    pub fn set_grouping(&mut self, grouping: Grouping, cx: &mut Context<Self>) {
        if self.grouping == grouping {
            return;
        }
        let previous = self.grouping;
        self.grouping = grouping;
        let filter = self.filter;
        self.grid.update(cx, |grid, cx| {
            let view = view(grouping, filter).with_sort(grid.sort().to_vec());
            grid.set_view(view, cx);
            grid.set_column_visible(previous.column(), true, cx);
            grid.set_column_visible(grouping.column(), false, cx);
        });
        cx.notify();
    }

    /// Shows or hides the alerts of `severity`.
    pub fn toggle_severity(&mut self, severity: Severity, cx: &mut Context<Self>) {
        self.filter = self.filter.toggled(severity);
        let (grouping, filter) = (self.grouping, self.filter);
        self.grid.update(cx, |grid, cx| {
            let view = view(grouping, filter).with_sort(grid.sort().to_vec());
            grid.set_view(view, cx);
        });
        cx.notify();
    }

    /// Shows the alerts whose text contains every word of `text`.
    pub fn set_search(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
    }

    /// Types `text` into the search box, as the keys would.
    pub fn type_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.set_search(SharedString::from(text.to_owned()), cx);
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Acknowledges what the grid's command scope holds: the selection, or
    /// the focused caption's group. The grid's `Confirm` resolves the scope
    /// and comes back as `LiveGridEvent::Confirm`.
    pub fn ack_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.grid.focus_handle(cx);
        window.focus(&focus, cx);
        window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
    }

    /// Asks the simulator to acknowledge `keys`.
    pub fn ack(&mut self, keys: Arc<[AlertKey]>, cx: &mut Context<Self>) {
        self.acked += keys.len();
        // A simulator that is gone has nothing left to acknowledge.
        let _ = self.commands.send(Command::Ack(keys));
        cx.notify();
    }

    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<Alerts>>,
        event: &LiveGridEvent<AlertKey, AlertColumn>,
        cx: &mut Context<Self>,
    ) {
        match event {
            LiveGridEvent::Confirm { keys, .. } => self.ack(keys.clone(), cx),
            LiveGridEvent::LayoutChange { .. }
            | LiveGridEvent::SortChange { .. }
            | LiveGridEvent::GroupChange { .. }
            | LiveGridEvent::ExpansionChange { .. } => self.save_layout(cx),
            _ => {}
        }
    }

    /// `OPS_LAYOUT`: the saved layout, and the grouping it holds.
    fn restore_layout(&mut self, cx: &mut Context<Self>) {
        let Some(store) = &self.layout else {
            return;
        };
        let Some(text) = store.load() else {
            return;
        };
        let restored = self.grid.update(cx, |grid, cx| {
            let report = grid.import_layout(&text, cx);
            let column = grid.group_by().first().and_then(GroupBy::as_column);
            report.map(|_| column)
        });
        match restored {
            Ok(column) => {
                if let Some(grouping) = Grouping::ALL
                    .into_iter()
                    .find(|grouping| Some(grouping.column()) == column)
                {
                    self.grouping = grouping;
                }
            }
            Err(error) => err!("{}: {error}", store.name()),
        }
    }

    /// Writes the grid's layout to the `OPS_LAYOUT` file: these events
    /// come once per finished gesture, rarely.
    fn save_layout(&self, cx: &App) {
        let Some(store) = &self.layout else {
            return;
        };
        match self.grid.read(cx).export_layout() {
            Ok(text) => {
                if let Err(error) = store.save(&text) {
                    err!("{}: {error}", store.name());
                }
            }
            Err(error) => err!("layout: {error}"),
        }
    }

    /// The severity chips and the search box; in a `narrow` window compact
    /// chips, the grouping switch and the pause button too, wrapping as
    /// they need.
    fn render_toolbar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        h_flex()
            .flex_none()
            .w_full()
            .px(PANEL_PADDING)
            .py(px(8.))
            .gap(px(6.))
            .when(narrow, |toolbar| toolbar.flex_wrap())
            .children(Severity::ALERTS.map(|severity| {
                ui::severity_chip(
                    severity,
                    self.control.active(severity),
                    self.filter.contains(severity),
                    narrow,
                    cx,
                )
                .on_click(
                    cx.listener(move |console, _, _, cx| console.toggle_severity(severity, cx)),
                )
            }))
            .when(narrow, |toolbar| {
                toolbar
                    .child(self.render_grouping(cx))
                    .child(self.render_pause(cx))
            })
            .when(!narrow, |toolbar| toolbar.child(div().flex_1()))
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .small()
                    .aria_label("Search alerts")
                    .map(|input| {
                        if narrow {
                            input.w_full()
                        } else {
                            input.w(px(280.))
                        }
                    }),
            )
    }

    /// By service or by data center.
    fn render_grouping(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        TabBar::new("grouping")
            .segmented()
            .small()
            .selected_index(self.grouping.ix())
            .children(Grouping::ALL.map(|grouping| Tab::new().label(grouping.label())))
            .on_click(cx.listener(|console, ix: &usize, _, cx| {
                if let Some(grouping) = Grouping::ALL.get(*ix) {
                    console.set_grouping(*grouping, cx);
                }
            }))
    }

    /// Pauses the order, or resumes it.
    fn render_pause(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        Button::new("pause-order")
            .label(if self.paused {
                "Resume order"
            } else {
                "Pause order"
            })
            .small()
            .ghost()
            .selected(self.paused)
            .tooltip_with_action(
                "Freeze the order of the alerts, or let it follow the data again",
                &PauseOrder,
                Some(super::KEY_CONTEXT),
            )
            .on_click(cx.listener(|console, _, _, cx| console.toggle_paused(cx)))
    }
}

impl Focusable for AlertConsole {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for AlertConsole {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = window.viewport_size().width < ui::NARROW;
        let rows = self.grid.read(cx).projection().row_count();
        let paused = if self.paused { " · order paused" } else { "" };
        let detail = format!(
            "{} keys · {} shown · {} acknowledged{paused}",
            ui::thousands(self.control.keys()),
            ui::thousands(rows as u64),
            ui::thousands(self.acked as u64),
        );
        let ack_hint = Keystroke::parse("a")
            .ok()
            .map(|key| Kbd::new(key).outline());
        ui::panel(cx)
            .size_full()
            .child(
                ui::panel_header("ALERTS", Some(detail.into()), cx)
                    .child(div().flex_1())
                    .when(!narrow, |header| {
                        header
                            .child(self.render_grouping(cx))
                            .child(self.render_pause(cx))
                    })
                    .child(
                        Button::new("ack")
                            .label("Acknowledge")
                            .small()
                            .primary()
                            .tooltip("Acknowledge the selected alerts, or the focused group")
                            .on_click(cx.listener(|console, _, window, cx| {
                                console.ack_focused(window, cx)
                            })),
                    )
                    .when(!narrow, |header| header.children(ack_hint)),
            )
            .child(self.render_toolbar(narrow, cx))
            .child(
                ui::grid_frame(cx).key_context(KEY_CONTEXT).child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Alerts"),
                ),
            )
    }
}

#[cfg(all(test, feature = "test-support"))]
mod element_tests {
    use gpui_kit::{
        Bounds, Context, Render, TestAppContext, WindowBounds, WindowOptions, size,
        test::TestWindowExt as _,
    };

    use super::super::rows::{Rule, Subject};
    use super::*;

    struct Root {
        grid: Entity<LiveGridState<Alerts>>,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                LiveGrid::new(&self.grid)
                    .with_size(Size::Size(ROW_HEIGHT))
                    .bordered(false),
            )
        }
    }

    fn alert(rule: Rule, node: u32, severity: Severity) -> Alert {
        Alert {
            key: AlertKey {
                rule,
                subject: Subject::Node(node),
            },
            severity,
            state: AlertState::Firing,
            subject: format!("host-{node}").into(),
            service: "billing".into(),
            dc: "wst1".into(),
            count: 1,
            rate: 0.0,
            first_seen: "02:48:00".into(),
            last_seen: "02:48:00".into(),
        }
    }

    #[gpui_kit::test]
    fn test_only_the_severity_cell_has_an_element(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
        });
        let store = cx.update(|cx| GridStore::new(store_options(), cx));
        store.writer().replace_all(
            [
                alert(Rule::HostUnreachable, 1, Severity::Critical),
                alert(Rule::CpuThrottled, 2, Severity::Major),
                alert(Rule::DiskLatency, 3, Severity::Warning),
            ]
            .map(Arc::new),
        );
        let (window, root) = cx
            .update(|cx| {
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1000.), px(400.)),
                    })),
                    ..Default::default()
                };
                gpui_kit::open_window(options, cx, |_, cx| {
                    cx.new(|cx| Root {
                        grid: cx.new(|cx| {
                            LiveGridState::new(schema(), Alerts, store, cx)
                                .with_view(view(Grouping::Service, SeverityFilter::ALL))
                        }),
                    })
                })
            })
            .expect("open window");
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
        let grid = root.read_with(cx, |root, _| root.grid.clone());
        // Three alerts under one caption: a pill each, nothing else.
        let (hosts, rows) = grid.read_with(cx, |grid, _| {
            (grid.stats().cell_hosts(), grid.visible_keys())
        });
        assert_eq!(rows.len(), 3);
        assert_eq!(hosts, 3);
        // Sorted by severity, worst first: the value is the severity.
        let severities: Vec<Severity> = rows
            .iter()
            .filter_map(|key| grid.read_with(cx, |grid, _| grid.snapshot().get(key).cloned()))
            .map(|alert| alert.severity)
            .collect();
        assert_eq!(
            severities,
            [Severity::Critical, Severity::Major, Severity::Warning]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_filter() {
        let all = SeverityFilter::ALL;
        assert!(
            Severity::ALERTS
                .iter()
                .all(|severity| all.contains(*severity))
        );
        let no_warnings = all.toggled(Severity::Warning);
        assert!(!no_warnings.contains(Severity::Warning));
        assert!(no_warnings.contains(Severity::Critical));
        assert_eq!(no_warnings.toggled(Severity::Warning), all);
        // None left: every one again.
        let critical = Severity::ALERTS[1..]
            .iter()
            .fold(all, |filter, severity| filter.toggled(*severity));
        assert_eq!(critical.toggled(Severity::Critical), all);
    }
}

/// The console over a real store in a headless window.
#[cfg(all(test, feature = "test-support"))]
mod console_tests {
    use std::sync::mpsc;

    use gpui_kit::{AnyWindowHandle, TestAppContext, WindowOptions, test::TestWindowExt as _};

    use super::super::{
        rows::{Rule, Subject},
        sim::Control,
    };
    use super::*;

    fn alert(host: u32, rate: f64) -> Arc<Alert> {
        Arc::new(Alert {
            key: AlertKey {
                rule: Rule::HostErrors,
                subject: Subject::Node(host),
            },
            severity: Severity::Major,
            state: AlertState::Firing,
            subject: format!("host-{host}").into(),
            service: "auth".into(),
            dc: "nth-1".into(),
            count: 1,
            rate,
            first_seen: "02:47:00".into(),
            last_seen: "02:47:00".into(),
        })
    }

    /// Lets the store publish, the grid rebuild past its throttle and the
    /// window paint.
    fn settle(cx: &mut TestAppContext, window: AnyWindowHandle) {
        for _ in 0..10 {
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .expect("window is open");
        }
    }

    #[gpui_kit::test]
    fn test_pause_holds_the_order_while_values_update(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
        });
        let store = cx.update(|cx| GridStore::new(store_options(), cx));
        let writer = store.writer();
        writer.replace_all([alert(1, 3.), alert(2, 1.)]);
        let (commands, _inbox) = mpsc::channel();
        let (window, console) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
                    cx.new(|cx| {
                        AlertConsole::new(store, commands, Control::new(), None, window, cx)
                    })
                })
            })
            .expect("open window");
        settle(cx, window);
        let grid = console.read_with(cx, |console, _| console.grid.clone());
        let shown = |cx: &mut TestAppContext| {
            grid.read_with(cx, |grid, _| {
                let snapshot = grid.snapshot();
                let hosts = grid.projection().rows().iter().filter_map(|id| {
                    snapshot.row(*id).map(|alert| match alert.key.subject {
                        Subject::Node(host) => (host, alert.rate),
                        Subject::Service(_) => (u32::MAX, alert.rate),
                    })
                });
                hosts.collect::<Vec<_>>()
            })
        };
        // The same severity: the faster firing first.
        assert_eq!(shown(cx), [(1, 3.), (2, 1.)]);

        // Paused: the rates change in place, the rows stay.
        console.update(cx, |console, cx| console.toggle_paused(cx));
        writer.upsert(alert(1, 0.5));
        writer.upsert(alert(2, 4.));
        settle(cx, window);
        assert_eq!(shown(cx), [(1, 0.5), (2, 4.)]);
        assert!(grid.read_with(cx, |grid, _| grid.is_order_held()));

        // Resumed: the held order shows.
        console.update(cx, |console, cx| console.toggle_paused(cx));
        settle(cx, window);
        assert_eq!(shown(cx), [(2, 4.), (1, 0.5)]);
        assert!(!console.read_with(cx, |console, _| console.is_paused()));
    }
}
