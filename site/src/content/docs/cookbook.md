---
title: Cookbook
description: Short recipes for tasks that come up in live applications.
---

Short recipes for tasks that come up in live applications. Each one is a function of the `cookbook` example, which builds with the rest of the repository, so the code here cannot drift from the API. The example is a quotes window whose toolbar runs them:

```sh
cargo run --release -p gpui-kit-grid --example cookbook
```

## Reload under a live stream

A user presses Refresh, and the application reloads every row from a slow source while live updates keep arriving. `begin_reload` makes the live writes wait; when the new rows are in, they are applied on top of the loaded ones, so nothing written during the load is lost and nothing older wins. One batch shows the loaded rows and clears the progress bar in the same snapshot.

```rust src="crates/grid/examples/cookbook.rs#reload-progress"
```

- `GridProgress::new(fraction)` shows a determinate bar instead.
- A `Reload` dropped without `finish` cancels the reload; the waiting writes apply as usual.
- `GridStore::builder(options).with_reload_policy(policy)` replaces how the two meet: a policy that knows row versions can drop live writes older than the snapshot.

## Hold the order while the user reads

A sorted grid under a fast stream reorders all the time. `HoldReorder` keeps the order on screen while the pointer is over the grid, for at most a few seconds, while the values keep updating in place; `LiveReorder`, the default, shows every order at once. A "Refresh order" button shows a held order on demand.

```rust src="crates/grid/examples/cookbook.rs#hold-order"
```

- Every built-in policy also holds the order while the context menu or a cell editor is open, and a user's own action (a click on a header) shows the new order at once.
- `ManualReorder` holds until `apply_held_order`, for a grid that reorders only when asked.
- `set_reorder_policy` hands an order that is already held to the new policy straight away.

## Save and restore the layout

The layout is the columns' order, widths, zones and visibility, the sort, the grouping and its expansion. Read it at startup, apply it with `with_layout`, and write it back on the four events that change it:

```rust src="crates/grid/examples/cookbook.rs#save-layout"
```

- `with_view_id` names the kind of grid, so a layout saved for another one is refused rather than half-applied.
- A layout survives schema changes: unknown columns are ignored and new ones are added at the end.
- A real application debounces the write; `blotter` saves 0.5 s after the last change and on exit, and `dock_layout` keeps every panel's layout and named layouts in one file.

## Your own export format

`export_with` hands one snapshot of what the grid shows to a closure in the background and resolves with what it returns: here a Markdown table, with the numbers as the grid shows them.

```rust src="crates/grid/examples/cookbook.rs#export-with"
```

- `control.advance` reports progress and is where a cancelled export stops.
- The same closure writes xlsx with your own library: `ExportTable` carries column widths, alignment, bands, frozen columns, group levels and a number format per cell ([Export](../guides/export/#xlsx)).
- Awaiting the job gives `ExportOutcome`; `into_output()` is the closure's value.

## Feed a grid from a WebSocket

The repository's gateway (`crates/grid-gateway`) serves simulated quotes over a WebSocket with the small binary protocol of `crates/grid-feed`: a snapshot, then deltas conflated per client to the latest value per key, and a heartbeat every second. `FeedClient` is the protocol's state machine, free of any socket; this recipe drives it from a thread with `tungstenite` and writes what it says into the grid.

```rust src="crates/grid/examples/cookbook.rs#websocket-feed"
```

- A snapshot replaces every row; each delta is one batch, so it lands in one snapshot.
- After a sequence gap the client asks for a resync once and drops deltas until a new snapshot covers the gap.
- The reload API is not needed here: the snapshot and the deltas arrive on one ordered stream, so there is nothing to replay.

Run the gateway, then the example against it:

```sh
cargo run --release -p gpui-kit-grid-gateway
COOKBOOK_FEED=ws://127.0.0.1:9001 cargo run --release -p gpui-kit-grid --example cookbook
```

The browser demos use the same protocol over the browser's WebSocket ([Web](../web/#live-quotes-from-your-own-gateway)).

## Two grids over one store

Two views of the same live rows: every quote by price in one grid, by volume in another. The store ingests each write once; each grid sorts, filters, groups and highlights on its own.

```rust src="crates/grid/examples/cookbook.rs#two-grids-one-store"
```

The second grid flashes only its Volume column, with a change painter of its own:

```rust src="crates/grid/examples/cookbook.rs#flash-some-columns"
```

A painter runs in prepaint for every changed visible cell, so it must not allocate or lock; `cell_style` still runs after it and has the last word.
