---
title: Settings
description: Every setting of the grid and its store in one place, with where it is set and its default.
---

Everything a grid does without being told can be changed: timings, policies, appearance and behaviors. This page lists the settings with where they are set and their defaults; the guides explain each one, and [Replaceable behaviors](../../concepts/replaceable-behaviors/) lists the strategies by role.

Settings of the **store** (`StoreOptions`, `GridStore::builder`) apply to every grid over it and are fixed when it is built. Settings of the **view** (`LiveGridState::with_…`) belong to one grid; the ones with a `set_…` change at run time. Settings of the **element** (`LiveGrid::new(&grid)…`) are made in `render`.

## Data and timing

| What | Where | Default |
|---|---|---|
| How often to re-sort under live data | `GridTuning::with_min_rebuild_interval`, through `with_tuning` | 100 ms |
| How often the view pulls snapshots | `GridTuning::with_pull_interval` | 8 ms |
| How often the store publishes snapshots | `StoreOptions::with_publish_interval` | 8 ms: under a fast stream each publish copies the changed pages, so publishing more often than a grid pulls only copies pages no grid reads |
| How queued writes collapse | `StoreOptions::with_write_coalescing` | `WriteCoalescing::ByKey` |
| Highlight duration and steps | `StoreOptions::with_highlight(HighlightPolicy::new(duration, steps))` | 5 s, 8 steps |
| Fading removed rows | `HighlightPolicy::default().with_removal_fade(duration)` | Off |
| Clock for highlight stamps, fading and held orders | `GridStore::builder(options).with_clock(clock)` | The GPUI executor clock |
| A reload over a live stream | `writer.begin_reload()`, then `reload.finish(rows)`; `GridStore::builder(options).with_reload_policy(policy)` | `ReplayLive`: the snapshot, then the writes made during the load |
| Several grids over the same data | `GridStore::new(options, cx)`, then `LiveGridState::new(schema, delegate, store.clone(), cx)` | One store per grid (`new_standalone`) |
| Pinned rows | `writer.set_pinned_rows(edge, rows)`, from any thread | None |
| Progress | `writer.set_progress(…)` from any thread; `LiveGridState::set_progress` for the view's own; `LiveGridDelegate::render_progress` for the look | A thin bar under the header; the label only on an empty grid |
| Waiting until writes are on screen | `LiveGridState::flush(cx)` → `Task<Option<FlushOutcome>>`; `writer.flush()` for the store alone | — |

A `Reload` dropped without `finish` cancels the reload, and the queued writes apply as usual. A new `begin_reload` replaces an unfinished one, whose `finish` is then ignored.

## Order and background work

| What | Where | Default |
|---|---|---|
| The order engine | `with_engine`; threads: `Rebuild::default().with_max_threads(n)` | `Rebuild`: a large snapshot on several threads (cores − 2, at most 8) |
| Where background work runs | `with_task_runner`: `BackgroundRunner`, `InlineRunner` | `BackgroundRunner` |
| When to show a new order | `with_reorder_policy`, `set_reorder_policy`: `LiveReorder`, `HoldReorder::new(max)`, `ManualReorder` | `LiveReorder`: at once |
| A held order | `is_order_held`, `apply_held_order`; `LiveGridEvent::OrderHeldChange { held }` | — |
| Header click sorting | `with_sort_cycle` | Ascending → descending → none; Shift adds a column |
| The view: sort, filter, grouping, tree | `with_view(ViewSpec)`, `set_view` | Unsorted, unfiltered, flat |
| Quick filter | `set_quick_filter(text, cx)`; columns: `with_quick_filter_columns`; matching: `with_quick_filter_matcher` | `ContainsAllTerms` over the visible columns |

`set_view` and a new data set (`replace_all`, `clear`) show the new order at once under any reorder policy.

## Structure

