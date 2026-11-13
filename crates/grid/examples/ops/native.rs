//! What only the native example has: its settings from the environment,
//! and the alert console's layout in a file (`OPS_LAYOUT`). The browser
//! build reads its settings from the page's address and keeps no layout.

use std::{path::PathBuf, rc::Rc};

use super::{Settings, alerts::LayoutStore, sim};

/// The settings the environment sets (see `main.rs`).
pub fn settings_from_env() -> Settings {
    let number = |name: &str| {
        std::env::var(name).ok().and_then(|value| {
            value
                .parse()
                .inspect_err(|_| err!("{name}={value}: not a number; ignored"))
                .ok()
        })
    };
    Settings {
        hosts: number("OPS_HOSTS").map_or(sim::DEFAULT_HOSTS, |hosts: u64| hosts as u32),
        seed: number("OPS_SEED"),
        storm_seconds: number("OPS_STORM_SECONDS").unwrap_or(super::STORM_SECONDS),
        layout: std::env::var_os("OPS_LAYOUT")
            .map(|path| Rc::new(LayoutFile(PathBuf::from(path))) as Rc<dyn LayoutStore>),
        quit_after: number("OPS_SECONDS"),
    }
}

/// The alert console's layout in a file, read at the start and written
/// after every finished gesture that changes it.
struct LayoutFile(PathBuf);

impl LayoutStore for LayoutFile {
    fn load(&self) -> Option<String> {
        std::fs::read_to_string(&self.0).ok()
    }

    fn save(&self, layout: &str) -> Result<(), String> {
        std::fs::write(&self.0, layout).map_err(|error| error.to_string())
    }

    fn name(&self) -> String {
        self.0.display().to_string()
    }
}
