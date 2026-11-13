//! `PORTFOLIO_E2E_LOG=<file>`: the portfolio reports what happens in its
//! window, one JSON object per line, for the Linux end-to-end run
//! (`script/e2e`), as `blotter`'s `BLOTTER_E2E_LOG` does. xdotool drives
//! the window; the script reads this file to find where to press and to
//! check what happened. Without the variable nothing here runs and the
//! window is the same.
//!
//! | `type` | Written | Holds |
//! |---|---|---|
//! | `start` | Once | The process id, the seed and the settings |
//! | `event` | On every `LiveGridEvent` | Its kind and fields (an `ExpansionChange` with its row, a `Click` with its target); a `state` line follows |
//! | `state` | After an event (`why: "event"`), and every 50 ms when something other than values, versions and the frame count changed (`why: "poll"`) | The default expansion, the quick filter, the first lines shown (key, depth, name, level, and for a row with children their count and whether they show), selection, scoped selection, focus, menu, row counts, the visible keys; with the values: the totals over the positions |
//! | `rows` | With `RATE=0`, `CHURN=0` and `PORTFOLIO_MOVES=0`, whenever the store has a new version of at most 500 rows | Every row: key, parent, level, name, instrument, P&L, delta |
//! | `geometry` | At start-up and whenever it changes | In window logical pixels: the window, the grid, the header cells by column id, the body, the first row, the row height, the tree rows with children on screen with their chevrons, the totals row and the toolbar buttons |
//!
//! The log also fixes the producer's random seed, so every run has the same
//! positions, and quits the portfolio through `App::quit` when the file
//! `<log>.quit` appears.

use std::{
    collections::BTreeMap,
    fs::File,
    io::Write as _,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};

use gpui_kit::component::Size;
use gpui_kit::{
    App, Bounds, Context, Entity, IntoElement, ParentElement, Pixels, Styled as _, Window, canvas,
    prelude::FluentBuilder, px,
};
use gpui_kit_grid::{
    CellValue, ColumnZone, ExpandTarget, GridColumns as _, GridRow as _, GridTarget, LiveGridEvent,
    LiveGridState, OwnedValue, PinnedEdge,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Portfolio, Root,
    model::{Level, Node, NodeColumn},
};

/// The producer's seed while the log is on.
const SEED: u64 = 20_261_104;
/// How often the state is compared with the last one written.
const POLL: Duration = Duration::from_millis(50);
/// Lines and keys listed per state line: every line of the end-to-end
/// portfolio with its books collapsed (74), and the expanded book.
const LINES: usize = 120;
/// The most rows a `rows` line lists: the end-to-end portfolio.
const LOGGED_ROWS: usize = 500;
/// The x of a chevron lane in the tree cell: the cell padding, 16 px per
/// level, then the 20 px lane.
const CHEVRON_PADDING: f32 = 6.;
const CHEVRON_INDENT: f32 = 16.;
const CHEVRON_LANE: f32 = 20.;

