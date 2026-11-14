//! A live portfolio as a tree: funds, desks, books and positions in one
//! Name column, for manual checks and an unattended run.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example portfolio
//! ROWS=100000 RATE=10000 cargo run --release -p gpui-kit-grid --example portfolio
//! PORTFOLIO_SELECT=branches PORTFOLIO_MOVES=2 cargo run --release -p gpui-kit-grid --example portfolio
//! PORTFOLIO_SECONDS=10 cargo run --release -p gpui-kit-grid --example portfolio
//! ```
//!
//! A producer thread writes the positions and the sums of the books, desks
//! and funds above them (`model.rs`). The tree is sorted by name; the funds
//! and desks start expanded, the books collapsed. The chevron or `+`/`-`
//! expands and collapses a row, Option/Alt on the chevron its whole
//! subtree; `-` on a position goes to its book. The toolbar expands
//! everything, collapses everything, opens every book (Books), sets the row
//! size and the theme; the filter field (Cmd/Ctrl+F) is the grid's quick
//! filter, which keeps the ancestors of matching positions for context. The
//! status bar shows the rows and the tree rows above the positions, the
//! lines shown, the default expansion, the rebuilds a second and the last
//! rebuild, the rows of parent cycles, the totals over the positions and
//! the last opened rows (Enter, a double click or the menu's primary
//! command). `PORTFOLIO_SECONDS` prints the same line every half second and
//! quits, for an unattended run, as `BLOTTER_SECONDS` does in `blotter`.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `ROWS` | 5000 | Positions at the first load, spread over 64 books |
//! | `RATE` | 1000 | Price updates per second |
//! | `CHURN` | 5 | Positions opened and closed per second |
//! | `PORTFOLIO_MOVES` | 0.2 | Positions moved to another book per second |
//! | `PORTFOLIO_SELECT` | `rows` | `branches`: rows with children take the focus, not the selection (`FocusBranches`) |
//! | `PORTFOLIO_SECONDS` | none | Print the status every half second and quit after N seconds |
//! | `PORTFOLIO_E2E_LOG` | none | The end-to-end log of `script/e2e` (`e2e.rs`) |

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

mod model;

use std::{
    io::Write as _,
    time::{Duration, Instant},
};

use giglabo_products::ProductsButton;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, Size, Theme, ThemeMode,
    button::{Button, Toggle},
    input::{Input, InputEvent, InputState},
    select::{Select, SelectEvent, SelectState},
    separator::Separator,
    status_bar::StatusBar,
    toolbar::{Toolbar, ToolbarGroup},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder as _, px, size,
};
use gpui_kit_grid::{
    Aggregate, CellStyle, CellStyleContext, ExpandDepth, FocusBranches, GridColumns as _,
    GridMenuContext, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, OwnedValue,
    PinnedEdge, SelectionMode, SortDirection, StoreOptions, TreeSpec, ViewSpec,
};

use crate::{
    e2e::RecordBounds as _,
    model::{BRANCHES, Feed, Level, Node, NodeColumn},
};

// A closed stdout or stderr, for example a parent that exited, must not abort the example.
macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

