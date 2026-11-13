//! Headless UI tests of images in cells: an image fitted to its cell, loading
//! then ready, a missing image, the text of an image cell, the image source
//! as a seam, the cache's capacity, a colour SVG, and autofit. Small data:
//! three tiny PNG files and one SVG written to the test's own directory and
//! loaded as files.

mod common;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::Size;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Pixels, Render, Styled as _, TestAppContext, Window, WindowBounds,
    WindowOptions, div, point, px, size, test::TestWindowExt as _,
};
use gpui_kit_grid::{
    AssetImageSource, CellImage, CellImageSource, CellImages, CellValue, ColumnAlign, ColumnIx,
    GridColumn, GridColumns, GridRow, GridSchema, ImageFit, ImageValue, LiveGrid, LiveGridDelegate,
    LiveGridState, SortDirection, StoreOptions,
};

use crate::common::{ROW, WINDOW_HEIGHT};

// The test images: a wide red, a tall blue and a square green PNG, and an
// SVG of two colours.

/// A 4×2 red PNG.
const WIDE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 127, 168, 125, 99, 0, 0, 0, 18, 73, 68, 65, 84, 120, 218, 99, 248, 207, 192, 240, 31, 25,
    51, 160, 11, 0, 0, 15, 33, 15, 241, 254, 69, 20, 99, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];
/// A 2×4 blue PNG.
const TALL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 4, 8, 6, 0,
    0, 0, 164, 239, 238, 57, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 96, 96, 248, 255, 31, 130,
    113, 50, 0, 15, 35, 15, 241, 75, 143, 102, 166, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
/// A 3×3 green PNG.
const SQUARE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 3, 8, 6, 0,
    0, 0, 86, 40, 181, 191, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 96, 88, 192, 240, 31, 142,
    113, 114, 0, 18, 20, 14, 152, 204, 221, 92, 65, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// An SVG in two colours: red on the left, blue on the right.
const TWO_COLOURS_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4" viewBox="0 0 8 4"><rect x="0" y="0" width="4" height="4" fill="#ff0000"/><rect x="4" y="0" width="4" height="4" fill="#0000ff"/></svg>"##;

/// The columns of a [`Person`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PersonColumn {
    /// `Int`.
    Id,
    /// `Image`.
    Avatar,
    /// `Text`.
    Name,
}

impl GridColumns for PersonColumn {
    fn all() -> &'static [Self] {
        &[Self::Id, Self::Avatar, Self::Name]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Avatar => "avatar",
            Self::Name => "name",
        }
    }

    fn ix(self) -> ColumnIx {
        ColumnIx::new(self as u16)
    }
}

/// A person with an avatar: the image's id and the name that stands for it.
#[derive(Clone, Debug)]
struct Person {
    id: u32,
    avatar: Arc<str>,
    name: &'static str,
}

impl GridRow for Person {
    type Key = u32;
    type Column = PersonColumn;

    fn key(&self) -> &u32 {
        &self.id
    }

    fn value(&self, column: PersonColumn) -> CellValue<'_> {
        match column {
            PersonColumn::Id => CellValue::Int(self.id.into()),
            PersonColumn::Avatar => CellValue::Image(ImageValue::new(&self.avatar, self.name)),
            PersonColumn::Name => CellValue::Text(self.name),
        }
    }
}

struct People;

impl LiveGridDelegate for People {
    type Row = Person;
}

/// The test's images as files in a directory of its own, by name: `wide`,
/// `tall`, `square` and `colours`.
fn image_files(test: &str) -> impl Fn(&str) -> Arc<str> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("images-{test}"));
    std::fs::create_dir_all(&dir).expect("a directory for the test images");
    for (name, bytes) in [
        ("wide.png", WIDE_PNG),
        ("tall.png", TALL_PNG),
        ("square.png", SQUARE_PNG),
        ("colours.svg", TWO_COLOURS_SVG.as_bytes()),
    ] {
        std::fs::write(dir.join(name), bytes).expect("a test image");
    }
    move |name: &str| {
        let file = match name {
            "colours" => "colours.svg".to_owned(),
            name => format!("{name}.png"),
        };
        let path: PathBuf = dir.join(file);
        Arc::from(path.to_str().expect("a UTF-8 path"))
    }
}

