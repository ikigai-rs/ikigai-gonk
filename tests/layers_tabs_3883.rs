//! Ledger [#3883](http://localhost:1062/l/default/item/3883): the Layers page as tabs by layer
//! kind, a "new" mark per tab, and the header's number in the nav link's own color.
//!
//! The layer host is the published `ikigai-layers` in process, its layers declaring kinds as a
//! real host's do (`tests/kinds_host`); gonk's HTTP door is a scratch one on `127.0.0.1:0`.
//!
//! - [`tabs_are_grouped_by_kind_in_the_vocabularys_order`]
//! - [`a_kind_the_viewer_cannot_see_has_no_tab`]
//! - [`a_layer_without_a_kind_is_under_other_last`]
//! - [`the_new_mark_appears_and_clears_with_the_unread_state`]
//! - [`the_overview_lands_on_the_first_tab_with_unread_entries`]
//! - [`the_header_number_is_the_nav_links_own_color`]
//!
//! axe over a tabbed page in every theme is CI's, over `examples/axe_site.rs`'s
//! `layers-tabs.html` and `layers-tab-other.html`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod kinds_host;

use ikigai_a11y::color::{ratio, Rgba};
use ikigai_gonk::layers_view::{self, LayersView, Waits, CAP_READ, CAP_SAY};
use ikigai_gonk::{compose, doors, identity::Passkeys, quic, web};
use ikigai_store::DurableStore;
use ikigai_ui::palette::{Kind, PALETTES};
use kinds_host::Host;

const CSS: &str = include_str!("../web/gonk.css");

const CHROME_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// A scratch gonk whose ANONYMOUS caller holds the Layers page's tokens, as the tests of
/// `tests/layers_page_1675.rs` do: the viewer is `anonymous`.
struct Gonk {
    addr: SocketAddr,
    _config: tempfile::TempDir,
}

impl Gonk {
    fn start(view: Arc<LayersView>) -> Gonk {
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
            holder_aliases: Default::default(),
            display: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: Vec::new(),
            inquiry: None,
        });
        let pages = layers_view::bind(web::space(Arc::clone(&face)), &face, Some(view));
        let http = Arc::new(doors::http_kernel_with(Arc::clone(&hub), pages, None));
        let door = doors::HttpDoor {
            anonymous: vec![CAP_READ.to_string(), CAP_SAY.to_string()],
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
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
        Gonk {
            addr,
            _config: config,
        }
    }

    fn get(&self, path: &str, htmx: bool) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let headers = if htmx {
            "Accept: */*\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nHX-Request: true\r\n"
                .to_string()
        } else {
            format!("Accept: {CHROME_ACCEPT}\r\nSec-Fetch-Site: none\r\n")
        };
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\n{headers}Connection: close\r\n\r\n",
            self.addr.port()
        )
        .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        (status, body.to_string())
    }

    fn page(&self, path: &str) -> String {
        let (status, body) = self.get(path, false);
        assert_eq!(status, 200, "{path}: {body}");
        body
    }
}

/// Wait (up to ten seconds) for the follower, which the first page started, to bring the
/// viewer's unread total in `kind`'s tab to `n`.
fn settle(view: &LayersView, kind: &str, n: u64) {
    let end = Instant::now() + Duration::from_secs(10);
    let unread = || {
        view.tabs("anonymous")
            .unwrap_or_default()
            .into_iter()
            .find(|t| t.key == kind)
            .map(|t| t.unread)
    };
    while Instant::now() < end && unread() != Some(n) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(unread(), Some(n), "the follower brings {kind}'s entries");
}

fn view_as(host: &Host, slug: &str) -> Arc<LayersView> {
    let view = LayersView::new(
        host.door(slug),
        "scratch-door".to_string(),
        None,
        Waits { feed_secs: 1 },
    );
    view.refresh().expect("the scratch door answers");
    view
}

/// The tab nav's HTML.
fn tabs(page: &str) -> &str {
    let start = page
        .find("<nav aria-label='Layer kinds' class='layer-kinds'>")
        .unwrap_or_else(|| panic!("no tab nav: {page}"));
    let end = page[start..].find("</nav>").unwrap() + start;
    &page[start..end]
}

