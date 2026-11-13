//! `ORDERS_E2E_LOG=<file>`: the orders example reports what happens in its
//! window, one JSON object per line, for the Linux end-to-end run
//! (`script/e2e`), in the format of the blotter's report
//! (`examples/blotter/e2e.rs`). Without the variable nothing here runs.
//!
//! | `type` | Written | Holds |
//! |---|---|---|
//! | `start` | Once | The process id, the seed and the settings |
//! | `event` | On every event of the orders grid (`grid: "orders"`) and of the basket (`grid: "basket"`) | Its kind and fields: an `EditStart` with its key, column and origin; an `EditCommit` with its id, key, column, value, base and current; an `EditCancel` with its reason and draft; a `Drop` with its source keys and target; a `state` line follows |
//! | `state` | After an event, and every 50 ms when something but the values changed | Focus, selection, the edited cell, the field's mark and message, the commits waiting, the edit states of the shown cells, the shown keys, the statuses and values of the first shown rows, the held order, the sort and grouping with the captions shown, the basket's keys, a running export |
//! | `geometry` | At start-up and whenever it changes | In window logical pixels: the window, the grid, its header cells by column id, the first data row and the row height, the open field and the elements of its list or calendar (with `test-support`), the basket and the toolbar buttons |
//!
//! The log fixes the seed of the orders and the market, and quits the
//! example through `App::quit` when the file `<log>.quit` appears. The
//! field's list and calendar are measured through gpui-kit's element
//! observation: the end-to-end build turns on `test-support`.

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
    ColumnZone, DisplayRow, DropSource, DropTarget, EditState, EditorMark, GridColumns as _,
    GridRow as _, GridTarget, GroupPath, LiveGridEvent, LiveGridState, OwnedValue, SortDirection,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Basket, Orders, Root,
    model::{Order, OrderColumn},
};

/// The seed of the orders and the market while the log is on.
const SEED: u64 = 20_261_106;
const POLL: Duration = Duration::from_millis(50);
/// Keys listed per state line.
const KEYS: usize = 60;
/// Rows whose values a state line lists: the first shown ones.
const VALUE_ROWS: usize = 25;

fn log() -> Option<&'static Log> {
    static LOG: OnceLock<Option<Log>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("ORDERS_E2E_LOG")?);
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
                err!("ORDERS_E2E_LOG={}: not written: {error}", path.display());
                None
            }
        }
    })
    .as_ref()
}

/// The seed: fixed while the log is on.
pub fn seed() -> Option<u64> {
    log().map(|_| SEED)
}