/// People with the images `avatars` gives, named after them.
fn people(avatars: &[(&'static str, Arc<str>)]) -> Vec<Person> {
    avatars
        .iter()
        .zip(1..)
        .map(|((name, avatar), id)| Person {
            id,
            avatar: avatar.clone(),
            name,
        })
        .collect()
}

/// `Id` 50 px, `Avatar` 60 px fitted by `fit` and aligned by `align`,
/// `Name` 100 px.
fn schema(fit: ImageFit, align: ColumnAlign, radius: Pixels) -> GridSchema<PersonColumn> {
    GridSchema::new()
        .column(GridColumn::new(PersonColumn::Id, "Id").with_width(px(50.)))
        .column(
            GridColumn::new(PersonColumn::Avatar, "Avatar")
                .with_width(px(60.))
                .with_align(align)
                .with_image_fit(fit)
                .with_image_corner_radius(radius),
        )
        .column(GridColumn::new(PersonColumn::Name, "Name").with_width(px(100.)))
}

/// A window with an `xsmall` grid in it.
struct GridRoot {
    grid: Entity<LiveGridState<People>>,
}

impl Render for GridRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::Sizable as _;
        div().size_full().child(
            LiveGrid::new(&self.grid)
                .with_size(Size::XSmall)
                .bordered(false),
        )
    }
}

/// An `xsmall` grid of `schema` holding `rows`, configured by `configure`
/// (its image source). Runs until the images are in and draws.
fn open_people(
    cx: &mut TestAppContext,
    schema: GridSchema<PersonColumn>,
    rows: Vec<Person>,
    configure: impl FnOnce(LiveGridState<People>) -> LiveGridState<People> + 'static,
) -> (AnyWindowHandle, Entity<LiveGridState<People>>) {
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
                        size: size(px(400.), px(WINDOW_HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| GridRoot {
                        grid: cx.new(|cx| {
                            configure(LiveGridState::new_standalone(
                                schema,
                                People,
                                StoreOptions::default(),
                                cx,
                            ))
                        }),
                    })
                },
            )
        })
        .expect("open window");
    let grid = root.read_with(cx, |root, _| root.grid.clone());
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all(rows.into_iter().map(Arc::new));
    for _ in 0..3 {
        draw(cx, window, &grid);
    }
    (window, grid)
}

/// Runs jobs, timers and loads for 20 ms, then draws a frame of `grid`.
fn draw(cx: &mut TestAppContext, window: AnyWindowHandle, grid: &Entity<LiveGridState<People>>) {
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    grid.update(cx, |_, cx| cx.notify());
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("window is open");
}

/// The images of the last frame, top to bottom: room, fitted bounds,
/// corner radius and size in pixels.
fn painted(
    cx: &mut TestAppContext,
    grid: &Entity<LiveGridState<People>>,
) -> Vec<(Bounds<Pixels>, Bounds<Pixels>, Pixels, (i32, i32))> {
    grid.read_with(cx, |grid, _| {
        grid.painted_images()
            .iter()
            .map(|(room, fitted, radius, data)| {
                let size = data.size(0);
                (*room, *fitted, *radius, (size.width.0, size.height.0))
            })
            .collect()
    })
}

fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

