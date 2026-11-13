//! `BLOTTER_E2E_LOG=<file>`: the blotter reports what happens in its window,
//! one JSON object per line, for the Linux end-to-end run (`script/e2e`).
//! xdotool drives the window with the mouse and the keyboard; the script
//! reads this file to find where to press and to check what happened.
//! Without the variable nothing here runs and the window is the same.
//!
//! | `type` | Written | Holds |
//! |---|---|---|
//! | `start` | Once | The process id, the seed and the settings |
//! | `event` | On every `LiveGridEvent` | Its kind and fields (a `Click` with its target, button, modifiers and click count; a `GroupChange` or `ExpansionChange` with its target and origin); a `state` line follows |
//! | `state` | After an event (`why: "event"`), and every 50 ms when something other than values, versions and the frame count changed (`why: "poll"`) | Sort, grouping, default expansion, the first lines shown (trades, removed trades fading out, group captions with their row counts, states and hot caption stamps, footers), selection, scoped selection, focus (a caption's group too), held order, menu, progress, row counts, the change paint, the first visible keys and the values of the first visible and the selected rows, the totals and whether their stamps are hot, columns with widths and zones, the quick filter |
//! | `rows` | With `RATE=0` and `CHURN=0`, whenever the store has a new version of at most 500 rows | The values of every trade, for checks of sums |
//! | `paint` | Every 50 ms when the style hooks saw a change painted, or the counters below changed | The changes painted since the last poll, as the delegate's `cell_style` and `aggregate_style` see them after the change paint: updated cells of trades and of the pinned rows with their arrow (`CellStyle::trend`), removed trades fading out, aggregate cells of captions, footers and the totals; the arrows and fading lines of the last frame (`GridStats::icons`, `GridStats::fading_lines`) and the frame counts the record spans |
//! | `focus` | At start-up and whenever it changes, after the frame's paint | Whether the grid has the keyboard focus, and which chips of the group panel |
//! | `geometry` | At start-up and whenever it changes | In window logical pixels: the window, the grid, the group panel with its chips, the header cells by column id, the body, the first data row, the row height, the group captions on screen with their chevrons, the pinned bands, the totals row and the toolbar buttons |
//!
//! The log also fixes the producer's random seed, so every run has the same
//! trades, and quits the blotter through `App::quit` (as `BLOTTER_SECONDS`
//! does, so the layout file is saved) when the file `<log>.quit` appears.
//!
//! The chips of the group panel are measured through gpui-kit's element
//! observation, so the end-to-end build turns on `test-support`
//! (`docker/linux/e2e.sh`); without it the geometry has no chips and the
//! `focus` line none of their focus.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Write as _,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

use gpui_kit::component::Size;
use gpui_kit::{
    App, Bounds, Context, Entity, Focusable as _, IntoElement, Modifiers, ParentElement, Pixels,
    Styled as _, Window, canvas, prelude::FluentBuilder, px,
};
use gpui_kit_grid::{
    AggregateLine, AggregateStyleContext, CellStyle, CellStyleContext, CellValue, ChangeDirection,
    ChangePaint, ColumnZone, DisplayRow, ExpandTarget, GridColumns as _, GridRow as _, GridTarget,
    GroupPath, LayoutPart, LiveGridEvent, LiveGridState, OwnedValue, PinnedEdge, RowChange, RowId,
    SortDirection, Stamp, StoreSnapshot,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Blotter, Root,
    model::{Quote, QuoteColumn},
    value_text,
};

/// The producer's seed while the log is on.
const SEED: u64 = 20_261_104;
/// How often the state is compared with the last one written.
const POLL: Duration = Duration::from_millis(50);
/// Keys listed per state line: visible rows, selection, scoped selection.
const KEYS: usize = 60;
/// Rows whose values a state line lists: the first visible ones, and the
/// scoped selection.
const VALUE_ROWS: usize = 20;
/// The most trades a `rows` line lists: the end-to-end data set.
const LOGGED_ROWS: usize = 500;
/// The window x of a chevron lane from the body's left edge: the cell
/// padding, 16 px per level, then the 20 px lane.
const CHEVRON_PADDING: f32 = 6.;
const CHEVRON_INDENT: f32 = 16.;
const CHEVRON_LANE: f32 = 20.;

/// The log, opened on first use; `None` without `BLOTTER_E2E_LOG`.
fn log() -> Option<&'static Log> {
    static LOG: OnceLock<Option<Log>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("BLOTTER_E2E_LOG")?);
        match File::create(&path) {
            Ok(file) => Some(Log {
                started: Instant::now(),
                quit: PathBuf::from(format!("{}.quit", path.display())),
                inner: Mutex::new(Inner {
                    file: Some(file),
                    ..Inner::default()
                }),
            }),
            Err(error) => {
                err!("BLOTTER_E2E_LOG={}: not written: {error}", path.display());
                None
            }
        }
    })
    .as_ref()
}

/// The producer's random seed: fixed while the log is on.
pub fn seed() -> Option<u64> {
    log().map(|_| SEED)
}

