//! The command line's language: a few short words, case-insensitive.
//!
//! | Command | Does |
//! |---|---|
//! | `<TICKER>` | Makes the instrument current (`NVDA`, `brk.b`) |
//! | `THEME <name>` | Switches the theme: `amber`, `depth`, `heat` |
//! | `RATE <n>` | Sets the simulated updates per second |
//! | `GROUP <column>` / `GROUP OFF` | Groups the watchlist by a column id, or not |
//! | `TF <timeframe>` | The chart's candles: `5s`, `1m`, `5m` |
//! | `TAPE CUR` / `TAPE ALL` | The trades tape shows the current instrument's trades, or every instrument's |
//! | `HELP` | Shows or hides the help strip |

use gpui_kit_grid::GridColumns as _;

use super::theme::DeskTheme;
use crate::trading::{
    chart::Timeframe,
    instruments::{self, InstrumentId},
    market::{MAX_RATE, TapeScope},
    rows::QuoteColumn,
    theme::TradingTheme as _,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Select(InstrumentId),
    Theme(DeskTheme),
    Rate(u64),
    /// `None`: no grouping.
    Group(Option<QuoteColumn>),
    Timeframe(Timeframe),
    Tape(TapeScope),
    Help,
}

/// The lines of the help strip.
pub const HELP: [&str; 9] = [
    "<TICKER>  make an instrument current (NVDA, JPM, BRK.B)",
    "THEME amber | depth | heat    F2 F3 F4",
    "RATE <n>  simulated updates per second",
    "GROUP <column> | OFF  group the watchlist (sector, ticker, ...)",
    "TF 5s | 1m | 5m  the chart's candles    F5 the next",
    "TAPE cur | all  trades of the current instrument, or of all",
    "F1 help   F6 command line   F7 watchlist   F8 news",
    "F9 order book   F10 trades   F12 positions",
    "Every price, trade and headline here is simulated, not market data.",
];

/// The command `text` asks for, or why it asks for none.
pub fn parse(text: &str) -> Result<Command, String> {
    let mut words = text.split_whitespace();
    let Some(verb) = words.next() else {
        return Err("Type a ticker or a command. HELP lists them.".into());
    };
    let argument = words.next();
    if let Some(extra) = words.next() {
        return Err(format!("Unexpected {extra:?}. HELP lists the commands."));
    }
    match (verb.to_ascii_uppercase().as_str(), argument) {
        ("HELP", None) => Ok(Command::Help),
        ("THEME", Some(name)) => DeskTheme::parse(name)
            .map(Command::Theme)
            .ok_or_else(|| format!("No theme {name:?}. Themes: amber, depth, heat.")),
        ("THEME", None) => Err("THEME takes amber, depth or heat.".into()),
        ("RATE", Some(rate)) => match rate.replace('_', "").parse::<u64>() {
            Ok(rate) if rate <= MAX_RATE => Ok(Command::Rate(rate)),
            _ => Err(format!("RATE takes a number from 0 to {MAX_RATE}.")),
        },
        ("RATE", None) => Err(format!("RATE takes a number from 0 to {MAX_RATE}.")),
        ("GROUP", Some(column)) if column.eq_ignore_ascii_case("off") => Ok(Command::Group(None)),
        ("GROUP", Some(column)) => QuoteColumn::all()
            .iter()
            .find(|candidate| candidate.id().eq_ignore_ascii_case(column))
            .map(|column| Command::Group(Some(*column)))
            .ok_or_else(|| format!("No column {column:?}. Columns: {}.", column_ids())),
        ("GROUP", None) => Err(format!("GROUP takes off or a column: {}.", column_ids())),
        ("TF", Some(label)) => Timeframe::parse(label)
            .map(Command::Timeframe)
            .ok_or_else(|| format!("No timeframe {label:?}. Timeframes: 5s, 1m, 5m.")),
        ("TF", None) => Err("TF takes 5s, 1m or 5m.".into()),
        ("TAPE", Some(scope)) if scope.eq_ignore_ascii_case("cur") => {
            Ok(Command::Tape(TapeScope::Current))
        }
        ("TAPE", Some(scope)) if scope.eq_ignore_ascii_case("all") => {
            Ok(Command::Tape(TapeScope::All))
        }
        ("TAPE", _) => Err("TAPE takes cur or all.".into()),
        (_, None) => instruments::find(verb)
            .map(Command::Select)
            .ok_or_else(|| format!("No command or ticker {verb:?}. HELP lists the commands.")),
        _ => Err(format!("No command {verb:?}. HELP lists the commands.")),
    }
}

fn column_ids() -> String {
    let ids: Vec<&str> = QuoteColumn::all()
        .iter()
        .map(|column| column.id())
        .collect();
    ids.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_parse() {
        let nvda = instruments::find("NVDA").expect("NVDA is listed");
        assert_eq!(parse("nvda"), Ok(Command::Select(nvda)));
        assert_eq!(parse(" THEME heat "), Ok(Command::Theme(DeskTheme::Heat)));
        assert_eq!(parse("rate 10_000"), Ok(Command::Rate(10_000)));
        assert_eq!(
            parse("group SECTOR"),
            Ok(Command::Group(Some(QuoteColumn::Sector)))
        );
        assert_eq!(parse("GROUP off"), Ok(Command::Group(None)));
        assert_eq!(
            parse("tf 5M"),
            Ok(Command::Timeframe(Timeframe::FiveMinutes))
        );
        assert_eq!(parse("tape ALL"), Ok(Command::Tape(TapeScope::All)));
        assert_eq!(parse("TAPE cur"), Ok(Command::Tape(TapeScope::Current)));
        assert_eq!(parse("help"), Ok(Command::Help));
        for bad in [
            "",
            "THEME neon",
            "RATE fast",
            "GROUP colour",
            "TF 15m",
            "TF",
            "TAPE",
            "TAPE some",
            "ZZZZ",
            "NVDA now",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
