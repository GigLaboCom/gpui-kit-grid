---
title: Replaceable behaviors
description: Every default behavior of the grid is a strategy an application can replace.
---

A grid makes many small decisions that reasonable products make differently: when a re-sorted order appears under the user's pointer, which press opens a menu, whether a double click edits or opens, what happens to a draft when the cell changes underneath it. gpui-kit-grid ships the common answer to each as a default, and makes every one of them replaceable.

## The pattern

Each behavior is one trait with its default implementation next to it, injected through a builder method:

- **A trait** names the role: `ReorderPolicy`, `EditGesture`, `ChangePainter`.
- **A default** implements it, usually the industry standard (Excel's keys, Windows' and macOS' selection conventions), and a few alternatives ship as more implementations of the same trait.
- **A builder method** `with_<behavior>(…)` on `LiveGridState`, `StoreOptions`, `GridStore::builder` or `ViewSpec` replaces it. Some have a `set_<behavior>(…, cx)` for run time as well.

There is no mode flag and no central registry: one seam per behavior, and an application that sets none of them gets the defaults.

```rust ignore
LiveGridState::new_standalone(schema, Trades, StoreOptions::default(), cx)
    .with_reorder_policy(HoldReorder::default()) // hold the order under the pointer
    .with_edit_policy(FollowLiveValue)           // untouched drafts follow the store
    .with_change_paint(ChangePaint::Directional(DirectionalChanges)) // arrows
```

Strategies called per cell or per frame (a change painter, an image source) are dispatched statically, or as an enum of the built-ins plus `Custom(Arc<dyn …>)`, and must not allocate or lock. Strategies the ingest or the engine call run in the background and are `Send + Sync`.

## The behaviors

### Order and background work

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| When a new display order replaces the shown one (`ReorderPolicy`) | `LiveGridState::with_reorder_policy`, `set_reorder_policy` | `LiveReorder`: at once | `HoldReorder::new(max)`: while the pointer is over the grid, at most `max` (3 s by default); `ManualReorder`: until `apply_held_order` |
| How projections are built (`ProjectionEngine`) | `with_engine` | `Rebuild`, several threads for a large snapshot | An incremental engine implements the same trait |
| Where background jobs run (`TaskRunner`) | `with_task_runner` | `BackgroundRunner`: GPUI's background executor | `InlineRunner`: on the calling thread |
| The clock of change stamps, fades and held orders (`Clock`) | `GridStore::builder(options).with_clock` | The GPUI executor clock | A test clock |
| How a reload meets live writes (`ReloadPolicy`) | `GridStore::builder(options).with_reload_policy` | `ReplayLive`: the snapshot, then the writes made during the load | A policy that knows row versions drops older writes |
| How queued writes collapse (`WriteCoalescing`) | `StoreOptions::with_write_coalescing` | `ByKey`: the latest write of a key replaces a queued one | `KeepEveryWrite` |

Every built-in reorder policy holds the order while the context menu or a cell editor is open, unless the new order comes from the user's own action.

### Sorting, filtering and structure

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| How a header click changes the sort (`SortCycle`) | `with_sort_cycle` | `StandardSortCycle`: ascending, descending, none; Shift adds a column | — |
| How quick filter text matches (`QuickFilterMatcher`) | `with_quick_filter_matcher`; columns: `with_quick_filter_columns` | `ContainsAllTerms` over the visible columns | Your own, off the UI thread |
| The order of sibling groups (`GroupOrder`) | `ViewSpec::with_group_order` | `KeyOrder`: by the group's key | `AggregateOrder::new(column, direction)` |
| Which groups have a footer (`GroupFooter`) | `ViewSpec::with_group_footer` | `NoGroupFooter` | `EveryGroupFooter` |
| An aggregate (`AggregateFn`) | `Aggregate::Custom` | `Sum`, `Avg`, `Min`, `Max`, `Count` | A weighted average, a median |
| Which rows of a filtered tree show (`TreeFilterRule`) | `TreeSpec::with_filter` | `TreeFilter::KeepAncestors` | `HideSubtree`, `Custom` |
| Which tree rows the totals add (`TreeTotalsRule`) | `TreeSpec::with_totals` | `TreeTotals::Leaves` | `Custom` |

### Selection, menus and expansion

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| How a press or a key edits the selection (`SelectionGesture`) | `with_selection_gesture` | `StandardSelectionGesture`: Windows and macOS conventions, navigation selects as in Excel | — |
| Which selected rows a command acts on (`SelectionScope`) | `with_selection_scope` | `ProjectionScope`: the selection within the rows on screen | `StoreScope`: rows a filter hides too |
| Which lines take the selection (`SelectionPolicy`) | `with_selection_policy` | `FocusCaptions`: captions take the focus only; tree rows are selected | `FocusBranches`: tree rows with children take the focus only, and a command acts on their leaves |
| Which press opens the context menu (`ContextMenuGesture`) | `with_context_menu_gesture` | `StandardContextMenuGesture`: the right button, Ctrl+click on macOS | — |
| Which press expands a caption or tree row (`ExpandGesture`) | `with_expand_gesture` | `StandardExpandGesture`: the chevron toggles, deep with Alt/Option; a double click on a caption | — |

### Columns and layout

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| Which columns move together (`ColumnMovePolicy`) | `with_column_move_policy` | `BandedMove`: a band stays together | `FreeMove`: a column may leave its band |
| How autofit measures (`AutofitPolicy`) | `with_autofit_policy` | `VisibleRowsAutofit` | — |
| How a layout becomes text (`LayoutCodec`) | `with_layout_codec` | `JsonLayoutCodec`, version 1 | A codec for a foreign format |
| How a saved layout meets a changed schema (`LayoutMerge`) | `with_layout_merge` | `AppendNewColumns` | — |
| What "Choose columns…" opens (`ColumnChooserFactory`) | `with_column_chooser` | `PopoverColumnChooser` | `ColumnChooserPanel` in your own popover or dialog |

### Painting and cells

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| How a change is painted (`ChangePainter`) | `with_change_paint`, `set_change_paint` | `ChangePaint::Uniform(UniformChanges)` | `ChangePaint::Directional(DirectionalChanges)`; `ChangePaint::Custom(Arc<dyn ChangePainter>)` |
| How a change highlight fades (`HighlightPolicy`) | `StoreOptions::with_highlight` | 5 s in 8 steps, no fade of removed rows | `with_removal_fade(duration)` |
| Where images come from (`CellImageSource`) | `with_image_source` | `AssetImageSource`: GPUI's asset loader | `AssetImageSource::default().with_capacity(n)`, your own |
| What a link does (`LinkOpener`) | `with_link_opener` | `OpenInBrowser` | Your own |

### Editing, drag and drop, copy and export

| Behavior | Builder | Default | Built-in alternatives |
|---|---|---|---|
| A column's editor (`CellEditor`) | `with_editor(column, editor)` | None: read-only | `TextEditor`, `NumberEditor`, `SelectEditor`, `DateEditor`, `CheckboxEditor` |
| Which input opens an editor, and where the focus goes after a commit (`EditGesture`) | `with_edit_gesture` | `StandardEditGesture`: Excel's | — |
| An edit under live data (`EditPolicy`) | `with_edit_policy` | `StandardEditPolicy`: the draft stays and shows a conflict | `FollowLiveValue`: an untouched draft follows the store |
| Which presses start a row drag (`RowDragGesture`) | `with_row_drag_gesture` | `NoRowDrag` | `SelectedRowsDrag` |
| Where a drop lands (`RowDropPolicy`) | `with_row_drop_policy` | `NoRowDrop` | `DropOnRows`, `DropBetweenRows` |
| How copied text is laid out (`Exporter`) | `with_exporter` | `TsvExporter` | `LineFilter` around any exporter |
| How a copied or exported cell is written (`CellExporter`) | `with_cell_export`, `ExportRequest::with_cell_export` | `RawCells`, with the decimals shown | `DisplayedCells`, `RawCells::new().with_precision(NumberPrecision::Full)` |
| A file format (`ExportFormat`) | an argument of `export`, `export_to_path` | — | `CsvFormat`, `TextFile`, your own (xlsx with your library) |

## Writing your own

An implementation is a type with the trait's methods; most are a few lines. A change painter from the `cookbook` example flashes updates in one column only, and new and removed rows whole:

```rust src="crates/grid/examples/cookbook.rs#flash-some-columns"
```

It goes in with `with_change_paint(ChangePaint::Custom(Arc::new(OnlyColumn(QuoteColumn::Volume.ix()))))`.

The `orders` example has two more: a `RowDropPolicy` that takes drops only on an account's group caption, and an `EditGesture` that keeps the double click for the primary command.

Each trait's rustdoc in the [API reference](../../api/) says when it is called, on which thread, and what it must not do. A hook called per cell or per frame must not allocate or lock, and no hook may read or update the grid entity while the grid calls it.