#[gpui_kit::test]
fn test_image_cell(cx: &mut TestAppContext) {
    let file = image_files("cell");
    let rows = people(&[("Wide", file("wide")), ("Tall", file("tall"))]);
    let (_, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(3.)),
        rows.clone(),
        |grid| grid,
    );
    // The room is the cell without its padding: 48 × 14 in a 60 px column
    // of 26 px rows. `Contain` keeps the proportions, in the middle.
    let images = painted(cx, &grid);
    assert_eq!(images.len(), 2);
    let room = bounds(56., ROW + 6., 48., 14.);
    assert_eq!(
        images[0],
        (room, bounds(66., ROW + 6., 28., 14.), px(3.), (4, 2))
    );
    let room = bounds(56., ROW * 2. + 6., 48., 14.);
    assert_eq!(
        images[1],
        (room, bounds(76.5, ROW * 2. + 6., 7., 14.), px(3.), (2, 4))
    );
    assert_eq!(grid.read_with(cx, |grid, _| grid.stats().images()), 2);

    // At the start of the room for `Start`.
    let (_, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Start, px(0.)),
        rows.clone(),
        |grid| grid,
    );
    assert_eq!(painted(cx, &grid)[0].1, bounds(56., ROW + 6., 28., 14.));

    // `Cover` fills the room and spills over it, so the room crops it.
    let (_, grid) = open_people(
        cx,
        schema(ImageFit::Cover, ColumnAlign::Auto, px(0.)),
        rows,
        |grid| grid,
    );
    let (room, fitted, _, _) = painted(cx, &grid)[0];
    assert_eq!(room, bounds(56., ROW + 6., 48., 14.));
    assert_eq!(fitted, bounds(56., ROW + 6. - 5., 48., 24.));
}

/// A source that records what the source it wraps gave.
struct Recording {
    inner: AssetImageSource,
    seen: Rc<RefCell<Vec<(String, &'static str)>>>,
}

impl CellImageSource for Recording {
    fn image(&self, id: &str, window: &mut Window, cx: &mut App) -> CellImage {
        let image = self.inner.image(id, window, cx);
        let state = match image {
            CellImage::Ready(_) => "ready",
            CellImage::Loading => "loading",
            CellImage::Missing => "missing",
            _ => "other",
        };
        self.seen.borrow_mut().push((id.to_owned(), state));
        image
    }
}

#[gpui_kit::test]
fn test_image_loading_then_ready(cx: &mut TestAppContext) {
    let file = image_files("loading");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let source = Recording {
        inner: AssetImageSource::default(),
        seen: seen.clone(),
    };
    let (window, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        Vec::new(),
        |grid| grid.with_image_source(CellImages::custom(source)),
    );
    let frames = |cx: &mut TestAppContext| grid.read_with(cx, |grid, _| grid.stats().frames());
    let before = frames(cx);
    // Rows come; the application does nothing else.
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    writer.replace_all([Arc::new(people(&[("Ada", file("square"))]).remove(0))]);
    for _ in 0..5 {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
    }
    // The first frame found it loading, a later one ready: the load
    // repainted the grid by itself.
    let states: Vec<&str> = seen.borrow().iter().map(|(_, state)| *state).collect();
    assert_eq!(states.first(), Some(&"loading"), "{states:?}");
    assert_eq!(states.last(), Some(&"ready"), "{states:?}");
    assert!(frames(cx) > before + 1);
    assert_eq!(painted(cx, &grid).len(), 1);
    let _ = window;
}

#[gpui_kit::test]
fn test_image_missing(cx: &mut TestAppContext) {
    let file = image_files("missing");
    let missing_file: Arc<str> = Arc::from(
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("images-missing/none.png")
            .to_str()
            .expect("a UTF-8 path"),
    );
    let rows = people(&[
        ("Gone", missing_file),
        ("Asset", Arc::from("images/none.png")),
        ("Here", file("square")),
    ]);
    let (window, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        rows,
        |grid| grid.with_image_source(AssetImageSource::default()),
    );
    // No file, no asset: no panic, nothing painted; the image that is
    // there is.
    let images = painted(cx, &grid);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].0.origin.y, px(ROW * 3. + 6.));
    // The text of a missing image is its tooltip and its label.
    cx.update_window(window, |_, window, cx| {
        common::move_pointer_to(window, point(px(80.), px(ROW * 1.5)), cx);
    })
    .expect("window is open");
    draw(cx, window, &grid);
    let tooltip = grid.read_with(cx, |grid, _| grid.hovered_tooltip_text());
    assert_eq!(tooltip.as_deref(), Some("Gone"));
    grid.update(cx, |grid, _| grid.set_a11y_forced(true));
    draw(cx, window, &grid);
    let cells = grid.read_with(cx, |grid, _| grid.a11y_row_cells(&common::order_row_id(2)));
    assert_eq!(cells[1].0, "Asset");
}

