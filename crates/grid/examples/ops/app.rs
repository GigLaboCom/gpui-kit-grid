//! The ops example itself: the app bar, the alert console, the chart, the
//! service health, the fleet and the status bar, over the simulator. The
//! native `main` (`main.rs`) opens it in a window; the browser build
//! (`crates/grid-web/apps/ops`, the page `ops/`) in the page's canvas. Both
//! call `setup` once, then `open` with their [`Settings`].
//!
//! The simulator runs natively on a thread of its own and in the browser,
//! where GPUI's web platform has one thread, a step at a time on the
//! executor's timer (`start_simulator`).

use std::{
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

use giglabo_products::ProductsButton;
use gpui_kit::base::{actions::Confirm, h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    kbd::Kbd,
    status_bar::StatusBar,
    tag::Tag,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, FontWeight, InteractiveElement as _,
    IntoElement, KeyBinding, Keystroke, ParentElement as _, Pixels, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{GridStore, LiveGrid, LiveGridEvent, LiveGridState};
// `std::time` natively; the browser's clock in wasm, where
// `std::time::Instant::now` panics.
use web_time::Instant;

pub mod alerts;
mod chart;
mod fleet;
#[cfg(not(target_family = "wasm"))]
pub mod native;
pub mod rows;
mod services;
pub mod sim;
#[cfg(feature = "test-support")]
pub mod snapshot;
mod theme;
mod ui;

pub use self::theme::OpsTheme;
use self::{
    alerts::{AlertConsole, LayoutStore},
    chart::AlertRateChart,
    fleet::Fleet,
    rows::{FleetColumn, FleetNode, Severity},
    services::Services,
    sim::{Control, Feeds, FleetShape, Phase, Pump, RateFeed, Sim},
    theme::status_color,
    ui::{GAP, PAGE_PADDING, ROW_HEIGHT},
};
use crate::trading::theme::TradingTheme as _;

gpui_kit::actions!(
    ops,
    [
        /// Starts a storm, or restores the failed feed.
        ToggleStorm,
        /// Shows or hides critical alerts.
        ToggleCritical,
        /// Shows or hides major alerts.
        ToggleMajor,
        /// Shows or hides minor alerts.
        ToggleMinor,
        /// Shows or hides warnings.
        ToggleWarning,
        /// Moves the focus to the alert search box.
        FocusSearch,
        /// Groups the alerts by service or by data center.
        SwitchGrouping,
        /// Expands the whole fleet.
        ExpandFleet,
        /// Collapses the fleet to its data centers.
        CollapseFleet,
        /// Freezes the order of the alerts, or lets it follow the data again.
        PauseOrder,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Ops";
/// Where `A` acknowledges: the grid inside the alert console.
const ACK_CONTEXT: &str = "OpsAlerts > LiveGrid";

/// The keys, as data: the bindings and the status bar's hints are both
/// made from this table. `A` acknowledges inside the alert grid only, so
/// it never reaches the search box.
const KEYS: [(&str, Command); 11] = [
    ("f8", Command::ToggleStorm),
    ("f9", Command::PauseOrder),
    ("a", Command::Acknowledge),
    ("secondary-1", Command::Severity(Severity::Critical)),
    ("secondary-2", Command::Severity(Severity::Major)),
    ("secondary-3", Command::Severity(Severity::Minor)),
    ("secondary-4", Command::Severity(Severity::Warning)),
    ("secondary-f", Command::FocusSearch),
    ("secondary-g", Command::SwitchGrouping),
    ("secondary-e", Command::ExpandFleet),
    ("shift-secondary-e", Command::CollapseFleet),
];

/// What a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    ToggleStorm,
    PauseOrder,
    Acknowledge,
    Severity(Severity),
    FocusSearch,
    SwitchGrouping,
    ExpandFleet,
    CollapseFleet,
}

impl Command {
    /// The binding of `keystrokes` to this command's action: `A` in the
    /// alert grid (the grid's own `Confirm`, the acknowledge command), the
    /// rest anywhere in the window.
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let context = Some(KEY_CONTEXT);
        match self {
            Self::ToggleStorm => KeyBinding::new(keystrokes, ToggleStorm, context),
            Self::PauseOrder => KeyBinding::new(keystrokes, PauseOrder, context),
            Self::Acknowledge => {
                KeyBinding::new(keystrokes, Confirm { secondary: false }, Some(ACK_CONTEXT))
            }
            Self::Severity(Severity::Critical) => {
                KeyBinding::new(keystrokes, ToggleCritical, context)
            }
            Self::Severity(Severity::Major) => KeyBinding::new(keystrokes, ToggleMajor, context),
            Self::Severity(Severity::Minor) => KeyBinding::new(keystrokes, ToggleMinor, context),
            Self::Severity(_) => KeyBinding::new(keystrokes, ToggleWarning, context),
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, context),
            Self::SwitchGrouping => KeyBinding::new(keystrokes, SwitchGrouping, context),
            Self::ExpandFleet => KeyBinding::new(keystrokes, ExpandFleet, context),
            Self::CollapseFleet => KeyBinding::new(keystrokes, CollapseFleet, context),
        }
    }

    /// The status bar's word for it; `None` keeps it out of the hints (the
    /// chips' tooltips name the severity keys).
    fn hint(self) -> Option<&'static str> {
        match self {
            Self::ToggleStorm => Some("Storm"),
            Self::PauseOrder => Some("Pause order"),
            Self::Acknowledge => Some("Acknowledge"),
            Self::FocusSearch => Some("Search"),
            Self::SwitchGrouping => Some("Grouping"),
            Self::ExpandFleet => Some("Expand fleet"),
            Self::CollapseFleet => Some("Collapse"),
            Self::Severity(_) => None,
        }
    }
}

