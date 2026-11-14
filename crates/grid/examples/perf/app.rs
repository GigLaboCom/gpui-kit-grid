//! The monitor itself: the app bar (arrangement, search, pause), the
//! process grid, the chart and the sources beside it, and the status bar,
//! over the sampler's thread. `main.rs` opens it in a window, `snapshot.rs`
//! offscreen; both call `configure` once, then [`Perf::new`].

use std::{sync::Arc, time::Duration};

use giglabo_products::ProductsButton;
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
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    KeyBinding, Keystroke, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_kit_grid::{
    Clock, ExpandDepth, GridStore, HoldReorder, LiveGrid, LiveGridState, ManualReorder,
};
use web_time::Instant;

mod chart;
pub mod drm;
pub mod gpu;
pub mod rows;
pub mod sampler;
#[cfg(feature = "test-support")]
pub mod snapshot;
pub mod table;
mod task;
mod theme;
mod ui;

use self::{
    chart::UsageChart,
    gpu::{GpuSamplerFactory, PlatformGpu},
    sampler::{Control, Sampler, Status},
    table::{Arrangement, Processes},
    theme::PerfTheme,
    ui::{GAP, PAGE_PADDING, ROW_HEIGHT},
};
use crate::trading::theme::TradingTheme as _;

gpui_kit::actions!(
    perf,
    [
        /// Freezes the order of the rows, or lets it follow the data again.
        PauseOrder,
        /// The processes as a tree by parent.
        ShowTree,
        /// The processes as one list.
        ShowFlat,
        /// The processes grouped by user.
        ShowByUser,
        /// The processes grouped by app.
        ShowByApp,
        /// Moves the focus to the search box.
        FocusSearch,
        /// Expands every branch of the tree.
        ExpandAll,
        /// Collapses the tree to its roots.
        CollapseAll,
    ]
);

/// The key context of the window content.
pub const KEY_CONTEXT: &str = "Perf";

/// The keys, as data: the bindings and the status bar's hints are both
/// made from this table; `secondary` is Cmd on macOS and Ctrl elsewhere.
const KEYS: [(&str, Command); 8] = [
    ("f9", Command::PauseOrder),
    ("secondary-1", Command::Arrange(Arrangement::Tree)),
    ("secondary-2", Command::Arrange(Arrangement::Flat)),
    ("secondary-3", Command::Arrange(Arrangement::User)),
    ("secondary-4", Command::Arrange(Arrangement::App)),
    ("secondary-f", Command::FocusSearch),
    ("secondary-e", Command::ExpandAll),
    ("shift-secondary-e", Command::CollapseAll),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    PauseOrder,
    Arrange(Arrangement),
    FocusSearch,
    ExpandAll,
    CollapseAll,
}

impl Command {
    fn binding(self, keystrokes: &str) -> KeyBinding {
        let context = Some(KEY_CONTEXT);
        match self {
            Self::PauseOrder => KeyBinding::new(keystrokes, PauseOrder, context),
            Self::Arrange(Arrangement::Tree) => KeyBinding::new(keystrokes, ShowTree, context),
            Self::Arrange(Arrangement::Flat) => KeyBinding::new(keystrokes, ShowFlat, context),
            Self::Arrange(Arrangement::User) => KeyBinding::new(keystrokes, ShowByUser, context),
            Self::Arrange(Arrangement::App) => KeyBinding::new(keystrokes, ShowByApp, context),
            Self::FocusSearch => KeyBinding::new(keystrokes, FocusSearch, context),
            Self::ExpandAll => KeyBinding::new(keystrokes, ExpandAll, context),
            Self::CollapseAll => KeyBinding::new(keystrokes, CollapseAll, context),
        }
    }

