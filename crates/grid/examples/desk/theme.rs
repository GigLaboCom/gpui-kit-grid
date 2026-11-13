//! The desk's three themes: gpui-kit theme files in `themes/`, registered
//! with their fonts by the trading examples' `theme::install`.

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeskTheme {
    /// Amber data on black, white headers, IBM Plex Mono everywhere.
    Amber,
    /// A slate depth-of-market look: JetBrains Mono numbers, Inter UI.
    Depth,
    /// Navy with a liquidity heat scale: Iosevka numbers, Barlow Semi
    /// Condensed UI.
    Heat,
}

impl TradingTheme for DeskTheme {
    const ALL: &'static [Self] = &[Self::Amber, Self::Depth, Self::Heat];

    fn label(self) -> &'static str {
        match self {
            Self::Amber => "Amber",
            Self::Depth => "Depth",
            Self::Heat => "Heat",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Amber => "Desk Amber",
            Self::Depth => "Desk Depth",
            Self::Heat => "Desk Heat",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Amber => include_str!("themes/amber.json"),
            Self::Depth => include_str!("themes/depth.json"),
            Self::Heat => include_str!("themes/heat.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::component::ThemeSet;

    use super::*;
    use crate::trading::theme::with_installed_fonts;

    fn families(file: &str) -> (Option<String>, Option<String>) {
        let set: ThemeSet = serde_json::from_str(file).expect("a gpui-kit theme file");
        let theme = &set.themes[0];
        (
            theme.font_family.as_ref().map(|family| family.to_string()),
            theme
                .mono_font_family
                .as_ref()
                .map(|family| family.to_string()),
        )
    }

    #[test]
    fn test_theme_files_parse() {
        for theme in DeskTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
        }
        assert_eq!(DeskTheme::parse("heat"), Some(DeskTheme::Heat));
        assert_eq!(DeskTheme::parse("AMBER"), Some(DeskTheme::Amber));
        assert_eq!(DeskTheme::parse("paper"), None);
    }

    #[test]
    fn test_missing_fonts_fall_back_to_system_fonts() {
        let installed = HashSet::from(["Inter".to_owned()]);
        let (file, missing) = with_installed_fonts(
            DeskTheme::Depth.file(),
            &installed,
            "System UI",
            "System Mono",
        )
        .expect("valid JSON");
        assert_eq!(missing, ["JetBrains Mono"]);
        assert_eq!(
            families(&file),
            (Some("Inter".into()), Some("System Mono".into()))
        );
        // Amber is monospace everywhere, and stays so.
        let (file, missing) = with_installed_fonts(
            DeskTheme::Amber.file(),
            &HashSet::new(),
            "System UI",
            "System Mono",
        )
        .expect("valid JSON");
        assert_eq!(missing, ["IBM Plex Mono", "IBM Plex Mono"]);
        assert_eq!(
            families(&file),
            (Some("System Mono".into()), Some("System Mono".into()))
        );
    }
}
