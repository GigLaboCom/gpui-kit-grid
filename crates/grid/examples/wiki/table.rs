//! The wikis table: one grid over the store of wikis, flat or grouped by
//! project family, sorted by edits a minute, busiest first, so the order
//! moves with the world's editing. A new edit flashes its wiki's time, a
//! rising rate its edits a minute; a quiet wiki fades.

use std::{fmt::Write as _, sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, FontWeight, SharedString, px};
use gpui_kit_grid::{
    Aggregate, AggregateOrder, CellChange, CellStyle, CellStyleContext, CellTooltip,
    CellTooltipContext, ChangeDirection, ChangePaint, ChangePainter, ChangePalette, ColumnZone,
    GridColumn, GridColumns as _, GridMenuContext, GridSchema, GroupBy, GroupCaptionContext,
    HighlightPolicy, LiveGridDelegate, OwnedValue, RowChange, SortDirection, StoreOptions,
    ViewSpec,
};

use super::{
    rows::{QUIET_STEPS, WikiColumn, WikiRow},
    ui::thousands,
};

/// How long a change shows.
const FLASH: Duration = Duration::from_millis(2_000);
/// The opacity of a flash at full intensity.
const FLASH_TINT: f32 = 0.28;
/// The text's opacity at each quiet step.
const QUIET_OPACITY: [f32; QUIET_STEPS as usize + 1] = [1., 0.78, 0.6, 0.46, 0.36];
/// A bot share at or above this shows in the bots' colour.
const MOSTLY_BOTS: u8 = 50;

/// The store's options: a short flash for each change.
pub fn store_options() -> StoreOptions {
    StoreOptions::default().with_highlight(HighlightPolicy::new(FLASH, 6))
}

/// How the table arranges the wikis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrangement {
    Flat,
    #[default]
    ByFamily,
}

impl Arrangement {
    pub const ALL: [Self; 2] = [Self::Flat, Self::ByFamily];

    pub fn label(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::ByFamily => "By family",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Flat => Self::ByFamily,
            Self::ByFamily => Self::Flat,
        }
    }

    /// The family column is the groups' caption when grouped.
    pub fn shows_family(self) -> bool {
        self == Self::Flat
    }
}

/// The columns a phone's width leaves out, so that edits a minute shows
/// next to the wiki.
pub const WIDE_ONLY: [WikiColumn; 2] = [WikiColumn::Lang, WikiColumn::Language];

/// The sort at start: edits a minute, then edits, both busiest first.
pub fn default_sort() -> Vec<(WikiColumn, SortDirection)> {
    vec![
        (WikiColumn::PerMinute, SortDirection::Descending),
        (WikiColumn::Edits, SortDirection::Descending),
    ]
}

/// The view of `arrangement` with `sort`: sums under the families'
/// captions, the busiest family first, and in the totals.
pub fn view(arrangement: Arrangement, sort: Vec<(WikiColumn, SortDirection)>) -> ViewSpec<WikiRow> {
    let view = ViewSpec::new().with_sort(sort).with_aggregates([
        (WikiColumn::PerMinute, Aggregate::Sum),
        (WikiColumn::Edits, Aggregate::Sum),
        (WikiColumn::NewPages, Aggregate::Sum),
        (WikiColumn::Added, Aggregate::Sum),
        (WikiColumn::Removed, Aggregate::Sum),
    ]);
    match arrangement {
        Arrangement::Flat => view,
        Arrangement::ByFamily => view
            .with_group_by([GroupBy::column(WikiColumn::Family)])
            .with_group_order(AggregateOrder::new(
                WikiColumn::PerMinute,
                SortDirection::Descending,
            )),
    }
}

fn column(column: WikiColumn, title: &'static str, width: f32) -> GridColumn<WikiColumn> {
    GridColumn::new(column, title).with_width(px(width))
}

/// The wiki frozen on the left; the page is a link: a click on its title
/// or Enter opens it.
pub fn schema() -> GridSchema<WikiColumn> {
    GridSchema::new()
        .column(column(WikiColumn::Wiki, "Wiki", 176.).with_zone(ColumnZone::FrozenLeft))
        .column(column(WikiColumn::Family, "Family", 96.))
        .column(column(WikiColumn::Lang, "Lang", 56.))
        .column(column(WikiColumn::Language, "Language", 116.))
        .column(column(WikiColumn::PerMinute, "Edits/min", 102.))
        .column(column(WikiColumn::Edits, "Edits", 70.))
        .column(column(WikiColumn::Bots, "Bots %", 66.))
        .column(column(WikiColumn::NewPages, "New", 58.))
        .column(column(WikiColumn::Added, "+Bytes", 86.))
        .column(column(WikiColumn::Removed, "−Bytes", 86.))
        .column(column(WikiColumn::Last, "Last UTC", 80.))
        .column(column(WikiColumn::Page, "Last page", 300.).with_min_width(px(120.)))
}

