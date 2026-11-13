//! Recipes for live applications, each between named region markers: the
//! site's Cookbook page shows those regions, so its code builds with every
//! check of the repository. The example around them is
//! a quotes window whose toolbar runs the recipes.
//!
//! ```sh
//! cargo run --release -p gpui-kit-grid --example cookbook
//! # quotes from the WebSocket gateway instead of the simulator:
//! cargo run --release -p gpui-kit-grid-gateway
//! COOKBOOK_FEED=ws://127.0.0.1:9001 cargo run --release -p gpui-kit-grid --example cookbook
//! ```
//!
//! The layout of the main grid is kept in `COOKBOOK_LAYOUT`, by default
//! `gpui-kit-grid-cookbook-layout.json` in the temporary directory.

use std::{
    fmt::Write as _,
    io::{Write as _, stdout},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, Toggle},
};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit_grid::{
    CellChange, CellExporter as _, CellStyle, CellValue, ChangePaint, ChangePainter, ChangePalette,
    ColumnIx, ExportError, ExportJob, ExportRequest, ExportScope, GridColumn, GridColumns,
    GridProgress, GridRow, GridSchema, GridStore, GridWriter, HoldReorder, JsonLayoutCodec,
    LayoutCodec as _, LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, LiveReorder,
    RowChange, SortDirection, StoreOptions, UniformChanges, ViewSpec,
};
use gpui_kit_grid_feed::{FeedAction, FeedClient, QuoteRecord, ServerMessage};
use tungstenite::Message;

// A closed stdout, for example a parent that exited, must not abort the example.
macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(stdout().lock(), $($arg)*);
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum QuoteColumn {
    Id,
    Last,
    Bid,
    Ask,
    Volume,
}

impl GridColumns for QuoteColumn {
    fn all() -> &'static [Self] {
        use QuoteColumn::*;
        &[Id, Last, Bid, Ask, Volume]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Last => "last",
            Self::Bid => "bid",
            Self::Ask => "ask",
            Self::Volume => "volume",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A quote keyed by instrument id, as the gateway's feed sends it.
#[derive(Clone)]
struct Quote(QuoteRecord);

impl GridRow for Quote {
    type Key = u32;
    type Column = QuoteColumn;

    fn key(&self) -> &u32 {
        &self.0.id
    }

    fn value(&self, column: QuoteColumn) -> CellValue<'_> {
        match column {
            QuoteColumn::Id => CellValue::Int(i64::from(self.0.id)),
            QuoteColumn::Last => CellValue::Float(self.0.last),
            QuoteColumn::Bid => CellValue::Float(self.0.bid),
            QuoteColumn::Ask => CellValue::Float(self.0.ask),
            QuoteColumn::Volume => CellValue::Int(self.0.volume),
        }
    }
}

struct Quotes;

impl LiveGridDelegate for Quotes {
    type Row = Quote;
}

type QuotesGrid = Entity<LiveGridState<Quotes>>;

fn schema() -> GridSchema<QuoteColumn> {
    GridSchema::new()
        .with_frozen(1)
        .column(GridColumn::new(QuoteColumn::Id, "Id").with_width(px(70.)))
        .column(GridColumn::new(QuoteColumn::Last, "Last"))
        .column(GridColumn::new(QuoteColumn::Bid, "Bid"))
        .column(GridColumn::new(QuoteColumn::Ask, "Ask"))
        .column(GridColumn::new(QuoteColumn::Volume, "Volume"))
}

// #region two-grids-one-store
/// Two views of one live store: every quote by price in the main grid, and
/// the same quotes by volume beside it. The store ingests each write once;
/// each grid sorts, filters and highlights on its own.
fn two_grids(layout_path: PathBuf, cx: &mut App) -> (QuotesGrid, QuotesGrid, GridWriter<Quote>) {
    let store = GridStore::builder(StoreOptions::default()).build(cx);
    let writer = store.writer();
    let main = grid_with_saved_layout(store.clone(), layout_path, cx);
    let by_volume = cx.new(|cx| {
        LiveGridState::new(schema(), Quotes, store, cx)
            .with_view(
                ViewSpec::new().with_sort([(QuoteColumn::Volume, SortDirection::Descending)]),
            )
            .with_change_paint(ChangePaint::Custom(Arc::new(OnlyColumn(
                QuoteColumn::Volume.ix(),
            ))))
    });
    (main, by_volume, writer)
}
// #endregion two-grids-one-store

