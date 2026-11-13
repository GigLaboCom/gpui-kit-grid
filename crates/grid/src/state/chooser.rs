//! The column chooser: a searchable list of the grid's columns by zone and
//! band, with a checkbox to show or hide each and a grip to reorder
//! ([`ColumnChooserPanel`]), and the grid's popover layer that shows it for
//! "Choose columns…" ([`ColumnChooserFactory`], [`PopoverColumnChooser`]).
//!
//! The panel works on a [`LiveGridHandle`]: it reads `columns()` and follows
//! `LayoutChange`, and its changes go to the grid with the `User` origin, one
//! `LayoutChange` each, at once. The grid's layer is a sibling of the grid
//! root outside the `LiveGrid` key context, as the menu layer is: a press
//! outside the panel closes it and does nothing else, and Escape closes it,
//! from the search field too.
//!
//! Element ids: the panel is `"live-grid-column-chooser"`; a column's row is
//! `NamedChild("chooser-column", <column id>)`, a band's
//! `NamedChild("chooser-band", <band id>)`, a section header
//! `NamedChild("chooser-zone", <zone>)`; a row's grip is `"chooser-grip"` and
//! its checkbox `"chooser-check"` inside it.

use std::{rc::Rc, sync::Arc};

use gpui_kit::base::actions::Cancel;
use gpui_kit::base::{POPUP_PRIORITY, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Escape, Input, InputEvent, InputState},
    list::{List, ListDelegate, ListItem, ListState},
};
use gpui_kit::{
    AnyView, App, AppContext as _, Context, DismissEvent, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, anchored, deferred, div, point, prelude::FluentBuilder as _, px,
};
use rust_i18n::t;

use super::{
    ColumnInfo, LiveGridDelegate, LiveGridHandle, LiveGridState, WeakLiveGridHandle,
    columns::{ColumnZone, LayoutDelta},
    events::{LayoutOrigin, LayoutPart},
    ghost::GhostTextStyle,
    handle::GridHandleEvent,
};
use crate::{
    actions::{COLUMN_CHOOSER_KEY_CONTEXT, MoveColumnLeft, MoveColumnRight, ResetLayout},
    model::{ColumnId, ColumnIx, ColumnSet, GridColumns as _},
};

/// The width of the column chooser.
pub(crate) const CHOOSER_WIDTH: Pixels = px(280.);

/// The tallest the list grows before it scrolls.
const LIST_MAX_HEIGHT: Pixels = px(360.);

/// The indent of a band's members, per level.
const BAND_INDENT: Pixels = px(16.);

/// The columns from which the panel shows its search field.
const DEFAULT_SEARCH_THRESHOLD: usize = 10;

/// What a row of the chooser stands for, by identity: a column by id, a
/// band by id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChooserItem {
    Column(ColumnId),
    Band(&'static str),
}