#[gpui_kit::test]
fn test_image_sort_filter_copy(cx: &mut TestAppContext) {
    let file = image_files("sort");
    let rows = people(&[
        ("Grace", file("tall")),
        ("Ada", file("wide")),
        ("Linus", file("square")),
    ]);
    let (window, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        rows,
        |grid| grid.with_quick_filter_columns([PersonColumn::Avatar]),
    );
    let visible = |cx: &mut TestAppContext| grid.read_with(cx, |grid, _| grid.visible_keys());
    // By the text, not the id.
    grid.update(cx, |grid, cx| {
        grid.set_sort([(PersonColumn::Avatar, SortDirection::Ascending)], cx)
    });
    draw(cx, window, &grid);
    assert_eq!(visible(cx), [2, 1, 3]);
    // The quick filter, over the avatar column only, searches its text.
    grid.update(cx, |grid, cx| grid.set_quick_filter("gra", cx));
    draw(cx, window, &grid);
    draw(cx, window, &grid);
    assert_eq!(visible(cx), [1]);
    // The file name does not match: an id is not searched.
    grid.update(cx, |grid, cx| grid.set_quick_filter("png", cx));
    draw(cx, window, &grid);
    draw(cx, window, &grid);
    assert!(visible(cx).is_empty());
}

#[gpui_kit::test]
fn test_image_custom_source(cx: &mut TestAppContext) {
    let file = image_files("custom");
    let rows = people(&[("Ada", file("wide")), ("Grace", file("tall"))]);
    // The application's source is asked for every image cell painted; the
    // built-in one, not at all.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let assets = AssetImageSource::default();
    let custom = Recording {
        inner: AssetImageSource::default(),
        seen: seen.clone(),
    };
    let (_, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        rows.clone(),
        |grid| grid.with_image_source(CellImages::custom(custom)),
    );
    assert_eq!(painted(cx, &grid).len(), 2);
    let ids: Vec<String> = seen.borrow().iter().map(|(id, _)| id.clone()).collect();
    assert!(ids.contains(&file("wide").to_string()));
    assert!(ids.contains(&file("tall").to_string()));
    assert_eq!(assets.asset_requests(), 0);

    // The built-in one, given: it asks GPUI once per image while it loads,
    // and no more once the images are in.
    let (window, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        rows,
        {
            let assets = assets.clone();
            move |grid| grid.with_image_source(assets)
        },
    );
    assert_eq!(painted(cx, &grid).len(), 2);
    let requests = assets.asset_requests();
    assert!(requests >= 2, "{requests}");
    for _ in 0..3 {
        draw(cx, window, &grid);
    }
    assert_eq!(assets.asset_requests(), requests);
    assert_eq!(assets.len(), 2);
}

