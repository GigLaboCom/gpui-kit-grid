//! A theme remix from the page ([`set_theme`](crate::set_theme)): a small,
//! stable set of a gpui-kit theme file's keys laid over the example's own
//! theme. The keys are spelled as in a theme file, so a remix is a valid
//! fragment of one: the fonts, sizes and radius at the top, the colours
//! under `colors`.
//!
//! The set is the contract of the embedded demos (version 1 of `#theme=`):
//! keys are only ever added. A key outside it is ignored with a warning, and
//! a font family the page does not ship falls back to the example's own:
//! the browser has no system fonts, and a family with no face would stop
//! the module.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use gpui_kit::App;
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, try_parse_color};
use serde_json::{Map, Value, json};

use crate::fonts;

/// The font families: a name the page ships (`fonts::shipped_families`).
pub const FAMILY_KEYS: [&str; 2] = ["font.family", "mono_font.family"];

/// The font sizes, in pixels.
pub const SIZE_KEYS: [&str; 2] = ["font.size", "mono_font.size"];

/// The corner radius of controls, in whole pixels.
pub const RADIUS_KEY: &str = "radius";

/// The colours (under `colors`): the surfaces, the table, and the meaning
/// the examples give colours. The grid's change highlight takes rises and
/// inserts from `success.background`, changes from `warning.background`,
/// and falls and removals from `danger.background`.
pub const COLOR_KEYS: [&str; 22] = [
    "background",
    "foreground",
    "border",
    "muted.foreground",
    "accent.background",
    "selection.background",
    "scrollbar.thumb.background",
    "table.background",
    "table.even.background",
    "table.head.background",
    "table.head.foreground",
    "table.hover.background",
    "table.active.background",
    "table.active.border",
    "table.row.border",
    "success.background",
    "danger.background",
    "warning.background",
    "chart.bullish",
    "chart.bearish",
    "base.yellow",
    "base.cyan",
];

/// The sizes a remix may set, in pixels.
const SIZES: std::ops::RangeInclusive<f64> = 8.0..=32.0;

/// The radii a remix may set, in pixels.
const RADII: std::ops::RangeInclusive<u64> = 0..=24;

/// The name of the theme a remix shows: how the next remix knows the theme
/// shown is a remix, and lays itself over the example's theme instead.
const REMIX_NAME: &str = "Remix";

thread_local! {
    /// The example's theme the remixes are laid over, kept from the first.
    static BASE: RefCell<Option<Rc<ThemeConfig>>> = const { RefCell::new(None) };
}

/// The faces (paths under `fonts/`) of the shipped families `json` names
/// that the text system does not have yet: the page fetches them and hands
/// each to [`add_font`] before [`apply`]. Empty for JSON that does not
/// parse, which [`apply`] reports.
pub fn missing_faces(json: &str, cx: &App) -> Vec<&'static str> {
    let Ok(Value::Object(remix)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let installed = installed_families(cx);
    FAMILY_KEYS
        .iter()
        .filter_map(|key| remix.get(*key)?.as_str())
        .filter(|family| !installed.contains(*family))
        .filter_map(fonts::family_faces)
        .flatten()
        .copied()
        .collect()
}

/// Adds one face (a `.ttf` file's bytes) to the text system, for a remix's
/// family.
pub fn add_font(face: Vec<u8>, cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(vec![face.into()]) {
        log::warn!("a remix's face does not load: {error}");
    }
}

/// Shows the example's theme with the remix `json` laid over it, and
/// returns what it did: `{applied, ignored: [{key, reason}],
/// fonts_replaced}`. `{}` shows the example's theme again. JSON that does
/// not parse, or that is not an object, is an error and changes nothing.
pub fn apply(json: &str, cx: &mut App) -> Result<Value, String> {
    let remix: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let Value::Object(remix) = remix else {
        return Err("a theme remix is a JSON object".into());
    };
    let base = base(cx);
    let mut config = serde_json::to_value(&*base).map_err(|error| error.to_string())?;
    let mut applied = Vec::new();
    let mut ignored = Vec::new();
    for (key, value) in &remix {
        match check(key, value) {
            Ok(()) if key == "colors" => {}
            Ok(()) => {
                config[key.as_str()] = value.clone();
                applied.push(key.clone());
            }
            Err(reason) => ignored.push((key.clone(), reason)),
        }
    }
    if let Some(colors) = remix.get("colors").and_then(Value::as_object) {
        for (key, value) in colors {
            match check_color(key, value) {
                Ok(()) => {
                    config["colors"][key.as_str()] = value.clone();
                    applied.push(format!("colors.{key}"));
                }
                Err(reason) => ignored.push((format!("colors.{key}"), reason)),
            }
        }
    }
    let fonts_replaced = replace_missing_families(&mut config, &base, cx);
    config["name"] = REMIX_NAME.into();
    let config: ThemeConfig = serde_json::from_value(config).map_err(|error| error.to_string())?;
    Theme::update(cx, |theme| theme.apply_config(&Rc::new(config)));
    cx.refresh_windows();
    for (key, reason) in &ignored {
        log::warn!("theme remix: {key} ignored: {reason}");
    }
    Ok(json!({
        "applied": applied,
        "ignored": ignored
            .into_iter()
            .map(|(key, reason)| json!({ "key": key, "reason": reason }))
            .collect::<Vec<_>>(),
        "fonts_replaced": fonts_replaced,
    }))
}

