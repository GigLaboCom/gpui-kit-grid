//! Events a grid emits.

use std::{fmt, sync::Arc};

use gpui_kit::{Modifiers, MouseButton};

use super::{
    drag::{DropSource, DropTarget},
    edit::{EditCancelReason, EditId, EditOrigin},
    menu::GridTarget,
    selection::{FocusedCell, GridSelection},
};
use crate::{
    engine::{GroupPath, SortDirection},
    model::{ColumnSet, OwnedValue},
};

/// Why the selection or the focus changed, so a subscriber that sets the
/// selection itself can avoid a feedback loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectionOrigin {
    /// A click, a key press or a menu command.
    User,
    /// A call of the grid API, such as [`LiveGridState::select_key`].
    ///
    /// [`LiveGridState::select_key`]: super::LiveGridState::select_key
    Programmatic,
    /// The row was removed from the store, or a new data set replaced it.
    DataRemoved,
    /// The row left the projection (a filter, the quick filter); it is
    /// still in the store.
    Filtered,
}

/// One part of the column layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutPart {
    /// Column widths.
    Width,
    /// The display order of columns.
    Order,
    /// Which columns are shown.
    Visibility,
    /// Which zone (frozen left, scrolling, frozen right) columns are in.
    Zone,
}

impl LayoutPart {
    const ALL: [Self; 4] = [Self::Width, Self::Order, Self::Visibility, Self::Zone];

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`LayoutPart`]s.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutParts(u8);

impl LayoutParts {
    /// No parts.
    pub(crate) const EMPTY: Self = Self(0);

    /// This set with `part` added.
    pub(crate) const fn with(self, part: LayoutPart) -> Self {
        Self(self.0 | part.bit())
    }

    /// Adds every part of `other`.
    pub(crate) fn union_with(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Returns true if the set contains `part`.
    pub fn contains(self, part: LayoutPart) -> bool {
        self.0 & part.bit() != 0
    }

    /// Returns true if the set has no parts.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl From<LayoutPart> for LayoutParts {
    fn from(part: LayoutPart) -> Self {
        Self::EMPTY.with(part)
    }
}

impl FromIterator<LayoutPart> for LayoutParts {
    fn from_iter<I: IntoIterator<Item = LayoutPart>>(parts: I) -> Self {
        parts
            .into_iter()
            .fold(Self::EMPTY, |parts, part| parts.with(part))
    }
}

impl fmt::Debug for LayoutParts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(LayoutPart::ALL.iter().filter(|part| self.contains(**part)))
            .finish()
    }
}

/// Who changed the column layout, so a subscriber can avoid feedback loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayoutOrigin {
    /// A pointer gesture or an Action.
    User,
    /// A call of the grid API.
    Programmatic,
}

/// Why the sort changed, so a subscriber that keeps the sort itself and
/// calls `set_sort` can avoid a feedback loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SortOrigin {
    /// A header click or a menu command.
    User,
    /// `set_sort`, `set_view`, or a restored layout.
    Programmatic,
}

/// Who changed the grouping or the expansion, so a subscriber that keeps
/// them itself and calls `set_group_by` or `set_expanded` can avoid a
/// feedback loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GroupOrigin {
    /// The group panel, a chevron, a key, a menu command or an Action.
    User,
    /// A call of the grid API (`set_group_by`, `set_view`,
    /// `set_expand_depth`, `set_expanded`), or a restored layout.
    Programmatic,
}

/// A group or a tree row to expand or collapse, by identity: a group by its
/// path, a tree row by its key.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpandTarget<K> {
    /// A group.
    Group(GroupPath),
    /// A tree row.
    Row(K),
}

