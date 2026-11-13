//! `#[derive(GridColumns)]`: the implementation a hand-written one would be,
//! the naming rule and explicit ids, discriminants, a renamed crate, deprecated
//! and `cfg` variants, 128 columns, and derived ids in a saved layout.
//!
//! A warning in the expansion fails the build of this file.

#![deny(warnings)]

use gpui_kit_grid::{ColumnIx, ColumnSet, GridColumns};

// #region derived-columns
/// The columns of the `quickstart` example, derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
enum TradeColumn {
    Id,
    Instrument,
    Side,
    Quantity,
    Price,
}
// #endregion derived-columns

/// The same columns, implemented by hand as `quickstart` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ManualColumn {
    Id,
    Instrument,
    Side,
    Quantity,
    Price,
}

impl GridColumns for ManualColumn {
    fn all() -> &'static [Self] {
        use ManualColumn::*;
        &[Id, Instrument, Side, Quantity, Price]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Instrument => "instrument",
            Self::Side => "side",
            Self::Quantity => "quantity",
            Self::Price => "price",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

fn ids<C: GridColumns>() -> Vec<&'static str> {
    C::all().iter().map(|column| column.id()).collect()
}

/// Every column sits at its index in `all()`, and the indices are dense.
fn assert_indices<C: GridColumns>() {
    for (ix, &column) in C::all().iter().enumerate() {
        assert_eq!(column.ix(), ColumnIx::new(ix as u16), "{column:?}");
        assert_eq!(C::all()[column.ix().get()], column);
    }
}

#[test]
fn test_derive_matches_manual() {
    assert_eq!(
        ids::<TradeColumn>(),
        ["id", "instrument", "side", "quantity", "price"]
    );
    assert_eq!(ids::<TradeColumn>(), ids::<ManualColumn>());
    for (derived, manual) in TradeColumn::all().iter().zip(ManualColumn::all()) {
        assert_eq!(derived.id(), manual.id());
        assert_eq!(derived.ix(), manual.ix());
        assert_eq!(derived.column_id(), manual.column_id());
    }
    assert_indices::<TradeColumn>();
    assert_indices::<ManualColumn>();
}

#[test]
fn test_derive_rename_and_ids() {
    // #region derive-rename
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
    #[grid_columns(rename_all = "kebab-case")]
    enum Column {
        Id,
        BidSize,
        HTTPStatus,
        Leg2Price,
        // An explicit id wins over the rule; any text but an empty one.
        #[grid_column(id = "Last price")]
        LastPrice,
    }

    assert_eq!(
        ids::<Column>(),
        ["id", "bid-size", "http-status", "leg2-price", "Last price"]
    );
    // #endregion derive-rename
    assert_indices::<Column>();
}

#[test]
fn test_derive_discriminants() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
    #[repr(u8)]
    enum Column {
        A = 10,
        B = 3,
    }

    // Declaration order, not the discriminant (`self as u16` would give 10).
    assert_eq!(Column::A.ix(), ColumnIx::new(0));
    assert_eq!(Column::B.ix(), ColumnIx::new(1));
    assert_eq!((Column::A as u8, Column::B as u8), (10, 3));
    assert_indices::<Column>();
}

/// An application that re-exports the grid under its own path.
mod ui {
    pub use gpui_kit_grid as grid;
}

#[test]
fn test_derive_crate_path() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ui::grid::GridColumns)]
    #[grid_columns(crate = "ui::grid")]
    enum Column {
        Bid,
        Ask,
    }

    fn column_ids<C: ui::grid::GridColumns>() -> Vec<ui::grid::ColumnId> {
        C::all().iter().map(|column| column.column_id()).collect()
    }

    assert_eq!(column_ids::<Column>(), ["bid".into(), "ask".into()]);
    assert_indices::<Column>();
}

#[test]
fn test_derive_deprecated_and_cfg() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
    enum Column {
        Id,
        #[deprecated = "use `Price`"]
        Px,
        // Not in this build: not in `all()`, and no gap in the indices.
        #[cfg(any())]
        Venue,
        Price,
    }

    assert_eq!(ids::<Column>(), ["id", "px", "price"]);
    assert_indices::<Column>();
}

macro_rules! wide_columns {
    ($($variant:ident)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
        enum Wide {
            $($variant),*
        }
    };
}