/// The tabs' names in order, each with its link and whether it is the one shown.
fn tab_links(page: &str) -> Vec<(String, String, bool)> {
    tabs(page)
        .split("<a ")
        .skip(1)
        .map(|a| {
            let href = a
                .split("href='")
                .nth(1)
                .and_then(|r| r.split('\'').next())
                .unwrap()
                .to_string();
            let label = a.split('>').nth(1).unwrap().split('<').next().unwrap();
            (label.to_string(), href, a.contains("aria-current='page'"))
        })
        .collect()
}

/// The layer rows' links on the page.
fn rows(page: &str) -> Vec<String> {
    page.split("<h2 class='layer-name'><a href='")
        .skip(1)
        .map(|r| r.split('\'').next().unwrap().to_string())
        .collect()
}

#[test]
fn tabs_are_grouped_by_kind_in_the_vocabularys_order() {
    let host = Host::open();
    let gonk = Gonk::start(view_as(&host, "brian"));
    let page = gonk.page("/layers/kind/talk");
    assert_eq!(
        tab_links(&page),
        [
            ("Talk".into(), "/layers/kind/talk".into(), true),
            ("Tasks".into(), "/layers/kind/task".into(), false),
            ("Other".into(), "/layers/kind/other".into(), false),
        ],
        "the vocabulary's labels and order, Other last: {page}"
    );
    assert_eq!(rows(&page), ["/layers/notes", "/layers/talk"], "{page}");
    assert!(
        page.contains("<title>Talk · Layers · gonk</title>"),
        "{page}"
    );
    assert!(page.contains("<h1 id='layers-title'>Layers</h1>"), "{page}");
    assert!(!page.contains("role='tablist'"), "links, never a tablist");

    let tasks = gonk.page("/layers/kind/task");
    assert_eq!(rows(&tasks), ["/layers/tasks"], "{tasks}");
    let shown: Vec<bool> = tab_links(&tasks).iter().map(|t| t.2).collect();
    assert_eq!(shown, [false, true, false]);
}

#[test]
fn a_kind_the_viewer_cannot_see_has_no_tab() {
    let host = Host::open();
    let brian = Gonk::start(view_as(&host, "brian"));
    let page = brian.page("/layers");
    assert!(
        !tab_links(&page).iter().any(|t| t.0 == "Join"),
        "Brian reads no join layer: {page}"
    );
    let (status, _) = brian.get("/layers/kind/join", false);
    assert_eq!(status, 404, "no tab, no page");
    let (status, _) = brian.get("/layers/kind/nonsense", false);
    assert_eq!(status, 404);

    // Bobby reads the join layer: his page has the tab, before Other.
    let bobby = Gonk::start(view_as(&host, "bobby"));
    let page = bobby.page("/layers/kind/join");
    let labels: Vec<String> = tab_links(&page).into_iter().map(|t| t.0).collect();
    assert_eq!(labels, ["Talk", "Tasks", "Join", "Other"], "{page}");
    assert_eq!(rows(&page), ["/layers/joins"]);
}

#[test]
fn a_layer_without_a_kind_is_under_other_last() {
    let host = Host::open();
    let gonk = Gonk::start(view_as(&host, "brian"));
    let page = gonk.page("/layers/kind/other");
    assert_eq!(rows(&page), ["/layers/misc"], "{page}");
    assert_eq!(tab_links(&page).last().unwrap().0, "Other");
    for kind in ["talk", "task"] {
        let page = gonk.page(&format!("/layers/kind/{kind}"));
        assert!(
            !rows(&page).contains(&"/layers/misc".to_string()),
            "{kind}: {page}"
        );
    }
}

