---
title: Getting started
description: Add gpui-kit-grid to a gpui-kit application and show a live grid.
---

gpui-kit-grid is a separate crate on top of [gpui-kit](https://gpui-kit.com). A first grid takes seven pieces: the columns as an enum, a row type that is an immutable value with a key, a delegate that styles cells, the `LiveGrid` element in a window, the grid's state with its schema and store, a subscription to its events, and a writer that upserts rows from any thread. This page walks through them in the `quickstart` example, whose code is shown here as it compiles.

## Install

The crate is not on crates.io yet: it depends on gpui-kit by git revision, and crates.io takes no git dependencies. Add both from git, gpui-kit at the revision the grid is built against, so the application and the grid share one gpui-kit and one GPUI:

```toml
[dependencies]
gpui-kit = { git = "https://github.com/longbridge/gpui-kit", rev = "146ab45ac0d0cc9dad5f5fd05ecd38da9b3db6e7" }
gpui-kit-grid = { git = "https://github.com/GigLaboCom/gpui-kit-grid", tag = "v1.0.0" }
```

The application reaches GPUI through gpui-kit (`gpui_kit::App`, `gpui_kit::Window` and the rest), as the grid does; it does not list a GPUI crate of its own.

Rust 1.95 or newer. Two features are off by default:

| Feature | What it adds |
|---|---|
| `decimal` | `CellValue::Decimal` and `OwnedValue::Decimal` with `rust_decimal`, for exact money values and sums |
| `derive` | `#[derive(GridColumns)]`, so the column enum needs no hand-written implementation ([Columns](../guides/columns/#deriving-gridcolumns)) |

The library builds for `wasm32-unknown-unknown` too; [Web](../web/) has what changes in the browser.

## Run the example

From a checkout of the repository:

```sh
cargo run --release -p gpui-kit-grid --example quickstart
```

A window with 1000 trades that a producer thread keeps updating, sorted by price. Selecting a row prints its key; Enter or a double click prints the keys it would open.

## 1. Columns are an enum

`id` is what a saved layout stores, so it must not change; `ix` is the column's index in the schema.

```rust src="crates/grid/examples/quickstart.rs#columns"
```

With the `derive` feature, `#[derive(GridColumns)]` writes the same implementation.

## 2. A row is an immutable value with a key

`GridRow::value` borrows from the row and must not allocate: it is called for every visible cell on every frame, and for every row when sorting.

```rust src="crates/grid/examples/quickstart.rs#row"
```

A key can be any `Clone + Eq + Hash + Debug + Send + Sync` type, a composite struct included; the grid needs neither `Copy` nor `Ord`. Selection, focus, highlight, menu and click targets and the copied rows all follow the key, never a position.

## 3. The delegate styles cells

The delegate's hooks take `&self` and run for visible cells only. They must not read or update the grid entity: the grid is borrowed while it paints.

```rust src="crates/grid/examples/quickstart.rs#delegate"
```

`LiveGridDelegate` has hooks for rows, aggregates, tooltips, the context menu, the primary command, elements in cells, editing and drag payloads; every one has a default. [Cells](../guides/cells/) covers the presentation hooks.

## 4. The window places the grid element

`LiveGrid` is an element (`RenderOnce`), like gpui-kit's `DataTable`. Its size sets the row height, as in gpui-kit's tables: `xsmall` 26 px, `small` 30, `medium` 32 (the default), `large` 40. `bordered(false)` removes the frame.

```rust src="crates/grid/examples/quickstart.rs#window"
```

Place `LiveGrid::new(&grid)` in your layout, not the state entity. `LiveGridState` is itself a view that `LiveGrid` embeds, so the grid repaints inside a cached dock panel too. Name every grid with `accessibility_label`: a screen reader announces the table by it.

## 5. The grid state

The schema lists the columns with their titles and widths (`with_frozen(1)` keeps the first one in place while the others scroll). `new_standalone` gives the grid a store of its own; `ViewSpec` sets the sort, the filter, the grouping or the tree.

```rust src="crates/grid/examples/quickstart.rs#state"
```

Several grids can share one store: build it with `GridStore::new(options, cx)` and pass it to `LiveGridState::new(schema, delegate, store.clone(), cx)`.

## 6. Events

Selection and focus are kept by key, and every change arrives as a `LiveGridEvent`. Enter, a double click and the primary menu command emit `Confirm` with the keys they act on.

```rust src="crates/grid/examples/quickstart.rs#events"
```

The example prints with `writeln!`, not `println!`: a print to a closed stdout (a parent that exited, `| head`) panics, and a panic in a GPUI callback aborts the application.

`grid.read(cx).selection()` returns the whole selection, `focused_cell()` the focused cell, and `scoped_selection()` what a command acts on: the selection within the rows on screen, in display order.

## 7. Data from any thread

`GridWriter` is `Send + Sync + Clone`. `replace_all` sets a whole data set, `upsert` inserts or replaces a row by key, and `remove` takes one out.

```rust src="crates/grid/examples/quickstart.rs#writer"
```

The writes go to the store's background ingest, which collapses repeated updates of a key and publishes snapshots; the grid shows a new one at most every 8 ms. [Concepts](../concepts/) explains the pipeline.

## Keys and icons

The application starts gpui-kit, then the grid:

```rust src="crates/grid/examples/quickstart.rs#main"
```

- `gpui_kit_grid::init(cx)`, called after `gpui_kit::init(cx)`, binds the grid's keys: arrows, Page Up and Page Down, Home and End, Shift to extend the selection, Cmd/Ctrl+A, Space, Enter, Escape, Cmd/Ctrl+C, Shift+F10 and the Menu key, `+` and `-` for groups and tree rows, F2 to edit. Without it the mouse works and the keys do not. [Keyboard](../guides/keyboard/) lists them and shows how to bind your own.
- The grid draws gpui-kit's icons (sort arrows, menu checks, chevrons, change arrows), so the application registers gpui-kit's assets with `with_assets(gpui_kit::assets::Assets)`. Without them the icons are blank.

## Next

- [Concepts](../concepts/): rows by key, ingest and snapshots, background rebuilds.
- [Grouping](../guides/grouping/) and [Tree](../guides/tree/) for structure; [Live changes](../guides/live-changes/) for how updates show.
- [Cookbook](../cookbook/) for reloads, held order, saved layouts and custom exports.
- The [examples](../examples/), from this quick start to full live applications.