// #region flash-some-columns
/// Paints changes as the default painter does, but an update only in one
/// column: a new or removed row still flashes whole. Runs in prepaint for
/// every changed visible cell, so it neither allocates nor locks.
struct OnlyColumn(ColumnIx);

impl ChangePainter for OnlyColumn {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        if change.change() != RowChange::Updated || change.column_ix() == Some(self.0) {
            UniformChanges.paint(change, palette, style);
        }
    }
}
// #endregion flash-some-columns

// #region save-layout
/// A grid whose layout (column order, widths, frozen and hidden columns,
/// the sort, the grouping and its expansion) is read from `path` at startup
/// and written back after every change.
fn grid_with_saved_layout(store: GridStore<Quote>, path: PathBuf, cx: &mut App) -> QuotesGrid {
    // A missing or unreadable file starts with the schema's layout.
    let saved = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| JsonLayoutCodec::default().decode(&text).ok());
    let grid = cx.new(|cx| {
        let grid = LiveGridState::new(schema(), Quotes, store, cx)
            // Names the kind of grid: a layout saved for another one is refused.
            .with_view_id("quotes")
            .with_view(ViewSpec::new().with_sort([(QuoteColumn::Last, SortDirection::Descending)]));
        match saved {
            Some(layout) => grid.with_layout(layout),
            None => grid,
        }
    });
    cx.subscribe(
        &grid,
        move |grid, event: &LiveGridEvent<u32, QuoteColumn>, cx| {
            // The four events that change what a layout holds. A small file is
            // written at once; a larger state would debounce the write.
            if matches!(
                event,
                LiveGridEvent::LayoutChange { .. }
                    | LiveGridEvent::SortChange { .. }
                    | LiveGridEvent::GroupChange { .. }
                    | LiveGridEvent::ExpansionChange { .. }
            ) && let Ok(text) = grid.read(cx).export_layout()
                && let Err(error) = std::fs::write(&path, text)
            {
                out!("layout not saved: {error}");
            }
        },
    )
    .detach();
    grid
}
// #endregion save-layout

// #region reload-progress
/// Reloads every quote from a slow source while live updates go on: a bar
/// under the header says so until the new rows are on screen.
fn reload_with_progress(
    writer: &GridWriter<Quote>,
    load: impl FnOnce() -> Vec<Quote> + Send + 'static,
) {
    // From now on, live writes wait and are replayed over the loaded rows.
    let reload = writer.begin_reload();
    writer.set_progress(Some(
        GridProgress::indeterminate().with_label("Reloading quotes"),
    ));
    let writer = writer.clone();
    std::thread::spawn(move || {
        let rows = load();
        // One snapshot shows the loaded rows, the replayed writes and no bar.
        writer.batch(|batch| {
            batch.finish_reload(reload, rows.into_iter().map(Arc::new));
            batch.set_progress(None);
        });
    });
}
// #endregion reload-progress

// #region hold-order
/// Holds the order while the user reads: with `hold`, a new order waits
/// while the pointer is over the grid, at most three seconds (and as long
/// as a menu or an editor is open). Without it, every new order shows at
/// once, a held one too.
fn set_hold_order(grid: &QuotesGrid, hold: bool, cx: &mut App) {
    grid.update(cx, |grid, cx| {
        if hold {
            grid.set_reorder_policy(HoldReorder::new(Duration::from_secs(3)), cx);
        } else {
            grid.set_reorder_policy(LiveReorder, cx);
        }
    });
}

