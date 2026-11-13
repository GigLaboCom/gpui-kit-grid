//! The wiki example's theme: a gpui-kit theme file in `themes/`,
//! registered with its fonts by the trading examples' `theme::install`.
//!
//! Folio: warm paper and ink, a deep teal accent for focus, selection and
//! the busiest family, links in a blue of their own. The status colours
//! belong to status alone: `success` bytes added and a rising wiki,
//! `danger` bytes removed, `warning` bots and the flash of a new edit, a
//! highlighter on paper. Inter for the interface and the table (with
//! tabular figures), IBM Plex Mono for the figures outside it.

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WikiTheme {
    Folio,
}

impl TradingTheme for WikiTheme {
    const ALL: &'static [Self] = &[Self::Folio];

    fn label(self) -> &'static str {
        match self {
            Self::Folio => "Folio",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Folio => "Wiki Folio",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Folio => include_str!("themes/folio.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in WikiTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(!set.themes[0].mode.is_dark());
            assert_eq!(set.themes[0].font_family.as_deref(), Some("Inter"));
            assert_eq!(
                set.themes[0].mono_font_family.as_deref(),
                Some("IBM Plex Mono")
            );
        }
        assert_eq!(WikiTheme::parse("folio"), Some(WikiTheme::Folio));
    }
}
