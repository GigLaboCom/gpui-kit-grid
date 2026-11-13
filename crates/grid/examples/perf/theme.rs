//! The monitor's theme: the `ops` example's Graphite (`../ops/themes/`),
//! registered with its fonts by the trading examples' `theme::install`.
//! Heat is `base.magenta`, as in `ops`; the change tint is the accent.

use gpui_kit::{Hsla, component::Theme};

use crate::trading::theme::TradingTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfTheme {
    Graphite,
}

impl TradingTheme for PerfTheme {
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
            Self::Graphite => include_str!("../ops/themes/graphite.json"),
        }
    }
}

/// How strongly the heat colour shows at the top of its range.
const HEAT_OPACITY: f32 = 0.62;

/// A heat tint for `value` between `cool` (none) and `hot` (full), in
/// `base.magenta`; `None` up to `cool`.
pub fn heat(value: f64, cool: f64, hot: f64, theme: &Theme) -> Option<Hsla> {
    (value > cool).then(|| {
        let level = ((value - cool) / (hot - cool)).clamp(0., 1.) as f32;
        theme.magenta.opacity(0.08 + level * (HEAT_OPACITY - 0.08))
    })
}