| What | Where | Default |
|---|---|---|
| Grouping | `ViewSpec::with_group_by`, `set_group_by(levels, cx)`; per column: `GridColumn::with_groupable` | None; every column groupable |
| Group panel | `LiveGrid::new(&grid).group_panel(true)` | Hidden |
| Aggregates | `ViewSpec::with_aggregates`, `set_aggregates(aggregates, cx)` | None |
| Group order and footers | `ViewSpec::with_group_order(AggregateOrder::new(…))`, `with_group_footer(EveryGroupFooter)` | By key; no footers |
| Totals row | `with_totals(PinnedEdge::Bottom)`, `set_totals(Some(edge), cx)` | None |
| Expansion | `with_expand_depth`, `set_expand_depth`, `set_expanded`, `is_expanded` | Everything expanded |
| Tree | `ViewSpec::with_tree(TreeSpec::new())`, `set_tree(Some(spec), cx)`; `TreeSpec::with_column`, `with_filter`, `with_totals` | None; `KeepAncestors`; totals over the leaves |
| Captions and tree rows in the selection | `with_selection_policy`: `FocusCaptions`, `FocusBranches` | `FocusCaptions` |
| How a press expands | `with_expand_gesture` | The chevron toggles, deep with Alt/Option; a double click on a caption toggles |

## Appearance

| What | Where | Default |
|---|---|---|
| Row height | `LiveGrid::new(&grid).xsmall()`, `small()`, `large()`, `with_size(…)` | `medium`, 32 px |
| Frame | `LiveGrid::new(&grid).bordered(false)` | Bordered |
| Accessible name | `LiveGrid::new(&grid).accessibility_label("Trades")` | None |
| Change paint | `with_change_paint(…)`, `set_change_paint(paint, cx)` | `ChangePaint::Uniform(UniformChanges)`: no arrows |
| Cell and aggregate styles | `LiveGridDelegate::cell_style`, `aggregate_style` | The theme's table tokens |
| Elements in cells | `GridColumn::with_content(CellContent::Element / Control)`, `LiveGridDelegate::render_cell` | Text |
| Images in cells | `with_image_source(…)`; `GridColumn::with_image_fit`, `with_image_corner_radius` | GPUI's asset loader, every image kept; `ImageFit::Contain` |
| Number format | `GridColumn::with_number_format(NumberFormat::new().with_decimals(n))` | Two decimals for floats |
| Header icon, hiding | `GridColumn::with_icon`, `with_icon_only`, `with_hideable` | The title; every column can be hidden |
| Menu and tooltips | `LiveGridDelegate::context_menu`, `primary_command`, `tooltip`, `header_tooltip` | Copy, Copy all, sorting and column commands; no primary command; the full text of a truncated cell or title |

Fonts come from GPUI's text style around the grid, with tabular figures; colours from the gpui-kit theme's semantic tokens.

## Selection and layout

| What | Where | Default |
|---|---|---|
| Selection mode | `with_selection_mode`, `set_selection_mode`: `RowSingle`, `RowSet`, `CellSingle`, `CellSet` | `RowSingle` |
| What a command acts on | `with_selection_scope`: `ProjectionScope`, `StoreScope` | `ProjectionScope`: the selection within the rows on screen |
| Layout persistence | `layout()`, `with_layout`, `set_layout`, `export_layout`, `import_layout`; `with_view_id`, `with_layout_codec`, `with_layout_merge` | `JsonLayoutCodec`, version 1; new columns appended |
| Column chooser | `with_column_chooser`, `show_column_chooser`; `ColumnChooserPanel` in your own popover | The grid's popover; search from 10 columns |

## Editing, drag and drop, copy and export

| What | Where | Default |
|---|---|---|
| Editors | `with_editor(column, editor)`, `set_editor`; `LiveGridDelegate::is_editable`, `validate_edit`, `render_edit_message` | None: read-only |
| How an edit starts and where the focus goes | `with_edit_gesture` | `StandardEditGesture`: Excel's |
| Edits under live data | `with_edit_policy`: `StandardEditPolicy`, `FollowLiveValue` | The draft is kept and a conflict shows; an accepted value waits up to 5 s for the store |
| Links | `with_link_opener` | `OpenInBrowser` |
| Row drag and drop | `with_row_drag_gesture(SelectedRowsDrag::new())`, `with_row_drop_policy(DropOnRows)` or `DropBetweenRows`, `with_drop_payload::<T>()`; `LiveGridDelegate::drag_payload` | Off; the copy cursor with Option or Ctrl |
| Copy format | `with_exporter`, `with_cell_export`; which lines: `LineFilter` | `TsvExporter`; numbers with the decimals shown, `TRUE`/`FALSE`; every line |
| Copy with groups | `copy_scope(ExportScope::Visible / Expanded / Selection, cx)` | Copy all: as shown, with captions, footers and totals |
| Export to files | `export`, `export_to_path`, `export_with`; `ExportRequest`; `CsvFormat` or your own `ExportFormat` | CSV, RFC 4180, UTF-8 with a byte order mark |
