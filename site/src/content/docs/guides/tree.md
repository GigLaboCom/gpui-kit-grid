---
title: Tree
description: Show rows under their parents, with a tree column, filters that keep ancestors, and totals over the leaves.
---

A row with a parent key sits under its parent. The tree is built from the live rows in the background, in a few O(n) passes, so rows can arrive, leave and change parents while the grid is on screen. The `portfolio` example is a live tree of funds, desks, books and positions.

## Parents

`GridRow::parent_key` returns the parent's key, or `None` for a root:

```rust src="crates/grid/examples/portfolio/model.rs#parent-key"
```

- A row whose parent is missing (an orphan), and rows in a parent cycle, become roots; `GridStats::tree_cycles` counts the latter.
- A row that gets another parent moves with its subtree.

## The tree view

`ViewSpec::with_tree(TreeSpec)` builds the view as a tree; `set_tree(Some(spec), cx)` switches a live grid to one and `set_tree(None, cx)` back. A tree and grouping replace each other. `portfolio`'s grid:

```rust src="crates/grid/examples/portfolio/main.rs#tree-grid"
```

- `TreeSpec::with_column` names the tree column, which shows the indent and the chevron.
- Siblings sort by the view's sort.

## Filters

A filter or the quick filter keeps the ancestors of matching rows, so a match is always shown where it lives (`TreeFilter::KeepAncestors`, the default). `TreeSpec::with_filter(TreeFilter::HideSubtree)` hides a non-matching row with everything under it, and `TreeFilter::Custom` takes your own `TreeFilterRule`.

## Totals

The totals add up the leaves (`TreeTotals::Leaves`): a parent that already holds its children's sum does not count twice. `TreeSpec::with_totals(TreeTotals::Custom(…))` takes your own rule. The grid does not roll children up into their parents' cells; `portfolio`'s producer writes the sums into the book and desk rows itself.

## Selection

Tree rows are data rows and are selected like any row, as in AG Grid's tree data. With `with_selection_policy(FocusBranches)`, rows with children take the focus but not the selection, and a command on one acts on its leaves (`PORTFOLIO_SELECT=branches` in the example).

## Keys

- The chevron toggles a row; with Alt/Option, its whole subtree.
- `+` expands and `-` collapses the focused row; `-` on a leaf goes to its parent.
- ← and → move between cells, as everywhere else in the grid.
- `ExpandDepth::Levels(n)` sets the default depth, and the user's toggles are kept by key across live updates.
