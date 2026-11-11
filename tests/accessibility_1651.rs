//! Ledger [#1651](http://localhost:1062/l/default/item/1651): what the accessibility fan-out found,
//! held where `cargo test` can hold it. The browser's half (the computed name and description, the
//! measured contrast, axe, the forms with JavaScript off) is `tests/browser/queue_badge_1651.mjs`,
//! and the badge's markup is in `tests/queue.rs`.

use std::net::SocketAddr;

use ikigai_gonk::admit;
use ikigai_gonk::doors::{self, HttpDoor, Page};
use ikigai_web::HttpRequest;

const CSS: &str = include_str!("../web/gonk.css");
const XSL: &str = include_str!("../web/gonk.xsl");

/// `css` less its comments.
fn without_comments(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find("*/").expect("a closed comment");
        rest = &rest[at + end + 2..];
    }
    out + rest
}

/// A color through `opacity` is a color no palette test measures: the badge's `+N` was 1.63:1 in
/// light and 1.92:1 in dark while every token in it cleared the floor. So a state is dimmed with a
/// token, which `tests/shared_ui_2834.rs` holds to the floor in every palette, and the one `opacity`
/// left is the spark, an `aria-hidden` dot that carries no text.
#[test]
fn no_text_is_dimmed_with_opacity() {
    let mut dimmed = Vec::new();
    for rule in without_comments(CSS).split('}') {
        let Some((selectors, body)) = rule.rsplit_once('{') else {
            continue;
        };
        if body.contains("opacity") {
            dimmed.push(selectors.trim().to_string());
        }
    }
    assert!(!dimmed.is_empty(), "the spark still pulses");
    for selector in &dimmed {
        assert!(
            selector.starts_with(".spark") || selector.contains("spark-pulse") || selector == "50%",
            "`{selector}` dims with opacity: {dimmed:?}"
        );
    }
    assert!(
        XSL.contains("<span aria-hidden=\"true\">\n          <xsl:attribute name=\"class\">spark ")
    );
}

/// Two of the fan-out's barriers that ledger #2834 closed, which nothing pinned until now.
#[test]
fn the_filters_wrap_and_the_sparql_example_takes_focus() {
    assert!(CSS.contains(".filters { display: flex; flex-wrap: wrap;"));
    assert!(XSL.contains("<pre class=\"mono cross-graph-text\" tabindex=\"0\">"));
}

fn post(headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: "POST".to_string(),
        headers: [("host", "localhost:1062")]
            .iter()
            .chain(headers)
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        path: "/act".to_string(),
        raw_target: "/act".to_string(),
        query: Vec::new(),
        body: Vec::new(),
        peer: None,
    }
}

/// With no script a browser posts a form as a navigation, and under `Referrer-Policy: no-referrer`
/// that POST says `Origin: null`. It is this server's own form only when the browser also labels it
/// `same-origin`: a sandboxed frame and a cross-site redirect send `null` too, labeled otherwise.
#[test]
fn a_plain_forms_null_origin_is_ours_only_when_the_browser_says_same_origin() {
    let bind: SocketAddr = "127.0.0.1:1062".parse().unwrap();
    let door = HttpDoor {
        anonymous: Vec::new(),
        bind,
        passkeys: None,
        public: Vec::new(),
        anonymous_sparql_budget_ms: 0,
    };
    let own = post(&[("origin", "null"), ("sec-fetch-site", "same-origin")]);
    assert_eq!(doors::http_refusal(&door, &own), None);
    assert_eq!(
        doors::page_of(&own, bind),
        Page::Ours,
        "and keeps its grant"
    );

    for headers in [
        &[("origin", "null")][..],
        &[("origin", "null"), ("sec-fetch-site", "cross-site")][..],
        &[("origin", "null"), ("sec-fetch-site", "same-site")][..],
        &[("origin", "null"), ("sec-fetch-site", "none")][..],
    ] {
        let foreign = post(headers);
        assert_eq!(
            doors::http_refusal(&door, &foreign),
            Some(admit::REFUSED_CROSS_SITE),
            "{headers:?}"
        );
        assert_ne!(doors::page_of(&foreign, bind), Page::Ours, "{headers:?}");
    }
}