/// The table's hooks, colours from the theme only. They take `&self`,
/// never read the grid, and never allocate.
pub struct WikiTable;

impl WikiTable {
    fn faded(color: gpui_kit::Hsla, row: &WikiRow) -> gpui_kit::Hsla {
        color.opacity(color.a * QUIET_OPACITY[usize::from(row.quiet.min(QUIET_STEPS))])
    }
}

impl LiveGridDelegate for WikiTable {
    type Row = WikiRow;

    fn cell_style(&self, cell: &CellStyleContext<'_, WikiRow>, style: &mut CellStyle, cx: &App) {
        let theme = cx.theme();
        let row = cell.row();
        let column = cell.column();
        let foreground = match column {
            WikiColumn::Added if row.added > 0 => theme.success,
            WikiColumn::Removed if row.removed > 0 => theme.danger,
            WikiColumn::Bots if row.bots_percent.is_some_and(|share| share >= MOSTLY_BOTS) => {
                theme.warning
            }
            WikiColumn::Family | WikiColumn::Lang | WikiColumn::Last => theme.muted_foreground,
            WikiColumn::Page => theme.link,
            _ => theme.foreground,
        };
        style.set_foreground(Self::faded(foreground, row));
        if column == WikiColumn::PerMinute {
            style.set_trend(row.trend);
            if row.per_minute > 0 && row.quiet == 0 {
                style.set_font_weight(FontWeight::BOLD);
            }
        }
    }

    fn group_caption(&self, caption: &GroupCaptionContext<'_, WikiRow>, out: &mut String, _: &App) {
        let int = |column: WikiColumn| match caption.aggregate(column) {
            Some(OwnedValue::Int(value)) => (*value).max(0) as u64,
            _ => 0,
        };
        let wikis = caption.row_count();
        let _ = write!(
            out,
            "{}  ·  {} wiki{}  ·  {} edits/min",
            caption.value_text(),
            thousands(wikis as u64),
            if wikis == 1 { "" } else { "s" },
            thousands(int(WikiColumn::PerMinute)),
        );
    }

    fn primary_command(&self, _: &GridMenuContext<'_, WikiRow>, _: &App) -> Option<SharedString> {
        Some("Open the last page".into())
    }

    fn tooltip(&self, cell: &CellTooltipContext<'_, WikiRow>, _: &App) -> Option<CellTooltip> {
        match cell.column() {
            WikiColumn::Page => cell
                .row()
                .url()
                .map(|url| CellTooltip::text(format!("Open {url}"))),
            WikiColumn::PerMinute => Some(CellTooltip::text(match cell.row().trend {
                Some(ChangeDirection::Up) => "Edits in the last minute; rising",
                Some(ChangeDirection::Down) => "Edits in the last minute; falling",
                None => "Edits in the last minute",
            })),
            _ => cell.default_tooltip(),
        }
    }
}

/// The flash, a highlighter's, apart from the selection's colour: a new
/// wiki whole in `warning`, a new edit on its time in `warning`, a rising
/// rate on its edits a minute in `success`. A falling rate is no news, and
/// nothing else flashes.
#[derive(Clone, Copy, Debug)]
pub struct WikiChanges;

impl ChangePainter for WikiChanges {
    fn paint(&self, change: &CellChange, palette: &ChangePalette, style: &mut CellStyle) {
        let column = change.column_ix();
        let is = |wiki: WikiColumn| column == Some(wiki.ix());
        let color = match (change.change(), change.direction()) {
            (RowChange::Removed, _) => return,
            (RowChange::Inserted, _) => palette.updated(),
            (_, Some(ChangeDirection::Up)) if is(WikiColumn::PerMinute) => palette.up(),
            _ if is(WikiColumn::Last) => palette.updated(),
            _ => return,
        };
        style.set_background(color.opacity(FLASH_TINT * change.intensity()));
    }
}

/// [`WikiChanges`] as a grid's change paint.
pub fn wiki_changes() -> ChangePaint {
    ChangePaint::Custom(Arc::new(WikiChanges))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arrangements_toggle_and_show_the_family_when_flat() {
        assert_eq!(Arrangement::default(), Arrangement::ByFamily);
        assert_eq!(Arrangement::Flat.toggled(), Arrangement::ByFamily);
        assert_eq!(Arrangement::ByFamily.toggled(), Arrangement::Flat);
        assert!(Arrangement::Flat.shows_family());
        assert!(!Arrangement::ByFamily.shows_family());
        for (ix, arrangement) in Arrangement::ALL.iter().enumerate() {
            assert_eq!(arrangement.ix(), ix);
        }
    }

    #[test]
    fn test_quiet_steps_fade_monotonically() {
        assert!(QUIET_OPACITY.windows(2).all(|pair| pair[0] > pair[1]));
        assert_eq!(QUIET_OPACITY[0], 1.);
    }
}
