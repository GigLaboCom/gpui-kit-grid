//! Rows, columns and cell values. This module has no GPUI types, so it can be
//! promoted to `gpui-base` later.

#[cfg(feature = "decimal")]
mod decimal;
mod float_text;
mod timestamp;

use std::{
    any::Any,
    borrow::{Borrow, Cow},
    cmp::Ordering,
    fmt::{self, Debug, Write as _},
    hash::{Hash, Hasher},
    sync::Arc,
};

use chrono::{DateTime, Utc};
#[cfg(feature = "decimal")]
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};

pub(crate) use self::float_text::push_fixed;
pub(crate) use self::timestamp::push_timestamp;

use crate::highlight::ChangeDirection;

/// The index of a column in the schema. Stable for the schema's lifetime and
/// used on the hot path. Persisted layouts use [`ColumnId`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ColumnIx(u16);

impl ColumnIx {
    /// Creates a column index.
    pub const fn new(ix: u16) -> Self {
        Self(ix)
    }

    /// Returns the index as `usize`.
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// A stable column identifier for persisted layouts: the value of
/// [`GridColumns::id`]. Unlike [`ColumnIx`] it survives schema changes, so
/// saved layouts and sort keys refer to columns by it. Not for the hot path.
///
/// An id from the schema borrows its `&'static str` and does not allocate;
/// an id read from a saved layout owns its text. Both compare, hash and
/// order by the text alone, so either can look up the other in a map.
/// Serialized as a plain string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ColumnId(Cow<'static, str>);

impl ColumnId {
    /// An id that borrows a static string, such as a [`GridColumns::id`].
    pub const fn new_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    /// An id that owns its text, such as one read from a file.
    pub fn new(id: impl Into<String>) -> Self {
        Self(Cow::Owned(id.into()))
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&'static str> for ColumnId {
    fn from(id: &'static str) -> Self {
        Self::new_static(id)
    }
}

impl From<String> for ColumnId {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

impl AsRef<str> for ColumnId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ColumnId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ColumnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The set of columns of a row type, usually an application `enum`.
///
/// # Deriving
///
/// With the `derive` feature, `#[derive(GridColumns)]` writes this
/// implementation for an enum of unit variants. Declaration order is the
/// schema order, and a column's id is its variant name in `snake_case`, as
/// a hand-written implementation spells it, so moving to the derive keeps
/// saved layouts. `#[grid_column(id = "…")]` sets one column's id,
/// `#[grid_columns(rename_all = "…")]` the naming rule, and
/// `#[grid_columns(crate = "…")]` the path of a re-exported grid crate.
/// The derive refuses at compile time what a hand-written implementation
/// can get wrong: a variant missing from `all()`, an index shifted by an
/// explicit discriminant, a repeated id, more than 128 columns.
///
#[cfg_attr(feature = "derive", doc = "```")]
#[cfg_attr(not(feature = "derive"), doc = "```ignore")]
/// use gpui_kit_grid::GridColumns;
///
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum TradeColumn {
///     Id,
///     Instrument,
///     BidSize, // "bid_size"
///     #[grid_column(id = "px")]
///     Price,
/// }
///
/// assert_eq!(TradeColumn::BidSize.id(), "bid_size");
/// assert_eq!(TradeColumn::Price.id(), "px");
/// assert_eq!(TradeColumn::Price.ix().get(), 3);
/// ```
pub trait GridColumns: Copy + Eq + Hash + Debug + Send + Sync + 'static {
    /// Every column in schema order. `all()[c.ix().get()] == c`.
    fn all() -> &'static [Self];
    /// A stable identifier for persisted layouts and element ids. Unique
    /// within the schema; changing it orphans the column in saved layouts.
    fn id(self) -> &'static str;
    /// The schema index of the column.
    fn ix(self) -> ColumnIx;

    /// The column's identifier as a [`ColumnId`]. Borrows [`GridColumns::id`]
    /// without allocating.
    fn column_id(self) -> ColumnId {
        ColumnId::new_static(self.id())
    }
}

/// A set of columns, up to 128.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColumnSet(u128);

impl ColumnSet {
    /// No columns.
    pub const EMPTY: Self = Self(0);

    /// The first `count` columns.
    pub fn first(count: usize) -> Self {
        assert!(count <= 128, "a grid supports at most 128 columns");
        if count == 128 {
            Self(u128::MAX)
        } else {
            Self((1u128 << count) - 1)
        }
    }

    /// Adds a column.
    pub fn insert(&mut self, column: ColumnIx) {
        self.0 |= 1u128 << column.get();
    }

    /// Returns true if the set contains the column.
    pub fn contains(self, column: ColumnIx) -> bool {
        self.0 & (1u128 << column.get()) != 0
    }

    /// Returns true if the set has no columns.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns true if both sets share a column.
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Adds every column of `other`.
    pub fn union_with(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// The number of columns in the set.
    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// The number of columns of the set before `column`: the place of
    /// `column` among the set's columns in schema order, if it is one.
    #[inline]
    pub(crate) fn rank(self, column: ColumnIx) -> usize {
        let below = (1u128 << column.get()) - 1;
        (self.0 & below).count_ones() as usize
    }

    /// The columns in both sets.
    pub(crate) fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Removes a column.
    pub(crate) fn remove(&mut self, column: ColumnIx) {
        self.0 &= !(1u128 << column.get());
    }

    /// The columns of the set in schema order.
    pub(crate) fn iter(self) -> impl Iterator<Item = ColumnIx> {
        let mut bits = self.0;
        std::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let ix = bits.trailing_zeros() as u16;
            bits &= bits - 1;
            Some(ColumnIx::new(ix))
        })
    }

    /// The set as a bit mask: bit `n` is the column of index `n`.
    pub(crate) const fn bits(self) -> u128 {
        self.0
    }

    /// The set of a bit mask from [`Self::bits`].
    pub(crate) const fn from_bits(bits: u128) -> Self {
        Self(bits)
    }
}

/// A cell value. Numbers, points in time and borrowed text do not allocate.
///
/// `==` compares two values of one variant: floats with `f64::eq` (a NaN is
/// not equal to itself, -0.0 equals 0.0), decimals by value (`1.0` equals
/// `1.00`), icons by path and text, custom values by
/// [`GridValue::cmp_value`]. Values of two variants are never equal, not
/// even `Int(1)` and `Float(1.0)`. [`GridRow::changed_columns`] diffs rows
/// with it; [`CellValue::cmp_total`] is the order.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum CellValue<'a> {
    /// No value.
    Empty,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating point number.
    Float(f64),
    /// An exact decimal number with its scale: money, quantities with a
    /// fixed number of decimals. `12.30` shows as `12.30`. Needs the
    /// `decimal` feature.
    #[cfg(feature = "decimal")]
    Decimal(Decimal),
    /// Text borrowed from the row.
    Text(&'a str),
    /// A point in time. Shown in UTC as `2026-11-04 12:34:56` until a column
    /// formatter exists; another zone or format is a [`CellValue::Custom`]
    /// value with its own text. A date without a time is the timestamp of
    /// its midnight in UTC.
    Timestamp(DateTime<Utc>),
    /// An icon with the text that stands for it. The body paints the icon;
    /// the text is what a screen reader, a tooltip, the quick filter, a copy
    /// and the sort read.
    Icon(IconValue<'a>),
    /// An image with the text that stands for it: a raster or a colour SVG
    /// that the grid's image source (`LiveGridState::with_image_source`)
    /// finds by its id. Like an icon, the body paints the image and the text
    /// is what a screen reader, a tooltip, the quick filter, a copy and the
    /// sort read.
    Image(ImageValue<'a>),
    /// A link: its text, which the cell shows in the theme's link colour,
    /// underlined, and the address a click on the text or Enter on the
    /// focused cell opens (`LiveGridState::with_link_opener`). The text is
    /// the value everywhere else, as an icon's: the sort, the quick filter,
    /// a copy, an export and a screen reader, which hears "link" after it.
    /// Painted by the body: no element per cell.
    Link(LinkValue<'a>),
    /// A value of the application's own type, borrowed from the row: its own
    /// order, text and export. See [`GridValue`].
    Custom(&'a dyn GridValue),
}

impl PartialEq for CellValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            #[cfg(feature = "decimal")]
            (Self::Decimal(a), Self::Decimal(b)) => a == b,
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Timestamp(a), Self::Timestamp(b)) => a == b,
            (Self::Icon(a), Self::Icon(b)) => a == b,
            (Self::Image(a), Self::Image(b)) => a == b,
            (Self::Link(a), Self::Link(b)) => a == b,
            (Self::Custom(a), Self::Custom(b)) => a.cmp_value(*b).is_eq(),
            _ => false,
        }
    }
}

impl CellValue<'_> {
    /// A total order across all values: `Empty` first, then `Bool`,
    /// numbers, `Timestamp`, `Text`, `Icon` and `Image`, and `Custom` last,
    /// each by value.
    ///
    /// Numbers follow `f64::total_cmp`, and an `Int` compares exactly, as
    /// the float of its value would: `Int(1)` equals `Float(1.0)` and
    /// `Int(2^53 + 1)` is above `Float(2^53)`. So -0.0 sorts just below 0.0
    /// and `Int(0)`, a negative NaN before -∞ and a positive NaN after +∞.
    ///
    /// A `Decimal` (the `decimal` feature) is a number too and compares
    /// exactly by its value, whatever its scale: `Decimal(1.00)` equals
    /// `Int(1)` and `Float(1.0)`, `Decimal(0.1)` is below `Float(0.1)`,
    /// whose binary value is a little above 0.1, and a zero `Decimal` of any
    /// sign or scale sorts as `Int(0)`, above -0.0.
    ///
    /// Timestamps compare by time. An `Icon`, an `Image` and a `Link` sort
    /// by their text, as `Text` does: an icon, an image, a link and a text
    /// with the same text are equal. Custom values compare with
    /// [`GridValue::cmp_value`], which keeps the order total.
    ///
    /// Not the same as `==`, which compares floats with `f64::eq` (a NaN is
    /// not equal to itself, -0.0 equals 0.0), compares decimals by value
    /// (`1.0` equals `1.00`) and never calls values of two variants equal.
    pub fn cmp_total(&self, other: &Self) -> Ordering {
        fn rank(value: &CellValue) -> u8 {
            match value {
                CellValue::Empty => 0,
                CellValue::Bool(_) => 1,
                CellValue::Int(_) | CellValue::Float(_) => 2,
                #[cfg(feature = "decimal")]
                CellValue::Decimal(_) => 2,
                CellValue::Timestamp(_) => 3,
                CellValue::Text(_)
                | CellValue::Icon(_)
                | CellValue::Image(_)
                | CellValue::Link(_) => 4,
                CellValue::Custom(_) => 5,
            }
        }
        match (self, other) {
            (CellValue::Bool(a), CellValue::Bool(b)) => a.cmp(b),
            (CellValue::Int(a), CellValue::Int(b)) => a.cmp(b),
            (CellValue::Float(a), CellValue::Float(b)) => a.total_cmp(b),
            (CellValue::Int(a), CellValue::Float(b)) => cmp_int_float(*a, *b),
            (CellValue::Float(a), CellValue::Int(b)) => cmp_int_float(*b, *a).reverse(),
            #[cfg(feature = "decimal")]
            (CellValue::Decimal(a), CellValue::Decimal(b)) => a.cmp(b),
            // Every `i64` is a `Decimal`: exact.
            #[cfg(feature = "decimal")]
            (CellValue::Decimal(a), CellValue::Int(b)) => a.cmp(&Decimal::from(*b)),
            #[cfg(feature = "decimal")]
            (CellValue::Int(a), CellValue::Decimal(b)) => Decimal::from(*a).cmp(b),
            #[cfg(feature = "decimal")]
            (CellValue::Decimal(a), CellValue::Float(b)) => decimal::cmp_decimal_float(*a, *b),
            #[cfg(feature = "decimal")]
            (CellValue::Float(a), CellValue::Decimal(b)) => {
                decimal::cmp_decimal_float(*b, *a).reverse()
            }
            (CellValue::Text(a), CellValue::Text(b)) => a.cmp(b),
            (CellValue::Timestamp(a), CellValue::Timestamp(b)) => a.cmp(b),
            (CellValue::Custom(a), CellValue::Custom(b)) => a.cmp_value(*b),
            // Text, an icon's text and an image's text: by the text.
            _ => match (self.sort_text(), other.sort_text()) {
                (Some(a), Some(b)) => a.cmp(b),
                _ => rank(self).cmp(&rank(other)),
            },
        }
    }

