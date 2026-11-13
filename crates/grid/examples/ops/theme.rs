//! The console's theme: a gpui-kit theme file in `themes/`, registered
//! with its fonts by the trading examples' `theme::install`.
//!
//! Graphite surfaces with a violet accent for focus, selection and change
//! highlight; the status colours belong to status alone, worst first:
//! `danger` (critical), `base.red.light` set to orange (major; the theme
//! has four status tokens for five levels), `warning` (minor), `info`
//! (warning) and `success` (OK). Heat is `base.magenta`, outside both.
//! Barlow Semi Condensed for the interface, Iosevka for the grids.

use gpui_kit::{Hsla, component::Theme};

use super::rows::Severity;
use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpsTheme {
    Graphite,
}

impl TradingTheme for OpsTheme {
    const ALL: &'static [Self] = &[Self::Graphite];

    fn label(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Graphite => "Ops Graphite",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Graphite => include_str!("themes/graphite.json"),
        }
    }
}

/// The status colour of `severity`: the one place severities meet
/// colours, for the grids, the chips and the chart alike.
pub fn status_color(severity: Severity, theme: &Theme) -> Hsla {
    match severity {
        Severity::Critical => theme.danger,
        Severity::Major => theme.red_light,
        Severity::Minor => theme.warning,
        Severity::Warning => theme.info,
        Severity::Ok => theme.success,
    }
}

/// How strongly the heat colour shows at the top of its range.
const HEAT_OPACITY: f32 = 0.62;

/// A heat tint for `value` between `cool` (none) and `hot` (full), in
/// `base.magenta`; `None` below `cool`.
pub fn heat(value: f64, cool: f64, hot: f64, theme: &Theme) -> Option<Hsla> {
    (value > cool).then(|| {
        let level = ((value - cool) / (hot - cool)).clamp(0., 1.) as f32;
        theme.magenta.opacity(0.08 + level * (HEAT_OPACITY - 0.08))
    })
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;

    use super::*;

    #[test]
    fn test_theme_file_parses() {
        for theme in OpsTheme::ALL {
            let set: ThemeSet = serde_json::from_str(theme.file()).expect("a gpui-kit theme file");
            assert_eq!(set.themes.len(), 1);
            assert_eq!(set.themes[0].name.as_ref(), theme.name());
            assert!(set.themes[0].mode.is_dark());
            assert_eq!(
                set.themes[0].mono_font_family.as_deref(),
                Some("Iosevka"),
                "Iosevka in the grids"
            );
        }
        assert_eq!(OpsTheme::parse("graphite"), Some(OpsTheme::Graphite));
    }
}
