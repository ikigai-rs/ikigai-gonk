//! Ledger [#2820](http://localhost:1062/l/default/item/2820): a summary per agent on the Dashboard,
//! every number a click-through to the SPARQL that answers it.
//!
//! These read the pages over real HTTP and write through the ledger's own actions, except a history
//! older than the test, which only a raw update can write. What a browser draws is in the pull
//! request. The names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;
use serde_json::Value;

/// What each column of the panel is, in order.
const COLUMNS: [&str; 7] = [
    "in-flight",
    "worked-on",
    "completed-24h",
    "completed-7d",
    "handed-back-24h",
    "released",
    "median-claim-to-handback",
];

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    http: Arc<Kernel>,
    _config: tempfile::TempDir,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    /// A server configured with `configured` ledgers, whose anonymous caller reads `readable`.
    fn readers(configured: &[&str], readable: &[&str]) -> Server {
        let names: Vec<String> = readable.iter().map(|s| s.to_string()).collect();
        let anonymous = grants_for_all(&names, Authority::Read).unwrap();
        // The door's stacks, as `main` builds them: the same reason as [`Server::ask`].
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

    /// A request straight to the HTTP door's kernel, on a stack the size the door gives one: a
    /// composition nests further than a debug build fits in a test thread's.
    fn ask(
        &self,
        iri: &str,
        args: &[(&str, &str)],
        reader: &Capability,
    ) -> ikigai_core::Result<Vec<u8>> {
        let (http, read, reader) = (
            Arc::clone(&self.http),
            request(Verb::Source, iri, args),
            reader.clone(),
        );
        std::thread::Builder::new()
            .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
            .spawn(move || futures::executor::block_on(http.issue(read, &reader)))
            .unwrap()
            .join()
            .unwrap()
            .map(|answer| answer.bytes)
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

    fn act(&self, ledger: &str, action: &str, args: &[(&str, &str)]) {
        let prefix = Ledger::parse(ledger).unwrap().prefix();
        self.issue(Verb::Sink, &format!("{prefix}{action}"), args);
    }

    /// File an item, whose number is the next: the caller counts them.
    fn file(&self, ledger: &str, title: &str) {
        self.act(ledger, "append", &[("content", title)]);
    }

    /// Time moves between two steps of a story, as it does between real ones: the questions order
    /// them by millisecond.
    fn tick() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    fn claim(&self, ledger: &str, item: &str, holder: &str) {
        Self::tick();
        self.act(ledger, "claim", &[("item", item), ("content", holder)]);
    }

    fn hand_back(&self, ledger: &str, item: &str) {
        Self::tick();
        self.act(
            ledger,
            "comment",
            &[
                ("item", item),
                ("content", "evidence"),
                ("handback", "true"),
            ],
        );
    }

    fn release(&self, ledger: &str, item: &str) {
        Self::tick();
        let prefix = Ledger::parse(ledger).unwrap().prefix();
        self.issue(Verb::Delete, &format!("{prefix}claim"), &[("item", item)]);
    }

    fn close(&self, ledger: &str, item: &str, reason: &str) {
        self.act(ledger, "close", &[("item", item), ("reason", reason)]);
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
            &[("graph", &graph), ("content", &update)],
        );
    }

    /// Age `item`'s hand-backs and close by `hours`.
    fn age(&self, ledger: &str, item: u64, hours: u32) {
        self.update(
            ledger,
            &format!(
                "DELETE {{ ?comment dcterms:created ?at . ?item ledger:closedAt ?closed }}\n\
                 INSERT {{ ?comment dcterms:created ?old . ?item ledger:closedAt ?old }}\n\
                 WHERE {{\n\
                   ?item ledger:number {item} ; ledger:closedAt ?closed .\n\
                   ?comment ledger:onItem ?item ; ledger:handedBackBy ?by ; dcterms:created ?at .\n\
                   BIND(?at - \"PT{hours}H\"^^xsd:dayTimeDuration AS ?old)\n\
                 }}"
            ),
        );
    }

    /// What the Dashboard says of `holder` in `ledger`: each column's visible number and its link.
    fn cells(&self, ledger: &str, holder: &str) -> Vec<(String, String)> {
        let page = self.ok("/dashboard/agents");
        cells_in(&page, ledger, holder)
    }

    /// A link's query through the SPARQL face: the JSON answer.
    fn through_the_face(&self, href: &str) -> Value {
        let href = href.strip_suffix("#results").unwrap_or(href);
        let (status, body) = self.page(&format!("{href}&as=application%2Fsparql-results%2Bjson"));
        assert_eq!(status, 200, "{href}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    /// The numbers the Dashboard shows for `holder` in `ledger`, in column order.
    fn numbers(&self, ledger: &str, holder: &str) -> Vec<String> {
        self.cells(ledger, holder)
            .into_iter()
            .map(|c| c.0)
            .collect()
    }
}

/// The panel of `ledger` in `page`, from its heading to the end of its section.
fn agents_panel<'a>(page: &'a str, ledger: &str) -> Option<&'a str> {
    let start = page.find(&format!("id=\"agents-{ledger}-title\""))?;
    let rest = &page[start..];
    Some(&rest[..rest.find("</section>").unwrap()])
}

fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
}

/// Text with its tags dropped.
fn visible(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    unescape(&out)
}

fn cells_in(page: &str, ledger: &str, holder: &str) -> Vec<(String, String)> {
    let panel =
        agents_panel(page, ledger).unwrap_or_else(|| panic!("no panel for {ledger}: {page}"));
    let row = panel
        .split("<tr>")
        .find(|row| {
            row.split_once("</th>")
                .is_some_and(|(th, _)| visible(th) == holder)
                && row.contains("<td>")
        })
        .unwrap_or_else(|| panic!("no row for {holder}: {panel}"));
    let cells: Vec<(String, String)> = row
        .split("<td>")
        .skip(1)
        .map(|cell| {
            let href = cell
                .split("href=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            let shown = cell
                .split("</span>")
                .nth(1)
                .unwrap()
                .split("<span")
                .next()
                .unwrap();
            (unescape(shown), unescape(href))
        })
        .collect();
    assert_eq!(cells.len(), COLUMNS.len(), "{row}");
    cells
}

fn rows_of(answer: &Value) -> usize {
    answer["results"]["bindings"].as_array().unwrap().len()
}

/// The reproduction: agents who claimed, handed back and closed, and a Dashboard saying so, one
/// number and one link per question. On `main` this page has no agent panel at all.
#[test]
fn the_dashboard_has_a_panel_of_the_agents() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "lawnboy-claude");
    let page = server.ok("/dashboard/agents");
    assert!(page.contains(">Agents</h2>"), "{page}");
    let panel = agents_panel(&page, "default").expect("a panel for the ledger");
    assert!(panel.contains("lawnboy-claude"), "{panel}");
    assert_eq!(server.numbers("default", "lawnboy-claude")[..2], ["1", "1"]);
}