    /// The text a `Text`, `Icon`, `Image` or `Link` sorts by.
    fn sort_text(&self) -> Option<&str> {
        match self {
            CellValue::Text(text) => Some(text),
            CellValue::Icon(icon) => Some(icon.text),
            CellValue::Image(image) => Some(image.text),
            CellValue::Link(link) => Some(link.text),
            _ => None,
        }
    }

    /// Returns true for numbers, which are right-aligned by default.
    pub fn is_number(&self) -> bool {
        match self {
            CellValue::Int(_) | CellValue::Float(_) => true,
            #[cfg(feature = "decimal")]
            CellValue::Decimal(_) => true,
            _ => false,
        }
    }
}

/// An icon in a cell ([`CellValue::Icon`]): the asset path of its SVG (the
/// `path()` of a gpui-kit `IconNamed`, or the application's own asset) and
/// the text that stands for it.
///
/// The text is the cell's value everywhere but on the screen: a screen
/// reader, a tooltip, the quick filter, a copy, an export and the sort read
/// it. Both borrow from the row: no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IconValue<'a> {
    path: &'a str,
    text: &'a str,
}

impl<'a> IconValue<'a> {
    /// An icon from its asset path and its text.
    pub const fn new(path: &'a str, text: &'a str) -> Self {
        Self { path, text }
    }

    /// The asset path of the icon's SVG.
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// The text that stands for the icon.
    pub const fn text(&self) -> &'a str {
        self.text
    }
}

/// A link in a cell ([`CellValue::Link`]): the text the cell shows and the
/// address it opens. Both borrow from the row: no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkValue<'a> {
    text: &'a str,
    url: &'a str,
}

impl<'a> LinkValue<'a> {
    /// A link showing `text` that opens `url`.
    pub const fn new(text: &'a str, url: &'a str) -> Self {
        Self { text, url }
    }

    /// The text the cell shows.
    pub const fn text(&self) -> &'a str {
        self.text
    }

    /// The address the link opens.
    pub const fn url(&self) -> &'a str {
        self.url
    }
}

/// An image in a cell ([`CellValue::Image`]): its id, which the grid's
/// image source reads (an asset path, an absolute file path, an
/// `http(s)://` URI or a key of the application's own), and the text that
/// stands for it.
///
/// The text is the cell's value everywhere but on the screen, as an icon's.
/// Both borrow from the row: no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageValue<'a> {
    id: &'a str,
    text: &'a str,
}

impl<'a> ImageValue<'a> {
    /// An image from its id and its text.
    pub const fn new(id: &'a str, text: &'a str) -> Self {
        Self { id, text }
    }

    /// The id the image source finds the image by.
    pub const fn id(&self) -> &'a str {
        self.id
    }

    /// The text that stands for the image.
    pub const fn text(&self) -> &'a str {
        self.text
    }
}

/// A cell value of the application's own type ([`CellValue::Custom`]): a
/// severity that sorts by rank, a tenor, a price in ticks, a time in the
/// local zone. The type decides its order, its text and what an export
/// writes; the row lends it, so a cell does not allocate.
///
/// Called on the UI thread in prepaint for visible cells, and in the
/// background for every row a rebuild sorts or filters: `cmp_value` and
/// `write_text` must not allocate (beyond `out` growing) or lock.
///
/// ```
/// use std::{any::TypeId, cmp::Ordering};
///
/// use gpui_kit_grid::{CellValue, GridValue};
///
/// /// Sorts by rank, worst first; shows its name.
/// #[derive(Clone, Copy, Debug)]
/// enum Severity {
///     Critical,
///     Warning,
///     Info,
/// }
///
/// impl GridValue for Severity {
///     fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
///         match other.downcast_ref::<Severity>() {
///             Some(other) => (*self as u8).cmp(&(*other as u8)),
///             // A value of another type: by type, so the order stays total.
///             None => TypeId::of::<Self>().cmp(&other.as_any().type_id()),
///         }
///     }
///
///     fn write_text(&self, out: &mut String) {
///         out.push_str(match self {
///             Self::Critical => "Critical",
///             Self::Warning => "Warning",
///             Self::Info => "Info",
///         });
///     }
/// }
///
/// let (critical, info) = (Severity::Critical, Severity::Info);
/// let (critical, info) = (CellValue::Custom(&critical), CellValue::Custom(&info));
/// assert!(critical.cmp_total(&info).is_lt());
/// // Custom values sort after text.
/// assert!(CellValue::Text("Critical").cmp_total(&critical).is_lt());
/// ```
pub trait GridValue: Any + Debug + Send + Sync {
    /// The order of this value against `other`, which may be of another
    /// type: `downcast_ref` tells. Values of two types must order by type,
    /// the same way every time (for example by the `TypeId` of `as_any`),
    /// so the order stays total: a sort relies on it. `Equal` also makes two cells `==`,
    /// which is how [`GridRow::changed_columns`] sees a change.
    fn cmp_value(&self, other: &dyn GridValue) -> Ordering;

    /// Appends the text shown in the cell, which the quick filter searches
    /// and a displayed copy writes. Appends with `push_str` or `write!`:
    /// `out` may already hold text, which must stay.
    fn write_text(&self, out: &mut String);

    /// What a raw copy ([`RawCells`](crate::RawCells)) and a file export
    /// write for the cell; `None` (the default) writes the text. A
    /// `CellValue::Custom` here is written as its text.
    fn export_value(&self) -> Option<CellValue<'_>> {
        None
    }

    /// The owned form of the value: a group key, a `Min` or `Max`
    /// aggregate, an edited value. The default is [`OwnedValue::Text`] of
    /// [`GridValue::write_text`]: a group caption shows the text, and an
    /// order by the aggregate ([`AggregateOrder`](crate::AggregateOrder))
    /// follows the text.
    ///
    /// Return [`OwnedValue::custom`] of a copy to keep the value itself:
    /// its order, its text, and its data for a caption hook, which
    /// downcasts [`GroupCaptionContext::value`](crate::GroupCaptionContext::value).
    /// Groups follow the values' own order either way: the rows are sorted
    /// by the borrowed values before the groups are found.
    fn to_owned_value(&self) -> OwnedValue {
        let mut text = String::new();
        self.write_text(&mut text);
        OwnedValue::Text(text.into())
    }
}

impl dyn GridValue {
    /// The value as `Any`, to downcast it.
    pub fn as_any(&self) -> &dyn Any {
        self
    }

    /// The value as `T`, if it is one.
    pub fn downcast_ref<T: GridValue>(&self) -> Option<&T> {
        self.as_any().downcast_ref()
    }
}

/// An owned cell value: a group key kept across snapshots, an aggregate
/// result.
///
/// Two values are equal exactly when [`CellValue::cmp_total`] calls them
/// equal, and equal values hash alike: `Int(1) == Float(1.0)`, while -0.0
/// and 0.0 are two values and a NaN equals only itself, bit for bit. A
/// `Decimal` equals the numbers of its value: `Decimal(1.00) == Int(1)`,
/// `Decimal(0.5) == Float(0.5)`.
///
/// Serialized as JSON-like data: `Empty` as `null`, `Bool` as a boolean,
/// `Int` as an integer, `Float` as a number with a fraction or an exponent
/// (`1.0`, never `1`, so it reads back as a `Float`), `Text` as a string,
/// `Decimal` as `{"decimal": "12.30"}`, text that keeps the scale,
/// `Timestamp` as `{"timestamp": "2026-11-04T12:34:56Z"}`, RFC 3339 in UTC
/// with the fraction it has. A NaN, an infinity or a timestamp outside the
/// years 0 to 9999 is not serialized: writing one fails. A string reads back
/// as `Text`, never as a timestamp.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum OwnedValue {
    /// No value.
    Empty,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating point number.
    Float(f64),
    /// An exact decimal number with its scale. Needs the `decimal` feature.
    #[cfg(feature = "decimal")]
    Decimal(Decimal),
    /// Text.
    Text(Arc<str>),
    /// A point in time.
    Timestamp(DateTime<Utc>),
    /// A value of the application's own type, shared: what
    /// [`GridValue::to_owned_value`] returns to keep its order and text.
    /// Two are equal when [`GridValue::cmp_value`] says so; it hashes by
    /// its type. Serialized as its [`GridValue::export_value`], else as
    /// its text, and read back as that: a restored layout's group of a
    /// custom key does not find the group again.
    Custom(Arc<dyn GridValue>),
}

impl OwnedValue {
    /// A value of the application's own type, kept as itself: for
    /// [`GridValue::to_owned_value`], usually `OwnedValue::custom(self.clone())`.
    /// Allocates once.
    pub fn custom(value: impl GridValue) -> Self {
        Self::Custom(Arc::new(value))
    }

    /// Copies a borrowed value; allocates only for `Text` and `Icon`, and
    /// for `Custom` as its [`GridValue::to_owned_value`] does. An `Icon`
    /// becomes the `Text` of its text: the icon is the cell's look, its
    /// text the value.
    pub fn from_cell(value: CellValue<'_>) -> Self {
        match value {
            CellValue::Empty => Self::Empty,
            CellValue::Bool(value) => Self::Bool(value),
            CellValue::Int(value) => Self::Int(value),
            CellValue::Float(value) => Self::Float(value),
            #[cfg(feature = "decimal")]
            CellValue::Decimal(value) => Self::Decimal(value),
            CellValue::Text(text) => Self::Text(text.into()),
            CellValue::Timestamp(value) => Self::Timestamp(value),
            CellValue::Icon(icon) => Self::Text(icon.text().into()),
            CellValue::Image(image) => Self::Text(image.text().into()),
            CellValue::Link(link) => Self::Text(link.text().into()),
            CellValue::Custom(value) => value.to_owned_value(),
        }
    }

    /// Borrows the value back, to show it as a cell does or to compare it.
    pub fn as_cell(&self) -> CellValue<'_> {
        match self {
            Self::Empty => CellValue::Empty,
            Self::Bool(value) => CellValue::Bool(*value),
            Self::Int(value) => CellValue::Int(*value),
            Self::Float(value) => CellValue::Float(*value),
            #[cfg(feature = "decimal")]
            Self::Decimal(value) => CellValue::Decimal(*value),
            Self::Text(text) => CellValue::Text(text),
            Self::Timestamp(value) => CellValue::Timestamp(*value),
            Self::Custom(value) => CellValue::Custom(&**value),
        }
    }

    /// The order of [`CellValue::cmp_total`].
    pub fn cmp_total(&self, other: &Self) -> Ordering {
        self.as_cell().cmp_total(&other.as_cell())
    }
}