/// A change the chooser asks the grid for, with the `User` origin.
pub(crate) enum ChooserCommand<'a> {
    /// Shows a hidden column, hides a shown one; shows every member of a
    /// band not fully shown, else hides those the user may hide.
    Toggle(&'a ChooserItem),
    /// "Show all".
    ShowAll,
    /// "Reset columns", as the menu item.
    Reset,
    /// Puts the item in `zone` before the column `before` (`None`: at the
    /// end of the zone), as the move policy allows; with `dry_run` only
    /// tells whether it would.
    Move {
        item: &'a ChooserItem,
        zone: ColumnZone,
        before: Option<&'a ColumnId>,
        dry_run: bool,
    },
    /// Moves the item one place up (`-1`) or down (`1`) in its zone.
    Step { item: &'a ChooserItem, step: isize },
}

/// One row of the chooser's list.
#[derive(Clone, Debug)]
enum ChooserRow {
    Column {
        info: ColumnInfo,
        /// Its band nesting: the indent.
        depth: usize,
        /// Its checkbox is disabled: not hideable, or the last visible one.
        locked: bool,
        /// Shows a grip: movable, and not while searching.
        grip: bool,
    },
    Band {
        id: &'static str,
        title: SharedString,
        depth: usize,
        shown: usize,
        count: usize,
        grip: bool,
    },
}

impl ChooserRow {
    fn item(&self) -> ChooserItem {
        match self {
            Self::Column { info, .. } => ChooserItem::Column(info.id().clone()),
            Self::Band { id, .. } => ChooserItem::Band(id),
        }
    }

    fn title(&self) -> &SharedString {
        match self {
            Self::Column { info, .. } => info.title(),
            Self::Band { title, .. } => title,
        }
    }
}

/// The rows of one zone; without frozen columns, one section without a
/// header.
#[derive(Clone, Debug)]
struct ChooserSection {
    zone: Option<ColumnZone>,
    rows: Vec<ChooserRow>,
    /// The first column of the zone: where a drop on the header goes.
    first: Option<ColumnId>,
}

/// The chooser's rows from the grid's columns: by zone and band, or a flat
/// list of the columns whose title holds `query` (lowercase).
fn chooser_sections(columns: &[ColumnInfo], query: &str) -> Vec<ChooserSection> {
    // Internal columns are never offered.
    let columns: Vec<ColumnInfo> = columns
        .iter()
        .filter(|column| !column.is_internal())
        .cloned()
        .collect();
    let columns = columns.as_slice();
    let visible = columns.iter().filter(|column| column.is_visible()).count();
    let column_row = |info: &ColumnInfo, depth: usize, grip: bool| ChooserRow::Column {
        locked: !info.is_hideable() || (info.is_visible() && visible <= 1),
        grip: grip && info.is_movable(),
        info: info.clone(),
        depth,
    };
    if !query.is_empty() {
        let rows = columns
            .iter()
            .filter(|column| column.title().to_lowercase().contains(query))
            .map(|column| column_row(column, 0, false))
            .collect();
        return vec![ChooserSection {
            zone: None,
            rows,
            first: None,
        }];
    }
    let sectioned = columns
        .iter()
        .any(|column| column.zone() != ColumnZone::Scrolling);
    let mut sections = Vec::new();
    for zone in ColumnZone::ALL {
        let zone_columns: Vec<&ColumnInfo> = columns
            .iter()
            .filter(|column| column.zone() == zone)
            .collect();
        if zone_columns.is_empty() {
            continue;
        }
        let mut rows = Vec::with_capacity(zone_columns.len());
        let mut open: Vec<&'static str> = Vec::new();
        for (position, column) in zone_columns.iter().enumerate() {
            let chain: Vec<&'static str> = column.bands().iter().map(|band| band.id()).collect();
            let common = open
                .iter()
                .zip(&chain)
                .take_while(|(open, band)| open == band)
                .count();
            open.truncate(common);
            for (level, band) in column.bands().iter().enumerate().skip(common) {
                // The band's members: the columns from here while they are
                // under it at this level.
                let members: Vec<&&ColumnInfo> = zone_columns[position..]
                    .iter()
                    .take_while(|member| {
                        member
                            .bands()
                            .get(level)
                            .is_some_and(|each| each.id() == band.id())
                    })
                    .collect();
                rows.push(ChooserRow::Band {
                    id: band.id(),
                    title: band.title().clone(),
                    depth: level,
                    shown: members.iter().filter(|member| member.is_visible()).count(),
                    count: members.len(),
                    grip: members.iter().all(|member| member.is_movable()),
                });
                open.push(band.id());
            }
            rows.push(column_row(column, chain.len(), true));
        }
        sections.push(ChooserSection {
            zone: Some(zone),
            first: zone_columns.first().map(|column| column.id().clone()),
            rows,
        });
    }
    if !sectioned {
        for section in &mut sections {
            section.zone = None;
        }
    }
    sections
}

/// The label of a zone's section.
fn zone_label(zone: ColumnZone) -> SharedString {
    let key = match zone {
        ColumnZone::FrozenLeft => "live_grid.chooser.frozen_left",
        ColumnZone::Scrolling => "live_grid.chooser.scrolling",
        ColumnZone::FrozenRight => "live_grid.chooser.frozen_right",
    };
    SharedString::from(t!(key).into_owned())
}

/// The id of a zone in element ids.
fn zone_id(zone: ColumnZone) -> &'static str {
    match zone {
        ColumnZone::FrozenLeft => "frozen-left",
        ColumnZone::Scrolling => "scrolling",
        ColumnZone::FrozenRight => "frozen-right",
    }
}

/// The payload of a grip drag: renders the ghost in the row's font.
#[derive(Clone)]
struct ChooserDrag {
    item: ChooserItem,
    title: SharedString,
    text: GhostTextStyle,
}

impl Render for ChooserDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.text.root().child(
            div()
                .px_2()
                .py_1()
                .popover_style(cx)
                .text_color(theme.popover_foreground)
                .opacity(0.9)
                .child(self.title.clone()),
        )
    }
}

/// The list delegate of a [`ColumnChooserPanel`].
pub(crate) struct ChooserDelegate {
    grid: WeakLiveGridHandle,
    sections: Vec<ChooserSection>,
    query: String,
    selected: Option<IndexPath>,
}

