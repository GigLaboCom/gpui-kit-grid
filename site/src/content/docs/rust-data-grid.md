---
title: 'Rust data grid: the options compared'
description: The data grids and tables of GPUI, egui, iced, Slint, GTK and Dioxus side by side, from their own documentation and source, and which to take when.
sidebar:
  label: Rust data grids compared
---

A Rust data grid comes with a UI toolkit: an egui application takes an egui table, a GPUI application a GPUI one. This page lists the tables each toolkit has, whether the toolkit is chosen already or not.

What it says about another library comes from that library's own documentation, README or source, at the version named, as of November 2026. A feature it does not list for a library is one its documentation does not mention. If something here has changed or is wrong, please [open an issue](https://github.com/GigLaboCom/gpui-kit-grid/issues).

## In short

| Library | Toolkit | Draws | Rows by | Sorting and filtering | Groups and trees |
|---|---|---|---|---|---|
| gpui-kit-grid 2.0 | GPUI, gpui-kit 0.10 | Visible cells, painted by one element | Key | Built in, in the background | Groups with aggregates, trees |
| `DataTable`, gpui-kit 0.10 | GPUI, gpui-kit 0.10 | An element per visible cell | Index | The delegate sorts on a click; no filtering | No |
| gpui-table 0.9 | GPUI, gpui-kit 0.9 | As `DataTable` | Index; an optional row id keeps the selection | Built in, on the UI thread | Not documented |
| sqlly-datatable 4.1 | GPUI, gpui 0.2 | Visible rows, painted on a canvas | Position | Built in, on the UI thread | One group column; a pivot view |
| egui_extras `TableBuilder` 0.37 | egui | Visible rows, with `rows()` | Index | The application's | No |
| egui_table 0.11 | egui | Visible rows and columns | Index | The application's | Expanding rows, kept by the application |
| egui-data-table 0.11 | egui 0.35 | Visible rows | Index | Built in, through hooks | Not documented |
| `table`, iced 0.14 | iced | Every row | Order of the rows given | The application's | No |
| iced_table 0.14 | iced | Every row | Index | The application's | No |
| `StandardTableView`, Slint 1.19 | Slint | Visible rows | Index | The application's model | No |
| `ColumnView`, GTK 4 | GTK | Visible rows | Model items | Sorting through a sort model | Trees through a tree list model |
| dioxus-tabular 0.3 | Dioxus 0.7 | Not documented | Key | Built in, per column | Not documented |

Applications on gpui-kit, egui, iced, Slint and Dioxus also run in the browser through WebAssembly. GTK reaches a browser only through its Broadway backend, which draws a running GTK program over WebSockets.

## Which one to take

- **The application is not built on GPUI.** Take a table of its toolkit; gpui-kit-grid needs gpui-kit and GPUI.
  - In egui, `TableBuilder` for a plain table, egui_table for millions of rows, sticky columns and rows of different heights, egui-data-table for a table people edit.
  - In iced, its `table` for a few rows: it lays out every row it is given.
  - In Slint, `StandardTableView`, with text cells and the sorting in the model.
  - In GTK, `ColumnView`, whose models sort and nest rows into trees.
- **GPUI, with data that changes now and then.** gpui-kit's `DataTable` ships with gpui-kit: it virtualizes rows and columns and has fixed columns, column moves, a row menu and loading on scroll. gpui-table adds a derive and filter widgets on top of it, on gpui-kit 0.9 for now.
- **GPUI, with data that keeps changing.** gpui-kit-grid: rows by key written from any thread, sorting, filtering, grouping and trees rebuilt in the background, and every change highlighted. [Performance](../performance/) measures it against `DataTable` on the same loads.

gpui-kit-grid is not the one to take for rows of different heights (each grid has one row height), for a pivot table, or for filter widgets in the column headers: it filters through the view (`ViewSpec`) and a quick filter, and leaves the controls to the application.

## GPUI

### gpui-kit-grid

A live data grid on gpui-kit 0.10, Apache-2.0. A background task owns the rows and takes writes by key from any thread; sorting, filtering, grouping, aggregates and trees are rebuilt from snapshots in the background; one element paints the visible cells; selection and focus follow the key. It edits under live updates, holds elements in cells, copies and exports in the background, and moves, freezes, hides and saves columns. [Getting started](../start/) builds a first grid, [Concepts](../concepts/) explains the model, and eight of the [examples](../examples/) run in the browser.