/// The log, opened on first use; `None` without `PORTFOLIO_E2E_LOG`.
fn log() -> Option<&'static Log> {
    static LOG: OnceLock<Option<Log>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("PORTFOLIO_E2E_LOG")?);
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
                err!("PORTFOLIO_E2E_LOG={}: not written: {error}", path.display());
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
pub fn install(grid: &Entity<LiveGridState<Portfolio>>, cx: &mut Context<Root>) {
    let Some(log) = log() else {
        return;
    };
    let settings: Map<String, Value> = [
        "ROWS",
        "RATE",
        "CHURN",
        "PORTFOLIO_MOVES",
        "PORTFOLIO_SELECT",
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

/// `element`, the grid's container, measured for the `geometry` line.
pub fn measure_grid<E: ParentElement>(
    element: E,
    grid: &Entity<LiveGridState<Portfolio>>,
    size: Size,
) -> E {
    let Some(log) = log() else {
        return element;
    };
    let grid = grid.clone();
    element.child(measure(move |bounds, window, cx| {
        log.geometry(bounds, grid.read(cx), size.table_row_height(), window);
    }))
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
/// static position, after the parent's other children.
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
    /// The file whose appearance quits the portfolio.
    quit: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// `None` after a write failed: the portfolio runs on without the log.
    file: Option<File>,
    seq: u64,
    controls: BTreeMap<&'static str, Bounds<Pixels>>,
    /// The last `geometry` line, and the last state without its values.
    geometry: Option<Value>,
    state: Option<Value>,
    /// The store version of the last `rows` line.
    rows_version: Option<u64>,
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
            err!("PORTFOLIO_E2E_LOG: not written: {error}");
            inner.file = None;
        }
    }

    fn write_state(&self, why: &str, grid: &LiveGridState<Portfolio>) {
        let state = state_line(grid);
        self.inner.lock().state = Some(state.clone());
        self.write_with_values(why, state, grid);
    }

    /// A `state` line if anything but the values changed since the last.
    fn poll(&self, grid: &LiveGridState<Portfolio>) {
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

    fn write_with_values(&self, why: &str, mut state: Value, grid: &LiveGridState<Portfolio>) {
        if let Value::Object(map) = &mut state {
            map.insert("why".into(), why.into());
            let totals = grid.projection().totals();
            let total = |column: NodeColumn| totals.get(column.ix()).map(owned_json);
            map.insert(
                "totals".into(),
                json!({
                    "rows": totals.row_count(),
                    "pnl": total(NodeColumn::Pnl),
                    "delta": total(NodeColumn::Delta),
                }),
            );
            map.insert("store_version".into(), grid.snapshot().version().into());
            map.insert(
                "projection_version".into(),
                grid.projection().store_version().into(),
            );
            map.insert("frames".into(), grid.stats().frames().into());
        }
        self.write("state", state);
        self.write_rows(grid);
    }

    /// A `rows` line with every row, when the store has a new version of
    /// at most `LOGGED_ROWS` and nothing streams: checks of the totals and
    /// of the filter read it.
    fn write_rows(&self, grid: &LiveGridState<Portfolio>) {
        let snapshot = grid.snapshot();
        let version = snapshot.version();
        let still = |name: &str| std::env::var(name).is_ok_and(|value| value == "0");
        if snapshot.is_empty()
            || snapshot.len() > LOGGED_ROWS
            || !still("RATE")
            || !still("CHURN")
            || !still("PORTFOLIO_MOVES")
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
        let rows: Vec<Value> = snapshot
            .iter()
            .map(|(_, row)| {
                json!([
                    row.key(),
                    row.parent_key(),
                    level_name(row.level()),
                    text(row, NodeColumn::Name),
                    text(row, NodeColumn::Instrument),
                    number(row, NodeColumn::Pnl),
                    number(row, NodeColumn::Delta),
                ])
            })
            .collect();
        self.write(
            "rows",
            json!({
                "store_version": version,
                "columns": ["key", "parent", "level", "name", "instrument", "pnl", "delta"],
                "rows": rows,
            }),
        );
    }

    /// A `geometry` line if anything moved. `grid_bounds` are the bounds of
    /// the grid's container; the header and zones are laid out as the grid
    /// does, with no horizontal scroll and no column bands. The tree rows
    /// with children on screen are placed by their display index in the
    /// viewport, their chevrons by depth in the tree column, as the grid
    /// paints them.
    fn geometry(
        &self,
        grid_bounds: Bounds<Pixels>,
        grid: &LiveGridState<Portfolio>,
        row_height: Pixels,
        window: &Window,
    ) {
        let origin = grid_bounds.origin;
        let width = grid_bounds.size.width;
        let columns: Vec<(NodeColumn, ColumnZone, Pixels)> = grid
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
        let mut next_x = [px(0.), left, width - right];
        let mut cells = Map::new();
        for (column, zone, column_width) in &columns {
            let slot = match zone {
                ColumnZone::FrozenLeft => 0,
                ColumnZone::Scrolling => 1,
                ColumnZone::FrozenRight => 2,
            };
            let x = next_x[slot];
            next_x[slot] += *column_width;
            cells.insert(
                column.id().into(),
                json!({
                    "x": f32::from(origin.x + x),
                    "y": f32::from(origin.y),
                    "w": f32::from(*column_width),
                    "h": f32::from(row_height),
                }),
            );
        }
        let tree_x = cells
            .get(NodeColumn::Name.id())
            .and_then(|cell| cell["x"].as_f64())
            .unwrap_or_default() as f32;
        let body_y = origin.y + row_height;
        let grid_bottom = origin.y + grid_bounds.size.height;
        let visible = grid.visible_rows();
        let projection = grid.projection();
        let snapshot = grid.snapshot();
        let branches: Vec<Value> = visible
            .clone()
            .filter_map(|ix| {
                let line = projection.line(ix)?;
                let id = line.row()?;
                projection.branch(id)?;
                let key = *snapshot.row(id)?.key();
                let y = body_y + row_height * (ix - visible.start) as f32;
                let lane = tree_x + CHEVRON_PADDING + CHEVRON_INDENT * f32::from(line.depth());
                Some(json!({
                    "key": key,
                    "ix": ix,
                    "y": f32::from(y),
                    "h": f32::from(row_height),
                    "chevron": { "x": lane, "w": CHEVRON_LANE },
                }))
            })
            .collect();
        let mut inner = self.inner.lock();
        let controls: Map<String, Value> = inner
            .controls
            .iter()
            .map(|(name, bounds)| ((*name).to_owned(), rect(*bounds)))
            .collect();
        let line = json!({
            "scale": window.scale_factor(),
            "window": rect(window.bounds()),
            "grid": rect(grid_bounds),
            "header": { "y": f32::from(origin.y), "h": f32::from(row_height), "cells": cells },
            "body": { "x": f32::from(origin.x), "y": f32::from(body_y), "w": f32::from(width), "h": f32::from(grid_bottom - body_y) },
            "row_height": f32::from(row_height),
            "rows_top": f32::from(body_y),
            "tree_x": tree_x,
            "branches": branches,
            "totals": grid.totals().map(|_| json!({ "y": f32::from(grid_bottom - row_height) })),
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

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Fund => "fund",
        Level::Desk => "desk",
        Level::Book => "book",
        Level::Position => "position",
    }
}

/// A text cell of `row`, `null` if empty.
fn text(row: &Node, column: NodeColumn) -> Value {
    match row.value(column) {
        CellValue::Text(text) => text.into(),
        _ => Value::Null,
    }
}

/// A number cell of `row`, `null` if empty.
fn number(row: &Node, column: NodeColumn) -> Value {
    match row.value(column) {
        CellValue::Float(value) => value.into(),
        CellValue::Int(value) => value.into(),
        _ => Value::Null,
    }
}

fn owned_json(value: &OwnedValue) -> Value {
    match value {
        OwnedValue::Float(value) => (*value).into(),
        OwnedValue::Int(value) => (*value).into(),
        _ => Value::Null,
    }
}

/// Up to `LINES` keys, ascending, and how many there are.
fn key_set<'a>(keys: impl Iterator<Item = &'a u64>) -> (usize, Vec<u64>) {
    let mut keys: Vec<u64> = keys.copied().collect();
    keys.sort_unstable();
    let len = keys.len();
    keys.truncate(LINES);
    (len, keys)
}

/// What the grid shows, but the values (they tick under the stream).
fn state_line(grid: &LiveGridState<Portfolio>) -> Value {
    let snapshot = grid.snapshot();
    let projection = grid.projection();
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
            "column": cell.column().id(),
            "ix": cell.row().and_then(display_ix),
        })
    });
    let visible_rows = grid.visible_rows();
    json!({
        "expand_depth": serde_json::to_value(grid.expand_depth()).unwrap_or(Value::Null),
        "filter": grid.quick_filter().as_ref(),
        "lines": lines_json(grid),
        "selection": { "len": selected_len, "keys": selected },
        "scoped": { "len": scoped.len(), "keys": &scoped.keys()[..scoped.len().min(LINES)] },
        "focus": focus,
        "menu": grid.is_menu_open(),
        "rows": projection.len(),
        "row_count": projection.row_count(),
        "store_rows": snapshot.len(),
        "tree_cycles": projection.tree_cycles(),
        "visible_rows": [visible_rows.start, visible_rows.end],
        "visible": grid.visible_keys().into_iter().take(LINES).collect::<Vec<_>>(),
    })
}