impl ChooserDelegate {
    fn row(&self, ix: IndexPath) -> Option<&ChooserRow> {
        self.sections.get(ix.section)?.rows.get(ix.row)
    }

    /// The index of the row of `item`.
    fn position(&self, item: &ChooserItem) -> Option<IndexPath> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section, rows)| {
                rows.rows
                    .iter()
                    .position(|row| row.item() == *item)
                    .map(|row| IndexPath::default().section(section).row(row))
            })
    }

    /// The item of the highlighted row.
    fn selected_item(&self) -> Option<ChooserItem> {
        self.row(self.selected?).map(ChooserRow::item)
    }

    /// Where a drop on the row at `ix` puts the dragged item: the row's zone
    /// and the column it goes before (a band's first member).
    fn drop_place(&self, ix: IndexPath) -> Option<(ColumnZone, Option<ColumnId>)> {
        let section = self.sections.get(ix.section)?;
        let zone = section.zone.unwrap_or(ColumnZone::Scrolling);
        let before = section.rows[ix.row..].iter().find_map(|row| match row {
            ChooserRow::Column { info, .. } => Some(info.id().clone()),
            ChooserRow::Band { .. } => None,
        });
        Some((zone, before))
    }

    /// A row; a grip drag's ghost draws in `ghost_text`.
    fn render_row(
        &self,
        ix: IndexPath,
        row: &ChooserRow,
        ghost_text: GhostTextStyle,
        cx: &mut Context<ListState<Self>>,
    ) -> ListItem {
        let theme = cx.theme();
        let (id, depth, grip, checked, disabled, icon, suffix) = match row {
            ChooserRow::Column {
                info,
                depth,
                locked,
                grip,
            } => (
                ElementId::NamedChild(
                    Arc::new(ElementId::Name("chooser-column".into())),
                    SharedString::from(info.id().as_ref().to_owned()),
                ),
                *depth,
                *grip,
                info.is_visible(),
                *locked,
                info.icon().cloned(),
                None,
            ),
            ChooserRow::Band {
                id,
                depth,
                shown,
                count,
                grip,
                ..
            } => (
                ElementId::NamedChild(
                    Arc::new(ElementId::Name("chooser-band".into())),
                    SharedString::new_static(id),
                ),
                *depth,
                *grip,
                shown == count,
                false,
                None,
                (shown != count).then(|| {
                    SharedString::from(
                        t!("live_grid.chooser.band_shown", shown = shown, count = count)
                            .into_owned(),
                    )
                }),
            ),
        };
        let title = row.title().clone();
        let drag = ChooserDrag {
            item: row.item(),
            title: title.clone(),
            text: ghost_text,
        };
        let place = self.drop_place(ix);
        let grid = self.grid.clone();
        let can_drop_grid = self.grid.clone();
        let can_drop_place = place.clone();
        ListItem::new(id)
            .py_0p5()
            .px_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .pl(BAND_INDENT * depth as f32)
                    .min_w(px(0.))
                    .child(
                        div()
                            .id("chooser-grip")
                            .test_support()
                            .flex_none()
                            .size_4()
                            .when(grip, |this| {
                                this.role(Role::Button)
                                    .aria_label(SharedString::from(
                                        t!("live_grid.chooser.move", title = title.as_ref())
                                            .into_owned(),
                                    ))
                                    .cursor_grab()
                                    .child(
                                        Icon::new(IconName::Menu)
                                            .size_4()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                                    // A press on the grip is no click on the
                                    // row: it does not toggle the column.
                                    .on_click(|_, _, cx| cx.stop_propagation())
                            }),
                    )
                    .child(
                        Checkbox::new("chooser-check")
                            .checked(checked)
                            .disabled(disabled)
                            .tab_stop(false)
                            .accessibility_label(title.clone())
                            .small(),
                    )
                    .children(icon.map(|icon| {
                        Icon::empty()
                            .path(icon)
                            .size_4()
                            .text_color(theme.muted_foreground)
                    }))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(title)),
            )
            .children(suffix.map(|suffix| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(suffix)
            }))
            .when_some(place, |this, (zone, before)| {
                let drag_border = theme.drag_border;
                this.drag_over::<ChooserDrag>(move |style, _, _, _| {
                    style.border_t_2().border_color(drag_border)
                })
                .can_drop(move |drag, _, cx| {
                    let (Some(drag), Some(grid), Some((zone, before))) = (
                        drag.downcast_ref::<ChooserDrag>(),
                        can_drop_grid.upgrade(),
                        can_drop_place.as_ref(),
                    ) else {
                        return false;
                    };
                    grid.chooser_command_in_app(
                        ChooserCommand::Move {
                            item: &drag.item,
                            zone: *zone,
                            before: before.as_ref(),
                            dry_run: true,
                        },
                        cx,
                    )
                })
                .on_drop(move |drag: &ChooserDrag, window, cx| {
                    if let Some(grid) = grid.upgrade() {
                        grid.chooser_command(
                            ChooserCommand::Move {
                                item: &drag.item,
                                zone,
                                before: before.as_ref(),
                                dry_run: false,
                            },
                            window,
                            cx,
                        );
                    }
                })
            })
    }
}

