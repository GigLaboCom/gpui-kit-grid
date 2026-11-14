//! The other GigLabo products, in every example: [`ProductsButton`] ("More
//! from GigLabo") for an example's app bar, and the dialog it opens
//! ([`open_products`]), laid out as the site's Products page: the products
//! as cards under the site's groups, each with its icon, its name, one line
//! and its address, then the studio's card. A click on a card opens its
//! site in the browser (a new tab on the web).
//!
//! ```ignore
//! h_flex()
//!     // the example's own app bar …
//!     .child(giglabo_products::ProductsButton::new())
//! ```
//!
//! The list is `products.json`, the entries of the site's Products page
//! (`site/src/data/products.ts`, `site/src/components/StudioCard.astro`);
//! the icons are `icons/`, 128 px PNG files made from the same originals as
//! the site's. A page embedded in another site hides the button
//! ([`set_hidden`]).
//!
//! `GIGLABO_PRODUCTS=off` in the environment hides it in any native example
//! (stills and recordings without it); the variable is read once, and only
//! the exact value `off` counts. A web page has no environment: there the
//! button follows [`set_hidden`] alone.
//!
//! The examples are in English only, and so is this.

use std::sync::{Arc, OnceLock};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable, Size, WindowExt as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::{
    AnyElement, App, FontWeight, Global, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, img, prelude::FluentBuilder as _, px, white,
};
use serde::Deserialize;

/// The button's label and the dialog's title.
const TITLE: &str = "More from GigLabo";
/// The line under the title, as the site's Products page says it.
const INTRO: &str = "gpui-kit-grid is one of the things built at GigLabo, a studio for custom \
    software, web apps and AI. These are the others we build and run.";
/// The studio's section, after the products'.
const STUDIO_TITLE: &str = "The studio";
/// The dialog's size: a smaller window shrinks it, and its sections scroll.
const DIALOG_WIDTH: f32 = 680.;
const DIALOG_HEIGHT: f32 = 640.;
/// A product's icon, the studio's logo and the button's, drawn from
/// 128 px PNG files.
const ICON_SIZE: f32 = 40.;
const LOGO_SIZE: f32 = 52.;
const BUTTON_LOGO_SIZE: f32 = 16.;

/// The products and the studio, as the site lists them.
const LIST: &str = include_str!("../products.json");

/// Each card's icon, by its `id` in `products.json`.
const ICONS: [(&str, &[u8]); 7] = [
    ("smbact", include_bytes!("../icons/smbact.png")),
    ("wipemark", include_bytes!("../icons/wipemark.png")),
    ("mnemovi", include_bytes!("../icons/mnemovi.png")),
    (
        "heretic-lazy-shot",
        include_bytes!("../icons/heretic-lazy-shot.png"),
    ),
    ("watchword", include_bytes!("../icons/watchword.png")),
    ("moat-atlas", include_bytes!("../icons/moat-atlas.png")),
    ("giglabo", include_bytes!("../icons/giglabo.png")),
];

/// The products' sections, in the site's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Group {
    Desktop,
    Ai,
    Web,
}

impl Group {
    const ALL: [Group; 3] = [Group::Desktop, Group::Ai, Group::Web];

    fn title(self) -> &'static str {
        match self {
            Group::Desktop => "Desktop apps",
            Group::Ai => "AI tools",
            Group::Web => "On the web",
        }
    }
}

/// `products.json`.
#[derive(Deserialize)]
struct List {
    products: Vec<ProductEntry>,
    studio: CardEntry,
}

/// A product: its group and its card.
#[derive(Deserialize)]
struct ProductEntry {
    group: Group,
    #[serde(flatten)]
    card: CardEntry,
}

/// What a card shows.
#[derive(Deserialize)]
struct CardEntry {
    id: String,
    name: String,
    url: String,
    tagline: String,
    /// A light plate behind an icon drawn for light backgrounds (a dark
    /// mark on transparent).
    #[serde(default)]
    plate: bool,
}

/// A card as the dialog draws it.
struct Card {
    name: SharedString,
    url: SharedString,
    /// The address without its scheme (`smbact.com`; the studio's with its
    /// path, `giglabo.com/agency`, as the site shows it).
    address: SharedString,
    tagline: SharedString,
    icon: Arc<Image>,
    plate: bool,
}

/// The dialog's content, read once.
struct Products {
    products: Vec<(Group, Card)>,
    studio: Card,
}

