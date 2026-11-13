//! gpui-kit-grid: a live-updating data grid for gpui-kit applications.
//!
//! The core: a background ingest that owns the rows, background projection
//! rebuilds, a custom-painted body with a colour-free text cache, and change
//! highlight on a global tick.
//!
//! ```text
//! GridWriter (any thread) → ingest task → StoreSnapshot → LiveGridState (view)
//!                                                       → Rebuild (background) → Projection
//! ```

// GPUI's `Action` derive spells its paths `gpui::`; the facade resolves them
// through this alias (gpui-kit's `crates/kit/src/lib.rs`).
extern crate gpui_kit as gpui;

pub mod actions;
mod element;
mod engine;
mod export;
mod grid;
mod highlight;
mod model;
mod persist;
mod policy;
mod quick_filter;
mod state;
mod store;
mod text;

pub use actions::init;
pub use engine::{
    Aggregate, AggregateFn, AggregateOrder, AggregateRow, Branch, DisplayRow, EveryGroupFooter,
    ExpandDepth, Expansion, GroupBy, GroupFooter, GroupId, GroupInfo, GroupKey, GroupOrder,
    GroupOrderContext, GroupPath, GroupTable, KeyOrder, NoGroupFooter, Projection,
    ProjectionEngine, Rebuild, SortDirection, TreeFilter, TreeFilterContext, TreeFilterRule,
    TreeSpec, TreeTotals, TreeTotalsContext, TreeTotalsRule, ViewSpec,
};
pub use export::{
    CellExport, CellExporter, CsvEncoding, CsvFormat, DisplayedCells, ExportCell, ExportColumn,
    ExportControl, ExportError, ExportFormat, ExportLine, ExportLines, ExportRow, ExportScope,
    ExportTable, Exporter, LineFilter, NumberHint, NumberPrecision, RawCells, TextFile,
    TsvExporter,
};
/// Derives [`GridColumns`](trait@GridColumns) for an enum of columns (feature
/// `derive`).
#[cfg(feature = "derive")]
pub use gpui_kit_grid_macros::GridColumns;
pub use grid::LiveGrid;
pub use highlight::{ChangeDirection, HighlightPolicy, RowChange, RowStamps, Stamp};
pub use model::{
    CellValue, ColumnId, ColumnIx, ColumnSet, GridColumns, GridRow, GridValue, IconValue,
    ImageValue, LinkValue, NumberFormat, OwnedValue,
};
pub use persist::{
    AppendNewColumns, GridLayout, JsonLayoutCodec, LayoutCodec, LayoutColumn, LayoutError,
    LayoutExpansion, LayoutGroup, LayoutLibrary, LayoutMerge, LayoutMergeContext, LayoutReport,
    LayoutSort,
};
pub use policy::{
    AfterCommit, AutofitContext, AutofitPolicy, BackgroundRunner, BandedMove, CellMove,
    ColumnMoveContext, ColumnMovePolicy, ColumnOrder, ContextMenuGesture,
    ContextMenuGestureContext, DraftAction, DropBetweenRows, DropLine, DropOnRows, EditCommitKey,
    EditEnd, EditGesture, EditGestureContext, EditPolicy, EditPolicyContext, ExpandArea,
    ExpandEdit, ExpandGesture, ExpandGestureContext, FocusBranches, FocusCaptions, FollowLiveValue,
    FreeMove, GestureInput, GridArea, HoldReorder, InlineRunner, LineScopeContext, LiveReorder,
    ManualReorder, NoRowDrag, NoRowDrop, ProjectionScope, ReorderContext, ReorderDecision,
    ReorderPolicy, RowDragGesture, RowDragPressContext, RowDropContext, RowDropPolicy,
    SelectableLine, SelectedRowsDrag, SelectionEdit, SelectionGesture, SelectionGestureContext,
    SelectionPolicy, SelectionScope, SelectionScopeContext, SortCycle, SortCycleContext,
    StandardContextMenuGesture, StandardEditGesture, StandardEditPolicy, StandardExpandGesture,
    StandardSelectionGesture, StandardSortCycle, StoreScope, TaskRunner, VisibleRowsAutofit,
};
pub use quick_filter::{
    CellTexts, ContainsAllTerms, ContainsAllTermsQuery, QuickFilter, QuickFilterMatcher,
    QuickFilterQuery, QuickFilterRule,
};
pub use state::{
    AggregateLine, AggregateStyleContext, AssetImageSource, BandInfo, CaptionTag, CellChange,
    CellContent, CellEditHandle, CellEditor, CellElementContext, CellImage, CellImageSource,
    CellImages, CellStyle, CellStyleContext, CellTooltip, CellTooltipContext, ChangePaint,
    ChangePainter, ChangePalette, CheckboxEditor, ColumnAlign, ColumnBand, ColumnChooserContext,
    ColumnChooserFactory, ColumnChooserPanel, ColumnInfo, ColumnZone, DateEditor,
    DirectionalChanges, DragPayload, DropPlace, DropSource, DropTarget, EditCancelReason,
    EditError, EditId, EditInput, EditMessageContext, EditOrigin, EditState, EditValidateContext,
    EditableContext, EditorCell, EditorField, EditorMark, EditorOpen, EditorStart, EmptyExport,
    ExpandTarget, ExportHeader, ExportJob, ExportOutcome, ExportProgress, ExportRequest,
    FlushOutcome, FocusedCell, GridColumn, GridHandleEvent, GridMenuContext, GridSchema,
    GridSelection, GridStats, GridTarget, GridTuning, GroupCaptionContext, GroupOrigin,
    HeaderLabel, HeaderTooltipContext, ImageFit, LayoutOrigin, LayoutPart, LayoutParts, LinkOpener,
    LiveGridDelegate, LiveGridEvent, LiveGridHandle, LiveGridState, MIN_COLUMN_WIDTH, NumberEditor,
    NumberKind, OpenInBrowser, PopoverColumnChooser, ProgressContext, RowDrag, RowDragContext,
    ScopedSelection, SelectEditor, SelectOption, SelectionMode, SelectionOrigin, SortOrigin,
    TextEditor, UniformChanges, ViewColumnIx, WeakLiveGridHandle,
};
pub use store::{
    ChangeSet, Clock, GridProgress, GridStore, GridStoreBuilder, GridWriter, Ingest, Op, PAGE_SIZE,
    PinnedEdge, Reload, ReloadPolicy, ReplayLive, RowId, StoreOptions, StoreSnapshot, StoreStats,
    StoreSubscription, WriteBatch, WriteCoalescing,
};
pub use text::TextStats;

