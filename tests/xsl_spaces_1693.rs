//! Ledger [#1693](http://localhost:1062/l/default/item/1693): no space gonk's pages render is one
//! xrust drops.
//!
//! Measured on xrust 2.2.0 (what gonk links) by ikigai-cms-web PR 112, and again here through
//! [`render::render`]: a whitespace-only text node inside `<xsl:text>` is dropped, and so is any
//! text node of only no-break spaces (`&#160;`), inside `<xsl:text>` or not. Two defects in
//! xrust's stylesheet stripping, both still in 2.3.0: the loop over the preserve list assigns
//! the STRIP score, so `xsl:text` is never preserved, and its whitespace test is `str::trim`,
//! which counts U+00A0 while XML whitespace is only space, tab, CR and LF. Through 9d4e722 gonk's
//! stylesheet had eight `<xsl:text> </xsl:text>` and every one rendered as nothing: the sign-in
//! notice's sentence ran into its button, the doctor's badge into its item link, a note into its
//! link. A space survives inside non-whitespace text (`<xsl:text> · </xsl:text>`) or from
//! `<xsl:value-of select="' '"/>`, which is what the stylesheet uses now.
//!
//! Two guards, the two ikigai-cms-web added: a scan of every stylesheet under `web/` for a text
//! node xrust drops, and renders through gonk's own stylesheet that assert a space survives.

use ikigai_gonk::render::{self, element, envelope, wrap};

/// The text nodes of an XSLT stylesheet that xrust drops and a reader would expect kept: ones
/// made only of whitespace that sit inside `<xsl:text>`, or that hold a no-break space
/// (`&#160;`, `&#xA0;` or a literal U+00A0). A whitespace-only text node OUTSIDE `<xsl:text>`
/// is stripped by every XSLT processor (XSLT 1.0 section 3.4), so nobody can rely on it and it
/// is not listed.
fn dropped_whitespace_text(xsl: &str) -> Vec<String> {
    // Comments first: the stylesheet's comments quote markup.
    let mut text = String::new();
    let mut rest = xsl;
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    text.push_str(rest);

    let nbsp = |s: &str| {
        s.replace("&#160;", "\u{a0}")
            .replace("&#xA0;", "\u{a0}")
            .replace("&#xa0;", "\u{a0}")
            .replace("&nbsp;", "\u{a0}")
    };
    let mut found = Vec::new();
    let mut in_xsl_text = false;
    for piece in text.split('<').skip(1) {
        // `piece` is one tag up to its `>`, then the text node after it.
        let Some((tag, after)) = piece.split_once('>') else {
            continue;
        };
        if tag.starts_with("xsl:text") && !tag.ends_with('/') {
            in_xsl_text = true;
        } else if tag.starts_with("/xsl:text") {
            in_xsl_text = false;
        }
        let node = nbsp(after);
        let blank = !node.is_empty() && node.chars().all(char::is_whitespace);
        let holds_nbsp = node.contains('\u{a0}');
        if blank && (in_xsl_text || holds_nbsp) {
            found.push(format!("<{tag}>{after:?}"));
        }
    }
    found
}

/// No stylesheet gonk serves relies on a text node xrust drops. Reads every `.xsl` under
/// `web/`, so a new one is covered.
#[test]
fn no_stylesheet_relies_on_whitespace_that_xrust_drops() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("web/ is readable") {
        let path = entry.expect("a directory entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("xsl") {
            continue;
        }
        let xsl = std::fs::read_to_string(&path).expect("a stylesheet reads as UTF-8");
        let found = dropped_whitespace_text(&xsl);
        assert!(
            found.is_empty(),
            "{}: xrust drops these text nodes, so the space never renders; put the space \
             inside non-whitespace text or use <xsl:value-of select=\"' '\"/>: {found:?}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 1, "read no stylesheet from {}", dir.display());

    // The scan itself: it finds each shape xrust drops, and passes the ones that render.
    assert_eq!(
        dropped_whitespace_text("<b>a</b><xsl:text> </xsl:text><b>b</b>").len(),
        1
    );
    assert_eq!(dropped_whitespace_text("<b>a</b>&#160;<b>b</b>").len(), 1);
    assert_eq!(
        dropped_whitespace_text("<xsl:text>&#xA0;</xsl:text>").len(),
        1
    );
    assert!(dropped_whitespace_text("<xsl:text> · </xsl:text>").is_empty());
    assert!(dropped_whitespace_text("<xsl:value-of select=\"' '\"/>").is_empty());
    assert!(dropped_whitespace_text("<p>\n  <b>a</b>\n</p>").is_empty());
    assert!(dropped_whitespace_text("<!-- <xsl:text> </xsl:text> -->").is_empty());
}

/// The sign-in notice a signed-out visitor reads: its sentence, a space, then the button. The
/// satellite that found ledger #1693 named this one.
#[test]
fn the_sign_in_notice_keeps_the_space_before_its_button() {
    let signin = wrap(
        "signin",
        &[
            ("signed-out", "Sign in to write."),
            ("signed-in", "You are signed in."),
        ],
        "",
    );
    let page = envelope(
        "page",
        &[("view", "nothing-else"), ("title", "Empty")],
        &signin,
    );
    let html = render::render(&page, false).expect("the notice renders");
    assert!(
        html.contains("Sign in to write. <button"),
        "the sentence and the button are two words apart: {html}"
    );
}

/// The doctor's problem list: a badge, a space, then the item it names.
#[test]
fn the_doctor_keeps_the_space_between_a_badge_and_its_item() {
    let problem = wrap(
        "problem",
        &[
            ("check", "orphan"),
            ("href", "/l/default/item/7"),
            ("item", "#7"),
        ],
        &(element("detail", &[], "no parent") + &element("remedy", &[], "relink it")),
    );
    let doctor = wrap(
        "doctor",
        &[
            ("ledger", "default"),
            ("ledger-href", "/l/default"),
            ("lifecycle", "default"),
            ("lifecycle-href", "/l/default/lifecycle"),
            ("checked", "just now"),
            ("checked-at", "2026-10-16T00:00:00Z"),
            ("checks", "5"),
            ("count", "1"),
        ],
        &problem,
    );
    let page = envelope("page", &[("view", "doctor")], &doctor);
    let html = render::render(&page, false).expect("the doctor renders");
    assert!(
        html.contains("orphan</span> <a"),
        "the badge and the item link are two words apart: {html}"
    );
}
