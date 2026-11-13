//! The gpui-kit icons a page draws, bundled: the web build of
//! `gpui_kit::assets::Assets` downloads icons from a server instead.

use std::borrow::Cow;

use gpui_kit::{AssetSource, SharedString};

/// A gpui-kit icon bundled into a module: its asset path (`icons/…`) and
/// its SVG.
pub type BundledIcon = (&'static str, &'static [u8]);

macro_rules! icon {
    ($file:literal) => {
        (
            concat!("icons/", $file),
            include_bytes!(concat!(
                "../../../../../gpui-kit/crates/assets/assets/icons/",
                $file
            )),
        )
    };
}

/// The icons the grid and the gpui-kit components of the examples draw.
const ICONS: [BundledIcon; 19] = [
    // The grid's.
    icon!("arrow-up.svg"),
    icon!("arrow-down.svg"),
    icon!("chevron-down.svg"),
    icon!("chevron-right.svg"),
    icon!("sort-ascending.svg"),
    icon!("sort-descending.svg"),
    icon!("close.svg"),
    // Menus and the dock.
    icon!("check.svg"),
    icon!("ellipsis.svg"),
    icon!("maximize.svg"),
    icon!("menu.svg"),
    icon!("minimize.svg"),
    // Top bars, search boxes and steppers.
    icon!("circle-user.svg"),
    icon!("circle-x.svg"),
    icon!("minus.svg"),
    icon!("moon.svg"),
    icon!("plus.svg"),
    icon!("search.svg"),
    icon!("sun.svg"),
];

/// The page's icons: those the shell bundles and the example's own. One
/// not listed is drawn blank and logged.
pub(crate) struct Icons {
    extra: &'static [BundledIcon],
}

impl Icons {
    pub(crate) fn new(extra: &'static [BundledIcon]) -> Self {
        Self { extra }
    }
}

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        let found = ICONS
            .iter()
            .chain(self.extra)
            .find(|(icon, _)| *icon == path);
        let Some((_, svg)) = found else {
            log::warn!("icon not bundled: {path}");
            return Ok(None);
        };
        Ok(Some(Cow::Borrowed(svg)))
    }

    fn list(&self, _: &str) -> gpui_kit::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
