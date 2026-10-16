//! Ledger [#915](http://localhost:1060/l/default/item/915): one request carrying a deeply
//! nested SPARQL query ABORTED gonk.
//!
//! `spargebra` reads nesting by recursion, and a stack overflow is not a panic — the process
//! aborts (`thread 'tokio-rt-worker' has overflowed its stack`). On `b16f79c` a single GET of
//! `/sparql/results?query=SELECT*{FILTER(((…3000…1…)))}` killed the server.
//!
//! ★ **Every test here runs a REAL gonk in a CHILD process** — the binary this crate builds,
//! over a scratch config home, data home, store and socket (`tests/scratch_gonk/mod.rs`, which
//! says how it binds its ports and when it counts a door as ready) — and asserts it is still
//! serving after the request. A reproduction that aborts must not take the
//! test binary with it, and only the binary has the stacks `main` sets (`ikigai_gonk::stack`),
//! which is half of what is under test. Nothing here touches a live gonk.
//!
//! One test per door, so each one reproduces on its own: on `b16f79c` every test in this file
//! fails at its first bomb, with the child's `has overflowed its stack` in the message.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

/// The scratch gonk's `config.toml`.
///
/// - The review space, so its `match` door is bound (no grant, not armed: nothing runs).
/// - ★ The anonymous SPARQL budget at its ceiling (ledger
///   [#1028](http://localhost:1060/l/default/item/1028)). A debug build answers a chain of
///   1,000 UNIONs (now [`longest_admitted_chain`]'s 1,023) in about 0.2 s, and on a saturated
///   machine past the 1,000 ms default: 17 of 480 runs of the HTTP chain test, at 24 concurrent
///   copies beside 24 copies of another test binary, were refused `503 … ran past its time
///   budget of 1000 ms`. What is under test here is the stack, not the budget, which
///   `tests/door_budget_964.rs` owns.
const CONFIG: &str = "gonk.review.space = \"reviews\"\n\
                      gonk.http.anonymous_sparql_budget_ms = 5000\n";

/// The graph the anonymous HTTP caller and the enrolled QUIC client may read.
const GRAPH: &str = "urn:iki:ledger:graph:default";

/// The query from #915, with no whitespace so it also fits `/k`'s grammar.
fn nested(depth: usize) -> String {
    format!(
        "SELECT*{{FILTER({}1{})}}",
        "(".repeat(depth),
        ")".repeat(depth)
    )
}

/// The bomb: 3000 parentheses, three times what aborts a 2 MiB release stack.
fn bomb() -> String {
    nested(3000)
}

/// A query AT the bound: `{` and `FILTER(` are two levels, the rest parentheses.
fn at_the_bound() -> String {
    nested(ikigai_gonk::sparql::MAX_NESTING - 2)
}

/// A chain of `n` UNIONs (`{}UNION{}UNION…`): no nesting past 2, a left-deep tree the store
/// walks by recursion, and — unlike a path chain, whose cost the store grows CUBICALLY (2,000
/// steps took 168 s in a release build) — answered in a fraction of a second.
fn chain(n: usize) -> String {
    format!("SELECT*{{{{}}{}}}", "UNION{}".repeat(n))
}

/// The algebra nodes the store counts in `query`, as its `MAX_ALGEBRA_NODES` check counts them
/// ([`ikigai_store::budget::measure_query`]). Only for SHORT queries: the walk recurses, and this
/// runs on a test thread.
fn algebra_nodes(query: &str) -> usize {
    let parsed = spargebra::SparqlParser::new()
        .parse_query(query)
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    ikigai_store::budget::measure_query(&parsed).nodes()
}

/// The longest [`chain`] the store's algebra bound admits, from the store's own count: a chain
/// costs a fixed base plus a fixed step per `UNION` (measured on short chains, where the walk is
/// cheap, and checked to be linear), so the longest is `(MAX_ALGEBRA_NODES - base) / step`.
/// 1,023 with `ikigai-store` 0.2.23 (`n + 1` nodes). Sized from the bound, not written down, so
/// the next change to the bound or the count moves it rather than flipping a test (ledger #1736).
fn longest_admitted_chain() -> usize {
    let base = algebra_nodes(&chain(0));
    let step = algebra_nodes(&chain(1)) - base;
    assert!(
        step > 0,
        "a UNION costs nothing; the bound cannot size this chain"
    );
    for n in [2, 10, 50] {
        assert_eq!(
            algebra_nodes(&chain(n)),
            base + step * n,
            "the count is linear"
        );
    }
    (ikigai_store::budget::MAX_ALGEBRA_NODES - base) / step
}