/// The story each question is about, and what each says of it.
#[test]
fn each_question_answers_what_it_says_and_equals_its_own_link() {
    let server = Server::readers(&["default"], &["default"]);
    for n in 1..=10 {
        server.file("default", &format!("sample item {n}"));
    }
    let (ann, bob) = ("ann", "bob");
    // 1: claimed, handed back, closed done: completed, in both windows.
    server.claim("default", "1", ann);
    server.hand_back("default", "1");
    server.close("default", "1", "done");
    // 2: claimed and still held.
    server.claim("default", "2", ann);
    // 3: let go with no hand-back.
    server.claim("default", "3", ann);
    server.release("default", "3");
    // 4: handed back and then released: not "released".
    server.claim("default", "4", ann);
    server.hand_back("default", "4");
    server.release("default", "4");
    // 5: handed back, released, claimed again and let go: released, for the second claim.
    server.claim("default", "5", ann);
    server.hand_back("default", "5");
    server.release("default", "5");
    server.claim("default", "5", ann);
    server.release("default", "5");
    // 6: both handed it back, and it was closed done: bob's, whose hand-back was last.
    server.claim("default", "6", ann);
    server.hand_back("default", "6");
    server.release("default", "6");
    server.claim("default", "6", bob);
    server.hand_back("default", "6");
    server.close("default", "6", "done");
    // 7: handed back but closed as something other than done: no completion.
    server.claim("default", "7", ann);
    server.hand_back("default", "7");
    server.close("default", "7", "wontfix");
    // 8: completed three days ago: in the week's window, not the day's.
    server.claim("default", "8", ann);
    server.hand_back("default", "8");
    server.close("default", "8", "done");
    server.age("default", 8, 72);
    // 9: completed ten days ago: in neither.
    server.claim("default", "9", ann);
    server.hand_back("default", "9");
    server.close("default", "9", "done");
    server.age("default", 9, 240);
    // 10: closed under ann's claim with no hand-back: the close ended it, so it is not "released".
    server.claim("default", "10", ann);
    server.close("default", "10", "done");

    let ann_cells = server.cells("default", ann);
    let number = |cells: &[(String, String)], name: &str| {
        cells[COLUMNS.iter().position(|c| *c == name).unwrap()]
            .0
            .clone()
    };
    for (name, want) in [
        ("in-flight", "1"),
        ("worked-on", "10"),
        // 1. Item 6's last hand-back is bob's, and 8 and 9 are older.
        ("completed-24h", "1"),
        // 1 and 8.
        ("completed-7d", "2"),
        // 1, 4, 5, 6, 7.
        ("handed-back-24h", "5"),
        // 3 and 5.
        ("released", "2"),
    ] {
        assert_eq!(number(&ann_cells, name), want, "ann: {name}");
    }
    let bob_cells = server.cells("default", bob);
    for (name, want) in [
        ("in-flight", "0"),
        ("worked-on", "1"),
        ("completed-24h", "1"),
        ("completed-7d", "1"),
        ("handed-back-24h", "1"),
        ("released", "0"),
    ] {
        assert_eq!(number(&bob_cells, name), want, "bob: {name}");
    }

    // ★ The acceptance: each number is the row count of its own link's query, through the SPARQL
    // face and on the page the link opens, which shows the results with no further click.
    for (cells, holder) in [(&ann_cells, ann), (&bob_cells, bob)] {
        for (column, (shown, href)) in COLUMNS.iter().zip(cells.iter()) {
            assert!(href.starts_with("/sparql?ledger=default&query="), "{href}");
            // It opens the tab at the results, which the page has already run.
            let path = href
                .strip_suffix("#results")
                .expect("a link to the results");
            let answer = server.through_the_face(href);
            let opened = server.ok(path);
            assert!(opened.contains("id=\"results\""), "{opened}");
            if *column != "median-claim-to-handback" {
                assert_eq!(
                    rows_of(&answer).to_string(),
                    *shown,
                    "{holder}: {column}: {href}"
                );
            }
            let summary = format!(
                "{} row(s) from urn:iki:ledger:graph:default",
                rows_of(&answer)
            );
            assert!(opened.contains(&summary), "{column}: {opened}");
        }
    }
}

/// A takeover is a let-go, whoever's claim it replaced, and the new holder holds the item.
#[test]
fn a_takeover_counts_as_released_for_the_holder_it_replaced() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    server.act(
        "default",
        "claim",
        &[("item", "1"), ("content", "ann"), ("lease", "PT1S")],
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    server.act(
        "default",
        "claim",
        &[
            ("item", "1"),
            ("content", "bob"),
            ("takeover", "true"),
            ("from", "ann"),
        ],
    );
    let (ann, bob) = (
        server.numbers("default", "ann"),
        server.numbers("default", "bob"),
    );
    assert_eq!((&ann[0][..], &ann[5][..]), ("0", "1"), "{ann:?}");
    assert_eq!((&bob[0][..], &bob[5][..]), ("1", "0"), "{bob:?}");
}

/// One holder's hand-backs at the given minutes after their claims, as raw history.
fn seed_times(server: &Server, ledger: &str, holder: &str, minutes: &[u32]) {
    let mut triples = String::new();
    for (n, minutes) in minutes.iter().enumerate() {
        let item = format!("urn:sample:{holder}:{n}");
        let (days, hours, mins) = (minutes / 1440, minutes % 1440 / 60, minutes % 60);
        triples.push_str(&format!(
            "<{item}> a ledger:Item ; ledger:number {number} ; dcterms:title \"t\" .\n\
             <{item}:claim> a ledger:ClaimEvent ; ledger:onItem <{item}> ; \
             ledger:claimedBy \"{holder}\" ; \
             dcterms:created \"2026-01-01T00:00:00Z\"^^xsd:dateTime .\n\
             <{item}:back> ledger:onItem <{item}> ; ledger:handedBackBy \"{holder}\" ; \
             dcterms:created \"2026-01-{day:02}T{hours:02}:{mins:02}:00Z\"^^xsd:dateTime .\n",
            number = n + 100,
            day = days + 1,
        ));
    }
    server.update(ledger, &format!("INSERT {{ {triples} }} WHERE {{}}"));
}

