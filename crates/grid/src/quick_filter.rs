//! The quick filter: a text search over the display text of chosen columns.
//! Part of the engine layer, so it has no GPUI types.
//!
//! A [`QuickFilterMatcher`] compiles the text once per change into a
//! [`QuickFilterRule`]. The background [`Rebuild`](crate::Rebuild) asks the
//! rule about every row that passes the view's own filter. Built-in rules
//! are dispatched statically; an application rule costs one dynamic call per
//! row.

use std::{
    cell::Cell,
    fmt,
    ops::ControlFlow,
    sync::{Arc, OnceLock},
};

use crate::model::{
    CellValue, ColumnIx, ColumnSet, GridColumns, GridRow, NumberFormat, display_text_in,
    may_occur_in_number_text,
};

/// How quick filter text matches rows. The default is [`ContainsAllTerms`].
/// Runs off the UI thread, so it is `Send + Sync`.
pub trait QuickFilterMatcher: Send + Sync + 'static {
    /// Compiles `text` once per filter change, not per row.
    /// [`QuickFilter::new`] passes it trimmed and never blank.
    fn compile(&self, text: &str) -> QuickFilterRule;
}

/// A compiled application quick filter, wrapped in
/// [`QuickFilterRule::Custom`].
pub trait QuickFilterQuery: Send + Sync + 'static {
    /// Returns true if the row passes. Called once per row of a rebuild in
    /// the background, on large snapshots from several threads at once: it
    /// must not allocate or lock.
    fn matches(&self, cells: &CellTexts<'_>) -> bool;
}

/// A compiled quick filter: a built-in rule, dispatched statically, or an
/// application one.
#[derive(Clone)]
#[non_exhaustive]
pub enum QuickFilterRule {
    /// Compiled by [`ContainsAllTerms`].
    ContainsAllTerms(ContainsAllTermsQuery),
    /// An application rule: one dynamic call per row.
    Custom(Arc<dyn QuickFilterQuery>),
}

impl QuickFilterQuery for QuickFilterRule {
    fn matches(&self, cells: &CellTexts<'_>) -> bool {
        match self {
            Self::ContainsAllTerms(query) => query.matches(cells),
            Self::Custom(query) => query.matches(cells),
        }
    }
}

impl fmt::Debug for QuickFilterRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainsAllTerms(query) => {
                f.debug_tuple("ContainsAllTerms").field(query).finish()
            }
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// The display text of the quick filter columns of one row, as the body
/// paints it. `Empty` and `Bool` cells have no searchable text and are left
/// out: the body shows `Bool` only as a check mark. An `Icon` cell is
/// searched by its text.
pub struct CellTexts<'a> {
    /// The searchable cells of the current row, in schema order.
    cells: Vec<(ColumnIx, CellValue<'a>)>,
    /// How each column shows its numbers, by `ColumnIx`.
    formats: &'a [NumberFormat],
    /// Numbers, timestamps and custom values are written here, one buffer
    /// for the whole rebuild.
    numbers: Cell<String>,
    /// [`ContainsAllTerms`] lowercases non-ASCII text here.
    lowercase: Cell<String>,
}

impl<'a> CellTexts<'a> {
    fn with_capacity(columns: usize, formats: &'a [NumberFormat]) -> Self {
        Self {
            cells: Vec::with_capacity(columns),
            formats,
            numbers: Cell::default(),
            lowercase: Cell::default(),
        }
    }

    /// Reads the cells of `row`. Allocates only while the buffers grow.
    fn load<R: GridRow>(&mut self, row: &'a R, columns: &[R::Column]) {
        self.cells.clear();
        self.cells.extend(
            columns
                .iter()
                .map(|&column| (column.ix(), row.value(column)))
                .filter(|(_, value)| !matches!(value, CellValue::Empty | CellValue::Bool(_))),
        );
    }

    /// How the column `ix` shows its numbers.
    fn format(&self, ix: ColumnIx) -> NumberFormat {
        self.formats.get(ix.get()).copied().unwrap_or_default()
    }

