//! Ledger [#2542](http://localhost:1062/l/default/item/2542): a ledger's open and closed counts as
//! a chart under its status line, and a Dashboard page with the chart of every ledger the viewer
//! may read.
//!
//! These read the pages over real HTTP. What a browser draws (the four themes, 320 CSS px, axe) is
//! in the pull request. The ledger names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::grants::{grants_for, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::budget::TimeBudget;
use ikigai_store::DurableStore;

const CSS: &str = include_str!("../web/gonk.css");

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Server {
    /// A server configured with `configured` ledgers, whose anonymous caller holds `anonymous`.
    fn start(configured: &[&str], anonymous: Vec<String>) -> Server {
        Self::over(DurableStore::in_memory().unwrap(), configured, anonymous)
    }

    /// [`Server::start`] over a store the test has set up.
    fn over(store: DurableStore, configured: &[&str], anonymous: Vec<String>) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(store));
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
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        };
        let serving = Arc::clone(&http);
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                serving,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server {
            addr,
            hub,
            http,
            _config: config,
        }
    }

    fn readers(configured: &[&str], readable: &[&str]) -> Server {
        let names: Vec<String> = readable.iter().map(|s| s.to_string()).collect();
        Self::start(configured, grants_for_all(&names, Authority::Read).unwrap())
    }

    /// A browser navigation: `(status, body)`, with the writer's quotes made double.
    fn page(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html,*/*;q=0.8\r\n\
             Sec-Fetch-Site: none\r\nConnection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.replace('\'', "\""))
    }

    fn ok(&self, path: &str) -> String {
        let (status, body) = self.page(path);
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    /// What no door may write, straight on the hub with root authority.
    fn issue(&self, verb: Verb, iri: &str, args: &[(&str, &str)]) {
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(verb, iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
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

    /// A raw update of `ledger`'s graph, for a history no action can write.
    fn update(&self, ledger: &str, body: &str) {
        let graph = Ledger::parse(ledger).unwrap().graph();
        let update = format!(
            "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
             PREFIX dcterms: <http://purl.org/dc/terms/>\n\
             PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n\
             WITH <{graph}> {body}"
        );
        self.issue(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("graph", graph.as_str()), ("content", &update)],
        );
    }

    /// Say every item in `ledger` was filed `days` days ago.
    fn file_back(&self, ledger: &str, days: u32) {
        self.update(
            ledger,
            &format!(
                "DELETE {{ ?item dcterms:created ?at }} \
                 INSERT {{ ?item dcterms:created ?then }} \
                 WHERE {{ ?item a ledger:Item ; dcterms:created ?at . \
                 BIND(NOW() - \"P{days}D\"^^xsd:dayTimeDuration AS ?then) }}"
            ),
        );
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// The chart's `<figure>`, whole, in `html`.
fn chart(html: &str) -> &str {
    let start = html.find("<figure").expect("a chart");
    &html[start..html[start..].find("</figure>").unwrap() + start]
}

/// The `class` of each line in `html`, in order.
fn lines(html: &str) -> Vec<&str> {
    html.split("<polyline")
        .skip(1)
        .map(|rest| {
            let at = rest.find("class=\"").unwrap() + 7;
            &rest[at..at + rest[at..].find('"').unwrap()]
        })
        .collect()
}

/// The ledger names the Dashboard titled charts with, in order.
fn charted(page: &str) -> Vec<String> {
    // The section the charts are in: the agent panels after it have headings of their own.
    let charts = page.split("id=\"dashboard-agents\"").next().unwrap();
    charts
        .split("<h3 ")
        .skip(1)
        .map(|h3| {
            let link = &h3[h3.find("<a").unwrap()..h3.find("</a>").unwrap()];
            link[link.find('>').unwrap() + 1..].to_string()
        })
        .collect()
}

#[test]
fn the_ledger_page_carries_the_chart_between_its_header_and_the_form() {
    let server = Server::start(
        &["default"],
        grants_for("default", Authority::Write).unwrap(),
    );
    server.seed("default", 2, 1);
    for path in ["/l/default", "/l/default/items"] {
        let page = server.ok(path);
        let head = page.find("ledger-head").expect("the ledger's header");
        let head_end = head + page[head..].find("</header>").unwrap();
        let figure = page.find("<figure").expect("a chart");
        let form = page.find("action=\"/act\"").expect("the file form");
        assert!(head_end < figure && figure < form, "{path}: {page}");
    }
}

#[test]
fn the_lines_follow_the_status_filter() {
    let server = Server::readers(&["default"], &["default"]);
    server.seed("default", 2, 1);
    let drawn = |query: &str| {
        let page = server.ok(&format!("/l/default{query}"));
        let chart = chart(&page).to_string();
        (lines(&chart).join(" | "), chart)
    };

    for (query, want, heading, numbers) in [
        ("", "chart-line chart-open", "Open items", ": 2 open."),
        (
            "?status=open",
            "chart-line chart-open",
            "Open items",
            ": 2 open.",
        ),
        (
            "?status=closed",
            "chart-line chart-closed",
            "Closed items",
            ": 1 closed.",
        ),
        (
            "?status=all",
            "chart-line chart-open | chart-line chart-closed",
            "Open and closed items",
            ": 2 open, 1 closed.",
        ),
    ] {
        let (lines, chart) = drawn(query);
        assert_eq!(lines, want, "{query}");
        assert!(chart.contains(heading), "{query}: {chart}");
        // The text alternative states the latest numbers, and the svg points at it.
        assert!(chart.contains(numbers), "{query}: {chart}");
        assert!(chart.contains("role=\"img\""), "{chart}");
        assert!(
            chart.contains("aria-labelledby=\"chart-default-summary\""),
            "{chart}"
        );
        assert!(chart.contains("id=\"chart-default-summary\""), "{chart}");
    }
    // Each line ends in its label, in words, so color is not what tells them apart…
    let (_, both) = drawn("?status=all");
    assert!(both.contains(">open 2</text>"), "{both}");
    assert!(both.contains(">closed 1</text>"), "{both}");
    // …and only one is dashed.
    assert!(CSS.contains(".chart-line.chart-closed { stroke: var(--muted); stroke-dasharray:"));
    assert!(!CSS.contains(".chart-line.chart-open { stroke: var(--accent); stroke-dasharray:"));
    // A filter's table has only that filter's numbers.
    let (_, open) = drawn("?status=open");
    assert!(
        open.contains(">Open</th>") && !open.contains(">Closed</th>"),
        "{open}"
    );
    assert!(
        both.contains(">Open</th>") && both.contains(">Closed</th>"),
        "{both}"
    );
}

#[test]
fn a_chart_labels_its_axes_and_has_its_numbers_in_a_table_behind_a_disclosure() {
    let server = Server::readers(&["default"], &["default"]);
    server.seed("default", 1, 0);
    server.file_back("default", 3);
    server.seed("default", 1, 0);
    let page = server.ok("/l/default?status=all");
    let chart = chart(&page);
    // Four days, each a point; the middle ones are on the line even with no change in them.
    let line = &chart[chart.find("<polyline").unwrap()..];
    let points = line.split("points=\"").nth(1).unwrap();
    assert_eq!(points[..points.find('"').unwrap()].split(' ').count(), 4);
    assert!(chart.contains(">Items</text>"), "{chart}");
    assert!(chart.contains(">Date (UTC)</text>"), "{chart}");
    assert!(chart.contains("Daily counts from "), "{chart}");
    let details = &chart[chart.find("<details").expect("a disclosure")..];
    assert!(
        details.contains("<summary>Data table</summary>"),
        "{details}"
    );
    assert!(details.contains("<table"), "{details}");
    assert!(details.contains("scope=\"row\""), "{details}");
    // The day it was filed, and today, a day with a second filing: two rows, not four.
    assert_eq!(details.matches("<th scope=\"row\"").count(), 2, "{details}");
    // No script and no library: the drawing is the page's own markup.
    assert!(!chart.contains("<script"), "{chart}");
}

#[test]
fn an_empty_ledger_says_so_rather_than_drawing_an_empty_axis() {
    let server = Server::readers(&["default"], &["default"]);
    let page = server.ok("/l/default");
    let chart = chart(&page);
    assert!(chart.contains("nothing to chart"), "{chart}");
    assert!(!chart.contains("<svg"), "{chart}");
}

/// ★ `counts` refuses a ledger with a closed item it cannot date, and gonk's oldest ledgers can
/// hold one until `backfill-closed-at` has run. Neither page may fall over with it.
#[test]
fn a_ledger_whose_counts_refuse_keeps_its_page_and_says_why() {
    let server = Server::readers(&["default", "alpha"], &["default", "alpha"]);
    server.seed("default", 1, 1);
    server.seed("alpha", 1, 0);
    server.update(
        "default",
        "DELETE { ?item ledger:closedAt ?at } WHERE { ?item ledger:closedAt ?at }",
    );
    let page = server.ok("/l/default");
    assert!(
        chart(&page).contains("No chart: the counts could not be read"),
        "{page}"
    );
    assert!(chart(&page).contains("backfill-closed-at"), "{page}");
    assert!(
        page.contains("sample item 0"),
        "the list is still there: {page}"
    );
    let dashboard = server.ok("/dashboard");
    assert_eq!(charted(&dashboard), ["default", "alpha"]);
    assert!(dashboard.contains("backfill-closed-at"), "{dashboard}");
    assert_eq!(lines(&dashboard).len(), 2, "alpha's chart still draws");
}

/// ★ Ledger #2970: the same refusal a slow ledger gets on a real page, from the store's own clock.
/// A base of zero runs out before any read answers, for every caller but root, who seeds here
/// (root's budget is the ceiling). A viewer holding no budget grant gets the base, as a signed-in
/// viewer does the real 5 s. The sentence says it was the clock and who can change that, and
/// none of the store's own text about it reaches the page.
#[test]
fn a_time_budget_refusal_says_the_counts_took_too_long_and_how_to_raise_it() {
    let store = DurableStore::in_memory()
        .unwrap()
        .with_time_budget(TimeBudget::new(Duration::ZERO).with_max_overdue(64));
    let server = Server::over(
        store,
        &["default"],
        grants_for_all(&["default".to_string()], Authority::Read).unwrap(),
    );
    server.seed("default", 1, 1);
    let page = server.ok("/dashboard");
    let chart = chart(&page);
    assert!(
        chart.contains(
            "No chart: the counts could not be read: they took longer than your time budget"
        ),
        "{chart}"
    );
    assert!(chart.contains("urn:cap:store:budget:&lt;ms&gt;"), "{chart}");
    for internal in ["ledger #964", "SPARQL", "ran past", "timeout:"] {
        assert!(!chart.contains(internal), "{internal}: {chart}");
    }
}

#[test]
fn the_dashboard_lists_exactly_the_ledgers_the_viewer_may_read() {
    let server = Server::readers(&["default", "alpha", "secret"], &["default", "alpha"]);
    server.seed("default", 2, 1);
    server.seed("alpha", 1, 0);
    server.seed("secret", 3, 3);
    let page = server.ok("/dashboard");
    assert_eq!(charted(&page), ["default", "alpha"]);
    assert!(!page.contains("/l/secret"), "{page}");
    assert!(!page.contains(">secret<"), "{page}");
    assert!(page.contains("2 ledgers you may read"), "{page}");
    // Each links to its ledger and shows both lines, and says its own numbers.
    assert!(
        page.contains("<h3 id=\"chart-alpha-title\"><a href=\"/l/alpha\">alpha</a></h3>"),
        "{page}"
    );
    assert_eq!(lines(&page).len(), 4, "{page}");
    assert!(page.contains(": 2 open, 1 closed."), "{page}");
    assert!(page.contains(": 1 open, 0 closed."), "{page}");
    assert!(page.contains("Data table for alpha"), "{page}");
    // One h1, the panel's h2 over its h3s (its tab is what a person reads), and a region named by
    // that tab (ledger #2971).
    assert_eq!(page.matches("<h1").count(), 1, "{page}");
    assert!(
        page.contains("aria-labelledby=\"dashboard-tab-ledgers\""),
        "{page}"
    );
    assert!(
        page.contains("<h2 class=\"sr-only\" id=\"dashboard-ledgers\">Ledgers</h2>"),
        "{page}"
    );
    assert!(page.contains(" lang=\"en\""), "{page}");
}

/// A grant for the ledger with no grant for its graph does not read it: absent, not an error, the
/// same as the header's menu.
#[test]
fn half_a_grant_reads_nothing_and_is_absent() {
    let mut anonymous = grants_for("default", Authority::Read).unwrap();
    anonymous.push(Ledger::parse("alpha").unwrap().cap_read());
    let server = Server::start(&["default", "alpha"], anonymous);
    server.seed("alpha", 1, 0);
    let page = server.ok("/dashboard");
    assert_eq!(charted(&page), ["default"]);
}

#[test]
fn the_top_navigation_offers_the_dashboard_to_whoever_reads_a_ledger() {
    let server = Server::readers(&["default"], &["default"]);
    let page = server.ok("/l/default");
    let header = &page[page.find("<header").unwrap()..page.find("</header>").unwrap()];
    assert!(
        header.contains("<a class=\"navlink\" href=\"/dashboard\">Dashboard</a>"),
        "{header}"
    );
    let page = server.ok("/dashboard");
    let header = &page[page.find("<header").unwrap()..page.find("</header>").unwrap()];
    assert!(
        header.contains("aria-current=\"page\" class=\"navlink\" href=\"/dashboard\""),
        "{header}"
    );

    // Nothing readable: no door in the header, and the page itself says how to get in.
    let nobody = Server::readers(&["default"], &[]);
    let page = nobody.ok("/dashboard");
    let header = &page[page.find("<header").unwrap()..page.find("</header>").unwrap()];
    assert!(!header.contains("/dashboard"), "{header}");
    assert!(page.contains("Sign in to read the ledgers."), "{page}");
    assert!(!page.contains("<figure"), "{page}");
}

/// Every invocation a kernel records while this is installed.
#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// The hub's work for one Dashboard load: store queries, chart transforms that ran rather than
/// hit, and counts reads.
fn one_load(server: &Server, reader: &Capability) -> (usize, usize, usize) {
    let trace = Arc::new(Collect::default());
    server.hub.set_tracer(trace.clone());
    let page = request(Verb::Source, ikigai_gonk::dashboard::PAGE_IRI, &[]);
    let html = futures::executor::block_on(server.http.issue_traced(
        page,
        reader,
        Arc::new(Collect::default()),
    ))
    .expect("the Dashboard renders");
    server.hub.clear_tracer();
    assert!(String::from_utf8_lossy(&html.bytes).contains("<figure"));
    let events = std::mem::take(&mut *trace.0.lock().unwrap());
    let queries = events
        .iter()
        .filter(|e| e.target.starts_with("urn:iki:store:"))
        .count();
    let transforms = events
        .iter()
        .filter(|e| e.target == ikigai_gonk::render::RENDER_IRI && !e.cache_hit)
        .count();
    let counts = events
        .iter()
        .filter(|e| e.target.ends_with(":counts"))
        .count();
    (queries, transforms, counts)
}

/// What one load costs with every ledger present: the numbers the pull request quotes.
#[test]
fn a_load_between_writes_asks_the_store_nothing_and_a_write_redraws_one_chart() {
    const NAMES: [&str; 4] = ["default", "alpha", "beta", "gamma"];
    let server = Server::readers(&NAMES, &NAMES);
    for name in NAMES {
        server.seed(name, 2, 1);
    }
    let names: Vec<String> = NAMES.iter().map(|s| s.to_string()).collect();
    let reader = Capability::scoped(grants_for_all(&names, Authority::Read).unwrap());

    // Cold: each ledger's counts, one store query each, and one transform each for its chart. The
    // agents are the other tab's (ledger #2971; `tests/agents_2820.rs` holds that tab's cost).
    assert_eq!(one_load(&server, &reader), (4, 4, 4));
    // Between writes: four cached reads, and nothing below them.
    assert_eq!(one_load(&server, &reader), (0, 0, 4));
    // ⚠ A write to one ledger: every ledger's counts are read again (measured: a store
    // read rests on the store's write thread, not on one ledger's), and only the chart whose
    // numbers moved is drawn again, because a chart's render is keyed on its document.
    server.seed("beta", 1, 0);
    assert_eq!(one_load(&server, &reader), (4, 1, 4));
    assert_eq!(one_load(&server, &reader), (0, 0, 4));
}