/// ⚠ Durations order by their length, and their text does not: `P2D` sorts before `PT1H`.
#[test]
fn the_median_orders_durations_by_length_and_takes_the_lower_middle() {
    let server = Server::readers(&["default"], &["default"]);
    // 2 days, 1 hour, 90 minutes: the middle is 90 minutes.
    seed_times(&server, "default", "odd", &[2880, 60, 90]);
    // 2 min, 2 days, 1 h, 3 h: the lower middle is 1 hour.
    seed_times(&server, "default", "even", &[2, 2880, 60, 180]);
    seed_times(&server, "default", "lone", &[45]);
    // No hand-back at all: the cell says so, and its link answers no row.
    server.file("default", "sample item");
    server.claim("default", "1", "idle");

    let median = |holder: &str| {
        let cells = server.cells("default", holder);
        cells[6].clone()
    };
    assert_eq!(median("odd").0, "1 h 30 min");
    assert_eq!(median("even").0, "1 h");
    assert_eq!(median("lone").0, "45 min");
    let (none, href) = median("idle");
    assert_eq!(none, "none");
    assert_eq!(rows_of(&server.through_the_face(&href)), 0);
    // The page's duration is the one its link's query answers.
    // That link lists every hand-back, shortest first, and the number is its middle row.
    let answer = server.through_the_face(&median("odd").1);
    assert_eq!(rows_of(&answer), 3);
    let rows = answer["results"]["bindings"].as_array().unwrap();
    let took: Vec<&str> = rows
        .iter()
        .map(|r| r["took"]["value"].as_str().unwrap())
        .collect();
    assert_eq!(took, ["PT1H", "PT1H30M", "P2D"]);
}

/// Each number's link says what it counts, for whom, and that it opens a new tab; the panel is a
/// table with real headers.
#[test]
fn the_panel_is_a_table_whose_links_say_what_they_count() {
    let server = Server::readers(&["default", "empty"], &["default", "empty"]);
    server.file("default", "sample item");
    server.claim("default", "1", "lawnboy-claude");
    let page = server.ok("/dashboard/agents");
    let panel = agents_panel(&page, "default").unwrap();
    assert!(panel.contains("<table"), "{panel}");
    assert!(panel.contains("<caption"), "{panel}");
    assert_eq!(
        panel.matches("<th scope=\"col\">").count(),
        1 + COLUMNS.len(),
        "{panel}"
    );
    assert!(
        panel.contains("<th scope=\"row\">lawnboy-claude</th>"),
        "{panel}"
    );
    // A row for the agent and the totals row.
    assert_eq!(
        panel.matches("target=\"_blank\"").count(),
        2 * COLUMNS.len()
    );
    assert_eq!(
        panel.matches("target=\"_blank\" rel=\"noopener\"").count(),
        2 * COLUMNS.len(),
        "{panel}"
    );
    let links: Vec<String> = panel
        .split("<a ")
        .filter(|a| a.contains("target=\"_blank\""))
        .map(|a| visible(&format!("<a {}", &a[..a.find("</a>").unwrap()])))
        .collect();
    for (link, want) in links.iter().zip([
        "lawnboy-claude: 1 in flight, opens in a new tab",
        "lawnboy-claude: 1 worked on, opens in a new tab",
        "lawnboy-claude: 0 completed in the last 24 hours, opens in a new tab",
        "lawnboy-claude: 0 completed in the last 7 days, opens in a new tab",
        "lawnboy-claude: 0 handed back in the last 24 hours, opens in a new tab",
        "lawnboy-claude: 0 released without a hand-back, opens in a new tab",
        "lawnboy-claude: none from claim to hand-back, the median, opens in a new tab",
        "All agents: 1 in flight, opens in a new tab",
        "All agents: 1 worked on, opens in a new tab",
        "All agents: 0 completed in the last 24 hours, opens in a new tab",
        "All agents: 0 completed in the last 7 days, opens in a new tab",
        "All agents: 0 handed back in the last 24 hours, opens in a new tab",
        "All agents: 0 released without a hand-back, opens in a new tab",
        "All agents: none from claim to hand-back, the median, opens in a new tab",
    ]) {
        assert_eq!(link, want);
    }
    assert_eq!(page.matches("<h1").count(), 1, "{page}");
    // The links' sr-only words stay inside the frame that scrolls, or they widen the page: measured
    // 1,235 px wide at 320 without this. A browser's reading is in the pull request.
    let css = include_str!("../web/gonk.css");
    assert!(css.contains(".agent-panel .table-wrap { position: relative; }"));
    // A ledger nobody has claimed in says so rather than showing an empty table.
    let empty = agents_panel(&page, "empty").unwrap();
    assert!(!empty.contains("<table"), "{empty}");
    assert!(empty.contains("No agent has claimed an item"), "{empty}");
}

