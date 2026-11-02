//! Ledger [#2834](http://localhost:1062/l/default/item/2834), UX 4 phase 1: gonk takes ikigai-ui's
//! tokens, shell script and htmx, and keeps its look.
//!
//! What a browser sees is in the pull request (the screenshots and the axe run). What can be held
//! here is the wiring that look and behavior rest on:
//!
//! - the three files under `/static/` are ikigai-ui's bytes, by their own names;
//! - every page links them in the order D14 needs, with htmx pinned by SRI;
//! - every page shows the one theme chooser, and renders `theme=` for a page with no script;
//! - every page has one announcing region (D13);
//! - `web/gonk.css` writes no color, and only a control wears `--line` (D12);
//! - the palettes clear the floor for the pairs gonk draws, and keep gonk's colors.
//!
//! There is no JavaScript runtime in this suite, so `web/gonk.js`'s half is pinned as wiring.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_a11y::color::{ratio, Rgba};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;
use ikigai_ui::palette::{DARK, HIGH_CONTRAST, LIGHT, PALETTES};
use ikigai_ui::testkit::{contrast, lint};

const CSS: &str = include_str!("../web/gonk.css");
const JS: &str = include_str!("../web/gonk.js");
const XSL: &str = include_str!("../web/gonk.xsl");

/// Pages every caller can open. A refusal's page is held to the same files and the same chooser by
/// `tests/styled_refusal_2021.rs::is_gonk_shell`.
const PAGES: [(&str, u16); 5] = [
    ("/", 200),
    ("/l", 200),
    ("/l/default", 200),
    ("/sparql", 200),
    ("/queue", 200),
];

