---
title: Cells
description: Cell values, number formats, styles, tooltips, and real elements in cells.
---

Most cells are painted by the grid's body directly: text, numbers, times, icons, images and links, with no element per cell. A column that needs a real gpui-kit element (a progress bar, a checkbox, a button) asks for one, and gets it only in visible rows.

## Values

`GridRow::value` returns a `CellValue` that borrows from the row, so it never allocates. `orders` uses most kinds:

```rust src="crates/grid/examples/orders/model.rs#order-value"
```

| Variant | Shows | Notes |
|---|---|---|
| `Empty` | Nothing | |
| `Bool` | `TRUE` / `FALSE` | |
| `Int`, `Float` | A number, aligned right | A `Float` shows two decimals unless its column says otherwise |
| `Decimal` | A number with its own scale | With the `decimal` feature; sums exactly |
| `Text` | Text | |
| `Timestamp` | UTC as `2026-11-04 12:34:56`, aligned left | Another zone or format is a `Custom` value with its own text |
| `Icon(IconValue::new(path, text))` | A one-colour SVG asset in the text colour, centred | 14 px in rows up to 30 px, 16 px above |
| `Image(ImageValue::new(id, text))` | A raster image or a colour SVG | Found by the grid's image source |
| `Link(LinkValue::new(text, url))` | The text as a link | A click on the text or Enter on the cell opens it |
| `Custom(&dyn GridValue)` | A value of your own type | Sorts, writes and exports by its own rules |

An icon, an image or a link comes with its text, and sorting, the quick filter, copy, export, the tooltip and a screen reader use the text.

### Number formats

`GridColumn::with_number_format(NumberFormat::new().with_decimals(1))` shows a column's floats, and its aggregates, with one decimal. A copy, a CSV file, the quick filter and an export's `NumberHint` follow the column's format, so they give what the grid shows (`-0.49`, not `-0.4899999999999949`). `Decimal` keeps its own scale.

### Images

The default image source, `AssetImageSource`, loads through GPUI's asset loader (the application's assets, files, `http(s)://` addresses) and keeps every image it loaded. `AssetImageSource::default().with_capacity(n)` keeps the `n` most recently used, and `with_image_source` takes your own `CellImageSource`. `GridColumn::with_image_fit` and `with_image_corner_radius` set how an image sits in its cell.

### Links

A `Link` shows its text in the theme's `link` colour, underlined, with the pointer cursor over the text (the one place the grid shows it). It sorts, filters and copies as its text; an export format finds the address in `ExportCell::value`, and a screen reader hears "link" after it. `with_link_opener` replaces `OpenInBrowser` (`App::open_url`) with your own `LinkOpener`.

### Values of your own type

`Custom(&dyn GridValue)` takes any type that implements `GridValue`. `ops` ranks alert severities worst first, and reads them as their label:

```rust src="crates/grid/examples/ops/rows.rs#severity-value"
```

- `cmp_value` orders it, for sorting, grouping and `Min`/`Max` aggregates.
- `write_text` writes what the cell shows, and what a copy, the quick filter and a screen reader read.
- `export_value` writes what an export writes.
- `to_owned_value` returning `OwnedValue::custom(self.clone())` keeps the value itself as a group's key.

They run in prepaint and in background sorts, so they must not allocate or lock.

## Styles

`LiveGridDelegate::cell_style` adjusts each visible cell's style after the change painter, through `CellStyle`: `set_background`, `set_foreground`, `set_font_weight`, `set_trend` (an arrow) and `set_error_mark`. The [quick start](../../start/#3-the-delegate-styles-cells) colours sell trades. `CellStyleContext` gives the row, the column, the value, the change and the edit state of the cell.

`aggregate_style` does the same for aggregate cells on captions, footers and totals, with `AggregateLine` to tell them apart. Colours come from the gpui-kit theme's semantic tokens (`table_*`, `success`, `warning`, `danger` and the rest), so a grid follows the application's theme.

## Tooltips

A cell whose text is truncated shows its full text as a tooltip, and so does a truncated header. `LiveGridDelegate::tooltip` returns your own for a cell, `header_tooltip` for a header.

## Elements in cells

A column with `CellContent::Element` (a progress bar, a tag) or `CellContent::Control` (a checkbox, a button) gets a real gpui-kit element in each visible cell from `LiveGridDelegate::render_cell`:

```rust src="crates/grid/examples/orders/model.rs#orders-schema"
```

In `orders`, the Filled column is a progress bar:

```rust src="crates/grid/examples/orders/main.rs#render-cell-filled"
```

and Active is a checkbox that commits like an editor, by click or by Space on the focused cell:

```rust src="crates/grid/examples/orders/main.rs#render-cell-active"
```

- `render_cell` runs in prepaint, for visible cells only. It may allocate (an element is an allocation) but must not read or update the grid entity; the element's own handlers run later and may write to the store.
- Return `None` for a column that should show its value as text.
- The element lives inside its row's element, keyed by row key and column id, so its state follows the row through a re-sort.
- A press reaches the element first. Unless the element stops it, it is also a press on the cell: selection, the menu, a double click and `Click` work as on any cell.
- The keyboard focus stays with the grid, which is one Tab stop: give focusable elements `tab_stop(false)`. Space on the focused cell of a `Control` column calls the handler registered with `CellElementContext::on_activate`; Enter stays the primary command.
- A fading row keeps its elements until the fade ends: `cell.change()` shows `RowChange::Removed`, and `change.intensity()` how far it has faded. They take no press, Space or edit.
- Group captions, footers and totals show text, not elements. An element sits inside the cell's padding.
