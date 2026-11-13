//! Aggregates of groups and of the whole view. No GPUI types.
//!
//! Every job computes them afresh, with no incremental `+new - old`, so a
//! float sum does not drift. A group of the last level adds its rows; an
//! upper group, and the totals, merge their subgroups' partial sums in key
//! order.

use std::{cmp::Ordering, fmt, mem, ops::Range, sync::Arc};

#[cfg(feature = "decimal")]
use rust_decimal::{Decimal, prelude::ToPrimitive as _};

use super::{
    Projection, ViewSpec,
    display::{AggregateValues, GroupData},
};
use crate::{
    model::{CellValue, ColumnSet, GridColumns, GridRow, OwnedValue},
    store::{RowId, StoreSnapshot},
};

/// A custom aggregate for [`Aggregate::Custom`]: a weighted average, a
/// median. Called in the background, from several threads of a large
/// rebuild, once per group.
pub trait AggregateFn<R: GridRow>: Send + Sync + 'static {
    /// The aggregate of `column` over the live rows of one group, or of the
    /// whole view for the totals.
    fn aggregate(&self, column: R::Column, rows: &mut dyn Iterator<Item = &R>) -> OwnedValue;

    /// The aggregate of a group from its subgroups' aggregates, when the
    /// function allows it (a sum does, a median does not). `None`, the
    /// default: the group's rows are iterated.
    fn combine(&self, _parts: &[&OwnedValue]) -> Option<OwnedValue> {
        None
    }

    /// The columns whose changes change the aggregate of `column`: that
    /// column by default.
    fn depends_on(&self, column: R::Column) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        columns.insert(column.ix());
        columns
    }

    /// The name a tooltip and a screen reader give the aggregate
    /// ("Weighted average"); empty by default.
    fn name(&self) -> &str {
        ""
    }
}

/// The aggregate of a column, for group captions, group footers and the
/// totals. Every job computes them afresh over the live rows; without a
/// value to aggregate, they are `Empty` (a `Count`, `Int(0)`).
#[non_exhaustive]
pub enum Aggregate<R: GridRow> {
    /// The sum of the numbers: an `Int` over integers while it fits in an
    /// `i64` (added exactly), a `Float` once a float is among them or it
    /// overflows. With the `decimal` feature, decimals and integers add up
    /// exactly to a `Decimal`, a `Float` once a float is among them or the
    /// sum overflows a `Decimal`. Text, booleans and blanks are skipped.
    Sum,
    /// The mean of the numbers, a `Float`; of decimals and integers alone, a
    /// `Decimal` rounded to two places more than the largest scale among
    /// them.
    Avg,
    /// The smallest value that is not `Empty`, by `CellValue::cmp_total`.
    Min,
    /// The largest value that is not `Empty`, by `CellValue::cmp_total`.
    Max,
    /// The number of values that are not `Empty`, as SQL's
    /// `COUNT(column)`; a group's rows are `GroupInfo::row_count`.
    Count,
    /// The application's aggregate.
    Custom(Arc<dyn AggregateFn<R>>),
}

impl<R: GridRow> Clone for Aggregate<R> {
    fn clone(&self) -> Self {
        match self {
            Self::Sum => Self::Sum,
            Self::Avg => Self::Avg,
            Self::Min => Self::Min,
            Self::Max => Self::Max,
            Self::Count => Self::Count,
            Self::Custom(function) => Self::Custom(function.clone()),
        }
    }
}

impl<R: GridRow> fmt::Debug for Aggregate<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sum => f.write_str("Sum"),
            Self::Avg => f.write_str("Avg"),
            Self::Min => f.write_str("Min"),
            Self::Max => f.write_str("Max"),
            Self::Count => f.write_str("Count"),
            Self::Custom(function) => f.debug_tuple("Custom").field(&function.name()).finish(),
        }
    }
}

impl<R: GridRow> Aggregate<R> {
    /// The columns whose changes change this aggregate of `column`.
    pub(super) fn depends_on(&self, column: R::Column) -> ColumnSet {
        match self {
            Self::Custom(function) => function.depends_on(column),
            Self::Sum | Self::Avg | Self::Min | Self::Max | Self::Count => {
                let mut columns = ColumnSet::EMPTY;
                columns.insert(column.ix());
                columns
            }
        }
    }
}

