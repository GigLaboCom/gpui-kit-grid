//! One change from Wikimedia's `recentchange` stream, reduced to what the
//! example shows, and the wiki it belongs to: its project family and its
//! language, read from its server name. No GPUI types.
//!
//! **Privacy.** The parser never reads the `user` field, the comment or
//! anything else that names or describes a person: those fields are not in
//! [`Change`], so serde skips them unread and nothing keeps them. A page
//! in a user namespace (`User:`, `User talk:`, whose titles are user names
//! or IP addresses), a special page and a log entry keep no title either,
//! only their counts.
//!
//! **Canary events**, which Wikimedia writes into every stream to tell an
//! empty stream from a broken one, are dropped (`meta.domain` is
//! `canary`), as the stream's documentation asks.

use std::{borrow::Cow, sync::Arc};

use serde::{Deserialize, Serialize};

use super::languages::language_name;

/// What happened: an edit and a new page count as edits of their wiki;
/// the rest only as events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditType {
    Edit,
    New,
    Log,
    Categorize,
    #[serde(other)]
    Other,
}

impl EditType {
    /// An edit of a page or a new page: what the table counts.
    pub fn is_edit(self) -> bool {
        matches!(self, Self::Edit | Self::New)
    }
}

/// A page the example may show and link to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub title: Arc<str>,
    pub url: Arc<str>,
}

/// One change, without its author: the wiki, what happened, whether a bot
/// did it, the change of the page's size and the page, if it may be shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    /// When it arrived, in ms on the clock of the source that delivered it
    /// (`source.rs`); in a recording, since the recording started.
    #[serde(rename = "at")]
    pub at_ms: u64,
    /// The server name, `en.wikipedia.org`: the row's key.
    pub wiki: Arc<str>,
    #[serde(rename = "type")]
    pub kind: EditType,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bot: bool,
    /// New length minus old length, in bytes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delta: i64,
    /// The time of day of the change, UTC, `12:34:56`.
    pub time: Arc<str>,
    /// `None` for a page in a user namespace, a special page or a log
    /// entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<Page>,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// The fields of a `recentchange` event the example reads. Everything else,
/// the user name first, is skipped unread.
#[derive(Deserialize)]
struct Change<'a> {
    #[serde(borrow)]
    meta: Meta<'a>,
    #[serde(rename = "type", default)]
    kind: Option<EditType>,
    #[serde(borrow, default)]
    server_name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    title: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    title_url: Option<Cow<'a, str>>,
    #[serde(default)]
    namespace: Option<i64>,
    #[serde(default)]
    bot: Option<bool>,
    #[serde(default)]
    length: Option<Length>,
}

#[derive(Deserialize)]
struct Meta<'a> {
    #[serde(borrow, default)]
    domain: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    dt: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Length {
    #[serde(default)]
    old: Option<i64>,
    #[serde(default)]
    new: Option<i64>,
}

/// The user namespaces: their page titles are user names or addresses.
const USER: i64 = 2;
const USER_TALK: i64 = 3;

/// Why an event gave no edit.
#[derive(Debug, PartialEq, Eq)]
pub enum Skipped {
    /// A canary event.
    Canary,
    /// Not a change of a wiki: no server name.
    NoWiki,
    /// Not JSON, or not an object with `meta`.
    Malformed,
}