/// Starts the log for `grid`: the `start` line, an `event` and a `state`
/// line per grid event, the polled `state` lines and the quit file.
pub fn install(grid: &Entity<LiveGridState<Blotter>>, cx: &mut Context<Root>) {
    let Some(log) = log() else {
        return;
    };
    let settings: Map<String, Value> = [
        "ROWS",
        "RATE",
        "CHURN",
        "RELOAD_MS",
        "BLOTTER_REORDER",
        "BLOTTER_COPY",
        "BLOTTER_LAYOUT",
        "BLOTTER_GROUP",
        "BLOTTER_GROUP_PANEL",
        "BLOTTER_TOTALS",
        "BLOTTER_FOOTERS",
        "BLOTTER_PAINT",
        "BLOTTER_FADE",
        "BLOTTER_EXPAND",
    ]
    .into_iter()
    .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?.into())))
    .collect();
    log.write(
        "start",
        json!({ "pid": std::process::id(), "seed": SEED, "env": settings }),
    );
    // A subscriber runs after the grid's update: reading it is safe.
    cx.subscribe(grid, move |_, grid, event, cx| {
        log.write("event", event_line(event));
        log.write_state("event", grid.read(cx));
    })
    .detach();
    let grid = grid.downgrade();
    cx.spawn(async move |_, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let quit = cx.update(|cx| {
                if let Some(grid) = grid.upgrade() {
                    log.poll(grid.read(cx));
                    log.write_paint(grid.read(cx));
                }
                log.quit.exists()
            });
            if quit {
                log.write("quit", json!({}));
                cx.update(|cx| cx.quit());
                break;
            }
        }
    })
    .detach();
}

/// `element`, the grid's container, measured for the `geometry` line;
/// `group_panel` tells whether the grid shows its group panel. Its last
/// child reads the keyboard focus for the `focus` line when it paints,
/// after the grid and its chips painted theirs.
pub fn measure_grid<E: ParentElement>(
    element: E,
    grid: &Entity<LiveGridState<Blotter>>,
    size: Size,
    group_panel: bool,
) -> E {
    let Some(log) = log() else {
        return element;
    };
    let measured = grid.clone();
    let focused = grid.clone();
    element
        .child(measure(move |bounds, window, cx| {
            let row_height = size.table_row_height();
            log.geometry(bounds, measured.read(cx), row_height, group_panel, window);
        }))
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, cx| {
                    let grid = focused.focus_handle(cx).is_focused(window);
                    log.focus(grid, chips_focus(focused.read(cx), window));
                },
            )
            .absolute()
            .size_0(),
        )
}

/// Paint of a data or pinned cell, for the `paint` line: the change the
/// grid's change paint gave it (`style` after it) while it is hot.
/// Inserted cells are left out (every row of a load is one).
pub fn painted_cell(cell: &CellStyleContext<'_, Quote>, style: &CellStyle) {
    if log().is_none() {
        return;
    }
    let Some(change) = cell.change() else {
        return;
    };
    if change.change() == RowChange::Inserted {
        return;
    }
    let painted = PaintedCell {
        pinned: cell.pinned_edge(),
        key: *cell.row().key(),
        column: cell.column(),
        change: change.change(),
        trend: style.trend(),
    };
    PAINTED.with_borrow_mut(|record| record.push_cell(painted));
}

/// Paint of an aggregate cell, for the `paint` line: on a group caption, a
/// footer or the totals row, while its stamp is hot.
pub fn painted_aggregate(cell: &AggregateStyleContext<'_, Quote>, style: &CellStyle) {
    if log().is_none() {
        return;
    }
    let Some(change) = cell.change() else {
        return;
    };
    // A path clones in O(1): no allocation in the hook.
    let place = match cell.line() {
        AggregateLine::Caption(group) => Place::Caption(group.path().clone()),
        AggregateLine::Footer(group) => Place::Footer(group.path().clone()),
        AggregateLine::Totals(_) => Place::Totals,
        _ => return,
    };
    let painted = PaintedAggregate {
        place,
        column: cell.column(),
        change: change.change(),
        trend: style.trend(),
    };
    PAINTED.with_borrow_mut(|record| record.push_aggregate(painted));
}

/// Data cells kept per `paint` line, and aggregate cells.
const PAINTED_CELLS: usize = 4096;
const PAINTED_AGGREGATES: usize = 512;

thread_local! {
    /// What the style hooks saw since the last poll. The hooks run in
    /// prepaint and the poll on the UI thread: no lock. Fixed capacity:
    /// the hooks never allocate, what does not fit is counted.
    static PAINTED: RefCell<Painted> = RefCell::new(Painted {
        cells: Vec::with_capacity(PAINTED_CELLS),
        aggregates: Vec::with_capacity(PAINTED_AGGREGATES),
        dropped: 0,
    });
}

#[derive(Default)]
struct Painted {
    cells: Vec<PaintedCell>,
    aggregates: Vec<PaintedAggregate>,
    dropped: usize,
}

impl Painted {
    fn push_cell(&mut self, cell: PaintedCell) {
        if self.cells.len() < PAINTED_CELLS {
            self.cells.push(cell);
        } else {
            self.dropped += 1;
        }
    }

    fn push_aggregate(&mut self, cell: PaintedAggregate) {
        if self.aggregates.len() < PAINTED_AGGREGATES {
            self.aggregates.push(cell);
        } else {
            self.dropped += 1;
        }
    }
}