/// Every count is read under the viewer's grant: a ledger they cannot read is absent, not an error.
#[test]
fn a_panel_appears_only_for_ledgers_the_viewer_may_read() {
    let server = Server::readers(&["default", "secret"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "shared-agent");
    server.file("secret", "sample item");
    server.claim("secret", "1", "shared-agent");
    server.file("secret", "sample item");
    server.claim("secret", "2", "hidden-agent");

    let (status, page) = server.page("/dashboard/agents");
    assert_eq!(status, 200, "{page}");
    assert!(agents_panel(&page, "default").is_some(), "{page}");
    assert!(agents_panel(&page, "secret").is_none(), "{page}");
    assert!(!page.contains("hidden-agent"), "{page}");
    assert!(!page.contains("secret"), "{page}");
    assert_eq!(server.numbers("default", "shared-agent")[0], "1");

    // The resources below the panel refuse the same viewer the same ledger.
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    for (iri, args) in [
        ("urn:iki:gonk:agent", vec![("ledger", "secret")]),
        (
            "urn:iki:gonk:agent:holder:hidden-agent",
            vec![("ledger", "secret")],
        ),
        (
            "urn:iki:gonk:agent:holder:hidden-agent:question:name:in-flight",
            vec![("ledger", "secret")],
        ),
    ] {
        let refused = server.ask(iri, &args, &reader).expect_err(iri);
        assert!(
            matches!(refused, ikigai_core::Error::Denied(_)),
            "{iri}: {refused}"
        );
    }
}

/// ⚠ A grant can hold a graph's store token without its ledger's read (a mirror's or a link's), and
/// the door's refusal of such a raw read (ledger #2836) does not see a read made inside a page.
#[test]
fn a_store_token_alone_does_not_list_a_ledgers_agents() {
    let server = Server::readers(&["default", "secret"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "shared-agent");
    server.file("secret", "sample item");
    server.claim("secret", "1", "hidden-agent");
    let secret = Ledger::parse("secret").unwrap();
    let mut scopes = grants_for_all(&["default".to_string()], Authority::Read).unwrap();
    scopes.push(ikigai_store::cap_read_graph(&secret.graph()));
    let reader = Capability::scoped(scopes);

    let all = server.ask("urn:iki:gonk:agent", &[], &reader).unwrap();
    let all = String::from_utf8_lossy(&all);
    assert!(all.contains("shared-agent"), "{all}");
    assert!(!all.contains("hidden-agent"), "{all}");
    for iri in [
        "urn:iki:gonk:agent",
        "urn:iki:gonk:agent:holder:hidden-agent",
    ] {
        let refused = server.ask(iri, &[("ledger", "secret")], &reader);
        assert!(
            matches!(refused, Err(ikigai_core::Error::Denied(_))),
            "{iri}"
        );
    }

    let nothing = Capability::scoped(vec![ikigai_store::cap_read_graph(&secret.graph())]);
    let refused = server.ask("urn:iki:gonk:agent", &[], &nothing);
    assert!(matches!(refused, Err(ikigai_core::Error::Denied(_))));
}

/// A label is free text: it is shown as text, and its link can only ask about it.
#[test]
fn a_hostile_label_stays_a_label() {
    let server = Server::readers(&["default"], &["default"]);
    let hostile = "x\" } } <script>alert(1)</script> & a:b/c %41 é";
    server.file("default", "sample item");
    server.claim("default", "1", hostile);
    server.file("default", "sample item");
    server.claim("default", "2", "someone-else");

    let page = server.ok("/dashboard/agents");
    assert!(!page.contains("<script>alert"), "{page}");
    let cells = cells_in(&page, "default", hostile);
    assert_eq!(cells[0].0, "1");
    assert_eq!(cells[1].0, "1");
    for (shown, href) in &cells[..6] {
        assert_eq!(
            rows_of(&server.through_the_face(href)).to_string(),
            *shown,
            "{href}"
        );
    }
}

/// The resources the panel is made of answer on their own, and say the same.
#[test]
fn a_question_is_a_resource_and_the_answer_is_the_panels_number() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "ann");
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    let ask = |iri: &str, args: &[(&str, &str)]| -> String {
        let answer = server
            .ask(iri, args, &reader)
            .unwrap_or_else(|e| panic!("{iri}: {e}"));
        String::from_utf8_lossy(&answer).into_owned()
    };
    for name in COLUMNS {
        let text = ask(&format!("urn:iki:gonk:question:name:{name}"), &[]);
        assert!(text.contains("?holder"), "{name}: {text}");
    }
    let agents: Value =
        serde_json::from_str(&ask("urn:iki:gonk:agent", &[("ledger", "default")])).unwrap();
    assert_eq!(agents["results"]["bindings"][0]["holder"]["value"], "ann");

    let answer: Value = serde_json::from_str(&ask(
        "urn:iki:gonk:agent:holder:ann:question:name:in-flight",
        &[("ledger", "default")],
    ))
    .unwrap();
    assert_eq!(rows_of(&answer), 1);
    // A window's start the store could not compare is refused, not counted as zero.
    let refused = server
        .ask(
            "urn:iki:gonk:agent:holder:ann:question:name:completed-24h",
            &[("ledger", "default"), ("since", "yesterday")],
            &reader,
        )
        .expect_err("a since that is no dateTime");
    assert!(refused.to_string().contains("since"), "{refused}");

    let list: Value = serde_json::from_str(&ask(
        "urn:iki:gonk:agent:holder:ann",
        &[("ledger", "default")],
    ))
    .unwrap();
    assert_eq!(list["answers"][0]["question"], "in-flight");
    assert_eq!(list["answers"][0]["answer"], "1");
    assert_eq!(
        list["answers"][0]["href"].as_str().map(unescape),
        Some(server.cells("default", "ann")[0].1.clone())
    );
}

