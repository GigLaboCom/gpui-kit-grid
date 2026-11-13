//! A trader's orders, edited in place while the market streams in: every
//! input and output the grid gained in stage 3 in one window, for manual
//! checks and the Linux end-to-end run.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example orders
//! ROWS=20000 RATE=2000 cargo run --release -p gpui-kit-grid --example orders
//! ORDERS_GROUP=account ORDERS_POLICY=follow cargo run --release -p gpui-kit-grid --example orders
//! ORDERS_SECONDS=10 RATE=50 cargo run --release -p gpui-kit-grid --example orders
//! ```
//!
//! Every editor is here: Side and TIF pick from a list (`SelectEditor`),
//! Quantity and Limit are numbers (`NumberEditor`), Expiry a date
//! (`DateEditor`), Active a checkbox that commits through its element
//! (`CellEditHandle`, `CheckboxEditor` for F2 and the keyboard), Note text
//! (`TextEditor`). A commit goes to a "server" thread, which thinks for
//! `ORDERS_LATENCY_MS`, applies it to the store and answers: the cell shows
//! the value muted until then (`resolve_edit`), and a refused one goes back
//! with an error mark ("Price out of band": a limit more than 10% from
//! Last; "Exchange rejected": `ORDERS_REJECT_PERCENT` of the edits).
//! Another trader moves a limit now and then (`ORDERS_CONFLICT_PERCENT`): a
//! draft open on it keeps the trader's value and shows the conflict, or
//! follows the store (`ORDERS_POLICY=follow`, Keep draft / Follow on the
//! toolbar). Fills arrive (`FILLS`) and filled orders leave
//! (`ORDERS_REMOVE_FILLED_MS`); an open field on a leaving order closes.
//!
//! Side is a tag and Filled a progress bar (elements); Active is a checkbox
//! whose click selects the row too, Cancel a button whose click does not
//! (controls); Status is an icon whose header shows the icon alone.
//! Selected orders drag into the Basket on the right, or onto an account's
//! caption when grouped by account (`ORDERS_GROUP=account`); the context
//! menu moves them to the basket too, and Delete removes them from it.
//! Export… writes the shown orders as CSV, the waiting values as the cells
//! show them. The status bar tells what every edit came to.
//!
//! | Variable | Default | What it sets |
//! |---|---|---|
//! | `ROWS` | 200 | Orders at start |
//! | `RATE` | 50 | Last updates per second; `0`: no stream |
//! | `FILLS` | 2 | Fills per second |
//! | `ORDERS_LATENCY_MS` | 300 | How long the server thinks over an edit |
//! | `ORDERS_REJECT_PERCENT` | 10 | Edits the server rejects ("Exchange rejected") |
//! | `ORDERS_CONFLICT_PERCENT` | 5 | Seconds in which another trader moves a limit |
//! | `ORDERS_REMOVE_FILLED_MS` | 3000 | When a filled order leaves; `0`: never |
//! | `ORDERS_REORDER` | `live` | `live`, `hold` or `manual` |
//! | `ORDERS_POLICY` | `keep` | `keep` (`StandardEditPolicy`) or `follow` (`FollowLiveValue`) |
//! | `ORDERS_GROUP` | none | Grouping at start: `account`, `side` or `status` |
//! | `ORDERS_SORT` | none | The sort at start: a column id, `-` after it for descending (`last-`) |
//! | `ORDERS_EXPORT_PATH` | none | Export… writes here without asking for a file |
//! | `ORDERS_SEED` | random | The seed of the orders and the market |
//! | `ORDERS_SECONDS` | none | Print the status every half second and quit after N seconds |
//! | `ORDERS_E2E_LOG` | none | The end-to-end log of `script/e2e` (`e2e.rs`) |
//! | `ORDERS_SNAPSHOT` | none | With `--features test-support`: draw PNG files into this folder, no window (`run_snapshot`) |
//! | `ORDERS_CONFLICT_ON_EDIT`, `ORDERS_FILL_ON_EDIT` | `0` | `1`: 300 ms after an edit starts, its order's limit moves, or the order fills and leaves |
//!
//! Excel's edit gesture is the grid's default: a double click on an
//! editable cell edits it. An application that keeps the double click for
//! its primary command everywhere sets its own gesture, one line:
//! `grid.with_edit_gesture(NoDoubleClickEdit)` with `starts_edit` returning
//! false for `EditInput::Press { click_count: 2 }`.

// GPUI's `Action` derive spells its paths `gpui::`.
extern crate gpui_kit as gpui;

mod model;
mod server;
mod toolbar;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use async_channel::Sender;
use chrono::Utc;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, Size, button::Button, checkbox::Checkbox,
    menu::PopupMenu, progress::Progress, status_bar::StatusBar, tag::Tag,
};
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions, div,
    prelude::FluentBuilder as _, px, size,
};
use gpui_kit_grid::{
    CellElementContext, CellStyle, CellStyleContext, CellValue, CheckboxEditor, CsvFormat,
    DateEditor, DirectionalChanges, DropLine, DropOnRows, DropSource, DropTarget, EditError,
    EditValidateContext, EditableContext, ExportRequest, ExportScope, FollowLiveValue, GridColumn,
    GridColumns as _, GridHandleEvent, GridMenuContext, GridRow as _, GridSchema, GridStore,
    GroupBy, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridHandle, LiveGridState,
    NumberEditor, OwnedValue, RowDropContext, RowDropPolicy, SelectEditor, SelectOption,
    SelectedRowsDrag, SelectionMode, SortDirection, StandardEditPolicy, StoreOptions, TextEditor,
    ViewSpec,
};