/// Starts the log for `grid` and `basket`: the `start` line once, an
/// `event` and a `state` line per event, the polled `state` lines and the
/// quit file. Called again for a grid built anew (Keep draft / Follow).
pub fn install(
    grid: &Entity<LiveGridState<Orders>>,
    basket: &Entity<LiveGridState<Basket>>,
    cx: &mut Context<Root>,
) {
    let Some(log) = log() else {
        return;
    };
    let first = {
        let mut inner = log.inner.lock();
        !std::mem::replace(&mut inner.installed, true)
    };
    // A subscriber runs after the grid's update: reading it is safe.
    cx.subscribe(grid, move |root, grid, event, cx| {
        log.write("event", event_line("orders", event));
        log.write_state("event", grid.read(cx), root.basket.read(cx));
    })
    .detach();
    if !first {
        return;
    }
    let settings: Map<String, Value> = [
        "ROWS",
        "RATE",
        "FILLS",
        "ORDERS_LATENCY_MS",
        "ORDERS_REJECT_PERCENT",
        "ORDERS_CONFLICT_PERCENT",
        "ORDERS_REMOVE_FILLED_MS",
        "ORDERS_REORDER",
        "ORDERS_POLICY",
        "ORDERS_GROUP",
        "ORDERS_SORT",
        "ORDERS_EXPORT_PATH",
        "ORDERS_CONFLICT_ON_EDIT",
        "ORDERS_FILL_ON_EDIT",
    ]
    .into_iter()
    .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?.into())))
    .collect();
    log.write(
        "start",
        json!({ "pid": std::process::id(), "seed": SEED, "env": settings }),
    );
    cx.subscribe(basket, move |root, basket, event, cx| {
        log.write("event", event_line("basket", event));
        log.write_state("event", root.grid.read(cx), basket.read(cx));
    })
    .detach();
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let quit = cx.update(|cx| {
                if let Some(root) = this.upgrade() {
                    let root = root.read(cx);
                    log.poll(
                        root.grid.read(cx),
                        root.basket.read(cx),
                        root.is_exporting(),
                    );
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

/// `element`, the orders grid's container, measured for the `geometry`
/// line.
pub fn measure_grid<E: ParentElement>(
    element: E,
    grid: &Entity<LiveGridState<Orders>>,
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

/// `element`, the basket grid's container, measured for the `geometry`
/// line.
pub fn measure_basket<E: ParentElement>(element: E, size: Size) -> E {
    let Some(log) = log() else {
        return element;
    };
    element.child(measure(move |bounds, _, _| {
        let mut inner = log.inner.lock();
        inner.basket = Some((bounds, size.table_row_height()));
    }))
}

/// Measures a toolbar control for the `geometry` line.
pub trait RecordBounds: ParentElement + FluentBuilder + Sized {
    fn e2e_bounds(self, name: &'static str) -> Self {
        self.when_some(log(), |this, log| {
            this.child(measure(move |bounds, _, _| {
                log.inner.lock().controls.insert(name, bounds);
            }))
        })
    }
}

impl<E: ParentElement + FluentBuilder> RecordBounds for E {}

/// A child that reports its parent's bounds in prepaint.
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
    quit: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    file: Option<File>,
    seq: u64,
    installed: bool,
    controls: BTreeMap<&'static str, Bounds<Pixels>>,
    basket: Option<(Bounds<Pixels>, Pixels)>,
    geometry: Option<Value>,
    state: Option<Value>,
}

impl Log {
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
            err!("ORDERS_E2E_LOG: not written: {error}");
            inner.file = None;
        }
    }

    fn write_state(&self, why: &str, grid: &LiveGridState<Orders>, basket: &LiveGridState<Basket>) {
        let state = state_line(grid, basket, false);
        self.inner.lock().state = Some(state.clone());
        self.write_with_values(why, state, grid);
    }

    fn poll(&self, grid: &LiveGridState<Orders>, basket: &LiveGridState<Basket>, exporting: bool) {
        let state = state_line(grid, basket, exporting);
        {
            let mut inner = self.inner.lock();
            if inner.state.as_ref() == Some(&state) {
                return;
            }
            inner.state = Some(state.clone());
        }
        self.write_with_values("poll", state, grid);
    }

    /// Adds the values of the first shown rows, which tick under the
    /// stream and so do not make a poll write.
    fn write_with_values(&self, why: &str, mut state: Value, grid: &LiveGridState<Orders>) {
        if let Value::Object(map) = &mut state {
            map.insert("why".into(), why.into());
            let snapshot = grid.snapshot();
            let rows: Vec<Value> = grid
                .visible_keys()
                .into_iter()
                .take(VALUE_ROWS)
                .filter_map(|key| snapshot.get(&key))
                .map(|order| row_values(order))
                .collect();
            map.insert("values".into(), rows.into());
            map.insert("store_version".into(), snapshot.version().into());
        }
        self.write("state", state);
    }

    fn geometry(
        &self,
        grid_bounds: Bounds<Pixels>,
        grid: &LiveGridState<Orders>,
        row_height: Pixels,
        window: &Window,
    ) {
        let origin = grid_bounds.origin;
        let width = grid_bounds.size.width;
        let columns: Vec<(OrderColumn, ColumnZone, Pixels)> = grid
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
        // The body is not scrolled sideways in the end-to-end run.
        let mut next_x = [px(0.), left, width - right];
        let mut cells = Map::new();
        for (column, zone, column_width) in &columns {
            let slot = match zone {
                ColumnZone::FrozenLeft => 0,
                ColumnZone::FrozenRight => 2,
                _ => 1,
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
                    "zone": format!("{zone:?}"),
                    "shown": slot != 1 || (x >= left && x + *column_width <= width - right),
                }),
            );
        }
        let rows_top = origin.y + row_height;
        let projection = grid.projection();
        let visible = grid.visible_rows();
        let captions: Vec<Value> = visible
            .clone()
            .filter_map(|ix| match projection.line(ix)? {
                DisplayRow::GroupCaption { group, .. } => {
                    let path = projection.groups().get(group)?.path();
                    Some(json!({
                        "path": path_json(path),
                        "y": f32::from(rows_top + row_height * (ix - visible.start) as f32),
                        "h": f32::from(row_height),
                    }))
                }
                _ => None,
            })
            .collect();
        let editor = editor_json(window);
        let mut inner = self.inner.lock();
        let controls: Map<String, Value> = inner
            .controls
            .iter()
            .map(|(name, bounds)| ((*name).to_owned(), rect(*bounds)))
            .collect();
        let basket = inner.basket.map(|(bounds, row_height)| {
            json!({
                "grid": rect(bounds),
                "rows_top": f32::from(bounds.origin.y + row_height),
                "row_height": f32::from(row_height),
            })
        });
        let line = json!({
            "scale": window.scale_factor(),
            "window": rect(window.bounds()),
            "grid": rect(grid_bounds),
            "header": { "y": f32::from(origin.y), "h": f32::from(row_height), "cells": cells },
            "row_height": f32::from(row_height),
            "rows_top": f32::from(rows_top),
            "captions": captions,
            "editor": editor,
            "basket": basket,
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

/// The open field and the elements of its list or calendar, as gpui-kit's
/// element observation saw them in the last frame: the field's text input
/// (its value) and every named element under the editor's host (the items
/// of a list, the days of a calendar), by name.
#[cfg(feature = "test-support")]
fn editor_json(window: &Window) -> Value {
    use gpui_kit::{ElementId, Role};
    let host = ElementId::Name("live-grid-editor".into());
    let mut input = Value::Null;
    let mut items = Map::new();
    for element in gpui_kit::base::test_support::snapshots(window) {
        let in_editor = element
            .path()
            .iter()
            .any(|id| matches!(id, ElementId::NamedChild(name, _) if **name == host));
        if !in_editor || !element.visible() {
            continue;
        }
        if element.role() == Some(Role::TextInput) {
            input = json!({ "rect": rect(element.bounds()), "value": element.value() });
        } else if let Some(label) = element.label() {
            items.insert(label.to_owned(), rect(element.bounds()));
        }
    }
    json!({ "input": input, "items": items })
}

#[cfg(not(feature = "test-support"))]
fn editor_json(_: &Window) -> Value {
    Value::Null
}

fn rect(bounds: Bounds<Pixels>) -> Value {
    json!({
        "x": f32::from(bounds.origin.x),
        "y": f32::from(bounds.origin.y),
        "w": f32::from(bounds.size.width),
        "h": f32::from(bounds.size.height),
    })
}

fn path_json(path: &GroupPath) -> Value {
    path.levels()
        .iter()
        .map(|(_, value)| owned_json(value))
        .collect::<Vec<_>>()
        .into()
}

/// A value as the log writes it: numbers as numbers, time as RFC 3339.
fn owned_json(value: &OwnedValue) -> Value {
    match value {
        OwnedValue::Empty => Value::Null,
        OwnedValue::Bool(value) => (*value).into(),
        OwnedValue::Int(value) => (*value).into(),
        OwnedValue::Float(value) => json!((value * 1e4).round() / 1e4),
        OwnedValue::Text(text) => text.as_ref().into(),
        OwnedValue::Timestamp(at) => at.to_rfc3339().into(),
        other => format!("{other:?}").into(),
    }
}

/// An order's values by column id.
fn row_values(order: &Order) -> Value {
    let values: Map<String, Value> = OrderColumn::all()
        .iter()
        .map(|column| {
            let value = OwnedValue::from_cell(order.value(*column));
            (column.id().to_owned(), owned_json(&value))
        })
        .collect();
    values.into()
}

fn edit_state_json(state: &EditState) -> Value {
    match state {
        EditState::Pending => json!({ "state": "pending" }),
        EditState::Accepted => json!({ "state": "accepted" }),
        EditState::Failed(error) => {
            json!({ "state": "failed", "message": error.message().as_ref() })
        }
        other => json!({ "state": format!("{other:?}") }),
    }
}

/// What the grid shows, but the values.
fn state_line(
    grid: &LiveGridState<Orders>,
    basket: &LiveGridState<Basket>,
    exporting: bool,
) -> Value {
    let snapshot = grid.snapshot();
    let visible = grid.visible_keys();
    let mut selection: Vec<u64> = grid.selection().iter().copied().collect();
    selection.sort_unstable();
    let focus = grid.focused_cell().map(|cell| {
        json!({
            "key": cell.row(),
            "group": cell.group().map(path_json),
            "column": cell.column().id(),
        })
    });
    let edit_states: Vec<Value> = visible
        .iter()
        .flat_map(|key| {
            OrderColumn::all().iter().filter_map(move |column| {
                let state = grid.edit_state(key, *column)?;
                let mut line = edit_state_json(state);
                if let Value::Object(map) = &mut line {
                    map.insert("key".into(), (*key).into());
                    map.insert("column".into(), column.id().into());
                }
                Some(line)
            })
        })
        .collect();
    let (mark, message) = match grid.edit_message() {
        Some((mark, message)) => (
            match mark {
                EditorMark::Invalid => "invalid",
                EditorMark::Conflict => "conflict",
                _ => "none",
            },
            Some(message.to_string()),
        ),
        None => ("none", None),
    };
    let lines: Vec<Value> = {
        let projection = grid.projection();
        let rows = grid.visible_rows();
        rows.filter_map(|ix| match projection.line(ix)? {
            DisplayRow::GroupCaption { group, .. } => {
                let group = projection.groups().get(group)?;
                Some(json!({
                    "kind": "caption",
                    "path": path_json(group.path()),
                    "rows": group.row_count(),
                    "expanded": group.is_expanded(),
                }))
            }
            DisplayRow::Data { row, .. } => snapshot
                .row(row)
                .map(|order| json!({ "kind": "data", "key": order.id })),
            _ => None,
        })
        .take(KEYS)
        .collect()
    };
    let mut basket_keys: Vec<u64> = basket
        .snapshot()
        .iter()
        .map(|(_, order)| order.id)
        .collect();
    basket_keys.sort_unstable();
    let mut basket_selection: Vec<u64> = basket.selection().iter().copied().collect();
    basket_selection.sort_unstable();
    json!({
        "focus": focus,
        "selection": { "len": selection.len(), "keys": &selection[..selection.len().min(KEYS)] },
        "editing": grid.editing_cell().map(|(key, column)| json!({ "key": key, "column": column.id() })),
        "mark": mark,
        "message": message,
        "pending": grid.pending_edit_count(),
        "edit_states": edit_states,
        "visible": visible.iter().take(KEYS).collect::<Vec<_>>(),
        "visible_rows": [grid.visible_rows().start, grid.visible_rows().end],
        "lines": lines,
        "rows": grid.projection().len(),
        "store_rows": snapshot.len(),
        "held": grid.is_order_held(),
        "menu": grid.is_menu_open(),
        "sort": grid.sort().iter().map(|(column, direction)| json!([
            column.id(),
            if *direction == SortDirection::Ascending { "asc" } else { "desc" },
        ])).collect::<Vec<_>>(),
        "group_by": grid.group_by().iter().map(|level| level.id().as_str().to_owned()).collect::<Vec<_>>(),
        // The statuses of the first shown rows: a cancel or a fill writes a
        // line (the values tick under the stream and do not).
        "statuses": visible
            .iter()
            .take(VALUE_ROWS)
            .filter_map(|key| snapshot.get(key))
            .map(|order| json!([order.id, order.status.icon().text()]))
            .collect::<Vec<_>>(),
        "basket": { "keys": basket_keys, "selection": basket_selection },
        "exporting": exporting,
    })
}

fn target_json(target: &GridTarget<u64, OrderColumn>) -> Value {
    match target {
        GridTarget::Cell { key, column } => {
            json!({ "kind": "cell", "key": key, "column": column.id() })
        }
        GridTarget::Header { column } => json!({ "kind": "header", "column": column.id() }),
        GridTarget::GroupCaption { group, .. } => {
            json!({ "kind": "caption", "path": path_json(group) })
        }
        GridTarget::Empty => json!({ "kind": "empty" }),
        other => json!({ "kind": format!("{other:?}") }),
    }
}

fn drop_target_json(target: &DropTarget<u64>) -> Value {
    match target {
        DropTarget::Row { key, place } => {
            json!({ "kind": "row", "key": key, "place": format!("{place:?}") })
        }
        DropTarget::Group(path) => json!({ "kind": "group", "path": path_json(path) }),
        DropTarget::Empty => json!({ "kind": "empty" }),
        other => json!({ "kind": format!("{other:?}") }),
    }
}

fn event_line(grid: &str, event: &LiveGridEvent<u64, OrderColumn>) -> Value {
    let mut line = match event {
        LiveGridEvent::SelectionChange { selection, origin } => {
            let mut keys: Vec<u64> = selection.iter().copied().collect();
            keys.sort_unstable();
            json!({ "event": "SelectionChange", "origin": format!("{origin:?}"), "keys": keys })
        }
        LiveGridEvent::FocusChange { cell, origin } => json!({
            "event": "FocusChange",
            "origin": format!("{origin:?}"),
            "key": cell.as_ref().and_then(|cell| cell.row().copied()),
            "column": cell.as_ref().map(|cell| cell.column().id()),
        }),
        LiveGridEvent::EditStart {
            key,
            column,
            origin,
        } => json!({
            "event": "EditStart",
            "key": key,
            "column": column.id(),
            "origin": format!("{origin:?}"),
        }),
        LiveGridEvent::EditCommit {
            edit,
            key,
            column,
            value,
            base,
            current,
        } => json!({
            "event": "EditCommit",
            "edit": format!("{edit:?}"),
            "key": key,
            "column": column.id(),
            "value": owned_json(value),
            "base": owned_json(base),
            "current": owned_json(current),
        }),
        LiveGridEvent::EditCancel {
            key,
            column,
            reason,
            draft,
        } => json!({
            "event": "EditCancel",
            "key": key,
            "column": column.id(),
            "reason": format!("{reason:?}"),
            "draft": draft.as_ref().map(owned_json),
        }),
        LiveGridEvent::Drop {
            source,
            target,
            modifiers,
        } => json!({
            "event": "Drop",
            "source": match source {
                DropSource::Rows(drag) => json!({ "keys": &drag.keys()[..] }),
                other => json!({ "other": format!("{other:?}") }),
            },
            "target": drop_target_json(target),
            "alt": modifiers.alt,
        }),
        LiveGridEvent::Confirm { keys, secondary } => json!({
            "event": "Confirm",
            "keys": &keys[..],
            "secondary": secondary,
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
        LiveGridEvent::OrderHeldChange { held } => {
            json!({ "event": "OrderHeldChange", "held": held })
        }
        LiveGridEvent::SortChange { sort, origin } => json!({
            "event": "SortChange",
            "sort": sort.iter().map(|(column, _)| column.id()).collect::<Vec<_>>(),
            "origin": format!("{origin:?}"),
        }),
        LiveGridEvent::ExpansionChange { origin, .. } => json!({
            "event": "ExpansionChange",
            "origin": format!("{origin:?}"),
        }),
        other => {
            let debug = format!("{other:?}");
            let name: String = debug
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            json!({ "event": name })
        }
    };
    if let Value::Object(map) = &mut line {
        map.insert("grid".into(), grid.into());
    }
    line
}