/// The chain these tests sent before ledger #1736: past the store's algebra bound, and long
/// enough that the store's whole read of it aborted a 2 MiB thread in either build (measured
/// with `examples/sparql-depth.rs`, store, `union`: release past 1,127, debug past 163). 3,500 is
/// 28 KB, under the length bound. Now refused by name after it is parsed and measured.
const STACK_CHAIN: usize = 3_500;

/// The checks and requests these tests make of a [`Gonk`]: the harness itself, readiness and
/// all, is `tests/scratch_gonk/mod.rs`.
impl Gonk {
    /// ⚠ `try_wait` only; a dead child is then reaped with `wait`, so its signal is reported.
    fn assert_alive(&mut self, after: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("gonk DIED {after} ({status}):\n{}", self.log());
        }
    }

    /// The server is still running AND still answers, on HTTP and the socket.
    fn assert_serving(&mut self, after: &str) {
        let (status, body) = self.get("/static/gonk.css");
        if status != 200 {
            // A refused connection is the death; let the reaped status say how it died.
            std::thread::sleep(Duration::from_millis(200));
            self.assert_alive(after);
            panic!("gonk stopped serving {after}: {status} {body}");
        }
        let answer = self.over_socket(select(&nested(1)));
        assert!(answer.is_ok(), "the socket after {after}: {answer:?}");
        self.assert_alive(after);
    }

    /// A GET the way any page can send one: by navigating the window here (ledger #880).
    /// `(0, …)` when the connection was refused or closed with no status line.
    fn get(&self, path: &str) -> (u16, String) {
        self.get_as(path, "*/*")
    }

    /// [`Gonk::get`] with an `Accept` — `text/html` for the editor's page and fragment.
    fn get_as(&self, path: &str, accept: &str) -> (u16, String) {
        let Ok(mut stream) = TcpStream::connect(self.http) else {
            return (0, "connection refused".to_string());
        };
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: {accept}\r\n\
             Sec-Fetch-Site: same-site\r\nSec-Fetch-Mode: navigate\r\nSec-Fetch-Dest: document\r\n\
             Referer: http://localhost:8090/innocent.html\r\nConnection: close\r\n\r\n",
            self.http.port()
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return (0, "write failed".to_string());
        }
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_quic(&self, request: Request) -> Result<Representation, Error> {
        // The door answered a handshake before `with_quic` returned; this is one more.
        let client = self
            .dial_quic()
            .unwrap_or_else(|e| panic!("QUIC connect: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// `urn:iki:store:graph-select` over the default ledger's graph.
fn select(query: &str) -> Request {
    request(
        Verb::Source,
        "urn:iki:store:graph-select",
        &[("query", query), ("graph", GRAPH)],
    )
}

/// `urn:sparql:select`, the default dataset.
fn sparql_select(query: &str) -> Request {
    request(Verb::Source, "urn:sparql:select", &[("query", query)])
}

fn refused(answer: Result<Representation, Error>, what: &str) {
    match answer {
        Err(Error::InvalidArgument { name, detail }) => {
            assert!(
                name == "query" || name == "content",
                "{what}: refused on `{name}`"
            );
            assert!(
                detail.contains("deep") || detail.contains("bytes"),
                "{what}: {detail}"
            );
        }
        other => panic!("{what} must be refused as InvalidArgument, got {other:?}"),
    }
}

/// THE reproduction from #915, at every HTTP path that hands a caller's text to a SPARQL
/// parse: the editor's fragment and page, the protocol face, the store's own door, `/k`.
#[test]
fn the_http_door_refuses_a_nested_query_and_keeps_serving() {
    let mut gonk = Gonk::start(CONFIG);
    // One item, so the default ledger's graph is readable and `urn:sparql:select`'s default
    // dataset is not empty: with nothing written, that face refuses before any parse, and on
    // `b16f79c` `/sparql/select` aborted only once a graph existed.
    gonk.over_socket(request(
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "one item")],
    ))
    .expect("append over the socket");
    let bomb = encoded(&bomb());
    let html = "text/html";
    for (path, accept) in [
        (format!("/sparql/results?ledger=default&query={bomb}"), html),
        (format!("/sparql?ledger=default&query={bomb}"), html),
        (
            format!("/sparql?as=application%2Fsparql-results%2Bjson&query={bomb}"),
            "*/*",
        ),
        (
            format!("/iki/store/graph-select?graph={GRAPH}&query={bomb}"),
            "*/*",
        ),
        (format!("/sparql/select?query={bomb}"), "*/*"),
        (
            format!(
                "/k?c={}",
                encoded(&format!(
                    "source urn:iki:store:graph-select graph={GRAPH} query={}",
                    self::bomb()
                ))
            ),
            "*/*",
        ),
    ] {
        let shown: String = path.chars().take(60).collect();
        let (status, body) = gonk.get_as(&path, accept);
        gonk.assert_serving(&format!("GET {shown}…"));
        assert_eq!(status, 400, "GET {shown}…: {body}");
        assert!(body.contains("deep"), "GET {shown}…: {body}");
    }
}

#[test]
fn the_socket_door_refuses_a_nested_query_or_update_and_keeps_serving() {
    let mut gonk = Gonk::start(CONFIG);
    let answer = gonk.over_socket(select(&bomb()));
    gonk.assert_serving("graph-select over the socket");
    refused(answer, "graph-select");
    let answer = gonk.over_socket(sparql_select(&bomb()));
    gonk.assert_serving("urn:sparql:select over the socket");
    refused(answer, "urn:sparql:select");
    // An update parses with the same recursion; the owner at root may send one.
    let update = format!(
        "INSERT {{ GRAPH <urn:g> {{ <urn:a> <urn:b> <urn:c> }} }} WHERE {{ FILTER({}1{}) }}",
        "(".repeat(3000),
        ")".repeat(3000)
    );
    let answer = gonk.over_socket(request(
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("content", &update), ("graph", "urn:g")],
    ));
    gonk.assert_serving("graph-update over the socket");
    refused(answer, "graph-update");
}

