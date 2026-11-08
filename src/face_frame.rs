//! gonk's frame around a face (ledger [#3279](http://localhost:1062/l/default/item/3279)): gonk
//! owns the frame, and a face stays a plain static page. When [`crate::face::FaceFile`] serves a
//! `text/html` file it writes, into a copy of the bytes:
//!
//! - in `<head>`, before the face's first stylesheet: [`SHEET`] and ikigai-ui's `shell.js`, so the
//!   theme chosen in gonk's header (`ikigai-ui:theme`) reaches this page's `<html data-theme>`;
//! - on `<html>`, `data-theme='auto'` when the face set none, since `shell.js` applies a
//!   remembered theme only over `auto`. A face that names its own theme keeps it;
//! - right after `<body …>`: the bar, a link home, the face's name marked "add-on", and the
//!   add-on row with this face `aria-current="page"`.
//!
//! No inline script or style, so the door's CSP is unchanged.
//!
//! # ★ Never break a face
//!
//! The scan is a small tokenizer over the bytes, not a substring search: a `<body>` in a comment
//! or a script's string is not the tag, and a `>` in a quoted attribute does not end it. It finds
//! the seams only in the part of a page every HTML parser reads the same way (doctype, comments,
//! `<html>`, `<head>` and head elements) up to an explicit `<body>`. Anything else, a page with no
//! `<body>` tag or with content before it, is served byte for byte as written.

use std::sync::LazyLock;

use ikigai_ui::palette::{Palette, DARK, HIGH_CONTRAST, LIGHT};

use crate::face::PATH;

/// The bar's stylesheet's name under `/static/`.
pub const SHEET: &str = "gonk-frame.css";

/// The one id everything in the bar is scoped under.
pub const ID: &str = "gonk-frame";

const LAYOUT: &str = include_str!("../web/gonk-frame.css");