impl ListDelegate for ChooserDelegate {
    type Item = ListItem;

    fn sections_count(&self, _: &App) -> usize {
        self.sections.len().max(1)
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.sections
            .get(section)
            .map_or(0, |section| section.rows.len())
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = self.row(ix)?.clone();
        Some(self.render_row(ix, &row, GhostTextStyle::capture(window), cx))
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let section = self.sections.get(section)?;
        let zone = section.zone?;
        let theme = cx.theme();
        let first = section.first.clone();
        let grid = self.grid.clone();
        let drag_border = theme.drag_border;
        Some(
            div()
                .id(ElementId::NamedChild(
                    Arc::new(ElementId::Name("chooser-zone".into())),
                    SharedString::new_static(zone_id(zone)),
                ))
                .test_support()
                .px_2()
                .pt_2()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(zone_label(zone))
                // A drop on the header puts the item first in the zone.
                .drag_over::<ChooserDrag>(move |style, _, _, _| {
                    style.border_b_2().border_color(drag_border)
                })
                .on_drop(move |drag: &ChooserDrag, window, cx| {
                    if let Some(grid) = grid.upgrade() {
                        grid.chooser_command(
                            ChooserCommand::Move {
                                item: &drag.item,
                                zone,
                                before: first.as_ref(),
                                dry_run: false,
                            },
                            window,
                            cx,
                        );
                    }
                }),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .id("chooser-no-match")
            .test_support()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(
                t!("live_grid.chooser.no_match").into_owned(),
            ))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if let (Some(item), Some(grid)) = (self.selected_item(), self.grid.upgrade()) {
            grid.chooser_command(ChooserCommand::Toggle(&item), window, cx);
        }
    }
}

/// The column chooser: a searchable list of the grid's columns, by zone and
/// band, with a checkbox to show or hide each and a grip to reorder.
/// Changes apply at once, one `LayoutChange { origin: User }` each.
///
/// The grid shows it in its own popover (`LiveGridState::show_column_chooser`);
/// an application may host it in its own `Popover`, dialog or sheet: create
/// it once over a [`LiveGridHandle`] and render the entity. It emits
/// `DismissEvent` on Escape, and when the grid went away.
///
/// ```ignore
/// // Once, when the toolbar is built:
/// let chooser = cx.new(|cx| ColumnChooserPanel::new(&LiveGridHandle::new(&grid), window, cx));
/// // In the toolbar's render:
/// Popover::new("columns")
///     .trigger(Button::new("columns").label("Columns…"))
///     .content(move |_, _| chooser.clone())
/// ```
pub struct ColumnChooserPanel {
    grid: WeakLiveGridHandle,
    list: Entity<ListState<ChooserDelegate>>,
    search: Entity<InputState>,
    search_threshold: usize,
    /// The columns the grid has.
    column_count: usize,
    /// Every column is shown: "Show all" is disabled.
    all_shown: bool,
    /// A layout change arrived: the rows are rebuilt in the next render.
    stale: bool,
    _subscriptions: Vec<Subscription>,
}

