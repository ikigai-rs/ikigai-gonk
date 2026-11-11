//! Ledger [#3034](http://localhost:1062/l/default/item/3034): holder aliases, so a renamed worker
//! loop keeps one row on the Dashboard's Agents tab.
//!
//! These read the pages over real HTTP. A history older than the test is written with a raw update,
//! as `tests/agents_2820.rs` does, and the rule's date is long past, so no clock can land on the
//! wrong side of it. The names are neutral samples.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::holders::HolderAliases;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;
use serde_json::Value;

const COLUMNS: [&str; 7] = [
    "in-flight",
    "worked-on",
    "completed-24h",
    "completed-7d",
    "handed-back-24h",
    "released",
    "median-claim-to-handback",
];
const MEDIAN: usize = 6;

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Server {
    /// A server reading `ledgers` to anyone, with these `gonk.holder.alias` values.
    fn with_rules(ledgers: &[&str], rules: &[&str]) -> Server {
        let names: Vec<String> = ledgers.iter().map(|s| s.to_string()).collect();
        let rules: Vec<String> = rules.iter().map(|s| s.to_string()).collect();
        let anonymous = grants_for_all(&names, Authority::Read).unwrap();
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
            ledgers: names,
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: HolderAliases::parse(&rules).unwrap(),
            page_assets: Default::default(),
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

    fn file(&self, ledger: &str, count: u32) {
        for n in 1..=count {
            self.act(
                ledger,
                "append",
                &[("content", &format!("sample item {n}"))],
            );
        }
    }

    fn claim(&self, ledger: &str, item: &str, holder: &str) {
        std::thread::sleep(std::time::Duration::from_millis(2));
        self.act(ledger, "claim", &[("item", item), ("content", holder)]);
    }

    fn hand_back(&self, ledger: &str, item: &str) {
        std::thread::sleep(std::time::Duration::from_millis(2));
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
        std::thread::sleep(std::time::Duration::from_millis(2));
        let prefix = Ledger::parse(ledger).unwrap().prefix();
        self.issue(Verb::Delete, &format!("{prefix}claim"), &[("item", item)]);
    }

    fn close(&self, ledger: &str, item: &str) {
        self.act(ledger, "close", &[("item", item), ("reason", "done")]);
    }

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

    /// A past claim of item `number`, signed `signer` at `at`.
    fn past_claim(&self, ledger: &str, number: u32, signer: &str, at: &str) {
        self.update(
            ledger,
            &format!(
                "INSERT {{ <urn:sample:{number}:{at}:claim> a ledger:ClaimEvent ; \
                 ledger:onItem ?item ; ledger:claimedBy \"{signer}\" ; \
                 dcterms:created \"{at}\"^^xsd:dateTime }} \
                 WHERE {{ ?item ledger:number {number} }}"
            ),
        );
    }

    /// A past hand-back of item `number`, signed `signer` at `at`.
    fn past_hand_back(&self, ledger: &str, number: u32, signer: &str, at: &str) {
        self.update(
            ledger,
            &format!(
                "INSERT {{ <urn:sample:{number}:{at}:back> ledger:onItem ?item ; \
                 ledger:handedBackBy \"{signer}\" ; dcterms:created \"{at}\"^^xsd:dateTime }} \
                 WHERE {{ ?item ledger:number {number} }}"
            ),
        );
    }

    /// A past release of item `number`, signed `signer` at `at`.
    fn past_release(&self, ledger: &str, number: u32, signer: &str, at: &str) {
        self.update(
            ledger,
            &format!(
                "INSERT {{ <urn:sample:{number}:{at}:release> a ledger:ReleaseEvent ; \
                 ledger:onItem ?item ; ledger:claimedBy \"{signer}\" ; \
                 ledger:releaseReason \"released\" ; dcterms:created \"{at}\"^^xsd:dateTime }} \
                 WHERE {{ ?item ledger:number {number} }}"
            ),
        );
    }

    /// Say item `number`'s live claim was taken at `at`.
    fn claim_dates_from(&self, ledger: &str, number: u32, at: &str) {
        self.update(
            ledger,
            &format!(
                "DELETE {{ ?item ledger:claimedAt ?was }} \
                 INSERT {{ ?item ledger:claimedAt \"{at}\"^^xsd:dateTime }} \
                 WHERE {{ ?item ledger:number {number} ; ledger:claimedAt ?was }}"
            ),
        );
    }

    fn through_the_face(&self, href: &str) -> Value {
        let href = href.strip_suffix("#results").unwrap_or(href);
        let (status, body) = self.page(&format!("{href}&as=application%2Fsparql-results%2Bjson"));
        assert_eq!(status, 200, "{href}: {body}");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    /// ★ The acceptance: each number the row shows is what its own link's question answers.
    fn assert_links_agree(&self, page: &str, ledger: &str, holder: &str) {
        for (column, (shown, href)) in COLUMNS.iter().zip(cells_in(page, ledger, holder)) {
            let answer = self.through_the_face(&href);
            let rows = answer["results"]["bindings"].as_array().unwrap();
            if *column == COLUMNS[MEDIAN] {
                assert_eq!(
                    shown == "none",
                    rows.is_empty(),
                    "{holder}: {column}: {href}"
                );
            } else {
                assert_eq!(rows.len().to_string(), shown, "{holder}: {column}: {href}");
            }
        }
    }
}

fn agents_panel<'a>(page: &'a str, ledger: &str) -> &'a str {
    let start = page
        .find(&format!("id=\"agents-{ledger}-title\""))
        .unwrap_or_else(|| panic!("no panel for {ledger}: {page}"));
    let rest = &page[start..];
    &rest[..rest.find("</section>").unwrap()]
}

fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
}

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

/// The agents a panel lists, in order.
fn row_names(panel: &str) -> Vec<String> {
    panel
        .split("<tr>")
        .filter(|row| row.contains("<td>"))
        .map(|row| visible(row.split_once("</th>").unwrap().0))
        .collect()
}

fn cells_in(page: &str, ledger: &str, holder: &str) -> Vec<(String, String)> {
    let panel = agents_panel(page, ledger);
    let row = panel
        .split("<tr>")
        .find(|row| {
            row.contains("<td>")
                && row
                    .split_once("</th>")
                    .is_some_and(|(th, _)| visible(th) == holder)
        })
        .unwrap_or_else(|| panic!("no row for {holder}: {panel}"));
    row.split("<td>")
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
        .collect()
}

fn numbers(page: &str, ledger: &str, holder: &str) -> Vec<String> {
    cells_in(page, ledger, holder)
        .into_iter()
        .map(|c| c.0)
        .collect()
}

/// A rename: the old name's events and the new name's are one row, and the old has none.
#[test]
fn two_holders_fold_into_one_row() {
    let server = Server::with_rules(&["default"], &["old-name -> new-name"]);
    server.file("default", 4);
    // 1 and 2: the old name, handed back; 3: the new name, handed back and held; 4: another loop.
    server.claim("default", "1", "old-name");
    server.hand_back("default", "1");
    server.close("default", "1");
    server.claim("default", "2", "old-name");
    server.hand_back("default", "2");
    server.release("default", "2");
    server.claim("default", "3", "new-name");
    server.hand_back("default", "3");
    server.claim("default", "4", "other");

    let page = server.ok("/dashboard/agents");
    let panel = agents_panel(&page, "default");
    assert_eq!(row_names(panel), ["new-name", "other"], "{panel}");
    let shown = numbers(&page, "default", "new-name");
    assert_eq!(shown[0], "1", "in flight: {shown:?}");
    assert_eq!(shown[1], "3", "worked on: {shown:?}");
    assert_eq!(shown[2], "1", "completed: {shown:?}");
    assert_eq!(shown[4], "3", "handed back: {shown:?}");
    assert_eq!(shown[5], "0", "released: {shown:?}");
    server.assert_links_agree(&page, "default", "new-name");
    server.assert_links_agree(&page, "default", "other");

    // Nothing was rewritten: the ledger still says who signed what.
    let raw = server.through_the_face(&format!(
        "/sparql?ledger=default&query={}",
        ikigai_fn::html::url_segment(
            "SELECT ?e WHERE { ?e a <https://ikigai-rs.dev/ns/ledger#ClaimEvent> ; \
             <https://ikigai-rs.dev/ns/ledger#claimedBy> \"old-name\" }"
        )
    ));
    assert_eq!(raw["results"]["bindings"].as_array().unwrap().len(), 2);
}