    /// Visits each quick filter column in schema order until `visit` breaks.
    /// Text and icon cells are borrowed from the row; numbers, timestamps
    /// and custom values are written into a buffer reused across rows.
    pub fn visit(&self, mut visit: impl FnMut(ColumnIx, &str) -> ControlFlow<()>) {
        // Taken, not borrowed: a nested `visit` gets an empty buffer instead
        // of a panic.
        let mut buffer = self.numbers.take();
        for &(column, value) in &self.cells {
            if visit(
                column,
                display_text_in(value, self.format(column), &mut buffer),
            )
            .is_break()
            {
                break;
            }
        }
        self.numbers.set(buffer);
    }
}

/// Every whitespace-separated term occurs, ignoring case, in some column, as
/// in AG Grid's quick filter. Terms may occur in different columns.
///
/// Case is ignored with `char::to_lowercase` (full Unicode), character by
/// character: "сбер" finds "СБЕРБАНК". Diacritics count: "e" does not find
/// "é" (ignoring them is another [`QuickFilterMatcher`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct ContainsAllTerms;

impl QuickFilterMatcher for ContainsAllTerms {
    fn compile(&self, text: &str) -> QuickFilterRule {
        let mut terms: Vec<Term> = Vec::new();
        for term in text.split_whitespace() {
            let text: String = term.chars().flat_map(char::to_lowercase).collect();
            if !terms.iter().any(|known| *known.text == *text) {
                terms.push(Term {
                    is_ascii: text.is_ascii(),
                    in_numbers: may_occur_in_number_text(&text),
                    text: text.into_boxed_str(),
                });
            }
        }
        QuickFilterRule::ContainsAllTerms(ContainsAllTermsQuery {
            terms: terms.into(),
        })
    }
}

/// [`ContainsAllTerms`] compiled: lowercase terms without repeats. Without
/// terms every row passes.
#[derive(Clone, Debug)]
pub struct ContainsAllTermsQuery {
    terms: Arc<[Term]>,
}

#[derive(Debug)]
struct Term {
    /// Lowercase and never empty.
    text: Box<str>,
    is_ascii: bool,
    /// The term may occur in the display text of a number.
    in_numbers: bool,
}

impl Term {
    /// `lowercase` is `text` lowercased when `text` is not ASCII.
    ///
    /// Cells are short, so this compares windows: `str::contains` would build
    /// a searcher per cell and term, which costs more than the search.
    fn occurs_in(&self, text: &str, is_ascii: bool, lowercase: &str) -> bool {
        let needle = self.text.as_bytes();
        if is_ascii {
            // ASCII lowercases to ASCII, so no other term can occur in it.
            self.is_ascii
                && text
                    .as_bytes()
                    .windows(needle.len())
                    .any(|window| window.eq_ignore_ascii_case(needle))
        } else {
            // Bytes of valid UTF-8 match only at character boundaries.
            lowercase
                .as_bytes()
                .windows(needle.len())
                .any(|window| window[0] == needle[0] && window == needle)
        }
    }
}

/// Appends `text` lowercased with `char::to_lowercase`, character by
/// character.
fn push_lowercase(text: &str, out: &mut String) {
    let table = small_lowercase();
    for c in text.chars() {
        match table.get(c as usize) {
            Some(&lower) if lower != '\0' => out.push(lower),
            _ => out.extend(c.to_lowercase()),
        }
    }
}

/// The lowercase of every character below U+0800 (Latin, Greek, Cyrillic,
/// Armenian, Hebrew, Arabic) that lowercases to one character, and `'\0'`
/// for the rest. Built once from `char::to_lowercase`, so it agrees with it;
/// it saves a binary search per character, the larger part of a quick filter
/// over Cyrillic text.
fn small_lowercase() -> &'static [char; 0x800] {
    static TABLE: OnceLock<[char; 0x800]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = ['\0'; 0x800];
        for (code, lower) in table.iter_mut().enumerate() {
            let c = char::from_u32(code as u32).unwrap_or('\0');
            let mut lowercase = c.to_lowercase();
            if let (Some(single), None) = (lowercase.next(), lowercase.next()) {
                *lower = single;
            }
        }
        table
    })
}