impl ColumnChooserPanel {
    /// A chooser of the columns of `grid`.
    pub fn new(grid: &LiveGridHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let columns = grid.columns(cx);
        let delegate = ChooserDelegate {
            grid: grid.downgrade(),
            sections: chooser_sections(&columns, ""),
            query: String::new(),
            selected: None,
        };
        let list = cx.new(|cx| ListState::new(delegate, window, cx));
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(SharedString::from(
                t!("live_grid.chooser.search").into_owned(),
            ))
        });
        let this = cx.weak_entity();
        let layout = grid.subscribe(cx, move |event, cx| {
            if matches!(event, GridHandleEvent::LayoutChange { .. }) {
                let _ = this.update(cx, |panel: &mut Self, cx| {
                    panel.stale = true;
                    cx.notify();
                });
            }
        });
        let search_changes = cx.subscribe_in(&search, window, Self::on_search_event);
        Self {
            grid: grid.downgrade(),
            list,
            search,
            search_threshold: DEFAULT_SEARCH_THRESHOLD,
            all_shown: columns.iter().all(ColumnInfo::is_visible),
            column_count: columns.len(),
            stale: false,
            _subscriptions: vec![layout, search_changes],
        }
    }

    /// Shows the search field from this many columns (default 10): 0
    /// always, `usize::MAX` never.
    pub fn with_search_threshold(mut self, columns: usize) -> Self {
        self.search_threshold = columns;
        self
    }

    /// Returns true if the search field shows.
    fn is_searchable(&self) -> bool {
        self.column_count >= self.search_threshold
    }

    fn on_search_event(
        &mut self,
        search: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            let query = search.read(cx).value().trim().to_lowercase();
            let changed = self.list.read(cx).delegate().query != query;
            if changed {
                self.list
                    .update(cx, |list, _| list.delegate_mut().query = query);
                self.refresh(window, cx);
            }
        }
    }

    /// Rebuilds the rows from the grid's columns; the highlight stays on the
    /// same column or band.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stale = false;
        let Some(grid) = self.grid.upgrade() else {
            return;
        };
        let columns = grid.columns(cx);
        self.column_count = columns.len();
        self.all_shown = columns.iter().all(ColumnInfo::is_visible);
        self.list.update(cx, |list, cx| {
            let selected = list.delegate().selected_item();
            let query = list.delegate().query.clone();
            list.delegate_mut().sections = chooser_sections(&columns, &query);
            let ix = selected.and_then(|item| list.delegate().position(&item));
            list.set_selected_index(ix, window, cx);
            cx.notify();
        });
        cx.notify();
    }

    /// Alt+↑ / Alt+↓: the highlighted column or band one place up or down
    /// in its zone.
    fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(item), Some(grid)) = (
            self.list.read(cx).delegate().selected_item(),
            self.grid.upgrade(),
        ) else {
            return;
        };
        grid.chooser_command(ChooserCommand::Step { item: &item, step }, window, cx);
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for ColumnChooserPanel {}

impl Focusable for ColumnChooserPanel {
    /// The search field if it shows, else the list.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.is_searchable() {
            self.search.focus_handle(cx)
        } else {
            self.list.focus_handle(cx)
        }
    }
}

impl Render for ColumnChooserPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.grid.upgrade().is_none() {
            // The grid went away: nothing left to choose.
            cx.defer_in(window, |panel, _, cx| panel.dismiss(cx));
        } else if self.stale {
            self.refresh(window, cx);
        }
        let theme = cx.theme();
        let searchable = self.is_searchable();
        let grid = self.grid.clone();
        let reset_grid = self.grid.clone();
        div()
            .id("live-grid-column-chooser")
            .test_support()
            .role(Role::Dialog)
            .aria_label(SharedString::from(
                t!("live_grid.chooser.label").into_owned(),
            ))
            .flex()
            .flex_col()
            .w(CHOOSER_WIDTH)
            .popover_style(cx)
            .text_sm()
            .overflow_hidden()
            .on_action(cx.listener(|panel, _: &Cancel, _, cx| panel.dismiss(cx)))
            .on_action(cx.listener(|panel, _: &Escape, _, cx| panel.dismiss(cx)))
            .when(searchable, |this| {
                this.child(
                    div().p_2().border_b_1().border_color(theme.border).child(
                        Input::new(&self.search)
                            .small()
                            .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                            .cleanable(true),
                    ),
                )
            })
            .child(
                div()
                    .key_context(COLUMN_CHOOSER_KEY_CONTEXT)
                    .on_action(cx.listener(|panel, _: &MoveColumnLeft, window, cx| {
                        panel.step(-1, window, cx)
                    }))
                    .on_action(cx.listener(|panel, _: &MoveColumnRight, window, cx| {
                        panel.step(1, window, cx)
                    }))
                    .py_1()
                    .child(List::new(&self.list).small().max_h(LIST_MAX_HEIGHT)),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .p_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("chooser-show-all")
                            .ghost()
                            .small()
                            .label(SharedString::from(
                                t!("live_grid.chooser.show_all").into_owned(),
                            ))
                            .disabled(self.all_shown)
                            .on_click(move |_, window, cx| {
                                if let Some(grid) = grid.upgrade() {
                                    grid.chooser_command(ChooserCommand::ShowAll, window, cx);
                                }
                            }),
                    )
                    .child(
                        Button::new("chooser-reset")
                            .ghost()
                            .small()
                            .label(SharedString::from(
                                t!("live_grid.menu.reset_columns").into_owned(),
                            ))
                            .on_click(move |_, window, cx| {
                                if let Some(grid) = reset_grid.upgrade() {
                                    grid.chooser_command(ChooserCommand::Reset, window, cx);
                                }
                            }),
                    ),
            )
    }
}