fn products() -> &'static Products {
    static PRODUCTS: OnceLock<Products> = OnceLock::new();
    PRODUCTS.get_or_init(|| {
        let list: List = serde_json::from_str(LIST).expect("products.json lists the products");
        Products {
            products: list
                .products
                .into_iter()
                .map(|entry| (entry.group, Card::new(entry.card, host)))
                .collect(),
            studio: Card::new(list.studio, without_scheme),
        }
    })
}

impl Card {
    fn new(entry: CardEntry, address: fn(&str) -> &str) -> Self {
        Self {
            address: address(&entry.url).into(),
            icon: icon(&entry.id),
            name: entry.name.into(),
            url: entry.url.into(),
            tagline: entry.tagline.into(),
            plate: entry.plate,
        }
    }
}

/// The icon `id` names, decoded once by whoever draws it.
fn icon(id: &str) -> Arc<Image> {
    ICONS
        .iter()
        .find(|(icon, _)| *icon == id)
        .map(|(_, png)| Arc::new(Image::from_bytes(ImageFormat::Png, png.to_vec())))
        .unwrap_or_else(|| panic!("no icon for `{id}`"))
}

/// `https://smbact.com/x` → `smbact.com/x`.
fn without_scheme(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.trim_end_matches('/')
}

/// `https://smbact.com/x` → `smbact.com`.
fn host(url: &str) -> &str {
    let rest = without_scheme(url);
    rest.split('/').next().unwrap_or(rest)
}

/// Whether [`ProductsButton`] draws nothing.
#[derive(Default)]
struct Hidden(bool);

impl Global for Hidden {}

/// Hides [`ProductsButton`] in every window (or shows it again): a page
/// embedded in another site carries no links to ours.
pub fn set_hidden(cx: &mut App, hidden: bool) {
    cx.set_global(Hidden(hidden));
}

/// Whether [`ProductsButton`] is hidden: by [`set_hidden`], or by
/// `GIGLABO_PRODUCTS=off` in the environment, which `set_hidden(cx, false)`
/// does not override.
pub fn is_hidden(cx: &App) -> bool {
    hidden_by_environment() || cx.try_global::<Hidden>().is_some_and(|hidden| hidden.0)
}

/// The environment variable that hides [`ProductsButton`] when it is `off`.
const ENVIRONMENT_VARIABLE: &str = "GIGLABO_PRODUCTS";

/// Whether the environment hides [`ProductsButton`], read once: the button
/// renders every frame. On the web `std::env::var` finds nothing, so a page
/// keeps the button unless it calls [`set_hidden`].
fn hidden_by_environment() -> bool {
    static HIDDEN: OnceLock<bool> = OnceLock::new();
    *HIDDEN.get_or_init(|| hides(std::env::var(ENVIRONMENT_VARIABLE).ok().as_deref()))
}

/// Whether a value of `GIGLABO_PRODUCTS` hides the button: only exactly
/// `off`, the one spelling the tooling writes. `OFF`, ` off `, any other
/// value, a value that is not Unicode and no variable at all leave the
/// button shown.
fn hides(value: Option<&str>) -> bool {
    value == Some("off")
}

/// "More from GigLabo" with the studio's logo, a button in the theme's
/// primary colour (outlined, so it never outweighs an example's own
/// primary action) that opens the products ([`open_products`]); nothing
/// when hidden ([`is_hidden`]). Small unless sized otherwise, or by the
/// `Toolbar` it is a child of.
#[derive(IntoElement)]
pub struct ProductsButton {
    size: Size,
}

impl ProductsButton {
    pub fn new() -> Self {
        Self { size: Size::Small }
    }
}

impl Default for ProductsButton {
    fn default() -> Self {
        Self::new()
    }
}

impl Sizable for ProductsButton {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for ProductsButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let shown = !is_hidden(cx);
        div().flex_none().when(shown, |this| {
            this.child(
                Button::new("giglabo-products")
                    .with_size(self.size)
                    .primary()
                    .outline()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(render_logo(
                                &products().studio,
                                BUTTON_LOGO_SIZE,
                                px(1.),
                                px(3.),
                            ))
                            .child(TITLE),
                    )
                    .on_click(|_, window, cx| open_products(window, cx)),
            )
        })
    }
}