/// Reads one event's `data` (JSON) into an [`Edit`], `at_ms` 0.
pub fn parse_change(data: &str) -> Result<Edit, Skipped> {
    let change: Change = serde_json::from_str(data).map_err(|_| Skipped::Malformed)?;
    if change.meta.domain.as_deref() == Some("canary") {
        return Err(Skipped::Canary);
    }
    let wiki = change
        .server_name
        .filter(|name| !name.is_empty())
        .ok_or(Skipped::NoWiki)?;
    let kind = change.kind.unwrap_or(EditType::Other);
    let (old, new) = change
        .length
        .map_or((None, None), |length| (length.old, length.new));
    let delta = match (kind, old, new) {
        (EditType::New, _, Some(new)) => new,
        (_, Some(old), Some(new)) => new - old,
        _ => 0,
    };
    let namespace = change.namespace.unwrap_or(-1);
    let shown = kind.is_edit() && namespace >= 0 && namespace != USER && namespace != USER_TALK;
    let page = match (shown, change.title, change.title_url) {
        (true, Some(title), Some(url)) if !title.is_empty() && url.starts_with("https://") => {
            Some(Page {
                title: title.into(),
                url: url.into(),
            })
        }
        _ => None,
    };
    Ok(Edit {
        at_ms: 0,
        wiki: wiki.into(),
        kind,
        bot: change.bot.unwrap_or(false),
        delta,
        time: time_of_day(change.meta.dt.as_deref().unwrap_or_default()).into(),
        page,
    })
}

/// `12:34:56` from an ISO 8601 time, `2026-11-07T12:34:56Z`; `--:--:--`
/// if it is not one.
fn time_of_day(dt: &str) -> &str {
    dt.get(11..19)
        .filter(|time| {
            time.bytes().enumerate().all(|(ix, byte)| {
                matches!(
                    (ix, byte),
                    (2 | 5, b':') | (0 | 1 | 3 | 4 | 6 | 7, b'0'..=b'9')
                )
            })
        })
        .unwrap_or("--:--:--")
}

/// The Wikimedia projects, as the table groups the wikis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Wikipedia,
    Wikidata,
    Commons,
    Wiktionary,
    Wikisource,
    Wikibooks,
    Wikiquote,
    Wikinews,
    Wikiversity,
    Wikivoyage,
    Wikispecies,
    Wikifunctions,
    MediaWiki,
    Meta,
    /// Chapters, test wikis, the incubator and the rest.
    Other,
}

impl Family {
    pub const ALL: [Self; 15] = [
        Self::Wikipedia,
        Self::Wikidata,
        Self::Commons,
        Self::Wiktionary,
        Self::Wikisource,
        Self::Wikibooks,
        Self::Wikiquote,
        Self::Wikinews,
        Self::Wikiversity,
        Self::Wikivoyage,
        Self::Wikispecies,
        Self::Wikifunctions,
        Self::MediaWiki,
        Self::Meta,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wikipedia => "Wikipedia",
            Self::Wikidata => "Wikidata",
            Self::Commons => "Commons",
            Self::Wiktionary => "Wiktionary",
            Self::Wikisource => "Wikisource",
            Self::Wikibooks => "Wikibooks",
            Self::Wikiquote => "Wikiquote",
            Self::Wikinews => "Wikinews",
            Self::Wikiversity => "Wikiversity",
            Self::Wikivoyage => "Wikivoyage",
            Self::Wikispecies => "Wikispecies",
            Self::Wikifunctions => "Wikifunctions",
            Self::MediaWiki => "MediaWiki",
            Self::Meta => "Meta-Wiki",
            Self::Other => "Other",
        }
    }

    pub fn ix(self) -> usize {
        self as usize
    }

    /// The projects with a wiki per language, `<lang>.<project>.org`.
    fn by_suffix(server: &str) -> Option<(Self, &str)> {
        const SUFFIXES: [(&str, Family); 8] = [
            (".wikipedia.org", Family::Wikipedia),
            (".wiktionary.org", Family::Wiktionary),
            (".wikisource.org", Family::Wikisource),
            (".wikibooks.org", Family::Wikibooks),
            (".wikiquote.org", Family::Wikiquote),
            (".wikinews.org", Family::Wikinews),
            (".wikiversity.org", Family::Wikiversity),
            (".wikivoyage.org", Family::Wikivoyage),
        ];
        SUFFIXES.iter().find_map(|(suffix, family)| {
            server
                .strip_suffix(suffix)
                .filter(|lang| !lang.is_empty() && !lang.contains('.'))
                .map(|lang| (*family, lang))
        })
    }
}

