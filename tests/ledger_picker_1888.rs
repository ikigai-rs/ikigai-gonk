//! The header's ledger picker and the `/l` index (ledger
//! [#1888](http://localhost:1062/l/default/item/1888)), driven over real HTTP the way a browser
//! drives them.
//!
//! The header used to draw one link per readable ledger, which wrapped on a phone at seven. It
//! is a menu of links now: a `<details>` that works with no script, which `web/gonk.js` turns
//! into a disclosure button, with a filter past `web::MENU_FILTER_PAST` ledgers and an "All
//! ledgers" link to `/l`, which carries the counts (the menu does not: measured, they cost every
//! page a read per ledger).
//!
//! ⚠ What a test here cannot do is run the script: there is no DOM in `cargo test`. The
//! script's half (Escape returns focus, a click outside closes, the filter narrows and
//! announces) is pinned as WIRING below, and was run in a real browser before merging; the PR
//! says where and how. The ledger names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Server {
    /// A server configured with `configured` ledgers whose anonymous loopback caller reads
    /// `readable` (its grant's names are listed too, as `web::readable_ledgers` does).
    fn start(configured: &[&str], readable: &[&str]) -> Server {
        let anonymous = grants_for_all(
            &readable.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            Authority::Read,
        )
        .unwrap();
        Self::with_grants(configured, anonymous)
    }

    fn with_grants(configured: &[&str], anonymous: Vec<String>) -> Server {
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
            ledgers: configured.iter().map(|s| s.to_string()).collect(),
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server {
            addr,
            hub,
            _config: config,
        }
    }

    /// A browser navigation: `(status, body)`.
    fn page(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: {CHROME_ACCEPT}\r\n\
             Sec-Fetch-Site: none\r\nConnection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        // The stylesheet's HTML writer single-quotes attributes and sorts them by name; the
        // assertions here are written with double quotes.
        (status, body.replace('\'', "\""))
    }

    /// A page that must answer 200.
    fn ok(&self, path: &str) -> String {
        let (status, body) = self.page(path);
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    /// What no door may write: items filed and closed with root authority, straight on the hub.
    fn issue(&self, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
        let request = args.iter().fold(
            Request::new(verb, Iri::parse(iri).unwrap()),
            |request, (name, value)| {
                request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()))
            },
        );
        let answer =
            futures::executor::block_on(Kernel::issue(&self.hub, request, &Capability::root()))
                .unwrap_or_else(|e| panic!("{iri}: {e}"));
        String::from_utf8_lossy(&answer.bytes).into_owned()
    }

    /// File `open` items in `ledger`, then `closed` more that are closed at once.
    fn seed(&self, ledger: &str, open: usize, closed: usize) {
        let prefix = Ledger::parse(ledger).unwrap().prefix();
        for n in 0..open + closed {
            self.issue(
                Verb::Sink,
                &format!("{prefix}append"),
                &[("content", &format!("sample item {n}"))],
            );
        }
        for n in open..open + closed {
            self.issue(
                Verb::Sink,
                &format!("{prefix}close"),
                &[("item", &(n + 1).to_string())],
            );
        }
    }
}

/// The header's `<nav class="ledgers">`, or `None` when it has none.
fn header_nav(page: &str) -> Option<&str> {
    let header = &page[page.find("<header")?..page.find("</header>")?];
    let start = header[..header.find("class=\"ledgers\"")?].rfind("<nav")?;
    let end = header[start..].find("</nav>")? + start;
    Some(&header[start..end])
}

/// Every `data-ledger-name` in `html`, in order.
fn menu_names(html: &str) -> Vec<String> {
    html.split("data-ledger-name=\"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].to_string())
        .collect()
}

/// The `<a …>…</a>` holding `href="{href}"` in `html`.
fn link<'a>(html: &'a str, href: &str) -> &'a str {
    let at = html
        .find(&format!("href=\"{href}\""))
        .unwrap_or_else(|| panic!("no link to {href} in {html}"));
    let start = html[..at].rfind("<a").unwrap();
    let end = html[at..].find("</a>").unwrap() + at + 4;
    &html[start..end]
}