impl<R: GridRow> ViewSpec<R> {
    /// Aggregates these columns, for group captions, group footers and the
    /// totals. A repeated column keeps its last aggregate. The aggregates
    /// survive a change of grouping or tree. A view without grouping has
    /// totals too.
    pub fn with_aggregates(
        mut self,
        aggregates: impl IntoIterator<Item = (R::Column, Aggregate<R>)>,
    ) -> Self {
        let mut unique: Vec<(R::Column, Aggregate<R>)> = Vec::new();
        for (column, aggregate) in aggregates {
            match unique.iter_mut().find(|(kept, _)| *kept == column) {
                Some(kept) => kept.1 = aggregate,
                None => unique.push((column, aggregate)),
            }
        }
        self.aggregates = (!unique.is_empty()).then(|| unique.into());
        self
    }

    /// The aggregates by column.
    pub fn aggregates(&self) -> &[(R::Column, Aggregate<R>)] {
        self.aggregates.as_deref().unwrap_or_default()
    }

    /// The columns whose changes change aggregates: the aggregated columns
    /// and what custom aggregates depend on.
    pub fn aggregate_columns(&self) -> ColumnSet {
        let mut columns = ColumnSet::EMPTY;
        for (column, aggregate) in self.aggregates() {
            columns.union_with(aggregate.depends_on(*column));
        }
        columns
    }
}

/// The aggregates of a view, in the schema order of their columns: the
/// order of an [`AggregateValues`] run.
pub(super) struct ViewAggregates<'s, R: GridRow> {
    columns: ColumnSet,
    list: Vec<(R::Column, &'s Aggregate<R>)>,
    /// Places in `list` of the built-in aggregates, computed row by row.
    built_in: Vec<usize>,
    /// Places in `list` of the custom ones, computed once per group.
    custom: Vec<(usize, &'s dyn AggregateFn<R>)>,
}

impl<'s, R: GridRow> ViewAggregates<'s, R> {
    pub(super) fn new(spec: &'s ViewSpec<R>) -> Self {
        let mut list: Vec<(R::Column, &Aggregate<R>)> = spec
            .aggregates()
            .iter()
            .map(|(column, aggregate)| (*column, aggregate))
            .collect();
        list.sort_by_key(|(column, _)| column.ix());
        let mut columns = ColumnSet::EMPTY;
        let mut built_in = Vec::new();
        let mut custom = Vec::new();
        for (ix, (column, aggregate)) in list.iter().enumerate() {
            columns.insert(column.ix());
            match aggregate {
                Aggregate::Custom(function) => custom.push((ix, &**function)),
                Aggregate::Sum
                | Aggregate::Avg
                | Aggregate::Min
                | Aggregate::Max
                | Aggregate::Count => built_in.push(ix),
            }
        }
        Self {
            columns,
            list,
            built_in,
            custom,
        }
    }

    /// The aggregated columns.
    pub(super) fn columns(&self) -> ColumnSet {
        self.columns
    }

    /// Returns true if the view aggregates nothing.
    pub(super) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// A fresh partial per aggregate.
    fn start<'a>(&self) -> impl Iterator<Item = Partial<'a>> + '_ {
        self.list
            .iter()
            .map(|(_, aggregate)| Partial::start(aggregate))
    }

    /// Adds `row` to the built-in partials of one run.
    #[inline]
    fn add<'a>(&self, partials: &mut [Partial<'a>], row: &'a R) {
        for &ix in &self.built_in {
            partials[ix].add(row.value(self.list[ix].0));
        }
    }

    /// Merges the built-in partials of `part` into `partials`.
    fn merge<'a>(&self, partials: &mut [Partial<'a>], part: &[Partial<'a>]) {
        for &ix in &self.built_in {
            partials[ix].merge(&part[ix]);
        }
    }

    /// Writes the built-in results of `partials` into `values`.
    fn finish(&self, partials: &[Partial<'_>], values: &mut [OwnedValue]) {
        for &ix in &self.built_in {
            values[ix] = partials[ix].finish();
        }
    }
}

