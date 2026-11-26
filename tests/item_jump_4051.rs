//! Going to a ledger item by its number from the ledger page's search (ledger
//! [#4051](http://localhost:1062/l/default/item/4051)), driven over real HTTP the way a browser
//! drives it.
//!
//! A search that is exactly `N` or `#N` (whitespace trimmed) answers with a page that refreshes
//! at once to `/l/{ledger}/item/N` and links there (WCAG H76: a read cannot redirect in
//! ikigai-web), whatever the item's status. A number the ledger does not hold is said in the
//! status region, with the number, over the unfiltered list. Text that merely contains digits
//! is still a title search.
//!
//! ⚠ What a test here cannot do is run the script: there is no DOM in `cargo test`. The
//! script's half (a number search leaves htmx and navigates) is pinned as WIRING below.

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
            holder_aliases: Default::default(),
            display: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: Vec::new(),
            inquiry: None,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
            public_origin: None,
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

/// The `<meta http-equiv="refresh">` target of `page`, if it has one.
fn refresh(page: &str) -> Option<&str> {
    let at = page.find("http-equiv=\"refresh\"")?;
    let start = page[..at].rfind("<meta")?;
    let end = page[start..].find('>')? + start;
    let meta = &page[start..end];
    let url = meta.find("url=")? + 4;
    Some(&meta[url..meta[url..].find('"')? + url])
}

/// The `#status` region's text.
fn status_region(page: &str) -> &str {
    let at = page.find("id=\"status\"").expect("a status region");
    let start = page[..at].rfind("<div").unwrap();
    let end = page[start..].find("</div>").unwrap() + start;
    &page[start..end]
}

#[test]
fn an_open_item_number_goes_to_its_page() {
    let server = Server::start(&["default"], &["default"]);
    server.seed("default", 3, 0);
    for query in ["2", "%232", "+2+", "%20%232%20"] {
        let page = server.ok(&format!("/l/default?status=open&text={query}"));
        assert_eq!(
            refresh(&page),
            Some("/l/default/item/2"),
            "text={query}: {page}"
        );
        assert!(
            page.contains("href=\"/l/default/item/2\""),
            "a visible link for a browser that does not refresh: {page}"
        );
        assert!(page.contains("sample item 1"), "the title is named: {page}");
    }
}

#[test]
fn a_closed_item_number_goes_to_its_page_from_the_open_list() {
    let server = Server::start(&["default"], &["default"]);
    server.seed("default", 1, 2);
    for status in ["open", "closed", "all"] {
        let page = server.ok(&format!("/l/default?status={status}&text=%233"));
        assert_eq!(
            refresh(&page),
            Some("/l/default/item/3"),
            "status={status}: {page}"
        );
    }
}

#[test]
fn a_number_the_ledger_does_not_hold_is_said_in_the_status_region() {
    let server = Server::start(&["default"], &["default"]);
    server.seed("default", 2, 0);
    for query in ["3980", "%233980"] {
        let page = server.ok(&format!("/l/default?text={query}"));
        assert_eq!(refresh(&page), None, "{page}");
        let status = status_region(&page);
        assert!(
            status.contains("No item #3980 in ledger default."),
            "the number, in the status region: {status}"
        );
        assert!(
            status.contains("data-announce"),
            "said once by web/gonk.js on load: {status}"
        );
        assert!(
            !page.contains("No items match."),
            "the list is not an empty one: {page}"
        );
        assert!(page.contains("sample item 0"), "{page}");
    }
}

#[test]
fn a_named_ledger_jumps_within_itself() {
    let server = Server::start(&["default", "crew"], &["default", "crew"]);
    server.seed("crew", 2, 1);
    let page = server.ok("/l/crew?text=3");
    assert_eq!(refresh(&page), Some("/l/crew/item/3"), "{page}");
    let page = server.ok("/l/crew?text=%232");
    assert_eq!(refresh(&page), Some("/l/crew/item/2"), "{page}");
    // `default` holds no item 3: the number is this ledger's, never another's.
    let page = server.ok("/l/default?text=3");
    assert_eq!(refresh(&page), None, "{page}");
    assert!(
        status_region(&page).contains("No item #3 in ledger default."),
        "{page}"
    );
}

#[test]
fn text_that_merely_contains_digits_is_a_title_search() {
    let server = Server::start(&["default"], &["default"]);
    server.seed("default", 3, 0);
    for query in ["item+2", "2a", "%23%232", "-2", "2+2"] {
        let page = server.ok(&format!("/l/default?text={query}"));
        assert_eq!(refresh(&page), None, "text={query}: {page}");
        assert!(
            !status_region(&page).contains("No item"),
            "text={query}: {page}"
        );
    }
    let page = server.ok("/l/default?text=item+2");
    assert!(page.contains("sample item 2"), "{page}");
    assert!(!page.contains("sample item 1"), "{page}");
}

#[test]
fn the_search_field_says_it_takes_a_number_and_a_number_leaves_htmx() {
    let server = Server::start(&["default"], &["default"]);
    let page = server.ok("/l/default");
    assert!(
        page.contains("aria-describedby=\"search-text-help\""),
        "{page}"
    );
    assert!(
        page.contains("id=\"search-text-help\""),
        "the field's help names the number: {page}"
    );
    assert!(page.contains("an item number"), "{page}");
    let script = server.ok("/static/gonk.js");
    assert!(
        script.contains("search-text") && script.contains("form.submit()"),
        "a number search is a normal navigation, not an htmx swap"
    );
}
