---
title: Editing
description: Built-in cell editors with Excel's keys, validation, and asynchronous commits that stay correct under live updates.
---

A column becomes editable when the grid has an editor for it. Edits leave the grid as events: the application writes the value where the rows come from (a server, a database, the store) and answers, while the grid keeps the draft, the order and the cell's state correct under live updates. The `orders` example edits orders in place while the market streams in.

## Editors

`with_editor(column, editor)` gives a column an editor, `set_editor` changes it later. `orders` uses every built-in one:

```rust src="crates/grid/examples/orders/main.rs#orders-grid"
```

| Editor | Edits |
|---|---|
| `TextEditor` | Text; `with_max_length` |
| `NumberEditor` | An integer, a float or a decimal; minimum, maximum, scale, decimal separator |
| `SelectEditor` | One of a searchable list of `SelectOption`s |
| `DateEditor` | A date from a calendar (`date()`), optionally with a time |
| `CheckboxEditor` | A boolean; toggles at once, with no field |

`CellEditor` is the trait for your own.

## Which cells

- `LiveGridDelegate::is_editable` refuses a cell for now: a filled order, a row the user may not change.
- `validate_edit` refuses a value with a message. The field stays open with a red frame and the message under the cell; `render_edit_message` replaces the message's look.

## Keys

The keys follow Excel:

- Typing, F2, Backspace or Delete (with an empty draft) and a double click on an editable cell open the field.
- In the field, Enter commits and moves down, Shift+Enter up, Tab right, Shift+Tab left; ↑ and ↓ commit and move; Escape cancels.
- Leaving the field commits a valid draft.
- Outside the field, Tab still leaves the grid.

The field's keys are bindings in the `LiveGridEditor` key context. `with_edit_gesture` replaces the rest of the gesture: an AG Grid-like single-click edit, or no double-click edit where a double click must run the primary command. Two defaults change earlier behavior: a double click on an editable cell edits instead of running the primary command, and `+` and `-` on an editable cell type into the draft instead of expanding or collapsing. A custom `EditGesture` restores either.

## Commits

A commit leaves the grid as `LiveGridEvent::EditCommit`. In `orders`, the window sends it to a simulated server thread:

```rust src="crates/grid/examples/orders/main.rs#edit-commit"
```

`EditCommit` carries `base`, the value when the edit started, and `current`, the store's value at the commit: when they differ, the commit goes over someone else's change, and the application decides what that means.

The answer comes back with `resolve_edit`, on the UI thread:

```rust src="crates/grid/examples/orders/main.rs#resolve-edit"
```

- After a commit the cell shows the new value, muted, until the store has it: at most 5 s after `resolve_edit(Ok)`. Sorting, filters, groups and aggregates follow the store, not the optimistic value.
- `resolve_edit(Err)` brings back the store's value with an error mark until the cell is edited again.
- There is no answer timeout (answer with an error on your own timer) and no undo.

`CellStyleContext::edit_state` lets `cell_style` restyle cells that wait or failed.

## Under live updates

- The field is bound to the row key and the column, not to a position. While it is open the built-in reorder policies hold the order, so the row stays under it.
- When the store changes the cell under an open draft, the draft stays and the field shows the conflict ("Changed to … while you edited"). With `with_edit_policy(FollowLiveValue)`, a draft the user has not touched follows the store instead.
- If a filter or a collapsed group hides the row, the edit is cancelled and `EditCancel` returns the draft.

`EditPolicy` holds these rules; `StandardEditPolicy` is the default.

## Controls

A checkbox or a button in a `Control` column commits through the same path as an editor: `CellElementContext::edit()` returns a handle whose `commit` emits `EditCommit` ([Cells](../cells/#elements-in-cells)).

## Events

`EditStart`, `EditCommit` and `EditCancel` report the session with the key and the column; the `orders` status bar shows what every edit came to. `start_edit(&key, column, window, cx)` opens an editor from code: `orders` opens the limit's editor for its Amend command.
