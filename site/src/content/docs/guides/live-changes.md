---
title: Live changes
description: Change highlight, direction arrows, fading removed rows, holding the order, pinned rows and progress.
---

The ingest compares every new row with the old one column by column, so the grid knows which cells changed, and in which direction, without the application saying so. This page covers how those changes show.

## Highlight

A changed cell is highlighted and fades out in steps on one shared timer, never with an animation frame per cell: 5 s in 8 steps by default (`StoreOptions::with_highlight(HighlightPolicy::new(duration, steps))`). Inserted rows flash too. A new `Arc` with the same column values is published without highlight.

How a change is painted is a `ChangePaint`:

- `ChangePaint::Uniform(UniformChanges)` (the default): inserts in the theme's `success` colour, updates in `warning`, as a background only.
- `ChangePaint::Directional(DirectionalChanges)`: a rise in `success` with ↑, a fall in `danger` with ↓. The arrow sits on the side away from the text, so the text does not move, and a screen reader hears "rising" or "falling".
- `ChangePaint::Custom(Arc<dyn ChangePainter>)`: your own painter. It learns the cell's column from `CellChange::column_ix()`, so it can flash some columns and leave others quiet ([Replaceable behaviors](../../concepts/replaceable-behaviors/#writing-your-own)).

`with_change_paint` sets it at start, `set_change_paint(paint, cx)` at run time. `blotter` switches between the two built-ins with a toolbar toggle:

```rust src="crates/grid/examples/blotter/main.rs#change-paint"
```

`LiveGridDelegate::cell_style` runs after the painter and still has the last word.

## Directions

The direction of a number is the sign of its change. `GridRow::change_direction` decides it for other values, or overrides it for a column where up is bad (a spread, a latency).

## Aggregates

Group captions, footers and the totals row flash when their aggregate changes, and a new group's caption flashes when it appears.

## Fading removed rows

A removed row can fade out in place instead of vanishing. Fading belongs to the store and is set once, at start:

```rust src="crates/grid/examples/blotter/main.rs#removal-fade"
```

- A fading row keeps its place, in `danger` with muted text.
- It is inert: no clicks, selection, focus, tooltip or screen reader node, and it counts in no group, aggregate or row count.
- Its elements in cells stay until the fade ends: `render_cell` sees `RowChange::Removed` in `cell.change()` and can fade them by `change.intensity()`.
- Fading is off by default; 1 s works well.

## Reduced motion

With the system's reduced motion setting on, highlights, arrows and fades go out in one step.

## Holding the order

Under a fast stream a sorted grid reorders all the time, which is hard to read and hard to click. The reorder policy decides when a new order replaces the one on screen:

| Policy | When a new order shows |
|---|---|
| `LiveReorder` (default) | At once |
| `HoldReorder::new(max)` | When the pointer leaves the grid, or after `max` (`HoldReorder::default()` is 3 s) |
| `ManualReorder` | When the application calls `apply_held_order` |

Every built-in policy also holds the order while the context menu or a cell editor is open. `set_view` and a new data set (`replace_all`, `clear`) show the new order at once under any policy, and so does a user's own action, such as a click on a header.

While an order is held the values keep updating in place, an accent line shows under the header, and `LiveGridEvent::OrderHeldChange { held }` reports it, for a "Refresh order" button. The [Cookbook](../../cookbook/#hold-the-order-while-the-user-reads) has the code.

## Pinned rows

`writer.set_pinned_rows(PinnedEdge::Top, rows)` (or `Bottom`) pins rows above or below the data rows, from any thread and in order with the other writes:

- they do not scroll vertically, and are highlighted by key like data rows;
- their keys are their band's own;
- they are not selected; Copy all includes them.

The totals row of [Grouping](../grouping/#footers-and-totals) is the outermost pinned row of its band.

## Progress

`writer.set_progress(Some(GridProgress::indeterminate().with_label("Loading trades")))`, or `GridProgress::new(fraction)`, shows a thin bar under the header, with its label on an empty grid; `set_progress(None)` clears it. A progress write is never reordered before the rows written before it. `LiveGridState::set_progress` sets the view's own progress over the store's, and `LiveGridDelegate::render_progress` replaces the look.