/// ⚠ A busy agent's history must fit the store's 5 s budget, in a debug build too. A question past it
/// is `unavailable`, and it holds up that caller's next questions as well (the store allows one
/// runaway per caller). `NOT EXISTS` over the events, the obvious way to write three of these, ran
/// past it at 1,000 hand-backs on a release build; a variable given by `bindings` joined quadratically.
#[test]
fn every_question_answers_for_a_busy_agent() {
    const HANDBACKS: usize = 1500;
    let server = Server::readers(&["default"], &["default"]);
    for batch in 0..HANDBACKS / 150 {
        let mut triples = String::new();
        for n in 0..150 {
            let number = batch * 150 + n + 1;
            let item = format!("urn:sample:busy:{number}");
            let at = format!(
                "\"2026-01-02T{:02}:{:02}:00Z\"^^xsd:dateTime",
                n / 60,
                n % 60
            );
            triples.push_str(&format!(
                "<{item}> a ledger:Item ; ledger:number {number} ; dcterms:title \"t\" ; \
                 ledger:status ledger:closed ; ledger:closedReason ledger:done ; \
                 ledger:closedAt {at} .\n\
                 <{item}:claim> a ledger:ClaimEvent ; ledger:onItem <{item}> ; \
                 ledger:claimedBy \"busy\" ; \
                 dcterms:created \"2026-01-01T00:00:00Z\"^^xsd:dateTime .\n\
                 <{item}:back> ledger:onItem <{item}> ; ledger:handedBackBy \"busy\" ; \
                 dcterms:created {at} .\n\
                 <{item}:release> a ledger:ReleaseEvent ; ledger:onItem <{item}> ; \
                 ledger:claimedBy \"busy\" ; ledger:releaseReason \"released\" ; \
                 dcterms:created {at} .\n"
            ));
        }
        server.update("default", &format!("INSERT {{ {triples} }} WHERE {{}}"));
    }
    let numbers = server.numbers("default", "busy");
    assert_eq!(numbers[1], HANDBACKS.to_string(), "{numbers:?}");
    assert!(numbers.iter().all(|n| n != "unavailable"), "{numbers:?}");
    assert_eq!(
        numbers[5], "0",
        "each was handed back before its release: {numbers:?}"
    );
}