use crate::{
    model::{Order, OrderColumn, Status},
    server::{Command, Market},
    toolbar::Reorder,
};

// A closed stdout or stderr, for example a parent that exited, must not
// abort the example.
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

macro_rules! err {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}

// After the macros, which they use.
mod e2e;
// The shared snapshot harness; `orders` does not time frames with its
// `measure`, which `desk` and `broker` use.
#[cfg(feature = "test-support")]
#[path = "../trading/snapshot.rs"]
#[allow(dead_code)]
mod snapshot;

gpui_kit::actions!(
    orders,
    [
        /// Inserts an order and opens the edit of its quantity (Cmd/Ctrl+N).
        NewOrder,
        /// Writes the shown orders to a CSV file (Export…).
        Export,
        /// Moves the orders of the command scope to the basket (the cell
        /// context menu); dragging them there does the same.
        MoveToBasket,
        /// Cancels the orders of the command scope (the cell context menu).
        CancelOrders,
        /// Removes the selected orders from the basket (Delete).
        RemoveFromBasket,
        /// Switches the edit policy: keep the draft, or follow the store.
        ToggleEditPolicy,
    ]
);

/// The key context of the window content, and of the basket.
const KEY_CONTEXT: &str = "Orders";
const BASKET_CONTEXT: &str = "Basket";

/// The grid's hooks. They take `&self` and never read the grid: commands
/// leave as events and Actions; the Cancel button sends to the server.
pub struct Orders {
    commands: Sender<Command>,
}

impl LiveGridDelegate for Orders {
    type Row = Order;

    /// The status icon in its colour.
    fn cell_style(&self, cell: &CellStyleContext<'_, Order>, style: &mut CellStyle, cx: &App) {
        if cell.column() == OrderColumn::Status {
            let theme = cx.theme();
            style.set_foreground(match cell.row().status {
                Status::New => theme.muted_foreground,
                Status::Working => theme.info,
                Status::PartFilled => theme.warning,
                Status::Filled => theme.success,
                Status::Cancelled => theme.muted_foreground,
                Status::Rejected => theme.danger,
            });
        }
    }

    /// Filled, cancelled and rejected orders take no edit.
    fn is_editable(&self, cell: &EditableContext<'_, Order>, _: &App) -> bool {
        !cell.row().status.is_final()
    }

    /// Quantities in lots of 100, an expiry not in the past.
    fn validate_edit(
        &self,
        edit: &EditValidateContext<'_, Order>,
        _: &App,
    ) -> Result<(), EditError> {
        match model::check_edit(edit.column(), edit.value(), Utc::now()) {
            Some(message) => Err(EditError::new(message)),
            None => Ok(()),
        }
    }

    /// Side as a tag, Filled as a bar, Active as a checkbox that commits
    /// through its handle (its click selects the row too), Cancel as a
    /// button that stops its press (the row stays as it was).
    fn render_cell(
        &self,
        cell: &CellElementContext<'_, Order>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let order = cell.row();
        let key = order.id;
        Some(match cell.column() {
            OrderColumn::Side => {
                let tag = if cell.text() == "Buy" {
                    Tag::success()
                } else {
                    Tag::danger()
                };
                tag.small()
                    .child(SharedString::from(cell.text().to_owned()))
                    .into_any_element()
            }
            // #region render-cell-filled
            OrderColumn::Filled => {
                let filled = match cell.value() {
                    CellValue::Float(filled) => filled as f32,
                    _ => 0.,
                };
                Progress::new("filled")
                    .value(filled * 100.)
                    .accessibility_label(format!("{:.0}% filled", filled * 100.))
                    .w_full()
                    .into_any_element()
            }
            // #endregion render-cell-filled
            // #region render-cell-active
            OrderColumn::Active => {
                let checked = matches!(cell.value(), CellValue::Bool(true));
                let edit = cell.edit();
                if let Some(edit) = edit.clone() {
                    cell.on_activate(move |window, cx| {
                        edit.commit(OwnedValue::Bool(!checked), window, cx);
                    });
                }
                Checkbox::new("active")
                    .checked(checked)
                    .with_size(cell.size())
                    .tab_stop(false)
                    .disabled(order.status.is_final())
                    .accessibility_label("Active")
                    .on_click(move |_, window, cx| {
                        if let Some(edit) = &edit {
                            edit.commit(OwnedValue::Bool(!checked), window, cx);
                        }
                    })
                    .into_any_element()
            }
            // #endregion render-cell-active
            OrderColumn::Cancel => {
                let closed = order.status.is_final();
                let commands = self.commands.clone();
                if !closed {
                    let commands = commands.clone();
                    cell.on_activate(move |_, _| {
                        let _ = commands.try_send(Command::Cancel(key));
                    });
                }
                div()
                    // The press is the button's: the row is not selected.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .xsmall()
                            .tab_stop(false)
                            .disabled(closed)
                            .on_click(move |_, _, _| {
                                let _ = commands.try_send(Command::Cancel(key));
                            }),
                    )
                    .into_any_element()
            }
            _ => return None,
        })
    }

    /// "Amend order": Enter and a double click on a cell that takes no
    /// edit open the limit's editor.
    fn primary_command(&self, _: &GridMenuContext<'_, Order>, _: &App) -> Option<SharedString> {
        Some("Amend order".into())
    }

    fn context_menu(
        &self,
        menu_context: &GridMenuContext<'_, Order>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let none = menu_context.selection().is_empty();
        let menu = menu
            .menu_with_disabled("Cancel orders", Box::new(CancelOrders), none)
            .menu_with_disabled("Move to basket", Box::new(MoveToBasket), none)
            .separator();
        menu_context.standard_items(menu, window, cx)
    }
}

