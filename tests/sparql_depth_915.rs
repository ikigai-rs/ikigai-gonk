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

/// A chain of `n` path steps (`?s a/a/a/… ?o`): no brackets at all, a left-deep tree the store
/// walks by recursion — the densest shape `examples/sparql-depth.rs` measured.
fn chain(n: usize) -> String {
    format!("SELECT*{{?s a{} ?o}}", "/a".repeat(n))
}

/// A chain long enough to abort a 2 MiB stack in this build, and short enough to be answered
/// on [`ikigai_gonk::stack::THREAD_STACK_BYTES`]. Measured with `examples/sparql-depth.rs`
/// (store): a release build aborts past 1,406 steps at 2 MiB, a debug build past far fewer, and
/// 64 MiB holds 32 times either. In release the chain is as long as the length bound admits —
/// the claim [`ikigai_gonk::stack`] makes; in debug, where every frame is several times larger,
/// 2,000 steps still abort the old stack and fit the new one.
fn deep_chain() -> String {
    if cfg!(debug_assertions) {
        chain(2_000)
    } else {
        // `SELECT*{?s a` + ` ?o}` is 16 bytes; each step 2.
        chain((ikigai_gonk::sparql::MAX_QUERY_BYTES - 16) / 2)
    }
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
        let Ok(mut stream) = TcpStream::connect(self.http) else {
            return (0, "connection refused".to_string());
        };
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\n\
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
    let bomb = encoded(&bomb());
    for path in [
        format!("/sparql/results?ledger=default&query={bomb}"),
        format!("/sparql?ledger=default&query={bomb}"),
        format!("/sparql?as=application%2Fsparql-results%2Bjson&query={bomb}"),
        format!("/iki/store/graph-select?graph={GRAPH}&query={bomb}"),
        format!("/iki/sparql/select?query={bomb}"),
        format!(
            "/k?c={}",
            encoded(&format!(
                "source urn:iki:store:graph-select graph={GRAPH} query={}",
                self::bomb()
            ))
        ),
    ] {
        let shown: String = path.chars().take(60).collect();
        let (status, body) = gonk.get(&path);
        gonk.assert_serving(&format!("GET {shown}…"));
        assert_eq!(status, 400, "GET {shown}…: {body}");
        assert!(body.contains("deep"), "GET {shown}…: {body}");
    }
}

#[test]
fn the_socket_door_refuses_a_nested_query_or_update_and_keeps_serving() {
    let mut gonk = Gonk::start();
    refused(gonk.over_socket(select(&bomb())), "graph-select");
    gonk.assert_serving("graph-select over the socket");
    refused(
        gonk.over_socket(sparql_select(&bomb())),
        "urn:sparql:select",
    );
    gonk.assert_serving("urn:sparql:select over the socket");
    // An update parses with the same recursion; the owner at root may send one.
    let update = format!(
        "INSERT {{ GRAPH <urn:g> {{ <urn:a> <urn:b> <urn:c> }} }} WHERE {{ FILTER({}1{}) }}",
        "(".repeat(3000),
        ")".repeat(3000)
    );
    refused(
        gonk.over_socket(request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("content", &update), ("graph", "urn:g")],
        )),
        "graph-update",
    );
    gonk.assert_serving("graph-update over the socket");
}

#[test]
fn the_quic_door_refuses_a_nested_query_and_keeps_serving() {
    let mut gonk = Gonk::start();
    refused(gonk.over_quic(select(&bomb())), "graph-select");
    gonk.assert_serving("graph-select over QUIC");
    refused(gonk.over_quic(sparql_select(&bomb())), "urn:sparql:select");
    gonk.assert_serving("urn:sparql:select over QUIC");
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
/// walks by recursion. Answered, because the threads serving it are larger than 2 MiB — on the
/// socket's per-connection thread and the QUIC runtime (both spawned inside their transport
/// crates, reached only through `stack::enlarge_default`) and the HTTP runtime (`main`'s
/// builder). And one byte past the length bound is refused.
#[test]
fn a_chain_with_no_nesting_is_answered_on_the_larger_stacks() {
    let mut gonk = Gonk::start();
    let query = deep_chain();
    assert!(query.len() <= ikigai_gonk::sparql::MAX_QUERY_BYTES);
    assert!(ikigai_gonk::sparql::nesting_depth(query.as_bytes()) <= 1);
    gonk.over_socket(select(&query))
        .unwrap_or_else(|e| panic!("a chain over the socket: {e}"));
    gonk.assert_serving("a chain over the socket");
    gonk.over_quic(select(&query))
        .unwrap_or_else(|e| panic!("a chain over QUIC: {e}"));
    gonk.assert_serving("a chain over QUIC");
    let (status, body) = gonk.get(&format!(
        "/iki/store/graph-select?graph={GRAPH}&query={}",
        encoded(&query)
    ));
    gonk.assert_serving("a chain over HTTP");
    assert_eq!(status, 200, "a chain over HTTP: {body}");

    let past = chain(ikigai_gonk::sparql::MAX_QUERY_BYTES / 2);
    assert!(past.len() > ikigai_gonk::sparql::MAX_QUERY_BYTES);
    refused(
        gonk.over_socket(select(&past)),
        "a query past the length bound",
    );
    gonk.assert_serving("a query past the length bound");
}

/// The bound must never refuse what gonk itself offers: the SPARQL editor's sample queries and
/// its cross-graph example. (`tests/web.rs::every_sample_query_returns_rows` RUNS the samples
/// through the bounded hub, so a refusal would fail there too.)
#[test]
fn every_sample_query_is_within_the_bound() {
    let samples = ikigai_gonk::web::SAMPLES
        .iter()
        .map(|(id, _, query)| (*id, *query))
        .chain([("cross-graph", ikigai_gonk::web::CROSS_GRAPH)]);
    for (id, query) in samples {
        ikigai_gonk::sparql::admit("query", query.as_bytes(), ikigai_gonk::sparql::Text::Query)
            .unwrap_or_else(|e| panic!("sample `{id}`: {e}"));
        assert!(
            ikigai_gonk::sparql::nesting_depth(query.as_bytes()) < 10,
            "sample `{id}` is deeper than any gonk writes; was a leak counted?"
        );
    }
}

/// The review space (`urn:space:{name}`, bound when `gonk.review.space` is configured) parses a
/// caller's `match` ASK with spargebra: bounded as the store is. In process, because the
/// refusal comes before any parse — were it ever to regress, this binary aborting is the
/// failure.
#[test]
fn the_review_space_refuses_a_nested_match() {
    let root = tempfile::tempdir().unwrap();
    let space = ikigai_gonk::sparql::bounded(
        std::sync::Arc::new(ikigai_intray::space(root.path().to_path_buf())),
        ikigai_gonk::sparql::SPACE_RULES,
    );
    let kernel = ikigai_core::Kernel::new(space);
    let ask = format!("ASK{{FILTER({}1{})}}", "(".repeat(3000), ")".repeat(3000));
    let answer = futures::executor::block_on(ikigai_core::Kernel::issue(
        &kernel,
        request(Verb::Source, "urn:space:review", &[("match", &ask)]),
        &ikigai_core::Capability::root(),
    ));
    match answer {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "match");
            assert!(detail.contains("deep"), "{detail}");
        }
        other => panic!("a nested match must be refused, got {other:?}"),
    }
}