impl PartialEq for OwnedValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_total(other).is_eq()
    }
}

impl Eq for OwnedValue {}

impl Hash for OwnedValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Empty => state.write_u8(0),
            Self::Bool(value) => {
                state.write_u8(1);
                value.hash(state);
            }
            Self::Int(value) => {
                state.write_u8(2);
                value.hash(state);
            }
            // A float equal to an integer hashes as that integer.
            Self::Float(value) => match integral(*value) {
                Some(int) => {
                    state.write_u8(2);
                    int.hash(state);
                }
                // `cmp_total` calls two floats equal only bit for bit.
                None => {
                    state.write_u8(3);
                    value.to_bits().hash(state);
                }
            },
            // A decimal equal to an integer or a float hashes as that number.
            #[cfg(feature = "decimal")]
            Self::Decimal(value) => match decimal::equal_int_or_float(*value) {
                Some(number) => number.hash(state),
                None => {
                    // Without trailing zeros, so `1.50` and `1.5` hash alike.
                    let normal = value.normalize();
                    state.write_u8(5);
                    normal.mantissa().hash(state);
                    normal.scale().hash(state);
                }
            },
            Self::Text(text) => {
                state.write_u8(4);
                text.hash(state);
            }
            Self::Timestamp(value) => {
                state.write_u8(6);
                value.hash(state);
            }
            // Equal custom values are of one type: `cmp_value` orders two
            // types by type.
            Self::Custom(value) => {
                state.write_u8(7);
                value.as_any().type_id().hash(state);
            }
        }
    }
}

/// The integer `cmp_total` calls equal to `float`, if there is one: an
/// integral value in the `i64` range, but not -0.0 (it sorts below 0).
fn integral(float: f64) -> Option<i64> {
    let in_range = (-9.223_372_036_854_775_808e18..9.223_372_036_854_775_808e18).contains(&float);
    (in_range && float.fract() == 0.0 && !(float == 0.0 && float.is_sign_negative()))
        .then_some(float as i64)
}

impl Serialize for OwnedValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Empty => serializer.serialize_none(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Float(value) if value.is_finite() => serializer.serialize_f64(*value),
            Self::Float(value) => Err(ser::Error::custom(format_args!(
                "{value} is not a finite number"
            ))),
            #[cfg(feature = "decimal")]
            Self::Decimal(value) => {
                use ser::SerializeMap as _;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("decimal", &decimal::DecimalText(*value))?;
                map.end()
            }
            Self::Text(text) => serializer.serialize_str(text),
            Self::Timestamp(value) => {
                use ser::SerializeMap as _;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("timestamp", &timestamp::TimestampText(*value))?;
                map.end()
            }
            Self::Custom(value) => match value.export_value() {
                Some(export) if !matches!(export, CellValue::Custom(_)) => {
                    Self::from_cell(export).serialize(serializer)
                }
                _ => {
                    let mut text = String::new();
                    value.write_text(&mut text);
                    serializer.serialize_str(&text)
                }
            },
        }
    }
}

/// The key of a value serialized as a one-entry map: `{"timestamp": …}`,
/// `{"decimal": …}`.
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "lowercase")]
enum TaggedValue {
    Timestamp,
    #[cfg(feature = "decimal")]
    Decimal,
}

impl<'de> Deserialize<'de> for OwnedValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;

        impl<'v> de::Visitor<'v> for ValueVisitor {
            type Value = OwnedValue;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(if cfg!(feature = "decimal") {
                    r#"null, a boolean, a number, a string, {"timestamp": text} or {"decimal": text}"#
                } else {
                    r#"null, a boolean, a number, a string or {"timestamp": text}"#
                })
            }

            fn visit_unit<E: de::Error>(self) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Empty)
            }

            fn visit_none<E: de::Error>(self) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Empty)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Int(value))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<OwnedValue, E> {
                i64::try_from(value)
                    .map(OwnedValue::Int)
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Float(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<OwnedValue, E> {
                Ok(OwnedValue::Text(value.into()))
            }

            fn visit_map<A: de::MapAccess<'v>>(self, mut map: A) -> Result<OwnedValue, A::Error> {
                let Some(tag) = map.next_key::<TaggedValue>()? else {
                    return Err(de::Error::invalid_length(0, &self));
                };
                let text: String = map.next_value()?;
                let value = match tag {
                    TaggedValue::Timestamp => {
                        OwnedValue::Timestamp(timestamp::read_timestamp(&text)?)
                    }
                    #[cfg(feature = "decimal")]
                    TaggedValue::Decimal => OwnedValue::Decimal(decimal::read_decimal(&text)?),
                };
                if map.next_key::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(2, &self));
                }
                Ok(value)
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

/// The text of a cell as the body paints it: `Text` and the text of an
/// `Icon` borrowed without a copy, `Int` as `{}`, `Float` as `{:.2}`,
/// `Decimal` as `{}` with its scale (`12.30`), `Timestamp` in UTC as
/// `2026-11-04 12:34:56`, `Custom` as its [`GridValue::write_text`],
/// `Bool(true)` as "✓", `Bool(false)` and `Empty` as no text.
///
/// Numbers, timestamps and custom values are written into `buffer`, which
/// the caller reuses across cells, so a frame does not allocate per cell.
/// Inlined: the generic `plan_frame` is instantiated in the application
/// crate and calls this for every cell.
#[inline]
pub(crate) fn display_text<'a>(value: CellValue<'a>, buffer: &'a mut String) -> &'a str {
    display_text_in(value, NumberFormat::new(), buffer)
}

/// How a column shows its numbers (`GridColumn::with_number_format`): the
/// decimals of a `Float`. The default shows two, as the grid always did.
///
/// A copy, a CSV file and an export's number hint follow the column's
/// format too, so they give what the grid shows (`RawCells`). An exact
/// `Decimal` keeps its own scale, an `Int` has no decimals, and a
/// `Custom` value writes its own text.
///
/// ```
/// use gpui_kit_grid::NumberFormat;
///
/// // Knots to a tenth.
/// let knots = NumberFormat::new().with_decimals(1);
/// assert_eq!(knots.decimals(), 1);
/// assert_eq!(NumberFormat::default().decimals(), 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NumberFormat {
    decimals: u8,
}

impl NumberFormat {
    /// The most decimals a format shows.
    pub const MAX_DECIMALS: u8 = 15;

    /// The default: two decimals.
    pub const fn new() -> Self {
        Self { decimals: 2 }
    }

    /// Shows `decimals` digits after the point, rounded half to even as
    /// `format!("{:.n}")` does; at most [`Self::MAX_DECIMALS`].
    pub const fn with_decimals(mut self, decimals: u8) -> Self {
        self.decimals = if decimals > Self::MAX_DECIMALS {
            Self::MAX_DECIMALS
        } else {
            decimals
        };
        self
    }

    /// The digits shown after the point of a `Float`.
    pub const fn decimals(&self) -> u8 {
        self.decimals
    }
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self::new()
    }
}

/// [`display_text`] with a column's [`NumberFormat`]: the text the body
/// paints in that column.
#[inline]
pub(crate) fn display_text_in<'a>(
    value: CellValue<'a>,
    format: NumberFormat,
    buffer: &'a mut String,
) -> &'a str {
    match value {
        CellValue::Text(text) => text,
        CellValue::Empty | CellValue::Bool(false) => "",
        CellValue::Bool(true) => "✓",
        CellValue::Int(value) => {
            buffer.clear();
            let _ = write!(buffer, "{value}");
            buffer
        }
        CellValue::Float(value) => {
            buffer.clear();
            float_text::push_fixed(value, format.decimals, buffer);
            buffer
        }
        #[cfg(feature = "decimal")]
        CellValue::Decimal(value) => {
            buffer.clear();
            decimal::push_decimal(value, buffer);
            buffer
        }
        CellValue::Timestamp(value) => {
            buffer.clear();
            push_timestamp(value, buffer);
            buffer
        }
        CellValue::Icon(icon) => icon.text(),
        CellValue::Image(image) => image.text(),
        CellValue::Link(link) => link.text(),
        CellValue::Custom(value) => {
            buffer.clear();
            value.write_text(buffer);
            buffer
        }
    }
}

/// The digits [`display_text_in`] writes after the decimal point of a number
/// in a column of `format`: `Int` 0, `Float` the format's decimals, `Decimal`
/// its scale. A `Custom` value counts by its
/// [`GridValue::export_value`] when that is one of these. `None` for any
/// other value.
///
/// One rule for the body and an export: an export format reads it as
/// `ExportCell::number_hint`, so a spreadsheet shows a number as the grid
/// does. No allocation: called per exported cell.
#[inline]
pub(crate) fn number_fraction_digits(value: CellValue<'_>, format: NumberFormat) -> Option<u32> {
    let value = match value {
        CellValue::Custom(value) => value.export_value()?,
        value => value,
    };
    match value {
        CellValue::Int(_) => Some(0),
        CellValue::Float(_) => Some(u32::from(format.decimals)),
        #[cfg(feature = "decimal")]
        CellValue::Decimal(value) => Some(value.scale()),
        _ => None,
    }
}

/// Returns true if `text` may occur, ignoring ASCII case, in the display
/// text of a number (`Int`, `Float`, `Decimal`). [`display_text`] writes
/// numbers with ASCII digits, `-`, `.`, `NaN` and `inf` alone, so a quick
/// filter term with any other character skips formatting numbers.
pub(crate) fn may_occur_in_number_text(text: &str) -> bool {
    text.bytes().all(|byte| {
        matches!(
            byte.to_ascii_lowercase(),
            b'0'..=b'9' | b'-' | b'.' | b'n' | b'a' | b'i' | b'f'
        )
    })
}