### gpui-kit `DataTable`

The table of [gpui-kit](https://gpui-kit.com/component/data-table/) 0.10 (crate `gpui-component`, Apache-2.0).

- Rows go through a uniform list and the scrolling columns through a virtual list, so it builds an element for each visible cell; fixed columns are always built. Rows have one height, from the table's size.
- A `TableDelegate`, owned by the `TableState` entity, gives the cells: its hooks take `&mut self` on the UI thread, and rows are addressed by index.
- A click on a header's sort icon calls the delegate's `perform_sort`, one column at a time; the delegate sorts. There is no filtering API. The selection is a position, so after a sort it stays on the same position, not on the same row.
- Columns resize (a double click fits one), move within their region and are fixed left or right; headers can be grouped in levels.
- One row, column or cell is selected at a time, with keyboard navigation; a row has a context menu; `has_more` and `load_more` load rows as the user scrolls.
- `cell_text` and `dump` give the cells as text for an export the application writes. There is no editing, change highlight, row grouping or tree.
- gpui-kit's plain `Table` is stateless, without virtual scrolling, for small static data.

Sources: [the data table's page](https://gpui-kit.com/component/data-table/), [delegate.rs](https://docs.rs/crate/gpui-component/0.10.0/source/src/table/delegate.rs) and [state.rs](https://docs.rs/crate/gpui-component/0.10.0/source/src/table/state.rs) of gpui-component 0.10.0.

### gpui-table

[gpui-table](https://github.com/stayhydated/gpui-table) 0.9 (MIT or Apache-2.0, Rust 1.99) derives a `DataTable` delegate from a row struct (`#[derive(GpuiTable)]`), on gpui-kit 0.9.

- It draws, resizes and selects as `DataTable` does.
- The delegate holds the rows in a `Vec` and sorts and filters them on the UI thread: sorting by several clauses, and text, faceted, date range and number range filters with their widgets.
- An optional row id keeps the selection on the same record after a reorder.
- It loads more rows as they are needed (`TableLoader`) and has a status bar.

Sources: [README](https://github.com/stayhydated/gpui-table), [docs.rs](https://docs.rs/gpui-table/0.9.0/gpui_table/).

### sqlly-datatable

[sqlly-datatable](https://docs.rs/sqlly-datatable/4.1.0/sqlly_datatable/) 4.1 (MIT), a "configurable virtualized data grid component for the GPUI toolkit" on crates.io's gpui 0.2 and gpui-component 0.5.

- It paints the visible rows on a canvas.
- It owns rows of cell values; `append_rows` adds streamed results, and `spawn_background` runs work off the UI thread.
- Sorting by one column and per-column filters with operators and value lists run on the UI thread; rows and selection are addressed by position.
- It groups by one column into sections that collapse, and has a pivot view with counts, sums, averages, minimums and maximums.
- Cells, rows, columns and ranges can be selected and copied as TSV; columns resize. There is no editing.

Source: [docs.rs](https://docs.rs/sqlly-datatable/4.1.0/sqlly_datatable/).

## egui

### egui_extras `TableBuilder`

Part of [egui](https://github.com/emilk/egui) (0.37, MIT or Apache-2.0), in immediate mode: the application gives the visible rows every frame.

- `rows()` and `heterogeneous_rows()` lay out only the visible rows; the docs call them much faster than adding rows one at a time.
- Rows are indices. The application keeps the selection (egui's demo keeps a set of row indices) and sorts its own data.
- Columns resize and size themselves to the visible rows; the header stays at the top; a cell holds any egui widget.
- The table has no sorting, filtering, grouping, frozen columns or copy of its own.

Sources: [TableBody](https://docs.rs/egui_extras/0.37.0/egui_extras/struct.TableBody.html), [egui's table demo](https://github.com/emilk/egui/blob/0.37.0/crates/egui_demo_lib/src/demo/table_demo.rs).

### egui_table

[egui_table](https://github.com/rerun-io/egui_crates/tree/main/crates/egui_table) 0.11 (MIT or Apache-2.0), from Rerun: "designed to be fast when there are millions of rows, but only hundreds of columns".

- It paints only the visible rows and columns. A `TableDelegate` draws each cell, header and row, and its `prepare` hook is told what is about to be drawn, to load just that.
- Columns resize and size themselves; the first columns and the header stick; column titles can be nested; rows can have different heights and expand, with the application keeping which are expanded.
- It is "batteries not included": no sorting, filtering, selection or copy.

Sources: [Table](https://docs.rs/egui_table/0.11.0/egui_table/struct.Table.html), [README](https://github.com/rerun-io/egui_crates/tree/main/crates/egui_table).

### egui-data-table and others

- [egui-data-table](https://github.com/kang-sw/egui-data-table) 0.11 (MIT), on egui 0.35, holds the rows in a `Vec` and sorts and filters them through the hooks of a `RowViewer`. Its README lists undo and redo of every edit, showing, hiding and reordering columns, keyboard navigation and the system clipboard.
- [egui-selectable-table](https://docs.rs/egui-selectable-table/) 0.9 selects cells and rows by dragging; [egui-table-kit](https://docs.rs/egui-table-kit/) 0.8 extends `TableBuilder` with filters, row tags, trees and tables of rows loaded as they scroll into view.

## iced

### `table`

iced 0.14 (MIT) has a [`table`](https://docs.rs/iced/0.14.0/iced/widget/fn.table.html) widget: columns from `column()`, rows from any iterator, any `Element` in a cell.

- It builds, lays out and draws every row it is given.
- A column has a width and an alignment; the widget does not resize, sort or select.

Sources: [docs.rs](https://docs.rs/iced/0.14.0/iced/widget/fn.table.html), [table.rs](https://github.com/iced-rs/iced/blob/0.14.0/widget/src/table.rs).

### iced_table

[iced_table](https://github.com/tarkah/iced_table) 0.14 (MIT), for iced 0.14: a header, a body and an optional footer, with widgets in cells.

- The body is a scrollable column with an element for each row.
- Columns resize, and the application stores the new widths; it also keeps the header and footer scrolled with the body.
- No sorting, filtering or selection.

Source: [docs.rs](https://docs.rs/iced_table/0.14.0/iced_table/).

## Slint

[`StandardTableView`](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/standardtableview/) in Slint 1.19 (GPL-3.0-only, or Slint's royalty-free or commercial licence).

- Its rows sit in a `ListView`, which creates only the visible ones. Cells are `StandardListViewItem`s: text.
- A click on a header shows the sort arrow and calls back for the model to be sorted; the application sorts, for example with Slint's `SortModel` and `FilterModel`.
- One row is current (`current-row`); columns resize.
- A model (`ModelRc`) is not `Send`: another thread hands updates to the event loop with `invoke_from_event_loop`.

Sources: [StandardTableView](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/standardtableview/), [ModelRc](https://docs.rs/slint/1.19.0/slint/struct.ModelRc.html).

## GTK

[`ColumnView`](https://docs.gtk.org/gtk4/class.ColumnView.html), through gtk4-rs 0.11, "presents a large dynamic list of items using multiple columns with headers" and creates cell widgets for the visible items.

- Clicking a header sorts, once a sorter is attached to a sort model; columns resize and reorder interactively.
- The selection comes from the selection model; a `TreeListModel` with `TreeExpander` shows trees.

Source: [ColumnView](https://docs.gtk.org/gtk4/class.ColumnView.html).

## Dioxus

[dioxus-tabular](https://docs.rs/dioxus-tabular/0.3.0/dioxus_tabular/) 0.3 (MIT or Apache-2.0), for Dioxus 0.7.

- Each row has a key, "unique within the table and stable across re-renders".
- Each column owns its rendering, filtering and sorting; sorting spans several columns with priorities; columns reorder and hide; tables export to formats such as CSV.
- Its documentation does not say whether it draws only the visible rows.

Source: [docs.rs](https://docs.rs/dioxus-tabular/0.3.0/dioxus_tabular/).

## Outside a GUI toolkit

- In a terminal, [ratatui's `Table`](https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Table.html) displays data in formatted columns.
- In a web page rather than a Rust application, [Perspective](https://github.com/perspective-dev/perspective) is a WebAssembly "data grid, pivot table and charting engine for large, real-time and streaming datasets".