wide_columns! {
    C0 C1 C2 C3 C4 C5 C6 C7 C8 C9 C10 C11 C12 C13 C14 C15
    C16 C17 C18 C19 C20 C21 C22 C23 C24 C25 C26 C27 C28 C29 C30 C31
    C32 C33 C34 C35 C36 C37 C38 C39 C40 C41 C42 C43 C44 C45 C46 C47
    C48 C49 C50 C51 C52 C53 C54 C55 C56 C57 C58 C59 C60 C61 C62 C63
    C64 C65 C66 C67 C68 C69 C70 C71 C72 C73 C74 C75 C76 C77 C78 C79
    C80 C81 C82 C83 C84 C85 C86 C87 C88 C89 C90 C91 C92 C93 C94 C95
    C96 C97 C98 C99 C100 C101 C102 C103 C104 C105 C106 C107 C108 C109 C110 C111
    C112 C113 C114 C115 C116 C117 C118 C119 C120 C121 C122 C123 C124 C125 C126 C127
}

#[test]
fn test_derive_128_columns() {
    assert_eq!(Wide::all().len(), 128);
    let last = Wide::all()[127];
    assert_eq!(last.ix(), ColumnIx::new(127));
    assert_eq!(last.id(), "c127");
    let mut set = ColumnSet::EMPTY;
    for column in Wide::all() {
        set.insert(column.ix());
    }
    assert_eq!(set, ColumnSet::first(128));
    assert_indices::<Wide>();
}

/// Derived ids in saved layouts. Needs a grid, so `test-support` too.
#[cfg(feature = "test-support")]
mod layout {
    use gpui_kit::{AppContext as _, TestAppContext, px};
    use gpui_kit_grid::{
        CellValue, ColumnId, GridColumn, GridColumns, GridLayout, GridRow, GridSchema,
        JsonLayoutCodec, LayoutCodec as _, LayoutColumn, LiveGridDelegate, LiveGridState,
        StoreOptions,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, GridColumns)]
    enum OrderColumn {
        Id,
        BidSize,
        #[grid_column(id = "px")]
        Price,
    }

    struct Order {
        id: u32,
    }

    impl GridRow for Order {
        type Key = u32;
        type Column = OrderColumn;

        fn key(&self) -> &u32 {
            &self.id
        }

        fn value(&self, column: OrderColumn) -> CellValue<'_> {
            match column {
                OrderColumn::Id => CellValue::Int(self.id.into()),
                OrderColumn::BidSize => CellValue::Int(100),
                OrderColumn::Price => CellValue::Float(1.5),
            }
        }
    }

    struct Orders;

    impl LiveGridDelegate for Orders {
        type Row = Order;
    }

    #[gpui_kit::test]
    fn test_derive_in_layout(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let schema = GridSchema::new()
            .column(GridColumn::new(OrderColumn::Id, "Id").with_width(px(60.)))
            .column(GridColumn::new(OrderColumn::BidSize, "Bid size").with_width(px(80.)))
            .column(GridColumn::new(OrderColumn::Price, "Price").with_width(px(90.)));
        let grid =
            cx.new(|cx| LiveGridState::new_standalone(schema, Orders, StoreOptions::default(), cx));
        let codec = JsonLayoutCodec::default();

        // The grid writes the derived ids, and they read back.
        let text = grid.read_with(cx, |grid, _| grid.export_layout()).unwrap();
        let written = codec.decode(&text).unwrap();
        let written_ids: Vec<&str> = written
            .columns()
            .iter()
            .map(|column| column.id().as_str())
            .collect();
        assert_eq!(written_ids, ["id", "bid_size", "px"]);

        // A layout of another build: price first and wider, a column this
        // enum no longer has, bid size hidden. The merge finds the columns
        // by their derived ids.
        let saved = GridLayout::new()
            .with_column(LayoutColumn::new(OrderColumn::Price.column_id(), 120.))
            .with_column(LayoutColumn::new("venue", 70.))
            .with_column(LayoutColumn::new("bid_size", 80.).with_visible(false));
        let text = codec.encode(&saved).unwrap();
        let report = grid
            .update(cx, |grid, cx| grid.import_layout(&text, cx))
            .unwrap();
        assert_eq!(report.ignored_columns(), [ColumnId::from("venue")]);
        assert_eq!(report.added_columns(), [OrderColumn::Id.column_id()]);
        grid.read_with(cx, |grid, _| {
            assert_eq!(
                grid.columns().collect::<Vec<_>>(),
                [OrderColumn::Price, OrderColumn::BidSize, OrderColumn::Id]
            );
            assert!(!grid.is_column_visible(OrderColumn::BidSize));
            assert_eq!(grid.column_width(OrderColumn::Price), px(120.));
        });
    }
}