/// Compares an integer with a float exactly, consistent with
/// `f64::total_cmp` between floats. Converting the integer to `f64` loses
/// precision above 2^53 and breaks transitivity, which sorting relies on.
fn cmp_int_float(int: i64, float: f64) -> Ordering {
    if float.is_nan() {
        // Positive NaN sorts after every number under `total_cmp`.
        return if float.is_sign_negative() {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    if float >= 9.223_372_036_854_775_807e18 {
        return Ordering::Less;
    }
    if float < -9.223_372_036_854_775_808e18 {
        return Ordering::Greater;
    }
    let whole = float.trunc();
    // The fraction decides between an integer and a float of the same whole
    // part. A zero keeps its sign: -0.0 - -0.0 is 0.0, but `total_cmp` puts
    // -0.0 below 0.0, so `Int(0)` is above -0.0 and equal to 0.0 only.
    let fraction = if float == 0.0 { float } else { float - whole };
    i128::from(int)
        .cmp(&(whole as i128))
        .then_with(|| 0.0f64.total_cmp(&fraction))
}

/// A row of the grid. Rows are immutable snapshots shared through `Arc`.
pub trait GridRow: Send + Sync + 'static {
    /// The domain key of the row.
    type Key: Clone + Eq + Hash + Debug + Send + Sync + 'static;
    /// The columns of the row type.
    type Column: GridColumns;

    /// The key of this row.
    fn key(&self) -> &Self::Key;

    /// The value of a column. Called for visible cells on every frame and
    /// for every row when sorting: it must not allocate or lock.
    fn value(&self, column: Self::Column) -> CellValue<'_>;

    /// Adds to `out` the columns whose values differ from `previous`.
    ///
    /// The default compares every column with [`GridRow::value`]; override
    /// it when the producer knows cheaper what changed. Called by ingest,
    /// and by a writer whose upsert takes the place of a pending upsert of
    /// the same key ([`WriteCoalescing::ByKey`](crate::WriteCoalescing::ByKey)),
    /// under the store's write lock: it must not lock.
    fn changed_columns(&self, previous: &Self, out: &mut ColumnSet) {
        for &column in Self::Column::all() {
            if self.value(column) != previous.value(column) {
                out.insert(column.ix());
            }
        }
    }

    /// The parent row's key in a tree view; `None` for a root. Called in
    /// the background while a tree is built, and by ingest (or a writer
    /// whose upsert merges into a pending one) on every replacement of the
    /// row, to compare it with the previous version
    /// ([`ChangeSet::has_parent_change`](crate::ChangeSet::has_parent_change)):
    /// it must not allocate or lock.
    fn parent_key(&self) -> Option<&Self::Key> {
        None
    }

    /// The direction of `column` after a change from `previous`.
    ///
    /// The default is `Up` or `Down` when both values are numbers, by
    /// [`CellValue::cmp_total`], and `None` otherwise or when they are
    /// equal. Override it for a column whose rise is bad news, such as a
    /// spread. Called for changed columns only, by ingest or by a writer
    /// whose upsert merges into a pending one: it must not allocate or lock.
    fn change_direction(&self, previous: &Self, column: Self::Column) -> Option<ChangeDirection> {
        let (value, before) = (self.value(column), previous.value(column));
        if !(value.is_number() && before.is_number()) {
            return None;
        }
        match value.cmp_total(&before) {
            Ordering::Greater => Some(ChangeDirection::Up),
            Ordering::Less => Some(ChangeDirection::Down),
            Ordering::Equal => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_value_order_is_exact_across_int_and_float() {
        let big = 1i64 << 53;
        let values = [
            CellValue::Int(big + 1),
            CellValue::Float(big as f64),
            CellValue::Int(big),
            CellValue::Float(f64::NAN),
            CellValue::Float(-0.5),
            CellValue::Int(-1),
            CellValue::Empty,
            CellValue::Text("a"),
        ];
        // Above 2^53 an i64 → f64 cast would call `big + 1` equal to the float.
        assert_eq!(values[0].cmp_total(&values[1]), Ordering::Greater);
        assert_eq!(values[2].cmp_total(&values[1]), Ordering::Equal);
        assert_eq!(values[1].cmp_total(&values[0]), Ordering::Less);
        assert_eq!(values[3].cmp_total(&values[0]), Ordering::Greater);
        assert_eq!(values[4].cmp_total(&values[5]), Ordering::Greater);
        // The order is a total order: sorting must not panic and must be stable
        // under every permutation of the inputs.
        let mut sorted = values;
        sorted.sort_by(|a, b| a.cmp_total(b));
        for window in sorted.windows(2) {
            assert_ne!(window[0].cmp_total(&window[1]), Ordering::Greater);
        }
    }

    /// Every kind of value in ascending order, written out by hand: each rung
    /// holds values `cmp_total` calls equal. The numbers mix `Int` and
    /// `Float` at the edges where an exact comparison is easy to get wrong:
    /// zeros, 2^53 (above it `f64` skips integers), 2^63 (the end of `i64`),
    /// infinities and NaNs of both signs.
    fn ladder() -> Vec<Vec<CellValue<'static>>> {
        use CellValue::{Bool, Empty, Float, Int, Text};
        let p53 = 1i64 << 53;
        let p63 = 9_223_372_036_854_775_808.0f64;
        vec![
            vec![Empty],
            vec![Bool(false)],
            vec![Bool(true)],
            // `total_cmp` orders negative NaNs by payload, the largest first.
            vec![Float(f64::from_bits(0xFFF8_0000_0000_0000))],
            vec![Float(f64::from_bits(0xFFF0_0000_0000_0001))],
            vec![Float(f64::NEG_INFINITY)],
            vec![Float(f64::MIN)],
            vec![Float(-p63 - 2048.0)],
            vec![Int(i64::MIN), Float(-p63)],
            vec![Int(i64::MIN + 1)],
            vec![Int(-p53 - 1)],
            vec![Int(-p53), Float(-p53 as f64)],
            vec![Float(-1.5)],
            vec![Int(-1), Float(-1.0)],
            vec![Float(-0.5)],
            vec![Float(-f64::MIN_POSITIVE)],
            vec![Float(-f64::from_bits(1))],
            vec![Float(-0.0)],
            vec![Int(0), Float(0.0)],
            vec![Float(f64::from_bits(1))],
            vec![Float(0.5)],
            vec![Int(1), Float(1.0)],
            vec![Float(1.5)],
            vec![Int(p53 - 1), Float((p53 - 1) as f64)],
            vec![Int(p53), Float(p53 as f64)],
            vec![Int(p53 + 1)],
            vec![Int(p53 + 2), Float((p53 + 2) as f64)],
            vec![Int(i64::MAX - 1023), Float(p63 - 1024.0)],
            vec![Int(i64::MAX - 1)],
            vec![Int(i64::MAX)],
            vec![Float(p63)],
            vec![Float(f64::MAX)],
            vec![Float(f64::INFINITY)],
            vec![Float(f64::from_bits(0x7FF0_0000_0000_0001))],
            vec![Float(f64::from_bits(0x7FF8_0000_0000_0000))],
            vec![Text("")],
            vec![Text("a")],
            vec![Text("b")],
        ]
    }

    /// Checks one triple against the laws of a total order: reflexive,
    /// antisymmetric and transitive, with equality transitive too.
    fn check_total_order(a: &CellValue, b: &CellValue, c: &CellValue) {
        let (ab, bc, ac) = (a.cmp_total(b), b.cmp_total(c), a.cmp_total(c));
        assert_eq!(
            a.cmp_total(a),
            Ordering::Equal,
            "{a:?} is not equal to itself"
        );
        assert_eq!(b.cmp_total(a), ab.reverse(), "{a:?} and {b:?}");
        let expected = match (ab, bc) {
            (Ordering::Equal, _) => bc,
            (_, Ordering::Equal) => ab,
            _ if ab == bc => ab,
            // a < b > c or a > b < c: any order of a and c is consistent.
            _ => return,
        };
        assert_eq!(ac, expected, "{a:?}, {b:?}, {c:?}: {ab:?} then {bc:?}");
    }

    #[test]
    fn test_cell_value_order_matches_ladder() {
        let ladder = ladder();
        for (rung, values) in ladder.iter().enumerate() {
            for (other_rung, others) in ladder.iter().enumerate() {
                for a in values {
                    for b in others {
                        assert_eq!(a.cmp_total(b), rung.cmp(&other_rung), "{a:?} against {b:?}");
                    }
                }
            }
        }

        // Signed zeros: -0.0 sorts just below 0, as under `total_cmp`, so
        // `Int(0)` cannot equal both zeros.
        let zero = CellValue::Int(0);
        assert_eq!(zero.cmp_total(&CellValue::Float(-0.0)), Ordering::Greater);
        assert_eq!(CellValue::Float(-0.0).cmp_total(&zero), Ordering::Less);
        assert_eq!(zero.cmp_total(&CellValue::Float(0.0)), Ordering::Equal);
        assert_eq!(
            CellValue::Int(-1).cmp_total(&CellValue::Float(-0.0)),
            Ordering::Less
        );
    }

    #[test]
    fn test_cell_value_order_is_total() {
        let ladder: Vec<CellValue> = ladder().into_iter().flatten().collect();
        // Every triple of the edge values.
        for a in &ladder {
            for b in &ladder {
                for c in &ladder {
                    check_total_order(a, b, c);
                }
            }
        }

        // Random triples: the edge values, their neighbours and random
        // integers, integral and fractional floats, and random bit patterns
        // (NaNs with any payload, subnormals). Small ranges make ties
        // between `Int` and `Float` common.
        let mut rng = fastrand::Rng::with_seed(20);
        let random = |rng: &mut fastrand::Rng| match rng.u8(0..9) {
            0 => ladder[rng.usize(..ladder.len())],
            1 => CellValue::Int(rng.i64(-3..=3)),
            2 => CellValue::Float(f64::from(rng.i8(-6..=6)) / 2.0),
            3 => CellValue::Float([0.0, -0.0][rng.usize(..2)]),
            4 => {
                let edge = [0, 1 << 53, i64::MIN, i64::MAX][rng.usize(..4)];
                CellValue::Int(edge.saturating_add(rng.i64(-3..=3)))
            }
            5 => {
                let edge = [1u64 << 53, 1 << 63, 0][rng.usize(..3)] as f64;
                let bits = edge.to_bits().saturating_add_signed(rng.i64(-2..=2));
                let float = f64::from_bits(bits);
                CellValue::Float(if rng.bool() { float } else { -float })
            }
            6 => CellValue::Int(rng.i64(..)),
            7 => CellValue::Float(rng.i64(..) as f64),
            _ => CellValue::Float(f64::from_bits(rng.u64(..))),
        };
        for _ in 0..200_000 {
            let (a, b, c) = (random(&mut rng), random(&mut rng), random(&mut rng));
            check_total_order(&a, &b, &c);
        }

        // An integer every `f64` holds compares as that float under
        // `total_cmp`, signed zeros and NaNs included.
        for _ in 0..100_000 {
            let int = match rng.u8(0..3) {
                0 => rng.i64(-4..=4),
                1 => rng.i64(-(1 << 53)..=1 << 53),
                _ => [0, 1 << 53, -(1 << 53)][rng.usize(..3)],
            };
            let float = match random(&mut rng) {
                CellValue::Float(float) => float,
                CellValue::Int(other) => other as f64,
                _ => -0.0,
            };
            assert_eq!(
                CellValue::Int(int).cmp_total(&CellValue::Float(float)),
                (int as f64).total_cmp(&float),
                "{int} against {float:?}"
            );
        }
    }

    #[test]
    fn test_column_id() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        enum Column {
            Price,
        }

        impl GridColumns for Column {
            fn all() -> &'static [Self] {
                &[Column::Price]
            }

            fn id(self) -> &'static str {
                "price"
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(0)
            }
        }

        // An id from the schema borrows `GridColumns::id`; one from a file owns
        // its text. They are the same id.
        const PRICE: ColumnId = ColumnId::new_static("price");
        let from_schema = Column::Price.column_id();
        assert!(matches!(from_schema.0, Cow::Borrowed("price")));
        let from_file: ColumnId = serde_json::from_str("\"price\"").unwrap();
        assert!(matches!(from_file.0, Cow::Owned(_)));
        assert_eq!(from_schema, from_file);
        assert_eq!(from_schema, PRICE);
        assert_eq!(serde_json::to_string(&from_schema).unwrap(), "\"price\"");

        // Map keys: borrowed and owned ids hash and order by text, and `&str`
        // looks them up.
        let ids = rustc_hash::FxHashSet::from_iter([from_file.clone()]);
        assert!(ids.contains(&from_schema) && ids.contains("price"));
        let ordered =
            std::collections::BTreeMap::from([(ColumnId::new("b"), 2), (ColumnId::from("a"), 1)]);
        assert_eq!(
            ordered.keys().map(ColumnId::as_str).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(ordered.get("b"), Some(&2));

        assert_eq!(ColumnId::from(String::from("fee")), ColumnId::from("fee"));
        assert_eq!(from_file.to_string(), "price");
        assert_eq!(from_file.as_ref(), "price");
    }

    #[test]
    fn test_display_text_in_a_number_format() {
        let mut buffer = String::new();
        let value = CellValue::Float(-0.489_999_999_999_994_9);
        for (decimals, expected) in [(0, "-0"), (1, "-0.5"), (2, "-0.49"), (4, "-0.4900")] {
            let format = NumberFormat::new().with_decimals(decimals);
            assert_eq!(display_text_in(value, format, &mut buffer), expected);
            assert_eq!(
                number_fraction_digits(value, format),
                Some(u32::from(decimals))
            );
        }
        // Capped; integers and text keep their text.
        let many = NumberFormat::new().with_decimals(40);
        assert_eq!(many.decimals(), NumberFormat::MAX_DECIMALS);
        assert_eq!(display_text_in(CellValue::Int(7), many, &mut buffer), "7");
        assert_eq!(
            display_text_in(CellValue::Text("1.5"), many, &mut buffer),
            "1.5"
        );
        assert_eq!(number_fraction_digits(CellValue::Int(7), many), Some(0));
        assert_eq!(NumberFormat::default(), NumberFormat::new());
    }

    #[test]
    fn test_display_text() {
        let mut buffer = String::with_capacity(32);
        let storage = buffer.as_ptr();
        let cases = [
            (CellValue::Empty, ""),
            (CellValue::Bool(true), "✓"),
            (CellValue::Bool(false), ""),
            (CellValue::Int(0), "0"),
            (CellValue::Int(-42), "-42"),
            (CellValue::Int(i64::MAX), "9223372036854775807"),
            (CellValue::Float(12.5), "12.50"),
            (CellValue::Float(2.0 / 3.0), "0.67"),
            (CellValue::Float(-1234.567), "-1234.57"),
            (CellValue::Float(f64::NAN), "NaN"),
            (CellValue::Text("AAPL"), "AAPL"),
            // A shorter number after a longer one leaves nothing behind.
            (CellValue::Int(7), "7"),
        ];
        for (value, expected) in cases {
            assert_eq!(display_text(value, &mut buffer), expected, "{value:?}");
        }
        // Numbers reuse the caller's buffer: no allocation per cell.
        assert_eq!(buffer.as_ptr(), storage);

        // Text is borrowed from the row, not copied into the buffer.
        let row_text = String::from("Сбербанк");
        let text = display_text(CellValue::Text(&row_text), &mut buffer);
        assert!(std::ptr::eq(text, row_text.as_str()));
    }

    #[test]
    fn test_number_text_alphabet() {
        // The quick filter skips numbers for terms outside this alphabet, so
        // every number must display inside it, the `core::fmt` fallbacks too.
        let mut buffer = String::new();
        let mut check = |value: CellValue| {
            let text = display_text(value, &mut buffer);
            assert!(may_occur_in_number_text(text), "{value:?} shows as {text}");
        };
        for value in [i64::MIN, -1, 0, 7, i64::MAX] {
            check(CellValue::Int(value));
        }
        for value in [
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -0.0,
            1e19,
            -1e300,
        ] {
            check(CellValue::Float(value));
        }
        let mut rng = fastrand::Rng::with_seed(3);
        for _ in 0..10_000 {
            check(CellValue::Int(rng.i64(..)));
            check(CellValue::Float(f64::from_bits(rng.u64(..))));
        }

        for term in ["12.50", "-3", "nan", "INF", "1e", "", "12,5", "✓", "sber"] {
            let expected = matches!(term, "12.50" | "-3" | "nan" | "INF" | "");
            assert_eq!(may_occur_in_number_text(term), expected, "{term}");
        }
    }

    #[test]
    fn test_number_fraction_digits() {
        /// Exports as a float, or as another custom value.
        #[derive(Debug)]
        struct Exported(Option<CellValue<'static>>);

        impl GridValue for Exported {
            fn cmp_value(&self, _: &dyn GridValue) -> Ordering {
                Ordering::Equal
            }

            fn write_text(&self, out: &mut String) {
                out.push_str("3M");
            }

            fn export_value(&self) -> Option<CellValue<'_>> {
                self.0
            }
        }

        static TENOR: Exported = Exported(Some(CellValue::Float(0.25)));
        static NAMED: Exported = Exported(None);
        static ALIAS: Exported = Exported(Some(CellValue::Custom(&TENOR)));

        // The digits after the point of a finite number's display text.
        let mut buffer = String::new();
        let numbers = [
            CellValue::Int(0),
            CellValue::Int(i64::MIN),
            CellValue::Float(12.5),
            CellValue::Float(-1234.567),
            CellValue::Float(2.0 / 3.0),
            CellValue::Float(1e300),
        ]
        .into_iter();
        #[cfg(feature = "decimal")]
        let numbers = numbers.chain(
            [(1230, 2), (5, 0), (-5, 2), (0, 3), (1, 28)]
                .map(|(mantissa, scale)| CellValue::Decimal(Decimal::new(mantissa, scale))),
        );
        for value in numbers {
            let text = display_text(value, &mut buffer);
            let shown = text
                .split_once('.')
                .map_or(0, |(_, fraction)| fraction.len());
            assert_eq!(
                number_fraction_digits(value, NumberFormat::new()),
                Some(shown as u32),
                "{value:?} shows as {text}"
            );
        }
        // A float that is not finite still asks for two decimals.
        assert_eq!(
            number_fraction_digits(CellValue::Float(f64::NAN), NumberFormat::new()),
            Some(2)
        );

        // A custom value by its export value, when that is a number.
        assert_eq!(
            number_fraction_digits(CellValue::Custom(&TENOR), NumberFormat::new()),
            Some(2)
        );
        for value in [
            CellValue::Empty,
            CellValue::Bool(true),
            CellValue::Text("1.5"),
            CellValue::Timestamp(DateTime::<Utc>::UNIX_EPOCH),
            CellValue::Icon(IconValue::new("icons/check.svg", "1.5")),
            CellValue::Image(ImageValue::new("images/one.png", "1.5")),
            CellValue::Custom(&NAMED),
            CellValue::Custom(&ALIAS),
        ] {
            assert_eq!(
                number_fraction_digits(value, NumberFormat::new()),
                None,
                "{value:?}"
            );
        }
    }

    fn hash_of(value: &OwnedValue) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn test_owned_value() {
        // Every kind of value comes back as it was borrowed, bit for bit.
        let ladder: Vec<CellValue> = ladder().into_iter().flatten().collect();
        for value in &ladder {
            let owned = OwnedValue::from_cell(*value);
            let back = owned.as_cell();
            match (value, back) {
                (CellValue::Float(a), CellValue::Float(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                _ => assert_eq!(*value, back),
            }
            assert_eq!(owned, owned.clone(), "{value:?} equals itself");
        }

        // Equal exactly when `cmp_total` says so, and equal values hash
        // alike: on the edge values and on random pairs that tie often.
        let check = |a: &OwnedValue, b: &OwnedValue| {
            let equal = a.cmp_total(b).is_eq();
            assert_eq!(a == b, equal, "{a:?} and {b:?}");
            if equal {
                assert_eq!(hash_of(a), hash_of(b), "{a:?} and {b:?} hash apart");
            }
        };
        let owned: Vec<OwnedValue> = ladder.iter().map(|v| OwnedValue::from_cell(*v)).collect();
        for a in &owned {
            for b in &owned {
                check(a, b);
            }
        }
        let mut rng = fastrand::Rng::with_seed(26);
        let random = |rng: &mut fastrand::Rng| match rng.u8(0..6) {
            0 => owned[rng.usize(..owned.len())].clone(),
            1 => OwnedValue::Int(rng.i64(-3..=3)),
            2 => OwnedValue::Float(f64::from(rng.i8(-6..=6)) / 2.0),
            3 => OwnedValue::Float([0.0, -0.0, f64::NAN, -f64::NAN][rng.usize(..4)]),
            4 => OwnedValue::Text(["", "a", "b"][rng.usize(..3)].into()),
            _ => [OwnedValue::Empty, OwnedValue::Bool(rng.bool())][rng.usize(..2)].clone(),
        };
        for _ in 0..10_000 {
            let (a, b) = (random(&mut rng), random(&mut rng));
            check(&a, &b);
        }

        // One group key for 1 and 1.0; two for the zeros; a NaN is itself.
        use OwnedValue::{Float, Int};
        assert_eq!(Int(1), Float(1.0));
        assert_eq!(hash_of(&Int(1)), hash_of(&Float(1.0)));
        assert_eq!(Int(i64::MIN), Float(-9_223_372_036_854_775_808.0));
        assert_ne!(Float(-0.0), Float(0.0));
        assert_eq!(Int(0), Float(0.0));
        assert_ne!(Int(0), Float(-0.0));
        assert_eq!(Float(f64::NAN), Float(f64::NAN));
        assert_ne!(Float(f64::NAN), Float(-f64::NAN));
        assert_ne!(Int(1), Float(1.5));
        // Transitive on the zeros.
        let zeros = [Int(0), Float(-0.0), Float(0.0)];
        for a in &zeros {
            for b in &zeros {
                for c in &zeros {
                    if a == b && b == c {
                        assert_eq!(a, c, "{a:?}, {b:?}, {c:?}");
                    }
                }
            }
        }
        let set = rustc_hash::FxHashSet::from_iter(zeros);
        assert_eq!(set.len(), 2, "-0.0 and 0 are two keys");
    }

    #[test]
    fn test_owned_value_serde() {
        use OwnedValue::{Bool, Empty, Float, Int, Text};
        for (value, json) in [
            (Empty, "null"),
            (Bool(true), "true"),
            (Int(-12), "-12"),
            (Int(i64::MAX), "9223372036854775807"),
            (Float(1.0), "1.0"),
            (Float(-0.0), "-0.0"),
            (Float(12.5), "12.5"),
            (Float(1e20), "1e+20"),
            (Text("AAPL".into()), r#""AAPL""#),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            let back: OwnedValue = serde_json::from_str(json).unwrap();
            assert_eq!(back, value, "{json}");
            // An integral float reads back as a float, an integer as one.
            assert_eq!(
                std::mem::discriminant(&back),
                std::mem::discriminant(&value),
                "{json}"
            );
        }
        assert!(matches!(serde_json::from_str("1"), Ok(Int(1))));
        assert!(matches!(serde_json::from_str("1.0"), Ok(Float(1.0))));
        // Neither a NaN nor an infinity is written; nor is an integer read
        // past `i64`, nor a list.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(serde_json::to_string(&Float(value)).is_err(), "{value}");
        }
        assert!(serde_json::from_str::<OwnedValue>("9223372036854775808").is_err());
        assert!(serde_json::from_str::<OwnedValue>("[1]").is_err());
    }

    #[test]
    fn test_change_direction_default() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct Value;

        impl GridColumns for Value {
            fn all() -> &'static [Self] {
                &[Self]
            }

            fn id(self) -> &'static str {
                "value"
            }

            fn ix(self) -> ColumnIx {
                ColumnIx::new(0)
            }
        }

        struct Row(CellValue<'static>);

        impl GridRow for Row {
            type Key = u8;
            type Column = Value;

            fn key(&self) -> &u8 {
                &0
            }

            fn value(&self, _: Value) -> CellValue<'_> {
                self.0
            }
        }

        use CellValue::{Bool, Empty, Float, Int, Text};
        use ChangeDirection::{Down, Up};
        for (before, after, expected) in [
            (Int(1), Int(2), Some(Up)),
            (Int(2), Int(1), Some(Down)),
            (Float(1.5), Float(0.5), Some(Down)),
            (Float(0.5), Float(1.5), Some(Up)),
            (Int(1), Float(1.5), Some(Up)),
            (Float(1.5), Int(1), Some(Down)),
            (Int(1), Float(1.0), None),
            (Int(3), Int(3), None),
            (Text("a"), Text("b"), None),
            (Bool(false), Bool(true), None),
            (Empty, Int(1), None),
            (Int(1), Text("2"), None),
        ] {
            assert_eq!(
                Row(after).change_direction(&Row(before), Value),
                expected,
                "{before:?} → {after:?}"
            );
        }
        assert_eq!(Row(Int(1)).parent_key(), None);
    }

    /// `Decimal` values, with the `decimal` feature.
    #[cfg(feature = "decimal")]
    mod decimal_values {
        use super::*;

        /// A decimal from its mantissa and scale.
        fn dec(mantissa: i128, scale: u32) -> Decimal {
            Decimal::from_i128_with_scale(mantissa, scale)
        }

        /// A zero with its sign bit set, as `-Decimal::ZERO` gives.
        fn negative_zero(scale: u32) -> Decimal {
            let mut zero = dec(0, scale);
            zero.set_sign_negative(true);
            zero
        }

        /// Numbers of the three kinds in ascending order, written out by hand:
        /// each rung holds values `cmp_total` calls equal. Decimals of several
        /// scales sit with the integers and floats of their value, and between
        /// the floats where no float equals them: around zero, 10^-28 and 0.1
        /// (floats a little off their decimals), 1, 2^53, 2^63 and the ends of
        /// the decimal range, ±(2^96 - 1).
        fn decimal_ladder() -> Vec<Vec<CellValue<'static>>> {
            use CellValue::{Bool, Decimal, Empty, Float, Int, Text};
            let p53 = 1i64 << 53;
            let p63 = 9_223_372_036_854_775_808.0f64;
            let p96 = 79_228_162_514_264_337_593_543_950_336.0f64;
            let below_p96 = p96.next_down();
            let below_p96_int = (1i128 << 96) - (1 << 43);
            let max = rust_decimal::Decimal::MAX;
            vec![
                vec![Empty],
                vec![Bool(true)],
                vec![Float(f64::from_bits(0xFFF8_0000_0000_0000))],
                vec![Float(f64::NEG_INFINITY)],
                vec![Float(f64::MIN)],
                vec![Float(-p96)],
                vec![Decimal(rust_decimal::Decimal::MIN)],
                vec![Float(-below_p96), Decimal(dec(-below_p96_int, 0))],
                vec![Int(i64::MIN), Float(-p63), Decimal(dec(i64::MIN.into(), 0))],
                vec![Decimal(dec(i128::from(i64::MIN) * 10 + 5, 1))],
                vec![Int(i64::MIN + 1), Decimal(dec((i64::MIN + 1).into(), 0))],
                vec![Int(-p53 - 1), Decimal(dec(i128::from(-p53 - 1) * 100, 2))],
                vec![
                    Int(-p53),
                    Float(-p53 as f64),
                    Decimal(dec((-p53).into(), 0)),
                ],
                vec![Float(-1.5), Decimal(dec(-150, 2))],
                vec![
                    Int(-1),
                    Float(-1.0),
                    Decimal(dec(-1, 0)),
                    Decimal(dec(-1000, 3)),
                ],
                vec![Float(-0.5), Decimal(dec(-5, 1)), Decimal(dec(-50, 2))],
                vec![Float(-0.1)],
                vec![Decimal(dec(-1, 1))],
                vec![Decimal(dec(-1, 28))],
                // The float nearest 10^-28 is a little below it.
                vec![Float(-1e-28)],
                vec![Float(-f64::MIN_POSITIVE)],
                vec![Float(-f64::from_bits(1))],
                vec![Float(-0.0)],
                vec![
                    Int(0),
                    Float(0.0),
                    Decimal(dec(0, 0)),
                    Decimal(dec(0, 2)),
                    Decimal(-rust_decimal::Decimal::ZERO),
                    Decimal(negative_zero(28)),
                ],
                vec![Float(f64::from_bits(1))],
                vec![Float(f64::MIN_POSITIVE)],
                vec![Float(1e-28)],
                vec![Decimal(dec(1, 28))],
                vec![Decimal(dec(1, 1)), Decimal(dec(10, 2))],
                vec![Float(0.1)],
                vec![Float(0.5), Decimal(dec(5, 1)), Decimal(dec(5000, 4))],
                vec![Float(1f64.next_down())],
                vec![Decimal(dec(9_999_999_999_999_999_999_999_999_999, 28))],
                vec![Int(1), Float(1.0), Decimal(dec(1, 0)), Decimal(dec(100, 2))],
                vec![Decimal(dec(10_000_000_000_000_000_000_000_000_001, 28))],
                vec![Float(1f64.next_up())],
                vec![Float(1.5), Decimal(dec(15, 1))],
                vec![Decimal(dec(max.mantissa(), 28))],
                vec![Decimal(dec(123, 1)), Decimal(dec(1230, 2))],
                vec![
                    Int(p53 - 1),
                    Float((p53 - 1) as f64),
                    Decimal(dec((p53 - 1).into(), 0)),
                ],
                vec![
                    Int(p53),
                    Float(p53 as f64),
                    Decimal(dec(i128::from(p53) * 10, 1)),
                ],
                vec![Decimal(dec(i128::from(p53) * 10 + 5, 1))],
                vec![Int(p53 + 1), Decimal(dec((p53 + 1).into(), 0))],
                vec![
                    Int(p53 + 2),
                    Float((p53 + 2) as f64),
                    Decimal(dec((p53 + 2).into(), 0)),
                ],
                vec![Int(i64::MAX), Decimal(dec(i64::MAX.into(), 0))],
                vec![Decimal(dec(i128::from(i64::MAX) * 10 + 5, 1))],
                vec![
                    Float(p63),
                    Decimal(dec(1 << 63, 0)),
                    Decimal(dec(10 << 63, 1)),
                ],
                vec![Decimal(dec((1 << 63) + 1, 0))],
                vec![Float(below_p96), Decimal(dec(below_p96_int, 0))],
                vec![Decimal(max)],
                vec![Float(p96)],
                vec![Float(f64::MAX)],
                vec![Float(f64::INFINITY)],
                vec![Float(f64::from_bits(0x7FF8_0000_0000_0000))],
                vec![Text("0")],
            ]
        }

        /// A random decimal: small values that tie with the integers and the
        /// halves `random_number` makes, zeros of any sign and scale, the
        /// decimal nearest a random float (often equal to it), and any mantissa
        /// below 2^96 with any scale.
        fn random_decimal(rng: &mut fastrand::Rng) -> Decimal {
            match rng.u8(0..4) {
                0 => dec(rng.i64(-30..=30).into(), rng.u32(0..=2)),
                1 => negative_zero(rng.u32(0..=28)),
                2 => Decimal::from_f64_retain(f64::from(rng.i32(..)) / 1024.0).unwrap_or_default(),
                _ => {
                    let bits = rng.u32(1..=96);
                    let magnitude = (rng.u128(..) >> (128 - bits)) as i128;
                    dec(
                        if rng.bool() { -magnitude } else { magnitude },
                        rng.u32(0..=28),
                    )
                }
            }
        }

        #[test]
        fn test_decimal_display_and_order() {
            // The scale shows; numbers go into the caller's buffer.
            let mut buffer = String::with_capacity(64);
            let storage = buffer.as_ptr();
            for (value, expected) in [
                (dec(1230, 2), "12.30"),
                (dec(-5, 2), "-0.05"),
                (dec(0, 2), "0.00"),
                (dec(7, 0), "7"),
                (Decimal::MAX, "79228162514264337593543950335"),
                (dec(1, 28), "0.0000000000000000000000000001"),
                // A shorter number after a longer one leaves nothing behind.
                (dec(-1, 0), "-1"),
            ] {
                let text = display_text(CellValue::Decimal(value), &mut buffer);
                assert_eq!(text, expected, "{value:?}");
                assert!(may_occur_in_number_text(text), "{text}");
            }
            assert_eq!(buffer.as_ptr(), storage);
            assert!(CellValue::Decimal(dec(1, 0)).is_number());
            // `==` compares decimals by value: a change of scale alone is no
            // change for `GridRow::changed_columns`.
            assert_eq!(
                CellValue::Decimal(dec(10, 1)),
                CellValue::Decimal(dec(100, 2))
            );
            assert_ne!(CellValue::Decimal(dec(1, 0)), CellValue::Int(1));

            // The hand-written order, and every triple of it.
            let ladder = decimal_ladder();
            for (rung, values) in ladder.iter().enumerate() {
                for (other_rung, others) in ladder.iter().enumerate() {
                    for a in values {
                        for b in others {
                            assert_eq!(
                                a.cmp_total(b),
                                rung.cmp(&other_rung),
                                "{a:?} against {b:?}"
                            );
                        }
                    }
                }
            }
            let values: Vec<CellValue> = ladder.into_iter().flatten().collect();
            for a in &values {
                for b in &values {
                    for c in &values {
                        check_total_order(a, b, c);
                    }
                }
            }

            // Random triples that tie often across the three kinds, zeros of
            // every kind included.
            let mut rng = fastrand::Rng::with_seed(0xdec1);
            let random = |rng: &mut fastrand::Rng| match rng.u8(0..8) {
                0 => values[rng.usize(..values.len())],
                1 => CellValue::Int(rng.i64(-3..=3)),
                2 => CellValue::Float(f64::from(rng.i8(-6..=6)) / 2.0),
                3 => CellValue::Float([0.0, -0.0][rng.usize(..2)]),
                4 => CellValue::Float(f64::from(rng.i32(..)) / 1024.0),
                5 => CellValue::Float(f64::from_bits(rng.u64(..))),
                _ => CellValue::Decimal(random_decimal(rng)),
            };
            for _ in 0..200_000 {
                let (a, b, c) = (random(&mut rng), random(&mut rng), random(&mut rng));
                check_total_order(&a, &b, &c);
            }

            // A float with more decimals than a `Decimal` holds, its neighbours
            // and the decimal nearest it: rounding the float to compare would
            // call the decimal equal to two different floats.
            for _ in 0..20_000 {
                let float = f64::from(rng.u32(..)) * 10f64.powi(-rng.i32(10..=40));
                let float = if rng.bool() { float } else { -float };
                let Some(near) = Decimal::from_f64_retain(float) else {
                    continue;
                };
                let near = CellValue::Decimal(near);
                for other in [float.next_down(), float.next_up()] {
                    let (float, other) = (CellValue::Float(float), CellValue::Float(other));
                    for (a, b, c) in [
                        (&near, &float, &other),
                        (&float, &near, &other),
                        (&float, &other, &near),
                    ] {
                        check_total_order(a, b, c);
                    }
                }
            }

            // Sorting a random mix does not panic, and neighbours are in order.
            let mut mix: Vec<CellValue> = (0..20_000).map(|_| random(&mut rng)).collect();
            mix.sort_by(|a, b| a.cmp_total(b));
            for pair in mix.windows(2) {
                assert_ne!(pair[0].cmp_total(&pair[1]), Ordering::Greater, "{pair:?}");
            }
        }

        #[test]
        fn test_decimal_owned_value() {
            use OwnedValue::{Decimal as Dec, Float, Int};
            let check = |a: &OwnedValue, b: &OwnedValue| {
                let equal = a.cmp_total(b).is_eq();
                assert_eq!(a == b, equal, "{a:?} and {b:?}");
                if equal {
                    assert_eq!(hash_of(a), hash_of(b), "{a:?} and {b:?} hash apart");
                }
            };
            let owned: Vec<OwnedValue> = decimal_ladder()
                .into_iter()
                .flatten()
                .map(OwnedValue::from_cell)
                .collect();
            for a in &owned {
                assert!(
                    matches!((a, a.as_cell()), (Dec(a), CellValue::Decimal(b)) if a.scale() == b.scale())
                        || !matches!(a, Dec(_)),
                    "{a:?} keeps its scale"
                );
                for b in &owned {
                    check(a, b);
                }
            }
            let mut rng = fastrand::Rng::with_seed(0xdec2);
            let random = |rng: &mut fastrand::Rng| match rng.u8(0..5) {
                0 => owned[rng.usize(..owned.len())].clone(),
                1 => Int(rng.i64(-30..=30)),
                2 => Float(f64::from(rng.i32(..)) / 1024.0),
                3 => Float(f64::from(rng.i8(-6..=6)) / 2.0),
                _ => Dec(random_decimal(rng)),
            };
            for _ in 0..50_000 {
                let (a, b) = (random(&mut rng), random(&mut rng));
                check(&a, &b);
            }

            // One group key for 1, 1.0 and 1.00; zeros of every sign and scale
            // are the key of `Int(0)`, not of -0.0.
            assert_eq!(Dec(dec(100, 2)), Int(1));
            assert_eq!(Dec(dec(100, 2)), Float(1.0));
            assert_eq!(Dec(dec(5, 1)), Float(0.5));
            assert_ne!(Dec(dec(1, 1)), Float(0.1));
            assert_eq!(hash_of(&Dec(dec(150, 2))), hash_of(&Dec(dec(15, 1))));
            let zeros = [
                Int(0),
                Float(0.0),
                Float(-0.0),
                Dec(dec(0, 0)),
                Dec(dec(0, 3)),
                Dec(negative_zero(2)),
            ];
            for a in &zeros {
                for b in &zeros {
                    check(a, b);
                }
            }
            let set = rustc_hash::FxHashSet::from_iter(zeros);
            assert_eq!(set.len(), 2, "-0.0 and 0 are two keys");
        }

        #[test]
        fn test_decimal_owned_value_serde() {
            for (value, json) in [
                (dec(1230, 2), r#"{"decimal":"12.30"}"#),
                (dec(-5, 2), r#"{"decimal":"-0.05"}"#),
                (dec(0, 3), r#"{"decimal":"0.000"}"#),
                (
                    Decimal::MAX,
                    r#"{"decimal":"79228162514264337593543950335"}"#,
                ),
                (
                    dec(1, 28),
                    r#"{"decimal":"0.0000000000000000000000000001"}"#,
                ),
            ] {
                let owned = OwnedValue::Decimal(value);
                assert_eq!(serde_json::to_string(&owned).unwrap(), json);
                let back: OwnedValue = serde_json::from_str(json).unwrap();
                // The scale survives the round trip: `12.30`, not `12.3`.
                assert!(
                    matches!(back, OwnedValue::Decimal(back) if back.to_string() == value.to_string()),
                    "{json}: {back:?}"
                );
            }
            let pretty: OwnedValue = serde_json::from_str(r#" { "decimal" : "1.50" } "#).unwrap();
            assert!(matches!(pretty, OwnedValue::Decimal(value) if value.to_string() == "1.50"));
            // A number, not a decimal text; text that is not a number; more
            // digits than a `Decimal` holds (it would round); another key, none
            // or one too many.
            for json in [
                r#"{"decimal":12.3}"#,
                r#"{"decimal":"12,30"}"#,
                r#"{"decimal":"1e3"}"#,
                r#"{"decimal":"0.00000000000000000000000000001"}"#,
                r#"{"decimal":"79228162514264337593543950336"}"#,
                r#"{"money":"12.30"}"#,
                r#"{}"#,
                r#"{"decimal":"1","decimal":"2"}"#,
            ] {
                assert!(serde_json::from_str::<OwnedValue>(json).is_err(), "{json}");
            }
            // Other values read as before.
            assert!(matches!(serde_json::from_str("1"), Ok(OwnedValue::Int(1))));
            assert!(
                matches!(serde_json::from_str(r#""12.30""#), Ok(OwnedValue::Text(text)) if &*text == "12.30")
            );
        }

        #[test]
        fn test_decimal_change_direction() {
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            struct Amount;

            impl GridColumns for Amount {
                fn all() -> &'static [Self] {
                    &[Self]
                }

                fn id(self) -> &'static str {
                    "amount"
                }

                fn ix(self) -> ColumnIx {
                    ColumnIx::new(0)
                }
            }

            struct Row(CellValue<'static>);

            impl GridRow for Row {
                type Key = u8;
                type Column = Amount;

                fn key(&self) -> &u8 {
                    &0
                }

                fn value(&self, _: Amount) -> CellValue<'_> {
                    self.0
                }
            }

            use CellValue::{Decimal as Dec, Float, Int, Text};
            use ChangeDirection::{Down, Up};
            for (before, after, expected) in [
                (Dec(dec(1230, 2)), Dec(dec(1231, 2)), Some(Up)),
                (Dec(dec(1230, 2)), Dec(dec(-1, 0)), Some(Down)),
                (Dec(dec(10, 1)), Dec(dec(100, 2)), None),
                (Int(1), Dec(dec(100, 2)), None),
                (Float(0.1), Dec(dec(1, 1)), Some(Down)),
                (Dec(dec(5, 1)), Float(0.75), Some(Up)),
                (Text("1"), Dec(dec(1, 0)), None),
            ] {
                assert_eq!(
                    Row(after).change_direction(&Row(before), Amount),
                    expected,
                    "{before:?} → {after:?}"
                );
            }
            // A change of scale alone is not a change.
            let mut changed = ColumnSet::EMPTY;
            Row(Dec(dec(100, 2))).changed_columns(&Row(Dec(dec(1, 0))), &mut changed);
            assert!(changed.is_empty());
        }
    }

    /// `Timestamp`, `Icon` and `Custom` values.
    mod new_values {
        use chrono::TimeZone as _;

        use super::*;

        /// A custom value that sorts by rank: worst first.
        #[derive(Debug)]
        struct Severity(u8);

        impl GridValue for Severity {
            fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
                match other.downcast_ref::<Severity>() {
                    Some(other) => self.0.cmp(&other.0),
                    None => type_order::<Self>(other),
                }
            }

            fn write_text(&self, out: &mut String) {
                out.push_str(["Critical", "Warning", "Info"][usize::from(self.0)]);
            }
        }

        /// A custom value with its own export and owned form: a tenor in
        /// days, shown as `3M`.
        #[derive(Debug)]
        struct Tenor(u32);

        impl GridValue for Tenor {
            fn cmp_value(&self, other: &dyn GridValue) -> Ordering {
                match other.downcast_ref::<Tenor>() {
                    Some(other) => self.0.cmp(&other.0),
                    None => type_order::<Self>(other),
                }
            }

            fn write_text(&self, out: &mut String) {
                let _ = write!(out, "{}M", self.0 / 30);
            }

            fn export_value(&self) -> Option<CellValue<'_>> {
                Some(CellValue::Int(self.0.into()))
            }

            fn to_owned_value(&self) -> OwnedValue {
                OwnedValue::Int(self.0.into())
            }
        }

        /// Values of two types order by type, as `GridValue::cmp_value` asks.
        fn type_order<T: GridValue>(other: &dyn GridValue) -> Ordering {
            std::any::TypeId::of::<T>().cmp(&other.as_any().type_id())
        }

        static SEVERITY: [Severity; 4] = [Severity(0), Severity(1), Severity(1), Severity(2)];
        static TENOR: [Tenor; 2] = [Tenor(30), Tenor(90)];

        fn at(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
            Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
                .unwrap()
        }

        fn icon(path: &'static str, text: &'static str) -> CellValue<'static> {
            CellValue::Icon(IconValue::new(path, text))
        }

        fn image(id: &'static str, text: &'static str) -> CellValue<'static> {
            CellValue::Image(ImageValue::new(id, text))
        }

        /// The `ladder()` with the new values in their places:
        /// timestamps between numbers and text, icons beside the text of
        /// their own text, custom values last, the two types apart.
        fn new_values_ladder() -> Vec<Vec<CellValue<'static>>> {
            use CellValue::{Custom, Text, Timestamp};
            let mut ladder = ladder();
            let text_at = ladder
                .iter()
                .position(|rung| matches!(rung[0], Text(_)))
                .unwrap();
            for (offset, time) in [
                DateTime::<Utc>::MIN_UTC,
                at(1969, 12, 31, 23, 59, 59),
                DateTime::UNIX_EPOCH,
                at(2026, 11, 4, 12, 34, 56),
                at(2026, 11, 4, 12, 34, 56) + chrono::Duration::nanoseconds(1),
                DateTime::<Utc>::MAX_UTC,
            ]
            .into_iter()
            .enumerate()
            {
                ladder.insert(text_at + offset, vec![Timestamp(time)]);
            }
            for rung in &mut ladder {
                match rung[0] {
                    Text("") => rung.push(icon("icons/blank.svg", "")),
                    Text("a") => rung.extend([
                        icon("icons/a.svg", "a"),
                        icon("icons/b.svg", "a"),
                        image("images/a.png", "a"),
                    ]),
                    _ => {}
                }
            }
            ladder.push(vec![icon("icons/z.svg", "z"), image("images/z.svg", "z")]);
            let severities = [
                vec![Custom(&SEVERITY[0])],
                vec![Custom(&SEVERITY[1]), Custom(&SEVERITY[2])],
                vec![Custom(&SEVERITY[3])],
            ];
            let tenors = [vec![Custom(&TENOR[0])], vec![Custom(&TENOR[1])]];
            // The two types in the order of their `TypeId`s.
            if type_order::<Severity>(&TENOR[0]).is_lt() {
                ladder.extend(severities.into_iter().chain(tenors));
            } else {
                ladder.extend(tenors.into_iter().chain(severities));
            }
            ladder
        }

        #[test]
        fn test_new_values_order() {
            let ladder = new_values_ladder();
            for (rung, values) in ladder.iter().enumerate() {
                for (other_rung, others) in ladder.iter().enumerate() {
                    for a in values {
                        for b in others {
                            assert_eq!(
                                a.cmp_total(b),
                                rung.cmp(&other_rung),
                                "{a:?} against {b:?}"
                            );
                        }
                    }
                }
            }
            let values: Vec<CellValue> = ladder.into_iter().flatten().collect();
            for a in &values {
                for b in &values {
                    for c in &values {
                        check_total_order(a, b, c);
                    }
                }
            }
            // An icon sorts by its text, not its path: equal to the text.
            assert_eq!(
                icon("icons/z.svg", "a").cmp_total(&CellValue::Text("a")),
                Ordering::Equal
            );
            assert_eq!(
                icon("icons/a.svg", "b").cmp_total(&icon("icons/z.svg", "a")),
                Ordering::Greater
            );
            // An image too, by its text, not its id.
            assert_eq!(
                image("images/z.png", "a").cmp_total(&CellValue::Text("a")),
                Ordering::Equal
            );
            assert_eq!(
                image("images/a.png", "b").cmp_total(&icon("icons/z.svg", "a")),
                Ordering::Greater
            );
            // Sorting a shuffled mix does not panic, and neighbours are in order.
            let mut rng = fastrand::Rng::with_seed(0x5e7);
            let mut mix: Vec<CellValue> = (0..5_000)
                .map(|_| values[rng.usize(..values.len())])
                .collect();
            mix.sort_by(|a, b| a.cmp_total(b));
            for pair in mix.windows(2) {
                assert_ne!(pair[0].cmp_total(&pair[1]), Ordering::Greater, "{pair:?}");
            }
            // None of them is a number: left-aligned, no direction, no sum.
            for value in [
                CellValue::Timestamp(DateTime::UNIX_EPOCH),
                icon("icons/a.svg", "1"),
                image("images/a.png", "1"),
                CellValue::Custom(&TENOR[0]),
            ] {
                assert!(!value.is_number(), "{value:?}");
            }
        }

        #[test]
        fn test_new_values_text() {
            let mut buffer = String::with_capacity(32);
            let (storage, capacity) = (buffer.as_ptr(), buffer.capacity());
            for _ in 0..2 {
                for (value, expected) in [
                    (
                        CellValue::Timestamp(at(2026, 11, 4, 12, 34, 56)),
                        "2026-11-04 12:34:56",
                    ),
                    (
                        CellValue::Timestamp(at(2026, 11, 4, 0, 0, 0)),
                        "2026-11-04 00:00:00",
                    ),
                    (
                        CellValue::Timestamp(at(2024, 2, 29, 23, 59, 59)),
                        "2024-02-29 23:59:59",
                    ),
                    // A fraction does not show.
                    (
                        CellValue::Timestamp(
                            at(1999, 12, 31, 23, 59, 59) + chrono::Duration::milliseconds(999),
                        ),
                        "1999-12-31 23:59:59",
                    ),
                    (icon("icons/check.svg", "Filled"), "Filled"),
                    (image("images/avatar.png", "Ada"), "Ada"),
                    (CellValue::Custom(&SEVERITY[0]), "Critical"),
                    (CellValue::Custom(&TENOR[1]), "3M"),
                    // A shorter text after a longer one leaves nothing behind.
                    (CellValue::Custom(&SEVERITY[3]), "Info"),
                ] {
                    assert_eq!(display_text(value, &mut buffer), expected, "{value:?}");
                }
            }
            // Timestamps and custom values reuse the caller's buffer: no
            // allocation per cell, even the second time round.
            assert_eq!((buffer.as_ptr(), buffer.capacity()), (storage, capacity));

            // The text of an icon is borrowed from the row, not its path.
            let row_text = String::from("Отменена");
            let borrowed = CellValue::Icon(IconValue::new("icons/x.svg", &row_text));
            assert!(std::ptr::eq(
                display_text(borrowed, &mut buffer),
                row_text.as_str()
            ));
            let value = IconValue::new("icons/x.svg", "x");
            assert_eq!((value.path(), value.text()), ("icons/x.svg", "x"));
            let value = ImageValue::new("images/x.png", "x");
            assert_eq!((value.id(), value.text()), ("images/x.png", "x"));
            assert_eq!(
                OwnedValue::from_cell(CellValue::Image(value)),
                OwnedValue::Text("x".into())
            );
        }

        #[test]
        fn test_new_values_eq_and_diff() {
            use CellValue::{Custom, Float, Int, Text, Timestamp};
            // The old variants compare as the derived `PartialEq` did.
            assert_ne!(Float(f64::NAN), Float(f64::NAN));
            assert_eq!(Float(-0.0), Float(0.0));
            assert_ne!(Int(1), Float(1.0));
            assert_eq!(Text("a"), Text("a"));
            assert_ne!(CellValue::Empty, CellValue::Bool(false));
            // A timestamp by its time; an icon by path and text, never equal
            // to a text; a custom value by `cmp_value`.
            let noon = at(2026, 11, 4, 12, 0, 0);
            assert_eq!(Timestamp(noon), Timestamp(noon));
            assert_ne!(
                Timestamp(noon),
                Timestamp(noon + chrono::Duration::seconds(1))
            );
            assert_eq!(icon("icons/a.svg", "a"), icon("icons/a.svg", "a"));
            assert_ne!(icon("icons/a.svg", "a"), icon("icons/b.svg", "a"));
            assert_ne!(icon("icons/a.svg", "a"), icon("icons/a.svg", "b"));
            assert_ne!(icon("icons/a.svg", "a"), Text("a"));
            assert_eq!(Custom(&SEVERITY[1]), Custom(&SEVERITY[2]));
            assert_ne!(Custom(&SEVERITY[0]), Custom(&SEVERITY[1]));
            assert_ne!(Custom(&SEVERITY[0]), Custom(&TENOR[0]));
            assert_ne!(Custom(&SEVERITY[0]), Text("Critical"));

            // The default `changed_columns` sees each kind of change.
            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            enum Column {
                Placed,
                Status,
                Severity,
            }

            impl GridColumns for Column {
                fn all() -> &'static [Self] {
                    &[Self::Placed, Self::Status, Self::Severity]
                }

                fn id(self) -> &'static str {
                    match self {
                        Self::Placed => "placed",
                        Self::Status => "status",
                        Self::Severity => "severity",
                    }
                }

                fn ix(self) -> ColumnIx {
                    ColumnIx::new(self as u16)
                }
            }

            struct Row {
                placed: DateTime<Utc>,
                status: (&'static str, &'static str),
                severity: &'static Severity,
            }

            impl GridRow for Row {
                type Key = u8;
                type Column = Column;

                fn key(&self) -> &u8 {
                    &0
                }

                fn value(&self, column: Column) -> CellValue<'_> {
                    match column {
                        Column::Placed => Timestamp(self.placed),
                        Column::Status => {
                            CellValue::Icon(IconValue::new(self.status.0, self.status.1))
                        }
                        Column::Severity => Custom(self.severity),
                    }
                }
            }

            let before = Row {
                placed: noon,
                status: ("icons/clock.svg", "Working"),
                severity: &SEVERITY[1],
            };
            let changes = |after: Row| {
                let mut changed = ColumnSet::EMPTY;
                after.changed_columns(&before, &mut changed);
                Column::all()
                    .iter()
                    .copied()
                    .filter(|column| changed.contains(column.ix()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                changes(Row {
                    severity: &SEVERITY[2],
                    ..before
                }),
                []
            );
            assert_eq!(
                changes(Row {
                    placed: noon + chrono::Duration::milliseconds(1),
                    ..before
                }),
                [Column::Placed]
            );
            assert_eq!(
                changes(Row {
                    status: ("icons/check.svg", "Working"),
                    ..before
                }),
                [Column::Status]
            );
            assert_eq!(
                changes(Row {
                    status: ("icons/clock.svg", "Filled"),
                    severity: &SEVERITY[0],
                    ..before
                }),
                [Column::Status, Column::Severity]
            );
            // No direction for new values: they are not numbers.
            let later = Row {
                placed: noon + chrono::Duration::hours(1),
                ..before
            };
            assert_eq!(later.change_direction(&before, Column::Placed), None);
        }

        #[test]
        fn test_owned_timestamp_serde() {
            let time = at(2026, 11, 4, 12, 34, 56);
            for (value, json) in [
                (time, r#"{"timestamp":"2026-11-04T12:34:56Z"}"#),
                (
                    time + chrono::Duration::milliseconds(250),
                    r#"{"timestamp":"2026-11-04T12:34:56.250Z"}"#,
                ),
                (
                    time + chrono::Duration::nanoseconds(1),
                    r#"{"timestamp":"2026-11-04T12:34:56.000000001Z"}"#,
                ),
                (
                    DateTime::UNIX_EPOCH,
                    r#"{"timestamp":"1970-01-01T00:00:00Z"}"#,
                ),
                (
                    at(0, 1, 1, 0, 0, 0),
                    r#"{"timestamp":"0000-01-01T00:00:00Z"}"#,
                ),
            ] {
                let owned = OwnedValue::Timestamp(value);
                assert_eq!(serde_json::to_string(&owned).unwrap(), json);
                let back: OwnedValue = serde_json::from_str(json).unwrap();
                assert!(
                    matches!(back, OwnedValue::Timestamp(back) if back == value),
                    "{json}"
                );
            }
            // Any offset reads as the point in time it names.
            let moscow: OwnedValue =
                serde_json::from_str(r#" { "timestamp" : "2026-11-04T15:34:56+03:00" } "#).unwrap();
            assert_eq!(moscow, OwnedValue::Timestamp(time));
            assert_eq!(hash_of(&moscow), hash_of(&OwnedValue::Timestamp(time)));
            // A string stays text, even one that reads as a time.
            assert!(matches!(
                serde_json::from_str(r#""2026-11-04T12:34:56Z""#),
                Ok(OwnedValue::Text(text)) if &*text == "2026-11-04T12:34:56Z"
            ));
            // Not a time, not RFC 3339, a number, another key, none, or one
            // too many.
            for json in [
                r#"{"timestamp":"x"}"#,
                r#"{"timestamp":"2026-11-04 12:34:56"}"#,
                r#"{"timestamp":"2026-11-04"}"#,
                r#"{"timestamp":1762259696}"#,
                r#"{"time":"2026-11-04T12:34:56Z"}"#,
                r#"{}"#,
                r#"{"timestamp":"2026-11-04T12:34:56Z","timestamp":"2026-11-04T12:34:56Z"}"#,
            ] {
                assert!(serde_json::from_str::<OwnedValue>(json).is_err(), "{json}");
            }
            // A year RFC 3339 cannot write is not serialized.
            for value in [DateTime::<Utc>::MAX_UTC, DateTime::<Utc>::MIN_UTC] {
                assert!(serde_json::to_string(&OwnedValue::Timestamp(value)).is_err());
            }

            // Owned values: a timestamp is itself, an icon its text, a custom
            // value what `to_owned_value` gives (by default its text).
            assert_eq!(
                OwnedValue::from_cell(CellValue::Timestamp(time)).as_cell(),
                CellValue::Timestamp(time)
            );
            assert!(matches!(
                OwnedValue::from_cell(icon("icons/check.svg", "Filled")),
                OwnedValue::Text(text) if &*text == "Filled"
            ));
            assert!(matches!(
                OwnedValue::from_cell(CellValue::Custom(&SEVERITY[0])),
                OwnedValue::Text(text) if &*text == "Critical"
            ));
            assert!(matches!(
                OwnedValue::from_cell(CellValue::Custom(&TENOR[1])),
                OwnedValue::Int(90)
            ));
            // Equal exactly when `cmp_total` says so, and equal values hash
            // alike, timestamps beside every other kind.
            let owned: Vec<OwnedValue> = new_values_ladder()
                .into_iter()
                .flatten()
                .map(OwnedValue::from_cell)
                .collect();
            for a in &owned {
                for b in &owned {
                    assert_eq!(a == b, a.cmp_total(b).is_eq(), "{a:?} and {b:?}");
                    if a == b {
                        assert_eq!(hash_of(a), hash_of(b), "{a:?} and {b:?} hash apart");
                    }
                }
            }
        }

        #[test]
        fn test_grid_value_downcast() {
            let value: &dyn GridValue = &TENOR[0];
            assert!(
                value
                    .downcast_ref::<Tenor>()
                    .is_some_and(|tenor| tenor.0 == 30)
            );
            assert!(value.downcast_ref::<Severity>().is_none());
            assert_eq!(value.as_any().type_id(), std::any::TypeId::of::<Tenor>());
            // The default export is none: a raw copy writes the text.
            assert!(SEVERITY[0].export_value().is_none());
        }
    }

    #[test]
    fn test_column_set_rank() {
        let mut set = ColumnSet::EMPTY;
        for ix in [1, 4, 127] {
            set.insert(ColumnIx::new(ix));
        }
        let ranks: Vec<usize> = [0, 1, 2, 4, 5, 127]
            .map(|ix| set.rank(ColumnIx::new(ix)))
            .to_vec();
        assert_eq!(ranks, [0, 0, 1, 1, 2, 2]);
    }
}
