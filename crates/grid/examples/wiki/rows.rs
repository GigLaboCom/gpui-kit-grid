//! The rows the aggregation writes and the table shows: one per wiki,
//! keyed by its server name. No GPUI types; `value` borrows everything it
//! shows, so it never allocates.

use std::sync::Arc;

use gpui_kit_grid::{CellValue, ChangeDirection, ColumnIx, GridColumns, GridRow, LinkValue};

use super::edit::Family;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WikiColumn {
    /// The server name: the key.
    Wiki,
    Family,
    /// The language code; empty for a wiki of no one language.
    Lang,
    Language,
    /// Edits in the last minute; its trend compares the last half minute
    /// with the one before.
    PerMinute,
    /// Edits since the source started.
    Edits,
    /// The share of those by bots, %.
    Bots,
    NewPages,
    /// Bytes added and removed, over every edit.
    Added,
    Removed,
    /// The time of the last edit, UTC.
    Last,
    /// The last page edited that may be shown, linked.
    Page,
}

impl GridColumns for WikiColumn {
    fn all() -> &'static [Self] {
        use WikiColumn::*;
        &[
            Wiki, Family, Lang, Language, PerMinute, Edits, Bots, NewPages, Added, Removed, Last,
            Page,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Wiki => "wiki",
            Self::Family => "family",
            Self::Lang => "lang",
            Self::Language => "language",
            Self::PerMinute => "per_minute",
            Self::Edits => "edits",
            Self::Bots => "bots",
            Self::NewPages => "new_pages",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Last => "last",
            Self::Page => "page",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// How long a wiki has been quiet, in steps: 0 edited lately, up to
/// [`QUIET_STEPS`] quiet for minutes. The table fades its text by it.
pub type Quiet = u8;
pub const QUIET_STEPS: Quiet = 4;

/// One wiki's row.
#[derive(Clone, Debug, PartialEq)]
pub struct WikiRow {
    pub wiki: Arc<str>,
    pub family: Family,
    pub lang: Arc<str>,
    pub language: Arc<str>,
    pub per_minute: u32,
    pub trend: Option<ChangeDirection>,
    pub edits: u64,
    /// `None` before the first edit.
    pub bots_percent: Option<u8>,
    pub new_pages: u64,
    pub added: u64,
    pub removed: u64,
    pub last: Arc<str>,
    /// The title as shown (`edit::TitleGlyphs`) and its address.
    pub page: Option<(Arc<str>, Arc<str>)>,
    pub quiet: Quiet,
}

impl WikiRow {
    /// The page's address, if the row has a page.
    pub fn url(&self) -> Option<&Arc<str>> {
        self.page.as_ref().map(|(_, url)| url)
    }
}

fn count(value: u64) -> CellValue<'static> {
    CellValue::Int(value.min(i64::MAX as u64) as i64)
}

impl GridRow for WikiRow {
    type Key = Arc<str>;
    type Column = WikiColumn;

    fn key(&self) -> &Arc<str> {
        &self.wiki
    }

    fn value(&self, column: WikiColumn) -> CellValue<'_> {
        match column {
            WikiColumn::Wiki => CellValue::Text(&self.wiki),
            WikiColumn::Family => CellValue::Text(self.family.label()),
            WikiColumn::Lang if self.lang.is_empty() => CellValue::Empty,
            WikiColumn::Lang => CellValue::Text(&self.lang),
            WikiColumn::Language if self.language.is_empty() => CellValue::Empty,
            WikiColumn::Language => CellValue::Text(&self.language),
            WikiColumn::PerMinute => CellValue::Int(self.per_minute.into()),
            WikiColumn::Edits => count(self.edits),
            WikiColumn::Bots => self
                .bots_percent
                .map_or(CellValue::Empty, |percent| CellValue::Int(percent.into())),
            WikiColumn::NewPages => count(self.new_pages),
            WikiColumn::Added => count(self.added),
            WikiColumn::Removed => count(self.removed),
            WikiColumn::Last => CellValue::Text(&self.last),
            WikiColumn::Page => self.page.as_ref().map_or(CellValue::Empty, |(title, url)| {
                CellValue::Link(LinkValue::new(title, url))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_columns_have_their_places_and_ids_once() {
        for (ix, column) in WikiColumn::all().iter().enumerate() {
            assert_eq!(column.ix(), ColumnIx::new(ix as u16));
            assert!(
                WikiColumn::all()[ix + 1..]
                    .iter()
                    .all(|other| other.id() != column.id())
            );
        }
    }
}
