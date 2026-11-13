//! The news monitor: the simulated stories on a grid, newest first, unread
//! ones bold, a new one tinted for a few seconds; category chips over it;
//! and the story the selection opens, which the newsroom then marks read.

use std::{
    sync::{Arc, mpsc::Sender},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription,
    Window, base::h_flex, div, px,
};
use gpui_kit_grid::{
    CellStyle, CellStyleContext, ColumnSet, GridColumn, GridSchema, GridStore, HighlightPolicy,
    LiveGrid, LiveGridDelegate, LiveGridEvent, LiveGridState, RowChange, SelectionMode,
    SortDirection, StoreOptions, ViewSpec,
};

use crate::{
    KEY_CONTEXT, MarkAllRead,
    newsroom::{Category, NewsCommand, NewsCounts, Story, StoryColumn},
    trading::paint::quiet_changes,
    ui::{self, CARD_PADDING, GRID_INSET},
};

/// The news monitor's rows: taller than the quote grids, a list to read.
pub const ROW_HEIGHT: Pixels = px(34.);
/// How long a new story stays tinted.
const NEW_STORY_FLASH: Duration = Duration::from_secs(6);
/// The tint of a new story at full intensity: the accent, faint.
const NEW_STORY_TINT: f32 = 0.24;
/// A read headline's text: the foreground, a little quieter.
const READ_OPACITY: f32 = 0.72;
/// The widths of the columns before the headline.
const UNREAD_WIDTH: Pixels = px(28.);
const TIME_WIDTH: Pixels = px(80.);
const CATEGORY_WIDTH: Pixels = px(100.);
const TICKERS_WIDTH: Pixels = px(132.);
/// The headline's width until the window says how wide the monitor is,
/// and its least.
const HEADLINE_WIDTH: Pixels = px(420.);
const MIN_HEADLINE_WIDTH: Pixels = px(240.);

/// The news store's options: a new story is highlighted for a few seconds.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(NEW_STORY_FLASH, 12))
}

/// The categories the monitor shows: a set over `Category::ALL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Categories(u8);

impl Categories {
    pub const ALL: Self = Self((1 << Category::ALL.len()) - 1);

    pub fn contains(self, category: Category) -> bool {
        self.0 & (1 << category.ix()) != 0
    }

    pub fn is_all(self) -> bool {
        self == Self::ALL
    }

    /// After a press on `category`'s chip: from every category, that one
    /// alone; else that one in or out; with none left, every one again.
    pub fn toggled(self, category: Category) -> Self {
        let bit = 1 << category.ix();
        match if self.is_all() { bit } else { self.0 ^ bit } {
            0 => Self::ALL,
            set => Self(set),
        }
    }
}

/// The news grid's hooks, colours from the theme only: an unread story
/// bold with the accent's dot, a read one's headline quieter; time and
/// category muted, tickers in `base.cyan`; a new story tinted with the
/// accent while its highlight fades. They take `&self`, never read the
/// grid and never allocate.
pub struct NewsDelegate;

impl LiveGridDelegate for NewsDelegate {
    type Row = Story;

    fn cell_style(&self, cell: &CellStyleContext<'_, Story>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let story = cell.row();
        match cell.column() {
            StoryColumn::Unread => style.set_foreground(theme.primary),
            StoryColumn::Time | StoryColumn::Category | StoryColumn::Seq => {
                style.set_foreground(theme.muted_foreground)
            }
            StoryColumn::Tickers => style.set_foreground(theme.cyan),
            StoryColumn::Headline if story.read => {
                style.set_foreground(theme.foreground.opacity(READ_OPACITY))
            }
            StoryColumn::Headline => style.set_foreground(theme.foreground),
        }
        if !story.read {
            style.set_font_weight(FontWeight::BOLD);
        }
        if let Some(change) = cell.change()
            && change.change() == RowChange::Inserted
        {
            style.set_background(theme.primary.opacity(NEW_STORY_TINT * change.intensity()));
        }
        style.set_trend(None);
    }
}

