//! The grid body: one custom element that paints quads and glyphs directly,
//! with no element per cell.
//!
//! While the accessibility tree is built, the body also lays out, prepaints
//! and paints one element per painted row: a `Row` node whose cells are
//! synthetic leaves, and in the focused cell's row a real cell element that
//! claims the active descendant. They draw nothing and take no input; the
//! body has no id, so the row nodes are children of the grid root's `Grid`
//! (or `TreeGrid`) node. A group caption's row, and a tree row's with
//! children, answers a screen reader's `Expand` and `Collapse` as its
//! chevron would.
//!
//! Real elements in cells are cell hosts: an element of an `Element` or
//! `Control` column, an open editor. A row with hosts gets an element too,
//! the same one its accessibility row is, with up to three zone containers
//! that clip its hosts as the regions clip text, frozen zones over the
//! scrolling one. Without hosts and without accessibility the body lays out
//! no element at all.

use std::{
    mem,
    ops::Range,
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Duration,
};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::{
    AnyElement, App, Bounds, ContentMask, Corners, CursorStyle, DispatchPhase, Div, Element,
    ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, LineLayout, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, RenderImage, Role,
    ScrollWheelEvent, SharedString, Stateful, StatefulInteractiveElement as _, Style, Styled as _,
    TransformationMatrix, Window, div, fill, prelude::FluentBuilder as _, px, relative,
};

use crate::{
    model::ColumnIx,
    state::{A11yPlan, CellHover, ColumnZone, FOCUS_CELL_ID, LiveGridDelegate, LiveGridState},
    text::{GlyphArena, TextItem, paint_text},
};

/// The name of the focused cell's element id, made once
/// (`NamedChild(name, column id)` in the id scope of its row).
static FOCUS_CELL_NAME: LazyLock<Arc<ElementId>> =
    LazyLock::new(|| Arc::new(ElementId::Name(FOCUS_CELL_ID.into())));

/// The names of cell hosts' element ids, by layer, made once
/// (`NamedChild(name, column id)` in the id scope of their row).
static WIDGET_HOST_NAME: LazyLock<Arc<ElementId>> =
    LazyLock::new(|| Arc::new(ElementId::Name(HostLayer::Widget.id_name().into())));
static EDITOR_HOST_NAME: LazyLock<Arc<ElementId>> =
    LazyLock::new(|| Arc::new(ElementId::Name(HostLayer::Editor.id_name().into())));

/// The zones in the order their containers paint: frozen columns over the
/// scrolling ones, as the regions.
const ZONE_PAINT_ORDER: [ColumnZone; 3] = [
    ColumnZone::Scrolling,
    ColumnZone::FrozenLeft,
    ColumnZone::FrozenRight,
];

/// What a cell host holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HostLayer {
    /// The element of an `Element` or `Control` column.
    Widget,
    /// An open editor's field.
    Editor,
}

impl HostLayer {
    /// The name of the host's element id: a cell that turns from a widget
    /// into an editor does not share element state between the two.
    pub(crate) const fn id_name(self) -> &'static str {
        match self {
            Self::Widget => "live-grid-cell",
            Self::Editor => "live-grid-editor",
        }
    }
}

/// A real element in a cell, planned in the frame: the body places it in
/// its row's element, in its zone's container, over the cell.
pub(crate) struct CellHost {
    pub(crate) layer: HostLayer,
    pub(crate) column: ColumnIx,
    pub(crate) zone: ColumnZone,
    /// Where the host goes, in window coordinates: the cell, without a tree
    /// row's indent.
    pub(crate) bounds: Bounds<Pixels>,
    /// The host's element, made from [`CellHost::host_div`]. The body sizes
    /// and places it.
    pub(crate) element: Stateful<Div>,
}

impl CellHost {
    /// The div of a host: its id is `NamedChild(name, column id)` in the
    /// id scope of its row, so it keeps its element state with its row's
    /// key and its column through any re-sort.
    pub(crate) fn host_div(layer: HostLayer, column_id: &'static str) -> Stateful<Div> {
        let name = match layer {
            HostLayer::Widget => &WIDGET_HOST_NAME,
            HostLayer::Editor => &EDITOR_HOST_NAME,
        };
        div().id(ElementId::NamedChild(
            Arc::clone(name),
            SharedString::new_static(column_id),
        ))
    }
}

