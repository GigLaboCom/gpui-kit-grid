//! The ladder's commands, as data: each has one key, one label and one
//! Action. The key bindings, the buttons and the key legend in the status
//! bar all come from [`Command::ALL`], and every entry point runs the same
//! `Screen::run`.

use gpui_kit::KeyBinding;

gpui_kit::actions!(
    ladder,
    [
        /// Centres the last price on the ladder.
        Recentre,
        /// The next larger order quantity.
        QuantityUp,
        /// The next smaller order quantity.
        QuantityDown,
        /// Buys the order quantity at the market.
        BuyMarket,
        /// Sells the order quantity at the market.
        SellMarket,
        /// Cancels every working order.
        CancelAll,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Recentre,
    QuantityUp,
    QuantityDown,
    BuyMarket,
    SellMarket,
    CancelAll,
}

impl Command {
    /// Every command, in the order of the key legend.
    pub const ALL: [Self; 6] = [
        Self::BuyMarket,
        Self::SellMarket,
        Self::CancelAll,
        Self::QuantityDown,
        Self::QuantityUp,
        Self::Recentre,
    ];

    /// The key, in GPUI's keystroke syntax. Not one of the grid's own
    /// (arrows, Page, Home/End, Enter, Escape, Space, `+`, `-`): the
    /// ladder's grid keeps them while it has the focus. The market orders
    /// and cancel all take Shift, so a stray key does not trade.
    pub fn key(self) -> &'static str {
        match self {
            Self::Recentre => "c",
            Self::QuantityUp => "]",
            Self::QuantityDown => "[",
            Self::BuyMarket => "shift-b",
            Self::SellMarket => "shift-s",
            Self::CancelAll => "shift-x",
        }
    }

    /// What its button and the key legend say.
    pub fn label(self) -> &'static str {
        match self {
            Self::Recentre => "Recentre",
            Self::QuantityUp => "Qty up",
            Self::QuantityDown => "Qty down",
            Self::BuyMarket => "Buy Mkt",
            Self::SellMarket => "Sell Mkt",
            Self::CancelAll => "Cancel all",
        }
    }

    /// The command's key binding under `context`.
    fn binding(self, context: &'static str) -> KeyBinding {
        let context = Some(context);
        match self {
            Self::Recentre => KeyBinding::new(self.key(), Recentre, context),
            Self::QuantityUp => KeyBinding::new(self.key(), QuantityUp, context),
            Self::QuantityDown => KeyBinding::new(self.key(), QuantityDown, context),
            Self::BuyMarket => KeyBinding::new(self.key(), BuyMarket, context),
            Self::SellMarket => KeyBinding::new(self.key(), SellMarket, context),
            Self::CancelAll => KeyBinding::new(self.key(), CancelAll, context),
        }
    }
}

/// Every command's key binding under `context`.
pub fn bindings(context: &'static str) -> Vec<KeyBinding> {
    Command::ALL
        .into_iter()
        .map(|command| command.binding(context))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::Keystroke;

    use super::*;

    #[test]
    fn test_every_command_has_its_own_key() {
        let keys: HashSet<&str> = Command::ALL.iter().map(|command| command.key()).collect();
        assert_eq!(keys.len(), Command::ALL.len());
        for command in Command::ALL {
            assert!(Keystroke::parse(command.key()).is_ok(), "{command:?}");
        }
    }
}
