//! An example's themes: its own gpui-kit theme files, registered with the
//! fonts they name, loaded from `examples/fonts/` (`fonts.rs`) when
//! `script/fetch-example-fonts` has downloaded them.
//!
//! A theme names two families: `font.family` for the interface and
//! `mono_font.family` for the numbers of the grids. A family that is not
//! installed falls back to the system's monospace font (or, for an
//! interface font that differs from the theme's monospace one, to the
//! system interface font), with one hint on stderr, so an example always
//! runs.

use std::collections::{BTreeSet, HashSet};

use gpui_kit::App;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeRegistry};

use super::fonts;

/// The themes of one example, usually an enum: each a gpui-kit theme file
/// with one dark or light theme in it.
pub trait TradingTheme: Copy + PartialEq + 'static {
    /// Every theme, the default first.
    const ALL: &'static [Self];

    /// The short name a `THEME` command takes.
    fn label(self) -> &'static str;

    /// The theme's name in its file and in the registry.
    fn name(self) -> &'static str;

    /// The theme file's JSON, usually `include_str!`.
    fn file(self) -> &'static str;

    /// `amber`, `Depth`, `HEAT`: the theme of a label, ignoring case.
    fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|theme| theme.label().eq_ignore_ascii_case(label))
    }

    /// The theme of this example shown now, if one is.
    fn current(cx: &App) -> Option<Self> {
        let name = cx.theme().theme_name();
        Self::ALL
            .iter()
            .copied()
            .find(|theme| theme.name() == name.as_ref())
    }

    /// Shows this theme in every window.
    fn apply(self, cx: &mut App) {
        let Some(config) = ThemeRegistry::global(cx).themes().get(self.name()).cloned() else {
            err!("theme {} is not registered", self.name());
            return;
        };
        Theme::update(cx, |theme| theme.apply_config(&config));
    }
}

/// Loads the downloaded fonts and registers every theme of `T`, each
/// family that is not installed replaced by a system font; says on stdout
/// how many faces were loaded, or on stderr which families are missing.
/// Call once, after `gpui_kit::init`.
///
/// The browser has no system font to stand in for a missing one: there a
/// theme whose fonts the page has not handed over yet is left out, and an
/// `install` after it has (`fonts::provide`) registers it.
pub fn install<T: TradingTheme>(cx: &mut App) {
    let faces = fonts::load(cx);
    let installed: HashSet<String> = cx.text_system().all_font_names().into_iter().collect();
    let system_ui = cx.theme().font_family.to_string();
    let system_mono = cx.theme().mono_font_family.to_string();
    let mut missing = BTreeSet::new();
    for theme in T::ALL {
        let file = match with_installed_fonts(theme.file(), &installed, &system_ui, &system_mono) {
            #[cfg(target_family = "wasm")]
            Ok((_, absent)) if !absent.is_empty() => {
                missing.extend(absent);
                continue;
            }
            Ok((file, absent)) => {
                missing.extend(absent);
                file
            }
            Err(error) => {
                err!("theme {}: {error}", theme.name());
                continue;
            }
        };
        if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(&file) {
            err!("theme {}: {error}", theme.name());
        }
    }
    if missing.is_empty() {
        out!("fonts: {faces} faces from {}", fonts::FONTS_DIR);
    } else {
        let names: Vec<&str> = missing.iter().map(String::as_str).collect();
        #[cfg(not(target_family = "wasm"))]
        err!(
            "fonts missing ({}): run {}. Using {system_mono} meanwhile.",
            names.join(", "),
            fonts::FETCH_SCRIPT
        );
        #[cfg(target_family = "wasm")]
        out!("themes waiting for their fonts ({})", names.join(", "));
    }
}

/// The theme file with every family that is not in `installed` replaced:
/// the monospace family by `system_mono`; the interface family by
/// `system_mono` too when it is the theme's monospace family, else by
/// `system_ui`. Returns the new file and the families replaced.
pub fn with_installed_fonts(
    file: &str,
    installed: &HashSet<String>,
    system_ui: &str,
    system_mono: &str,
) -> Result<(String, Vec<String>), serde_json::Error> {
    let mut set: serde_json::Value = serde_json::from_str(file)?;
    let mut missing = Vec::new();
    if let Some(themes) = set
        .get_mut("themes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for theme in themes {
            let family = |key: &str| {
                theme
                    .get(key)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            };
            let (ui, mono) = (family("font.family"), family("mono_font.family"));
            let replace = |name: &Option<String>| {
                name.as_ref()
                    .filter(|name| !installed.contains(name.as_str()))
                    .cloned()
            };
            if let Some(name) = replace(&mono) {
                theme["mono_font.family"] = system_mono.into();
                missing.push(name);
            }
            if let Some(name) = replace(&ui) {
                let fallback = if ui == mono { system_mono } else { system_ui };
                theme["font.family"] = fallback.into();
                missing.push(name);
            }
        }
    }
    Ok((set.to_string(), missing))
}