impl QuickFilterQuery for ContainsAllTermsQuery {
    fn matches(&self, cells: &CellTexts<'_>) -> bool {
        // More than 64 terms take a pass per 64.
        self.terms
            .chunks(u64::BITS as usize)
            .all(|terms| cells.contains_all(terms))
    }
}

impl CellTexts<'_> {
    /// Returns true if every term occurs in some cell, with a bit per term,
    /// and stops as soon as every term is found. Which cell holds a term
    /// does not matter, so text and the text of icons, which are borrowed,
    /// are searched first; numbers are formatted only while a missing term
    /// may occur in one, timestamps and custom values while any term is
    /// missing.
    fn contains_all(&self, terms: &[Term]) -> bool {
        let all = u64::MAX >> (u64::BITS as usize - terms.len());
        let in_numbers = terms
            .iter()
            .enumerate()
            .filter(|(_, term)| term.in_numbers)
            .fold(0u64, |mask, (ix, _)| mask | 1 << ix);
        let mut found = 0u64;
        let mut lowercase = self.lowercase.take();
        for &(_, value) in &self.cells {
            let text = match value {
                CellValue::Text(text) => text,
                CellValue::Icon(icon) => icon.text(),
                CellValue::Image(image) => image.text(),
                CellValue::Link(link) => link.text(),
                _ => continue,
            };
            find_terms(terms, all, &mut found, text, &mut lowercase);
            if found == all {
                break;
            }
        }
        if found != all {
            let mut buffer = self.numbers.take();
            for &(column, value) in &self.cells {
                if found == all {
                    break;
                }
                let wanted = match value {
                    CellValue::Text(_)
                    | CellValue::Icon(_)
                    | CellValue::Image(_)
                    | CellValue::Link(_) => 0,
                    _ if value.is_number() => in_numbers,
                    _ => all,
                };
                if wanted & !found != 0 {
                    let text = display_text_in(value, self.format(column), &mut buffer);
                    find_terms(terms, wanted, &mut found, text, &mut lowercase);
                }
            }
            self.numbers.set(buffer);
        }
        self.lowercase.set(lowercase);
        found == all
    }
}

/// Sets the bit of every term of `terms` in `wanted` and missing from
/// `found` that occurs in `text`. `lowercase` is a buffer for non-ASCII
/// text.
fn find_terms(terms: &[Term], wanted: u64, found: &mut u64, text: &str, lowercase: &mut String) {
    let is_ascii = text.is_ascii();
    if !is_ascii {
        lowercase.clear();
        push_lowercase(text, lowercase);
    }
    for (ix, term) in terms.iter().enumerate() {
        let bit = 1u64 << ix;
        if wanted & !*found & bit != 0 && term.occurs_in(text, is_ascii, lowercase) {
            *found |= bit;
        }
    }
}

/// A compiled quick filter over a set of columns. Cloning is cheap.
#[derive(Clone, Debug)]
pub struct QuickFilter {
    text: Arc<str>,
    columns: ColumnSet,
    rule: QuickFilterRule,
    formats: Arc<[NumberFormat]>,
}

impl QuickFilter {
    /// Compiles `text`, trimmed, to search `columns`. `None` when the text
    /// is empty or only whitespace: no filter at all.
    pub fn new(text: &str, columns: ColumnSet, matcher: &dyn QuickFilterMatcher) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(Self {
            rule: matcher.compile(text),
            text: text.into(),
            columns,
            formats: Arc::new([]),
        })
    }

    /// Searches numbers as these formats show them, by `ColumnIx`
    /// (`GridColumn::with_number_format`); a column past the end shows the
    /// default. The grid sets its columns' formats.
    pub fn with_number_formats(mut self, formats: impl Into<Arc<[NumberFormat]>>) -> Self {
        self.formats = formats.into();
        self
    }

    /// The text, trimmed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The columns searched. The view depends on them: a change in one of
    /// them rebuilds it.
    pub fn columns(&self) -> ColumnSet {
        self.columns
    }
}