/// The hosts of one row: what its element is named and clipped to.
pub(crate) struct HostRow {
    /// The row element's id is `NamedInteger(id_name, key_hash)`, its
    /// accessibility row's.
    id_name: &'static str,
    key_hash: u64,
    /// The row's line across its band, in window coordinates.
    line: Bounds<Pixels>,
    /// What the row's element is clipped to: its band, or no area for an
    /// editor's row off screen (the field keeps the focus, the mouse finds
    /// nothing).
    clip: Bounds<Pixels>,
    hosts: Range<usize>,
}

/// The cell hosts of a frame, row by row.
#[derive(Default)]
pub(crate) struct HostPlan {
    rows: Vec<HostRow>,
    hosts: Vec<CellHost>,
    /// Each zone's left edge and width in window coordinates, by zone slot.
    zones: [(Pixels, Pixels); 3],
}

impl HostPlan {
    /// A plan over the zones, each one's left edge and width in window
    /// coordinates, by zone slot.
    pub(crate) fn new(zones: [(Pixels, Pixels); 3]) -> Self {
        Self {
            zones,
            ..Self::default()
        }
    }

    /// Room for `hosts` hosts, as many as the last frame had.
    pub(crate) fn reserve(&mut self, hosts: usize) {
        self.hosts.reserve(hosts);
    }

    /// The hosts planned so far.
    pub(crate) fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Returns true if no host is planned.
    pub(crate) fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Adds a host of the row being planned.
    pub(crate) fn push(&mut self, host: CellHost) {
        self.hosts.push(host);
    }

    /// Ends a row whose hosts began at `first` (the length before them):
    /// its element is `NamedInteger(id_name, key_hash)`, laid out along
    /// `line` and clipped to `clip`.
    pub(crate) fn end_row(
        &mut self,
        first: usize,
        id_name: &'static str,
        key_hash: u64,
        line: Bounds<Pixels>,
        clip: Bounds<Pixels>,
    ) {
        if self.hosts.len() > first {
            self.rows.push(HostRow {
                id_name,
                key_hash,
                line,
                clip,
                hosts: first..self.hosts.len(),
            });
        }
    }
}

/// An SVG icon painted in a region: a change arrow, a disclosure chevron.
pub(crate) struct IconItem {
    pub bounds: Bounds<Pixels>,
    /// The asset path, as `IconNamed::path` gives it.
    pub path: SharedString,
    pub color: Hsla,
}

/// An image painted in a region: a cell's `CellValue::Image`.
pub(crate) struct ImageItem {
    /// The room of the image, the cell without its padding: the image is
    /// clipped to it.
    pub bounds: Bounds<Pixels>,
    /// The image fitted to its room by the column's fit.
    pub image_bounds: Bounds<Pixels>,
    pub corner_radius: Pixels,
    pub data: Arc<RenderImage>,
}

/// Quads, text and icons of one clipped region: the columns of one zone
/// (frozen left, scrolling, frozen right) in one band of the body (the top
/// pinned rows, the data rows, the bottom pinned rows). Painted in this
/// order: backgrounds, the selection fill, grid lines, text and icons in
/// one layer, then the focus outline over it, all inside the region's mask.
/// Pinned rows are hit-tested by the state, before the data rows.
pub(crate) struct Region {
    mask: Bounds<Pixels>,
    /// Row and cell backgrounds (stripes, highlight, styles).
    pub quads: Vec<(Bounds<Pixels>, Hsla)>,
    /// The selection fill: one quad per run of selected rows or cells.
    pub fills: Vec<(Bounds<Pixels>, Hsla)>,
    /// Row and column lines.
    pub lines: Vec<(Bounds<Pixels>, Hsla)>,
    /// Rounded quads under the text: a group caption's tag, with its
    /// corner radius.
    pub pills: Vec<(Bounds<Pixels>, Hsla, Pixels)>,
    pub texts: Vec<TextItem>,
    /// Icons, after the text.
    pub icons: Vec<IconItem>,
    /// Images in cells, after the icons.
    pub images: Vec<ImageItem>,
    /// The focused cell's outline, after the layer.
    pub outline: Vec<(Bounds<Pixels>, Hsla)>,
}

impl Region {
    pub fn new(mask: Bounds<Pixels>) -> Self {
        Self {
            mask,
            quads: Vec::new(),
            fills: Vec::new(),
            lines: Vec::new(),
            pills: Vec::new(),
            texts: Vec::new(),
            icons: Vec::new(),
            images: Vec::new(),
            outline: Vec::new(),
        }
    }

    /// The bounds the region is clipped to.
    pub fn mask(&self) -> Bounds<Pixels> {
        self.mask
    }
}