#[derive(Clone, Copy)]
struct PaintedCell {
    pinned: Option<PinnedEdge>,
    key: u64,
    column: QuoteColumn,
    change: RowChange,
    trend: Option<ChangeDirection>,
}

/// The line of a painted aggregate cell.
enum Place {
    Caption(GroupPath),
    Footer(GroupPath),
    Totals,
}

struct PaintedAggregate {
    place: Place,
    column: QuoteColumn,
    change: RowChange,
    trend: Option<ChangeDirection>,
}

/// Records where a toolbar control is, for the `geometry` line.
pub trait RecordBounds: ParentElement + FluentBuilder + Sized {
    /// This control, measured as `name` while the log is on.
    fn e2e_bounds(self, name: &'static str) -> Self {
        self.when_some(log(), |this, log| {
            this.child(measure(move |bounds, _, _| {
                log.inner.lock().controls.insert(name, bounds);
            }))
        })
    }
}

impl<E: ParentElement + FluentBuilder> RecordBounds for E {}

/// A child that reports its parent's bounds in prepaint. Pinned to the
/// parent's top left corner: an absolute child without insets sits at its
/// static position, after the parent's other children (as with
/// `ElementExt::on_prepaint`, which measured the grid 840 px too low).
fn measure(
    report: impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    canvas(report, |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

struct Log {
    started: Instant,
    /// The file whose appearance quits the blotter.
    quit: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// `None` after a write failed: the blotter runs on without the log.
    file: Option<File>,
    seq: u64,
    controls: BTreeMap<&'static str, Bounds<Pixels>>,
    /// The last `geometry` line, and the last state without its values.
    geometry: Option<Value>,
    state: Option<Value>,
    /// The store version of the last `rows` line.
    rows_version: Option<u64>,
    /// The counters of the last `paint` line, and the frame count of the
    /// last poll: the record of the next line starts after it.
    paint_counters: Option<(usize, usize)>,
    painted_since: u64,
    /// The last `focus` line.
    focus: Option<Value>,
}

impl Log {
    /// Appends one line: `fields` with `type`, a sequence number and the
    /// milliseconds since start.
    fn write(&self, kind: &str, mut fields: Value) {
        let mut inner = self.inner.lock();
        inner.seq += 1;
        if let Value::Object(map) = &mut fields {
            map.insert("type".into(), kind.into());
            map.insert("seq".into(), inner.seq.into());
            map.insert(
                "t".into(),
                json!((self.started.elapsed().as_secs_f64() * 1e4).round() / 10.),
            );
        }
        let Some(file) = inner.file.as_mut() else {
            return;
        };
        let mut line = fields.to_string();
        line.push('\n');
        if let Err(error) = file.write_all(line.as_bytes()) {
            err!("BLOTTER_E2E_LOG: not written: {error}");
            inner.file = None;
        }
    }

    fn write_state(&self, why: &str, grid: &LiveGridState<Blotter>) {
        let state = state_line(grid);
        self.inner.lock().state = Some(state.clone());
        self.write_with_values(why, state, grid);
    }

    /// A `state` line if anything but the values changed since the last.
    fn poll(&self, grid: &LiveGridState<Blotter>) {
        let state = state_line(grid);
        {
            let mut inner = self.inner.lock();
            if inner.state.as_ref() == Some(&state) {
                return;
            }
            inner.state = Some(state.clone());
        }
        self.write_with_values("poll", state, grid);
    }

    fn write_with_values(&self, why: &str, mut state: Value, grid: &LiveGridState<Blotter>) {
        if let Value::Object(map) = &mut state {
            map.insert("why".into(), why.into());
            let columns: Vec<QuoteColumn> = grid.visible_columns().collect();
            let snapshot = grid.snapshot();
            let values = |keys: &mut dyn Iterator<Item = u64>| -> Vec<Value> {
                keys.take(VALUE_ROWS)
                    .filter_map(|key| snapshot.get(&key))
                    .map(|row| row_values(row, &columns))
                    .collect()
            };
            map.insert(
                "top_values".into(),
                values(&mut grid.visible_keys().into_iter()).into(),
            );
            map.insert(
                "scoped_values".into(),
                values(&mut grid.scoped_selection().keys().iter().copied()).into(),
            );
            map.insert("totals".into(), totals_json(grid));
            map.insert("store_version".into(), snapshot.version().into());
            map.insert(
                "projection_version".into(),
                grid.projection().store_version().into(),
            );
            map.insert("frames".into(), grid.stats().frames().into());
        }
        self.write("state", state);
        self.write_rows(grid);
    }

    /// A `rows` line with the values of every trade, when the store has a
    /// new version of at most `LOGGED_ROWS` and no stream writes (`RATE=0`,
    /// `CHURN=0`): checks of the totals and of group sums read it.
    fn write_rows(&self, grid: &LiveGridState<Blotter>) {
        let snapshot = grid.snapshot();
        let version = snapshot.version();
        let still = |name: &str| std::env::var(name).is_ok_and(|value| value == "0");
        if snapshot.is_empty() || snapshot.len() > LOGGED_ROWS || !still("RATE") || !still("CHURN")
        {
            return;
        }
        {
            let mut inner = self.inner.lock();
            if inner.rows_version == Some(version) {
                return;
            }
            inner.rows_version = Some(version);
        }
        let columns = QuoteColumn::all();
        let rows: Vec<Value> = snapshot
            .iter()
            .map(|(_, row)| row_values(row, columns))
            .collect();
        let ids: Vec<&str> = columns.iter().map(|column| column.id()).collect();
        self.write(
            "rows",
            json!({ "store_version": version, "columns": ids, "rows": rows }),
        );
    }

    /// A `paint` line with what the style hooks saw since the last poll,
    /// if they saw anything or the frame's counters changed. Repeats of a
    /// cell over the frames since are written once.
    fn write_paint(&self, grid: &LiveGridState<Blotter>) {
        type Cell = (
            Option<&'static str>,
            u64,
            &'static str,
            &'static str,
            Option<&'static str>,
        );
        let (cells, aggregates, dropped) = PAINTED.with_borrow_mut(|record| {
            let cells: BTreeSet<Cell> = record
                .cells
                .drain(..)
                .map(|cell| {
                    (
                        cell.pinned.map(edge_name),
                        cell.key,
                        cell.column.id(),
                        change_name(cell.change),
                        cell.trend.map(trend_name),
                    )
                })
                .collect();
            let mut aggregates: Vec<Value> = Vec::new();
            for cell in record.aggregates.drain(..) {
                let (line, path) = match &cell.place {
                    Place::Caption(path) => ("caption", path_json(path)),
                    Place::Footer(path) => ("footer", path_json(path)),
                    Place::Totals => ("totals", Value::Null),
                };
                let entry = json!([
                    line,
                    path,
                    cell.column.id(),
                    change_name(cell.change),
                    cell.trend.map(trend_name),
                ]);
                if !aggregates.contains(&entry) {
                    aggregates.push(entry);
                }
            }
            (cells, aggregates, std::mem::take(&mut record.dropped))
        });
        let stats = grid.stats();
        let counters = (stats.icons(), stats.fading_lines());
        let since = {
            let mut inner = self.inner.lock();
            let since = std::mem::replace(&mut inner.painted_since, stats.frames());
            let same = inner.paint_counters.replace(counters) == Some(counters);
            if same && cells.is_empty() && aggregates.is_empty() && dropped == 0 {
                return;
            }
            since
        };
        let (pinned, cells): (Vec<_>, Vec<_>) =
            cells.into_iter().partition(|(edge, ..)| edge.is_some());
        let (removed, cells): (Vec<_>, Vec<_>) = cells
            .into_iter()
            .partition(|(_, _, _, change, _)| *change == "removed");
        let ghosts: BTreeSet<u64> = removed.into_iter().map(|(_, key, ..)| key).collect();
        self.write(
            "paint",
            json!({
                "cells": cells
                    .into_iter()
                    .map(|(_, key, column, change, trend)| json!([key, column, change, trend]))
                    .collect::<Vec<_>>(),
                "pinned": pinned
                    .into_iter()
                    .map(|(edge, key, column, change, trend)| {
                        json!([edge, key, column, change, trend])
                    })
                    .collect::<Vec<_>>(),
                "ghosts": ghosts,
                "aggregates": aggregates,
                "dropped": dropped,
                "icons": counters.0,
                "fading_lines": counters.1,
                "since_frames": since,
                "frames": stats.frames(),
            }),
        );
    }

    /// A `focus` line if the keyboard focus moved between the grid and the
    /// chips of its group panel.
    fn focus(&self, grid: bool, chips: Value) {
        let line = json!({ "grid": grid, "chips": chips });
        {
            let mut inner = self.inner.lock();
            if inner.focus.as_ref() == Some(&line) {
                return;
            }
            inner.focus = Some(line.clone());
        }
        self.write("focus", line);
    }

    /// A `geometry` line if anything moved. `grid_bounds` are the bounds of
    /// the grid's container; the group panel (a row high, above the header,
    /// when shown), the header and zones are laid out as the grid does
    /// (`ColumnLayout::geometry`), with no horizontal scroll and no column
    /// bands (the blotter's schema has none: one header line). Group
    /// captions are placed by their display index in the viewport, the body
    /// scrolled to a whole row; their chevrons by depth, as the grid paints
    /// them.
    fn geometry(
        &self,
        grid_bounds: Bounds<Pixels>,
        grid: &LiveGridState<Blotter>,
        row_height: Pixels,
        group_panel: bool,
        window: &Window,
    ) {
        let panel_height = if group_panel { row_height } else { px(0.) };
        let origin = grid_bounds.origin + gpui_kit::point(px(0.), panel_height);
        let width = grid_bounds.size.width;
        let header_height = row_height;
        let columns: Vec<(QuoteColumn, ColumnZone, Pixels)> = grid
            .visible_columns()
            .map(|column| (column, grid.column_zone(column), grid.column_width(column)))
            .collect();
        let zone_width = |zone: ColumnZone| {
            columns
                .iter()
                .filter(|(_, in_zone, _)| *in_zone == zone)
                .fold(px(0.), |sum, (_, _, width)| sum + *width)
        };
        let left = zone_width(ColumnZone::FrozenLeft).min(width);
        let right = zone_width(ColumnZone::FrozenRight).min(width - left);
        let scrolling_end = width - right;
        let mut next_x = [px(0.), left, width - right];
        let mut cells = Map::new();
        for (column, zone, column_width) in &columns {
            let slot = zone_slot(*zone);
            let x = next_x[slot];
            next_x[slot] += *column_width;
            cells.insert(
                column.id().into(),
                json!({
                    "x": f32::from(origin.x + x),
                    "y": f32::from(origin.y),
                    "w": f32::from(*column_width),
                    "h": f32::from(header_height),
                    "zone": zone_name(*zone),
                    "clipped": *zone == ColumnZone::Scrolling && x + *column_width > scrolling_end,
                }),
            );
        }
        let snapshot = grid.snapshot();
        let top = snapshot.pinned_rows(PinnedEdge::Top).len();
        let bottom = snapshot.pinned_rows(PinnedEdge::Bottom).len();
        // The totals row is the outermost row of its band, at the body's edge.
        let totals = grid.totals();
        let totals_top = usize::from(totals == Some(PinnedEdge::Top));
        let totals_bottom = usize::from(totals == Some(PinnedEdge::Bottom));
        let body_y = origin.y + header_height;
        let grid_bottom = grid_bounds.origin.y + grid_bounds.size.height;
        let rows_top = body_y + row_height * (top + totals_top) as f32;
        let visible = grid.visible_rows();
        let projection = grid.projection();
        let groups = projection.groups();
        let captions: Vec<Value> = visible
            .clone()
            .filter_map(|ix| match projection.line(ix)? {
                DisplayRow::GroupCaption { group, depth } => {
                    let path = groups.get(group)?.path();
                    let y = rows_top + row_height * (ix - visible.start) as f32;
                    let lane = CHEVRON_PADDING + CHEVRON_INDENT * f32::from(depth);
                    Some(json!({
                        "path": path_json(path),
                        "ix": ix,
                        "y": f32::from(y),
                        "h": f32::from(row_height),
                        "chevron": { "x": f32::from(origin.x) + lane, "w": CHEVRON_LANE },
                    }))
                }
                _ => None,
            })
            .collect();
        let panel = group_panel.then(|| {
            json!({
                "x": f32::from(grid_bounds.origin.x),
                "y": f32::from(grid_bounds.origin.y),
                "w": f32::from(width),
                "h": f32::from(panel_height),
                "chips": chips_json(grid, window),
            })
        });
        let mut inner = self.inner.lock();
        let controls: Map<String, Value> = inner
            .controls
            .iter()
            .map(|(name, bounds)| ((*name).to_owned(), rect(*bounds)))
            .collect();
        let line = json!({
            "scale": window.scale_factor(),
            "window": rect(window.bounds()),
            "viewport": { "w": f32::from(window.viewport_size().width), "h": f32::from(window.viewport_size().height) },
            "grid": rect(grid_bounds),
            "header": { "y": f32::from(origin.y), "h": f32::from(header_height), "cells": cells },
            "body": { "x": f32::from(origin.x), "y": f32::from(body_y), "w": f32::from(width), "h": f32::from(grid_bottom - body_y) },
            "row_height": f32::from(row_height),
            "rows_top": f32::from(rows_top),
            "pinned": {
                "top": { "rows": top, "y": f32::from(body_y + row_height * totals_top as f32) },
                "bottom": {
                    "rows": bottom,
                    "y": f32::from(grid_bottom - row_height * (bottom + totals_bottom) as f32),
                },
            },
            "totals": totals.map(|edge| json!({
                "edge": edge_name(edge),
                "y": f32::from(match edge {
                    PinnedEdge::Top => body_y,
                    PinnedEdge::Bottom => grid_bottom - row_height,
                }),
            })),
            "group_panel": panel,
            "captions": captions,
            "controls": controls,
        });
        if inner.geometry.as_ref() == Some(&line) {
            return;
        }
        inner.geometry = Some(line.clone());
        drop(inner);
        self.write("geometry", line);
    }
}

fn rect(bounds: Bounds<Pixels>) -> Value {
    json!({
        "x": f32::from(bounds.origin.x),
        "y": f32::from(bounds.origin.y),
        "w": f32::from(bounds.size.width),
        "h": f32::from(bounds.size.height),
    })
}

/// The chips of the group panel by level id, each with its remove button,
/// and `end`: the right edge of the last chip, after which a header dropped
/// on the panel lands last. Read from gpui-kit's element observation
/// (`test-support`); `null` without it.
#[cfg(feature = "test-support")]
fn chips_json(grid: &LiveGridState<Blotter>, window: &Window) -> Value {
    use std::sync::Arc;

    use gpui_kit::{ElementId, SharedString, test::TestWindowExt as _};

    // The grid names chip parts by level id.
    let find = |part: &'static str, level: &str| {
        window
            .try_find(ElementId::NamedChild(
                Arc::new(ElementId::Name(part.into())),
                SharedString::from(level.to_owned()),
            ))
            .map(|element| element.bounds())
    };
    let mut chips = Map::new();
    let mut end = None;
    for level in grid.group_by() {
        let id = level.id();
        let Some(chip) = find("live-grid-group-chip", id.as_str()) else {
            continue;
        };
        let remove = find("live-grid-group-chip-remove", id.as_str());
        end = Some(remove.unwrap_or(chip).right());
        chips.insert(
            id.as_str().to_owned(),
            json!({ "chip": rect(chip), "remove": remove.map(rect) }),
        );
    }
    json!({ "levels": chips, "end": end.map(f32::from) })
}

#[cfg(not(feature = "test-support"))]
fn chips_json(_: &LiveGridState<Blotter>, _: &Window) -> Value {
    Value::Null
}

/// Which chips of the group panel have the keyboard focus, by level id,
/// read when the chips painted (element observation sets it in paint);
/// `null` without `test-support`.
#[cfg(feature = "test-support")]
fn chips_focus(grid: &LiveGridState<Blotter>, window: &Window) -> Value {
    use std::sync::Arc;

    use gpui_kit::{ElementId, SharedString, test::TestWindowExt as _};

    let chips: Map<String, Value> = grid
        .group_by()
        .iter()
        .filter_map(|level| {
            let id = level.id();
            let chip = window.try_find(ElementId::NamedChild(
                Arc::new(ElementId::Name("live-grid-group-chip".into())),
                SharedString::from(id.as_str().to_owned()),
            ))?;
            Some((
                id.as_str().to_owned(),
                chip.focused().unwrap_or(false).into(),
            ))
        })
        .collect();
    chips.into()
}

#[cfg(not(feature = "test-support"))]
fn chips_focus(_: &LiveGridState<Blotter>, _: &Window) -> Value {
    Value::Null
}

/// `"updated"`, `"removed"`, `"inserted"`.
fn change_name(change: RowChange) -> &'static str {
    match change {
        RowChange::Inserted => "inserted",
        RowChange::Updated => "updated",
        RowChange::Removed => "removed",
        _ => "other",
    }
}