/// A quick filter applied row by row in one rebuild: the columns are
/// resolved once and every row shares the text buffers.
pub(crate) struct QuickFilterPass<'a, R: GridRow> {
    rule: &'a QuickFilterRule,
    columns: Vec<R::Column>,
    cells: CellTexts<'a>,
}

impl<'a, R: GridRow> QuickFilterPass<'a, R> {
    pub(crate) fn new(filter: &'a QuickFilter) -> Self {
        let columns: Vec<R::Column> = R::Column::all()
            .iter()
            .copied()
            .filter(|column| filter.columns.contains(column.ix()))
            .collect();
        Self {
            rule: &filter.rule,
            cells: CellTexts::with_capacity(columns.len(), &filter.formats),
            columns,
        }
    }

    /// Returns true if `row` passes. Allocates only while the buffers grow.
    pub(crate) fn matches(&mut self, row: &'a R) -> bool {
        self.cells.load(row, &self.columns);
        self.rule.matches(&self.cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Column {
        Ticker,
        Name,
        Price,
        Quantity,
        Active,
        Note,
    }

    impl GridColumns for Column {
        fn all() -> &'static [Self] {
            &[
                Self::Ticker,
                Self::Name,
                Self::Price,
                Self::Quantity,
                Self::Active,
                Self::Note,
            ]
        }

        fn id(self) -> &'static str {
            match self {
                Self::Ticker => "ticker",
                Self::Name => "name",
                Self::Price => "price",
                Self::Quantity => "quantity",
                Self::Active => "active",
                Self::Note => "note",
            }
        }

        fn ix(self) -> ColumnIx {
            ColumnIx::new(self as u16)
        }
    }

    struct Row {
        ticker: &'static str,
        name: &'static str,
        price: f64,
        quantity: i64,
    }

    impl GridRow for Row {
        type Key = &'static str;
        type Column = Column;

        fn key(&self) -> &&'static str {
            &self.ticker
        }