/// Everything the body paints in one frame, planned in prepaint.
pub(crate) struct FramePlan {
    pub regions: Vec<Region>,
    /// Painted after the regions' layers (dividers, outlines).
    pub overlay: Vec<(Bounds<Pixels>, Hsla)>,
    pub ellipsis: Arc<LineLayout>,
    pub line_height: Pixels,
    /// Glyphs of every number in the frame (see [`GlyphArena`]).
    pub glyphs: GlyphArena,
    /// The hovered cell, if it has a tooltip.
    pub hover: Option<CellHover>,
    /// The accessibility tree of the body, while it is built.
    pub a11y: Option<Rc<A11yPlan>>,
    /// Real elements in cells, row by row.
    pub hosts: HostPlan,
    /// Where a drag over the rows would land: painted last, over the outlines.
    pub drop_indicator: Vec<(Bounds<Pixels>, Hsla)>,
}

pub(crate) struct BodyPrepaint {
    plan: FramePlan,
    hitbox: Hitbox,
    /// The transparent div over the hovered cell that carries its tooltip,
    /// laid out, prepainted and painted here as `uniform_list` does its
    /// items.
    hover: Option<AnyElement>,
    /// The elements of the rows, likewise: accessibility rows and rows with
    /// cell hosts, each with the mask a row with hosts is clipped to.
    rows: Vec<(AnyElement, Option<Bounds<Pixels>>)>,
}

/// The element of a planned row: its `Row` node carries the row's index
/// and selection, and its cells as synthetic leaves, written from the plan
/// when GPUI asks (only while the window's accessibility is active). The
/// focused cell, if in this row, is its real child, before the synthetic
/// ones. Laid out as a root at the row's visible part.
///
/// `zones` are the containers of the row's cell hosts; a host that took the
/// focused cell (`focus_taken`) is its real `GridCell`, so no focus element
/// is added.
fn a11y_row<D: LiveGridDelegate>(
    plan: &Rc<A11yPlan>,
    row_ix: usize,
    state: &Entity<LiveGridState<D>>,
    zones: Vec<AnyElement>,
    focus_taken: bool,
) -> (AnyElement, Bounds<Pixels>) {
    let row = &plan.rows()[row_ix];
    let bounds = row.bounds();
    let (content, key_hash) = (row.content(), row.key_hash());
    let focus = plan
        .focus()
        .filter(|focus| focus.row_ix() == row_ix && !focus_taken)
        .map(|_| a11y_focus_cell(plan, bounds.origin));
    let element = div()
        .id(ElementId::NamedInteger(
            SharedString::new_static(row.id_name()),
            row.key_hash(),
        ))
        .test_support()
        .role(Role::Row)
        .aria_row_index(row.row_index())
        // `write_row` sets it on the node too; here it is also what a
        // headless test observes.
        .when_some(row.selected(), |this, selected| {
            this.aria_selected(selected)
        })
        .when_some(row.level(), |this, level| this.aria_level(level))
        .when_some(row.expanded(), |this, expanded| {
            this.aria_expanded(expanded)
        })
        // One listener, for the one action the node offers: GPUI adds an
        // action to the node for each listener.
        .when_some(row.disclosure_action(), |this, action| {
            let state = state.clone();
            this.on_a11y_action(action, move |_, _, cx| {
                state.update(cx, |state, cx| {
                    state.on_a11y_disclosure(content, key_hash, action, cx)
                })
            })
        })
        .a11y_synthetic_children({
            let plan = plan.clone();
            move |builder| {
                let row = &plan.rows()[row_ix];
                plan.write_row(row, builder.parent_node());
                for cell in plan.cells(row) {
                    let id = builder.synthetic_node_id(cell.column());
                    builder.push_child(id, plan.cell_node(row, cell));
                }
            }
        })
        .w(bounds.size.width)
        .h(bounds.size.height)
        .children(focus)
        .children(zones)
        .into_any_element();
    (element, bounds)
}

/// The element of a row with cell hosts and no accessibility row: its id
/// and its zone containers, no node, no listener.
fn host_row_element(row: &HostRow, zones: Vec<AnyElement>, bounds: Bounds<Pixels>) -> AnyElement {
    div()
        .id(ElementId::NamedInteger(
            SharedString::new_static(row.id_name),
            row.key_hash,
        ))
        .test_support()
        .w(bounds.size.width)
        .h(bounds.size.height)
        .children(zones)
        .into_any_element()
}

