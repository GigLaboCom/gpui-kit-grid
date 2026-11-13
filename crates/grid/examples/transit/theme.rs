//! The transit example's theme: a gpui-kit theme file in `themes/`,
//! registered with its fonts by the trading examples' `theme::install`.
//!
//! Paper: a warm light ground like a printed map, ink text and a plum
//! accent for focus, selection, flashes and the selected run, a colour no
//! mode uses. The chart colours are the modes' (U-Bahn, S-Bahn, tram, bus)
//! wherever the data gives a line no colour of its own; the status colours
//! belong to status alone. Barlow Semi Condensed for the interface, IBM
//! Plex Mono for the table's figures (it has the arrow of `A → B`).

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitTheme {
    Paper,
}

impl TradingTheme for TransitTheme {
    const ALL: &'static [Self] = &[Self::Paper];

    fn label(self) -> &'static str {
        match self {
            Self::Paper => "Paper",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Paper => "Transit Paper",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Paper => include_str!("themes/paper.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in TransitTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(!set.themes[0].mode.is_dark());
            assert_eq!(
                set.themes[0].font_family.as_deref(),
                Some("Barlow Semi Condensed")
            );
            assert_eq!(
                set.themes[0].mono_font_family.as_deref(),
                Some("IBM Plex Mono")
            );
        }
        assert_eq!(TransitTheme::parse("paper"), Some(TransitTheme::Paper));
    }
}
