//! The faces a page fetches for its example's themes, and the face of
//! GPUI's default family. The browser has no installed fonts, and no module
//! embeds any: the page fetches the start theme's faces before `run` and
//! the other themes' after the first rows, from `fonts/` (the build links
//! `crates/grid/examples/fonts/` there, which `script/fetch-example-fonts`
//! fills), and hands each over.

use gpui_kit::component::ThemeSet;

/// The faces of every family the examples' themes name, as
/// `script/fetch-example-fonts` saves them (Iosevka's as the build subsets
/// them): a new family in a theme is one more entry here.
const FAMILIES: [(&str, [&str; 2]); 6] = [
    (
        "IBM Plex Mono",
        [
            "ibm-plex-mono/IBMPlexMono-Regular.ttf",
            "ibm-plex-mono/IBMPlexMono-Bold.ttf",
        ],
    ),
    ("Inter", ["inter/Inter-Regular.ttf", "inter/Inter-Bold.ttf"]),
    (
        "JetBrains Mono",
        [
            "jetbrains-mono/JetBrainsMono-Regular.ttf",
            "jetbrains-mono/JetBrainsMono-Bold.ttf",
        ],
    ),
    (
        "Barlow Semi Condensed",
        [
            "barlow-semi-condensed/BarlowSemiCondensed-Regular.ttf",
            "barlow-semi-condensed/BarlowSemiCondensed-Bold.ttf",
        ],
    ),
    // The subset `scripts/build-wasm.sh` makes (`www/pkg/fonts/`): the
    // whole faces are 8.4 MB each.
    (
        "Iosevka",
        [
            "../pkg/fonts/iosevka/Iosevka-Regular.ttf",
            "../pkg/fonts/iosevka/Iosevka-Bold.ttf",
        ],
    ),
    (
        "Manrope",
        ["manrope/Manrope-Regular.ttf", "manrope/Manrope-Bold.ttf"],
    ),
];

/// The face GPUI's web platform draws its default family, `.SystemUIFont`,
/// in (IBM Plex Sans), fetched with the start theme's on every page: the
/// text that no theme styles draws in it. GPUI draws a drag's view outside
/// the window's root, which sets the theme's family, so a view that sets
/// none (the ghost of a dock tab, gpui-kit's) lays its text out in the
/// default; so does an input whose text is set before its first frame.
/// Without the face that panics, and the page stops.
pub const SYSTEM_UI: &str = "ibm-plex-sans/IBMPlexSans-Regular.ttf";

/// A face with the symbols an example draws that its themes' families
/// lack, which a desktop's system fonts supply natively: the broker's ▲
/// and ▼ (Manrope has neither). The browser has no system fonts; the text
/// system falls back to any face it has.
pub const SYMBOLS: &str = "inter/Inter-Regular.ttf";

/// The faces a page fetches, as paths under `fonts/`: those of the start
/// theme, before the example runs, and those the other themes need
/// besides, after its first rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFiles {
    start: Vec<&'static str>,
    later: Vec<&'static str>,
}

impl FontFiles {
    /// The faces of the families the theme file `start` names, then those
    /// the theme files of `others` name besides (gpui-kit theme files, JSON).
    pub fn from_themes<'a>(start: &str, others: impl IntoIterator<Item = &'a str>) -> Self {
        let start = faces(start);
        let later = others
            .into_iter()
            .flat_map(faces)
            .fold(Vec::new(), |mut later, face| {
                if !start.contains(&face) && !later.contains(&face) {
                    later.push(face);
                }
                later
            });
        Self { start, later }
    }

    /// The same, with `face` fetched before the example runs too, after the
    /// start theme's.
    pub fn with_start_face(mut self, face: &'static str) -> Self {
        self.start.push(face);
        self
    }

    /// The faces fetched before the example runs.
    pub fn start(&self) -> &[&'static str] {
        &self.start
    }

    /// The faces fetched after the first rows.
    pub fn later(&self) -> &[&'static str] {
        &self.later
    }
}

/// The faces of `family` the pages can fetch (paths under `fonts/`),
/// `None` for a family the examples do not ship. GPUI's default face,
/// IBM Plex Sans, is one of them: every page has it.
pub fn family_faces(family: &str) -> Option<&'static [&'static str]> {
    const SYSTEM_UI_FACES: &[&str] = &[SYSTEM_UI];
    if family == "IBM Plex Sans" {
        return Some(SYSTEM_UI_FACES);
    }
    FAMILIES
        .iter()
        .find(|(name, _)| *name == family)
        .map(|(_, files)| files.as_slice())
}

/// The families the pages ship, by name.
pub fn shipped_families() -> impl Iterator<Item = &'static str> {
    FAMILIES
        .iter()
        .map(|(name, _)| *name)
        .chain(["IBM Plex Sans"])
}

/// The faces of the interface and number families a theme file names.
fn faces(file: &str) -> Vec<&'static str> {
    let Ok(set) = serde_json::from_str::<ThemeSet>(file) else {
        log::warn!("a theme file does not parse");
        return Vec::new();
    };
    let mut faces = Vec::new();
    for theme in &set.themes {
        for family in [&theme.font_family, &theme.mono_font_family]
            .into_iter()
            .flatten()
        {
            match FAMILIES.iter().find(|(name, _)| *name == family.as_ref()) {
                Some((_, files)) => {
                    for face in files {
                        if !faces.contains(face) {
                            faces.push(*face);
                        }
                    }
                }
                None => log::warn!("no faces listed for the font family {family}"),
            }
        }
    }
    faces
}