    /// The status bar's word for it; `None` keeps it out of the hints
    /// (the arrangement tabs name theirs).
    fn hint(self) -> Option<&'static str> {
        match self {
            Self::PauseOrder => Some("Pause order"),
            Self::FocusSearch => Some("Search"),
            Self::ExpandAll => Some("Expand"),
            Self::CollapseAll => Some("Collapse"),
            Self::Arrange(_) => None,
        }
    }
}

fn key_bindings() -> Vec<KeyBinding> {
    KEYS.iter()
        .map(|(keystrokes, command)| command.binding(keystrokes))
        .collect()
}

/// Where the process numbers come from, and what another user's process
/// does not show without root (the Sources panel).
#[cfg(target_os = "macos")]
const PROCESS_SOURCE: &str = "sysinfo; threads, footprint and identity from libproc and sysctl";
#[cfg(target_os = "macos")]
const WITHOUT_ROOT: &str = "other users' processes show no CPU, memory or threads";
#[cfg(target_os = "linux")]
const PROCESS_SOURCE: &str = "sysinfo over /proc; threads from status, pressure from PSI";
#[cfg(target_os = "linux")]
const WITHOUT_ROOT: &str = "other users' processes show no disk I/O or GPU";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PROCESS_SOURCE: &str = "sysinfo";
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const WITHOUT_ROOT: &str = "what the system keeps from other users stays blank";

const SIDE_WIDTH: Pixels = px(380.);
const CHART_HEIGHT: Pixels = px(240.);
const APP_BAR_HEIGHT: Pixels = px(44.);
/// How often the status bar reads the sampler's status.
const STATUS_REFRESH: Duration = Duration::from_millis(500);

/// What the monitor starts with (`native::settings_from_env`).
#[derive(Clone)]
pub struct Settings {
    interval: Duration,
    gpu: Arc<dyn GpuSamplerFactory>,
    quit_after: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interval: sampler::DEFAULT_INTERVAL,
            gpu: Arc::new(PlatformGpu),
            quit_after: None,
        }
    }
}

impl Settings {
    /// Samples every `interval`, within `MIN_INTERVAL..=MAX_INTERVAL`.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval.clamp(sampler::MIN_INTERVAL, sampler::MAX_INTERVAL);
        self
    }

    /// Prints the status every second and quits after `seconds`.
    pub fn with_quit_after(mut self, seconds: u64) -> Self {
        self.quit_after = Some(seconds);
        self
    }

    /// Reads the GPU with the samplers `factory` makes ([`PlatformGpu`]
    /// by default).
    pub fn with_gpu_sampler(mut self, factory: impl GpuSamplerFactory + 'static) -> Self {
        self.gpu = Arc::new(factory);
        self
    }
}

