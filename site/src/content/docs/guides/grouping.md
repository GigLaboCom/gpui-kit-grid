---
title: Grouping, aggregates and totals
description: Group rows by columns or keys, show aggregates on captions and footers, and add a totals row.
---

Rows group by one or more columns, or by a function of the row, and the groups are built in the background together with the sort. Each group starts with a caption that shows its aggregates under their columns; a totals row shows the aggregates of every row the view passes. The `blotter` example has all of it.

## Grouping

`ViewSpec::with_group_by` takes the levels, the outermost first; `set_group_by(levels, cx)` changes them on a live grid. `blotter` builds its view like this:

```rust src="crates/grid/examples/blotter/main.rs#grouping-view"
```

- `GroupBy::column(column)` groups by a column's value; `.with_direction(SortDirection::Descending)` orders that level's groups backwards.
- `GroupBy::key(GroupKey::new(id, depends_on, |row| …))` groups by a function of the row. It borrows its value from the row and must not allocate; `depends_on` names the columns it reads, so a change elsewhere does not regroup.
- `GridColumn::with_groupable(false)` keeps a column out of grouping.

Groups follow their key in the key's own order: a `Custom` value by its `cmp_value` (lines by rank, not by name), whatever its text. A `GridValue` whose `to_owned_value` returns `OwnedValue::custom(self.clone())` stays itself as the group's value, so the caption hook can downcast it. `ViewSpec::with_group_order(AggregateOrder::new(column, direction))` orders groups by an aggregate instead, the busiest first, say.

## Captions

Each group starts with a caption line: a chevron, the caption (`Instrument: AAPL (120)`) and the group's aggregates under their columns. The caption stays put when the columns scroll sideways.

- `LiveGridDelegate::group_caption` writes your own caption text.
- `group_caption_tag` adds a tag before the text, a pill such as a transit line's (`CaptionTag::new(background, foreground)`, with a weight and a least width). The hook writes its text into a reused buffer and the body paints it, so it costs no element and no allocation.
- Captions, footers and totals show text, never elements.

## The group panel

`LiveGrid::new(&grid).group_panel(true)` shows a panel above the header (hidden by default). The user groups by dragging a column header into it:

- a click on a chip reverses that level's order, a drag reorders the levels, and a drop on the grid or the chip's button removes one;
- the header menu and the cell menu's Column submenu have Group by this column, Ungroup and Ungroup all.

## Expanding and collapsing

- The chevron toggles a group; with Alt/Option, every subgroup too. A double click on the caption, `+` and `-`, and ← and → on a caption do the same.
- The caption menu has Expand all, Collapse all and Captions only.
- In code: `with_expand_depth(ExpandDepth::Levels(1))` at start, `set_expand_depth(ExpandDepth::Captions, cx)` and `set_expanded(ExpandTarget::Group(path), false, cx)` later, `is_expanded` to ask.

Expansion is kept by group path (the key values), not by position: a new row never expands a group, and the user's toggles survive live data and a change of grouping.

## Focus, not selection

A caption takes the focus but never the selection. Enter, the primary command and Cmd/Ctrl+C on a caption act on every row of the group, collapsed subgroups included; Cmd/Ctrl+A selects only the data rows on screen. `with_selection_policy` replaces the rule ([Replaceable behaviors](../../concepts/replaceable-behaviors/)).

## Aggregates

`ViewSpec::with_aggregates` gives columns an aggregate, `set_aggregates(aggregates, cx)` on a live grid. `blotter`'s four:

```rust src="crates/grid/examples/blotter/model.rs#aggregates"
```

- `Sum`, `Avg`, `Min`, `Max`, `Count` (non-empty values), or your own `Aggregate::Custom` with an `AggregateFn`.
- Aggregates are recomputed in full in the background, so floating-point sums do not drift. When only aggregated columns change, a cheaper job recounts them without re-sorting.
- With the `decimal` feature, `CellValue::Decimal` columns sum exactly.
- `LiveGridDelegate::aggregate_style` styles an aggregate cell; `AggregateLine` tells a caption, a footer and the totals apart.
- Captions, footers and the totals row flash when their aggregate changes.

## Footers and totals

- `ViewSpec::with_group_footer(EveryGroupFooter)` adds a footer line with the aggregates under every expanded group.
- `LiveGridState::with_totals(PinnedEdge::Bottom)` (or `set_totals(Some(edge), cx)`) shows the totals row: the aggregates of every row the view passes, collapsed groups included, in semibold with a "Total" label, as the outermost row of the top or bottom pinned band. It works without grouping too.

## Copying grouped data

Copy all writes the captions (the key value under the level's column, aggregates under theirs), footers and totals as plain TSV rows. `copy_scope(ExportScope::Expanded, cx)` copies the grid as if every group were expanded; `LineFilter` leaves kinds of lines out of any exporter. [Export](../export/) has the details.

## Events and the layout

`LiveGridEvent::GroupChange` and `ExpansionChange` report every change, with `GroupOrigin::User` for the user's. `layout()` keeps the grouping, its directions, the default depth and the groups the user toggled, so save the layout on these events as on `LayoutChange` and `SortChange` ([Cookbook](../../cookbook/#save-and-restore-the-layout)).
