//! Ledger [#915](http://localhost:1060/l/default/item/915): one request carrying a deeply
//! nested SPARQL query ABORTED gonk.
//!
//! `spargebra` reads nesting by recursion, and a stack overflow is not a panic — the process
//! aborts (`thread 'tokio-rt-worker' has overflowed its stack`). On `b16f79c` a single GET of
//! `/sparql/results?query=SELECT*{FILTER(((…3000…1…)))}` killed the server.
//!
//! ★ **Every test here runs a REAL gonk in a CHILD process** — the binary this crate builds,
//! over a scratch config home, data home, store and socket, on ports picked at random — and
//! asserts it is still serving after the request. A reproduction that aborts must not take the
//! test binary with it, and only the binary has the stacks `main` sets (`ikigai_gonk::stack`),
//! which is half of what is under test. Nothing here touches a live gonk.
//!
//! One test per door, so each one reproduces on its own: on `b16f79c` every test in this file
//! fails at its first bomb, with the child's `has overflowed its stack` in the message.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic;
use ikigai_resolve::Resolver;

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

/// A chain long enough to abort a 2 MiB stack in this build and short enough to be answered on
/// [`ikigai_gonk::stack::THREAD_STACK_BYTES`]. Measured with `examples/sparql-depth.rs`
/// (store, `union`): a release build aborts past 1,127 at 2 MiB, a debug build past 163, and
/// 64 MiB holds 32 times either. Release: 3,500 (28 KB, under the length bound); debug: 1,000.
/// Both were checked to ABORT a 2 MiB thread and to be answered on a 64 MiB one, in 0.2 s.
fn deep_chain() -> String {
    chain(if cfg!(debug_assertions) { 1_000 } else { 3_500 })
}