/// Every binding of `KEYS`.
fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// The right-hand column beside the alert console.
const SIDE_WIDTH: Pixels = px(720.);
const CHART_HEIGHT: Pixels = px(196.);
/// The header, the grid's header, three pinned services and six more.
const SERVICES_HEIGHT: Pixels = px(302.);
const APP_BAR_HEIGHT: Pixels = px(44.);

/// How long a storm lasts before the feed comes back on its own, unless
/// the settings say otherwise.
const STORM_SECONDS: u64 = 45;

/// What the console starts with: natively from the environment
/// (`native::settings_from_env`), in the browser from the page's address.
pub struct Settings {
    hosts: u32,
    seed: Option<u64>,
    storm_seconds: u64,
    /// Where the alert console's layout is kept; none in the browser.
    layout: Option<Rc<dyn LayoutStore>>,
    quit_after: Option<u64>,
}

/// The browser build's side (`crates/grid-web`): the page's settings.
#[cfg(target_family = "wasm")]
impl Settings {
    /// The page's `hosts`, `seed` and `storm_seconds`, the defaults
    /// otherwise; no layout is kept.
    pub fn new(hosts: Option<u32>, seed: Option<u64>, storm_seconds: Option<u64>) -> Self {
        Self {
            hosts: hosts.unwrap_or(sim::DEFAULT_HOSTS),
            seed,
            storm_seconds: storm_seconds.unwrap_or(STORM_SECONDS),
            layout: None,
            quit_after: None,
        }
    }
}

/// The status line's counters, refreshed every half second.
#[derive(Default)]
struct Status {
    /// Alert updates a second over the last interval.
    rate: u64,
    rebuilds: u64,
    last_rebuild: Duration,
    frame: Duration,
    phase: Phase,
}

/// The console: the app bar, the alert console, the side column and the
/// status bar.
pub struct Ops {
    control: Arc<Control>,
    commands: mpsc::Sender<sim::Command>,
    alerts: Entity<AlertConsole>,
    fleet: Entity<LiveGridState<Fleet>>,
    services: Entity<LiveGridState<Services>>,
    chart: Entity<AlertRateChart>,
    hosts: u32,
    status: Status,
    /// The status's refresh, and the simulator in the browser.
    _tasks: [Task<()>; 2],
    _subscriptions: Vec<Subscription>,
}