/// A claim signed with the old name and a hand-back signed with the new are one loop's: the median
/// joins across the rename rather than losing the pair.
#[test]
fn the_median_joins_a_claim_and_a_hand_back_across_a_rename() {
    let server = Server::with_rules(&["default"], &["old-name -> new-name"]);
    server.file("default", 1);
    server.past_claim("default", 1, "old-name", "2019-03-01T00:00:00Z");
    server.past_hand_back("default", 1, "new-name", "2019-03-01T02:00:00Z");
    let page = server.ok("/dashboard/agents");
    assert_eq!(numbers(&page, "default", "new-name")[MEDIAN], "2 h");
    server.assert_links_agree(&page, "default", "new-name");
}

/// ★ The reason an alias is not a rename: one old name, two rows, cut at the date.
#[test]
fn a_time_bound_splits_one_old_name_across_two_rows() {
    let rules = [
        "machine -> machine-one before 2020-06-15",
        "machine -> machine-two",
    ];
    let server = Server::with_rules(&["default"], &rules);
    server.file("default", 8);
    // 1: well before, completed since; its last hand-back is what is credited.
    server.past_claim("default", 1, "machine", "2020-06-01T00:00:00Z");
    server.past_hand_back("default", 1, "machine", "2020-06-01T01:00:00Z");
    server.close("default", "1");
    // 2: the last second before the cut, let go with no hand-back.
    server.past_claim("default", 2, "machine", "2020-06-14T23:59:59Z");
    server.past_release("default", 2, "machine", "2020-06-14T23:59:59Z");
    // 3: the first second of the cut, let go with no hand-back: the later name's.
    server.past_claim("default", 3, "machine", "2020-06-15T00:00:00Z");
    server.past_release("default", 3, "machine", "2020-06-15T00:00:00Z");
    // 4: claimed now under the old name, and handed back: the later name's.
    server.claim("default", "4", "machine");
    server.hand_back("default", "4");
    server.close("default", "4");
    // 5: claimed now under the old name, and held.
    server.claim("default", "5", "machine");
    // 6: claimed now under the new name, and held.
    server.claim("default", "6", "machine-two");
    // 7: claimed long ago under the old name and still held: the earlier name's.
    server.claim("default", "7", "machine");
    server.claim_dates_from("default", 7, "2020-06-02T00:00:00Z");
    server.update(
        "default",
        "DELETE { ?e dcterms:created ?at } \
         INSERT { ?e dcterms:created \"2020-06-02T00:00:00Z\"^^xsd:dateTime } \
         WHERE { ?e a ledger:ClaimEvent ; ledger:onItem ?item ; dcterms:created ?at . \
                 ?item ledger:number 7 }",
    );
    // 8: straddling the cut. Claimed before it and handed back after: each event in its own row.
    server.past_claim("default", 8, "machine", "2020-06-14T00:00:00Z");
    server.past_hand_back("default", 8, "machine", "2020-06-16T00:00:00Z");

    let page = server.ok("/dashboard/agents");
    let panel = agents_panel(&page, "default");
    assert_eq!(row_names(panel), ["machine-one", "machine-two"], "{panel}");

    let one = numbers(&page, "default", "machine-one");
    assert_eq!(one[0], "1", "in flight, 7: {one:?}");
    assert_eq!(one[1], "4", "worked on, 1 2 7 8: {one:?}");
    assert_eq!(one[2], "1", "completed, 1: {one:?}");
    assert_eq!(one[3], "1", "completed, 1: {one:?}");
    assert_eq!(one[4], "0", "handed back in 24 hours: {one:?}");
    assert_eq!(one[5], "1", "released, 2: {one:?}");
    assert_eq!(one[MEDIAN], "1 h", "from item 1: {one:?}");

    let two = numbers(&page, "default", "machine-two");
    assert_eq!(two[0], "2", "in flight, 5 and 6: {two:?}");
    assert_eq!(two[1], "4", "worked on, 3 4 5 6: {two:?}");
    assert_eq!(two[2], "1", "completed, 4: {two:?}");
    assert_eq!(two[3], "1", "completed, 4: {two:?}");
    assert_eq!(two[4], "1", "handed back in 24 hours, 4: {two:?}");
    assert_eq!(two[5], "1", "released, 3: {two:?}");

    server.assert_links_agree(&page, "default", "machine-one");
    server.assert_links_agree(&page, "default", "machine-two");
}