fn trend_name(trend: ChangeDirection) -> &'static str {
    match trend {
        ChangeDirection::Up => "up",
        ChangeDirection::Down => "down",
    }
}

/// A hot stamp as `"updated"` or `"inserted"`, with its direction:
/// `"updated up"`.
fn stamp_name(stamp: Stamp) -> String {
    match stamp.direction() {
        Some(direction) => format!("{} {}", change_name(stamp.change()), trend_name(direction)),
        None => change_name(stamp.change()).to_owned(),
    }
}

fn edge_name(edge: PinnedEdge) -> &'static str {
    match edge {
        PinnedEdge::Top => "top",
        PinnedEdge::Bottom => "bottom",
    }
}

/// A group path as its key values, the top level first: `["INS042", "Buy"]`.
fn path_json(path: &GroupPath) -> Value {
    path.levels()
        .iter()
        .map(|(_, value)| Value::from(value_text(value)))
        .collect()
}

/// A value as `RawCells` copies it: an aggregate of the totals row.
fn owned_text(value: &OwnedValue) -> String {
    match value {
        OwnedValue::Float(value) if !value.is_finite() => String::new(),
        value => value_text(value),
    }
}

/// The totals by column id, as the projection on screen has them, and the
/// trades they count; `null` without aggregates.
fn totals_json(grid: &LiveGridState<Blotter>) -> Value {
    let totals = grid.projection().totals();
    let values: Map<String, Value> = grid
        .aggregates()
        .iter()
        .filter_map(|(column, _)| {
            let value = totals.get(column.ix())?;
            Some((column.id().to_owned(), owned_text(value).into()))
        })
        .collect();
    if values.is_empty() {
        return Value::Null;
    }
    let hot = grid.projection().totals_stamps().is_some();
    json!({ "rows": totals.row_count(), "values": values, "hot": hot })
}