/// The remix keys of the theme shown now, as a remix: what an editor
/// starts from.
pub fn current(cx: &App) -> Value {
    let theme = shown(cx);
    let Ok(Value::Object(config)) = serde_json::to_value(&*theme) else {
        return Value::Null;
    };
    let mut remix = Map::new();
    for key in FAMILY_KEYS.iter().chain(&SIZE_KEYS).chain([&RADIUS_KEY]) {
        if let Some(value) = config.get(*key).filter(|value| !value.is_null()) {
            remix.insert((*key).into(), value.clone());
        }
    }
    let colors: Map<String, Value> = COLOR_KEYS
        .iter()
        .filter_map(|key| {
            let value = config.get("colors")?.get(*key)?;
            (!value.is_null()).then(|| ((*key).into(), value.clone()))
        })
        .collect();
    remix.insert("colors".into(), colors.into());
    remix.into()
}

/// The theme config shown now, light or dark.
fn shown(cx: &App) -> Rc<ThemeConfig> {
    let theme = cx.theme();
    if theme.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    }
}

/// The example's theme a remix is laid over: the one shown, unless that is
/// a remix already.
fn base(cx: &App) -> Rc<ThemeConfig> {
    let shown = shown(cx);
    BASE.with_borrow_mut(|base| {
        if shown.name.as_ref() != REMIX_NAME || base.is_none() {
            *base = Some(shown);
        }
        base.clone().expect("set above")
    })
}

/// Whether the top-level `key` may be set to `value`; why not otherwise.
fn check(key: &str, value: &Value) -> Result<(), String> {
    if key == "colors" {
        return value
            .is_object()
            .then_some(())
            .ok_or_else(|| "not an object of colours".into());
    }
    if FAMILY_KEYS.contains(&key) {
        return value
            .is_string()
            .then_some(())
            .ok_or_else(|| "not a font family's name".into());
    }
    if SIZE_KEYS.contains(&key) {
        return match value.as_f64() {
            Some(size) if SIZES.contains(&size) => Ok(()),
            _ => Err(format!(
                "not a size from {} to {} px",
                SIZES.start(),
                SIZES.end()
            )),
        };
    }
    if key == RADIUS_KEY {
        return match value.as_u64() {
            Some(radius) if RADII.contains(&radius) => Ok(()),
            _ => Err(format!(
                "not a whole number from {} to {} px",
                RADII.start(),
                RADII.end()
            )),
        };
    }
    if COLOR_KEYS.contains(&key) {
        return Err("a colour: put it under \"colors\"".into());
    }
    Err("not a key of the remix".into())
}

/// Whether the colour `key` may be set to `value`; why not otherwise.
fn check_color(key: &str, value: &Value) -> Result<(), String> {
    if !COLOR_KEYS.contains(&key) {
        return Err("not a colour of the remix".into());
    }
    match value.as_str() {
        Some(color) if try_parse_color(color).is_ok() => Ok(()),
        _ => Err("not a colour (#rrggbb or #rrggbbaa)".into()),
    }
}

/// Puts the base theme's family in place of a family the text system does
/// not have; returns the families replaced.
fn replace_missing_families(config: &mut Value, base: &ThemeConfig, cx: &App) -> Vec<String> {
    let installed = installed_families(cx);
    let mut replaced = Vec::new();
    for (key, fallback) in [
        (FAMILY_KEYS[0], &base.font_family),
        (FAMILY_KEYS[1], &base.mono_font_family),
    ] {
        if let Some(family) = config.get(key).and_then(Value::as_str)
            && !installed.contains(family)
        {
            replaced.push(family.to_owned());
            config[key] = fallback
                .as_ref()
                .map_or(Value::Null, |family| family.to_string().into());
        }
    }
    replaced
}

fn installed_families(cx: &App) -> HashSet<String> {
    cx.text_system().all_font_names().into_iter().collect()
}