/// The first `LINES` lines shown: `{"key": 108, "depth": 2, "name": "Book
/// 1A-1", "level": "book", "children": 3, "expanded": false}`; a row
/// without children has neither of the last two. A row removed since the
/// projection was built reads `"key": null`.
fn lines_json(grid: &LiveGridState<Portfolio>) -> Vec<Value> {
    let projection = grid.projection();
    let snapshot = grid.snapshot();
    (0..projection.len().min(LINES))
        .filter_map(|ix| {
            let line = projection.line(ix)?;
            let id = line.row()?;
            let Some(row) = snapshot.row(id) else {
                return Some(json!({ "key": null, "depth": line.depth() }));
            };
            let mut entry = json!({
                "key": row.key(),
                "depth": line.depth(),
                "name": text(row, NodeColumn::Name),
                "level": level_name(row.level()),
            });
            if let (Some(branch), Value::Object(map)) = (projection.branch(id), &mut entry) {
                map.insert("children".into(), branch.child_count().into());
                map.insert("expanded".into(), branch.is_expanded().into());
            }
            Some(entry)
        })
        .collect()
}

/// `{"kind": "cell", "key": 42, "column": "name"}`; the totals row its
/// `edge`; an empty target only `kind`.
fn target_json(target: &GridTarget<u64, NodeColumn>) -> Value {
    match target {
        GridTarget::Cell { key, column } => {
            json!({ "kind": "cell", "key": key, "column": column.id() })
        }
        GridTarget::Header { column } => json!({ "kind": "header", "column": column.id() }),
        GridTarget::Totals { edge, column } => json!({
            "kind": "totals",
            "edge": match edge {
                PinnedEdge::Top => "top",
                PinnedEdge::Bottom => "bottom",
            },
            "column": column.id(),
        }),
        GridTarget::Empty => json!({ "kind": "empty" }),
        // A target this example does not show.
        other => json!({ "kind": format!("{other:?}") }),
    }
}

fn event_line(event: &LiveGridEvent<u64, NodeColumn>) -> Value {
    match event {
        LiveGridEvent::SelectionChange { selection, origin } => {
            let (len, keys) = key_set(selection.iter());
            json!({ "event": "SelectionChange", "origin": format!("{origin:?}"), "len": len, "keys": keys })
        }
        LiveGridEvent::FocusChange { cell, origin } => json!({
            "event": "FocusChange",
            "origin": format!("{origin:?}"),
            "key": cell.as_ref().and_then(|cell| cell.row().copied()),
            "column": cell.as_ref().map(|cell| cell.column().id()),
        }),
        LiveGridEvent::ExpansionChange { target, origin } => json!({
            "event": "ExpansionChange",
            "target": match target {
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
        LiveGridEvent::Click {
            target,
            button,
            click_count,
            ..
        } => json!({
            "event": "Click",
            "target": target_json(target),
            "button": format!("{button:?}"),
            "click_count": click_count,
        }),
        // Any other event: its name and fields.
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