/// The columns: the unread dot, time, category, tickers and a wide
/// headline. The grid has no column that takes the rest of the width, so
/// the monitor sets the headline's width from its own (`fit_headline`).
fn schema() -> GridSchema<StoryColumn> {
    GridSchema::new()
        .column(
            GridColumn::new(StoryColumn::Unread, "")
                .with_width(UNREAD_WIDTH)
                .with_resizable(false)
                .with_sortable(false)
                .with_movable(false),
        )
        .column(GridColumn::new(StoryColumn::Time, "Time").with_width(TIME_WIDTH))
        .column(GridColumn::new(StoryColumn::Category, "Category").with_width(CATEGORY_WIDTH))
        .column(GridColumn::new(StoryColumn::Tickers, "Tickers").with_width(TICKERS_WIDTH))
        .column(GridColumn::new(StoryColumn::Headline, "Headline").with_width(HEADLINE_WIDTH))
        // The order of publication: newest first sorts by it.
        .column(
            GridColumn::new(StoryColumn::Seq, "Seq")
                .with_width(px(64.))
                .with_visible(false),
        )
}

/// The stories of `categories`, newest first. A story's category never
/// changes, so the filter depends on no column.
fn view(categories: Categories) -> ViewSpec<Story> {
    let view = ViewSpec::new().with_sort([(StoryColumn::Seq, SortDirection::Descending)]);
    if categories.is_all() {
        view
    } else {
        view.with_filter(ColumnSet::EMPTY, move |story: &Story| {
            categories.contains(story.category)
        })
    }
}

/// The story open in the monitor changed (`NewsMonitor::story`).
pub struct StoryChange;

/// The news monitor: the stories' grid, the category chips, Mark all read,
/// and the open story.
pub struct NewsMonitor {
    grid: Entity<LiveGridState<NewsDelegate>>,
    categories: Categories,
    commands: Sender<NewsCommand>,
    counts: Arc<NewsCounts>,
    story: Option<Arc<Story>>,
    _opens: Subscription,
}

impl EventEmitter<StoryChange> for NewsMonitor {}

impl NewsMonitor {
    /// A monitor over `store`, which the newsroom writes; `commands` reach
    /// the newsroom, `counts` come from it.
    pub fn new(
        store: GridStore<Story>,
        commands: Sender<NewsCommand>,
        counts: Arc<NewsCounts>,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            LiveGridState::new(schema(), NewsDelegate, store, cx)
                .with_view_id("news")
                .with_selection_mode(SelectionMode::RowSingle)
                .with_change_paint(quiet_changes())
                .with_quick_filter_columns([
                    StoryColumn::Category,
                    StoryColumn::Tickers,
                    StoryColumn::Headline,
                ])
                .with_view(view(Categories::ALL))
        });
        // Selecting a story opens it.
        let opens = cx.subscribe(
            &grid,
            |monitor, grid, event: &LiveGridEvent<u64, StoryColumn>, cx| {
                if let LiveGridEvent::SelectionChange { selection, .. } = event {
                    let story = selection
                        .iter()
                        .next()
                        .and_then(|seq| grid.read(cx).snapshot().get(seq).cloned());
                    monitor.open(story, cx);
                }
            },
        );
        Self {
            grid,
            categories: Categories::ALL,
            commands,
            counts,
            story: None,
            _opens: opens,
        }
    }

    pub fn grid(&self) -> &Entity<LiveGridState<NewsDelegate>> {
        &self.grid
    }

    /// The open story, as it was when opened.
    pub fn story(&self) -> Option<&Arc<Story>> {
        self.story.as_ref()
    }

    /// Opens `story`: the story pane shows it and the newsroom marks it
    /// read.
    fn open(&mut self, story: Option<Arc<Story>>, cx: &mut Context<Self>) {
        let seq = |story: &Option<Arc<Story>>| story.as_ref().map(|story| story.seq);
        if seq(&self.story) == seq(&story) {
            return;
        }
        if let Some(story) = &story
            && !story.read
        {
            // A newsroom that is gone has nothing left to mark.
            let _ = self.commands.send(NewsCommand::Read(story.seq));
        }
        self.story = story;
        cx.emit(StoryChange);
        cx.notify();
    }

    /// Opens the story `step` lines below the open one in the monitor's
    /// order (above for a negative step), or the first one shown if none
    /// is open or the open one is filtered out.
    pub fn open_adjacent(&mut self, step: isize, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| {
            let rows = grid.projection().rows();
            if rows.is_empty() {
                return;
            }
            let snapshot = grid.snapshot().clone();
            let position = grid
                .selection()
                .iter()
                .next()
                .and_then(|seq| snapshot.row_id(seq))
                .and_then(|id| rows.iter().position(|row| *row == id));
            let target = match position {
                Some(position) => position.saturating_add_signed(step).min(rows.len() - 1),
                None => 0,
            };
            let Some(seq) = snapshot.row(rows[target]).map(|story| story.seq) else {
                return;
            };
            grid.select_key(Some(seq), cx);
            grid.scroll_to_key(&seq, cx);
        });
    }

    /// Asks the newsroom to mark every story read.
    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        let _ = self.commands.send(NewsCommand::ReadAll);
        cx.notify();
    }

    /// Shows the stories of `categories`, in the order shown.
    pub fn set_categories(&mut self, categories: Categories, cx: &mut Context<Self>) {
        if self.categories == categories {
            return;
        }
        self.categories = categories;
        self.grid.update(cx, |grid, cx| {
            let view = view(categories).with_sort(grid.sort().to_vec());
            grid.set_view(view, cx);
        });
        cx.notify();
    }

    /// Shows the stories whose category, tickers or headline contain every
    /// word of `text` (the grid's quick filter).
    pub fn set_search(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.set_quick_filter(text, cx));
    }

    /// Gives the headline the width the monitor leaves it when it is
    /// `width` wide.
    pub fn fit_headline(&mut self, width: Pixels, cx: &mut Context<Self>) {
        // Inside the card's border, the grid's inset at each side.
        let frame = width - px(2.) - GRID_INSET * 2.;
        ui::fill_column(
            &self.grid,
            StoryColumn::Headline,
            frame,
            MIN_HEADLINE_WIDTH,
            cx,
        );
    }

    fn render_chips(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let unread = |count: u32, label: &str| -> SharedString {
            if count == 0 {
                SharedString::from(label.to_owned())
            } else {
                format!("{label}  {count}").into()
            }
        };
        h_flex()
            .flex_none()
            .w_full()
            .px(CARD_PADDING)
            .pb(px(12.))
            .gap(px(8.))
            .child(
                ui::chip(
                    "category-all",
                    unread(self.counts.total_unread(), "All"),
                    self.categories.is_all(),
                    cx,
                )
                .on_click(
                    cx.listener(|monitor, _, _, cx| monitor.set_categories(Categories::ALL, cx)),
                ),
            )
            .children(Category::ALL.map(|category| {
                ui::chip(
                    category.label(),
                    unread(self.counts.unread(category), category.label()),
                    !self.categories.is_all() && self.categories.contains(category),
                    cx,
                )
                .on_click(cx.listener(move |monitor, _, _, cx| {
                    let categories = monitor.categories.toggled(category);
                    monitor.set_categories(categories, cx)
                }))
            }))
    }
}

