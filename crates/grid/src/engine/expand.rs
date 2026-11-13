//! Laying the lines of a grouped projection out for its expansion.
//! No GPUI types.
//!
//! A group shows its caption; an expanded one then its subgroups or, at the
//! last level, its data rows, and its footer if it has one. A collapsed
//! group is its caption alone: its footer is not shown, as in DevExpress,
//! and its rows stay in `Projection::rows`.

use std::slice;

use super::{
    Projection, ViewSpec,
    display::{DisplayRow, GroupData, GroupId},
};
use crate::{
    model::GridRow,
    store::{RowId, StoreSnapshot},
};

/// Lays the lines of `groups` out over `rows` by each group's `expanded`
/// and `footer` flags, and sets each group's caption line.
pub(super) fn lay_out(groups: &mut GroupData, rows: &[RowId]) -> Vec<DisplayRow> {
    let mut count = 0;
    walk(groups, rows, |_| count += 1);
    let mut lines = Vec::with_capacity(count);
    walk(groups, rows, |line| lines.push(line));
    for group in &mut groups.groups {
        group.line = u32::MAX;
    }
    for (line_ix, line) in lines.iter().enumerate() {
        if let DisplayRow::GroupCaption { group, .. } = line {
            groups.groups[group.ix()].line = line_ix as u32;
        }
    }
    lines
}

/// Visits the shown lines of `groups` over `rows` in order. O(lines):
/// a collapsed group's subgroups are skipped, not visited.
fn walk(groups: &GroupData, rows: &[RowId], mut visit: impl FnMut(DisplayRow)) {
    let levels = groups.levels;
    let table = &groups.groups;
    // The deepest group whose lines are being shown; its ancestors are
    // shown too, so they close by walking up.
    let mut open: Option<GroupId> = None;
    let mut ix = 0;
    while let Some(group) = table.get(ix) {
        let depth = group.depth();
        while let Some(shown) = open.filter(|shown| table[shown.ix()].depth() >= depth) {
            let shown_group = &table[shown.ix()];
            if shown_group.footer {
                visit(DisplayRow::GroupFooter {
                    group: shown,
                    depth: shown_group.depth(),
                });
            }
            open = shown_group.parent;
        }
        let id = GroupId::new(ix);
        visit(DisplayRow::GroupCaption { group: id, depth });
        if group.expanded {
            if groups.is_leaf(group) {
                for &row in &rows[group.row_range()] {
                    visit(DisplayRow::Data { row, depth: levels });
                }
            }
            open = Some(id);
            ix += 1;
        } else {
            ix = group.end as usize;
        }
    }
    while let Some(shown) = open {
        let shown_group = &table[shown.ix()];
        if shown_group.footer {
            visit(DisplayRow::GroupFooter {
                group: shown,
                depth: shown_group.depth(),
            });
        }
        open = shown_group.parent;
    }
}

/// The order of a flat or grouped projection laid out for
/// `spec.expansion()`: the rows, groups, aggregates and footers stay, as
/// does the store version. A flat projection has nothing to expand: a copy.
pub(super) fn relayout<R: GridRow>(
    previous: &Projection,
    _snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
) -> Projection {
    let Some(data) = previous.group_data() else {
        return previous.duplicate();
    };
    let levels = usize::from(data.levels);
    let expansion = spec.expansion();
    let mut groups = data.clone();
    for group in &mut groups.groups {
        group.expanded = expansion.is_group_expanded(&group.path, levels);
    }
    previous.with_group_layout(groups)
}

/// `Projection::expanded_lines` of a grouped projection: the lines as if
/// every group were expanded, walked without a list. The open groups are
/// the last caption and its ancestors, so the walk keeps no stack either.
pub(super) struct ExpandedGroupLines<'a> {
    groups: &'a GroupData,
    rows: &'a [RowId],
    /// The next group to caption.
    next: usize,
    /// The deepest group whose footer is still to come.
    open: Option<GroupId>,
    /// The data rows of the group last captioned, if of the last level.
    pending: slice::Iter<'a, RowId>,
}

impl<'a> ExpandedGroupLines<'a> {
    pub(super) fn new(groups: &'a GroupData, rows: &'a [RowId]) -> Self {
        Self {
            groups,
            rows,
            next: 0,
            open: None,
            pending: [].iter(),
        }
    }
}

impl Iterator for ExpandedGroupLines<'_> {
    type Item = DisplayRow;

    fn next(&mut self) -> Option<DisplayRow> {
        let table = &self.groups.groups;
        if let Some(&row) = self.pending.next() {
            return Some(DisplayRow::Data {
                row,
                depth: self.groups.levels,
            });
        }
        let next_depth = table.get(self.next).map(|group| group.depth());
        while let Some(open) = self.open {
            let group = &table[open.ix()];
            if next_depth.is_some_and(|depth| depth > group.depth()) {
                break;
            }
            self.open = group.parent;
            if group.footer {
                return Some(DisplayRow::GroupFooter {
                    group: open,
                    depth: group.depth(),
                });
            }
        }
        let group = table.get(self.next)?;
        let id = GroupId::new(self.next);
        self.next += 1;
        self.open = Some(id);
        if self.groups.is_leaf(group) {
            self.pending = self.rows[group.row_range()].iter();
        }
        Some(DisplayRow::GroupCaption {
            group: id,
            depth: group.depth(),
        })
    }
}