struct Server {
    addr: SocketAddr,
    _config: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel_serving(
            hub,
            web::space(face),
            Vec::new(),
            None,
        ));
        let door = doors::HttpDoor {
            anonymous: anonymous_http_grants(&["default".to_string()]).unwrap(),
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let edge = doors::edge_config(door.clone());
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door),
                listener,
                edge,
            ))
        });
        Server {
            addr,
            _config: config,
        }
    }

    /// A person opening `path` by typing its address: the status, the head and the body.
    fn open(&self, path: &str) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\
             Accept: text/html,application/xhtml+xml,*/*;q=0.8\r\n\
             Sec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nConnection: close\r\n\r\n",
            port = self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a head");
        let head = String::from_utf8_lossy(&response[..split]).to_string();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {head}"));
        (status, head, response[split + 4..].to_vec())
    }

    fn page(&self, path: &str, status: u16) -> String {
        let (got, head, body) = self.open(path);
        assert_eq!(got, status, "{path}\n{head}");
        String::from_utf8(body).expect("a page is text")
    }
}

/// How many times `needle` appears in `page`.
fn count(page: &str, needle: &str) -> usize {
    page.matches(needle).count()
}

#[test]
fn the_shared_files_are_ikigai_uis_bytes_by_their_own_names() {
    let gonk = Server::start();
    for (path, iri, media) in [
        ("/static/shell.js", "urn:iki:ui:shell.js", "text/javascript"),
        ("/static/htmx.min.js", "urn:iki:ui:htmx", "text/javascript"),
    ] {
        let (status, head, body) = gonk.open(path);
        assert_eq!(status, 200, "{path}\n{head}");
        assert!(head.contains(media), "{path}\n{head}");
        let artifact = ikigai_ui::artifact(iri).expect("an artifact of ikigai-ui's");
        assert_eq!(body, artifact.bytes, "{path} is {iri}, whole");
    }
    let (status, head, tokens) = gonk.open("/static/tokens.css");
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("text/css"), "{head}");
    let tokens = String::from_utf8(tokens).unwrap();
    for palette in PALETTES {
        assert!(
            tokens.contains(&palette.declarations()),
            "the token sheet holds the {} palette",
            palette.name
        );
    }
    let (status, _, _) = gonk.open("/static/gonk.css");
    assert_eq!(status, 200, "gonk's own layout is still gonk's");
    assert!(
        !std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/web/htmx.min.js")).exists(),
        "gonk vendors no htmx of its own"
    );
}

/// D14's order: the tokens before gonk's layout, so gonk wins a tie; the shell script with no
/// `defer`, so the theme is on `<html>` before the first paint; htmx deferred and pinned by SRI;
/// gonk's own script after it. xrust writes attributes in name order.
#[test]
fn every_page_links_the_shared_files_in_order_and_pins_htmx() {
    let gonk = Server::start();
    for (path, status) in PAGES {
        let page = gonk.page(path, status);
        let at = |needle: &str| {
            page.find(needle)
                .unwrap_or_else(|| panic!("{path}: no `{needle}` in {page}"))
        };
        assert!(at("href='/static/tokens.css'") < at("href='/static/gonk.css'"));
        let shell = at("<script src='/static/shell.js'></script>");
        let htmx = at(&format!(
            "<script defer='defer' integrity='{}' src='/static/htmx.min.js'></script>",
            ikigai_ui::HTMX_INTEGRITY
        ));
        let own = at("<script defer='defer' src='/static/gonk.js'></script>");
        assert!(shell < htmx && htmx < own, "{path}: {page}");
    }
}

#[test]
fn every_page_shows_the_one_chooser_and_renders_a_theme_with_no_script() {
    let gonk = Server::start();
    for (path, status) in PAGES {
        let page = gonk.page(path, status);
        assert_eq!(count(&page, "id=\"ui-theme\""), 1, "{path}: {page}");
        assert!(page.contains("data-theme='auto'"), "{path}: {page}");
        assert!(
            page.contains(&ikigai_ui::theme_chooser("auto").unwrap()),
            "{path}: ikigai-ui's markup, whole: {page}"
        );
        let header = &page[page.find("<header class='top'").expect("the header")
            ..page.find("</header>").expect("the header ends")];
        assert!(header.contains("data-ui-theme"), "{path}: {header}");
        assert!(
            !page.contains("<!--theme-chooser-->"),
            "{path}: the marker is replaced: {page}"
        );
    }
    // Apply with no script: a GET form, so the page itself renders what it was asked.
    for path in ["/", "/l", "/l/default", "/sparql", "/queue"] {
        for theme in ikigai_ui::THEMES {
            let page = gonk.page(&format!("{path}?theme={theme}"), 200);
            assert!(
                page.contains(&format!("data-theme='{theme}'")),
                "{path} in {theme}: {page}"
            );
            assert!(
                page.contains(&ikigai_ui::theme_chooser(theme).unwrap()),
                "{path}: {theme} is selected: {page}"
            );
        }
        // A value that is no theme is a readable page, not a stylesheet's selector.
        let page = gonk.page(&format!("{path}?theme=sepia"), 200);
        assert!(page.contains("data-theme='auto'"), "{path}: {page}");
        assert!(!page.contains("sepia"), "{path}: {page}");
    }
}

/// D13: `#status` is the page's only live region, and the ids the shell script reads are there.
#[test]
fn a_page_has_one_announcing_region() {
    let gonk = Server::start();
    for (path, status) in PAGES {
        let page = gonk.page(path, status);
        assert_eq!(count(&page, "aria-live="), 1, "{path}: {page}");
        assert_eq!(count(&page, "role='status'"), 1, "{path}: {page}");
        assert!(
            page.contains("<div aria-live='polite' class='flash-area' id='status' role='status'>"),
            "{path}: {page}"
        );
        assert!(page.contains("<main id='main'>"), "{path}: {page}");
        assert!(!page.contains("id='flash'"), "{path}: {page}");
    }
    assert!(!XSL.contains("id=\"flash\""), "every swap writes #status");
    assert!(
        !XSL.contains("aria-live=\"polite\"></p>") && !XSL.contains("role=\"status\"><"),
        "a note in a page is plain text; web/gonk.js says it in #status"
    );
}

/// Refusals, failures and the status region are the shell script's, so gonk.js holds no copy of
/// them, and what retries on its own is marked in the markup the way the shell script reads it.
#[test]
fn the_refusal_code_is_the_shell_scripts_and_a_poll_is_quiet() {
    for gone in ["denyAt", "isOwnRetry", "isBadge", "affordance-error"] {
        assert!(!JS.contains(gone), "web/gonk.js still has `{gone}`");
        assert!(!CSS.contains(gone), "web/gonk.css still has `{gone}`");
    }
    assert!(
        !JS.contains("htmx:beforeRequest"),
        "the shell clears #status"
    );
    let shell = std::str::from_utf8(
        ikigai_ui::artifact("urn:iki:ui:shell.js")
            .expect("the shell script")
            .bytes,
    )
    .unwrap();
    for hook in [
        "[data-ui-quiet]",
        "htmx:responseError",
        "htmx:sendError",
        "htmx:beforeRequest",
        "aria-describedby",
        "getElementById(\"status\")",
        "getElementById(\"main\")",
    ] {
        assert!(shell.contains(hook), "the shell script reads `{hook}`");
    }
    for quiet in [
        "<span class=\"queue-badge\" data-ui-quiet=\"\"",
        "<span class=\"layers-badge\" data-ui-quiet=\"\"",
        "<div class=\"layers-watch\" data-ui-quiet=\"\"",
        "<p id=\"github\" class=\"github\" data-ui-quiet=\"\"",
    ] {
        assert!(XSL.contains(quiet), "{quiet}");
    }
    // A re-armed watcher is drawn by the script, and is as quiet as the one it replaces.
    assert!(JS.contains("next.setAttribute(\"data-ui-quiet\", \"\")"));
    // The Queue swaps into itself: a note in its place would take the findings away.
    assert!(XSL.contains("<xsl:attribute name=\"data-ui-quiet\"></xsl:attribute>"));
    // A 403's control is marked by the shell script, and is dimmed by this attribute.
    assert!(shell.contains("aria-disabled"));
    assert!(CSS.contains("[aria-disabled=\"true\"]"));
}

/// `css` less its comments, where `ledger #1604` would read as a color.
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

#[test]
fn gonk_css_writes_no_color_and_uses_only_defined_tokens() {
    let css = without_comments(CSS);
    assert!(
        lint::hex_colors(&css).is_empty(),
        "{:?}",
        lint::hex_colors(&css)
    );
    let undefined = lint::undefined_vars(&[CSS]);
    assert!(undefined.is_empty(), "{undefined:?}");
}

/// D12: `--line` is the border that identifies a control, and every other line is `--rule`.
#[test]
fn only_a_control_wears_line() {
    let mut wearers = Vec::new();
    for rule in without_comments(CSS).split('}') {
        let Some((selectors, body)) = rule.rsplit_once('{') else {
            continue;
        };
        if body.contains("var(--line)") {
            wearers.push(selectors.trim().to_string());
        }
    }
    assert_eq!(
        wearers,
        ["input[type=\"text\"], input[type=\"search\"], select, textarea"]
    );
}

/// The tokens' `nav a` padding is for the shared shell, which gonk's pages take in phase 2.
#[test]
fn gonks_navigation_keeps_its_look() {
    assert!(CSS.contains("nav a { display: inline; padding: 0; }"));
}

#[test]
fn the_palettes_clear_the_floor_for_the_pairs_gonk_draws() {
    let failing = contrast::failures(&PALETTES);
    assert!(failing.is_empty(), "{failing:?}");
    // ikigai-ui's list has no `--ok` as text; gonk's open badge and praise are that.
    for palette in PALETTES {
        for ground in ["bg", "surface"] {
            let color = |token: &str| Rgba::parse(palette.get(token).unwrap()).unwrap();
            let got = ratio(color("ok"), color(ground));
            let floor = palette.floor(ikigai_ui::palette::Kind::Text);
            assert!(
                got >= floor,
                "{}: --ok on --{ground} is {got:.2}:1, under {floor}:1",
                palette.name
            );
        }
    }
}

/// Keeping the look rests on this: light and dark are gonk's own colors, and what was `--line` is
/// `--rule` now. A palette that moved would be a change to every page, found only by eye.
#[test]
fn light_and_dark_are_the_colors_gonk_had() {
    for (palette, had) in [
        (
            LIGHT,
            [
                ("bg", "#ffffff"),
                ("surface", "#f4f6f8"),
                ("fg", "#1b1f24"),
                ("muted", "#545d68"),
                ("accent", "#0b5cad"),
                ("accent-fg", "#ffffff"),
                ("danger", "#a8200d"),
                ("danger-fg", "#ffffff"),
                ("ok", "#19703a"),
                ("rule", "#c9d1d9"),
                ("focus", "#0b5cad"),
            ],
        ),
        (
            DARK,
            [
                ("bg", "#0d1117"),
                ("surface", "#161b22"),
                ("fg", "#e6edf3"),
                ("muted", "#9ea8b3"),
                ("accent", "#6cb6ff"),
                ("accent-fg", "#0d1117"),
                ("danger", "#ff9b8f"),
                ("danger-fg", "#0d1117"),
                ("ok", "#5bd07a"),
                ("rule", "#30363d"),
                ("focus", "#6cb6ff"),
            ],
        ),
    ] {
        for (token, value) in had {
            assert_eq!(palette.get(token), Some(value), "{}: {token}", palette.name);
        }
    }
    assert!(HIGH_CONTRAST.enhanced);
}
