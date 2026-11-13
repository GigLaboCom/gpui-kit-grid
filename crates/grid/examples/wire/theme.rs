//! The wire's theme: a gpui-kit theme file in `themes/`, registered with
//! its font by the trading examples' `theme::install`.

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTheme {
    /// A navy canvas, surfaces a step lighter, a teal accent, soft
    /// blue-grey borders, muted green and red; Inter everywhere.
    Navy,
}

impl TradingTheme for WireTheme {
    const ALL: &'static [Self] = &[Self::Navy];

    fn label(self) -> &'static str {
        match self {
            Self::Navy => "Navy",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Navy => "Wire Navy",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Navy => include_str!("themes/navy.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in WireTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
            assert_eq!(
                set.themes[0].font_family.as_deref(),
                Some("Inter"),
                "Inter for everything"
            );
        }
        assert_eq!(WireTheme::parse("NAVY"), Some(WireTheme::Navy));
    }
}