/// Keeps `Root::order_held` in step with the grid, for a "Refresh order"
/// button that shows the held order at once.
fn watch_held_order(grid: &QuotesGrid, cx: &mut Context<Root>) -> Subscription {
    cx.subscribe(
        grid,
        |root, _, event: &LiveGridEvent<u32, QuoteColumn>, cx| {
            if let LiveGridEvent::OrderHeldChange { held } = event {
                root.order_held = *held;
                cx.notify();
            }
        },
    )
}

fn refresh_order(grid: &QuotesGrid, cx: &mut App) {
    grid.update(cx, |grid, cx| grid.apply_held_order(cx));
}
// #endregion hold-order

// #region export-with
/// The lines on screen as a Markdown table, written in the background from
/// one snapshot of the grid; the job resolves with the text.
fn export_markdown(
    grid: &mut LiveGridState<Quotes>,
    cx: &mut Context<LiveGridState<Quotes>>,
) -> Result<ExportJob<String>, ExportError> {
    let request = ExportRequest::new(ExportScope::Visible);
    grid.export_with(
        request,
        |table, cells, control| {
            let mut out = String::new();
            let columns = table.columns();
            for column in columns {
                write!(out, "| {} ", column.title()).ok();
            }
            out.push_str("|\n");
            out.push_str(&"|---".repeat(columns.len()));
            out.push_str("|\n");
            let mut text = String::new();
            for (ix, row) in table.rows().iter().enumerate() {
                for column in columns {
                    text.clear();
                    // The text a copy writes: numbers with the decimals shown.
                    if let Some(cell) = row.cell(column.column()) {
                        cells.write_cell(&cell, &mut text);
                    }
                    write!(out, "| {} ", text.replace('|', "\\|")).ok();
                }
                out.push_str("|\n");
                // Progress, and the point where a cancelled export stops.
                if ix % 1024 == 1023 {
                    control.advance(1024)?;
                }
            }
            control.advance(table.rows().len() % 1024)?;
            Ok(out)
        },
        cx,
    )
}
// #endregion export-with