fn zone_slot(zone: ColumnZone) -> usize {
    match zone {
        ColumnZone::FrozenLeft => 0,
        ColumnZone::Scrolling => 1,
        ColumnZone::FrozenRight => 2,
    }
}

fn zone_name(zone: ColumnZone) -> &'static str {
    match zone {
        ColumnZone::FrozenLeft => "left",
        ColumnZone::Scrolling => "scrolling",
        ColumnZone::FrozenRight => "right",
    }
}

fn direction_name(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "asc",
        SortDirection::Descending => "desc",
    }
}

fn sort_json(sort: &[(QuoteColumn, SortDirection)]) -> Value {
    sort.iter()
        .map(|(column, direction)| json!([column.id(), direction_name(*direction)]))
        .collect()
}

/// The values of `columns` in `row`, written as `RawCells` copies them.
fn row_values(row: &Quote, columns: &[QuoteColumn]) -> Value {
    let mut values = vec![Value::from(*row.key())];
    values.extend(columns.iter().map(|column| {
        Value::from(match row.value(*column) {
            CellValue::Bool(value) => if value { "TRUE" } else { "FALSE" }.to_owned(),
            CellValue::Int(value) => value.to_string(),
            CellValue::Float(value) if value.is_finite() => value.to_string(),
            CellValue::Text(text) => text.to_owned(),
            _ => String::new(),
        })
    }));
    values.into()
}

