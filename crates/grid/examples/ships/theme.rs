//! The ships example's theme: a gpui-kit theme file in `themes/`,
//! registered with its fonts by the trading examples' `theme::install`.
//!
//! Night bridge: the deep blue-black of a chart display dimmed for the
//! night watch, hairlines in the colour of shallow water, and signal
//! orange for focus, selection and the selected vessel. The status
//! colours keep to status: `danger` a vessel that asks others to keep
//! clear, `success` the live feed. Ship types take the base colours,
//! the same on the map and in the table. Barlow Semi Condensed for the
//! interface, IBM Plex Mono for the figures.

use gpui_kit::Hsla;
use gpui_kit::component::Theme;

use super::ais::ShipCategory;
use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShipsTheme {
    Bridge,
}

impl TradingTheme for ShipsTheme {
    const ALL: &'static [Self] = &[Self::Bridge];

    fn label(self) -> &'static str {
        match self {
            Self::Bridge => "Bridge",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bridge => "Ships Night Bridge",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Bridge => include_str!("themes/bridge.json"),
        }
    }
}

/// A ship type's colour, on the map and in the Type column.
pub fn category_color(category: ShipCategory, theme: &Theme) -> Hsla {
    match category {
        ShipCategory::Cargo => theme.green,
        ShipCategory::Tanker => theme.red,
        ShipCategory::Passenger => theme.blue,
        ShipCategory::HighSpeed => theme.yellow,
        ShipCategory::Tug => theme.cyan,
        ShipCategory::Service => theme.cyan_light,
        ShipCategory::Leisure => theme.magenta_light,
        ShipCategory::Special => theme.magenta,
        ShipCategory::Other => theme.muted_foreground,
        ShipCategory::Unknown => theme.muted_foreground.opacity(0.7),
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in ShipsTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
            assert_eq!(
                set.themes[0].font_family.as_deref(),
                Some("Barlow Semi Condensed")
            );
            assert_eq!(
                set.themes[0].mono_font_family.as_deref(),
                Some("IBM Plex Mono")
            );
        }
        assert_eq!(ShipsTheme::parse("bridge"), Some(ShipsTheme::Bridge));
    }
}