// #region websocket-feed
/// Feeds the grid from the quotes gateway over a WebSocket: the first
/// snapshot replaces every row, each delta in order is one batch, and a gap
/// asks the server for a new snapshot. A lost connection comes back after
/// 1 s, doubling up to 30 s; the thread ends with the grid.
fn feed_from_gateway(writer: GridWriter<Quote>, address: String) {
    std::thread::spawn(move || {
        let mut client = FeedClient::new();
        let mut backoff = Duration::from_secs(1);
        while !writer.is_closed() {
            if let Ok((mut socket, _)) = tungstenite::connect(address.as_str()) {
                backoff = Duration::from_secs(1);
                let mut open = socket
                    .send(Message::binary(client.on_open().to_bytes()))
                    .is_ok();
                // The server sends a heartbeat every second, so this loop
                // sees a closed writer soon after the window closes.
                while open && !writer.is_closed() {
                    let Ok(message) = socket.read() else { break };
                    let Message::Binary(bytes) = message else {
                        continue;
                    };
                    let action = match ServerMessage::decode(&bytes) {
                        Ok(message) => client.on_message(message),
                        Err(_) => client.on_malformed(),
                    };
                    match action {
                        Some(FeedAction::ReplaceAll(rows)) => {
                            writer.replace_all(rows.into_iter().map(|row| Arc::new(Quote(row))));
                        }
                        Some(FeedAction::Batch { upserts, removes }) => writer.batch(|batch| {
                            upserts.into_iter().for_each(|row| batch.upsert(Quote(row)));
                            removes.into_iter().for_each(|id| batch.remove(id));
                        }),
                        Some(FeedAction::Send(message)) => {
                            open = socket.send(Message::binary(message.to_bytes())).is_ok();
                        }
                        None => {}
                    }
                }
                client.on_close();
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}
// #endregion websocket-feed

/// Simulated quotes: a random walk of every price, a few hundred a second.
fn simulate(writer: GridWriter<Quote>) {
    std::thread::spawn(move || {
        let mut rng = fastrand::Rng::new();
        let mut quotes = fresh_quotes(&mut rng);
        writer.replace_all(quotes.iter().cloned().map(Arc::new));
        while !writer.is_closed() {
            let Quote(record) = &mut quotes[rng.usize(..QUOTES)];
            record.last *= 1. + (rng.f64() - 0.5) / 100.;
            record.bid = record.last * 0.999;
            record.ask = record.last * 1.001;
            record.volume += rng.i64(1..500);
            writer.upsert(Quote(*record));
            std::thread::sleep(Duration::from_millis(4));
        }
    });
}

const QUOTES: usize = 200;

fn fresh_quotes(rng: &mut fastrand::Rng) -> Vec<Quote> {
    (0..QUOTES as u32)
        .map(|id| {
            let last = 10. + rng.f64() * 290.;
            Quote(QuoteRecord {
                id,
                last,
                bid: last * 0.999,
                ask: last * 1.001,
                volume: rng.i64(0..100_000),
            })
        })
        .collect()
}

struct Root {
    grid: QuotesGrid,
    by_volume: QuotesGrid,
    writer: GridWriter<Quote>,
    live_feed: bool,
    hold: bool,
    order_held: bool,
    _held: Subscription,
}

impl Root {
    fn reload(&mut self) {
        reload_with_progress(&self.writer, || {
            // A slow server.
            std::thread::sleep(Duration::from_millis(1500));
            fresh_quotes(&mut fastrand::Rng::new())
        });
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        match self.grid.update(cx, export_markdown) {
            Ok(job) => cx
                .spawn(async move |_, _| match job.await {
                    Ok(outcome) => out!("{}", outcome.into_output()),
                    Err(error) => out!("export failed: {error}"),
                })
                .detach(),
            Err(error) => out!("nothing exported: {error}"),
        }
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new("reload")
                            .label("Reload")
                            .small()
                            .disabled(self.live_feed)
                            .on_click(cx.listener(|root, _, _, _| root.reload())),
                    )
                    .child(
                        Toggle::new("hold")
                            .label("Hold order")
                            .small()
                            .checked(self.hold)
                            .on_click(cx.listener(|root, hold: &bool, _, cx| {
                                root.hold = *hold;
                                set_hold_order(&root.grid, *hold, cx);
                            })),
                    )
                    .child(
                        Button::new("refresh-order")
                            .label("Refresh order")
                            .small()
                            .disabled(!self.order_held)
                            .on_click(cx.listener(|root, _, _, cx| refresh_order(&root.grid, cx))),
                    )
                    .child(
                        Button::new("export")
                            .label("Export Markdown")
                            .small()
                            .on_click(cx.listener(|root, _, _, cx| root.export(cx))),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .p_2()
                    .child(
                        div().flex_1().h_full().child(
                            LiveGrid::new(&self.grid)
                                .small()
                                .accessibility_label("Quotes"),
                        ),
                    )
                    .child(
                        div().w(px(360.)).h_full().child(
                            LiveGrid::new(&self.by_volume)
                                .small()
                                .accessibility_label("Quotes by volume"),
                        ),
                    ),
            )
    }
}

fn main() {
    let feed = std::env::var("COOKBOOK_FEED").ok();
    let layout_path = std::env::var_os("COOKBOOK_LAYOUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gpui-kit-grid-cookbook-layout.json"));
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit_grid::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1100.), px(720.)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |_, cx| {
                let (grid, by_volume, writer) = two_grids(layout_path, cx);
                let live_feed = feed.is_some();
                match feed {
                    Some(address) => feed_from_gateway(writer.clone(), address),
                    None => simulate(writer.clone()),
                }
                cx.new(|cx| Root {
                    _held: watch_held_order(&grid, cx),
                    grid,
                    by_volume,
                    writer,
                    live_feed,
                    hold: false,
                    order_held: false,
                })
            })
            .expect("open window");
            cx.activate(true);
        });
}
