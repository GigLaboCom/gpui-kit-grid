//! The JSON the page's scripts read through `stats` and `grid_state`: the
//! parts every example reports the same way.

use std::time::Duration;

use gpui_kit_grid::{
    GridColumns, GridRow, GridStats, LiveGridDelegate, LiveGridState, SortDirection,
};
use serde_json::{Map, Value, json};

/// A duration in milliseconds, to the microsecond.
pub fn ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1e6).round() / 1e3
}

/// What `stats` starts with: `updates` and `rate`, the example's own (the
/// rows written and how many a second), then the counters of its main
/// grid. Durations are in milliseconds.
pub fn stats_json(
    updates: impl Into<Value>,
    rate: impl Into<Value>,
    grid: &GridStats,
) -> Map<String, Value> {
    let Value::Object(stats) = json!({
        "updates": updates.into(),
        "rate": rate.into(),
        "frames": grid.frames(),
        "rebuilds": grid.rebuilds(),
        "last_rebuild_ms": ms(grid.last_rebuild()),
        "max_rebuild_ms": ms(grid.max_rebuild()),
        "last_prepaint_ms": ms(grid.last_prepaint()),
        "last_pull_ms": ms(grid.last_pull()),
        "snapshots": grid.snapshots(),
        "visible_cells": grid.visible_cells(),
    }) else {
        unreachable!("an object")
    };
    stats
}

/// A grid's scroll, selection and sort, for `grid_state`: `rows` is the
/// range of display rows in view, `visible` the rows painted (top first),
/// `selected` and `focused` rows, each by `name`, and `sort` as
/// [`sort_json`] reports it.
pub fn grid_json<D: LiveGridDelegate, N: Into<Value>>(
    grid: &LiveGridState<D>,
    name: impl Fn(&<D::Row as GridRow>::Key) -> N,
) -> Map<String, Value> {
    let rows = grid.visible_rows();
    let Value::Object(state) = json!({
        "rows": [rows.start, rows.end],
        "visible": grid.visible_keys().iter().map(|key| name(key).into()).collect::<Vec<Value>>(),
        "selected": grid.selection().iter().map(|key| name(key).into()).collect::<Vec<Value>>(),
        "focused": grid.focused_cell().and_then(|cell| cell.row()).map(|key| name(key).into()),
        "sort": sort_json(grid.sort()),
    }) else {
        unreachable!("an object")
    };
    state
}

/// A grid's sort as pairs of column id and direction (`asc`, `desc`).
pub fn sort_json<C: GridColumns>(sort: &[(C, SortDirection)]) -> Vec<[&'static str; 2]> {
    sort.iter()
        .map(|(column, direction)| {
            let direction = match direction {
                SortDirection::Ascending => "asc",
                SortDirection::Descending => "desc",
            };
            [column.id(), direction]
        })
        .collect()
}
