//! The broker's themes: gpui-kit theme files in `themes/`, registered with
//! their font by the trading examples' `theme::install`. Light is the
//! default; the top bar's toggle and a key switch to the other one.

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokerTheme {
    /// A near-white canvas, white cards with soft borders, an indigo
    /// accent; Manrope.
    Light,
    /// Soft charcoal, cards a step lighter, a lighter indigo; Manrope.
    Dark,
}

impl BrokerTheme {
    /// The other theme.
    pub fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

impl TradingTheme for BrokerTheme {
    const ALL: &'static [Self] = &[Self::Light, Self::Dark];

    fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Light => "Broker Light",
            Self::Dark => "Broker Dark",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Light => include_str!("themes/light.json"),
            Self::Dark => include_str!("themes/dark.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_files_parse_light_first() {
        for (ix, theme) in BrokerTheme::ALL.iter().enumerate() {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            let config = &set.themes[0];
            assert_eq!(config.name.as_ref(), theme.name());
            assert_eq!(config.mode.is_dark(), ix == 1, "{}", theme.name());
            assert_eq!(config.font_family.as_deref(), Some("Manrope"));
            assert_eq!(config.mono_font_family.as_deref(), Some("Manrope"));
        }
        assert_eq!(BrokerTheme::parse("dark"), Some(BrokerTheme::Dark));
        assert_eq!(BrokerTheme::Light.toggled().toggled(), BrokerTheme::Light);
    }

    /// The change paint tints with the palette's `success` and `danger`,
    /// the text takes `base.green` and `base.red`, the ticket's sides the
    /// `success` and `danger` buttons: one green and one red.
    #[test]
    fn test_success_and_danger_are_the_base_green_and_red() {
        for theme in BrokerTheme::ALL {
            let file: serde_json::Value = serde_json::from_str(theme.file()).expect("JSON");
            let colors = &file["themes"][0]["colors"];
            assert_eq!(colors["success.background"], colors["base.green"]);
            assert_eq!(colors["danger.background"], colors["base.red"]);
            assert_eq!(colors["chart.bullish"], colors["base.green"]);
            assert_eq!(colors["chart.bearish"], colors["base.red"]);
            // Buy and Sell are filled with them (`ticket.rs`).
            assert_eq!(colors["button.success.background"], colors["base.green"]);
            assert_eq!(colors["button.danger.background"], colors["base.red"]);
        }
    }
}