impl Ops {
    fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let control = Control::new();
        let (commands, inbox) = mpsc::channel();
        let alert_store = GridStore::new(alerts::store_options(), cx);
        let fleet_store = GridStore::new(fleet::store_options(), cx);
        let service_store = GridStore::new(services::store_options(), cx);
        let rates = RateFeed::default();

        let alerts = cx.new(|cx| {
            AlertConsole::new(
                alert_store.clone(),
                commands.clone(),
                control.clone(),
                settings.layout.clone(),
                window,
                cx,
            )
        });
        let fleet = fleet::fleet_grid(fleet_store.clone(), cx);
        let services = services::service_grid(service_store.clone(), cx);
        let chart = cx.new(|cx| AlertRateChart::new(rates.clone(), cx));
        let subscriptions = vec![
            cx.observe(&alerts, |_, _, cx| cx.notify()),
            cx.subscribe_in(&fleet, window, Self::on_fleet_event),
        ];

        // The simulator; it stops once the alert store is gone.
        let shape = FleetShape::for_hosts(settings.hosts);
        let sim = Sim::new(shape, settings.seed, settings.storm_seconds);
        let feeds = Feeds {
            alerts: alert_store.writer(),
            fleet: fleet_store.writer(),
            services: service_store.writer(),
            rates,
        };
        let simulator = start_simulator(sim, feeds, control.clone(), inbox, cx);