macro_rules! err {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

// After the macros, which it uses.
mod e2e;

gpui_kit::actions!(
    portfolio,
    [
        /// Moves the focus to the filter field (Cmd/Ctrl+F).
        FocusFilter,
    ]
);

/// The key context of the window content.
const KEY_CONTEXT: &str = "Portfolio";

/// The row sizes of `LiveGrid` (26, 30, 32 and 40 px rows).
const SIZES: [(&str, Size); 4] = [
    ("XSmall", Size::XSmall),
    ("Small", Size::Small),
    ("Medium", Size::Medium),
    ("Large", Size::Large),
];

/// The grid's hooks. They take `&self` and never read the grid.
struct Portfolio;

impl LiveGridDelegate for Portfolio {
    type Row = Node;

    /// The sums of funds and desks in semibold; a loss in the danger color.
    fn cell_style(&self, cell: &CellStyleContext<'_, Node>, style: &mut CellStyle, cx: &App) {
        let row = cell.row();
        if matches!(row.level(), Level::Fund | Level::Desk) {
            style.set_font_weight(FontWeight::SEMIBOLD);
        }
        if cell.column() == NodeColumn::Pnl && row.pnl() < 0. {
            style.set_foreground(cx.theme().danger);
        }
    }

    /// "Open row", or "Open 12 rows" (the command scope: the selected rows,
    /// or with `FocusBranches` a book's positions): Enter and a double click
    /// run it too (`LiveGridEvent::Confirm`).
    fn primary_command(&self, menu: &GridMenuContext<'_, Node>, _: &App) -> Option<SharedString> {
        Some(rows_label("Open", menu.selection().len()).into())
    }
}

/// "Open row", "Open 3 rows".
fn rows_label(verb: &str, count: usize) -> String {
    match count {
        1 => format!("{verb} row"),
        count => format!("{verb} {count} rows"),
    }
}

/// The default expansion for the status bar: `all`, `levels 2`.
fn depth_label(depth: ExpandDepth) -> String {
    match depth {
        ExpandDepth::All => "all".into(),
        ExpandDepth::Levels(levels) => format!("levels {levels}"),
        depth => format!("{depth:?}").to_lowercase(),
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// What the environment sets.
struct Settings {
    feed: Feed,
    focus_branches: bool,
    quit_after: Option<u64>,
}

impl Settings {
    fn from_env() -> Self {
        let moves = std::env::var("PORTFOLIO_MOVES").ok().map_or(0.2, |value| {
            value.parse().unwrap_or_else(|_| {
                err!("PORTFOLIO_MOVES={value}: not a number; 0.2 is used");
                0.2
            })
        });
        let focus_branches = match std::env::var("PORTFOLIO_SELECT").as_deref() {
            Ok("branches") => true,
            Ok("rows") | Err(_) => false,
            Ok(other) => {
                err!("PORTFOLIO_SELECT={other}: not rows or branches; rows is used");
                false
            }
        };
        Self {
            feed: Feed {
                rows: env("ROWS", 5_000),
                rate: env("RATE", 1_000),
                churn: env("CHURN", 5),
                moves,
            },
            focus_branches,
            quit_after: std::env::var("PORTFOLIO_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok()),
        }
    }
}

struct Root {
    grid: Entity<LiveGridState<Portfolio>>,
    size: Size,
    size_select: Entity<SelectState<Vec<&'static str>>>,
    filter: Entity<InputState>,
    /// The last `Confirm`, for the status bar.
    last_confirm: Option<SharedString>,
    /// The grid's counters and totals, refreshed every half second.
    counters: SharedString,
    totals: SharedString,
    _status: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            feed,
            focus_branches,
            quit_after,
        } = settings;
        // #region tree-grid
        let grid = cx.new(|cx| {
            let grid = LiveGridState::new_standalone(
                model::schema(),
                Portfolio,
                StoreOptions::default(),
                cx,
            )
            .with_view(
                ViewSpec::new()
                    .with_sort([(NodeColumn::Name, SortDirection::Ascending)])
                    .with_tree(TreeSpec::new().with_column(NodeColumn::Name))
                    // The totals add up the positions, the leaves
                    // (`TreeTotals::Leaves`): the books' sums do not count
                    // twice. The totals row and the status bar show them.
                    .with_aggregates([
                        (NodeColumn::Pnl, Aggregate::Sum),
                        (NodeColumn::Delta, Aggregate::Sum),
                    ]),
            )
            .with_totals(PinnedEdge::Bottom)
            // Funds and desks expanded, books collapsed.
            .with_expand_depth(ExpandDepth::Levels(2))
            // Rows at a time, as in `blotter`: a drag or Cmd/Ctrl+A takes
            // books and desks too, or with `FocusBranches` the positions
            // shown.
            .with_selection_mode(SelectionMode::RowSet);
            if focus_branches {
                grid.with_selection_policy(FocusBranches)
            } else {
                grid
            }
        });
        // #endregion tree-grid
        let size = Size::Small;
        let size_select = cx.new(|cx| {
            let selected = SIZES.iter().position(|(_, each)| *each == size);
            SelectState::new(
                SIZES.iter().map(|(label, _)| *label).collect(),
                selected.map(|row| IndexPath::default().row(row)),
                window,
                cx,
            )
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe(
                &size_select,
                |root, _, event: &SelectEvent<Vec<&'static str>>, cx| {
                    let SelectEvent::Confirm(Some(label)) = event else {
                        return;
                    };
                    if let Some((_, size)) = SIZES.iter().find(|(each, _)| each == label) {
                        root.size = *size;
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(&filter, |root, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = filter.read(cx).value();
                    root.grid
                        .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
                }
            }),
        ];

        // PORTFOLIO_E2E_LOG: events, state and geometry for script/e2e.
        e2e::install(&grid, cx);

        let writer = grid.read(cx).writer();
        std::thread::spawn(move || model::produce(writer, feed));

        Self {
            grid,
            size,
            size_select,
            filter,
            last_confirm: None,
            counters: SharedString::default(),
            totals: SharedString::default(),
            _status: Self::refresh_status(feed, quit_after, cx),
            _subscriptions: subscriptions,
        }
    }

    fn on_grid_event(
        &mut self,
        _: Entity<LiveGridState<Portfolio>>,
        event: &LiveGridEvent<u64, NodeColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::Confirm { keys, secondary } = event {
            const SHOWN: usize = 5;
            let mut ids: Vec<String> = keys.iter().take(SHOWN).map(u64::to_string).collect();
            if keys.len() > SHOWN {
                ids.push("…".into());
            }
            let how = if *secondary { " (secondary)" } else { "" };
            let opened = rows_label("Opened", keys.len());
            self.last_confirm = Some(format!("{opened}{how}: {}", ids.join(", ")).into());
            cx.notify();
        }
    }

    /// Updates the counters every half second; with `PORTFOLIO_SECONDS=N`
    /// prints them and quits after N seconds.
    fn refresh_status(feed: Feed, quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let mut last_rebuilds = 0;
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let Ok(done) = this.update(cx, |root: &mut Root, cx| {
                    let grid = root.grid.read(cx);
                    let stats = grid.stats();
                    let projection = grid.projection();
                    let rebuilds = (stats.rebuilds() - last_rebuilds) * 2;
                    last_rebuilds = stats.rebuilds();
                    root.counters = format!(
                        "{} rows ({BRANCHES} with children), {} updates/s · {} lines shown · expanded {} · {rebuilds} rebuilds/s, last {:.1} ms (max {:.1} ms) · cycles {}",
                        grid.snapshot().len(),
                        feed.rate,
                        projection.len(),
                        depth_label(grid.expand_depth()),
                        stats.last_rebuild().as_secs_f64() * 1e3,
                        stats.max_rebuild().as_secs_f64() * 1e3,
                        stats.tree_cycles(),
                    )
                    .into();
                    // The totals over the positions (the leaves): the sums
                    // of the books and above are not counted again.
                    let totals = projection.totals();
                    let total = |column: NodeColumn| match totals.get(column.ix()) {
                        Some(OwnedValue::Float(value)) => format!("{value:.2}"),
                        Some(OwnedValue::Int(value)) => value.to_string(),
                        _ => "–".into(),
                    };
                    root.totals = format!(
                        "Total P&L {} · Delta {}",
                        total(NodeColumn::Pnl),
                        total(NodeColumn::Delta)
                    )
                    .into();
                    if quit_after.is_some() {
                        out!(
                            "{:>5.1}s  {} · {}",
                            started.elapsed().as_secs_f64(),
                            root.counters,
                            root.totals
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

    fn set_expand_depth(&mut self, depth: ExpandDepth, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.set_expand_depth(depth, cx));
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter
            .update(cx, |filter, cx| filter.focus(window, cx));
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let dark = cx.theme().is_dark();
        Toolbar::new("portfolio-toolbar")
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                ToolbarGroup::new("expansion")
                    .label("Tree")
                    .gap_1()
                    .child(
                        Button::new("expand-all")
                            .label("Expand all")
                            .e2e_bounds("expand-all")
                            .on_click(cx.listener(|root, _, _, cx| {
                                root.set_expand_depth(ExpandDepth::All, cx)
                            })),
                    )
                    .child(
                        Button::new("collapse-all")
                            .label("Collapse all")
                            .e2e_bounds("collapse-all")
                            .on_click(cx.listener(|root, _, _, cx| {
                                root.set_expand_depth(ExpandDepth::Levels(0), cx)
                            })),
                    )
                    .child(
                        Button::new("books")
                            .label("Books")
                            .tooltip("Expand the books: every position shown")
                            .e2e_bounds("books")
                            .on_click(cx.listener(|root, _, _, cx| {
                                root.set_expand_depth(ExpandDepth::Levels(3), cx)
                            })),
                    ),
            )
            .content(Separator::vertical().h_5())
            .child(
                ToolbarGroup::new("view")
                    .label("View")
                    .gap_1()
                    .child(
                        Select::new(&self.size_select)
                            .title_prefix("Size: ")
                            .accessibility_label("Row size")
                            .w_32(),
                    )
                    .child(
                        Toggle::new("dark-theme")
                            .label("Dark theme")
                            .checked(dark)
                            .on_click(|dark, window, cx| {
                                let mode = if *dark {
                                    ThemeMode::Dark
                                } else {
                                    ThemeMode::Light
                                };
                                Theme::change(mode, Some(window), cx);
                            }),
                    ),
            )
            .content(div().flex_1())
            .child(
                Input::new(&self.filter)
                    .prefix(Icon::new(IconName::Search).small())
                    .cleanable(true)
                    .aria_label("Filter positions")
                    .w_48(),
            )
            .child(ProductsButton::new())
    }

    fn render_status(&self) -> impl IntoElement + use<> {
        let right: Vec<SharedString> = [Some(self.totals.clone()), self.last_confirm.clone()]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .collect();
        StatusBar::new()
            .left(self.counters.clone())
            .right(SharedString::from(right.join(" · ")))
    }

    fn grid_focus(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_focus_filter))
            .bg(cx.theme().background)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        LiveGrid::new(&self.grid)
                            .with_size(self.size)
                            .bordered(false)
                            // The grid's name for assistive technology.
                            .accessibility_label("Portfolio"),
                    )
                    .map(|this| e2e::measure_grid(this, &self.grid, self.size)),
            )
            .child(self.render_status())
    }
}

fn main() {
    let settings = Settings::from_env();

    gpui_kit::application()
        // gpui-kit's icons (chevrons, sort arrows, menu checks): without
        // them icons are blank.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            cx.bind_keys([KeyBinding::new(
                "secondary-f",
                FocusFilter,
                Some(KEY_CONTEXT),
            )]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1400.), px(900.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                let root = cx.new(|cx| Root::new(settings, window, cx));
                // The keyboard starts in the grid.
                let grid = root.read(cx).grid_focus(cx);
                window.focus(&grid, cx);
                root
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}