/// Makes what the grid shows for "Choose columns…".
pub trait ColumnChooserFactory: 'static {
    /// Returns the view the grid shows in its popover at
    /// `context.position()`, focused by the factory, or `None` after
    /// showing a chooser elsewhere (a dialog with `window.open_dialog`).
    /// Called once per opening, after the event that opened it, outside the
    /// grid's update: the factory may read the grid through the handle.
    fn open(
        &self,
        context: &ColumnChooserContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyView>;
}

/// What a [`ColumnChooserFactory`] knows.
pub struct ColumnChooserContext {
    grid: LiveGridHandle,
    position: Point<Pixels>,
}

impl ColumnChooserContext {
    /// The grid.
    pub fn grid(&self) -> &LiveGridHandle {
        &self.grid
    }

    /// Where the popover's top left goes, in window coordinates.
    pub fn position(&self) -> Point<Pixels> {
        self.position
    }
}

/// The default: [`ColumnChooserPanel`] in the grid's popover, focused on its
/// search field or its list.
#[derive(Clone, Copy, Debug)]
pub struct PopoverColumnChooser {
    search_threshold: usize,
}

impl PopoverColumnChooser {
    /// The panel with its search field from 10 columns.
    pub const fn new() -> Self {
        Self {
            search_threshold: DEFAULT_SEARCH_THRESHOLD,
        }
    }

    /// Shows the panel's search field from this many columns (default 10).
    pub const fn with_search_threshold(mut self, columns: usize) -> Self {
        self.search_threshold = columns;
        self
    }

    /// The columns from which the search field shows.
    pub const fn search_threshold(&self) -> usize {
        self.search_threshold
    }
}

impl Default for PopoverColumnChooser {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnChooserFactory for PopoverColumnChooser {
    fn open(
        &self,
        context: &ColumnChooserContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyView> {
        let threshold = self.search_threshold;
        let panel = cx.new(|cx| {
            ColumnChooserPanel::new(context.grid(), window, cx).with_search_threshold(threshold)
        });
        let focus = panel.focus_handle(cx);
        window.focus(&focus, cx);
        Some(panel.into())
    }
}

/// How a grid shows its column chooser, and the one open.
pub(super) struct ChooserSlot {
    factory: Rc<dyn ColumnChooserFactory>,
    open: Option<OpenChooser>,
}

impl Default for ChooserSlot {
    fn default() -> Self {
        Self {
            factory: Rc::new(PopoverColumnChooser::new()),
            open: None,
        }
    }
}

/// The chooser in the grid's layer.
struct OpenChooser {
    view: AnyView,
    position: Point<Pixels>,
}

impl<D: LiveGridDelegate> LiveGridState<D> {
    /// Sets what "Choose columns…" shows (default [`PopoverColumnChooser`]).
    pub fn with_column_chooser(mut self, factory: impl ColumnChooserFactory) -> Self {
        self.column_chooser.factory = Rc::new(factory);
        self
    }

    /// Opens the column chooser below the right end of the header, its
    /// right edge at the header's (`ShowColumnChooser`). The view is built
    /// after the current event. An open context menu closes.
    pub fn show_column_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.body_bounds;
        let position = point(
            (body.origin.x + body.size.width - CHOOSER_WIDTH).max(body.origin.x),
            body.origin.y,
        );
        self.show_column_chooser_at(position, window, cx);
    }