/// The monitor: the app bar, the grid, the side column and the status bar.
pub struct Perf {
    control: Arc<Control>,
    grid: Entity<LiveGridState<Processes>>,
    chart: Entity<UsageChart>,
    search: Entity<InputState>,
    arrangement: Arrangement,
    paused: bool,
    interval: Duration,
    status: Status,
    _refresh: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Perf {
    pub fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let control = Control::new();
        let store = GridStore::new(table::store_options(), cx);
        let grid = table::process_grid(store.clone(), cx);
        let chart = cx.new(|cx| UsageChart::new(control.clone(), cx));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search processes, users, apps"));
        let subscriptions = vec![
            cx.subscribe(&search, |perf, search, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = search.read(cx).value();
                    perf.grid
                        .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                }
            }),
        ];
        let clock: Arc<dyn Clock> = Arc::new(cx.background_executor().clone());
        start_sampler(&store, settings, clock, control.clone());
        Self {
            control,
            grid,
            chart,
            search,
            arrangement: Arrangement::default(),
            paused: false,
            interval: settings.interval,
            status: Status::default(),
            _refresh: Self::refresh(settings.quit_after, cx),
            _subscriptions: subscriptions,
        }
    }

    /// Every half second: the sampler's status, for the status bar.
    /// With `PERF_SECONDS=N` it also prints the status every second and
    /// quits after N seconds, for a windowed run nobody watches.
    fn refresh(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        // Profiling only: how long the run has lasted.
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let mut ticks = 0u64;
            loop {
                cx.background_executor().timer(STATUS_REFRESH).await;
                let Ok(done) = this.update(cx, |perf, cx| {
                    perf.status = perf.control.status();
                    ticks += 1;
                    if quit_after.is_some() && ticks % 2 == 0 {
                        out!(
                            "{:>5.1}s  {}",
                            started.elapsed().as_secs_f64(),
                            perf.status_line(cx)
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

    pub fn grid(&self) -> &Entity<LiveGridState<Processes>> {
        &self.grid
    }

    /// The status as one line, for `PERF_SECONDS` and the snapshot.
    pub fn status_line(&self, cx: &App) -> String {
        let grid = self.grid.read(cx);
        let stats = grid.stats();
        format!(
            "{} processes · {} shown · sample {} of every {} ms ({:.0} ms since the last) took {:.1} ms · GPU source: {}{} · last rebuild {:.1} ms · {}{}",
            self.status.processes,
            grid.projection().row_count(),
            self.status.samples,
            self.interval.as_millis(),
            self.status.interval.as_secs_f64() * 1e3,
            self.status.sample_time.as_secs_f64() * 1e3,
            self.status.gpu_source,
            self.status
                .gpu_error
                .as_deref()
                .map_or(String::new(), |error| format!(" ({error})")),
            stats.last_rebuild().as_secs_f64() * 1e3,
            self.arrangement.label(),
            if self.paused { " · order paused" } else { "" },
        )
    }

    /// Arranges the processes as `arrangement`, keeping the sort, and
    /// hides the column a grouping's captions name.
    pub fn set_arrangement(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        if self.arrangement == arrangement {
            return;
        }
        let previous = self.arrangement;
        self.arrangement = arrangement;
        self.grid.update(cx, |grid, cx| {
            let view = table::view(arrangement, grid.sort().to_vec());
            grid.set_view(view, cx);
            if let Some(column) = previous.group_column() {
                grid.set_column_visible(column, true, cx);
            }
            if let Some(column) = arrangement.group_column() {
                grid.set_column_visible(column, false, cx);
            }
            grid.set_expand_depth(ExpandDepth::All, cx);
        });
        cx.notify();
    }

    /// Pauses the order or resumes it: while paused values update in
    /// place, new processes wait in the held order, and a sort or an
    /// arrangement still applies (`ManualReorder`).
    pub fn toggle_paused(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        let paused = self.paused;
        self.grid.update(cx, |grid, cx| {
            if paused {
                grid.set_reorder_policy(ManualReorder, cx);
            } else {
                grid.set_reorder_policy(HoldReorder::new(table::HOLD), cx);
            }
        });
        cx.notify();
    }

    fn set_depth(&mut self, depth: ExpandDepth, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.set_expand_depth(depth, cx));
    }

    fn on_pause_order(&mut self, _: &PauseOrder, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_paused(cx);
    }

    fn on_show_tree(&mut self, _: &ShowTree, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(Arrangement::Tree, cx);
    }

    fn on_show_flat(&mut self, _: &ShowFlat, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(Arrangement::Flat, cx);
    }

    fn on_show_by_user(&mut self, _: &ShowByUser, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(Arrangement::User, cx);
    }

    fn on_show_by_app(&mut self, _: &ShowByApp, _: &mut Window, cx: &mut Context<Self>) {
        self.set_arrangement(Arrangement::App, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_expand_all(&mut self, _: &ExpandAll, _: &mut Window, cx: &mut Context<Self>) {
        self.set_depth(ExpandDepth::All, cx);
    }

    fn on_collapse_all(&mut self, _: &CollapseAll, _: &mut Window, cx: &mut Context<Self>) {
        self.set_depth(ExpandDepth::Levels(0), cx);
    }

    /// The mark, the arrangement tabs, the search box, the pause button and
    /// "More from GigLabo".
    fn render_app_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let selected = Arrangement::ALL
            .iter()
            .position(|arrangement| *arrangement == self.arrangement)
            .unwrap_or(0);
        h_flex()
            .flex_none()
            .w_full()
            .h(APP_BAR_HEIGHT)
            .px(PAGE_PADDING + px(4.))
            .gap(px(16.))
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
                            .child("perf"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("processes and GPU, live"),
                    )
                    .child(Tag::success().outline().small().child("LIVE")),
            )
            .child(
                TabBar::new("arrangement")
                    .segmented()
                    .small()
                    .selected_index(selected)
                    .children(
                        Arrangement::ALL.map(|arrangement| Tab::new().label(arrangement.label())),
                    )
                    .on_click(cx.listener(|perf, ix: &usize, _, cx| {
                        if let Some(arrangement) = Arrangement::ALL.get(*ix) {
                            perf.set_arrangement(*arrangement, cx);
                        }
                    })),
            )
            .child(div().flex_1())
            .child(
                Input::new(&self.search)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .small()
                    .aria_label("Search processes")
                    .w(px(280.)),
            )
            .child(
                Button::new("pause")
                    .label(if self.paused {
                        "Resume order"
                    } else {
                        "Pause order"
                    })
                    .small()
                    .map(|button| {
                        if self.paused {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .tooltip_with_action(
                        "Freeze the order of the rows; values keep updating",
                        &PauseOrder,
                        Some(KEY_CONTEXT),
                    )
                    .on_click(cx.listener(|perf, _, _, cx| perf.toggle_paused(cx))),
            )
            .child(ProductsButton::new())
    }

    fn render_processes(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let detail = format!(
            "{} · sorted live, held while the pointer rests on it",
            self.arrangement.label()
        );
        ui::panel(cx)
            .flex_1()
            .h_full()
            .child(
                ui::panel_header("PROCESSES", Some(detail.into()), cx)
                    .child(div().flex_1())
                    .when(self.arrangement == Arrangement::Tree, |header| {
                        header
                            .child(
                                Button::new("expand")
                                    .label("Expand all")
                                    .small()
                                    .ghost()
                                    .tooltip_with_action(
                                        "Expand every branch",
                                        &ExpandAll,
                                        Some(KEY_CONTEXT),
                                    )
                                    .on_click(cx.listener(|perf, _, _, cx| {
                                        perf.set_depth(ExpandDepth::All, cx)
                                    })),
                            )
                            .child(
                                Button::new("collapse")
                                    .label("Collapse")
                                    .small()
                                    .ghost()
                                    .tooltip_with_action(
                                        "Collapse to the roots",
                                        &CollapseAll,
                                        Some(KEY_CONTEXT),
                                    )
                                    .on_click(cx.listener(|perf, _, _, cx| {
                                        perf.set_depth(ExpandDepth::Levels(0), cx)
                                    })),
                            )
                    }),
            )
            .child(
                ui::grid_frame(cx).child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("Processes"),
                ),
            )
    }

    /// The chart and where the numbers come from.
    fn render_side(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let gpu = match &self.status.gpu_error {
            Some(error) => format!("{} — unavailable: {error}", self.status.gpu_source),
            None if self.status.gpu_source.starts_with("none") => {
                format!("{}: the GPU columns stay blank", self.status.gpu_source)
            }
            None => format!("{}: device and per-process % GPU", self.status.gpu_source),
        };
        let line = |label: &'static str, text: SharedString| {
            v_flex()
                .gap(px(2.))
                .child(div().text_color(theme.muted_foreground).child(label))
                .child(div().text_color(theme.foreground).child(text))
        };
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
                        "MACHINE",
                        Some(format!("CPU and GPU, last {} samples", sampler::HISTORY_LEN).into()),
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
                    .flex_1()
                    .child(ui::panel_header("SOURCES", None, cx))
                    .child(
                        v_flex()
                            .p(ui::PANEL_PADDING)
                            .gap(px(10.))
                            .child(line("CPU, memory, disk, users", PROCESS_SOURCE.into()))
                            .child(line("GPU", gpu.into()))
                            .child(line(
                                "Not read",
                                "command lines and environments: they are personal data".into(),
                            ))
                            .child(line("Without root", WITHOUT_ROOT.into())),
                    ),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let shown = self.grid.read(cx).projection().row_count();
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
            .left(SharedString::from(format!(
                "{} processes · {shown} rows shown",
                self.status.processes
            )))
            .left(SharedString::from(format!(
                "sample {} every {} ms (last {:.0} ms apart, {:.1} ms to read)",
                self.status.samples,
                self.interval.as_millis(),
                self.status.interval.as_secs_f64() * 1e3,
                self.status.sample_time.as_secs_f64() * 1e3
            )))
            .left(SharedString::from(format!(
                "GPU source: {}",
                self.status.gpu_source
            )))
            .when(self.paused, |bar| {
                bar.left(
                    div()
                        .text_color(theme.warning)
                        .font_weight(FontWeight::BOLD)
                        .child("order paused"),
                )
            })
            .right(
                h_flex()
                    .gap(px(14.))
                    .text_color(theme.muted_foreground)
                    .children(hints),
            )
    }
}

impl Render for Perf {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(Self::on_pause_order))
            .on_action(cx.listener(Self::on_show_tree))
            .on_action(cx.listener(Self::on_show_flat))
            .on_action(cx.listener(Self::on_show_by_user))
            .on_action(cx.listener(Self::on_show_by_app))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_expand_all))
            .on_action(cx.listener(Self::on_collapse_all))
            .bg(background)
            .text_color(foreground)
            .font_family(family)
            .text_size(text_size)
            .child(self.render_app_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_start()
                    .p(PAGE_PADDING)
                    .gap(GAP)
                    .child(self.render_processes(cx))
                    .child(self.render_side(cx)),
            )
            .child(self.render_status(cx))
    }
}

/// Starts the sampler on a thread of its own, writing `store` every
/// interval until the store is gone.
fn start_sampler(
    store: &GridStore<rows::ProcRow>,
    settings: &Settings,
    clock: Arc<dyn Clock>,
    control: Arc<Control>,
) {
    let writer = store.writer();
    let (factory, interval) = (settings.gpu.clone(), settings.interval);
    let spawned = std::thread::Builder::new()
        .name("perf-sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::new(factory.create(), clock);
            while !writer.is_closed() {
                // Profiling only: how long the sample took, to sleep the rest.
                let started = Instant::now();
                sampler.step(&writer, &control);
                std::thread::sleep(interval.saturating_sub(started.elapsed()));
            }
        });
    if let Err(error) = spawned {
        err!("perf: the sampler's thread: {error}");
    }
}

/// After `gpui_kit::init` and `gpui_kit_grid::init`: the theme with its
/// fonts, and the keys.
pub fn configure(cx: &mut App) {
    crate::trading::theme::install::<PerfTheme>(cx);
    PerfTheme::Graphite.apply(cx);
    cx.bind_keys(key_bindings());
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
        for arrangement in Arrangement::ALL {
            assert!(
                KEYS.iter()
                    .any(|(_, command)| *command == Command::Arrange(arrangement))
            );
        }
    }

    #[test]
    fn test_settings_clamp_the_interval() {
        let fast = Settings::default().with_interval(Duration::from_millis(10));
        assert_eq!(fast.interval, sampler::MIN_INTERVAL);
        let slow = Settings::default().with_interval(Duration::from_secs(60));
        assert_eq!(slow.interval, sampler::MAX_INTERVAL);
    }
}
