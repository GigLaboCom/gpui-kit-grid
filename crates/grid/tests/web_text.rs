//! Headless UI tests of text the grid lays out where GPUI's default font
//! would apply (`.agents/COMPONENT_TEST_RULES.md`). They run on a text
//! system that knows one family, as GPUI's web platform knows only the
//! faces a page loaded, and where laying out text in any other family,
//! GPUI's default `.SystemUIFont` among them, panics. The window places the
//! grid in that family.
//!
//! - Drag ghosts: GPUI draws the view of a drag outside the window's root
//!   element, so a ghost inherits no font from where its drag started. A
//!   header, a group level, a column chooser row and rows are dragged, and
//!   each ghost draws.
//! - Editors: a field created outside a frame takes the window's default
//!   font until its first frame, so its text must not be laid out before
//!   then. The built-in editors open by typing and by F2, and a field takes
//!   a value from the store.

mod common;

use std::{borrow::Cow, io, sync::Arc, time::Duration};

use gpui_kit::component::Sizable as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Context, DevicePixels, ElementId, Entity, Font,
    FontId, FontMetrics, FontRun, GlyphId, IntoElement, LineLayout, Modifiers, MouseButton,
    NoopTextSystem, ParentElement as _, Pixels, PlatformTextSystem, Point, Render,
    RenderGlyphParams, Result, Size, Styled as _, TestAppContext, TextRenderingMode, Window,
    WindowBounds, WindowOptions, div, point, prelude::FluentBuilder as _, px, size,
    test::TestWindowExt as _,
};
use gpui_kit_grid::{
    ColumnChooserPanel, DateEditor, FollowLiveValue, GridRow, GridSchema, GroupBy, LiveGrid,
    LiveGridDelegate, LiveGridHandle, LiveGridState, OwnedValue, SelectEditor, SelectOption,
    SelectedRowsDrag, SelectionMode, StoreOptions,
};

use crate::common::{
    EditFixture, Grouped, GroupedColumn, GroupedFixture, OrderColumn, Orders, ROW, drag_move_to,
    grouped_cell, grouped_rows, grouped_schema, mouse_down_at, mouse_up_at, order_schema, orders,
    press_at, with_order_editors,
};

/// The one family the text system knows.
const FAMILY: &str = "Grid Test Sans";
/// The grid's width; the column chooser is right of it.
const GRID_WIDTH: f32 = 800.;
const WIDTH: f32 = GRID_WIDTH + 300.;
/// The group panel, the header and 14 `xsmall` rows.
const HEIGHT: f32 = ROW * 16.;

/// GPUI's no-op text system, resolving [`FAMILY`] alone.
struct OneFamily(NoopTextSystem);

impl PlatformTextSystem for OneFamily {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.add_fonts(fonts)
    }

    /// None installed, as on the web before a page adds its faces:
    /// gpui-kit's theme then keeps `.SystemUIFont` as it is.
    fn all_font_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        if font.family.as_ref() == FAMILY {
            self.0.font_id(font)
        } else {
            Err(io::Error::other(format!("no font family {}", font.family)).into())
        }
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.0.font_metrics(font_id)
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        self.0.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.glyph_raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.layout_line(text, font_size, runs)
    }

    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.0.recommended_rendering_mode(font_id, font_size)
    }
}

/// An application on [`OneFamily`], on the executor of the test's `cx`.
fn one_family_app(cx: &TestAppContext) -> TestAppContext {
    TestAppContext::build_with_text_system(
        cx.dispatcher.clone(),
        None,
        Arc::new(OneFamily(NoopTextSystem::new())),
    )
}

/// Quits an application of [`one_family_app`].
fn quit(cx: &mut TestAppContext) {
    cx.update(|cx| cx.quit());
    cx.run_until_parked();
}

/// A grid with its group panel, and a column chooser beside it if there is
/// one, all in [`FAMILY`].
struct FamilyRoot<D: LiveGridDelegate> {
    grid: Entity<LiveGridState<D>>,
    chooser: Option<Entity<ColumnChooserPanel>>,
}