rust_i18n::i18n!("locales", fallback = "en");

#[cfg(test)]
mod tests {
    use rust_i18n::t;

    /// Every key of `locales/live_grid.yml` with its English text.
    const CATALOG: &[(&str, &str)] = &[
        ("live_grid.menu.copy", "Copy"),
        ("live_grid.menu.copy_all", "Copy all"),
        ("live_grid.menu.sort", "Sort"),
        ("live_grid.menu.sort_ascending", "Sort ascending"),
        ("live_grid.menu.sort_descending", "Sort descending"),
        ("live_grid.menu.clear_sort", "Clear sort"),
        ("live_grid.menu.clear_all_sorting", "Clear all sorting"),
        ("live_grid.menu.column", "Column"),
        ("live_grid.menu.autofit_column", "Autofit column"),
        ("live_grid.menu.autofit_columns", "Autofit all columns"),
        ("live_grid.menu.hide_column", "Hide column"),
        ("live_grid.menu.show_all_columns", "Show all columns"),
        ("live_grid.menu.choose_columns", "Choose columns…"),
        ("live_grid.menu.freeze_left", "Freeze left"),
        ("live_grid.menu.freeze_right", "Freeze right"),
        ("live_grid.menu.unfreeze", "Unfreeze"),
        ("live_grid.menu.reset_columns", "Reset columns"),
        ("live_grid.sort.ascending", "Sorted ascending"),
        ("live_grid.sort.descending", "Sorted descending"),
        (
            "live_grid.sort.ascending_priority",
            "Sorted ascending, priority %{priority}",
        ),
        (
            "live_grid.sort.descending_priority",
            "Sorted descending, priority %{priority}",
        ),
        ("live_grid.progress.loading", "Loading…"),
        ("live_grid.menu.group_by", "Group by this column"),
        ("live_grid.menu.ungroup", "Ungroup"),
        ("live_grid.menu.clear_grouping", "Ungroup all"),
        ("live_grid.menu.move_left", "Move left"),
        ("live_grid.menu.move_right", "Move right"),
        ("live_grid.group_panel.label", "Grouping"),
        (
            "live_grid.group_panel.hint",
            "Drag a column header here to group by it",
        ),
        (
            "live_grid.group_panel.tree",
            "Grouping is off in the tree view",
        ),
        ("live_grid.group_panel.chip", "%{title}, %{direction}"),
        (
            "live_grid.group_panel.remove",
            "Remove grouping by %{title}",
        ),
        // Group captions, their menu and aggregates.
        ("live_grid.group.caption", "%{title}: %{value} (%{count})"),
        ("live_grid.group.blank", "(Blank)"),
        ("live_grid.group.true", "Yes"),
        ("live_grid.group.false", "No"),
        ("live_grid.menu.expand", "Expand"),
        ("live_grid.menu.collapse", "Collapse"),
        ("live_grid.menu.expand_all", "Expand all"),
        ("live_grid.menu.collapse_all", "Collapse all"),
        ("live_grid.menu.captions_only", "Captions only"),
        ("live_grid.aggregate.sum", "Sum"),
        ("live_grid.aggregate.avg", "Average"),
        ("live_grid.aggregate.min", "Min"),
        ("live_grid.aggregate.max", "Max"),
        ("live_grid.aggregate.count", "Count"),
        ("live_grid.aggregate.tooltip", "%{name}: %{value}"),
        // The totals row.
        ("live_grid.totals.label", "Total"),
        // Accessibility texts.
        ("live_grid.a11y.up", "rising"),
        ("live_grid.a11y.down", "falling"),
        ("live_grid.a11y.expanded", "Expanded"),
        ("live_grid.a11y.collapsed", "Collapsed"),
        // The column chooser.
        ("live_grid.chooser.label", "Columns"),
        ("live_grid.chooser.search", "Search columns"),
        ("live_grid.chooser.frozen_left", "Frozen left"),
        ("live_grid.chooser.scrolling", "Scrollable"),
        ("live_grid.chooser.frozen_right", "Frozen right"),
        ("live_grid.chooser.band_shown", "%{shown} of %{count}"),
        ("live_grid.chooser.no_match", "No columns match"),
        ("live_grid.chooser.show_all", "Show all"),
        ("live_grid.chooser.move", "Move %{title}"),
        // The messages of the built-in editors.
        ("live_grid.edit.not_a_number", "Enter a number"),
        ("live_grid.edit.not_a_whole_number", "Enter a whole number"),
        (
            "live_grid.edit.too_many_decimals",
            "Use at most %{scale} decimals",
        ),
        ("live_grid.edit.below_min", "Enter %{min} or more"),
        ("live_grid.edit.above_max", "Enter %{max} or less"),
        ("live_grid.edit.too_long", "Use at most %{count} characters"),
        // The list of a select editor.
        ("live_grid.edit.search", "Search"),
        ("live_grid.edit.no_matches", "No matches"),
        // The ghost of a row drag.
        ("live_grid.drag.row", "1 row"),
        ("live_grid.drag.rows", "%{count} rows"),
        // Editing under live updates.
        (
            "live_grid.edit.conflict",
            "Changed to %{value} while you edited",
        ),
        ("live_grid.edit.pending", "Saving…"),
        ("live_grid.edit.failed", "Not saved: %{message}"),
        ("live_grid.a11y.saving", "saving"),
        ("live_grid.a11y.not_saved", "not saved"),
        ("live_grid.a11y.link", "link"),
    ];

