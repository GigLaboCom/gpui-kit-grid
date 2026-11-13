---
title: Copy and export
description: Copy as TSV, export CSV files in the background, and write your own formats such as xlsx.
---

The grid copies to the clipboard and exports to files from one snapshot of what it shows. The rows, their order and the columns are taken on the UI thread at once; the text or the file is written in the background.

## Copy

- Cmd/Ctrl+C copies `scoped_selection()`, the selected rows on screen in display order, as TSV.
- Copy all, in the context menu, copies every line on screen with the headers, pinned rows, group captions, footers and totals included.
- `copy_scope(ExportScope::Visible / Expanded / Selection, cx)` copies a scope from code; `Expanded` copies the grid as if every group were expanded.

Numbers are copied as raw values with the decimals the grid shows (`NumberPrecision::Shown`), booleans as `TRUE` and `FALSE`. `with_cell_export` changes that for every copy: `CellExport::Displayed(DisplayedCells)` copies the painted text, and `RawCells::new().with_precision(NumberPrecision::Full)` every digit. `with_exporter` replaces the TSV layout, and `LineFilter` leaves kinds of lines (captions, footers, totals, pinned rows) out of any exporter.

## Export to a file

```rust src="crates/grid/examples/orders/main.rs#export-to-path"
```

`export_to_path(request, format, path, cx)` returns an `ExportJob`:

- `job.progress()` reports the rows written; `job.cancel()`, or dropping the job, cancels.
- Awaiting it gives the outcome: `data_rows()`, `output()`.
- It writes `<path>.part` beside the file and renames it at the end, so a failed or cancelled export leaves the old file as it was.
- A cell whose commit is still waiting is written as shown; `ExportCell::is_pending` tells.

`export(request, format, writer, cx)` writes to any `io::Write` instead (on the web, a `Vec<u8>`; there is no `export_to_path` there). The file dialog, opening the file and the menu item are the application's: `orders` asks for the path with `App::prompt_for_new_path` first.

`ExportRequest` picks the scope, the columns (`with_columns`, which can name hidden and internal columns), the header and the cell format. Nothing to export fails with `ExportError::Nothing`; `is_export_empty` checks it before the file dialog, and `with_empty(EmptyExport::HeaderOnly)` writes the header anyway.

## CSV

`CsvFormat` follows RFC 4180: commas, CRLF and a UTF-8 byte order mark, so Excel opens the file without the import wizard. `CsvFormat::default().with_encoding(CsvEncoding::Utf8)` drops the mark. Fields that start with `=`, `+`, `-` or `@` are written as they are.

## Your own format

`export_with(request, |table, cells, control| …, cx)` hands the table to a closure that runs in the background and returns whatever it builds. The [Cookbook](../../cookbook/#your-own-export-format) writes a Markdown table this way.

For a format you reuse, implement `ExportFormat` and pass it to `export` or `export_to_path`. It must call `ExportControl::advance` at least once per 1024 rows and stop with the error it returns: that is how progress shows and how a cancelled export stops.

### xlsx

The grid has no spreadsheet dependency. Write xlsx with your own library in an `ExportFormat` or `export_with`: `ExportTable` carries what a spreadsheet needs, and the rustdoc of `ExportFormat` maps it to xlsx.

| The table tells | A spreadsheet makes of it |
|---|---|
| `ExportCell::value`: `Int`, `Float`, `Decimal`, `Bool`, `Text`, `Timestamp` | A number, a boolean, a string or a date and time cell |
| `ExportCell::number_hint` | The number format: the digits after the point, as on screen |
| `ExportColumn::title` | The header row |
| `ExportColumn::bands`, `ExportTable::band_depth` | Rows of merged header cells above the titles |
| `ExportColumn::width`, `ExportColumn::align` | The column width and its horizontal alignment |
| `ExportTable::has_header`, `ExportTable::frozen_columns`, `ExportRow::band` | The frozen panes: the header rows, the leading columns and the top band |
| `ExportRow::depth`, `ExportRow::is_hidden`, `ExportRow::branch` | The outline level of a row, and rows hidden under a collapsed level |
| `ExportRow::line`, `ExportCell::is_aggregate` | Summary rows set apart, or formulas in place of their values |
| `ExportCell::is_pending`, `ExportCell::row_value` | An edit not yet confirmed: its value, or the stored one |

A spreadsheet keeps 15 significant digits: an `Int` beyond 2^53 loses its last digits unless the format writes it as text.

## Performance

Copying all of 100k rows × 39 columns (56 MB of text) takes about 180 ms in the background and at most 0.04 ms on the UI thread; a CSV export of the same table about 200 ms ([Performance](../../performance/)).