/// Every invocation a kernel records while this is installed.
#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// The store queries of one load of a Dashboard `page`, and the minute it began and ended in.
fn one_load(server: &Server, page: &str, reader: &Capability) -> (usize, u64, u64) {
    let minute = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 60
    };
    let began = minute();
    let trace = Arc::new(Collect::default());
    server.hub.set_tracer(trace.clone());
    server
        .ask(page, &[], reader)
        .expect("the Dashboard renders");
    server.hub.clear_tracer();
    let events = std::mem::take(&mut *trace.0.lock().unwrap());
    let queries = events
        .iter()
        .filter(|e| e.target.starts_with("urn:iki:store:graph-select") && !e.cache_hit)
        .count();
    (queries, began, minute())
}

/// What one load costs with every agent and ledger present: the numbers the pull request quotes.
#[test]
fn a_load_between_writes_asks_the_store_nothing() {
    const NAMES: [&str; 2] = ["default", "alpha"];
    const AGENTS: usize = 3;
    let server = Server::readers(&NAMES, &NAMES);
    for name in NAMES {
        for n in 0..AGENTS {
            server.file(name, "sample item");
            server.claim(name, &(n + 1).to_string(), &format!("agent-{n}"));
        }
    }
    let names: Vec<String> = NAMES.iter().map(|s| s.to_string()).collect();
    let reader = Capability::scoped(grants_for_all(&names, Authority::Read).unwrap());

    // A write to `ledger`, then a load and the load after it. A window turns over with the minute,
    // so a pair across one is taken again, after a write of its own: the retry's first load has to
    // be cold as well.
    let after_a_write = |page: &str, ledger: &str| loop {
        server.file(ledger, "sample item");
        let (cold, began, _) = one_load(&server, page, &reader);
        let (warm, _, ended) = one_load(&server, page, &reader);
        if began == ended {
            return (cold, warm);
        }
    };
    // ★ Each tab asks its own questions and none of the other's (ledger #2971). Per ledger, the
    // Ledgers tab: the chart's counts. The Agents tab: its agents, seven questions per agent, and
    // the seven again across all of them for the totals row (ledger #3178).
    let tabs = [
        ("urn:iki:gonk:page:dashboard:ledgers", NAMES.len()),
        (
            "urn:iki:gonk:page:dashboard:agents",
            NAMES.len() * (1 + 7 * (AGENTS + 1)),
        ),
    ];
    // ⚠ A write to one ledger: every ledger's answers are asked again, and then not until the next.
    for (page, every) in tabs {
        for ledger in NAMES {
            assert_eq!(
                after_a_write(page, ledger),
                (every, 0),
                "{page}, after a write to {ledger}"
            );
        }
    }
}

/// The totals row of `ledger` in `page`: each column's visible number and its link.
fn totals_in(page: &str, ledger: &str) -> Vec<(String, String)> {
    let panel =
        agents_panel(page, ledger).unwrap_or_else(|| panic!("no panel for {ledger}: {page}"));
    let foot = panel
        .split_once("<tfoot>")
        .and_then(|(_, rest)| rest.split_once("</tfoot>"))
        .unwrap_or_else(|| panic!("no totals row: {panel}"))
        .0;
    assert!(
        foot.starts_with("<tr><th scope=\"row\">All agents</th>"),
        "{foot}"
    );
    cells_in(
        &format!("id=\"agents-{ledger}-title\"{foot}</section>"),
        ledger,
        "All agents",
    )
}