impl Focusable for NewsMonitor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.focus_handle(cx)
    }
}

impl Render for NewsMonitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (stories, unread) = (self.counts.stories(), self.counts.total_unread());
        let detail = match unread {
            0 => format!("{stories} stories, all read"),
            _ => format!("{stories} stories · {unread} unread"),
        };
        ui::card(cx)
            .size_full()
            .child(
                ui::card_header("News monitor", Some(detail.into()), cx)
                    .child(div().flex_1())
                    .child(
                        Button::new("mark-all-read")
                            .label("Mark all read")
                            .small()
                            .ghost()
                            .tooltip_with_action("Mark all read", &MarkAllRead, Some(KEY_CONTEXT))
                            .on_click(cx.listener(|monitor, _, _, cx| monitor.mark_all_read(cx))),
                    ),
            )
            .child(self.render_chips(cx))
            .child(
                ui::grid_frame().child(
                    LiveGrid::new(&self.grid)
                        .with_size(Size::Size(ROW_HEIGHT))
                        .bordered(false)
                        .accessibility_label("News monitor"),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_chips() {
        let all = Categories::ALL;
        assert!(Category::ALL.iter().all(|category| all.contains(*category)));
        // From every category, a chip shows its own alone.
        let equities = all.toggled(Category::Equities);
        assert!(equities.contains(Category::Equities) && !equities.contains(Category::Markets));
        // Then chips add and remove theirs.
        let both = equities.toggled(Category::Volume);
        assert!(both.contains(Category::Volume) && both.contains(Category::Equities));
        assert_eq!(both.toggled(Category::Volume), equities);
        // None left: every one again.
        assert_eq!(equities.toggled(Category::Equities), Categories::ALL);
    }
}
