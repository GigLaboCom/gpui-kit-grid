//! The ladder's theme: one dark gpui-kit theme file in `themes/`,
//! registered with its fonts by the trading examples' `theme::install`.
//!
//! What the ladder reads from it, all semantic tokens:
//!
//! | Token | Colour | On the ladder |
//! |---|---|---|
//! | `background`, `table.background` | `#16191E` | the canvas |
//! | `border`, `table.row.border`, `chart.grid` | `#252932` | grid lines |
//! | `button.success.background`, `success` | `#143828`, `#00E676` | bid depth, the best bid |
//! | `button.danger.background`, `danger` | `#3D1A22`, `#FF1744` | ask depth, the best ask |
//! | `warning` | `#FFD600` | the volume profile's point of control |
//! | `chart.bullish`, `chart.bearish` | `#26A69A`, `#EF5350` | delta: candles, P&L, volume by side |
//! | `chart.1` | `#2E3B52` | the volume profile's shade |
//! | `chart.4` | `#E8EAF0` | the last trade's price |
//! | `base.blue`, `base.magenta` | `#2979FF`, `#E040FB` | the trader's working buys and sells |

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderTheme {
    /// Engineering grey: JetBrains Mono numbers, Inter interface.
    Dark,
}

impl TradingTheme for LadderTheme {
    const ALL: &'static [Self] = &[Self::Dark];

    fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Dark => "Ladder Dark",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Dark => include_str!("themes/dark.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in LadderTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
        }
        assert_eq!(LadderTheme::parse("DARK"), Some(LadderTheme::Dark));
    }
}