/// Opens the products in a dialog over `window`: Escape, a click outside
/// or its close button closes it.
pub fn open_products(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let products = products();
        dialog
            .w(px(DIALOG_WIDTH))
            .h(px(DIALOG_HEIGHT))
            .title(TITLE)
            .child(
                div()
                    .flex_none()
                    .pb_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(INTRO),
            )
            .children(Group::ALL.map(|group| {
                render_section(
                    group.title(),
                    div().grid().grid_cols(2).gap_3().children(
                        products
                            .products
                            .iter()
                            .enumerate()
                            .filter(|(_, (of, _))| *of == group)
                            .map(|(ix, (_, card))| render_card(ix, card, cx)),
                    ),
                    cx,
                )
            }))
            .child(render_section(
                STUDIO_TITLE,
                render_studio(&products.studio, cx),
                cx,
            ))
    });
}

/// A section: its title over `content`.
fn render_section(title: &'static str, content: impl IntoElement, cx: &App) -> AnyElement {
    v_flex()
        .flex_none()
        .gap_2()
        .pt_3()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(title),
        )
        .child(content)
        .into_any_element()
}

/// A product's card; a click opens its site.
fn render_card(ix: usize, card: &'static Card, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let hover = theme.list_hover;
    let url = card.url.clone();
    h_flex()
        .id(("giglabo-product", ix))
        .items_start()
        .gap_3()
        .p_3()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .cursor_pointer()
        .hover(move |this| this.bg(hover))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(render_logo(card, ICON_SIZE, px(4.), theme.radius))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(card.tagline.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(card.address.clone()),
                ),
        )
}

/// The studio's card, as the site draws it: as wide as the dialog, in the
/// theme's primary colour.
fn render_studio(card: &'static Card, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (normal, hover) = (theme.primary.opacity(0.1), theme.primary.opacity(0.18));
    let url = card.url.clone();
    h_flex()
        .id("giglabo-studio")
        .gap_4()
        .px_4()
        .py_3()
        .border_1()
        .border_color(theme.primary)
        .rounded(theme.radius_lg)
        .bg(normal)
        .cursor_pointer()
        .hover(move |this| this.bg(hover))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(render_logo(card, LOGO_SIZE, px(5.), theme.radius_lg))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(card.tagline.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(card.address.clone()),
                ),
        )
}

/// A card's icon, `size` square, on a white plate (`inset` wide) when it is
/// drawn for light backgrounds.
fn render_logo(
    card: &'static Card,
    size: f32,
    inset: Pixels,
    radius: Pixels,
) -> impl IntoElement + use<> {
    div()
        .flex_none()
        .size(px(size))
        .rounded(radius)
        .overflow_hidden()
        .when(card.plate, |this| this.bg(white()).p(inset))
        .child(img(card.icon.clone()).size_full())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_every_card_has_an_icon_and_every_icon_a_card() {
        let list: List = serde_json::from_str(LIST).unwrap();
        let ids: Vec<&str> = list
            .products
            .iter()
            .map(|entry| entry.card.id.as_str())
            .chain([list.studio.id.as_str()])
            .collect();
        assert_eq!(ids.len(), ICONS.len());
        for (id, _) in ICONS {
            assert!(ids.contains(&id), "the icon `{id}` names no card");
        }
        // Decodes the list and finds every card's icon.
        assert_eq!(products().products.len(), list.products.len());
    }

    #[test]
    fn test_every_group_has_products() {
        for group in Group::ALL {
            assert!(
                products().products.iter().any(|(of, _)| *of == group),
                "{group:?} is empty"
            );
        }
    }

    #[test]
    fn test_addresses() {
        assert_eq!(host("https://smbact.com"), "smbact.com");
        assert_eq!(
            host("https://github.com/GigLaboCom/wipemark-app/releases/latest"),
            "github.com"
        );
        assert_eq!(host("moa.giglabo.com/x"), "moa.giglabo.com");
        assert_eq!(
            without_scheme("https://giglabo.com/agency/"),
            "giglabo.com/agency"
        );
        assert_eq!(products().studio.address, "giglabo.com/agency");
    }

    #[test]
    fn test_only_off_hides() {
        assert!(!hides(None));
        assert!(hides(Some("off")));
        for value in ["", "on", "0", "false", "OFF", "Off", " off ", "off\n"] {
            assert!(!hides(Some(value)), "{value:?} hides the button");
        }
    }

    #[test]
    fn test_links_are_https() {
        let products = products();
        for card in products
            .products
            .iter()
            .map(|(_, card)| card)
            .chain([&products.studio])
        {
            assert!(card.url.starts_with("https://"), "{}", card.url);
        }
    }
}