#[test]
fn the_new_mark_appears_and_clears_with_the_unread_state() {
    let host = Host::open();
    let view = view_as(&host, "brian");
    let gonk = Gonk::start(Arc::clone(&view));
    let quiet = gonk.page("/layers/kind/talk");
    assert!(!quiet.contains("new messages"), "nothing unread: {quiet}");
    assert!(
        quiet.contains("hx-get='/layers/~/count?quiet=1&amp;kinds=1'"),
        "the tabs' marks ride on the header's one count poll: {quiet}"
    );
    assert_eq!(
        quiet.matches("hx-get=").count(),
        2,
        "two polls on the page whatever the number of tabs: the count and the page's own: {quiet}"
    );

    host.offer("bobby", "tasks", 1, "Wire the tabs");
    settle(&view, "task", 1);
    let page = gonk.page("/layers/kind/talk");
    let nav = tabs(&page);
    assert!(
        nav.contains(
            "Tasks<span id='layers-kind-new-task'><span class='sr-only'>, new messages</span>\
             <span aria-hidden='true' class='kind-new'></span></span>"
        ),
        "the tab reads \"Tasks, new messages\", the dot aria-hidden: {nav}"
    );
    assert!(
        nav.contains("Talk<span id='layers-kind-new-talk'></span>"),
        "{nav}"
    );

    // The count's answer on the Layers page carries every tab's mark, out of band.
    let (status, count) = gonk.get("/layers/~/count?quiet=1&kinds=1", true);
    assert_eq!(status, 200, "{count}");
    assert!(
        count.contains(
            "<span hx-swap-oob='true' id='layers-kind-new-task'><span class='sr-only'>, new messages</span>"
        ),
        "{count}"
    );
    assert!(
        count.contains("<span hx-swap-oob='true' id='layers-kind-new-talk'></span>"),
        "{count}"
    );
    assert!(
        count.contains("kinds=1"),
        "the next poll asks again: {count}"
    );
    // Elsewhere the count carries no marks.
    let (_, plain) = gonk.get("/layers/~/count", true);
    assert!(!plain.contains("layers-kind-new"), "{plain}");

    // Opening the layer is reading it: the mark clears.
    gonk.page("/layers/tasks");
    let (_, cleared) = gonk.get("/layers/~/count?quiet=1&kinds=1", true);
    assert!(
        cleared.contains("<span hx-swap-oob='true' id='layers-kind-new-task'></span>"),
        "{cleared}"
    );
    assert!(!cleared.contains("new messages"), "{cleared}");
    assert!(!gonk.page("/layers/kind/talk").contains("new messages"));
}

#[test]
fn the_overview_lands_on_the_first_tab_with_unread_entries() {
    let host = Host::open();
    let view = view_as(&host, "brian");
    let gonk = Gonk::start(Arc::clone(&view));
    let first = gonk.page("/layers");
    assert_eq!(
        tab_links(&first).iter().find(|t| t.2).unwrap().0,
        "Talk",
        "nothing unread: the first tab"
    );
    assert!(
        first.contains("&amp;kind=talk"),
        "the page's poll asks for its own tab's rows: {first}"
    );
    host.message("bobby", "misc", "hello");
    settle(&view, "other", 1);
    let page = gonk.page("/layers");
    assert_eq!(
        tab_links(&page).iter().find(|t| t.2).unwrap().0,
        "Other",
        "{page}"
    );
    assert_eq!(rows(&page), ["/layers/misc"]);
}

/// The `.layers-count` rule, without its comment.
fn rule(selector: &str) -> &'static str {
    let at = CSS
        .find(&format!("\n{selector} {{"))
        .unwrap_or_else(|| panic!("no `{selector}` rule"));
    let rest = &CSS[at + 1..];
    &rest[..rest.find('}').unwrap() + 1]
}

#[test]
fn the_header_number_is_the_nav_links_own_color() {
    let count = rule(".layers-count");
    assert!(
        count.contains("color: currentColor;") && count.contains("border: 1px solid currentColor;"),
        "the number and its ring are the link's own color: {count}"
    );
    assert!(
        !count.contains("var(--"),
        "no color token of its own, so no theme can part them: {count}"
    );
    assert!(
        count.contains("background: none;"),
        "no fill: the number stands on the header's ground, as the link's text does: {count}"
    );
    assert!(
        !CSS.contains(".layers-count {") || CSS.matches(".layers-count").count() == 1,
        "one rule, never one per theme"
    );
    // The link's text is `a { color: var(--accent) }` (ikigai-ui's tokens) on the header's
    // `--surface`, and so is the number now: that pair clears the text floor in every palette.
    for palette in PALETTES {
        let color = |token: &str| Rgba::parse(palette.get(token).unwrap()).unwrap();
        let got = ratio(color("accent"), color("surface"));
        let floor = palette.floor(Kind::Text).max(4.5);
        assert!(
            got >= floor,
            "{}: the number (--accent) on --surface is {got:.2}:1, under {floor}:1",
            palette.name
        );
    }
}
