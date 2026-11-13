//! The orbit example's theme: a gpui-kit theme file in `themes/`,
//! registered with its fonts by the trading examples' `theme::install`.
//!
//! Night: a warm near-black with a dim red accent for focus, selection and
//! the selected object, the red an observer's torch keeps dark-adapted
//! eyes with. The status colours belong to status alone: `success` an
//! object above the horizon (and a rise), `warning` sunlit, `info` in the
//! Earth's shadow, `danger` data that cannot be used. Manrope for the
//! interface, JetBrains Mono for the numbers.

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrbitTheme {
    Night,
}

impl TradingTheme for OrbitTheme {
    const ALL: &'static [Self] = &[Self::Night];

    fn label(self) -> &'static str {
        match self {
            Self::Night => "Night",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Night => "Orbit Night",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Night => include_str!("themes/night.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in OrbitTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
            assert_eq!(set.themes[0].font_family.as_deref(), Some("Manrope"));
            assert_eq!(
                set.themes[0].mono_font_family.as_deref(),
                Some("JetBrains Mono"),
                "JetBrains Mono in the grid: it has ▲ ▽ ● ○"
            );
        }
        assert_eq!(OrbitTheme::parse("night"), Some(OrbitTheme::Night));
    }
}
