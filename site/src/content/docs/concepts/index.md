---
title: Concepts
description: Rows by key, background ingest and snapshots, background rebuilds, and what the grid is for.
---

gpui-kit-grid is a live table of state, not a log viewer. Producers write rows by key from any thread, and repeated updates of a key collapse into one row with its latest value: the grid shows what is true now for every key, not every event that led there. Sorting, filtering, grouping and trees are rebuilt from that state in the background, and only visible cells are painted, so scrolling, selection and commands stay responsive while thousands of updates a second arrive.

## The pipeline

```text
  app threads ──GridWriter──▶ ingest (background, single writer)
                                │  rows in pages of 1024, copy-on-write
                                │  column diff → highlight stamps
                                ▼
                            StoreSnapshot (immutable, with a key index)
                                │  pulled at most every 8 ms
                                ▼
                            LiveGridState (view) ──background──▶ rebuild, compact,
                                │                                aggregate, expand
                                ▼                                → projection
                            GridBody: one custom element, quads and glyphs
```

1. **Writers.** A `GridWriter` is `Send + Sync + Clone`; any thread holds one. Its writes go into a queue and return at once.
2. **Ingest.** One background task per store owns the rows. It applies the queued writes, compares each new row with the old one column by column, stamps the changed cells for highlighting with their direction, and publishes an immutable `StoreSnapshot`. Rows live in pages of 1024 that the snapshot shares with the previous one, so a publish copies only the pages that changed.
3. **The view.** `LiveGridState` pulls the newest snapshot at most every 8 ms, one update and one repaint per pull. When the change affects the order (a sorted or filtered column, a new or removed row), it starts a background job that builds a new *projection*: the rows on screen, in order, with group captions, footers and tree levels.
4. **The body.** One custom element paints the visible cells as quads and glyphs, with no element per cell. Only columns that ask for real elements (a progress bar, a checkbox) get them, and only in visible rows.

## Rows by key

Every row has a key (`GridRow::Key`), and everything the grid remembers is keyed by it: the selection, the focus, the anchor of a Shift range, highlights, an open editor, a menu or click target, expanded tree rows. Positions on screen are transient. A re-sort, a filter or an insert above never moves the selection to another row, and a removed row simply leaves it.

Columns work the same way: `ColumnId` (the `id` of your column enum) is what layouts and events name, while the schema index (`ColumnIx`) and the position on screen (`ViewColumnIx`) are distinct types that never mix.

## Writing

| `GridWriter` method | What it does |
|---|---|
| `upsert(row)` | Inserts or replaces a row by key. |
| `remove(key)` | Removes a row by key; it leaves the screen on the next frame, or fades out first (see [Live changes](../guides/live-changes/)). |
| `replace_all(rows)`, `clear()` | A new data set: the order on screen follows at once, under any reorder policy. |
| `batch(\|b\| …)` | Several writes that land in one snapshot. |
| `snapshot()` | The latest published snapshot, for reading a row by key (`snapshot().get(&key)`, about 20 ns). |
| `begin_reload()` → `finish(rows)` | A reload under a live stream: live writes wait while the snapshot loads, then apply on top of it (the [cookbook](../cookbook/#reload-under-a-live-stream) shows it). |
| `set_pinned_rows(edge, rows)` | Rows pinned above or below the data, in order with the other writes. |
| `set_progress(progress)` | A thin progress bar under the header, with its label on an empty grid. |
| `flush().await` | Resolves with the store's version once every write before it is published. |
| `is_closed()` | Whether every grid over the store is gone, so a producer can stop. |

A new `Arc` whose column values are the same as before is still published, without highlight or re-sorting, so hooks see data outside the columns; the same `Arc` again does nothing.

On the UI thread, `grid.update(cx, |grid, cx| grid.flush(cx))` returns a task that resolves once the order on screen reflects every write before the call, for example to select the first row after a load.

## Collapsing by key

While the ingest is busy, a later upsert of a key that is still queued takes the earlier one's place (`WriteCoalescing::ByKey`, the default). The queue then grows with the number of keys written, not with the number of writes: a hot key updated a thousand times between two publishes costs one row's work. `StoreOptions::default().with_write_coalescing(WriteCoalescing::KeepEveryWrite)` keeps every write instead.

The collapse keeps pace with the data rather than a fixed server interval: a quiet stream is shown write by write, a storm one snapshot per pull. The [performance](../performance/#keyed-storm-s42) page measures both policies under a storm.

## Background rebuilds

Sorting, filtering, grouping, aggregates and trees run on a snapshot in the background, never on the UI thread, and the UI thread does no work that grows with the total row count in a frame.

- A rebuild after live changes runs at most once per `min_rebuild_interval` (100 ms by default, `GridTuning`). A user's action (a click on a header, a new filter) starts one at once.
- The default engine, `Rebuild`, sorts a large snapshot on several threads (cores − 2, at most 8): 100k rows in 3–12 ms.
- Removals alone do not re-sort: the order is restored in O(n), about 0.3 ms per 100k rows.
- When only aggregated columns change, a cheaper job recounts the aggregates over the order on screen.
- Expanding or collapsing lays the lines out again in a background job that starts at once.
- When the new order is ready, the [reorder policy](./replaceable-behaviors/) decides when to show it: at once by default, or held while the pointer is over the grid, or until the user asks.

On the web there are no background threads: the same jobs run on the main thread (see [Web](../web/)).

| Operation | Cost |
|---|---|
| `upsert` by key | O(1), about 0.5 µs including the diff, the direction and the highlight |
| `remove` by key | O(1); leaves the screen on the next frame |
| `snapshot().get(&key)`, `scroll_to_key` | about 20 ns, a persistent index in the snapshot |
| Re-sorting 100k rows | 3.2–11.6 ms in the background on several threads, at most once per `min_rebuild_interval` |
| Restoring the order after removals | O(n) without sorting, about 0.3 ms per 100k rows |
| Grouping and trees | The same background sort with the group keys first, or a tree built in O(n) passes; aggregates bottom-up in one pass |
| Repaint | Only on visible changes; highlight fading runs on one timer |

## What it is for, and what it is not

The grid is built for live state: quotes, orders, positions, alerts, vehicles, anything where a key's latest value is what matters, at up to hundreds of thousands of rows and thousands of updates a second.

It does not replace aggregation upstream. A stream with hundreds of thousands of *new* keys a second, or a raw event log, grows the rows, the rebuild time and the memory with every event: count, sample or roll it up before it reaches the grid. In the browser, rebuilds share the main thread with painting and stay within a frame up to about ten to twenty thousand rows that reorder on every update.

## The view's state

`LiveGridState` holds everything about the view: the columns and their layout, the sort, the filter and quick filter, the grouping or tree, the expansion, the selection and focus, open editors, the projection. Read it with `grid.read(cx)` and change it with `grid.update(cx, |grid, cx| …)`; every change a user makes arrives as a `LiveGridEvent` with its origin (`User` or `Programmatic`).

A toolbar that should not know the delegate type drives a grid through `LiveGridHandle` ([Columns](../guides/columns/#livegridhandle)).