/// What a built-in aggregate keeps while a job adds rows to it: its partial
/// sums. Extremes borrow their values from the snapshot's rows, so a group
/// allocates only for its final text.
#[derive(Clone, Copy, Debug)]
enum Partial<'a> {
    Sum(Numbers),
    Avg(Numbers),
    /// The smallest value so far; `Empty` for none (no other value is).
    Min(CellValue<'a>),
    /// The largest value so far; `Empty` for none.
    Max(CellValue<'a>),
    /// Values that are not `Empty`.
    Count(u64),
    /// A custom aggregate keeps nothing: it reads rows or its parts.
    Custom,
}

/// The numbers of a sum or a mean: integers and decimals exactly, floats
/// apart. A sum adds floats in the order rows and then groups come, so it
/// is the same on every run.
#[derive(Clone, Copy, Debug, Default)]
struct Numbers {
    int: i128,
    float: f64,
    has_float: bool,
    count: u64,
    #[cfg(feature = "decimal")]
    decimal: Decimals,
}

impl Numbers {
    fn add(&mut self, value: CellValue<'_>) {
        match value {
            CellValue::Int(value) => {
                self.int += i128::from(value);
                self.count += 1;
            }
            CellValue::Float(value) => {
                self.float += value;
                self.has_float = true;
                self.count += 1;
            }
            #[cfg(feature = "decimal")]
            CellValue::Decimal(value) => {
                self.decimal.add(value);
                self.count += 1;
            }
            // Text, booleans and blanks are not numbers.
            _ => {}
        }
    }

    fn merge(&mut self, other: &Self) {
        self.int += other.int;
        self.float += other.float;
        self.has_float |= other.has_float;
        self.count += other.count;
        #[cfg(feature = "decimal")]
        self.decimal.merge(&other.decimal);
    }

    /// The sum: `Int` while there are integers alone and it fits, `Decimal`
    /// while there are decimals and integers alone and it fits, `Float`
    /// otherwise; `Empty` without numbers.
    fn sum(&self) -> OwnedValue {
        if self.count == 0 {
            return OwnedValue::Empty;
        }
        #[cfg(feature = "decimal")]
        if self.decimal.has_values && !self.has_float {
            return self
                .decimal
                .exact_sum(self.int)
                .map_or_else(OwnedValue::Float, OwnedValue::Decimal);
        }
        if self.has_float {
            OwnedValue::Float(self.float_sum())
        } else {
            i64::try_from(self.int).map_or(OwnedValue::Float(self.int as f64), OwnedValue::Int)
        }
    }

    /// The mean: a `Decimal` over decimals and integers alone, a `Float`
    /// otherwise; `Empty` without numbers.
    fn mean(&self) -> OwnedValue {
        if self.count == 0 {
            return OwnedValue::Empty;
        }
        #[cfg(feature = "decimal")]
        if self.decimal.has_values && !self.has_float {
            return self
                .decimal
                .mean(self.int, self.count)
                .map_or_else(OwnedValue::Float, OwnedValue::Decimal);
        }
        OwnedValue::Float(self.float_sum() / self.count as f64)
    }

    /// Every number added as a float.
    fn float_sum(&self) -> f64 {
        let sum = self.float + self.int as f64;
        #[cfg(feature = "decimal")]
        if self.decimal.has_values {
            return sum + self.decimal.sum.to_f64();
        }
        sum
    }
}

/// The decimals of a sum or a mean: added exactly, so money has no float drift.
#[cfg(feature = "decimal")]
#[derive(Clone, Copy, Debug, Default)]
struct Decimals {
    sum: DecimalSum,
    /// The largest scale among the values: a mean keeps two places more.
    scale: u32,
    has_values: bool,
}

/// A sum of decimals: exact until it overflows a `Decimal` (about 7.9 ×
/// 10^28), a float from then on, as an `Int` sum that overflows `i64`.
#[cfg(feature = "decimal")]
#[derive(Clone, Copy, Debug)]
enum DecimalSum {
    Exact(Decimal),
    Overflow(f64),
}

#[cfg(feature = "decimal")]
impl Default for DecimalSum {
    fn default() -> Self {
        Self::Exact(Decimal::ZERO)
    }
}

#[cfg(feature = "decimal")]
impl DecimalSum {
    fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Exact(a), Self::Exact(b)) => a
                .checked_add(b)
                .map_or_else(|| Self::Overflow(as_f64(a) + as_f64(b)), Self::Exact),
            (a, b) => Self::Overflow(a.to_f64() + b.to_f64()),
        }
    }

    fn to_f64(self) -> f64 {
        match self {
            Self::Exact(sum) => as_f64(sum),
            Self::Overflow(sum) => sum,
        }
    }
}