        Self {
            control,
            commands,
            alerts,
            fleet,
            services,
            chart,
            hosts: shape.host_count(),
            status: Status::default(),
            _tasks: [Self::refresh(settings.quit_after, cx), simulator],
            _subscriptions: subscriptions,
        }
    }

    /// Every half second: the measured rate, the grids' counters and the
    /// storm, which the fleet follows to its hall. With `OPS_SECONDS=N`
    /// prints the status every second and quits after N seconds.
    fn refresh(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (mut last_updates, mut last_rebuilds, mut ticks) = (0, 0, 0u64);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let Ok(done) = this.update(cx, |ops, cx| {
                    let updates = ops.control.updates();
                    let stats = ops.alerts.read(cx).grid().read(cx).stats();
                    let phase = ops.control.phase();
                    if phase == Phase::Incident
                        && ops.status.phase != Phase::Incident
                        && let Some(hall) = ops.control.storm_hall()
                    {
                        fleet::reveal(&ops.fleet, hall, cx);
                    }
                    ops.status = Status {
                        rate: updates.saturating_sub(last_updates) * 2,
                        rebuilds: stats.rebuilds().saturating_sub(last_rebuilds) * 2,
                        last_rebuild: stats.last_rebuild(),
                        frame: stats.last_prepaint(),
                        phase,
                    };
                    last_updates = updates;
                    last_rebuilds = stats.rebuilds();
                    ticks += 1;
                    if quit_after.is_some() && ticks % 2 == 0 {
                        out!(
                            "{:>5.1}s  {}",
                            started.elapsed().as_secs_f64(),
                            ops.status_line(cx)
                        );
                    }
                    cx.notify();
                    quit_after.is_some_and(|seconds| started.elapsed().as_secs() >= seconds)
                }) else {
                    break;
                };
                if done {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
    }

    /// The status as one line, for `OPS_SECONDS` and the snapshot.
    pub fn status_line(&self, cx: &App) -> String {
        let alerts = self.alerts.read(cx);
        let alert_rows = alerts.grid().read(cx).snapshot().len();
        let fleet_rows = self.fleet.read(cx).snapshot().len();
        let paused = if alerts.is_paused() {
            " · order paused"
        } else {
            ""
        };
        format!(
            "SIMULATED · {} alert updates/s · {} alert keys · {} alert rows, {} fleet rows · {} rebuilds/s, last {:.1} ms · frame {:.2} ms · {} · {}{paused}",
            ui::thousands(self.status.rate),
            ui::thousands(self.control.keys()),
            ui::thousands(alert_rows as u64),
            ui::thousands(fleet_rows as u64),
            self.status.rebuilds,
            self.status.last_rebuild.as_secs_f64() * 1e3,
            self.status.frame.as_secs_f64() * 1e3,
            hosts_down(self.control.hosts_down()),
            self.status.phase.label(),
        )
    }

    /// Starts a storm, or restores the failed feed.
    pub fn toggle_storm(&mut self, cx: &mut Context<Self>) {
        let _ = self.commands.send(sim::Command::ToggleStorm);
        cx.notify();
    }

    /// The fleet's primary command: the console searches for the node.
    fn on_fleet_event(
        &mut self,
        fleet: &Entity<LiveGridState<Fleet>>,
        event: &LiveGridEvent<u32, FleetColumn>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let LiveGridEvent::Confirm { keys, .. } = event else {
            return;
        };
        let Some(name) = keys
            .first()
            .and_then(|key| fleet.read(cx).snapshot().get(key))
            .map(|node: &Arc<FleetNode>| node.name.clone())
        else {
            return;
        };
        self.alerts.update(cx, |console, cx| {
            console.type_search(&name, window, cx);
        });
    }

    fn on_toggle_storm(&mut self, _: &ToggleStorm, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_storm(cx);
    }

    /// Pauses the alert order, or resumes it.
    pub fn toggle_paused(&mut self, cx: &mut Context<Self>) {
        self.alerts
            .update(cx, |console, cx| console.toggle_paused(cx));
    }

    fn on_pause_order(&mut self, _: &PauseOrder, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_paused(cx);
    }

    fn toggle_severity(&mut self, severity: Severity, cx: &mut Context<Self>) {
        self.alerts
            .update(cx, |console, cx| console.toggle_severity(severity, cx));
    }

    fn on_toggle_critical(&mut self, _: &ToggleCritical, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_severity(Severity::Critical, cx);
    }

    fn on_toggle_major(&mut self, _: &ToggleMajor, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_severity(Severity::Major, cx);
    }

    fn on_toggle_minor(&mut self, _: &ToggleMinor, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_severity(Severity::Minor, cx);
    }

    fn on_toggle_warning(&mut self, _: &ToggleWarning, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_severity(Severity::Warning, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.alerts
            .update(cx, |console, cx| console.focus_search(window, cx));
    }

    fn on_switch_grouping(&mut self, _: &SwitchGrouping, _: &mut Window, cx: &mut Context<Self>) {
        self.alerts.update(cx, |console, cx| {
            let grouping = console.grouping().toggled();
            console.set_grouping(grouping, cx)
        });
    }

    fn set_fleet_depth(&mut self, depth: gpui_kit_grid::ExpandDepth, cx: &mut Context<Self>) {
        self.fleet
            .update(cx, |grid, cx| grid.set_expand_depth(depth, cx));
    }

    fn on_expand_fleet(&mut self, _: &ExpandFleet, _: &mut Window, cx: &mut Context<Self>) {
        self.set_fleet_depth(gpui_kit_grid::ExpandDepth::All, cx);
    }

    fn on_collapse_fleet(&mut self, _: &CollapseFleet, _: &mut Window, cx: &mut Context<Self>) {
        self.set_fleet_depth(fleet::COLLAPSED_DEPTH, cx);
    }

    /// The mark, the SIMULATED badge, the incident banner, the storm
    /// button, the simulated clock and "More from GigLabo"; a `narrow`
    /// window keeps the mark, the banner and the storm button.
    fn render_app_bar(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let phase = self.status.phase;
        let storm_label = match phase {
            Phase::Incident => "Restore feed",
            Phase::Quiet | Phase::Recovering => "Start storm",
        };
        h_flex()
            .flex_none()
            .w_full()
            .h(APP_BAR_HEIGHT)
            .px(PAGE_PADDING + px(4.))
            .gap(if narrow { px(10.) } else { px(18.) })
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .child(div().size(px(10.)).bg(theme.primary))
                    .child(
                        div()
                            .text_size(px(19.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child("ops"),
                    )
                    .when(!narrow, |mark| {
                        mark.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child("operations console"),
                        )
                        .child(Tag::secondary().outline().small().child(ui::SIMULATED))
                    }),
            )
            .child(self.render_banner(narrow, cx))
            .child(div().flex_1())
            .child(
                Button::new("storm")
                    .label(storm_label)
                    .small()
                    .when(phase == Phase::Incident, |button| button.outline())
                    .when(phase != Phase::Incident, |button| button.primary())
                    .tooltip_with_action(
                        "Start a storm, or restore the failed feed",
                        &ToggleStorm,
                        Some(KEY_CONTEXT),
                    )
                    .on_click(cx.listener(|ops, _, _, cx| ops.toggle_storm(cx))),
            )
            .when(!narrow, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .font_features(ui::tabular_figures())
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(format!("{} sim", self.control.clock()))),
                )
                .child(ProductsButton::new())
            })
    }

    /// The storm's state: all quiet, the incident with its hall and its
    /// time, or the recovery; in a `narrow` window the state alone.
    fn render_banner(&self, narrow: bool, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let hall = self
            .control
            .storm_hall()
            .and_then(|id| self.fleet.read(cx).snapshot().get(&id).cloned());
        let elapsed = self.control.storm_elapsed().as_secs();
        let clock = format!("{:02}:{:02}", elapsed / 60, elapsed % 60);
        let (tag, text) = match (self.status.phase, hall) {
            (Phase::Incident, Some(hall)) => (
                Tag::danger().child(format!("{} INCIDENT", Severity::Critical.icon())),
                format!(
                    "{} · power feed B down, cooling on one feed · {} · {clock}",
                    hall.name,
                    hosts_down(self.control.hosts_down())
                ),
            ),
            (Phase::Recovering, Some(hall)) => (
                Tag::warning().child("RECOVERING"),
                format!(
                    "{} · feed B restored, the hall cools down · {clock}",
                    hall.name
                ),
            ),
            _ => (
                Tag::success()
                    .outline()
                    .child(format!("{} ALL QUIET", Severity::Ok.icon())),
                "baseline alerts only".into(),
            ),
        };
        h_flex()
            .min_w_0()
            .gap(px(10.))
            .child(tag.small())
            .when(!narrow, |banner| {
                banner.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_features(ui::tabular_figures())
                        .text_color(theme.foreground)
                        .child(text),
                )
            })
    }

    fn render_side(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let peak = format!("last {} s", sim::HISTORY_SECONDS);
        let fleet_detail = format!(
            "{} hosts · {} · roll-ups per branch",
            ui::thousands(u64::from(self.hosts)),
            hosts_down(self.control.hosts_down())
        );
        v_flex()
            .flex_none()
            .w(SIDE_WIDTH)
            .h_full()
            .gap(GAP)
            .child(
                ui::panel(cx)
                    .flex_none()
                    .h(CHART_HEIGHT)
                    .child(ui::panel_header(
                        "ALERT UPDATES / S",
                        Some(format!("by severity · {peak}").into()),
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px(ui::PANEL_PADDING)
                            .py(px(8.))
                            .child(self.chart.clone()),
                    ),
            )
            .child(
                ui::panel(cx)
                    .flex_none()
                    .h(SERVICES_HEIGHT)
                    .child(ui::panel_header(
                        "SERVICE HEALTH",
                        Some(
                            format!("lowest {} error budgets pinned", sim::PINNED_SERVICES).into(),
                        ),
                        cx,
                    ))
                    .child(
                        ui::grid_frame(cx).child(
                            LiveGrid::new(&self.services)
                                .with_size(Size::Size(ROW_HEIGHT))
                                .bordered(false)
                                .accessibility_label("Service health"),
                        ),
                    ),
            )
            .child(
                ui::panel(cx)
                    .flex_1()
                    .child(
                        ui::panel_header("FLEET", Some(fleet_detail.into()), cx)
                            .child(div().flex_1())
                            .child(
                                Button::new("expand-fleet")
                                    .label("Expand all")
                                    .small()
                                    .ghost()
                                    .tooltip_with_action(
                                        "Expand every host",
                                        &ExpandFleet,
                                        Some(KEY_CONTEXT),
                                    )
                                    .on_click(cx.listener(|ops, _, _, cx| {
                                        ops.set_fleet_depth(gpui_kit_grid::ExpandDepth::All, cx)
                                    })),
                            )
                            .child(
                                Button::new("collapse-fleet")
                                    .label("Collapse")
                                    .small()
                                    .ghost()
                                    .tooltip_with_action(
                                        "Collapse to the data centers",
                                        &CollapseFleet,
                                        Some(KEY_CONTEXT),
                                    )
                                    .on_click(cx.listener(|ops, _, _, cx| {
                                        ops.set_fleet_depth(fleet::COLLAPSED_DEPTH, cx)
                                    })),
                            ),
                    )
                    .child(
                        ui::grid_frame(cx).child(
                            LiveGrid::new(&self.fleet)
                                .with_size(Size::Size(ROW_HEIGHT))
                                .bordered(false)
                                .accessibility_label("Fleet"),
                        ),
                    ),
            )
            .text_color(theme.foreground)
    }

    /// The counters, the storm and the keys; a `narrow` window keeps the
    /// rate and the storm.
    fn render_status(&self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let alert_rows = self.alerts.read(cx).grid().read(cx).snapshot().len();
        let fleet_rows = self.fleet.read(cx).snapshot().len();
        let phase = self.status.phase;
        let phase_color = match phase {
            Phase::Quiet => status_color(Severity::Ok, theme),
            Phase::Incident => status_color(Severity::Critical, theme),
            Phase::Recovering => status_color(Severity::Minor, theme),
        };
        let hints = KEYS.iter().filter_map(|(keystrokes, command)| {
            let hint = command.hint()?;
            let keystroke = Keystroke::parse(keystrokes).ok()?;
            Some(
                h_flex()
                    .gap(px(5.))
                    .child(Kbd::new(keystroke).outline())
                    .child(hint),
            )
        });
        StatusBar::new()
            .font_features(ui::tabular_figures())
            .left(Tag::secondary().small().child(ui::SIMULATED))
            .left(SharedString::from(format!(
                "{} alert updates/s",
                ui::thousands(self.status.rate)
            )))
            .when(!narrow, |bar| {
                bar.left(SharedString::from(format!(
                    "{} alert keys · {} rows · fleet {} rows",
                    ui::thousands(self.control.keys()),
                    ui::thousands(alert_rows as u64),
                    ui::thousands(fleet_rows as u64),
                )))
                .left(SharedString::from(format!(
                    "{} rebuilds/s · last {:.1} ms · frame {:.2} ms",
                    self.status.rebuilds,
                    self.status.last_rebuild.as_secs_f64() * 1e3,
                    self.status.frame.as_secs_f64() * 1e3,
                )))
            })
            .left(
                div()
                    .text_color(phase_color)
                    .font_weight(FontWeight::BOLD)
                    .child(format!("storm: {}", phase.label())),
            )
            .when(!narrow, |bar| {
                bar.right(
                    h_flex()
                        .gap(px(14.))
                        .text_color(theme.muted_foreground)
                        .children(hints),
                )
            })
    }
}

