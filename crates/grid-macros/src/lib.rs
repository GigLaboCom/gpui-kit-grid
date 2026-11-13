//! `#[derive(GridColumns)]` for gpui-kit-grid. Applications reach it
//! through the grid crate's `derive` feature: `gpui_kit_grid::GridColumns`
//! names both the trait and this derive.

mod case;
mod columns;
mod expand;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Derives `GridColumns` for an enum of columns (feature `derive` of
/// gpui-kit-grid).
///
/// Each unit variant is a column. Declaration order is the schema order:
/// the order of `all()` and the index `ix()`. The column id, which saved
/// layouts store, is the variant name in `snake_case` unless an attribute
/// sets it.
///
/// ```
/// use gpui_kit_grid::GridColumns;
///
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum TradeColumn {
///     Id,         // "id"
///     Instrument, // "instrument"
///     BidSize,    // "bid_size"
///     // A saved layout's id, kept after a rename.
///     #[grid_column(id = "px")]
///     Price,
/// }
///
/// assert_eq!(TradeColumn::BidSize.id(), "bid_size");
/// assert_eq!(TradeColumn::Price.id(), "px");
/// assert_eq!(TradeColumn::Price.ix().get(), 3);
/// assert_eq!(TradeColumn::all()[3], TradeColumn::Price);
/// ```
///
/// The expansion is the implementation one would write by hand: `all`
/// lists the variants, `id` and `ix` match on them. `ix` follows
/// declaration order rather than the discriminant, so `Id = 10` does not
/// shift it. With the same names, the ids, the order and the indices are
/// those of a hand-written implementation that spells its ids in
/// `snake_case`, so moving to the derive keeps saved layouts.
///
/// # Attributes
///
/// | Attribute | Where | Value | Default |
/// |---|---|---|---|
/// | `id = "…"` | `#[grid_column(…)]` on a variant | The column id: not empty, unique in the enum | The variant name by `rename_all` |
/// | `rename_all = "…"` | `#[grid_columns(…)]` on the enum | `"snake_case"`, `"kebab-case"`, `"camelCase"`, `"PascalCase"` (the name as written) or `"lowercase"` | `"snake_case"` |
/// | `crate = "…"` | `#[grid_columns(…)]` on the enum | The path of the grid crate when it is renamed or re-exported, such as `"my_ui::grid"` | `::gpui_kit_grid` |
///
/// Names split into words as the `heck` crate splits them: `BidSize` is
/// `bid_size`, `HTTPStatus` is `http_status` and `Leg2Price` is
/// `leg2_price`.
///
/// A variant may carry `#[deprecated]` (the expansion does not warn) and
/// `#[cfg(…)]`: indices count the variants of the build, while ids do not
/// depend on it, so a saved layout survives a feature switch.
///
/// # Errors
///
/// The derive refuses, pointing at the part at fault:
///
/// - a struct or a union, and an enum with generic parameters;
/// - a variant with fields, an enum without variants and a 129th variant
///   (a grid has at most 128 columns);
/// - an empty or repeated column id;
/// - an unknown or repeated key, and an unknown `rename_all` rule.
///
/// It reports every error of the enum at once. The enum derives `Clone`,
/// `Copy`, `Debug`, `PartialEq`, `Eq` and `Hash` itself: `GridColumns`
/// requires them and the derive does not add them.
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum TradeColumn {
///     Price,
///     // error: the column id `price` is already used by `Price`
///     #[grid_column(id = "price")]
///     Px,
/// }
/// ```
#[proc_macro_derive(GridColumns, attributes(grid_columns, grid_column))]
pub fn derive_grid_columns(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    columns::ColumnEnum::parse(&input)
        .map(|columns| expand::expand(&columns))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Each input the derive refuses, next to the same input fixed, so that a
/// `compile_fail` block fails for its error and not for a typo. The unit
/// tests of the `columns` module check the messages.
///
/// Not an enum:
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// struct Columns;
/// ```
///
/// ```
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Price }
/// ```
///
/// A variant with fields:
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Id, Price(u8) }
/// ```
///
/// ```
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Id, Price }
/// ```
///
/// A repeated id:
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Price, #[grid_column(id = "price")] Px }
/// ```
///
/// ```
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Price, #[grid_column(id = "px")] Px }
/// ```
///
/// No variants:
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns {}
/// ```
///
/// ```
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { Price }
/// ```
///
/// An unknown key:
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { #[grid_column(name = "px")] Price }
/// ```
///
/// ```
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns { #[grid_column(id = "px")] Price }
/// ```
///
/// Generic parameters: rustc accepts an unused const parameter, so only the
/// derive refuses this one.
///
/// ```compile_fail
/// # use gpui_kit_grid::GridColumns;
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
/// enum Columns<const N: usize> { Price }
/// ```
///
/// ```
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// enum Columns<const N: usize> { Price }
/// ```
#[cfg(doctest)]
mod compile_fail {}
