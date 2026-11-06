//! Ledger [#2967](http://localhost:1062/l/default/item/2967): an open Dashboard refreshes each chart
//! and agent table by polling its own fragment, with a validator, and a person can pause it.
//!
//! These read the door over real HTTP. What a browser does with it (a close updating the chart, focus
//! staying put, the pause surviving a reload) is `tests/browser/dashboard_poll_2967.mjs`. The ledger
//! names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, dashboard, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

const CSS: &str = include_str!("../web/gonk.css");
const JS: &str = include_str!("../web/gonk.js");

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _config: tempfile::TempDir,
}

/// One HTTP answer.
struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn etag(&self) -> String {
        self.header("etag")
            .unwrap_or_else(|| panic!("no ETag: {}", self.head))
            .to_string()
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    /// A server configured with `configured` ledgers, whose anonymous caller reads `readable`.
    fn readers(configured: &[&str], readable: &[&str]) -> Server {
        let names: Vec<String> = readable.iter().map(|s| s.to_string()).collect();
        let anonymous = grants_for_all(&names, Authority::Read).unwrap();
        // The door's stacks, as `main` builds them: the agent questions go deep.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
            .enable_all()
            .build()
            .unwrap();
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
            toolbox: false,
            faces: Vec::new(),
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous,
            bind: addr,
            passkeys: Some(passkeys),
            public: Vec::new(),
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

    /// A GET from this page's own script, with whatever `extra` header lines it adds.
    fn get(&self, path: &str, extra: &str) -> Answer {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html,*/*;q=0.8\r\n\
             Sec-Fetch-Site: same-origin\r\nHX-Request: true\r\n{extra}Connection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        Answer {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn if_none_match(&self, path: &str, tag: &str) -> Answer {
        self.get(path, &format!("If-None-Match: {tag}\r\n"))
    }

    fn ok(&self, path: &str) -> Answer {
        let answer = self.get(path, "");
        assert_eq!(answer.status, 200, "{path}: {}", answer.body);
        answer
    }

    /// A browser's navigation to the page.
    fn page(&self) -> String {
        self.page_at("/dashboard")
    }

    /// A browser's navigation to `path`.
    fn page_at(&self, path: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html,*/*;q=0.8\r\n\
             Sec-Fetch-Site: none\r\nConnection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        body.replace('\'', "\"")
    }

    fn act(&self, ledger: &str, action: &str, args: &[(&str, &str)]) {
        let iri = format!("{}{action}", Ledger::parse(ledger).unwrap().prefix());
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(Verb::Sink, &iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
    }

    fn file(&self, ledger: &str, title: &str) {
        self.act(ledger, "append", &[("content", title)]);
    }

    /// Lose every `closedAt` in `ledger`, the state `counts` refuses until a backfill.
    fn undate_closes(&self, ledger: &str) {
        let graph = Ledger::parse(ledger).unwrap().graph();
        let update = format!(
            "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
             WITH <{graph}> DELETE {{ ?item ledger:closedAt ?at }} \
             WHERE {{ ?item ledger:closedAt ?at }}"
        );
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(
                Verb::Sink,
                "urn:iki:store:graph-update",
                &[("graph", graph.as_str()), ("content", &update)],
            ),
            &Capability::root(),
        ))
        .expect("the update applies");
    }
}

#[test]
fn a_fragment_answers_304_while_unchanged_and_200_after_a_write() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    for path in ["/dashboard/chart/default", "/dashboard/agents/default"] {
        let first = server.ok(path);
        assert!(first.body.contains("data-live"), "{}", first.body);
        // ★ No browser cache holds a viewer's numbers: the page's script sends the validator.
        assert_eq!(first.header("cache-control"), Some("no-store"), "{path}");
        let unchanged = server.if_none_match(path, &first.etag());
        assert_eq!(unchanged.status, 304, "{path}");
        assert_eq!(unchanged.body, "", "{path}: a 304 costs no body");
        assert_eq!(unchanged.etag(), first.etag(), "{path}");
        // A validator it never handed out is not a match.
        assert_eq!(
            server.if_none_match(path, "\"other\"").status,
            200,
            "{path}"
        );
    }

    let chart = "/dashboard/chart/default";
    let before = server.ok(chart).etag();
    assert!(server.ok(chart).body.contains(": 1 open, 0 closed."));
    server.act("default", "close", &[("item", "1"), ("reason", "done")]);
    let after = server.if_none_match(chart, &before);
    assert_eq!(after.status, 200, "a write changes the chart");
    assert_ne!(after.etag(), before);
    assert!(after.body.contains(": 0 open, 1 closed."), "{}", after.body);
    assert_eq!(server.if_none_match(chart, &after.etag()).status, 304);

    // An agent who claims is a new row in that ledger's table.
    let table = "/dashboard/agents/default";
    let before = server.ok(table).etag();
    server.file("default", "another");
    server.act(
        "default",
        "claim",
        &[("item", "2"), ("content", "sample-agent")],
    );
    let after = server.if_none_match(table, &before);
    assert_eq!(after.status, 200);
    assert!(after.body.contains("sample-agent"), "{}", after.body);
}