    /// Opens the column chooser with its top left at `position` (window
    /// coordinates), after the current event.
    pub fn show_column_chooser_at(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One overlay at a time (Design Guides, "Avoid stacking overlays").
        if self.menu.take().is_some() {
            self.reconsider_held(cx);
        }
        let factory = self.column_chooser.factory.clone();
        let grid = self.handle(cx);
        let this = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let context = ColumnChooserContext { grid, position };
            let view = factory.open(&context, window, cx);
            let _ = this.update(cx, |state, cx| {
                state.column_chooser.open = view.map(|view| OpenChooser { view, position });
                cx.notify();
            });
        });
    }

    /// Closes the column chooser the grid shows, and gives the grid the
    /// focus. Does nothing if none is open.
    pub fn close_column_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.column_chooser.open.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Returns true while the grid shows its column chooser.
    pub fn is_column_chooser_open(&self) -> bool {
        self.column_chooser.open.is_some()
    }

    /// Closes the chooser without moving the focus: a menu opens over it.
    pub(super) fn drop_column_chooser(&mut self) {
        self.column_chooser.open = None;
    }

    /// `ShowColumnChooser`: where the context menu was, if one is open (its
    /// "Choose columns…" item), else below the right end of the header.
    pub(super) fn on_show_column_chooser(
        &mut self,
        _: &crate::actions::ShowColumnChooser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.menu_position() {
            Some(position) => self.show_column_chooser_at(position, window, cx),
            None => self.show_column_chooser(window, cx),
        }
    }

    /// The chooser's layer: over the whole window, a press outside the
    /// panel closes it and goes no further, the wheel stops there, and
    /// Escape closes it from anywhere inside.
    pub(super) fn render_column_chooser(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<D>> {
        let open = self.column_chooser.open.as_ref()?;
        let viewport = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("live-grid-chooser-layer")
                        .w(viewport.width)
                        .h(viewport.height)
                        .occlude()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_any_mouse_down(cx.listener(|state, _, window, cx| {
                            cx.stop_propagation();
                            state.close_column_chooser(window, cx);
                        }))
                        .capture_action(cx.listener(|state, _: &Cancel, window, cx| {
                            state.close_column_chooser(window, cx);
                        }))
                        .capture_action(cx.listener(|state, _: &Escape, window, cx| {
                            state.close_column_chooser(window, cx);
                        }))
                        .child(
                            anchored()
                                .position(open.position)
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        // A press in the panel stays there.
                                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                        .child(open.view.clone()),
                                ),
                        ),
                ),
            )
            .with_priority(POPUP_PRIORITY),
        )
    }

    /// Runs a command of the column chooser with the `User` origin. Returns
    /// whether it changed (or, for a dry run, would change) the layout.
    pub(super) fn run_chooser_command(
        &mut self,
        command: ChooserCommand<'_>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> bool {
        match command {
            ChooserCommand::Toggle(item) => self.chooser_toggle(item, cx),
            ChooserCommand::ShowAll => {
                let all_shown = self.columns.visible_set().len() == self.columns.order().len();
                self.show_all_columns_from(LayoutOrigin::User, cx);
                !all_shown
            }
            ChooserCommand::Reset => match window {
                Some(window) => {
                    self.on_reset_layout(&ResetLayout, window, cx);
                    true
                }
                None => false,
            },
            ChooserCommand::Move {
                item,
                zone,
                before,
                dry_run,
            } => self.chooser_move(item, zone, before, dry_run, cx),
            ChooserCommand::Step { item, step } => self.chooser_step(item, step, cx),
        }
    }

    /// The columns of `item` in its zone, in zone order.
    fn chooser_members(&self, item: &ChooserItem) -> Option<(ColumnZone, Vec<ColumnIx>)> {
        match item {
            ChooserItem::Column(id) => {
                let ix = self.column_by_id(id.as_ref())?.ix();
                Some((self.columns.zone_of(ix), vec![ix]))
            }
            ChooserItem::Band(band) => {
                let first = self
                    .columns
                    .order()
                    .iter()
                    .copied()
                    .find(|ix| self.columns.bands_of(*ix).contains(band))?;
                let zone = self.columns.zone_of(first);
                Some((zone, self.columns.band_members(zone, band)))
            }
        }
    }

    fn chooser_toggle(&mut self, item: &ChooserItem, cx: &mut Context<Self>) -> bool {
        let Some((_, members)) = self.chooser_members(item) else {
            return false;
        };
        // A band not fully shown is shown; a shown one hides the members
        // the user may hide, the last visible column of the grid excepted.
        let show = !members.iter().all(|ix| self.columns.is_visible(*ix));
        let hideable: Vec<bool> = members
            .iter()
            .map(|ix| self.columns.column(*ix).hideable)
            .collect();
        self.change_layout(LayoutOrigin::User, cx, |layout, _| {
            let mut changed = ColumnSet::EMPTY;
            for (ix, hideable) in members.iter().zip(&hideable) {
                if !show && !hideable {
                    continue;
                }
                if layout.set_visible(*ix, show) == Some(true) {
                    changed.insert(*ix);
                }
            }
            if changed.is_empty() {
                LayoutDelta::default()
            } else {
                LayoutDelta::new(LayoutPart::Visibility, changed.iter())
            }
        })
    }

    fn chooser_move(
        &mut self,
        item: &ChooserItem,
        zone: ColumnZone,
        before: Option<&ColumnId>,
        dry_run: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((from, members)) = self.chooser_members(item) else {
            return false;
        };
        let before = match before {
            Some(id) => match self.column_by_id(id.as_ref()) {
                Some(column) => Some(column.ix()),
                None => return false,
            },
            None => None,
        };
        if members.iter().any(|ix| !self.columns.column(*ix).movable)
            || before.is_some_and(|ix| members.contains(&ix))
        {
            return false;
        }
        if dry_run {
            let mut layout = self.columns.clone();
            return chooser_move_in(&mut layout, &*self.column_move, from, members, zone, before)
                .is_some();
        }
        self.change_layout(LayoutOrigin::User, cx, |layout, policy| {
            chooser_move_in(layout, policy, from, members, zone, before).unwrap_or_default()
        })
    }

    fn chooser_step(&mut self, item: &ChooserItem, step: isize, cx: &mut Context<Self>) -> bool {
        let Some((zone, members)) = self.chooser_members(item) else {
            return false;
        };
        if members.iter().any(|ix| !self.columns.column(*ix).movable) {
            return false;
        }
        let positions: Vec<usize> = members
            .iter()
            .filter_map(|ix| self.columns.zone_position(*ix).map(|(_, i)| i))
            .collect();
        let (Some(&first), Some(&last)) = (positions.iter().min(), positions.iter().max()) else {
            return false;
        };
        let boundary = if step < 0 {
            match first.checked_sub(1) {
                Some(k) => k,
                None => return false,
            }
        } else {
            last + 2
        };
        let super::columns::Boundary::Gap(gap) = self
            .columns
            .zone_move(zone, members.clone())
            .resolve(boundary, &*self.column_move)
        else {
            return false;
        };
        self.change_layout(LayoutOrigin::User, cx, |layout, _| {
            if layout.move_members(zone, &members, gap) {
                LayoutDelta::new(LayoutPart::Order, members.iter().copied()).revealing()
            } else {
                LayoutDelta::default()
            }
        })
    }
}