#[test]
fn the_quic_door_refuses_a_nested_query_and_keeps_serving() {
    let mut gonk = Gonk::with_quic(CONFIG);
    let answer = gonk.over_quic(select(&bomb()));
    gonk.assert_serving("graph-select over QUIC");
    refused(answer, "graph-select");
    let answer = gonk.over_quic(sparql_select(&bomb()));
    gonk.assert_serving("urn:sparql:select over QUIC");
    refused(answer, "urn:sparql:select");
}

/// A query AT the bound is answered — through every door, by a server whose stacks the
/// binary set.
#[test]
fn a_query_at_the_bound_is_answered_at_every_door() {
    let mut gonk = Gonk::with_quic(CONFIG);
    let query = at_the_bound();
    assert_eq!(
        ikigai_gonk::sparql::nesting_depth(query.as_bytes()),
        ikigai_gonk::sparql::MAX_NESTING
    );
    gonk.over_socket(select(&query))
        .expect("at the bound, over the socket");
    gonk.over_quic(select(&query))
        .expect("at the bound, over QUIC");
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&query)
    ));
    assert_eq!(status, 200, "at the bound, over HTTP: {body}");
    gonk.assert_serving("a query at the bound");
}

/// One request through `door` of a fresh gonk: a `graph-select` of `query` over the default
/// ledger's graph. The HTTP door's non-200 is an `Error::Endpoint` carrying status and body.
fn through(gonk: &Gonk, door: &str, query: &str) -> Result<(), Error> {
    match door {
        "socket" => gonk.over_socket(select(query)).map(|_| ()),
        "quic" => gonk.over_quic(select(query)).map(|_| ()),
        _ => {
            let (status, body) = gonk.get(&format!(
                "/iki/store/graph-select?graph={GRAPH}&query={}",
                encoded(query)
            ));
            if status == 200 {
                Ok(())
            } else {
                Err(Error::Endpoint(format!("{status} {body}")))
            }
        }
    }
}

fn gonk_for(door: &str) -> Gonk {
    if door == "quic" {
        Gonk::with_quic(CONFIG)
    } else {
        Gonk::start(CONFIG)
    }
}

