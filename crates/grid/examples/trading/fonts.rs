//! The fonts of the trading examples' themes: one folder for all of them,
//! `examples/fonts/`, filled by `script/fetch-example-fonts` (gitignored,
//! not committed).
//!
//! The browser has neither a file system nor installed fonts: the page
//! (`crates/grid-web`) fetches the faces a theme needs from that folder and
//! hands each over with `provide`, and `load` takes them.

use std::{borrow::Cow, path::Path};

use gpui_kit::App;

/// Where `script/fetch-example-fonts` puts the fonts.
#[cfg(not(target_family = "wasm"))]
pub const FONTS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/fonts");
/// In the browser, the page's link to that folder.
#[cfg(target_family = "wasm")]
pub const FONTS_DIR: &str = "fonts/";

/// The script that downloads them, for the hint when a family is missing.
pub const FETCH_SCRIPT: &str = "script/fetch-example-fonts";

/// Hands every `.ttf` and `.otf` file in `FONTS_DIR` and its subfolders to
/// the text system; in the browser, the faces the page handed over since
/// the last `load`. Returns how many were loaded.
pub fn load(cx: &App) -> usize {
    let mut fonts = Vec::new();
    #[cfg(not(target_family = "wasm"))]
    collect(Path::new(FONTS_DIR), 2, &mut fonts);
    #[cfg(target_family = "wasm")]
    fonts.append(&mut PROVIDED.take());
    let count = fonts.len();
    if count == 0 {
        return 0;
    }
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => count,
        Err(error) => {
            err!("fonts in {FONTS_DIR} not loaded: {error}");
            0
        }
    }
}

#[cfg(target_family = "wasm")]
thread_local! {
    static PROVIDED: std::cell::RefCell<Vec<Cow<'static, [u8]>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// In the browser: one face (a `.ttf` or `.otf` file's bytes) the page
/// fetched, for the next `load`.
#[cfg(target_family = "wasm")]
pub fn provide(face: Vec<u8>) {
    PROVIDED.with_borrow_mut(|faces| faces.push(Cow::Owned(face)));
}

fn collect(dir: &Path, depth: u8, fonts: &mut Vec<Cow<'static, [u8]>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect(&path, depth - 1, fonts);
            }
            continue;
        }
        let font = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "ttf" | "otf"));
        if !font {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => fonts.push(Cow::Owned(bytes)),
            Err(error) => err!("{}: not read: {error}", path.display()),
        }
    }
}