#[gpui_kit::test]
fn test_image_capacity(cx: &mut TestAppContext) {
    let file = image_files("capacity");
    let mut each = people(&[
        ("Ada", file("wide")),
        ("Grace", file("tall")),
        ("Linus", file("square")),
    ])
    .into_iter()
    .map(Arc::new);
    let (ada, grace, linus) = (
        each.next().expect("Ada"),
        each.next().expect("Grace"),
        each.next().expect("Linus"),
    );
    let source = AssetImageSource::default().with_capacity(2);
    let (window, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        Vec::new(),
        {
            let source = source.clone();
            move |grid| grid.with_image_source(source)
        },
    );
    let writer = grid.read_with(cx, |grid, _| grid.writer());
    let show = |rows: &[&Arc<Person>], cx: &mut TestAppContext| {
        writer.replace_all(rows.iter().map(|row| Arc::clone(row)));
        for _ in 0..3 {
            draw(cx, window, &grid);
        }
        painted(cx, &grid).len()
    };
    // Two images: both kept.
    assert_eq!(show(&[&ada, &grace], cx), 2);
    assert_eq!(source.len(), 2);
    let requests = source.asset_requests();
    // A third: the least recently used (Ada, asked for first) goes.
    assert_eq!(show(&[&linus], cx), 1);
    assert_eq!(source.len(), 2);
    let after_linus = source.asset_requests();
    assert!(after_linus > requests);
    // Grace is still in: no request.
    assert_eq!(show(&[&grace], cx), 1);
    assert_eq!(source.asset_requests(), after_linus);
    // Ada again: loaded anew, and Linus, used longest ago, goes.
    assert_eq!(show(&[&ada], cx), 1);
    let after_ada = source.asset_requests();
    assert!(after_ada > after_linus);
    assert_eq!(source.len(), 2);
    assert_eq!(show(&[&grace], cx), 1);
    assert_eq!(source.asset_requests(), after_ada);

    // Three on screen at once, over the capacity: all kept and painted,
    // none pushed out by another, and no more requests once they are in.
    assert_eq!(show(&[&ada, &grace, &linus], cx), 3);
    assert_eq!(source.len(), 3);
    let settled = source.asset_requests();
    assert_eq!(show(&[&ada, &grace, &linus], cx), 3);
    assert_eq!(source.asset_requests(), settled);
}

#[gpui_kit::test]
fn test_image_svg_color(cx: &mut TestAppContext) {
    let file = image_files("svg");
    let rows = people(&[("Colours", file("colours"))]);
    let (_, grid) = open_people(
        cx,
        schema(ImageFit::Contain, ColumnAlign::Auto, px(0.)),
        rows,
        |grid| grid,
    );
    // A colour SVG is an image: a raster in its own colours, not an icon
    // painted in one colour.
    let images = grid.read_with(cx, |grid, _| grid.painted_images().to_vec());
    assert_eq!(images.len(), 1);
    let icons = grid.read_with(cx, |grid, _| grid.painted_icons().len());
    assert_eq!(icons, 0);
    let data = &images[0].3;
    let bytes = data.as_bytes(0).expect("a frame");
    let width = data.size(0).width.0 as usize;
    // BGRA: red on the left, blue on the right.
    let pixel = |x: usize| &bytes[x * 4..x * 4 + 4];
    assert_eq!(pixel(0)[..3], [0, 0, 255]);
    assert_eq!(pixel(width - 1)[..3], [255, 0, 0]);
}

#[gpui_kit::test]
fn test_image_autofit(cx: &mut TestAppContext) {
    let file = image_files("autofit");
    let rows = people(&[("A name much wider than its image", file("wide"))]);
    // No title to keep room for: the square of the row without its
    // padding, and the padding: 14 + 2 × 6 + 1.
    let schema = GridSchema::new().column(
        GridColumn::new(PersonColumn::Avatar, "")
            .with_width(px(200.))
            .with_sortable(false),
    );
    let (window, grid) = open_people(cx, schema, rows, |grid| grid);
    grid.update(cx, |grid, cx| grid.autofit_column(PersonColumn::Avatar, cx));
    draw(cx, window, &grid);
    let width = grid.read_with(cx, |grid, _| grid.column_width(PersonColumn::Avatar));
    assert_eq!(width, px(27.));
}