/// The `/l` index's rows, as `(name, open, total)` cell texts.
fn index_rows(page: &str) -> Vec<(String, String, String)> {
    let body = &page[page.find("<tbody>").expect("a table body")..page.find("</tbody>").unwrap()];
    body.split("<tr>")
        .skip(1)
        .map(|row| {
            let text = |cell: &str| -> String {
                let mut out = String::new();
                let mut in_tag = false;
                for c in cell.chars() {
                    match c {
                        '<' => in_tag = true,
                        '>' => in_tag = false,
                        c if !in_tag => out.push(c),
                        _ => {}
                    }
                }
                out.trim().to_string()
            };
            let cells: Vec<&str> = row.split("<td").collect();
            (
                text(&format!("<{}", cells[0])),
                text(&format!("<td{}", cells[1])),
                text(&format!("<td{}", cells[2])),
            )
        })
        .collect()
}

/// ★ The menu lists exactly what the caller may read — the configured ledgers it holds a grant
/// for, then any its grant names — `default` first and then by name, and the current ledger
/// carries `aria-current`. No counts: those are `/l`'s. A configured ledger it may not read is not
/// offered. The SPARQL page's `<select>` (a form value, not navigation) is untouched.
#[test]
fn the_menu_lists_exactly_the_readable_ledgers_with_the_current_one_marked() {
    let server = Server::start(
        &["default", "zeta", "alpha", "hidden"],
        &["zeta", "default", "alpha", "beta"],
    );
    server.seed("alpha", 2, 1);
    server.seed("default", 1, 0);

    let page = server.ok("/l/alpha");
    let nav = header_nav(&page).expect("a ledger nav in the header");
    assert_eq!(
        menu_names(nav),
        ["default", "alpha", "beta", "zeta"],
        "{nav}"
    );
    assert!(
        !nav.contains("hidden"),
        "a ledger the grant does not read: {nav}"
    );

    let current = link(nav, "/l/alpha");
    assert!(current.contains("aria-current=\"page\""), "{current}");
    assert!(current.contains(">alpha</a>"), "{current}");
    for other in ["/l/default", "/l/beta", "/l/zeta"] {
        assert!(!link(nav, other).contains("aria-current"), "{other}: {nav}");
    }
    assert!(
        !nav.contains(" open<"),
        "the counts are /l's, not every page's: {nav}"
    );
    assert_eq!(nav.matches("aria-current").count(), 1, "{nav}");
    assert!(
        nav.contains("Ledger: alpha"),
        "the button names the current ledger: {nav}"
    );

    let all = link(nav, "/l");
    assert!(all.contains("All ledgers"), "{all}");
    assert!(
        nav.rfind("All ledgers") > nav.rfind("data-ledger-name"),
        "All ledgers is the last link: {nav}"
    );

    // The SPARQL form's select is still a select of the same set, and the header has none.
    let sparql = server.ok("/sparql");
    let nav = header_nav(&sparql).expect("a ledger nav on the SPARQL page");
    assert!(
        sparql.contains("<select id=\"q-ledger\" name=\"ledger\">"),
        "{sparql}"
    );
    assert!(sparql.contains("<option value=\"beta\">"), "{sparql}");
    assert!(!nav.contains("<select"), "{nav}");
}

/// ★ With no script it is a `<details>`: the summary opens it, every link is inside it and
/// none is hidden, so a browser with scripting off follows them like any other link. The
/// script's own pieces (the filter) are the only thing hidden, until the script shows them.
#[test]
fn the_menu_works_without_a_script() {
    let server = Server::start(&["default", "alpha", "beta"], &["default", "alpha", "beta"]);
    let page = server.ok("/l/beta");
    let nav = header_nav(&page).expect("a ledger nav");
    let details = &nav[nav.find("<details").expect("a details element")..];
    assert!(details.contains("data-ledger-menu=\"true\""), "{details}");
    assert!(
        details.contains("<summary class=\"ledger-summary\">Ledger: beta</summary>"),
        "{details}"
    );
    let panel = &details[details.find("id=\"ledger-panel\"").expect("the panel")..];
    for href in ["/l/default", "/l/alpha", "/l/beta", "/l"] {
        link(panel, href);
    }
    assert!(
        !panel.contains("hidden"),
        "nothing in the panel is hidden without a script: {panel}"
    );
    assert!(nav.find("</details>").is_some(), "{nav}");

    // The script that enhances it ships, and enhances exactly this element.
    let js = include_str!("../web/gonk.js");
    assert!(
        js.contains("details[data-ledger-menu]"),
        "gonk.js wires the menu"
    );
    assert!(
        js.contains("$(\"ledger-panel\")"),
        "gonk.js finds its panel by id"
    );
}