#[cfg(feature = "decimal")]
fn as_f64(value: Decimal) -> f64 {
    value.to_f64().unwrap_or_default()
}

#[cfg(feature = "decimal")]
impl Decimals {
    fn add(&mut self, value: Decimal) {
        self.sum = self.sum.plus(DecimalSum::Exact(value));
        self.scale = self.scale.max(value.scale());
        self.has_values = true;
    }

    fn merge(&mut self, other: &Self) {
        if other.has_values {
            self.sum = self.sum.plus(other.sum);
            self.scale = self.scale.max(other.scale);
            self.has_values = true;
        }
    }

    /// The sum with the integers `int`: the exact `Decimal`, or the float of
    /// a sum too large for one.
    fn exact_sum(&self, int: i128) -> Result<Decimal, f64> {
        let int = Decimal::try_from_i128_with_scale(int, 0)
            .map_or_else(|_| DecimalSum::Overflow(int as f64), DecimalSum::Exact);
        match self.sum.plus(int) {
            DecimalSum::Exact(sum) => Ok(sum),
            DecimalSum::Overflow(sum) => Err(sum),
        }
    }

    /// The mean of `count` values that add up to the decimals and `int`,
    /// rounded to two places more than the largest scale; a float when the
    /// sum is too large.
    fn mean(&self, int: i128, count: u64) -> Result<Decimal, f64> {
        let sum = self.exact_sum(int).map_err(|sum| sum / count as f64)?;
        sum.checked_div(Decimal::from(count))
            .map(|mean| mean.round_dp(self.scale + 2))
            .ok_or_else(|| as_f64(sum) / count as f64)
    }
}

impl<'a> Partial<'a> {
    fn start<R: GridRow>(aggregate: &Aggregate<R>) -> Self {
        match aggregate {
            Aggregate::Sum => Self::Sum(Numbers::default()),
            Aggregate::Avg => Self::Avg(Numbers::default()),
            Aggregate::Min => Self::Min(CellValue::Empty),
            Aggregate::Max => Self::Max(CellValue::Empty),
            Aggregate::Count => Self::Count(0),
            Aggregate::Custom(_) => Self::Custom,
        }
    }

    #[inline]
    fn add(&mut self, value: CellValue<'a>) {
        match self {
            Self::Sum(numbers) | Self::Avg(numbers) => numbers.add(value),
            Self::Min(min) => keep_extreme(min, value, Ordering::Less),
            Self::Max(max) => keep_extreme(max, value, Ordering::Greater),
            Self::Count(count) => *count += u64::from(!matches!(value, CellValue::Empty)),
            Self::Custom => {}
        }
    }

    fn merge(&mut self, other: &Self) {
        match (self, other) {
            (Self::Sum(numbers), Self::Sum(other)) | (Self::Avg(numbers), Self::Avg(other)) => {
                numbers.merge(other)
            }
            (Self::Min(min), Self::Min(other)) => keep_extreme(min, *other, Ordering::Less),
            (Self::Max(max), Self::Max(other)) => keep_extreme(max, *other, Ordering::Greater),
            (Self::Count(count), Self::Count(other)) => *count += other,
            _ => {}
        }
    }

    fn finish(&self) -> OwnedValue {
        match self {
            Self::Sum(numbers) => numbers.sum(),
            Self::Avg(numbers) => numbers.mean(),
            Self::Min(value) | Self::Max(value) => OwnedValue::from_cell(*value),
            Self::Count(count) => OwnedValue::Int(i64::try_from(*count).unwrap_or(i64::MAX)),
            Self::Custom => OwnedValue::Empty,
        }
    }
}

/// Keeps `value` in `extreme` if it is a value and goes further in `side`
/// than the one kept: on a tie the first stays.
#[inline]
fn keep_extreme<'a>(extreme: &mut CellValue<'a>, value: CellValue<'a>, side: Ordering) {
    if !matches!(value, CellValue::Empty)
        && (matches!(extreme, CellValue::Empty) || value.cmp_total(extreme) == side)
    {
        *extreme = value;
    }
}

/// A group as aggregates see it, in key order: before its subgroups, and
/// siblings in the order of their keys, before the group order sorts them.
#[derive(Clone, Debug)]
pub(super) struct KeyedGroup {
    pub(super) depth: u16,
    /// The group past its last subgroup, in key order.
    pub(super) end: u32,
    /// Its rows; read at the last level only.
    pub(super) rows: Range<u32>,
}