// #region account-drop
/// Drops on the orders grid land on account captions only: the orders move
/// to that account.
struct AccountDrop;

impl RowDropPolicy<Order> for AccountDrop {
    fn place(&self, drop: &RowDropContext<'_, Order>) -> Option<DropTarget<u64>> {
        match drop.line() {
            DropLine::Caption(group)
                if group.level_id().as_str() == OrderColumn::Account.id()
                    && drop.rows().is_some() =>
            {
                Some(DropTarget::Group(group.path().clone()))
            }
            _ => None,
        }
    }
}
// #endregion account-drop

/// The basket's grid: orders put aside.
pub struct Basket;

impl LiveGridDelegate for Basket {
    type Row = Order;
}

/// The basket's columns: the order, its account and instrument.
fn basket_schema() -> GridSchema<OrderColumn> {
    GridSchema::new()
        .column(GridColumn::new(OrderColumn::Id, "Id").with_width(px(70.)))
        .column(GridColumn::new(OrderColumn::Account, "Account").with_width(px(80.)))
        .column(GridColumn::new(OrderColumn::Instrument, "Instrument").with_width(px(90.)))
        .column(GridColumn::new(OrderColumn::Quantity, "Quantity").with_width(px(80.)))
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// What the environment sets.
struct Settings {
    rows: u64,
    market: Market,
    reorder: Reorder,
    follow: bool,
    group: Option<OrderColumn>,
    sort: Option<(OrderColumn, SortDirection)>,
    export_path: Option<PathBuf>,
    quit_after: Option<u64>,
    conflict_on_edit: bool,
    fill_on_edit: bool,
}

impl Settings {
    fn from_env() -> Self {
        let seed = e2e::seed().unwrap_or_else(|| env("ORDERS_SEED", fastrand::u64(..)));
        let reorder = std::env::var("ORDERS_REORDER")
            .ok()
            .map_or(Reorder::Live, |value| {
                Reorder::from_env(&value).unwrap_or_else(|| {
                    err!("ORDERS_REORDER={value}: not live, hold or manual; live is used");
                    Reorder::Live
                })
            });
        let follow = match std::env::var("ORDERS_POLICY").as_deref() {
            Ok("keep") | Err(_) => false,
            Ok("follow") => true,
            Ok(other) => {
                err!("ORDERS_POLICY={other}: not keep or follow; keep is used");
                false
            }
        };
        let group = std::env::var("ORDERS_GROUP").ok().and_then(|value| {
            let column = OrderColumn::from_id(&value).filter(|column| {
                matches!(
                    column,
                    OrderColumn::Account | OrderColumn::Side | OrderColumn::Status
                )
            });
            if column.is_none() {
                err!("ORDERS_GROUP={value}: not account, side or status; not grouped");
            }
            column
        });
        let sort = std::env::var("ORDERS_SORT").ok().and_then(|value| {
            let (id, direction) = match value.strip_suffix('-') {
                Some(id) => (id, SortDirection::Descending),
                None => (value.as_str(), SortDirection::Ascending),
            };
            let column = OrderColumn::from_id(id);
            if column.is_none() {
                err!("ORDERS_SORT={value}: no such column; not sorted");
            }
            column.map(|column| (column, direction))
        });
        let remove = env("ORDERS_REMOVE_FILLED_MS", 3_000);
        Self {
            rows: env("ROWS", 200),
            market: Market {
                latency: Duration::from_millis(env("ORDERS_LATENCY_MS", 300)),
                reject_percent: env("ORDERS_REJECT_PERCENT", 10),
                rate: env("RATE", 50),
                fills: env("FILLS", 2),
                conflict_percent: env("ORDERS_CONFLICT_PERCENT", 5),
                remove_filled: (remove > 0).then(|| Duration::from_millis(remove)),
                seed,
            },
            reorder,
            follow,
            group,
            sort,
            export_path: std::env::var_os("ORDERS_EXPORT_PATH").map(PathBuf::from),
            quit_after: std::env::var("ORDERS_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok()),
            conflict_on_edit: env("ORDERS_CONFLICT_ON_EDIT", 0) != 0,
            fill_on_edit: env("ORDERS_FILL_ON_EDIT", 0) != 0,
        }
    }
}

/// The view at start: grouped and sorted as the environment says.
fn view(group: Option<OrderColumn>, sort: Option<(OrderColumn, SortDirection)>) -> ViewSpec<Order> {
    ViewSpec::new()
        .with_sort(sort)
        .with_group_by(group.map(GroupBy::column))
}

/// Builds the orders grid over `store`: every editor, the drag of selected
/// orders, drops on account captions, the reorder and edit policies.
fn orders_grid(
    store: GridStore<Order>,
    commands: Sender<Command>,
    follow: bool,
    cx: &mut Context<LiveGridState<Orders>>,
) -> LiveGridState<Orders> {
    let options = |labels: &[&'static str]| {
        labels
            .iter()
            .map(|label| SelectOption::new(OwnedValue::Text((*label).into()), *label))
            .collect::<Vec<_>>()
    };
    // #region orders-grid
    let grid = LiveGridState::new(model::schema(), Orders { commands }, store, cx)
        .with_selection_mode(SelectionMode::RowSet)
        .with_change_paint(gpui_kit_grid::ChangePaint::Directional(DirectionalChanges))
        .with_editor(OrderColumn::Side, SelectEditor::new(options(&model::SIDES)))
        .with_editor(OrderColumn::Tif, SelectEditor::new(options(&model::TIFS)))
        .with_editor(
            OrderColumn::Quantity,
            NumberEditor::new().with_min(OwnedValue::Int(1)),
        )
        .with_editor(OrderColumn::Limit, NumberEditor::new().with_scale(2))
        .with_editor(OrderColumn::Expiry, DateEditor::date())
        .with_editor(OrderColumn::Active, CheckboxEditor)
        .with_editor(OrderColumn::Note, TextEditor::new().with_max_length(80))
        .with_row_drag_gesture(SelectedRowsDrag::new())
        .with_row_drop_policy(AccountDrop);
    // #endregion orders-grid
    if follow {
        grid.with_edit_policy(FollowLiveValue)
    } else {
        grid.with_edit_policy(StandardEditPolicy)
    }
}

/// The export running: how many rows it writes.
struct Exporting {
    rows: usize,
}

pub struct Root {
    window: AnyWindowHandle,
    store: GridStore<Order>,
    grid: Entity<LiveGridState<Orders>>,
    basket: Entity<LiveGridState<Basket>>,
    commands: Sender<Command>,
    reorder: Reorder,
    follow: bool,
    size: Size,
    /// The counts and the held order, from the handle's events.
    status: GridStatus,
    /// The grid without its delegate type: the status bar and Columns….
    pub(crate) handle: LiveGridHandle,
    conflict_on_edit: bool,
    fill_on_edit: bool,
    export_path: Option<PathBuf>,
    exporting: Option<Exporting>,
    _export_task: Option<Task<()>>,
    /// What the status bar tells.
    last_commit: Option<SharedString>,
    last_answer: Option<SharedString>,
    last_cancel: Option<SharedString>,
    last_drop: Option<SharedString>,
    last_export: Option<SharedString>,
    counters: SharedString,
    next_id: u64,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Settings {
            rows,
            market,
            reorder,
            follow,
            group,
            sort,
            export_path,
            quit_after,
            conflict_on_edit,
            fill_on_edit,
        } = settings;
        let store = GridStore::new(StoreOptions::default(), cx);
        let writer = store.writer();
        let (commands, answers) = server::start(writer.clone(), market);
        let grid = cx.new(|cx| {
            orders_grid(store.clone(), commands.clone(), follow, cx).with_view(view(group, sort))
        });
        grid.update(cx, |grid, cx| reorder.apply(grid, cx));
        let handle = LiveGridHandle::new(&grid);
        // #region basket-grid
        let basket = cx.new(|cx| {
            LiveGridState::new_standalone(basket_schema(), Basket, StoreOptions::default(), cx)
                .with_selection_mode(SelectionMode::RowSet)
                .with_row_drop_policy(DropOnRows)
        });
        // #endregion basket-grid
        let subscriptions = vec![
            cx.subscribe(&grid, Self::on_grid_event),
            cx.subscribe(&basket, Self::on_basket_event),
            Self::follow_handle(&handle, cx),
        ];
        // ORDERS_E2E_LOG: events, state and geometry for script/e2e.
        e2e::install(&grid, &basket, cx);

        writer.replace_all(
            model::orders(rows, market.seed)
                .into_iter()
                .map(std::sync::Arc::new),
        );
        // The first data set: once it is on screen, select its first row.
        let loading = cx.spawn(async move |this, cx| {
            let Ok(flushed) = this.update(cx, |root, cx| {
                root.grid.update(cx, |grid, cx| grid.flush(cx))
            }) else {
                return;
            };
            if flushed.await.is_some() {
                let _ = this.update(cx, |root, cx| {
                    root.grid.update(cx, |grid, cx| {
                        let projection = grid.projection();
                        let first = (0..projection.len())
                            .find_map(|ix| projection.row(ix))
                            .and_then(|id| grid.snapshot().row(id))
                            .map(|row| *row.key());
                        grid.select_key(first, cx);
                    })
                });
            }
        });
        // #region resolve-edit
        // The server's answers settle the commits.
        let answering = cx.spawn(async move |this, cx| {
            while let Ok((edit, result)) = answers.recv().await {
                let alive = this.update(cx, |root, cx| {
                    root.last_answer = Some(match &result {
                        Ok(()) => "Server: accepted".into(),
                        Err(reason) => format!("Server: {reason}").into(),
                    });
                    let result = result.map_err(EditError::new);
                    root.grid
                        .update(cx, |grid, cx| grid.resolve_edit(edit, result, cx));
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // #endregion resolve-edit
        Self {
            window: window.window_handle(),
            next_id: rows + 1,
            store,
            grid,
            basket,
            commands,
            reorder,
            follow,
            size: Size::Small,
            status: GridStatus::default(),
            handle,
            conflict_on_edit,
            fill_on_edit,
            export_path,
            exporting: None,
            _export_task: None,
            last_commit: None,
            last_answer: None,
            last_cancel: None,
            last_drop: None,
            last_export: None,
            counters: SharedString::default(),
            _tasks: vec![loading, answering, Self::refresh_status(quit_after, cx)],
            _subscriptions: subscriptions,
        }
    }

    fn on_grid_event(
        &mut self,
        grid: Entity<LiveGridState<Orders>>,
        event: &LiveGridEvent<u64, OrderColumn>,
        cx: &mut Context<Self>,
    ) {
        match event {
            LiveGridEvent::EditStart { key, .. } => {
                if self.conflict_on_edit {
                    let _ = self.commands.try_send(Command::ConflictLater(*key));
                }
                if self.fill_on_edit {
                    let _ = self.commands.try_send(Command::FillLater(*key));
                }
            }
            // #region edit-commit
            LiveGridEvent::EditCommit {
                edit,
                key,
                column,
                value,
                base,
                current,
            } => {
                let conflict = if base == current {
                    ""
                } else {
                    " (over a newer value)"
                };
                self.last_commit = Some(
                    format!(
                        "Sent {} of order {key}: {}{conflict}",
                        column.id(),
                        value_text(value)
                    )
                    .into(),
                );
                self.last_answer = Some("Server: thinking…".into());
                let _ = self.commands.try_send(Command::Edit {
                    edit: *edit,
                    key: *key,
                    column: *column,
                    value: value.clone(),
                });
            }
            // #endregion edit-commit
            LiveGridEvent::EditCancel {
                key,
                column,
                reason,
                draft,
            } => {
                let draft = draft
                    .as_ref()
                    .map(|draft| format!(", draft {}", value_text(draft)))
                    .unwrap_or_default();
                self.last_cancel = Some(
                    format!(
                        "Edit of order {key} {} closed: {reason:?}{draft}",
                        column.id()
                    )
                    .into(),
                );
            }
            LiveGridEvent::Confirm { keys, .. } => {
                // Amend order: the limit's editor of the first order, once
                // the grid's update is over.
                if let Some(key) = keys.first().copied() {
                    let window = self.window;
                    cx.defer(move |cx| {
                        let _ = window.update(cx, |_, window, cx| {
                            grid.update(cx, |grid, cx| {
                                grid.start_edit(&key, OrderColumn::Limit, window, cx)
                            });
                        });
                    });
                }
            }
            // #region drop-on-caption
            LiveGridEvent::Drop { source, target, .. } => {
                // Orders dropped on an account's caption move to it.
                if let (DropSource::Rows(drag), DropTarget::Group(path)) = (source, target)
                    && let Some((_, OwnedValue::Text(account))) = path.levels().first()
                {
                    let account = OwnedValue::Text(account.clone());
                    let writer = self.store.writer();
                    let snapshot = writer.snapshot();
                    for key in drag.keys().iter() {
                        if let Some(order) = snapshot
                            .get(key)
                            .and_then(|order| order.with(OrderColumn::Account, &account))
                        {
                            writer.upsert(order);
                        }
                    }
                    self.last_drop = Some(
                        format!(
                            "Moved {} to {}",
                            orders_label(drag.keys().len()),
                            value_text(&account)
                        )
                        .into(),
                    );
                }
            }
            // #endregion drop-on-caption
            LiveGridEvent::SelectionChange { .. } | LiveGridEvent::FocusChange { .. } => {}
            _ => return,
        }
        cx.notify();
    }

    /// The basket takes dragged orders: they leave the orders grid.
    fn on_basket_event(
        &mut self,
        _: Entity<LiveGridState<Basket>>,
        event: &LiveGridEvent<u64, OrderColumn>,
        cx: &mut Context<Self>,
    ) {
        if let LiveGridEvent::Drop {
            source: DropSource::Rows(drag),
            ..
        } = event
            && drag.grid() == self.grid.entity_id()
        {
            let keys = drag.keys().to_vec();
            self.move_to_basket(&keys, cx);
        }
    }

    fn move_to_basket(&mut self, keys: &[u64], cx: &mut Context<Self>) {
        let writer = self.store.writer();
        let snapshot = writer.snapshot();
        let basket = self.basket.read(cx).writer();
        let mut moved = 0;
        for key in keys {
            if let Some(order) = snapshot.get(key) {
                basket.upsert(order.clone());
                writer.remove(*key);
                moved += 1;
            }
        }
        self.last_drop = Some(format!("Moved {} to the basket", orders_label(moved)).into());
        cx.notify();
    }

    fn on_move_to_basket(&mut self, _: &MoveToBasket, _: &mut Window, cx: &mut Context<Self>) {
        let keys = self.grid.read(cx).scoped_selection().keys().to_vec();
        self.move_to_basket(&keys, cx);
    }

    fn on_cancel_orders(&mut self, _: &CancelOrders, _: &mut Window, cx: &mut Context<Self>) {
        for key in self.grid.read(cx).scoped_selection().keys() {
            let _ = self.commands.try_send(Command::Cancel(*key));
        }
    }

    fn on_remove_from_basket(
        &mut self,
        _: &RemoveFromBasket,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let basket = self.basket.read(cx);
        let writer = basket.writer();
        for key in basket.scoped_selection().keys() {
            writer.remove(*key);
        }
    }

    /// New order: a working order on the first instrument, then the edit of
    /// its quantity once the grid shows it.
    fn on_new_order(&mut self, _: &NewOrder, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id += 1;
        let Some(template) = model::orders(1, id).pop() else {
            return;
        };
        let order = Order {
            id,
            status: Status::New,
            filled: 0.,
            placed: Utc::now(),
            note: "".into(),
            ..template
        };
        self.store.writer().upsert(order);
        let flushed = self.grid.update(cx, |grid, cx| grid.flush(cx));
        cx.spawn_in(window, async move |this, cx| {
            if flushed.await.is_none() {
                return;
            }
            let _ = this.update_in(cx, |root, window, cx| {
                root.grid.update(cx, |grid, cx| {
                    grid.select_key(Some(id), cx);
                    grid.start_edit(&id, OrderColumn::Quantity, window, cx)
                });
            });
        })
        .detach();
    }

    /// Keep draft / Follow: a new grid over the same store, with the
    /// layout of the old one (the grid takes its edit policy when it is
    /// built).
    fn on_toggle_edit_policy(
        &mut self,
        _: &ToggleEditPolicy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_follow(!self.follow, window, cx);
    }

    pub(crate) fn set_follow(&mut self, follow: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.follow == follow {
            return;
        }
        self.follow = follow;
        let layout = self.grid.read(cx).layout();
        let (store, commands, reorder) = (self.store.clone(), self.commands.clone(), self.reorder);
        let grid = cx.new(|cx| orders_grid(store, commands, follow, cx).with_layout(layout));
        grid.update(cx, |grid, cx| reorder.apply(grid, cx));
        self._subscriptions[0] = cx.subscribe(&grid, Self::on_grid_event);
        self.handle = LiveGridHandle::new(&grid);
        self._subscriptions[2] = Self::follow_handle(&self.handle, cx);
        e2e::install(&grid, &self.basket, cx);
        self.grid = grid;
        let focus = self.grid.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The status bar's counts and the held order, from the handle's events.
    fn follow_handle(handle: &LiveGridHandle, cx: &mut Context<Self>) -> Subscription {
        let this = cx.weak_entity();
        handle.subscribe(cx, move |event, cx| {
            let _ = this.update(cx, |root, cx| {
                let total = root.handle.total_row_count(cx);
                if root.status.apply(event, total) {
                    cx.notify();
                }
            });
        })
    }

    pub(crate) fn is_order_held(&self) -> bool {
        self.status.order_held
    }

    pub(crate) fn is_exporting(&self) -> bool {
        self.exporting.is_some()
    }

    pub(crate) fn set_reorder(&mut self, reorder: Reorder, cx: &mut Context<Self>) {
        self.reorder = reorder;
        self.grid.update(cx, |grid, cx| reorder.apply(grid, cx));
        cx.notify();
    }

    /// Export…: the shown orders to a CSV file, the waiting values as the
    /// cells show them. Asks for the file unless `ORDERS_EXPORT_PATH` names
    /// it; opens it when written (not with `ORDERS_EXPORT_PATH`).
    fn on_export(&mut self, _: &Export, _: &mut Window, cx: &mut Context<Self>) {
        if self.exporting.is_some() {
            return;
        }
        if self.grid.read(cx).is_export_empty(ExportScope::Visible) {
            self.last_export = Some("Nothing to export".into());
            cx.notify();
            return;
        }
        let fixed = self.export_path.clone();
        self._export_task = Some(cx.spawn(async move |this, cx| {
            let (path, open) = match fixed {
                Some(path) => (path, false),
                None => {
                    let directory = std::env::current_dir().unwrap_or_default();
                    let chosen =
                        cx.update(|cx| cx.prompt_for_new_path(&directory, Some("orders.csv")));
                    match chosen.await {
                        Ok(Ok(Some(path))) => (path, true),
                        _ => return,
                    }
                }
            };
            let Ok(Ok(job)) = this.update(cx, |root, cx| {
                root.grid.update(cx, |grid, cx| {
                    // #region export-to-path
                    grid.export_to_path(
                        ExportRequest::new(ExportScope::Visible),
                        CsvFormat::default(),
                        path.clone(),
                        cx,
                    )
                    // #endregion export-to-path
                })
            }) else {
                let _ = this.update(cx, |root, cx| {
                    root.last_export = Some("Export failed to start".into());
                    cx.notify();
                });
                return;
            };
            let rows = job.progress().rows_total();
            let _ = this.update(cx, |root, cx| {
                root.exporting = Some(Exporting { rows });
                cx.notify();
            });
            let result = job.await;
            let _ = this.update(cx, |root, cx| {
                root.exporting = None;
                root.last_export = Some(match &result {
                    Ok(outcome) => format!(
                        "Exported {} to {}",
                        orders_label(outcome.data_rows()),
                        outcome.output().display()
                    )
                    .into(),
                    Err(error) => format!("Export failed: {error}").into(),
                });
                if let (Ok(outcome), true) = (&result, open) {
                    cx.open_with_system(outcome.output());
                }
                cx.notify();
            });
        }));
    }

    /// The counters every half second; with `ORDERS_SECONDS=N` printed, and
    /// the example quits after N seconds.
    fn refresh_status(quit_after: Option<u64>, cx: &mut Context<Self>) -> Task<()> {
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let mut last_frames = 0;
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let Ok(done) = this.update(cx, |root: &mut Root, cx| {
                    let grid = root.grid.read(cx);
                    let stats = grid.stats();
                    let fps = (stats.frames() - last_frames) * 2;
                    last_frames = stats.frames();
                    root.counters = format!(
                        "{} orders · {fps} fps · rebuilds {} · hosts {} ({:.2} ms) · pending edits {}",
                        grid.snapshot().len(),
                        stats.rebuilds(),
                        stats.cell_hosts(),
                        stats.last_hosts_prepaint().as_secs_f64() * 1e3,
                        grid.pending_edit_count(),
                    )
                    .into();
                    if quit_after.is_some() {
                        out!("{:>5.1}s  {}", started.elapsed().as_secs_f64(), root.counters);
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

    fn render_status(&self, cx: &App) -> impl IntoElement + use<> {
        let grid = self.grid.read(cx);
        let editing = grid.editing_cell().map(|(key, column)| {
            SharedString::from(format!("Editing order {key} {}", column.id()))
        });
        let export = self.exporting.as_ref().map(|exporting| {
            SharedString::from(format!("Exporting {}…", orders_label(exporting.rows)))
        });
        let state: Vec<SharedString> = self
            .status
            .items()
            .into_iter()
            .map(SharedString::from)
            .chain(
                [
                    editing,
                    self.last_commit.clone(),
                    self.last_answer.clone(),
                    self.last_cancel.clone(),
                    self.last_drop.clone(),
                    export.or_else(|| self.last_export.clone()),
                ]
                .into_iter()
                .flatten(),
            )
            .collect();
        StatusBar::new()
            .left(self.counters.clone())
            .right(SharedString::from(state.join(" · ")))
    }
}

/// What the status bar shows of the grid, from its handle's events
/// (`GridHandleEvent`): the orders shown of the orders loaded, the selected
/// ones and a held order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GridStatus {
    shown: usize,
    total: usize,
    selected: usize,
    order_held: bool,
}

impl GridStatus {
    /// Takes in an event of the grid, `total` the orders loaded now.
    /// Returns true if the status bar should show it at once.
    fn apply(&mut self, event: &GridHandleEvent, total: usize) -> bool {
        match *event {
            GridHandleEvent::ProjectionChange { row_count, .. } => {
                self.shown = row_count;
                self.total = total;
                true
            }
            GridHandleEvent::SelectionChange { count, .. } => {
                self.selected = count;
                true
            }
            GridHandleEvent::OrderHeldChange { held } => {
                self.order_held = held;
                true
            }
            _ => false,
        }
    }

    /// "180 of 200 orders", "Order held", "3 orders selected".
    fn items(&self) -> Vec<String> {
        let shown = if self.shown == self.total {
            orders_label(self.total)
        } else {
            format!("{} of {}", self.shown, orders_label(self.total))
        };
        [
            Some(shown),
            self.order_held.then(|| "Order held".to_owned()),
            (self.selected > 0).then(|| format!("{} selected", orders_label(self.selected))),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// "1 order", "3 orders".
fn orders_label(count: usize) -> String {
    if count == 1 {
        "1 order".into()
    } else {
        format!("{count} orders")
    }
}

/// A value as the status bar tells it.
pub fn value_text(value: &OwnedValue) -> String {
    match value {
        OwnedValue::Empty => "empty".into(),
        OwnedValue::Text(text) => text.to_string(),
        OwnedValue::Int(value) => value.to_string(),
        OwnedValue::Float(value) => format!("{value:.2}"),
        OwnedValue::Bool(value) => value.to_string(),
        OwnedValue::Timestamp(at) => at.format("%Y-%m-%d").to_string(),
        other => format!("{other:?}"),
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_new_order))
            .on_action(cx.listener(Self::on_export))
            .on_action(cx.listener(Self::on_move_to_basket))
            .on_action(cx.listener(Self::on_cancel_orders))
            .on_action(cx.listener(Self::on_toggle_edit_policy))
            .bg(cx.theme().background)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .relative()
                            .child(
                                LiveGrid::new(&self.grid)
                                    .with_size(self.size)
                                    .bordered(false)
                                    .group_panel(false)
                                    .accessibility_label("Orders"),
                            )
                            .map(|this| e2e::measure_grid(this, &self.grid, self.size)),
                    )
                    .child(
                        div()
                            .w(px(300.))
                            .h_full()
                            .relative()
                            .flex()
                            .flex_col()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .key_context(BASKET_CONTEXT)
                            .on_action(cx.listener(Self::on_remove_from_basket))
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Basket: drag orders here"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .relative()
                                    .child(
                                        LiveGrid::new(&self.basket)
                                            .with_size(self.size)
                                            .bordered(false)
                                            .accessibility_label("Basket"),
                                    )
                                    .map(|this| e2e::measure_basket(this, self.size)),
                            ),
                    ),
            )
            .child(self.render_status(cx))
    }
}

fn main() {
    let settings = Settings::from_env();
    // Offscreen only: never fall through to a window.
    if let Some(dir) = std::env::var_os("ORDERS_SNAPSHOT") {
        #[cfg(feature = "test-support")]
        run_snapshot(settings, std::path::Path::new(&dir));
        #[cfg(not(feature = "test-support"))]
        err!(
            "ORDERS_SNAPSHOT={}: needs --features test-support",
            dir.to_string_lossy()
        );
        return;
    }

    gpui_kit::application()
        // gpui-kit's icons: the status icons, the sort arrows, the menus.
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            cx.bind_keys([
                KeyBinding::new("secondary-n", NewOrder, Some(KEY_CONTEXT)),
                KeyBinding::new("delete", RemoveFromBasket, Some(BASKET_CONTEXT)),
                KeyBinding::new("backspace", RemoveFromBasket, Some(BASKET_CONTEXT)),
            ]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(Bounds::centered(
                    None,
                    size(px(1500.), px(900.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                let root = cx.new(|cx| Root::new(settings, window, cx));
                // The keyboard starts in the grid.
                let grid = root.read(cx).grid.focus_handle(cx);
                window.focus(&grid, cx);
                root
            })
            .expect("open window");
            // Bring the window to the front: an occluded window draws no frames.
            cx.activate(true);
        });
}

/// `ORDERS_SNAPSHOT=<dir>`: the orders drawn offscreen into PNG files
/// (`examples/trading/snapshot.rs`): the window after the load
/// (`orders-light.png`), a limit being edited (`orders-edit.png`), Side's
/// list open (`orders-select.png`), Expiry's calendar (`orders-date.png`)
/// and the dark theme (`orders-dark.png`). The market runs meanwhile.
#[cfg(feature = "test-support")]
fn run_snapshot(mut settings: Settings, dir: &std::path::Path) {
    use gpui_kit::component::{Theme, ThemeMode};

    settings.quit_after = None;
    let Some((mut snapshot, root)) = snapshot::Snapshot::open(
        dir,
        size(px(1500.), px(800.)),
        |cx| {
            cx.bind_keys([KeyBinding::new("secondary-n", NewOrder, Some(KEY_CONTEXT))]);
        },
        |window, cx| cx.new(|cx| Root::new(settings, window, cx)),
    ) else {
        return;
    };
    snapshot.run_for(Duration::from_millis(1_500));
    snapshot.save("orders-light");
    let grid = snapshot.read(&root, |root, _| root.grid.clone());
    let window = snapshot.read(&root, |root, _| root.window);
    let edit = |snapshot: &mut snapshot::Snapshot, column: OrderColumn| {
        let grid = grid.clone();
        snapshot.update(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                let focus = grid.focus_handle(cx);
                window.focus(&focus, cx);
                grid.update(cx, |grid, cx| {
                    grid.cancel_edit(window, cx);
                    let key = grid.visible_keys().into_iter().find(|key| {
                        grid.snapshot()
                            .get(key)
                            .is_some_and(|order| !order.status.is_final())
                    });
                    if let Some(key) = key {
                        grid.start_edit(&key, column, window, cx);
                    }
                });
            });
        });
    };
    edit(&mut snapshot, OrderColumn::Limit);
    snapshot.run_for(Duration::from_millis(400));
    snapshot.save("orders-edit");
    edit(&mut snapshot, OrderColumn::Side);
    snapshot.run_for(Duration::from_millis(400));
    snapshot.save("orders-select");
    edit(&mut snapshot, OrderColumn::Expiry);
    snapshot.run_for(Duration::from_millis(400));
    snapshot.save("orders-date");
    snapshot.update(|cx| {
        let _ = window.update(cx, |_, window, cx| {
            grid.update(cx, |grid, cx| grid.cancel_edit(window, cx));
            Theme::change(ThemeMode::Dark, Some(window), cx);
        });
    });
    snapshot.run_for(Duration::from_millis(400));
    snapshot.save("orders-dark");
}

#[cfg(test)]
mod tests {
    use gpui_kit_grid::SelectionOrigin;

    use super::*;

    #[test]
    fn test_grid_status() {
        let mut status = GridStatus::default();
        assert!(status.apply(
            &GridHandleEvent::ProjectionChange {
                row_count: 180,
                store_version: 3,
            },
            200,
        ));
        assert!(status.apply(
            &GridHandleEvent::SelectionChange {
                count: 3,
                origin: SelectionOrigin::User,
            },
            200,
        ));
        assert!(status.apply(&GridHandleEvent::OrderHeldChange { held: true }, 200));
        assert_eq!(
            status.items(),
            ["180 of 200 orders", "Order held", "3 orders selected"]
        );
        assert_eq!(value_text(&OwnedValue::Float(101.5)), "101.50");
    }
}