/// ★ The half of #915 no bracket scan sees: a chain with no nesting at all, which the store
/// walks by recursion, sent through one door to a fresh gonk. The chain is the LONGEST the
/// store's algebra bound admits ([`longest_admitted_chain`]), so it is ANSWERED in both builds.
///
/// ⚠ Through ledger #1736 this sent 3,500 UNIONs in release, past the algebra bound the store
/// gained in 0.2.8 (ledger #979), so a release run refused it and failed; CI builds debug only and
/// never saw it. And this used to say that, with `THREAD_STACK_BYTES` set back to 2 MiB, every
/// door aborted the child. Not since the store parses and evaluates on a thread it sizes itself
/// (`ikigai_store::limits::on_sparql_stack`): measured, with that mutation every test here passes
/// in both builds. What each door proves now is that the longest admitted chain is answered and
/// gonk keeps serving; gonk's own stack is
/// [`the_editor_parses_a_path_chain_on_a_large_enough_stack`]'s.
fn a_chain_is_answered_through(door: &str) {
    let mut gonk = gonk_for(door);
    let n = longest_admitted_chain();
    let query = chain(n);
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    assert_eq!(ikigai_gonk::sparql::nesting_depth(query.as_bytes()), 2);
    let answer = through(&gonk, door, &query);
    gonk.assert_serving(&format!("a chain of {n} over {door}"));
    answer.unwrap_or_else(|e| panic!("a chain of {n} over {door}: {e}"));
}

#[test]
fn a_chain_with_no_nesting_is_answered_over_the_socket() {
    a_chain_is_answered_through("socket");
}

#[test]
fn a_chain_with_no_nesting_is_answered_over_quic() {
    a_chain_is_answered_through("quic");
}

#[test]
fn a_chain_with_no_nesting_is_answered_over_http() {
    a_chain_is_answered_through("http");
}

/// A chain one `UNION` past the algebra bound, then [`STACK_CHAIN`], through one door of a fresh
/// gonk: each REFUSED by the store's algebra bound, by name, and gonk lives. The first pins the
/// bound where [`longest_admitted_chain`] says it is, in both builds (so a release run cannot
/// again pass one claim while failing another). The second is the chain release used to send:
/// it is parsed and measured on the store's sized thread before the bound refuses it.
fn a_chain_past_the_algebra_bound_is_refused_through(door: &str) {
    let mut gonk = gonk_for(door);
    for n in [longest_admitted_chain() + 1, STACK_CHAIN] {
        let query = chain(n);
        assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
        let answer = through(&gonk, door, &query);
        gonk.assert_serving(&format!("a chain of {n} over {door}"));
        let detail = match answer {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "query", "a chain of {n} over {door}");
                detail
            }
            Err(Error::Endpoint(detail)) if door == "http" => {
                assert!(
                    detail.starts_with("400 "),
                    "a chain of {n} over http: {detail}"
                );
                detail
            }
            other => panic!("a chain of {n} over {door} must be refused, got {other:?}"),
        };
        assert!(
            detail.contains("MAX_ALGEBRA_NODES"),
            "a chain of {n} over {door}: refused by the algebra bound, by name: {detail}"
        );
    }
}

#[test]
fn a_chain_past_the_algebra_bound_is_refused_over_the_socket() {
    a_chain_past_the_algebra_bound_is_refused_through("socket");
}

#[test]
fn a_chain_past_the_algebra_bound_is_refused_over_quic() {
    a_chain_past_the_algebra_bound_is_refused_through("quic");
}

#[test]
fn a_chain_past_the_algebra_bound_is_refused_over_http() {
    a_chain_past_the_algebra_bound_is_refused_through("http");
}

/// `!` is the one prefix operator spargebra reads by recursion, so a run of them nests with no
/// bracket at all: 6,000 abort a 2 MiB release parse (measured by the sibling `ikigai-store` arc
/// at 5,229 bytes, and here with `examples/sparql-depth.rs`, shape `not`). Counted as nesting.
#[test]
fn a_run_of_negations_is_refused_and_the_server_lives() {
    let mut gonk = Gonk::start(CONFIG);
    let query = format!("SELECT*{{FILTER({}1)}}", "!".repeat(6_000));
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&query)
    ));
    gonk.assert_serving("a run of negations over HTTP");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("deep"), "{body}");
}