/// The aggregates of the groups of a view and its totals.
pub(super) struct GroupValues {
    /// A run of values per group, in key order.
    pub(super) values: Vec<OwnedValue>,
    /// The live rows of each group, in key order.
    pub(super) live: Vec<u32>,
    /// The totals' run.
    pub(super) totals: Vec<OwnedValue>,
    /// The live rows of the view.
    pub(super) total_live: usize,
}

/// Aggregates `groups`, listed in key order over `rows`, from the bottom
/// up: a group of the last level adds its live rows (`row` gives `None` for
/// a row the snapshot no longer has), an upper group merges its subgroups'
/// partials in key order, and the totals merge the top level's. So the
/// result depends neither on the group order nor on the number of threads.
///
/// A custom aggregate combines its subgroups' values when it can
/// ([`AggregateFn::combine`]); otherwise it reads the group's rows, in key
/// order too.
pub(super) fn aggregate_groups<'r, 'a, R: GridRow, T>(
    aggregates: &ViewAggregates<'_, R>,
    groups: &[KeyedGroup],
    levels: u16,
    rows: &'r [T],
    row: impl Fn(&'r T) -> Option<&'a R> + Copy,
) -> GroupValues {
    let width = aggregates.list.len();
    let is_leaf = |group: &KeyedGroup| usize::from(group.depth) + 1 == usize::from(levels);
    // The live rows of the last-level groups in `span`, in key order.
    let rows_of = |span: Range<usize>| {
        groups[span]
            .iter()
            .filter(move |group| is_leaf(group))
            .flat_map(move |group| {
                rows[group.rows.start as usize..group.rows.end as usize]
                    .iter()
                    .filter_map(row)
            })
    };
    let mut partials: Vec<Partial<'a>> = Vec::with_capacity(groups.len() * width);
    for _ in groups {
        partials.extend(aggregates.start());
    }
    let mut values = vec![OwnedValue::Empty; groups.len() * width];
    let mut live = vec![0u32; groups.len()];

    for (ix, group) in groups.iter().enumerate().rev() {
        let run = ix * width..(ix + 1) * width;
        if is_leaf(group) {
            let partial = &mut partials[run.clone()];
            for row in rows_of(ix..ix + 1) {
                live[ix] += 1;
                aggregates.add(partial, row);
            }
            for &(place, function) in &aggregates.custom {
                let column = aggregates.list[place].0;
                values[ix * width + place] = function.aggregate(column, &mut rows_of(ix..ix + 1));
            }
        } else {
            let span = ix + 1..group.end as usize;
            // Subgroups come after the group, so they are done.
            let (head, tail) = partials.split_at_mut((ix + 1) * width);
            let mut child = span.start;
            while child < span.end {
                live[ix] += live[child];
                let part = (child - span.start) * width;
                aggregates.merge(&mut head[run.clone()], &tail[part..part + width]);
                child = groups[child].end as usize;
            }
            for &(place, function) in &aggregates.custom {
                let column = aggregates.list[place].0;
                let (head, tail) = values.split_at_mut((ix + 1) * width);
                let parts = children(groups, span.clone())
                    .map(|child| &tail[(child - span.start) * width + place])
                    .collect::<Vec<_>>();
                head[ix * width + place] = function
                    .combine(&parts)
                    .unwrap_or_else(|| function.aggregate(column, &mut rows_of(span.clone())));
            }
        }
        aggregates.finish(&partials[run.clone()], &mut values[run]);
    }

    // The totals from the top level's groups, in key order.
    let mut total: Vec<Partial<'a>> = aggregates.start().collect();
    let mut total_live = 0;
    for top in children(groups, 0..groups.len()) {
        total_live += live[top] as usize;
        aggregates.merge(&mut total, &partials[top * width..(top + 1) * width]);
    }
    let mut totals = vec![OwnedValue::Empty; width];
    aggregates.finish(&total, &mut totals);
    for &(place, function) in &aggregates.custom {
        let column = aggregates.list[place].0;
        let parts = children(groups, 0..groups.len())
            .map(|top| &values[top * width + place])
            .collect::<Vec<_>>();
        // A view without groups has nothing to combine: its rows, none.
        totals[place] = (!parts.is_empty())
            .then(|| function.combine(&parts))
            .flatten()
            .unwrap_or_else(|| function.aggregate(column, &mut rows_of(0..groups.len())));
    }
    GroupValues {
        values,
        live,
        totals,
        total_live,
    }
}

/// The groups of `span` with no ancestor in it: the children of the group
/// before `span`, or the top level for the whole list.
fn children(groups: &[KeyedGroup], span: Range<usize>) -> impl Iterator<Item = usize> + '_ {
    let mut next = span.start;
    std::iter::from_fn(move || {
        let child = next;
        (child < span.end).then(|| {
            next = groups[child].end as usize;
            child
        })
    })
}