/// The zone containers of a row element at `origin`: one per zone with
/// hosts, in [`ZONE_PAINT_ORDER`], each the row's height and its zone's
/// width, clipping its hosts (`overflow_hidden`). A widget host of the
/// focused cell, if `focus` plans it in this row, claims the active
/// descendant; returns whether one did.
fn host_zones(
    row: &HostRow,
    hosts: &mut [Option<CellHost>],
    zones: &[(Pixels, Pixels); 3],
    origin: Point<Pixels>,
    focus: Option<&Rc<A11yPlan>>,
) -> (Vec<AnyElement>, bool) {
    let focus_column = focus
        .and_then(|plan| plan.focus())
        .map(|focus| focus.cell().column());
    let mut focus_taken = false;
    let mut containers = Vec::new();
    for zone in ZONE_PAINT_ORDER {
        let (zone_x, zone_width) = zones[zone.slot()];
        let mut children = Vec::new();
        for slot in &mut hosts[row.hosts.clone()] {
            let Some(host) = slot.take_if(|host| host.zone == zone) else {
                continue;
            };
            let takes_focus = host.layer == HostLayer::Widget && Some(host.column) == focus_column;
            focus_taken |= takes_focus;
            let bounds = host.bounds;
            children.push(
                host.element
                    .when_some(focus.filter(|_| takes_focus), |this, plan| {
                        let plan = plan.clone();
                        this.role(Role::GridCell)
                            .aria_active_descendant()
                            .a11y_synthetic_children(move |builder| {
                                if let Some(focus) = plan.focus() {
                                    plan.write_focus(focus, builder.parent_node());
                                }
                            })
                    })
                    .absolute()
                    .left(bounds.origin.x - zone_x)
                    .top(bounds.origin.y - row.line.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .test_support()
                    .into_any_element(),
            );
        }
        if !children.is_empty() {
            containers.push(
                div()
                    .absolute()
                    .left(zone_x - origin.x)
                    .top(row.line.origin.y - origin.y)
                    .w(zone_width)
                    .h(row.line.size.height)
                    .overflow_hidden()
                    .children(children)
                    .into_any_element(),
            );
        }
    }
    (containers, focus_taken)
}

/// The elements of a frame's rows, laid out and prepainted: each
/// accessibility row, with the hosts of its row; then each other row with
/// hosts, such as an open editor's row off screen. A row with hosts is
/// clipped to its band; the mask comes with it for paint.
fn prepaint_rows<D: LiveGridDelegate>(
    plan: &mut FramePlan,
    state: &Entity<LiveGridState<D>>,
    window: &mut Window,
    cx: &mut App,
) -> Vec<(AnyElement, Option<Bounds<Pixels>>)> {
    let HostPlan {
        rows: host_rows,
        hosts,
        zones,
    } = mem::take(&mut plan.hosts);
    let mut hosts: Vec<Option<CellHost>> = hosts.into_iter().map(Some).collect();
    let mut placed_hosts = vec![false; host_rows.len()];
    let a11y_rows = plan.a11y.as_ref().map_or(0, |a11y| a11y.rows().len());
    let mut rows = Vec::with_capacity(a11y_rows + host_rows.len());
    let mut place = |element: AnyElement,
                     bounds: Bounds<Pixels>,
                     clip: Option<Bounds<Pixels>>,
                     window: &mut Window,
                     cx: &mut App| {
        let mut element = element;
        window.with_content_mask(clip.map(|bounds| ContentMask { bounds }), |window| {
            element.layout_as_root(bounds.size.into(), window, cx);
            element.prepaint_at(bounds.origin, window, cx);
        });
        rows.push((element, clip));
    };
    if let Some(a11y) = &plan.a11y {
        for (row_ix, row) in a11y.rows().iter().enumerate() {
            let hosted = host_rows.iter().position(|host_row| {
                (host_row.id_name, host_row.key_hash) == (row.id_name(), row.key_hash())
            });
            let (zones, focus_taken, clip) = match hosted {
                Some(ix) => {
                    placed_hosts[ix] = true;
                    let focus = a11y
                        .focus()
                        .filter(|focus| focus.row_ix() == row_ix)
                        .map(|_| a11y);
                    let host_row = &host_rows[ix];
                    let (containers, focus_taken) =
                        host_zones(host_row, &mut hosts, &zones, row.bounds().origin, focus);
                    (containers, focus_taken, Some(host_row.clip))
                }
                None => (Vec::new(), false, None),
            };
            let (element, bounds) = a11y_row(a11y, row_ix, state, zones, focus_taken);
            place(element, bounds, clip, window, cx);
        }
    }
    for (host_row, _) in host_rows
        .iter()
        .zip(&placed_hosts)
        .filter(|(_, placed)| !**placed)
    {
        // The visible part of the row; off screen, the row itself under a
        // mask of no area.
        let visible = host_row.line.intersect(&host_row.clip);
        let bounds = if visible.size.width > px(0.) && visible.size.height > px(0.) {
            visible
        } else {
            host_row.line
        };
        let (containers, _) = host_zones(host_row, &mut hosts, &zones, bounds.origin, None);
        place(
            host_row_element(host_row, containers, bounds),
            bounds,
            Some(host_row.clip),
            window,
            cx,
        );
    }
    rows
}

/// The focused cell's element in its row's element at `row_origin`: no
/// style, no listeners, no hitbox; a `GridCell` node that claims the
/// active descendant (GPUI honors it while the grid root holds the focus).
/// Its id is by column within the row's id scope, so a re-sort keeps the
/// node and a move to another cell makes a new one.
fn a11y_focus_cell(plan: &Rc<A11yPlan>, row_origin: Point<Pixels>) -> AnyElement {
    let focus = plan.focus().expect("the row holds the focused cell");
    let bounds = focus.cell().bounds();
    div()
        .id(ElementId::NamedChild(
            FOCUS_CELL_NAME.clone(),
            SharedString::new_static(focus.column_id()),
        ))
        .test_support()
        .role(Role::GridCell)
        .aria_active_descendant()
        .a11y_synthetic_children({
            let plan = plan.clone();
            move |builder| {
                if let Some(focus) = plan.focus() {
                    plan.write_focus(focus, builder.parent_node());
                }
            }
        })
        .absolute()
        .left(bounds.origin.x - row_origin.x)
        .top(bounds.origin.y - row_origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
        .into_any_element()
}

/// The grid body element.
pub(crate) struct GridBody<D: LiveGridDelegate> {
    state: Entity<LiveGridState<D>>,
}

impl<D: LiveGridDelegate> GridBody<D> {
    pub fn new(state: Entity<LiveGridState<D>>) -> Self {
        Self { state }
    }
}

impl<D: LiveGridDelegate> IntoElement for GridBody<D> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<D: LiveGridDelegate> Element for GridBody<D> {
    type RequestLayoutState = ();
    type PrepaintState = BodyPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.flex_grow = 1.;
        style.flex_shrink = 1.;
        style.flex_basis = relative(0.).into();
        style.size.width = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // Style hooks run here, with the state borrowed only for planning;
        // paint below touches the plan, never the state.
        let mut plan = self
            .state
            .update(cx, |state, cx| state.plan_frame(bounds, window, cx));
        // After the body's hitbox, so the div's hitbox is above it; both
        // are `Normal`, so the body still sees the pointer.
        let hover = plan.hover.take().map(|hover| {
            let bounds = hover.bounds();
            let mut element = hover.into_element();
            element.layout_as_root(bounds.size.into(), window, cx);
            element.prepaint_at(bounds.origin, window, cx);
            element
        });
        // Then the rows' elements, so cell hosts are over the body and the
        // tooltip's div.
        let hosts_started = (!plan.hosts.is_empty()).then(web_time::Instant::now);
        let rows = prepaint_rows(&mut plan, &self.state, window, cx);
        if let Some(started) = hosts_started {
            let elapsed: Duration = started.elapsed();
            self.state
                .update(cx, |state, _| state.set_last_hosts_prepaint(elapsed));
        }
        BodyPrepaint {
            plan,
            hitbox,
            hover,
            rows,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let plan = &prepaint.plan;
        for region in &plan.regions {
            // A zone or a pinned band with no room this frame.
            if region.mask.size.width <= px(0.) || region.mask.size.height <= px(0.) {
                continue;
            }
            window.with_content_mask(
                Some(ContentMask {
                    bounds: region.mask,
                }),
                |window| {
                    // One layer per region: quads skip the scene's R-tree, and all
                    // quads of the layer draw before its glyphs.
                    window.paint_layer(region.mask, |window| {
                        for (bounds, color) in region
                            .quads
                            .iter()
                            .chain(&region.fills)
                            .chain(&region.lines)
                        {
                            window.paint_quad(fill(*bounds, *color));
                        }
                        for (bounds, color, radius) in &region.pills {
                            window.paint_quad(fill(*bounds, *color).corner_radii(*radius));
                        }
                        for item in &region.texts {
                            paint_text(
                                item,
                                &plan.glyphs,
                                item.ellipsis.as_deref().unwrap_or(&plan.ellipsis),
                                plan.line_height,
                                window,
                            );
                        }
                        // A missing asset leaves its icon out.
                        for icon in &region.icons {
                            let _ = window.paint_svg(
                                icon.bounds,
                                icon.path.clone(),
                                None,
                                TransformationMatrix::unit(),
                                icon.color,
                                cx,
                            );
                        }
                        // Images in cells, each clipped to its room.
                        for image in &region.images {
                            let _ = window.paint_image(
                                image.bounds,
                                image.image_bounds,
                                Corners::all(image.corner_radius),
                                image.data.clone(),
                                0,
                                false,
                            );
                        }
                    });
                },
            );
        }
        for (bounds, color) in &plan.overlay {
            window.paint_quad(fill(*bounds, *color));
        }
        if let Some(hover) = &mut prepaint.hover {
            hover.paint(window, cx);
        }
        // The rows' elements, cell hosts among them: over the regions,
        // under the focused cell's outline.
        for (row, clip) in &mut prepaint.rows {
            window.with_content_mask(clip.map(|bounds| ContentMask { bounds }), |window| {
                row.paint(window, cx);
            });
        }
        let mut outlines = 0;
        for region in &plan.regions {
            if region.outline.is_empty() {
                continue;
            }
            window.with_content_mask(
                Some(ContentMask {
                    bounds: region.mask,
                }),
                |window| {
                    for (bounds, color) in &region.outline {
                        window.paint_quad(fill(*bounds, *color));
                    }
                },
            );
            outlines += region.outline.len();
        }
        // The drop indicator, over everything in the body.
        for (bounds, color) in &plan.drop_indicator {
            window.paint_quad(fill(*bounds, *color));
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            let mut order = vec!["regions"];
            order.extend((!plan.overlay.is_empty()).then_some("dividers"));
            order.extend(prepaint.hover.is_some().then_some("tooltip"));
            order.extend((!prepaint.rows.is_empty()).then_some("rows"));
            order.extend((outlines > 0).then_some("outlines"));
            order.extend((!plan.drop_indicator.is_empty()).then_some("drop"));
            self.state
                .update(cx, |state, _| state.set_last_paint_order(order));
        }
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = outlines;

        // A link is the one place a pointer cursor is right.
        if self.state.read(cx).is_link_hovered() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }
        let hitbox = prepaint.hitbox.clone();
        let state = self.state.clone();
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let state = state.clone();
            move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                    state.update(cx, |state, cx| state.on_scroll_wheel(event.delta, cx));
                    cx.stop_propagation();
                }
            }
        });
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let state = state.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    let inside = hitbox.is_hovered(window);
                    state.update(cx, |state, cx| {
                        state.set_pointer_inside(inside, cx);
                        state.set_hover_position(inside.then_some(event.position), cx);
                        // Past the drag threshold a press is no click.
                        state.on_click_pointer_move(event.position);
                        // A drag-selection follows the pointer anywhere in
                        // the window.
                        if event.pressed_button == Some(MouseButton::Left) {
                            state.on_body_drag_move(event.position, cx);
                        }
                    });
                }
            }
        });
        window.on_mouse_event({
            let state = state.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                // GPUI hides a shown tooltip on any press, in the grid or not.
                // A new press ends the last one's click; the body and the
                // header start their own in the bubble phase.
                if phase == DispatchPhase::Capture {
                    state.update(cx, |state, cx| {
                        state.on_window_mouse_down(event.position, window, cx);
                        state.cancel_click();
                    });
                } else if hitbox.is_hovered(window) {
                    state.update(cx, |state, cx| state.on_body_mouse_down(event, window, cx));
                }
            }
        });
        // A column gesture (resize, header drag) and a press on the body end
        // wherever the button goes up, in or out of the grid; then a click
        // of any button, after the press's selection events.
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Capture {
                state.update(cx, |state, cx| {
                    if event.button == MouseButton::Left {
                        state.on_column_mouse_up(cx);
                        state.on_body_mouse_up(cx);
                        // A drop target places the drop itself, later in
                        // this release.
                        state.end_drop_hover(cx);
                    }
                    state.on_click_mouse_up(event, cx);
                });
            }
        });
    }
}