/// "1 host down", "12 hosts down".
fn hosts_down(count: u32) -> String {
    match count {
        1 => "1 host down".into(),
        count => format!("{count} hosts down"),
    }
}

impl Render for Ops {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = window.viewport_size().width < ui::NARROW;
        let theme = cx.theme();
        let (background, foreground, family, text_size) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
            theme.font_size,
        );
        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_toggle_storm))
            .on_action(cx.listener(Self::on_pause_order))
            .on_action(cx.listener(Self::on_toggle_critical))
            .on_action(cx.listener(Self::on_toggle_major))
            .on_action(cx.listener(Self::on_toggle_minor))
            .on_action(cx.listener(Self::on_toggle_warning))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_switch_grouping))
            .on_action(cx.listener(Self::on_expand_fleet))
            .on_action(cx.listener(Self::on_collapse_fleet))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(narrow, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .p(PAGE_PADDING)
                    .gap(GAP)
                    .child(div().flex_1().min_w_0().h_full().child(self.alerts.clone()))
                    .when(!narrow, |main| main.child(self.render_side(cx))),
            )
            .child(self.render_status(narrow, cx))
    }
}

/// Starts `sim` over `feeds`, taking the console's commands from
/// `commands`, until the alert store is gone. Returns the task to keep
/// while it runs: natively it is done at once, and the simulator runs on a
/// thread of its own; in the browser, where GPUI's web platform has one
/// thread, the task runs it a step at a time on the executor's timer,
/// between frames.
fn start_simulator(
    sim: Sim,
    feeds: Feeds,
    control: Arc<Control>,
    commands: mpsc::Receiver<sim::Command>,
    cx: &App,
) -> Task<()> {
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = cx;
        std::thread::spawn(move || {
            let mut pump = Pump::start(sim, feeds, control, commands);
            while !pump.is_closed() {
                std::thread::sleep(pump.step());
            }
        });
        Task::ready(())
    }
    #[cfg(target_family = "wasm")]
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        // After the first frame: the warm-up and the first load take a
        // while on the page's only thread.
        executor.timer(Duration::from_millis(1)).await;
        let mut pump = Pump::start(sim, feeds, control, commands);
        while !pump.is_closed() {
            // Never a zero timer: the loop must yield to the page.
            let wait = pump.step().max(Duration::from_millis(1));
            executor.timer(wait).await;
        }
    })
}