/// The totals of `rows` in one pass, in order: the live rows of a flat
/// view, or the rows a tree's totals count. Returns the totals' run and the
/// number of rows. A custom aggregate goes over the rows again.
pub(super) fn flat_totals<'a, R: GridRow>(
    aggregates: &ViewAggregates<'_, R>,
    rows: impl Iterator<Item = &'a R> + Clone,
) -> (Vec<OwnedValue>, usize) {
    let mut partials: Vec<Partial<'a>> = aggregates.start().collect();
    let mut count = 0;
    for row in rows.clone() {
        count += 1;
        aggregates.add(&mut partials, row);
    }
    let mut totals = vec![OwnedValue::Empty; aggregates.list.len()];
    aggregates.finish(&partials, &mut totals);
    for &(place, function) in &aggregates.custom {
        let column = aggregates.list[place].0;
        totals[place] = function.aggregate(column, &mut rows.clone());
    }
    (totals, count)
}

/// The order of a flat or grouped projection with fresh aggregates and row
/// counts from the rows `snapshot` still has: the aggregate job. The order,
/// the lines, the expansion and the footers stay; the version is the
/// snapshot's. Groups combine in key order, as in a rebuild, so the result
/// is what a rebuild of the same order gives.
pub(super) fn refresh<R: GridRow>(
    previous: &Projection,
    snapshot: &StoreSnapshot<R>,
    spec: &ViewSpec<R>,
) -> Projection {
    let aggregates = ViewAggregates::new(spec);
    let row = |id: &RowId| snapshot.row(*id).map(|row| &**row);
    let Some(data) = previous.group_data() else {
        let (totals, live) = flat_totals(&aggregates, previous.rows().iter().filter_map(row));
        return previous
            .duplicate()
            .with_store_version(snapshot.version())
            .with_row_count(live)
            .with_totals(AggregateValues::new(aggregates.columns(), totals));
    };

    // The groups in key order: the order their key places give.
    let mut key_order: Vec<usize> = (0..data.groups.len()).collect();
    key_order.sort_unstable_by_key(|&ix| data.groups[ix].key_ix);
    let mut keyed: Vec<KeyedGroup> = key_order
        .iter()
        .map(|&ix| {
            let group = &data.groups[ix];
            KeyedGroup {
                depth: group.depth(),
                end: 0,
                rows: group.rows.clone(),
            }
        })
        .collect();
    // A group's subgroups follow it in key order too, up to the next group
    // no deeper than it.
    let mut open: Vec<usize> = Vec::new();
    for ix in 0..keyed.len() {
        while let Some(&top) = open.last()
            && keyed[top].depth >= keyed[ix].depth
        {
            keyed[top].end = ix as u32;
            open.pop();
        }
        open.push(ix);
    }
    for top in open {
        keyed[top].end = keyed.len() as u32;
    }

    let mut computed = aggregate_groups(&aggregates, &keyed, data.levels, previous.rows(), row);
    let width = aggregates.columns().len();
    let mut groups = data.groups.clone();
    let mut values = vec![OwnedValue::Empty; groups.len() * width];
    for (key_ix, &ix) in key_order.iter().enumerate() {
        groups[ix].live = computed.live[key_ix];
        for (value, computed) in values[ix * width..(ix + 1) * width]
            .iter_mut()
            .zip(&mut computed.values[key_ix * width..(key_ix + 1) * width])
        {
            *value = mem::replace(computed, OwnedValue::Empty);
        }
    }
    let columns = aggregates.columns();
    previous.with_group_values(
        GroupData::new(data.levels, groups, AggregateValues::new(columns, values)),
        AggregateValues::new(columns, computed.totals),
        computed.total_live,
        snapshot.version(),
    )
}