/// ★ The filter is drawn only PAST eight readable ledgers, hidden until the script that runs
/// it is there, with a label and a polite live region for the count. Eight has none.
#[test]
fn the_filter_appears_only_past_eight_and_announces_politely() {
    let eight = [
        "default", "alpha", "beta", "gamma", "delta", "epsilon", "eta", "theta",
    ];
    let server = Server::start(&eight, &eight);
    let page = server.ok("/l/default");
    let nav = header_nav(&page).unwrap();
    assert_eq!(menu_names(nav).len(), 8, "{nav}");
    assert!(!nav.contains("ledger-filter"), "no filter at eight: {nav}");

    let nine = [
        "default", "alpha", "beta", "gamma", "delta", "epsilon", "eta", "theta", "iota",
    ];
    let server = Server::start(&nine, &nine);
    let page = server.ok("/l/default");
    let nav = header_nav(&page).unwrap();
    assert_eq!(menu_names(nav).len(), 9, "{nav}");
    let filter = &nav[nav
        .find("class=\"ledger-filter\"")
        .expect("a filter past eight")..];
    assert!(
        filter.contains("hidden"),
        "hidden until the script runs it: {filter}"
    );
    assert!(
        filter.contains("<label for=\"ledger-filter\">Filter ledgers</label>"),
        "{filter}"
    );
    assert!(
        filter.contains("id=\"ledger-filter\" spellcheck=\"false\" type=\"search\""),
        "{filter}"
    );
    assert!(
        filter.contains(
            "aria-live=\"polite\" class=\"note\" id=\"ledger-filter-status\" role=\"status\""
        ),
        "{filter}"
    );

    // The script narrows by the names the server drew and says the count in that region.
    let js = include_str!("../web/gonk.js");
    let wiring = &js[js
        .find("function wireLedgerFilter")
        .expect("the filter's wiring")..];
    let wiring = &wiring[..wiring.find("async function init").unwrap()];
    for needle in [
        "box.hidden = false",
        "li[data-ledger-name]",
        "item.hidden = !match",
        "status.textContent = sentence",
        " ledgers match.",
    ] {
        assert!(wiring.contains(needle), "the filter wiring lost `{needle}`");
    }
}

/// ★ Escape closes the menu and puts focus back on the button; a click outside closes it. The
/// button says its state and what it opens. Wiring only: see the module docs.
#[test]
fn escape_closes_the_menu_and_returns_focus_to_its_button() {
    let js = include_str!("../web/gonk.js");
    let wiring = &js[js
        .find("function wireLedgerMenu")
        .expect("the menu's wiring")..];
    let wiring = &wiring[..wiring.find("function wireLedgerFilter").unwrap()];
    for needle in [
        "button.type = \"button\"",
        "setAttribute(\"aria-expanded\", \"false\")",
        "setAttribute(\"aria-controls\", panel.id)",
        "e.key !== \"Escape\"",
        "button.focus()",
        "!menu.contains(e.target)",
    ] {
        assert!(wiring.contains(needle), "the menu wiring lost `{needle}`");
    }
    let escape = &wiring[wiring.find("\"Escape\"").unwrap()..];
    assert!(
        escape.find("setOpen(false)") < escape.find("button.focus()"),
        "Escape closes, then focuses the button"
    );
}

