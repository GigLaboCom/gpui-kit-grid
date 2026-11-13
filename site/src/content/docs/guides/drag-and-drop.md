---
title: Drag and drop
description: Drag selected rows out of a grid, and take drops on rows, between rows or on group captions.
---

Rows can be dragged out of a grid and dropped onto another grid, onto a group caption, or onto any part of the application that takes them. The grid draws the gesture and reports the drop; moving or copying the data is the application's. The `orders` example drags selected orders into a basket grid, or onto an account's caption to move them to that account.

## Turning it on

Row dragging is off by default (`NoRowDrag`, `NoRowDrop`): without a drop target the gesture would only get in the way of drag-selection. The source grid sets a drag gesture, the target grid a drop policy. `orders`' grid does both (the last two lines):

```rust src="crates/grid/examples/orders/main.rs#orders-grid"
```

and its basket takes drops on its rows:

```rust src="crates/grid/examples/orders/main.rs#basket-grid"
```

## The gesture

`SelectedRowsDrag` is a two-phase gesture: a press on a row that is already selected drags the selection once the pointer is 10 px away (`with_threshold` changes the distance); a press on an unselected row selects it and drag-selects as before.

`LiveGridDelegate::drag_payload` adds application data and the drag ghost's label (`DragPayload::new().with_label("3 orders")`); the keys always come along.

## Where a drop lands

| Policy | Lands |
|---|---|
| `NoRowDrop` (default) | Nowhere: the grid takes no drop |
| `DropOnRows` | On rows, group captions and the empty area |
| `DropBetweenRows` | Before or after a row by the half the pointer is in; in a tree the top third is before, the middle onto the row, the bottom third after; onto group captions. Refuses a sorted flat view, where a place between rows would not hold |
| Your own `RowDropPolicy` | Wherever `place` says |

`orders` takes drops only on an account's caption:

```rust src="crates/grid/examples/orders/main.rs#account-drop"
```

While a drag is over a grid, the target is highlighted, the rows scroll near an edge, and the cursor shows whether the grid takes the drop. Pinned rows, group footers and totals never take one. Escape cancels the drag.

## What a grid takes

A grid takes rows from grids with the same key type, and values of types registered with `with_drop_payload::<T>()`, so a panel or another part of the application can be the source or the target.

## The drop

The grid dropped on emits `LiveGridEvent::Drop { source, target, modifiers }`. `orders` moves the dropped orders to the account of the caption:

```rust src="crates/grid/examples/orders/main.rs#drop-on-caption"
```

With Option or Ctrl held the cursor is the copy cursor (`RowDropPolicy::is_copy` changes the rule), but copy or move is still the application's choice, from `modifiers`.

## Limits

- Rows do not leave the application as files.
- A drop onto an element in a cell, or onto an open editor, is not taken: GPUI drops only onto the element under the pointer.
- Starting a drag of 100k selected rows takes about 4 ms on the UI thread.