impl<D: LiveGridDelegate> Render for FamilyRoot<D> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .font_family(FAMILY)
            .child(
                div().w(px(GRID_WIDTH)).h_full().child(
                    LiveGrid::new(&self.grid)
                        .xsmall()
                        .bordered(false)
                        .group_panel(self.chooser.is_some()),
                ),
            )
            .when_some(self.chooser.clone(), |this, chooser| this.child(chooser))
    }
}

/// A grid of `schema` holding `rows` in a window of `cx`, configured by
/// `configure`; with the group panel and a column chooser if `chooser`.
fn open<D: LiveGridDelegate>(
    cx: &mut TestAppContext,
    schema: GridSchema<<D::Row as GridRow>::Column>,
    rows: Vec<D::Row>,
    delegate: D,
    chooser: bool,
    configure: impl FnOnce(LiveGridState<D>) -> LiveGridState<D> + 'static,
) -> (AnyWindowHandle, Entity<LiveGridState<D>>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        gpui_kit_grid::init(cx);
    });
    let (window, root) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    let grid = cx.new(|cx| {
                        configure(LiveGridState::new_standalone(
                            schema,
                            delegate,
                            StoreOptions::default(),
                            cx,
                        ))
                    });
                    let chooser = chooser.then(|| {
                        let handle = LiveGridHandle::new(&grid);
                        cx.new(|cx| ColumnChooserPanel::new(&handle, window, cx))
                    });
                    cx.new(|_| FamilyRoot { grid, chooser })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    (window, grid)
}

/// Presses at `from`, drags in steps to `to` (each step draws a frame, the
/// ghost in it once the drag started), checks that a drag is on, and
/// releases where it started.
fn drag(fixture: &GroupedFixture, cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>) {
    fixture.input(cx, |window, cx| {
        mouse_down_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
        for step in 1..=4 {
            let at = from + (to - from) * (step as f32 / 4.);
            drag_move_to(window, at, cx);
        }
    });
    assert!(cx.update(|cx| cx.has_active_drag()), "a drag from {from:?}");
    fixture.input(cx, |window, cx| {
        drag_move_to(window, from, cx);
        mouse_up_at(window, from, MouseButton::Left, Modifiers::default(), 1, cx);
    });
    fixture.settle(cx);
    assert!(!cx.update(|cx| cx.has_active_drag()));
}

/// The centre of the bounds `find` reads from the window.
fn center(
    fixture: &GroupedFixture,
    cx: &mut TestAppContext,
    find: impl FnOnce(&mut Window) -> Bounds<Pixels>,
) -> Point<Pixels> {
    cx.update_window(fixture.window, |_, window, _| find(window).center())
        .expect("window is open")
}

