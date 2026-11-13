---
title: Columns and layout
description: The schema, frozen zones, bands, icon headers, the column chooser, LiveGridHandle, saved layouts and the GridColumns derive.
---

The schema lists the columns once, in their default order. Users then resize, move, freeze, hide and autofit them; the grid keeps all of that by column id in a layout that an application saves and restores.

## The schema

`GridSchema::new()` takes `GridColumn`s and `ColumnBand`s in order. The `dock_layout` example groups three columns under an Execution band and hides Fee by default:

```rust src="crates/grid/examples/dock_layout.rs#banded-schema"
```

| `GridColumn` builder | Sets |
|---|---|
| `with_width`, `with_min_width`, `with_max_width` | The width and its limits |
| `with_zone(ColumnZone::FrozenLeft / Scrolling / FrozenRight)` | Where the column sits; `GridSchema::with_frozen(n)` freezes the first `n` on the left |
| `with_align` | Left, right, centre or by value (`Auto`: numbers right, text left) |
| `with_visible(false)` | Hidden at start |
| `with_resizable`, `with_movable`, `with_sortable`, `with_groupable`, `with_hideable` | What the user may do with it; all on by default |
| `with_icon(icon)`, `with_icon_only(icon)` | An icon before the title, or the icon alone; the chooser, the tooltip and screen readers keep the title |
| `with_number_format`, `with_image_fit`, `with_image_corner_radius` | How values show ([Cells](../cells/)) |
| `with_content(CellContent::Element / Control)` | A real element per visible cell ([Cells](../cells/#elements-in-cells)) |
| `with_internal(true)` | A helper column the view can sort, group and aggregate by but nobody sees |

An internal column stays hidden whatever a layout, Show all or `set_column_visible` say; the chooser leaves it out, and a copy or an export of the visible columns does not write it (one that names it in `ExportRequest::with_columns` does).

The `orders` schema freezes Id on the left and a Cancel button column on the right, and shows Status as an icon-only header:

```rust src="crates/grid/examples/orders/model.rs#orders-schema"
```

## What users do

- **Resize** by dragging a header's edge; a double click on the edge autofits the column to its visible rows (`with_autofit_policy` replaces the measure). `autofit_column` and `autofit_columns` do it from code.
- **Move** by dragging a header, or with Move left and Move right in its menu. A band moves as a whole and its columns stay inside it (`BandedMove`); `with_column_move_policy(FreeMove)` lets a column leave its band.
- **Sort** with a click on a header: ascending, descending, none; Shift adds a column to the sort (`with_sort_cycle` replaces the cycle).
- **The header menu** has the sort commands, Group by this column, Freeze left, Freeze right and Unfreeze, Move left and Move right, Autofit, Hide column, Show all columns, Choose columns… and Reset columns.

From code: `set_column_visible`, `show_all_columns`, `move_column`, `move_band`, `set_column_zone`, `set_column_width`, all by column.

## The column chooser

"Choose columns…" in the header menu opens a popover: the columns by zone, band members indented, a search field from 10 columns on, a checkbox to show or hide each, a grip and Alt+↑/↓ to reorder, Show all and Reset columns. Changes apply at once and arrive as `LayoutChange`, like a change made with the mouse.

`show_column_chooser(window, cx)` opens it from a toolbar. `ColumnChooserPanel` is the same panel as an entity, for an application that shows it in its own popover, dialog or sheet; `with_column_chooser` replaces what "Choose columns…" opens.

## LiveGridHandle

A toolbar or a status bar drives any grid without knowing its delegate type through `LiveGridHandle::new(&grid)`. It reads the columns (`ColumnInfo`, `BandInfo`) and commands the grid by column id: visibility, order, zones, widths, the layout, the sort, grouping, expansion, the quick filter, counts, the held order and the reorder policy, copy and export to a path. An unknown id returns `false`. `blotter`'s status bar follows its grid through one:

```rust src="crates/grid/examples/blotter/main.rs#handle-subscribe"
```

Each handle method is one read or one update of the grid: never call it from inside an update of the same grid, such as a delegate hook.

## Layouts

A `GridLayout` holds the columns by id in display order, each with its width, visibility and zone, the sort, and the grouping with the expansion of its groups. It does not hold the quick filter, the scroll position, the selection, a tree, or a grouping by a function.

| Method | Does |
|---|---|
| `layout()`, `default_layout()` | The current layout, and the schema's |
| `with_layout(layout)`, `set_layout(&layout, cx)`, `reset_layout(cx)` | Apply a layout at start, later, or go back to the schema's |
| `export_layout()`, `import_layout(text, cx)` | The layout as text (`JsonLayoutCodec`, version 1) and back |
| `with_view_id(id)` | Names the kind of grid: a layout saved for another view is refused |
| `with_layout_codec`, `with_layout_merge` | Another text format; another rule for a layout that meets a changed schema |

A saved layout survives schema changes: unknown columns are ignored and new ones are added (`AppendNewColumns`). Save it on `LayoutChange`, `SortChange`, `GroupChange` and `ExpansionChange`; the [Cookbook](../../cookbook/#save-and-restore-the-layout) has the code, and `dock_layout` keeps named layouts and each dock panel's grid in one file.

## Deriving GridColumns

With the `derive` feature, `#[derive(GridColumns)]` writes the column enum's implementation:

```toml
gpui-kit-grid = { git = "https://github.com/GigLaboCom/gpui-kit-grid", tag = "v1.0.0", features = ["derive"] }
```

```rust src="crates/grid/tests/derive.rs#derived-columns"
```

The declaration order is the schema order, and an id is the variant name in `snake_case`, as a hand-written implementation spells it, so saved layouts survive the switch. `#[grid_columns(rename_all = "…")]` changes the rule and `#[grid_column(id = "…")]` sets one id:

```rust src="crates/grid/tests/derive.rs#derive-rename"
```

A missing variant, a shifted index, a repeated id and more than 128 columns are compile errors. The feature is off by default, and the hand-written implementation stays supported.