/// A wiki's family, its language code (empty for a wiki of no one
/// language) and the language's English name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WikiInfo {
    pub family: Family,
    pub lang: Arc<str>,
    pub language: Arc<str>,
}

/// Language subdomains that are not languages.
const NOT_LANGUAGES: [&str; 4] = ["test", "test2", "beta", "www"];

/// What `server` (`en.wikipedia.org`) is.
pub fn wiki_info(server: &str) -> WikiInfo {
    let single = |family: Family| WikiInfo {
        family,
        lang: "".into(),
        language: "Multilingual".into(),
    };
    match server {
        "www.wikidata.org" => return single(Family::Wikidata),
        "commons.wikimedia.org" => return single(Family::Commons),
        "species.wikimedia.org" => return single(Family::Wikispecies),
        "www.wikifunctions.org" => return single(Family::Wikifunctions),
        "www.mediawiki.org" => return single(Family::MediaWiki),
        "meta.wikimedia.org" => return single(Family::Meta),
        "wikisource.org" => return single(Family::Wikisource),
        _ => {}
    }
    match Family::by_suffix(server) {
        Some((family, lang)) if !NOT_LANGUAGES.contains(&lang) => WikiInfo {
            family,
            lang: lang.into(),
            language: language_name(lang).unwrap_or(lang).into(),
        },
        _ => WikiInfo {
            family: Family::Other,
            lang: "".into(),
            language: "".into(),
        },
    }
}

/// Which characters the target can draw in a title: natively every one
/// (the system's fonts stand in for the theme's), in the browser only those
/// of the faces the page loaded, and CJK, which the browser's canvas draws.
/// A title the target cannot draw shows as the name of its script, still
/// linked.
#[derive(Clone, Debug, Default)]
pub enum TitleGlyphs {
    #[default]
    All,
    /// The characters below U+3000 the loaded faces have, as a bit set;
    /// CJK ideographs, kana and Hangul besides.
    Covered(Arc<[u64]>),
}

/// The characters [`TitleGlyphs::Covered`] looks up; everything from here
/// on is CJK or not drawn.
pub const COVERED_BELOW: u32 = 0x3000;

impl TitleGlyphs {
    /// A set from a lookup of every character below [`COVERED_BELOW`].
    pub fn covered(has: impl Fn(char) -> bool) -> Self {
        let mut bits = vec![0u64; COVERED_BELOW as usize / 64];
        for code in 0x20..COVERED_BELOW {
            if char::from_u32(code).is_some_and(&has) {
                bits[code as usize / 64] |= 1 << (code % 64);
            }
        }
        Self::Covered(bits.into())
    }

    fn draws(&self, ch: char) -> bool {
        match self {
            Self::All => true,
            Self::Covered(bits) => {
                let code = ch as u32;
                if code < COVERED_BELOW {
                    bits[code as usize / 64] & (1 << (code % 64)) != 0
                } else {
                    is_cjk(ch)
                }
            }
        }
    }

    /// `title` as the table shows it: itself, or `‹Arabic script›` when the
    /// target cannot draw it.
    pub fn shown(&self, title: &Arc<str>) -> Arc<str> {
        match title.chars().find(|ch| !self.draws(*ch)) {
            None => title.clone(),
            Some(ch) => format!("‹{} script›", script_name(ch)).into(),
        }
    }
}

/// The characters the browser's canvas draws for GPUI's web platform when
/// no loaded face has them (`CanvasFontFallback::EmojiAndCjk`): ideographs,
/// kana, Hangul syllables and jamo, and CJK punctuation.
fn is_cjk(ch: char) -> bool {
    matches!(ch,
        '\u{3001}'..='\u{3002}' | '\u{3008}'..='\u{3011}' | '\u{3014}'..='\u{3015}'
        | '\u{3041}'..='\u{3096}' | '\u{30a1}'..='\u{30fc}' | '\u{3131}'..='\u{3163}'
        | '\u{3400}'..='\u{9fff}' | '\u{ac00}'..='\u{d7a3}' | '\u{f900}'..='\u{faff}'
        | '\u{ff01}'..='\u{ff5e}' | '\u{20000}'..='\u{323af}')
}