/// gpui-kit, the grid's key bindings (arrows, Page, Home/End, Enter,
/// Escape, `+`/`-`), then `configure`. Once, before `open`.
pub fn setup(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit_grid::init(cx);
    configure(cx);
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// fonts, and the console's keys.
fn configure(cx: &mut App) {
    crate::trading::theme::install::<OpsTheme>(cx);
    OpsTheme::Graphite.apply(cx);
    cx.bind_keys(key_bindings());
}

/// The console in `window`: the simulator started, the keyboard in the
/// alerts.
pub fn open(settings: &Settings, window: &mut Window, cx: &mut App) -> Entity<Ops> {
    let ops = cx.new(|cx| Ops::new(settings, window, cx));
    let focus = ops.read(cx).alerts.focus_handle(cx);
    window.focus(&focus, cx);
    ops
}

/// What the browser page reads and drives the console with.
#[cfg(target_family = "wasm")]
impl Ops {
    /// The simulator's counters.
    pub fn control(&self) -> &Arc<Control> {
        &self.control
    }

    /// The alert console's grid, the page's main grid.
    pub fn alert_grid<'a>(&self, cx: &'a App) -> &'a Entity<LiveGridState<alerts::Alerts>> {
        self.alerts.read(cx).grid()
    }

    /// The fleet's grid.
    pub fn fleet_grid(&self) -> &Entity<LiveGridState<Fleet>> {
        &self.fleet
    }

    /// The alert updates a second over the last half second.
    pub fn rate(&self) -> u64 {
        self.status.rate
    }

    /// The console's own state, for the page's input checks: the
    /// grouping, the severities shown, the search, the order paused and
    /// the storm.
    pub fn page_state(&self, cx: &App) -> serde_json::Value {
        let console = self.alerts.read(cx);
        serde_json::json!({
            "grouping": console.grouping().label(),
            "paused": console.is_paused(),
            "search": console.grid().read(cx).quick_filter().to_string(),
            "shown": console.grid().read(cx).projection().row_count(),
            "phase": self.control.phase().label(),
            "hosts_down": self.control.hosts_down(),
            "keys": self.control.keys(),
            "fleet_rows": self.fleet.read(cx).snapshot().len(),
        })
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
        for severity in Severity::ALERTS {
            assert!(
                KEYS.iter()
                    .any(|(_, command)| *command == Command::Severity(severity))
            );
        }
    }
}