/// `1*1*1*…`: a chain the PARSER reads by recursion (4,092 terms abort a 2 MiB release parse,
/// the sibling arc measured; 5,000 abort the whole read here) with no nesting.
///
/// ★ Re-sized for `ikigai-store` 0.2.8 (ledger #979), which changed two things under this test.
/// The store now refuses an expression of more than `MAX_ALGEBRA_NODES` (1024) operators before
/// planning it, so the 10,000-term release chain this used to send is REFUSED, cleanly, rather
/// than answered (the second request below). And the evaluation runs on the store's OWN
/// sized thread (16 MiB plus 512 bytes per byte of query), not on gonk's 64 MiB request
/// thread — which in a DEBUG build holds 405 terms of this chain and aborts the process at 406
/// (`examples/sparql-depth.rs`, shape `mul`, `store`; release holds every size up to the
/// algebra bound). So the answered chain is 1,000 terms in release, inside the bound, and 400
/// in debug, inside what the store's thread holds there. The debug abort is the store's, and
/// is reported to its arc rather than routed around here: a release build cannot reach it.
#[test]
fn a_multiplication_chain_is_answered_over_http() {
    let mut gonk = Gonk::start(CONFIG);
    let n = if cfg!(debug_assertions) { 400 } else { 1_000 };
    let query = format!("SELECT*{{FILTER(1{})}}", "*1".repeat(n));
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&query)
    ));
    gonk.assert_serving("a multiplication chain over HTTP");
    assert_eq!(status, 200, "{body}");

    // Past the store's algebra bound: refused before it is planned, and gonk lives.
    let past = format!("SELECT*{{FILTER(1{})}}", "*1".repeat(2_000));
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&past)
    ));
    gonk.assert_serving("a multiplication chain past the algebra bound over HTTP");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("1024"), "names the bound: {body}");
}

/// A property path `?s a/a/a/… ?o`: 6,000 steps (12 KB) abort a 2 MiB release PARSE, and the
/// store's own walk aborts far sooner. On the larger stacks the parse and the walk both finish.
///
/// ★ The marker is the ANSWER (ledger #1736). This used to send the path, sleep 3 s and check
/// gonk still served, because the store then planned and evaluated a path in time that grows
/// with the cube of its length (2,000 steps took 168 s in release) and the answer never came in
/// a test's time; nothing said the parse and walk had finished, so a slow machine could pass it
/// before they did. Since the store bounds the algebra before planning (ledger #964), a sequence
/// path counts one join operand a step, so this path is refused by `MAX_JOIN_OPERANDS` after it is parsed and
/// measured and before it is planned: the refusal arrives, it can only arrive once the parse and
/// the walk are done, and gonk is checked after it. ⚠ This door no longer tests gonk's stack:
/// the store parses on a thread it sizes itself, so with `THREAD_STACK_BYTES` set back to 2 MiB
/// this still passes. [`the_editor_parses_a_path_chain_on_a_large_enough_stack`] is the path
/// test that does.
#[test]
fn a_path_chain_does_not_abort_the_server() {
    let mut gonk = Gonk::start(CONFIG);
    let n = if cfg!(debug_assertions) { 2_000 } else { 6_000 };
    let query = format!("SELECT*{{?s a{} ?o}}", "/a".repeat(n));
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    // Over the SOCKET (a thread of its own per connection), on a thread of the test's own so a
    // hang is a failure with a message rather than a test that never ends.
    let socket = gonk.socket.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let answer = ikigai_ipc::connect(&socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))
            .and_then(|client| client.issue(select(&query)).map(|_| ()));
        let _ = tx.send(answer);
    });
    // A refusal before planning takes milliseconds; the bound is only a hang guard, and running
    // out of it FAILS the test.
    let answer = rx
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("no answer to a path of {n} steps in 120 s:\n{}", gonk.log()));
    gonk.assert_serving("a path chain over the socket, answered");
    match answer {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "query");
            assert!(detail.contains("MAX_JOIN_OPERANDS"), "{detail}");
        }
        other => panic!("a path of {n} steps must be refused before planning, got {other:?}"),
    }
}

/// ★ Where gonk's OWN stack is still load-bearing (ledger #1736). The store parses and
/// evaluates every query on a thread it sizes itself (`ikigai_store::limits::on_sparql_stack`),
/// so no store door above depends on [`ikigai_gonk::stack::THREAD_STACK_BYTES`]: with it set
/// back to 2 MiB, every other test in this file passes in both builds. But the SPARQL editor
/// parses the caller's text with spargebra ITSELF, on the HTTP runtime's thread, to learn its
/// form ([`ikigai_gonk::web::query_form`]) before the store sees it, and a path chain is the
/// densest shape that parser recurses on with no nesting: measured with
/// `examples/sparql-depth.rs` (`apath`, `parse`), a 2 MiB thread aborts past 763 steps in a
/// debug build and past 4,369 in release. So this path, inside the length bound, aborts a
/// 2 MiB gonk in either build — the test fails, with the child's `has overflowed its stack` —
/// and on the stacks `main` sets it is parsed, handed to the store, and refused there before planning
/// (`MAX_JOIN_OPERANDS`), which the page shows as the error it is. The refusal in the page is
/// the marker that the parse finished.
#[test]
fn the_editor_parses_a_path_chain_on_a_large_enough_stack() {
    let mut gonk = Gonk::start(CONFIG);
    let n = if cfg!(debug_assertions) { 2_000 } else { 6_000 };
    let query = format!("SELECT*{{?s a{} ?o}}", "/a".repeat(n));
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    assert_eq!(ikigai_gonk::sparql::nesting_depth(query.as_bytes()), 1);
    let (status, body) = gonk.get_as(
        &format!("/sparql/results?ledger=default&query={}", encoded(&query)),
        "text/html",
    );
    gonk.assert_serving(&format!("a path of {n} steps through the editor"));
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("MAX_JOIN_OPERANDS"),
        "parsed by the editor, then refused by the store before planning: {body}"
    );
}