/// The poll fetches the view the page composed, not a second drawing of it.
#[test]
fn the_page_is_made_of_the_fragments_it_polls() {
    let server = Server::readers(&["default", "alpha"], &["default", "alpha"]);
    server.file("alpha", "sample item");
    for (tab, path) in [
        ("/dashboard/ledgers", "/dashboard/chart/default"),
        ("/dashboard/ledgers", "/dashboard/chart/alpha"),
        ("/dashboard/agents", "/dashboard/agents/default"),
        ("/dashboard/agents", "/dashboard/agents/alpha"),
    ] {
        let page = server.page_at(tab);
        let fragment = server.ok(path).body.replace('\'', "\"");
        assert!(page.contains(&fragment), "{path}: {fragment}\n\n{page}");
        for wired in [
            format!("hx-get=\"{path}\""),
            format!(
                "hx-trigger=\"every {}, gonk:refresh\"",
                dashboard::POLL_EVERY
            ),
            "hx-swap=\"outerHTML\"".to_string(),
            "hx-sync=\"this:drop\"".to_string(),
            "data-ui-quiet=\"\"".to_string(),
        ] {
            assert!(fragment.contains(&wired), "{wired}: {fragment}");
        }
    }
    // The sentence that names the tables' minute moves with them, and is not news.
    let page = server.page_at("/dashboard/agents");
    assert!(page.contains("data-live=\"note\""), "{page}");
    assert!(page.contains("hx-get=\"/dashboard/agents-note\""), "{page}");
    assert!(server.ok("/dashboard/agents-note").body.contains("As of "));
}

#[test]
fn the_page_names_its_pause_control_and_is_honest_with_no_script() {
    let server = Server::readers(&["default"], &["default"]);
    let page = server.page();
    let button =
        "<button hidden=\"hidden\" id=\"dashboard-pause\" type=\"button\">Pause updates</button>";
    assert!(page.contains(button), "{page}");
    // Before what it pauses, so nobody has to get past the updates to reach it.
    assert!(page.find(button).unwrap() < page.find("id=\"dashboard-ledgers\"").unwrap());
    // With no script nothing polls, so there is nothing to pause and the sentence says so…
    assert!(
        page.contains(">These numbers are as of when this page loaded.</span>"),
        "{page}"
    );
    // …and the script has the running sentence from the server, with the cadence in it.
    assert!(
        page.contains("data-running=\"These numbers update every 30 seconds"),
        "{page}"
    );
    assert_eq!(dashboard::POLL_EVERY, "30s");
    // One live region: an update is said in #status, and the fragments carry none of their own.
    assert_eq!(page.matches("aria-live").count(), 1, "{page}");
    // Nothing to pause on a page with no chart.
    let nobody = Server::readers(&["default"], &[]);
    assert!(!nobody.page().contains("dashboard-pause"));
}

/// ★ `counts` refuses until an operator runs `backfill-closed-at`, and no poll can change that.
#[test]
fn a_chart_whose_counts_refuse_keeps_its_sentence_and_is_not_polled() {
    let server = Server::readers(&["default", "alpha"], &["default", "alpha"]);
    server.file("default", "sample item");
    server.file("alpha", "sample item");
    server.act("default", "close", &[("item", "1"), ("reason", "done")]);
    let healthy = server.ok("/dashboard/chart/default");
    assert!(healthy.body.contains("hx-get"), "{}", healthy.body);
    server.undate_closes("default");

    let page = server.page();
    assert!(page.contains("backfill-closed-at"), "{page}");
    assert!(
        !page.contains("hx-get=\"/dashboard/chart/default\""),
        "{page}"
    );
    assert!(page.contains("hx-get=\"/dashboard/chart/alpha\""), "{page}");
    // Its agent table is still good, and the other chart still draws.
    let agents = server.page_at("/dashboard/agents");
    assert!(
        agents.contains("hx-get=\"/dashboard/agents/default\""),
        "{agents}"
    );
    // A page open across the moment it began refusing: its answer brings the sentence and no timer.
    let refused = server.if_none_match("/dashboard/chart/default", &healthy.etag());
    assert_eq!(refused.status, 200);
    assert!(
        refused.body.contains("backfill-closed-at"),
        "{}",
        refused.body
    );
    assert!(!refused.body.contains("hx-get"), "{}", refused.body);
}

/// ★ The read runs under the viewer's capability before any validator is compared, so a viewer who
/// lost the grant is refused, not told their old numbers still stand.
#[test]
fn a_ledger_the_viewer_may_not_read_is_refused_whatever_the_validator() {
    let server = Server::readers(&["default", "secret"], &["default"]);
    server.file("secret", "sample item");
    assert!(!server.page().contains("/dashboard/chart/secret"));
    assert!(!server
        .page_at("/dashboard/agents")
        .contains("/dashboard/agents/secret"));
    for path in ["/dashboard/chart/secret", "/dashboard/agents/secret"] {
        assert_eq!(server.get(path, "").status, 403, "{path}");
        assert_eq!(server.if_none_match(path, "*").status, 403, "{path}");
    }
}