/// Up to `KEYS` keys, ascending, and how many there are.
fn key_set<'a>(keys: impl Iterator<Item = &'a u64>) -> (usize, Vec<u64>) {
    let mut keys: Vec<u64> = keys.copied().collect();
    keys.sort_unstable();
    let len = keys.len();
    keys.truncate(KEYS);
    (len, keys)
}

/// What the grid shows, but the values (they tick under the stream).
fn state_line(grid: &LiveGridState<Blotter>) -> Value {
    let snapshot = grid.snapshot();
    let projection = grid.projection();
    let key_at = |ix: usize| {
        projection
            .row(ix)
            .and_then(|id| snapshot.row(id))
            .map(|row| *row.key())
    };
    let display_ix = |key: &u64| {
        snapshot
            .row_id(key)
            .and_then(|id| projection.display_ix(id))
    };
    let (selected_len, selected) = key_set(grid.selection().iter());
    let scoped = grid.scoped_selection();
    let focus = grid.focused_cell().map(|cell| {
        json!({
            "key": cell.row(),
            "group": cell.group().map(path_json),
            "column": cell.column().id(),
            "ix": cell.row().and_then(display_ix),
        })
    });
    let group_by: Vec<Value> = grid
        .group_by()
        .iter()
        .map(|level| {
            json!({
                "column": level.id().as_str(),
                "direction": direction_name(level.direction()),
            })
        })
        .collect();
    let visible_rows = grid.visible_rows();
    let columns: Vec<Value> = grid
        .visible_columns()
        .map(|column| {
            json!({
                "id": column.id(),
                "width": f32::from(grid.column_width(column)),
                "zone": zone_name(grid.column_zone(column)),
            })
        })
        .collect();
    let hidden: Vec<&str> = grid
        .columns()
        .filter(|column| !grid.is_column_visible(*column))
        .map(|column| column.id())
        .collect();
    json!({
        "mode": format!("{:?}", grid.selection_mode()),
        "sort": sort_json(grid.sort()),
        "group_by": group_by,
        "expand_depth": serde_json::to_value(grid.expand_depth()).unwrap_or(Value::Null),
        "lines": lines_json(grid),
        "groups": groups_json(grid),
        "row_count": projection.row_count(),
        "change_paint": match grid.change_paint() {
            ChangePaint::Uniform(_) => "uniform",
            ChangePaint::Directional(_) => "directional",
            _ => "custom",
        },
        "filter": grid.quick_filter().as_ref(),
        "selection": { "len": selected_len, "keys": selected },
        "scoped": { "len": scoped.len(), "keys": &scoped.keys()[..scoped.len().min(KEYS)] },
        "focus": focus,
        "held": grid.is_order_held(),
        "menu": grid.is_menu_open(),
        "progress": grid.progress().is_some(),
        "rows": projection.len(),
        "store_rows": snapshot.len(),
        "first_key": key_at(0),
        "last_key": projection.len().checked_sub(1).and_then(key_at),
        "visible_rows": [visible_rows.start, visible_rows.end],
        "visible": grid.visible_keys().into_iter().take(KEYS).collect::<Vec<_>>(),
        "pinned": {
            "top": grid.pinned_keys(PinnedEdge::Top),
            "bottom": grid.pinned_keys(PinnedEdge::Bottom),
        },
        "current_column": grid.current_column().map(|column| column.id()),
        "columns": columns,
        "hidden": hidden,
    })
}