/// A scratch gonk in a child process. Killed and reaped on drop.
struct Gonk {
    child: Child,
    http: SocketAddr,
    quic: SocketAddr,
    socket: PathBuf,
    stderr: PathBuf,
    layout: quic::Layout,
    client: quic::Bundle,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Gonk {
    fn start() -> Gonk {
        let home = tempfile::tempdir().unwrap();
        // A Unix socket path must fit `sun_path` (104 bytes on macOS); a scratch dir does not.
        let short = tempfile::Builder::new()
            .prefix("gk")
            .tempdir_in("/tmp")
            .unwrap();
        let config = home.path().join("config");
        let data = home.path().join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        // The review space, so its `match` door is bound (no grant, not armed: nothing runs).
        std::fs::write(
            config.join("config.toml"),
            "gonk.review.space = \"reviews\"\n",
        )
        .unwrap();

        // One QUIC client, enrolled for the default ledger — what `client add` writes.
        let layout = quic::Layout::in_config_home(&config);
        quic::server_identity(&layout).unwrap();
        let client = quic::add_client(&layout, "alpha", None, false).unwrap();
        quic::enrol(
            &layout,
            "alpha",
            &client.fingerprint,
            &grants_for("default", Authority::Write).unwrap(),
            false,
        )
        .unwrap();

        let (port, quic_port) = (free_tcp_port(), free_udp_port());
        let socket = short.path().join("s");
        let stderr = home.path().join("stderr.log");
        let child = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .arg("--config-home")
            .arg(&config)
            .arg("--data-home")
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .args(["--port", &port.to_string()])
            .args(["--quic-bind", &format!("127.0.0.1:{quic_port}")])
            .arg("--no-backup")
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .expect("spawn ikigai-gonk");
        let mut gonk = Gonk {
            child,
            http: format!("127.0.0.1:{port}").parse().unwrap(),
            quic: format!("127.0.0.1:{quic_port}").parse().unwrap(),
            socket,
            stderr,
            layout,
            client,
            _dirs: (home, short),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !(gonk.socket.exists() && TcpStream::connect(gonk.http).is_ok()) {
            gonk.assert_alive("starting");
            assert!(
                Instant::now() < deadline,
                "gonk never came up: {}",
                gonk.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        gonk
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

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
        let identity = ikigai_quic::Identity {
            cert_pem: std::fs::read_to_string(self.client.dir.join("client.crt")).unwrap(),
            key_pem: std::fs::read_to_string(self.client.dir.join("client.key")).unwrap(),
        };
        let (server, _) = quic::server_identity(&self.layout).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let client = loop {
            match ikigai_quic::connect(self.quic, &identity, &server.cert_pem) {
                Ok(client) => break client,
                Err(e) if Instant::now() > deadline => panic!("QUIC connect: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        client.issue(request).map(|(repr, _)| repr)
    }
}

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    let mut gonk = Gonk::start();
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
    let mut gonk = Gonk::start();
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
    let mut gonk = Gonk::start();
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
    let mut gonk = Gonk::start();
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

/// ★ The half of #915 no bracket scan sees: a chain with no nesting at all, which the store
/// walks by recursion, sent through one door to a fresh gonk. Answered, because the thread
/// serving it is larger than 2 MiB: the socket's per-connection thread and the QUIC runtime are
/// spawned inside their transport crates and reached only through `stack::enlarge_default`;
/// the HTTP runtime is `main`'s builder. One test per door, so each one is proven on its own —
/// with `THREAD_STACK_BYTES` set back to 2 MiB, all three abort the child.
fn a_chain_is_answered_through(door: &str) {
    let mut gonk = Gonk::start();
    let query = deep_chain();
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    assert_eq!(ikigai_gonk::sparql::nesting_depth(query.as_bytes()), 2);
    let answer = match door {
        "socket" => gonk.over_socket(select(&query)).map(|_| ()),
        "quic" => gonk.over_quic(select(&query)).map(|_| ()),
        _ => {
            let (status, body) = gonk.get(&format!(
                "/iki/store/graph-select?graph={GRAPH}&query={}",
                encoded(&query)
            ));
            if status == 200 {
                Ok(())
            } else {
                Err(Error::Endpoint(format!("{status} {body}")))
            }
        }
    };
    gonk.assert_serving(&format!("a chain over {door}"));
    answer.unwrap_or_else(|e| panic!("a chain over {door}: {e}"));
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

/// `!` is the one prefix operator spargebra reads by recursion, so a run of them nests with no
/// bracket at all: 6,000 abort a 2 MiB release parse (measured by the sibling `ikigai-store` arc
/// at 5,229 bytes, and here with `examples/sparql-depth.rs`, shape `not`). Counted as nesting.
#[test]
fn a_run_of_negations_is_refused_and_the_server_lives() {
    let mut gonk = Gonk::start();
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
/// the sibling arc measured; 5,000 abort the whole read here) with no nesting. Answered on the
/// larger stacks — release 10,000 terms in about 1.3 s, debug 1,000 (which abort a 2 MiB debug
/// thread).
#[test]
fn a_multiplication_chain_is_answered_over_http() {
    let mut gonk = Gonk::start();
    let n = if cfg!(debug_assertions) {
        1_000
    } else {
        10_000
    };
    let query = format!("SELECT*{{FILTER(1{})}}", "*1".repeat(n));
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&query)
    ));
    gonk.assert_serving("a multiplication chain over HTTP");
    assert_eq!(status, 200, "{body}");
}

/// A property path `?s a/a/a/… ?o`: 6,000 steps (12 KB) abort a 2 MiB release PARSE, and the
/// store's own walk aborts far sooner. On the larger stacks the parse and the walk both finish —
/// and then the store EVALUATES the path in time that grows with the cube of its length (2,000
/// steps took 168 s in release), which is not an abort and not this arc's to bound (reported). So
/// the request is sent and left running, and what is asserted is that gonk survives the part
/// that used to kill it and keeps serving beside it.
#[test]
fn a_path_chain_does_not_abort_the_server() {
    let mut gonk = Gonk::start();
    let n = if cfg!(debug_assertions) { 2_000 } else { 6_000 };
    let query = format!("SELECT*{{?s a{} ?o}}", "/a".repeat(n));
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    // Left running, over the SOCKET (a thread of its own per connection); the child is killed
    // when `gonk` drops.
    let socket = gonk.socket.clone();
    std::thread::spawn(move || {
        if let Ok(client) = ikigai_ipc::connect(&socket) {
            let _ = client.issue(select(&query));
        }
    });
    // Release aborts in milliseconds on a 2 MiB stack; a debug parse of 2,000 steps is slower.
    std::thread::sleep(Duration::from_secs(3));
    gonk.assert_serving("a path chain over the socket, still being evaluated");
}

/// One element past what the length bound admits is refused, never parsed.
#[test]
fn a_query_past_the_length_bound_is_refused() {
    let mut gonk = Gonk::start();
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
    let mut gonk = Gonk::start();
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