    #[test]
    fn test_locale_catalog() {
        // `ru` is added only if the product turns out to be Russian-speaking.
        assert_eq!(rust_i18n::available_locales!(), ["en", "zh-CN", "zh-HK"]);
        for &(key, english) in CATALOG {
            assert_eq!(t!(key, locale = "en"), english);
            for locale in ["zh-CN", "zh-HK"] {
                // A missing translation falls back to English.
                let text = t!(key, locale = locale);
                assert_ne!(text, english, "{key} has no {locale} text");
                for placeholder in [
                    "%{priority}",
                    "%{title}",
                    "%{direction}",
                    "%{value}",
                    "%{count}",
                    "%{name}",
                    "%{scale}",
                    "%{min}",
                    "%{max}",
                    "%{message}",
                    "%{shown}",
                ] {
                    assert_eq!(
                        text.contains(placeholder),
                        english.contains(placeholder),
                        "{key} in {locale}: {placeholder}"
                    );
                }
            }
        }
        for locale in ["en", "zh-CN", "zh-HK"] {
            let text = t!(
                "live_grid.sort.descending_priority",
                locale = locale,
                priority = 2
            );
            assert!(text.ends_with(" 2"), "{locale}: {text}");
            assert!(!text.contains("..."), "{locale}: {text}");
        }
        assert!(!t!("live_grid.progress.loading", locale = "en").contains("..."));
    }
}