/// The groups of the shown projection: how many, and the trades of the
/// top-level ones together (every trade of a grouped view is in one).
fn groups_json(grid: &LiveGridState<Blotter>) -> Value {
    let groups = grid.projection().groups();
    let top_rows: usize = groups
        .iter()
        .filter(|group| group.depth() == 0)
        .map(|group| group.row_count())
        .sum();
    json!({ "count": groups.len(), "top_rows": top_rows })
}

/// The first `KEYS` lines of the shown order: `{"kind": "data", "key": 42}`,
/// `{"kind": "caption", "path": ["INS042"], "rows": 3, "expanded": true,
/// "stamp": "updated", "hot": true}` (the rows of the group, collapsed ones
/// too, its state in the projection, its own stamp and whether a stamp of
/// its aggregates is hot), `{"kind": "footer", "path": [...]}`. A trade
/// removed since the projection was built reads `"key": null`, or its key
/// and `"fading": true` while it fades out (`BLOTTER_FADE`).
fn lines_json(grid: &LiveGridState<Blotter>) -> Vec<Value> {
    let projection = grid.projection();
    let snapshot = grid.snapshot();
    let groups = projection.groups();
    (0..projection.len().min(KEYS))
        .filter_map(|ix| {
            Some(match projection.line(ix)? {
                DisplayRow::Data { row, .. } => data_line(&snapshot, row),
                DisplayRow::GroupCaption { group, .. } => {
                    let stamp = projection.caption_stamp(group).map(stamp_name);
                    let hot = projection.group_stamps(group).is_some();
                    let group = groups.get(group)?;
                    json!({
                        "kind": "caption",
                        "path": path_json(group.path()),
                        "rows": group.row_count(),
                        "expanded": group.is_expanded(),
                        "stamp": stamp,
                        "hot": hot,
                    })
                }
                DisplayRow::GroupFooter { group, .. } => json!({
                    "kind": "footer",
                    "path": path_json(groups.get(group)?.path()),
                }),
                _ => json!({ "kind": "other" }),
            })
        })
        .collect()
}