        fn value(&self, column: Column) -> CellValue<'_> {
            match column {
                Column::Ticker => CellValue::Text(self.ticker),
                Column::Name => CellValue::Text(self.name),
                Column::Price => CellValue::Float(self.price),
                Column::Quantity => CellValue::Int(self.quantity),
                Column::Active => CellValue::Bool(true),
                Column::Note => CellValue::Empty,
            }
        }
    }

    const SBER: Row = Row {
        ticker: "SBER",
        name: "СБЕРБАНК",
        price: 12.5,
        quantity: 1500,
    };

    fn columns(columns: &[Column]) -> ColumnSet {
        let mut set = ColumnSet::EMPTY;
        for column in columns {
            set.insert(column.ix());
        }
        set
    }

    fn passes(text: &str, set: ColumnSet, row: &Row) -> bool {
        let filter = QuickFilter::new(text, set, &ContainsAllTerms).expect("not blank");
        QuickFilterPass::<Row>::new(&filter).matches(row)
    }

    #[test]
    fn test_contains_all_terms() {
        let all = ColumnSet::first(Column::all().len());
        // Case is ignored for ASCII and Cyrillic; a term may be a substring.
        for text in ["sber", "Sber", "сбер", "Сбер", "БАНК", "ербан"] {
            assert!(passes(text, all, &SBER), "{text}");
        }
        // Every term is required, but each may occur in another column.
        assert!(passes("sber сбер 2.5", all, &SBER));
        assert!(!passes("sber gazp", all, &SBER));
        // Extra whitespace only separates terms; the text is trimmed.
        assert!(passes("  sber \t  2.5 ", all, &SBER));
        let filter = QuickFilter::new("  sber \t 2.5 ", all, &ContainsAllTerms).unwrap();
        assert_eq!(filter.text(), "sber \t 2.5");
        assert_eq!(filter.columns(), all);
        // Repeated terms are compiled once.
        let QuickFilterRule::ContainsAllTerms(query) = ContainsAllTerms.compile("Sber SBER sber")
        else {
            panic!("a built-in rule");
        };
        assert_eq!(query.terms.len(), 1);
        // Empty and blank text is no filter at all.
        for blank in ["", "   ", " \t\n "] {
            assert!(QuickFilter::new(blank, all, &ContainsAllTerms).is_none());
        }
        // Empty and Bool cells have no text: the check mark is not searchable.
        for text in ["✓", "true"] {
            assert!(!passes(text, all, &SBER), "{text}");
        }
        // Numbers are searched by their display text: 12.5 shows as "12.50".
        for text in ["2.5", "12.50", "150", "1500"] {
            assert!(passes(text, all, &SBER), "{text}");
        }
        assert!(!passes("12.500", all, &SBER));
        // Only the given columns are searched.
        assert!(passes("сбер", columns(&[Column::Name]), &SBER));
        assert!(!passes("sber 2.5", columns(&[Column::Name]), &SBER));
        assert!(!passes("sber", ColumnSet::EMPTY, &SBER));
        // More terms than one pass tracks: 70 distinct terms, all present.
        let many: Vec<String> = (0..70).map(|ix| format!("{ix}")).collect();
        let row = Row {
            name: Box::leak(
                (0..70)
                    .map(|ix| format!("<{ix}>"))
                    .collect::<String>()
                    .into(),
            ),
            ..SBER
        };
        assert!(passes(&many.join(" "), all, &row));
        assert!(!passes(&format!("{} 70", many.join(" ")), all, &row));
    }

    #[test]
    fn test_lowercase_table() {
        // The table only skips a binary search: every character lowercases
        // exactly as `char::to_lowercase` does, in any Unicode version.
        let (mut fast, mut expected) = (String::new(), String::new());
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            fast.clear();
            expected.clear();
            push_lowercase(c.encode_utf8(&mut [0; 4]), &mut fast);
            expected.extend(c.to_lowercase());
            assert_eq!(fast, expected, "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn test_contains_all_terms_searches_text_first() {
        let all = ColumnSet::first(Column::all().len());
        let nan = Row {
            price: f64::NAN,
            ..SBER
        };
        // A term no number shows is never looked for in numbers: the
        // number buffer stays empty, found or not.
        for (text, expected) in [("банк", true), ("sber банк", true), ("gazp", false)] {
            let filter = QuickFilter::new(text, all, &ContainsAllTerms).unwrap();
            let mut pass = QuickFilterPass::<Row>::new(&filter);
            assert_eq!(pass.matches(&SBER), expected, "{text}");
            assert_eq!(pass.cells.numbers.take(), "", "{text}");
        }
        // Missing from the text, a term that may be in a number is looked
        // for there, and the row fails only after the last number.
        assert!(passes("sber 1500", all, &SBER));
        assert!(passes("nan", all, &nan));
        assert!(passes("NaN сбер", all, &nan));
        assert!(!passes("sber 1501", all, &SBER));
        assert!(!passes("банк nan", all, &SBER));
        // A term in both text and numbers, and terms only in numbers.
        assert!(passes("50 12.5 150", all, &SBER));
        assert!(!passes("sber gazp 1500", all, &SBER));
        // Text columns that hold digits are searched like any text.
        let digits = Row {
            name: "Counterparty 42",
            ..SBER
        };
        assert!(passes("42 party", columns(&[Column::Name]), &digits));
    }

    #[test]
    fn test_cell_texts_visit() {
        let filter = QuickFilter::new("x", ColumnSet::first(6), &ContainsAllTerms).unwrap();
        let mut pass = QuickFilterPass::<Row>::new(&filter);
        pass.cells.load(&SBER, &pass.columns);
        // Schema order, without the Bool and Empty columns, numbers as painted.
        let mut visited = Vec::new();
        pass.cells.visit(|column, text| {
            visited.push((column, text.to_owned()));
            ControlFlow::Continue(())
        });
        assert_eq!(
            visited,
            [
                (Column::Ticker.ix(), "SBER".to_owned()),
                (Column::Name.ix(), "СБЕРБАНК".to_owned()),
                (Column::Price.ix(), "12.50".to_owned()),
                (Column::Quantity.ix(), "1500".to_owned()),
            ]
        );
        // Breaking stops the visit.
        let mut count = 0;
        pass.cells.visit(|_, _| {
            count += 1;
            ControlFlow::Break(())
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn test_quick_filter_new_values() {
        use chrono::{DateTime, TimeZone as _, Utc};

        use crate::model::{GridValue, IconValue};

        /// Shows as `3M`.
        #[derive(Debug)]
        struct Tenor(u32);

        impl GridValue for Tenor {
            fn cmp_value(&self, other: &dyn GridValue) -> std::cmp::Ordering {
                other
                    .downcast_ref::<Tenor>()
                    .map_or(std::cmp::Ordering::Less, |other| self.0.cmp(&other.0))
            }

            fn write_text(&self, out: &mut String) {
                use std::fmt::Write as _;
                let _ = write!(out, "{}M", self.0 / 30);
            }
        }

        /// An order: the columns of `Column` with an icon, a time and a
        /// custom value in place of the ticker, the name and the price.
        struct Order {
            status: IconValue<'static>,
            placed: DateTime<Utc>,
            tenor: Tenor,
        }

        impl GridRow for Order {
            type Key = u8;
            type Column = Column;

            fn key(&self) -> &u8 {
                &0
            }

            fn value(&self, column: Column) -> CellValue<'_> {
                match column {
                    Column::Ticker => CellValue::Icon(self.status),
                    Column::Name => CellValue::Timestamp(self.placed),
                    Column::Price => CellValue::Custom(&self.tenor),
                    Column::Quantity => CellValue::Int(1500),
                    Column::Active => CellValue::Bool(true),
                    Column::Note => CellValue::Empty,
                }
            }
        }

        let order = Order {
            status: IconValue::new("icons/circle-check.svg", "Filled"),
            placed: Utc.with_ymd_and_hms(2026, 11, 4, 12, 34, 56).unwrap(),
            tenor: Tenor(90),
        };
        let passes = |text: &str, set: ColumnSet| {
            let filter = QuickFilter::new(text, set, &ContainsAllTerms).expect("not blank");
            QuickFilterPass::<Order>::new(&filter).matches(&order)
        };
        let all = ColumnSet::first(Column::all().len());
        // The icon by its text, never its path; the time by its text in UTC;
        // the custom value by its text.
        for text in [
            "filled",
            "FILL",
            "2026-11",
            "12:34",
            "11-04 12",
            "3m",
            "filled 3M 2026 1500",
        ] {
            assert!(passes(text, all), "{text}");
        }
        for text in ["circle", "svg", "2026-11-05", "12:35", "90", "3M gazp"] {
            assert!(!passes(text, all), "{text}");
        }
        assert!(passes("fill", columns(&[Column::Ticker])));
        assert!(!passes("fill", columns(&[Column::Name, Column::Price])));
        assert!(passes("3m", columns(&[Column::Price])));

        // The icon's text is borrowed and searched first: a term found there
        // writes nothing into the buffer.
        let filter = QuickFilter::new("filled", all, &ContainsAllTerms).unwrap();
        let mut pass = QuickFilterPass::<Order>::new(&filter);
        assert!(pass.matches(&order));
        assert_eq!(pass.cells.numbers.take(), "");
        // Visiting shows each cell as the body paints it.
        pass.cells.load(&order, &pass.columns);
        let mut visited = Vec::new();
        pass.cells.visit(|column, text| {
            visited.push((column, text.to_owned()));
            ControlFlow::Continue(())
        });
        assert_eq!(
            visited,
            [
                (Column::Ticker.ix(), "Filled".to_owned()),
                (Column::Name.ix(), "2026-11-04 12:34:56".to_owned()),
                (Column::Price.ix(), "3M".to_owned()),
                (Column::Quantity.ix(), "1500".to_owned()),
            ]
        );
    }
}