/// One element past what the length bound admits is refused, never parsed.
#[test]
fn a_query_past_the_length_bound_is_refused() {
    let mut gonk = Gonk::start(CONFIG);
    let past = chain(ikigai_gonk::sparql::MAX_QUERY_BYTES / 7);
    assert!(past.len() > ikigai_gonk::sparql::MAX_QUERY_BYTES);
    let answer = gonk.over_socket(select(&past));
    gonk.assert_serving("a query past the length bound");
    refused(answer, "a query past the length bound");
}

/// The queries the gonk Book sends gonk, copied from `ikigai-tutorial/books/gonk/src` as of
/// 2026-10-09 (`ledger/browser.md`'s three `curl`s, `embedding/own-kernel.md`'s join).
const BOOK_QUERIES: [(&str, &str); 4] = [
    (
        "browser.md titles",
        "PREFIX dcterms: <http://purl.org/dc/terms/> SELECT ?title WHERE { ?item dcterms:title ?title }",
    ),
    (
        "browser.md census",
        "SELECT ?g (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g",
    ),
    ("browser.md one ledger", "SELECT ?s WHERE { ?s ?p ?o }"),
    (
        "own-kernel.md join",
        "
    PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
    PREFIX ik: <https://ikigai-rs.dev/ns#>
    PREFIX oa: <http://www.w3.org/ns/oa#>
    SELECT ?item ?line WHERE {
      ?i ledger:about ?file ; ledger:number ?item .
      ?a a oa:Annotation ; ik:annotates ?file ; oa:hasSelector ?s .
      ?s oa:exact ?line .
    }",
    ),
];

/// The bound must never refuse what gonk itself offers — the SPARQL editor's sample queries and
/// its cross-graph example — or what the gonk Book sends it. (`tests/web.rs::every_sample_query_returns_rows` RUNS the samples
/// through the bounded hub, so a refusal would fail there too.)
#[test]
fn every_sample_query_is_within_the_bound() {
    let samples = ikigai_gonk::web::SAMPLES
        .iter()
        .map(|(id, _, query)| (*id, *query))
        .chain([("cross-graph", ikigai_gonk::web::CROSS_GRAPH)])
        .chain(BOOK_QUERIES);
    for (id, query) in samples {
        ikigai_gonk::sparql::admit("query", query.as_bytes(), ikigai_gonk::sparql::Text::Query)
            .unwrap_or_else(|e| panic!("sample `{id}`: {e}"));
        assert!(
            ikigai_gonk::sparql::nesting_depth(query.as_bytes()) < 10,
            "sample `{id}` is deeper than any gonk writes; was a leak counted?"
        );
    }
}

/// The review space (`urn:space:{name}`, bound because this scratch config names one) parses
/// a caller's `match` ASK with spargebra: bounded as the store is. Reachable by the owner over
/// the socket; no network grant carries `urn:cap:space:read`.
#[test]
fn the_review_space_refuses_a_nested_match_and_keeps_serving() {
    let mut gonk = Gonk::start(CONFIG);
    let ask = format!("ASK{{FILTER({}1{})}}", "(".repeat(3000), ")".repeat(3000));
    let answer = gonk.over_socket(request(
        Verb::Source,
        "urn:space:reviews",
        &[("match", &ask)],
    ));
    gonk.assert_serving("a nested match over the socket");
    match answer {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "match");
            assert!(detail.contains("deep"), "{detail}");
        }
        other => panic!("a nested match must be refused, got {other:?}"),
    }
}