/// A trade's line: its key; a removed one's `null`, or while it fades
/// out its key and `"fading": true`.
fn data_line(snapshot: &StoreSnapshot<Quote>, row: RowId) -> Value {
    match (snapshot.row(row), snapshot.fading_row(row)) {
        (Some(live), _) => json!({ "kind": "data", "key": *live.key() }),
        (None, Some((fading, _))) => {
            json!({ "kind": "data", "key": *fading.key(), "fading": true })
        }
        (None, None) => json!({ "kind": "data", "key": null }),
    }
}

/// `{"kind": "cell", "key": 42, "column": "bid"}`; a pinned target has its
/// band's `edge`, a header target only `column`, an empty one only `kind`;
/// a group caption or footer has its group's `path`, the totals row its
/// `edge`.
fn target_json(target: &GridTarget<u64, QuoteColumn>) -> Value {
    match target {
        GridTarget::Cell { key, column } => {
            json!({ "kind": "cell", "key": key, "column": column.id() })
        }
        GridTarget::Pinned { edge, key, column } => json!({
            "kind": "pinned",
            "edge": edge_name(*edge),
            "key": key,
            "column": column.id(),
        }),
        GridTarget::Header { column } => json!({ "kind": "header", "column": column.id() }),
        GridTarget::GroupCaption { group, column } => json!({
            "kind": "group_caption",
            "path": path_json(group),
            "column": column.map(|column| column.id()),
        }),
        GridTarget::GroupFooter { group, column } => json!({
            "kind": "group_footer",
            "path": path_json(group),
            "column": column.id(),
        }),
        GridTarget::Totals { edge, column } => json!({
            "kind": "totals",
            "edge": edge_name(*edge),
            "column": column.id(),
        }),
        GridTarget::Empty => json!({ "kind": "empty" }),
        // A target this example does not know yet.
        other => json!({ "kind": format!("{other:?}") }),
    }
}

/// The modifiers held, by GPUI's names: `["control", "shift"]`.
fn modifier_names(modifiers: &Modifiers) -> Vec<&'static str> {
    [
        (modifiers.control, "control"),
        (modifiers.alt, "alt"),
        (modifiers.shift, "shift"),
        (modifiers.platform, "platform"),
        (modifiers.function, "function"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .collect()
}

fn event_line(event: &LiveGridEvent<u64, QuoteColumn>) -> Value {
    match event {
        LiveGridEvent::SelectionChange { selection, origin } => {
            let (len, keys) = key_set(selection.iter());
            json!({ "event": "SelectionChange", "origin": format!("{origin:?}"), "len": len, "keys": keys })
        }
        LiveGridEvent::FocusChange { cell, origin } => json!({
            "event": "FocusChange",
            "origin": format!("{origin:?}"),
            "key": cell.as_ref().and_then(|cell| cell.row().copied()),
            "group": cell.as_ref().and_then(|cell| cell.group()).map(path_json),
            "column": cell.as_ref().map(|cell| cell.column().id()),
        }),
        LiveGridEvent::GroupChange { origin } => json!({
            "event": "GroupChange",
            "origin": format!("{origin:?}"),
        }),
        LiveGridEvent::ExpansionChange { target, origin } => json!({
            "event": "ExpansionChange",
            "target": match target {
                Some(ExpandTarget::Group(path)) => json!({ "group": path_json(path) }),
                Some(ExpandTarget::Row(key)) => json!({ "row": key }),
                Some(_) => json!({ "other": true }),
                None => Value::Null,
            },
            "origin": format!("{origin:?}"),
        }),
        LiveGridEvent::Confirm { keys, secondary } => json!({
            "event": "Confirm",
            "keys": &keys[..],
            "secondary": secondary,
        }),
        LiveGridEvent::ProjectionChange {
            row_count,
            store_version,
        } => json!({
            "event": "ProjectionChange",
            "row_count": row_count,
            "store_version": store_version,
        }),
        LiveGridEvent::OrderHeldChange { held } => {
            json!({ "event": "OrderHeldChange", "held": held })
        }
        LiveGridEvent::LayoutChange {
            parts,
            columns,
            origin,
        } => {
            let parts: Vec<String> = [
                LayoutPart::Width,
                LayoutPart::Order,
                LayoutPart::Visibility,
                LayoutPart::Zone,
            ]
            .into_iter()
            .filter(|part| parts.contains(*part))
            .map(|part| format!("{part:?}"))
            .collect();
            let columns: Vec<&str> = QuoteColumn::all()
                .iter()
                .filter(|column| columns.contains(column.ix()))
                .map(|column| column.id())
                .collect();
            json!({
                "event": "LayoutChange",
                "parts": parts,
                "columns": columns,
                "origin": format!("{origin:?}"),
            })
        }
        LiveGridEvent::SortChange { sort, origin } => json!({
            "event": "SortChange",
            "sort": sort_json(sort),
            "origin": format!("{origin:?}"),
        }),
        LiveGridEvent::Click {
            target,
            button,
            modifiers,
            click_count,
        } => json!({
            "event": "Click",
            "target": target_json(target),
            "button": format!("{button:?}"),
            "modifiers": modifier_names(modifiers),
            "click_count": click_count,
        }),
        // An event this example does not know yet: its name and fields.
        other => {
            let debug = format!("{other:?}");
            let name: String = debug
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            json!({ "event": name, "debug": debug })
        }
    }
}