/// ★ The reproduction of ledger #3178: two agents worked the same item, and the totals row counts it
/// once. A sum of the column would say 5 worked on; there are 3 items.
#[test]
fn a_total_is_the_distinct_count_never_the_columns_sum() {
    let server = Server::readers(&["default"], &["default"]);
    for n in 1..=3 {
        server.file("default", &format!("sample item {n}"));
    }
    let (ann, bob) = ("ann", "bob");
    // 1: both worked it and both handed it back; bob's was last, and it closed done.
    server.claim("default", "1", ann);
    server.hand_back("default", "1");
    server.release("default", "1");
    server.claim("default", "1", bob);
    server.hand_back("default", "1");
    server.close("default", "1", "done");
    // 2: ann holds it.
    server.claim("default", "2", ann);
    // 3: both let it go with no hand-back.
    server.claim("default", "3", ann);
    server.release("default", "3");
    server.claim("default", "3", bob);
    server.release("default", "3");

    let page = server.ok("/dashboard/agents");
    let numbers = |holder: &str| -> Vec<String> {
        cells_in(&page, "default", holder)
            .into_iter()
            .map(|c| c.0)
            .collect()
    };
    assert_eq!(numbers(ann)[..6], ["1", "3", "0", "0", "1", "1"]);
    assert_eq!(numbers(bob)[..6], ["0", "2", "1", "1", "1", "1"]);
    let totals = totals_in(&page, "default");
    let shown: Vec<&str> = totals.iter().map(|c| c.0.as_str()).collect();
    // By hand: in flight {2}; worked on {1, 2, 3}, not 3 + 2; completed {1}, not ann's 0 + bob's 1
    // read twice; two hand-backs, each one agent's; released {3}, not 1 + 1.
    assert_eq!(shown[..6], ["1", "3", "1", "1", "2", "1"], "{totals:?}");
    // The median of both hand-backs, never "none".
    assert!(!["none", "unavailable"].contains(&shown[6]), "{totals:?}");

    // Each total is its own link's answer, through the SPARQL face, like every other cell.
    for (column, (shown, href)) in COLUMNS.iter().zip(&totals) {
        let answer = server.through_the_face(href);
        if *column == "median-claim-to-handback" {
            assert_eq!(rows_of(&answer), 2, "every hand-back: {href}");
        } else {
            assert_eq!(rows_of(&answer).to_string(), *shown, "{column}: {href}");
        }
    }

    // The set's own resource answers the same, and the stored text is the per-holder one, asked
    // with `?holder` free.
    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    let worked: Value = serde_json::from_slice(
        &server
            .ask(
                "urn:iki:gonk:agent:question:name:worked-on",
                &[("ledger", "default")],
                &reader,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rows_of(&worked), 3);
}

/// A viewer who reads one ledger sees that ledger's totals, and the set's resource refuses the other.
#[test]
fn a_viewer_sees_totals_only_over_what_they_may_read() {
    let server = Server::readers(&["default", "secret"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "shared-agent");
    for n in 1..=3 {
        server.file("secret", "sample item");
        server.claim("secret", &n.to_string(), "hidden-agent");
    }
    let page = server.ok("/dashboard/agents");
    assert!(agents_panel(&page, "secret").is_none(), "{page}");
    let totals = totals_in(&page, "default");
    assert_eq!(totals[0].0, "1", "{totals:?}");
    assert_eq!(totals[1].0, "1", "{totals:?}");
    assert!(totals
        .iter()
        .all(|(_, href)| href.contains("ledger=default&")));

    let reader =
        Capability::scoped(grants_for_all(&["default".to_string()], Authority::Read).unwrap());
    for name in COLUMNS {
        let iri = format!("urn:iki:gonk:agent:question:name:{name}");
        let refused = server
            .ask(&iri, &[("ledger", "secret")], &reader)
            .expect_err(&iri);
        assert!(
            matches!(refused, ikigai_core::Error::Denied(_)),
            "{iri}: {refused}"
        );
    }
}

/// The poll's fragment carries the totals row, so a refresh moves it with the rows.
#[test]
fn the_polled_table_updates_the_totals() {
    let server = Server::readers(&["default"], &["default"]);
    server.file("default", "sample item");
    server.claim("default", "1", "ann");
    let fragment = "/dashboard/agents/default";
    let before = server.ok(fragment);
    assert_eq!(totals_in(&before, "default")[0].0, "1");
    server.file("default", "another");
    server.claim("default", "2", "bob");
    let after = server.ok(fragment);
    let totals = totals_in(&after, "default");
    assert_eq!(totals[0].0, "2", "{totals:?}");
    assert_eq!(totals[1].0, "2", "{totals:?}");
}