/// The ledger page's chart has its own request to be drawn by: only the Dashboard polls.
#[test]
fn the_ledger_page_does_not_poll_its_chart() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    let page = server.ok("/l/default").body;
    assert!(page.contains("<figure"), "{page}");
    assert!(!page.contains("data-live"), "{page}");
}

#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// The hub's work for one poll of `iri`: store queries, and transforms that ran rather than hit.
fn one_poll(server: &Server, iri: &str, reader: &Capability) -> (usize, usize) {
    let trace = Arc::new(Collect::default());
    server.hub.set_tracer(trace.clone());
    futures::executor::block_on(server.http.issue_traced(
        request(Verb::Source, iri, &[]),
        reader,
        Arc::new(Collect::default()),
    ))
    .expect("the fragment answers");
    server.hub.clear_tracer();
    let events = std::mem::take(&mut *trace.0.lock().unwrap());
    (
        events
            .iter()
            .filter(|e| e.target.starts_with("urn:iki:store:"))
            .count(),
        events
            .iter()
            .filter(|e| e.target == ikigai_gonk::render::RENDER_IRI && !e.cache_hit)
            .count(),
    )
}

/// Cheap by construction, the numbers the pull request quotes with its timings: between writes a
/// poll asks the store nothing and transforms nothing.
#[test]
fn a_poll_between_writes_asks_the_store_nothing() {
    let names = ["default".to_string()];
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    let reader = Capability::scoped(grants_for_all(&names, Authority::Read).unwrap());
    let chart = "urn:iki:gonk:fragment:dashboard-chart:default";
    let table = "urn:iki:gonk:fragment:dashboard-agents:default";

    assert_eq!(one_poll(&server, chart, &reader), (1, 1), "cold");
    assert_eq!(one_poll(&server, table, &reader), (1, 1), "cold");
    for _ in 0..3 {
        assert_eq!(one_poll(&server, chart, &reader), (0, 0), "idle");
        assert_eq!(one_poll(&server, table, &reader), (0, 0), "idle");
    }
    // A write: one read of the counts and one drawing of the chart whose numbers moved; a table
    // with no agents is read again and its document has not changed, so it is not drawn.
    server.file("default", "another");
    assert_eq!(one_poll(&server, chart, &reader), (1, 1), "after a write");
    assert_eq!(one_poll(&server, table, &reader), (1, 0), "after a write");
    assert_eq!(one_poll(&server, chart, &reader), (0, 0), "idle again");
}

/// The script's half is run by `tests/browser/dashboard_poll_2967.mjs`; this pins it as WIRING, so
/// CI notices the parts going missing.
#[test]
fn the_script_holds_the_polls_and_sends_the_validator() {
    for wired in [
        // WCAG 2.2.2: paused, the hidden tab and a focused panel are all held, before asking…
        "livePaused || liveStopped || document.hidden || elt.contains(document.activeElement)",
        "document.addEventListener(\"htmx:confirm\"",
        "document.addEventListener(\"visibilitychange\", liveRefresh)",
        // …remembered per browser…
        "window.localStorage.setItem(LIVE_PAUSED, \"true\")",
        // …conditional, with a 304 told not to swap, as htmx's rule for `[23]..` would…
        "e.detail.headers[\"If-None-Match\"] = tag",
        "detail.xhr.status === 304",
        "detail.shouldSwap = false",
        // …and a real update said politely, without touching focus.
        "announce(\"Counts updated.\")",
    ] {
        assert!(JS.contains(wired), "web/gonk.js: {wired}");
    }
    // No focus call anywhere in the polling section.
    let section = &JS[JS.find("the Dashboard's polling (ledger #2967)").unwrap()
        ..JS.find("function wireDashboard()").unwrap()];
    assert!(!section.contains(".focus("), "{section}");
    // ★ Canceled any later than `htmx:confirm`, a poll would leave the shell's `aria-busy` set for
    // good. (`tests/web.rs` holds the whole file to no listener on the event after it.)
    assert!(section.contains("htmx:confirm"), "{section}");
}

/// `prefers-reduced-motion`: an update has no motion in it to reduce. The element adds no box.
#[test]
fn an_update_has_no_motion_and_the_fragment_adds_no_box() {
    assert!(CSS.contains(".dashboard-live { display: contents; }"));
    for line in CSS.lines() {
        if line.contains(".dashboard-live") || line.contains(".chart") {
            assert!(
                !line.contains("transition") && !line.contains("animation"),
                "{line}"
            );
        }
    }
    assert!(!CSS.contains(".htmx-swapping"), "no swap transition");
    assert!(!CSS.contains(".htmx-settling"), "no settle transition");
    assert!(CSS.contains("@media (prefers-reduced-motion: reduce)"));
}