/// The tab says which aliases apply, table by table, and only of rows that table shows.
#[test]
fn a_table_says_which_aliases_fill_its_rows() {
    let rules = [
        "machine -> machine-one before 2020-06-15",
        "machine -> machine-two",
        "elsewhere -> elsewhere-too",
    ];
    let server = Server::with_rules(&["default", "second"], &rules);
    server.file("default", 2);
    server.past_claim("default", 1, "machine", "2020-06-01T00:00:00Z");
    server.claim("default", "2", "machine");
    server.file("second", 2);
    server.claim("second", "1", "elsewhere");
    server.claim("second", "2", "unaliased");

    let page = server.ok("/dashboard/agents");
    let default = visible(agents_panel(&page, "default"));
    assert!(
        default.contains(
            "machine before 2020-06-15 counts as machine-one; \
             machine from 2020-06-15 counts as machine-two."
        ),
        "{default}"
    );
    assert!(!default.contains("elsewhere"), "{default}");
    let second = visible(agents_panel(&page, "second"));
    assert!(
        second.contains("elsewhere counts as elsewhere-too."),
        "{second}"
    );
    assert!(!second.contains("machine"), "{second}");
    server.assert_links_agree(&page, "second", "elsewhere-too");
    server.assert_links_agree(&page, "second", "unaliased");
}

/// With no rule, no table says anything of aliases.
#[test]
fn no_rule_is_no_sentence_and_the_rows_as_signed() {
    let server = Server::with_rules(&["default"], &[]);
    server.file("default", 2);
    server.claim("default", "1", "old-name");
    server.claim("default", "2", "new-name");
    let page = server.ok("/dashboard/agents");
    let panel = agents_panel(&page, "default");
    assert_eq!(row_names(panel), ["new-name", "old-name"], "{panel}");
    assert!(!panel.contains("aliases"), "{panel}");
}

/// ⚠ A folded row may not cost the store's budget where a plain one does not: the same shape as
/// `every_question_answers_for_a_busy_agent`, with its history under an old name split at a date.
#[test]
fn every_question_answers_for_a_busy_folded_agent() {
    const HANDBACKS: usize = 1500;
    let rules = ["busy -> busy-one before 2026-01-02", "busy -> busy-two"];
    let server = Server::with_rules(&["default"], &rules);
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
                 dcterms:created \"2026-01-02T00:00:00Z\"^^xsd:dateTime .\n\
                 <{item}:back> ledger:onItem <{item}> ; ledger:handedBackBy \"busy\" ; \
                 dcterms:created {at} .\n\
                 <{item}:release> a ledger:ReleaseEvent ; ledger:onItem <{item}> ; \
                 ledger:claimedBy \"busy\" ; ledger:releaseReason \"released\" ; \
                 dcterms:created {at} .\n"
            ));
        }
        server.update("default", &format!("INSERT {{ {triples} }} WHERE {{}}"));
    }
    let page = server.ok("/dashboard/agents");
    let shown = numbers(&page, "default", "busy-two");
    assert_eq!(shown[1], HANDBACKS.to_string(), "{shown:?}");
    assert!(shown.iter().all(|n| n != "unavailable"), "{shown:?}");
    assert_eq!(row_names(agents_panel(&page, "default")), ["busy-two"]);
}