/// Events of a [`LiveGridState`]: `K` is the row key, `C` the column.
///
/// [`LiveGridState`]: super::LiveGridState
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum LiveGridEvent<K, C> {
    /// The selection changed: at most once per action, before its
    /// `FocusChange`.
    SelectionChange {
        /// The whole selection, by key; O(1) to clone. Commands use
        /// `scoped_selection()` instead: this set may hold rows a filter
        /// hides.
        selection: GridSelection<K>,
        /// Why it changed.
        origin: SelectionOrigin,
    },
    /// The focused cell (the keyboard target) changed: at most once per
    /// action, after its `SelectionChange`.
    FocusChange {
        /// The new focused cell; `None` when no row is left to focus.
        cell: Option<FocusedCell<K, C>>,
        /// Why it changed.
        origin: SelectionOrigin,
    },
    /// Enter, Cmd/Ctrl+Enter or a double click on a selected row: the
    /// application's primary command.
    Confirm {
        /// The scoped selection in display order; never empty.
        keys: Arc<[K]>,
        /// Cmd/Ctrl+Enter: the secondary command.
        secondary: bool,
    },
    /// A click: a press and a release of one button on a cell, a pinned
    /// cell, a header or the empty area, with no drag between them. Once per
    /// click, when the button goes up, after the events the click caused
    /// (`SelectionChange`, `FocusChange`, `SortChange`; the `Confirm` of a
    /// double click comes at its second press, before the release).
    ///
    /// Not a click: a drag (drag-selection, column move, resize), a press on
    /// a resize handle, a press that only closes the context menu, and a
    /// press the [`ContextMenuGesture`] takes (the menu is its result).
    ///
    /// [`ContextMenuGesture`]: crate::ContextMenuGesture
    Click {
        /// What the press hit, captured when the button went down, by key
        /// and column: a re-sort before the release does not retarget it. A
        /// row removed from the store (or its band) before the release
        /// emits no `Click`.
        target: GridTarget<K, C>,
        /// The button.
        button: MouseButton,
        /// The modifiers held when the button went down.
        modifiers: Modifiers,
        /// The press's place in a series: 2 for the second press of a
        /// double click.
        click_count: usize,
    },
    /// A new display order is shown: a rebuild the reorder policy applied
    /// (a new sort, filter, quick filter, grouping or data set among them),
    /// `apply_held_order`, a compaction after removals, or the lines laid
    /// out again after an `ExpansionChange`. Once per order shown; not while
    /// a newer order is held (`OrderHeldChange` reports that), and not when
    /// only values (aggregates among them), pinned rows or the progress
    /// change. A `flush` that waits for the order resolves after it.
    ProjectionChange {
        /// Data rows in the shown order (`Projection::row_count`): rows of
        /// collapsed groups count, pinned rows do not.
        row_count: usize,
        /// The version of the store snapshot the shown order was built
        /// from.
        store_version: u64,
    },
    /// A newer order is ready but held by the [`ReorderPolicy`] (`held`), or
    /// the held order was applied (`!held`). Drive a "refresh order" cue.
    ///
    /// [`ReorderPolicy`]: crate::policy::ReorderPolicy
    OrderHeldChange {
        /// Whether a newer order is waiting.
        held: bool,
    },
    /// The column layout changed: once per finished gesture, Action or API
    /// call, never while a gesture is in progress. Emitted after the state
    /// changed, so a subscriber that reads the grid sees the new layout.
    LayoutChange {
        /// What changed.
        parts: LayoutParts,
        /// The columns that changed (every moved column for an order change),
        /// by schema index: `C::all()[ix.get()]` is the column.
        columns: ColumnSet,
        /// Who changed it.
        origin: LayoutOrigin,
    },
    /// The sort changed. Emitted at once, before the rebuild (after an
    /// `OrderHeldChange { held: false }` if a held order was dropped); a
    /// `ProjectionChange` follows when the new order is shown.
    SortChange {
        /// The new sort, most significant column first; empty when unsorted.
        sort: Arc<[(C, SortDirection)]>,
        /// Why it changed.
        origin: SortOrigin,
    },
    /// The grouping or the tree changed: `group_by()` and `tree()` read the
    /// new one. Emitted at once, before the rebuild (after an
    /// `OrderHeldChange { held: false }` if a held order was dropped, and
    /// after the `SortChange` of the same `set_view`); a `ProjectionChange`
    /// follows when the new order is shown. A change of the aggregates
    /// alone emits nothing: they are values, not the order.
    GroupChange {
        /// Who changed it.
        origin: GroupOrigin,
    },
    /// Groups or tree rows were expanded or collapsed. Emitted at once,
    /// before the lines are laid out again: `is_expanded` reads the new
    /// state, and a `ProjectionChange` follows when the lines are shown.
    ExpansionChange {
        /// The one group or tree row toggled (with its descendants, for a
        /// deep toggle); `None` for a new default depth, which clears every
        /// toggle (expand all, collapse all, captions only).
        target: Option<ExpandTarget<K>>,
        /// Who changed it.
        origin: GroupOrigin,
    },
    // Editing and dropping rows.
    /// An editor opened on a cell (not for a commit with no field).
    EditStart {
        /// The row.
        key: K,
        /// The column.
        column: C,
        /// Who started the edit.
        origin: EditOrigin,
    },
    /// A value to apply: write it where the rows come from, then answer
    /// with `LiveGridState::resolve_edit(edit, result)`. Until then the
    /// cell shows `value`. `base` is the value the edit started from,
    /// `current` the store's value at the commit: they differ after a
    /// concurrent change (a conflict).
    EditCommit {
        /// The commit, for `resolve_edit`.
        edit: EditId,
        /// The row.
        key: K,
        /// The column.
        column: C,
        /// The value to apply.
        value: OwnedValue,
        /// The cell's value when the edit started.
        base: OwnedValue,
        /// The cell's value in the store at the commit.
        current: OwnedValue,
    },
    /// An editor closed without a commit.
    EditCancel {
        /// The row.
        key: K,
        /// The column.
        column: C,
        /// Why.
        reason: EditCancelReason,
        /// The draft as it parsed, if it did and differs from the value the
        /// edit started from: a dropped edit to show or retry.
        draft: Option<OwnedValue>,
    },
    /// Rows or an application value were dropped on the rows. Emitted by
    /// the grid dropped on, once per drop, after the source grid's own
    /// events of the gesture. The grid changes nothing: the application
    /// moves, copies or links data.
    Drop {
        /// What was dropped.
        source: DropSource<K>,
        /// Where it landed.
        target: DropTarget<K>,
        /// The modifiers held at the drop; copy or move is the
        /// application's choice (Option on macOS, Ctrl elsewhere usually
        /// means copy).
        modifiers: Modifiers,
    },
}