/// One palette's tokens on the bar and each thing in it, under `root`. Each element gets them
/// directly, so a face's `* { --fg: … }` cannot reach what the bar inherits.
fn palette_rule(roots: &[&str], palette: &Palette) -> String {
    let selector = roots
        .iter()
        .map(|root| format!("{root}#{ID}, {root}#{ID} *"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{selector} {{\n{}\n}}\n", palette.declarations())
}

/// The bar's sheet: its layout, then ikigai-ui's palettes scoped to the bar, selected the way
/// `tokens.css` selects them. gonk writes no color of its own.
pub fn stylesheet() -> &'static str {
    static SHEET_TEXT: LazyLock<String> = LazyLock::new(|| {
        let auto = [":root:not([data-theme]) ", ":root[data-theme=\"auto\"] "];
        let dark = palette_rule(&auto, &DARK);
        let contrast = palette_rule(&auto, &HIGH_CONTRAST);
        format!(
            "{LAYOUT}\n{}\n@media (prefers-color-scheme: dark) {{\n{dark}}}\n\n\
             @media (prefers-contrast: more) {{\n{contrast}}}\n\n{}{}{}",
            palette_rule(&[""], &LIGHT),
            palette_rule(&[":root[data-theme=\"light\"] "], &LIGHT),
            palette_rule(&[":root[data-theme=\"dark\"] "], &DARK),
            palette_rule(&[":root[data-theme=\"high-contrast\"] "], &HIGH_CONTRAST),
        )
    });
    &SHEET_TEXT
}

/// What gonk writes into one face's pages: built once, when the face is bound. Face names are
/// lowercase letters, digits and `-` ([`crate::face::Face::at`]), so none needs escaping.
#[derive(Debug, Clone)]
pub struct Frame {
    head: String,
    bar: String,
}

impl Frame {
    /// The frame for the face `current`, listing `roster` (every face, in config order).
    pub fn new(current: &str, roster: &[String]) -> Frame {
        let links: String = roster
            .iter()
            .map(|name| {
                let mark = if name == current {
                    " aria-current=\"page\""
                } else {
                    ""
                };
                format!("<li><a href=\"{PATH}/{name}/\"{mark}>{name}</a></li>")
            })
            .collect();
        Frame {
            head: format!(
                "<link rel=\"stylesheet\" href=\"/static/{SHEET}\">\
                 <script src=\"/static/shell.js\"></script>"
            ),
            bar: format!(
                "<nav id=\"{ID}\" aria-label=\"gonk\">\
                 <a class=\"gonk-frame-home\" href=\"/\">gonk</a> \
                 <span class=\"gonk-frame-face\">{current}</span> \
                 <span class=\"gonk-frame-mark\">add-on</span> \
                 <nav aria-label=\"Add-ons\"><span>Add-ons</span> <ul>{links}</ul></nav>\
                 </nav>"
            ),
        }
    }

    /// `page` with the frame written in, or `None` when it has no seams to write at and must be
    /// served as written.
    pub fn apply(&self, page: &[u8]) -> Option<Vec<u8>> {
        let seams = seams(page)?;
        let theme = match seams.theme {
            Theme::Attribute(_) => " data-theme='auto'",
            Theme::Element(_) => "<html data-theme='auto'>",
            Theme::Named => "",
        };
        let extra = theme.len() + self.head.len() + self.bar.len();
        let mut out = Vec::with_capacity(page.len() + extra);
        let mut from = 0;
        let mut write = |at: usize, text: &str| {
            out.extend_from_slice(&page[from..at]);
            out.extend_from_slice(text.as_bytes());
            from = at;
        };
        if let Theme::Attribute(at) | Theme::Element(at) = seams.theme {
            write(at, theme);
        }
        write(seams.head, &self.head);
        write(seams.body, &self.bar);
        out.extend_from_slice(&page[from..]);
        Some(out)
    }
}

/// Where a page takes the theme.
#[derive(Debug, PartialEq, Eq)]
enum Theme {
    /// Its `<html>` has no `data-theme`: the offset just after the tag's name.
    Attribute(usize),
    /// It has no `<html>` tag: the offset of its first element.
    Element(usize),
    /// Its `<html>` names one already, which is the face's to keep.
    Named,
}

/// The three places a page is written into, in page order.
#[derive(Debug, PartialEq, Eq)]
struct Seams {
    theme: Theme,
    /// Before the first head element a face could style or script with, after any leading
    /// `<meta>`: `<meta charset>` has to stay inside the first 1024 bytes.
    head: usize,
    /// Just after the `<body …>` tag's `>`.
    body: usize,
}

/// A start or end tag, its name lowercased.
struct Tag {
    name: String,
    closing: bool,
    name_end: usize,
    end: usize,
    has_theme: bool,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn find(page: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    page.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// The tag at `at` (a `<`), attribute by attribute, so a `>` in a quoted value does not end it.
/// `None` when it is not a tag or the page ends inside it.
fn tag(page: &[u8], at: usize) -> Option<Tag> {
    let closing = page.get(at + 1) == Some(&b'/');
    let start = at + 1 + usize::from(closing);
    if !page.get(start)?.is_ascii_alphabetic() {
        return None;
    }
    let mut i = start;
    while !is_space(*page.get(i)?) && !matches!(page[i], b'/' | b'>') {
        i += 1;
    }
    let name = String::from_utf8_lossy(&page[start..i]).to_ascii_lowercase();
    let name_end = i;
    let mut has_theme = false;
    loop {
        while is_space(*page.get(i)?) || page[i] == b'/' {
            i += 1;
        }
        if page[i] == b'>' {
            return Some(Tag {
                name,
                closing,
                name_end,
                end: i + 1,
                has_theme,
            });
        }
        let attribute = i;
        i += 1; // A leading `=` is part of the name.
        while !is_space(*page.get(i)?) && !matches!(page[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        has_theme |= page[attribute..i].eq_ignore_ascii_case(b"data-theme");
        while is_space(*page.get(i)?) {
            i += 1;
        }
        if page[i] != b'=' {
            continue;
        }
        i += 1;
        while is_space(*page.get(i)?) {
            i += 1;
        }
        match page[i] {
            quote @ (b'"' | b'\'') => i = find(page, i + 1, &[quote])? + 1,
            b'>' => {}
            _ => {
                while !is_space(*page.get(i)?) && page[i] != b'>' {
                    i += 1;
                }
            }
        }
    }
}

/// Past the end tag closing the raw-text element `name`, whose content is not markup.
fn past_raw_text(page: &[u8], from: usize, name: &str) -> Option<usize> {
    let close = format!("</{name}");
    let mut at = from;
    loop {
        let found = page
            .get(at..)?
            .windows(close.len())
            .position(|window| window.eq_ignore_ascii_case(close.as_bytes()))?
            + at;
        let after = *page.get(found + close.len())?;
        if is_space(after) || matches!(after, b'/' | b'>') {
            return Some(tag(page, found)?.end);
        }
        at = found + close.len();
    }
}

/// The page's seams, or `None` when it has no explicit `<body>` every parser would agree on.
fn seams(page: &[u8]) -> Option<Seams> {
    let mut at = if page.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let mut theme = None;
    let mut first_element = None;
    let mut head = None;
    loop {
        while is_space(*page.get(at)?) {
            at += 1;
        }
        if page[at] != b'<' {
            return None; // Text here starts the body with no tag for it.
        }
        if page[at..].starts_with(b"<!--") {
            at = find(page, at + 2, b"-->")? + 3;
            continue;
        }
        if matches!(page.get(at + 1)?, b'!' | b'?') {
            at = find(page, at, b">")? + 1;
            continue;
        }
        let found = tag(page, at)?;
        let start = at;
        at = found.end;
        if found.closing {
            if found.name != "head" {
                return None;
            }
            head.get_or_insert(start);
            continue;
        }
        let first = *first_element.get_or_insert(start);
        match found.name.as_str() {
            "html" if start == first => {
                theme = Some(if found.has_theme {
                    Theme::Named
                } else {
                    Theme::Attribute(found.name_end)
                });
            }
            "head" if head.is_none() => {}
            "meta" => {}
            "link" | "base" | "basefont" | "bgsound" => {
                head.get_or_insert(start);
            }
            "title" | "style" | "script" | "noscript" | "noframes" | "template" => {
                head.get_or_insert(start);
                at = past_raw_text(page, at, &found.name)?;
            }
            "body" => {
                return Some(Seams {
                    theme: theme.unwrap_or(Theme::Element(first)),
                    head: head.unwrap_or(start),
                    body: found.end,
                });
            }
            _ => return None, // Anything else starts the body with no tag for it.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame::new("ops", &["ops".to_string(), "intake".to_string()])
    }

    fn text(page: &[u8]) -> String {
        String::from_utf8(page.to_vec()).unwrap()
    }

    #[test]
    fn the_seams_of_an_ordinary_page() {
        let page = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
                    <title>T</title><link rel=\"stylesheet\" href=\"a.css\"></head>\
                    <body class=\"x\"><p>hi</p></body></html>";
        let framed = text(&frame().apply(page.as_bytes()).unwrap());
        assert!(framed.starts_with("<!doctype html>\n<html data-theme='auto' lang=\"en\"><head>"));
        let sheet = framed.find("gonk-frame.css").unwrap();
        assert!(framed.find("<meta charset").unwrap() < sheet);
        assert!(sheet < framed.find("shell.js").unwrap());
        assert!(framed.find("shell.js").unwrap() < framed.find("a.css").unwrap());
        assert!(framed.contains("<body class=\"x\"><nav id=\"gonk-frame\""));
        assert!(framed.ends_with("</nav><p>hi</p></body></html>"));
    }

    #[test]
    fn what_looks_like_a_body_is_not_one() {
        for page in [
            "<!-- <body> --><!doctype html><body>",
            "<html><head><script>var b = '<body>'</script></head><body>",
            "<title><body></title><style>a::before{content:'<body>'}</style><body>",
        ] {
            let framed = text(&frame().apply(page.as_bytes()).unwrap());
            assert!(framed.ends_with("<body><nav id=\"gonk-frame\" aria-label=\"gonk\"><a class=\"gonk-frame-home\" href=\"/\">gonk</a> <span class=\"gonk-frame-face\">ops</span> <span class=\"gonk-frame-mark\">add-on</span> <nav aria-label=\"Add-ons\"><span>Add-ons</span> <ul><li><a href=\"/face/name/ops/\" aria-current=\"page\">ops</a></li><li><a href=\"/face/name/intake/\">intake</a></li></ul></nav></nav>"), "{framed}");
        }
    }

    #[test]
    fn a_quoted_angle_bracket_does_not_end_the_body_tag() {
        let page = "<body data-a=\"x>y\" data-b='>'>after";
        let framed = text(&frame().apply(page.as_bytes()).unwrap());
        assert!(
            framed.contains("data-b='>'><nav id=\"gonk-frame\""),
            "{framed}"
        );
    }

    #[test]
    fn a_page_with_no_explicit_body_is_left_alone() {
        for page in [
            "",
            "plain text",
            "<!doctype html><title>x</title><script src=a.js></script>\n",
            "<!doctype html><p>a body with no tag</p><body>",
            "<title>x</title>text<body>",
            "<body",
            "<body class=\"unterminated>",
            "<script>never closed <body>",
            "<!-- never closed <body>",
            "<html></html><body>",
        ] {
            assert_eq!(frame().apply(page.as_bytes()), None, "{page}");
        }
    }

    #[test]
    fn the_theme_seam() {
        let named = "<html DATA-THEME=dark><body>";
        let framed = text(&frame().apply(named.as_bytes()).unwrap());
        assert!(framed.starts_with("<html DATA-THEME=dark>"), "{framed}");
        assert_eq!(framed.matches("data-theme").count(), 0, "{framed}");

        // A value that only mentions the attribute does not count.
        let mention = "<html title=\"data-theme\"><body>";
        let framed = text(&frame().apply(mention.as_bytes()).unwrap());
        assert!(
            framed.starts_with("<html data-theme='auto' title="),
            "{framed}"
        );

        // No <html> tag: one is written before the first element, after the doctype.
        let bare = "<!doctype html><!-- c --><meta charset=utf-8><body>";
        let framed = text(&frame().apply(bare.as_bytes()).unwrap());
        assert!(
            framed.starts_with("<!doctype html><!-- c --><html data-theme='auto'><meta charset"),
            "{framed}"
        );
    }

    #[test]
    fn a_byte_order_mark_and_non_utf8_text_survive() {
        let mut page = b"\xef\xbb\xbf<head><title>caf\xe9</title></head><body>".to_vec();
        page.extend_from_slice(b"\xff");
        let framed = frame().apply(&page).unwrap();
        assert!(framed.starts_with(b"\xef\xbb\xbf<html data-theme='auto'><head>"));
        assert!(framed.windows(5).any(|w| w == b"caf\xe9<"));
        assert!(framed.ends_with(b"</nav></nav>\xff"));
    }

    #[test]
    fn the_stylesheet_scopes_every_palette_to_the_bar() {
        let sheet = stylesheet();
        assert_eq!(sheet.matches("--bg:").count(), 6, "{sheet}");
        assert!(sheet.contains(":root[data-theme=\"auto\"] #gonk-frame *"));
        assert!(!sheet.contains("\n:root {"), "{sheet}");
    }
}