/// Puts `members` (of zone `from`) in `zone` before `before`, in one change:
/// to the zone first if it differs (the policy may take the rest of a band
/// along), then to its place. `None` if the policy refuses or nothing
/// changes.
fn chooser_move_in<C: crate::model::GridColumns>(
    layout: &mut super::columns::ColumnLayout<C>,
    policy: &dyn crate::policy::ColumnMovePolicy,
    from: ColumnZone,
    members: Vec<ColumnIx>,
    zone: ColumnZone,
    before: Option<ColumnIx>,
) -> Option<LayoutDelta> {
    if before.is_some_and(|ix| layout.zone_of(ix) != zone) {
        return None;
    }
    let mut delta = LayoutDelta::default();
    let mut members = members;
    if from != zone {
        let mut moved = Vec::new();
        for ix in &members {
            if layout.zone_of(*ix) == from {
                moved.extend(layout.set_zone(*ix, zone, policy));
            }
        }
        if moved.is_empty() {
            return None;
        }
        delta = LayoutDelta::new(LayoutPart::Zone, moved.iter().copied());
        delta.parts = delta.parts.with(LayoutPart::Order);
        members = layout
            .zone_order(zone)
            .iter()
            .copied()
            .filter(|ix| moved.contains(ix))
            .collect();
    }
    match layout.move_before(zone, members.clone(), before, policy) {
        Some(true) => {
            delta.union_with(LayoutDelta::new(LayoutPart::Order, members.iter().copied()));
        }
        Some(false) => {}
        None if from == zone => return None,
        // In the new zone at its edge: the place is refused, the zone not.
        None => {}
    }
    (!delta.is_empty()).then(|| delta.revealing())
}

impl LiveGridHandle {
    /// Runs a command of the column chooser on the grid (`User` origin).
    pub(crate) fn chooser_command(
        &self,
        command: ChooserCommand<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.ops().chooser(command, Some(window), cx)
    }

    /// [`Self::chooser_command`] without a window: a dry run, which needs
    /// none.
    pub(crate) fn chooser_command_in_app(&self, command: ChooserCommand<'_>, cx: &mut App) -> bool {
        self.ops().chooser(command, None, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_factory() {
        let factory = PopoverColumnChooser::default();
        assert_eq!(factory.search_threshold(), 10);
        assert_eq!(
            PopoverColumnChooser::new()
                .with_search_threshold(3)
                .search_threshold(),
            3
        );
        assert_eq!(ChooserItem::Band("prices"), ChooserItem::Band("prices"));
    }
}