/// ★ `/l` lists the same set as the menu, in the same order, with each ledger's open and total
/// counts, and the menu's "All ledgers" link is current there.
#[test]
fn the_index_lists_the_same_set_as_the_menu_with_its_counts() {
    let server = Server::start(
        &["default", "zeta", "alpha", "hidden"],
        &["zeta", "default", "alpha", "beta"],
    );
    server.seed("alpha", 2, 1);
    server.seed("zeta", 0, 2);

    let page = server.ok("/l");
    let nav = header_nav(&page).expect("the header menu on the index");
    let rows = index_rows(&page);
    let names: Vec<&str> = rows.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names, menu_names(nav), "{page}");
    assert_eq!(
        rows,
        [
            ("default".to_string(), "0".to_string(), "0".to_string()),
            ("alpha".to_string(), "2".to_string(), "3".to_string()),
            ("beta".to_string(), "0".to_string(), "0".to_string()),
            ("zeta".to_string(), "0".to_string(), "2".to_string()),
        ]
    );
    assert!(page.contains("<title>Ledgers · gonk</title>"), "{page}");
    assert!(page.contains("4 ledgers you may read"), "{page}");
    assert!(link(&page, "/l/alpha").contains(">alpha<"), "{page}");
    assert!(!page.contains("hidden</a>"), "{page}");
    // Off a ledger's page the button says "Ledgers", and the current page is "All ledgers".
    assert!(nav.contains(">Ledgers</summary>"), "{nav}");
    assert!(link(nav, "/l").contains("aria-current=\"page\""), "{nav}");
    assert_eq!(nav.matches("aria-current").count(), 1, "{nav}");
}

/// ★ A caller who may read exactly one ledger sees its name, and no menu.
#[test]
fn one_readable_ledger_is_its_name_and_no_menu() {
    let server = Server::start(&["default", "hidden"], &["default"]);
    let page = server.ok("/l/default");
    let nav = header_nav(&page).expect("a ledger nav");
    assert!(!nav.contains("<details"), "{nav}");
    assert!(!nav.contains("ledger-panel"), "{nav}");
    assert!(
        nav.contains("<span class=\"ledger-only\">Ledger: <a"),
        "{nav}"
    );
    let only = link(nav, "/l/default");
    assert!(only.contains("aria-current=\"page\""), "{only}");
    assert!(only.contains(">default</a>"), "{only}");

    // And the index still answers, with its one row.
    let index = server.ok("/l");
    assert_eq!(index_rows(&index).len(), 1, "{index}");
    assert!(index.contains("1 ledger you may read"), "{index}");
}

/// A caller who may read no ledger gets no ledger nav at all, and `/l` tells it how to get one.
#[test]
fn no_readable_ledger_draws_no_menu_and_the_index_says_how_to_get_one() {
    let server = Server::start(&["default"], &[]);
    let (status, page) = server.page("/l");
    assert_eq!(status, 200, "{page}");
    assert!(header_nav(&page).is_none(), "{page}");
    assert!(page.contains("Sign in to read the ledgers."), "{page}");
}

/// A link is offered only when both the ledger and its graph can be read (#1892).
#[test]
fn partial_read_grants_never_offer_a_refused_ledger_link() {
    let complete = Ledger::parse("complete").unwrap();
    let ledger_only = Ledger::parse("ledger-only").unwrap();
    let graph_only = Ledger::parse("graph-only").unwrap();
    let server = Server::with_grants(
        &["complete", "ledger-only", "graph-only"],
        vec![
            complete.cap_read(),
            ikigai_store::cap_read_graph(&complete.graph()),
            ledger_only.cap_read(),
            ikigai_store::cap_read_graph(&graph_only.graph()),
        ],
    );

    assert_eq!(server.page("/l/ledger-only").0, 403);
    assert_eq!(server.page("/l/graph-only").0, 403);
    let page = server.ok("/l/complete");
    let nav = header_nav(&page).expect("the readable ledger's link");
    link(nav, "/l/complete");
    assert!(!nav.contains("href=\"/l/ledger-only\""), "{nav}");
    assert!(!nav.contains("href=\"/l/graph-only\""), "{nav}");

    let index = server.ok("/l");
    assert_eq!(
        index_rows(&index),
        [("complete".into(), "0".into(), "0".into())]
    );
}