/// The script a character not drawn belongs to, for the placeholder.
fn script_name(ch: char) -> &'static str {
    const SCRIPTS: [(u32, u32, &str); 27] = [
        (0x0370, 0x03ff, "Greek"),
        (0x0400, 0x052f, "Cyrillic"),
        (0x0530, 0x058f, "Armenian"),
        (0x0590, 0x05ff, "Hebrew"),
        (0x0600, 0x06ff, "Arabic"),
        (0x0700, 0x074f, "Syriac"),
        (0x0750, 0x077f, "Arabic"),
        (0x0780, 0x07bf, "Thaana"),
        (0x0900, 0x097f, "Devanagari"),
        (0x0980, 0x09ff, "Bengali"),
        (0x0a00, 0x0a7f, "Gurmukhi"),
        (0x0a80, 0x0aff, "Gujarati"),
        (0x0b00, 0x0b7f, "Odia"),
        (0x0b80, 0x0bff, "Tamil"),
        (0x0c00, 0x0c7f, "Telugu"),
        (0x0c80, 0x0cff, "Kannada"),
        (0x0d00, 0x0d7f, "Malayalam"),
        (0x0d80, 0x0dff, "Sinhala"),
        (0x0e00, 0x0e7f, "Thai"),
        (0x0e80, 0x0eff, "Lao"),
        (0x0f00, 0x0fff, "Tibetan"),
        (0x1000, 0x109f, "Myanmar"),
        (0x10a0, 0x10ff, "Georgian"),
        (0x1200, 0x139f, "Ethiopic"),
        (0x1780, 0x17ff, "Khmer"),
        (0x1800, 0x18af, "Mongolian"),
        (0x08a0, 0x08ff, "Arabic"),
    ];
    let code = ch as u32;
    SCRIPTS
        .iter()
        .find(|(from, to, _)| (*from..=*to).contains(&code))
        .map_or("other", |(_, _, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic event as the stream sends it, with the fields the parser
    /// must never keep.
    fn event(namespace: i64, kind: &str, domain: &str) -> String {
        format!(
            r#"{{"$schema":"/mediawiki/recentchange/1.0.0","meta":{{"uri":"https://en.wikipedia.org/wiki/Example","domain":"{domain}","dt":"2026-11-07T12:34:56Z","stream":"mediawiki.recentchange"}},"id":1,"type":"{kind}","namespace":{namespace},"title":"Example \"page\"","title_url":"https://en.wikipedia.org/wiki/Example_%22page%22","comment":"secret comment","timestamp":1794054896,"user":"Some Person","bot":true,"minor":false,"length":{{"old":100,"new":142}},"revision":{{"old":1,"new":2}},"server_url":"https://en.wikipedia.org","server_name":"en.wikipedia.org","wiki":"enwiki"}}"#
        )
    }

    #[test]
    fn test_parse_keeps_what_is_shown_and_no_user() {
        let edit = parse_change(&event(0, "edit", "en.wikipedia.org")).expect("an edit");
        assert_eq!(&*edit.wiki, "en.wikipedia.org");
        assert_eq!(edit.kind, EditType::Edit);
        assert!(edit.bot);
        assert_eq!(edit.delta, 42);
        assert_eq!(&*edit.time, "12:34:56");
        let page = edit.page.as_ref().expect("a page in the main namespace");
        assert_eq!(&*page.title, "Example \"page\"");
        assert_eq!(
            &*page.url,
            "https://en.wikipedia.org/wiki/Example_%22page%22"
        );
        // Nothing the edit keeps, nor its recording, names the author.
        let recorded = serde_json::to_string(&edit).expect("serializes");
        assert!(!recorded.contains("Some Person") && !recorded.contains("secret"));
    }

    #[test]
    fn test_user_pages_logs_and_special_pages_keep_no_title() {
        for (namespace, kind) in [
            (2, "edit"),
            (3, "new"),
            (-1, "log"),
            (0, "log"),
            (4, "categorize"),
        ] {
            let edit = parse_change(&event(namespace, kind, "en.wikipedia.org")).expect("parses");
            assert_eq!(edit.page, None, "namespace {namespace}, {kind}");
        }
        let new = parse_change(&event(0, "new", "en.wikipedia.org")).expect("parses");
        assert_eq!(new.kind, EditType::New);
        assert_eq!(new.delta, 142, "a new page's size is all added");
        assert!(new.page.is_some());
    }

    #[test]
    fn test_canary_and_malformed_events_are_dropped() {
        assert_eq!(
            parse_change(&event(0, "edit", "canary")),
            Err(Skipped::Canary)
        );
        assert_eq!(parse_change("{\"meta\":"), Err(Skipped::Malformed));
        assert_eq!(parse_change("[1,2]"), Err(Skipped::Malformed));
        assert_eq!(
            parse_change(r#"{"meta":{"domain":"x","dt":"bad"},"type":"edit"}"#),
            Err(Skipped::NoWiki)
        );
        let unknown = parse_change(r#"{"meta":{},"type":"external","server_name":"x.org"}"#)
            .expect("an unknown type is kept as an event");
        assert_eq!(unknown.kind, EditType::Other);
        assert_eq!(&*unknown.time, "--:--:--");
    }

    #[test]
    fn test_wiki_info_reads_family_and_language() {
        let info = wiki_info("de.wikipedia.org");
        assert_eq!(info.family, Family::Wikipedia);
        assert_eq!((&*info.lang, &*info.language), ("de", "German"));
        assert_eq!(
            wiki_info("zh-min-nan.wiktionary.org").family,
            Family::Wiktionary
        );
        assert_eq!(
            &*wiki_info("xx-unknown.wikibooks.org").language,
            "xx-unknown"
        );
        assert_eq!(wiki_info("www.wikidata.org").family, Family::Wikidata);
        assert_eq!(wiki_info("commons.wikimedia.org").family, Family::Commons);
        assert_eq!(wiki_info("wikisource.org").family, Family::Wikisource);
        let test = wiki_info("test.wikipedia.org");
        assert_eq!((test.family, &*test.lang), (Family::Other, ""));
        assert_eq!(wiki_info("ru.wikimedia.org").family, Family::Other);
        assert_eq!(wiki_info("wikipedia.org").family, Family::Other);
    }

    #[test]
    fn test_titles_the_target_cannot_draw_name_their_script() {
        let latin_only = TitleGlyphs::covered(|ch| (ch as u32) < 0x0250);
        let shown = |title: &str| latin_only.shown(&Arc::from(title)).to_string();
        assert_eq!(shown("Zürich"), "Zürich");
        assert_eq!(
            shown("東京 とうきょう"),
            "東京 とうきょう",
            "CJK and kana are drawn"
        );
        assert_eq!(shown("Ꭰ"), "‹other script›");
        assert_eq!(shown("القاهرة"), "‹Arabic script›");
        assert_eq!(shown("Москва"), "‹Cyrillic script›");
        let everything = TitleGlyphs::All;
        assert_eq!(&*everything.shown(&Arc::from("القاهرة")), "القاهرة");
    }

    #[test]
    fn test_time_of_day_needs_an_iso_time() {
        assert_eq!(time_of_day("2026-11-07T01:02:03Z"), "01:02:03");
        assert_eq!(time_of_day("2026-11-07"), "--:--:--");
        assert_eq!(time_of_day("2026-11-07Tab:cd:efZ"), "--:--:--");
    }
}
