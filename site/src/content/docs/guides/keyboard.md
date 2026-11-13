---
title: Keyboard, menus and accessibility
description: The default keys, binding your own, the context menu and primary command, clicks, selection modes and screen readers.
---

Everything the mouse does in the grid, the keyboard does too: navigation, selection, the context menu, the primary command, sorting through the menu, expanding, editing and copying. The keys are GPUI Actions an application can rebind, and the grid exposes itself to screen readers as a named table.

## The default keys

`gpui_kit_grid::init(cx)`, after `gpui_kit::init(cx)`, binds Excel's keys, with Cmd on macOS where Excel uses Ctrl:

| Keys | Action |
|---|---|
| ↑ ↓ ← → | Move the focus (and the selection, as in Excel) |
| Page Up, Page Down | A page up or down |
| Home, End; Cmd/Ctrl+← and → | The first or last cell of the row |
| Cmd/Ctrl+↑ and ↓ | The first or last row |
| Cmd/Ctrl+Home and End | The first or last cell of the grid |
| Shift with any of the above | Extend the selection |
| Cmd/Ctrl+A | Select all rows on screen |
| Space | Toggle the focused row's selection; on a `Control` cell, press its element |
| Enter | The primary command (`Confirm`); on a link, open it |
| Cmd/Ctrl+Enter | The secondary command (`Confirm { secondary: true }`) |
| Escape | Clear the selection; with nothing selected the key goes on, so a dialog can close; during a row drag, cancel it |
| Cmd/Ctrl+C | Copy the selection as TSV |
| Shift+F10, Menu | Open the context menu |
| `+` and `-` | Expand and collapse a group caption or a tree row |
| F2; Backspace, Delete | Edit the focused cell; with an empty draft |

In an open editor, Enter commits and moves down, Shift+Enter and ↑ up, ↓ down, Tab right, Shift+Tab left, Escape cancels; every other grid key stays out of the way ([Editing](../editing/#keys)). In the group panel, Delete and Backspace remove the focused chip; in the column chooser, Alt+↑ and Alt+↓ move a column.

Tab is not bound: the grid is one Tab stop and Tab leaves it, as in the WAI-ARIA grid pattern. The sort, column and expand-all commands have no default key (Excel binds none, and Ctrl+Plus is usually the application's zoom); bind them yourself.

## Binding your own

The grid's Actions are in `gpui_kit_grid::actions`, handled in the `actions::KEY_CONTEXT` key context on the focused grid, together with gpui-kit's shared ones (`SelectUp`, `Confirm`, `Cancel` and the rest).

- Rebind a key in the same context after `init`: the later binding wins.
- Unbind one with `NoAction`.
- Skip `init` and bind a filtered `actions::key_bindings()`, or your own keys to the actions, instead.
- `actions::ClearAllSorting`, `ExpandAll`, `CollapseAll` and the column commands act on the focused grid; column commands act on `current_column()`, the header pressed last or the focused cell's column.

## The context menu

A right click (on macOS also Ctrl+click) on a cell or a header, Shift+F10 and the Menu key open the context menu:

- on a cell: Copy, Copy all, and the Sort and Column submenus, so the keyboard reaches the column commands;
- on a header: sorting and the column commands, Clear sort and Clear all sorting;
- on a group caption: Expand all, Collapse all and Captions only.

`LiveGridDelegate::context_menu` changes the items, and returning an empty menu means no menu, by mouse or by keyboard. `with_context_menu_gesture` replaces the press that opens it.

## The primary command

`LiveGridDelegate::primary_command` puts a command first in the menu, in bold. The command, Enter and a double click on a row emit the same `LiveGridEvent::Confirm { keys, secondary }`, with the keys the command acts on (`scoped_selection()`, or a group caption's rows). An application opens a ticket, a chart or a dialog there.

## Clicks

`LiveGridEvent::Click { target, button, modifiers, click_count }` reports a press and a release of one button with no drag between them.

- `target` is a `GridTarget`: `Cell { key, column }`, `Pinned { edge, key, column }`, `Header { column }` or `Empty`. It is captured at the press, by key, so a re-sort before the release does not retarget it, and a row removed before the release gives no click.
- It comes once per click, at the release, after the events the click caused: `SelectionChange` and `FocusChange` on a row, `SortChange` on a header.
- A double click emits `Confirm` at its second press, then `Click` with `click_count: 2`.
- A drag (drag-selection, a column move, a resize), a press on a resize handle, and a press that opens or closes the context menu are not clicks.

## Selection modes

`with_selection_mode` (or `set_selection_mode`) picks what is selected: `RowSingle` (the default), `RowSet`, `CellSingle` or `CellSet`. Selection is kept by key: it survives re-sorting, filtering and live updates, and a removed row leaves it. Which presses and keys extend or toggle it is the `SelectionGesture`; which rows a command takes from it, the `SelectionScope` ([Replaceable behaviors](../../concepts/replaceable-behaviors/#selection-menus-and-expansion)).

## Accessibility

Name each grid:

```rust src="crates/grid/examples/quickstart.rs#window"
```

While a screen reader listens, the grid exposes a table named by that label, with its header, the painted rows and cells, and the focused cell, described by its column and its text tooltip. Changes with arrows read as "rising" or "falling", links as "link", group captions with their level and state. Nothing is announced: GPUI has no announcement API yet. Without a screen reader the grid builds none of this, so it costs nothing.

On macOS, run an example as a signed app bundle to try it with VoiceOver: `script/run-example-macos blotter`.