#[gpui_kit::test]
fn test_drag_ghosts_draw_in_the_grid_font(cx: &mut TestAppContext) {
    let mut web = one_family_app(cx);
    let cx = &mut web;
    // 24 trades grouped by instrument, their rows dragged by
    // `SelectedRowsDrag`.
    let (window, grid) = open(
        cx,
        grouped_schema(),
        grouped_rows(24),
        Grouped,
        true,
        |grid| {
            grid.with_selection_mode(SelectionMode::RowSet)
                .with_row_drag_gesture(SelectedRowsDrag::new())
        },
    );
    grid.update(cx, |grid, cx| {
        grid.set_group_by([GroupBy::column(GroupedColumn::Instrument)], cx)
    });
    let fixture = GroupedFixture {
        window,
        grid,
        events: Default::default(),
    };
    fixture.settle(cx);

    // A header: Price, under the group panel's row.
    let price = grouped_cell(0, GroupedColumn::Price);
    let header = point(price.x, px(ROW * 1.5));
    drag(&fixture, cx, header, header - point(px(40.), px(0.)));

    // A group level: the Instrument chip in the panel.
    let chip = ElementId::NamedChild(
        Arc::new(ElementId::Name("live-grid-group-chip".into())),
        "instrument".into(),
    );
    let chip = center(&fixture, cx, |window| window.find(chip).bounds());
    drag(&fixture, cx, chip, chip + point(px(40.), px(0.)));

    // A row of the chooser, by its grip.
    let row = ElementId::NamedChild(
        Arc::new(ElementId::Name("chooser-column".into())),
        "price".into(),
    );
    let grip = center(&fixture, cx, |window| {
        window
            .within("live-grid-column-chooser")
            .within(row)
            .find("chooser-grip")
            .bounds()
    });
    drag(&fixture, cx, grip, grip + point(px(0.), px(ROW * 2.)));

    // Rows: a selected data row, under the first caption.
    let line = point(price.x, px(ROW * 3.5));
    fixture.input(cx, |window, cx| {
        press_at(window, line, MouseButton::Left, Modifiers::default(), 1, cx)
    });
    fixture.settle(cx);
    assert_eq!(
        fixture.grid.read_with(cx, |grid, _| grid.selection().len()),
        1
    );
    drag(&fixture, cx, line, line + point(px(0.), px(ROW * 3.)));

    quit(cx);
}

/// The text of `value` as a select option.
fn text(value: &str) -> OwnedValue {
    OwnedValue::Text(value.into())
}

#[gpui_kit::test]
fn test_editors_lay_out_in_the_grid_font(cx: &mut TestAppContext) {
    use OrderColumn::{Instrument, Placed, Price, Quantity, Side};
    let mut web = one_family_app(cx);
    let cx = &mut web;
    // Every built-in editor; an untouched draft follows the store.
    let (window, grid) = open(cx, order_schema(), orders(20), Orders, false, |grid| {
        with_order_editors(grid)
            .with_editor(
                Side,
                SelectEditor::new([
                    SelectOption::new(text("Buy"), "Buy"),
                    SelectOption::new(text("Sell"), "Sell"),
                ]),
            )
            .with_editor(Placed, DateEditor::date_time())
            .with_edit_policy(FollowLiveValue)
    });
    let fixture = EditFixture {
        window,
        grid,
        events: Default::default(),
    };
    fixture.settle(cx);

    // Each editor opened by typing (the calendar opens by F2 only), and
    // by F2, then cancelled.
    for (column, typed) in [
        (Instrument, Some("Z")),
        (Quantity, Some("7")),
        (Side, Some("s")),
        (Placed, None),
    ] {
        fixture.focus_cell(cx, 3, column);
        if let Some(typed) = typed {
            fixture.type_text(cx, typed);
            fixture.settle(cx);
            assert_eq!(fixture.editing(cx), Some((3, column)), "typing {typed:?}");
            fixture.press_key(cx, "escape");
            fixture.settle(cx);
        }
        fixture.press_key(cx, "f2");
        fixture.settle(cx);
        assert_eq!(fixture.editing(cx), Some((3, column)), "F2");
        fixture.press_key(cx, "escape");
        fixture.settle(cx);
    }
    // The text that opened the list shows in its search field.
    fixture.focus_cell(cx, 3, Side);
    fixture.type_text(cx, "s");
    fixture.settle(cx);
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("s"));
    fixture.press_key(cx, "enter");
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), None);

    // The store changes the price under an open field: the untouched
    // draft takes it, laid out in the cell's font.
    fixture.focus_cell(cx, 3, Price);
    fixture.press_key(cx, "f2");
    let mut order = orders(20).remove(2);
    order.price = 321.5;
    fixture.writer(cx).upsert(Arc::new(order));
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    fixture.settle(cx);
    assert_eq!(fixture.editing(cx), Some((3, Price)));
    assert_eq!(fixture.editor_text(cx).as_deref(), Some("321.5"));
    fixture.press_key(cx, "escape");
    fixture.settle(cx);

    quit(cx);
}
