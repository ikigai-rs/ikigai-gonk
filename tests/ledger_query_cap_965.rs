//! Ledger [#965](http://localhost:1060/l/default/item/965): the 32 KiB SPARQL length bound
//! (ledger #915, PR 101) refused gonk's OWN ledger query.
//!
//! `ikigai-ledger` reads every open item, then asks for their labels, links and comments in one
//! query whose `VALUES` holds every item IRI, about 51 bytes each. The bound sat in the HUB
//! around the store and applied at every depth, so once a ledger held roughly 650 open items
//! that internal query crossed 32 KiB and the home page, `/l/default` and `urn:iki:ledger:next`
//! all answered `400 … is 35225 bytes`. The #915 tests used small ledgers.
//!
//! The length bound is for a CALLER's text: a request a door issued (depth 0 at the hub), and
//! `urn:sparql:*`'s own early check. The nesting bound stays at every depth.
//!
//! ★ A REAL gonk in a child process, as `tests/sparql_depth_915.rs` runs one: over a scratch
//! config home, data home, store and socket, on a random port. Nothing here touches a live gonk.
//! On `eeeeeaf` the first test fails at its first page.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;

/// More open items than fit one 32 KiB `VALUES` at ~51 bytes an IRI (642 do).
const ITEMS: usize = 750;

const GRAPH: &str = "urn:iki:ledger:graph:default";

/// A scratch gonk in a child process. Killed and reaped on drop.
struct Gonk {
    child: Child,
    http: SocketAddr,
    socket: PathBuf,
    stderr: PathBuf,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
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
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
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
            .arg("--no-quic")
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
            socket,
            stderr,
            _dirs: (home, short),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !(gonk.socket.exists() && TcpStream::connect(gonk.http).is_ok()) {
            if let Some(status) = gonk.child.try_wait().unwrap() {
                panic!("gonk DIED starting ({status}):\n{}", gonk.log());
            }
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

    /// A same-origin GET, as gonk's own pages send one. `(0, …)` when nothing answered.
    fn get(&self, path: &str) -> (u16, String) {
        let Ok(mut stream) = TcpStream::connect(self.http) else {
            return (0, "connection refused".to_string());
        };
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html, */*\r\n\
             Sec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: navigate\r\n\
             Sec-Fetch-Dest: document\r\nConnection: close\r\n\r\n",
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

    /// `n` open items in the default ledger, appended over one socket connection.
    fn fill(&self, n: usize) {
        let client = ikigai_ipc::connect(&self.socket).expect("connect");
        for i in 0..n {
            client
                .issue(request(
                    Verb::Sink,
                    "urn:iki:ledger:append",
                    &[("content", &format!("item {i}"))],
                ))
                .unwrap_or_else(|e| panic!("append {i}: {e}"));
        }
    }
}

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A caller's query just past the length bound: a long string literal, no nesting at all.
fn past_the_length_bound() -> String {
    let query = format!(
        "SELECT * WHERE {{ ?s ?p \"{}\" }}",
        "x".repeat(ikigai_gonk::sparql::MAX_QUERY_BYTES + 2_000)
    );
    assert!(query.len() > ikigai_gonk::sparql::MAX_QUERY_BYTES);
    assert!(ikigai_gonk::sparql::nesting_depth(query.as_bytes()) < 4);
    query
}

fn refused_for_length(answer: Result<Representation, Error>, what: &str) {
    match answer {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "query", "{what}");
            assert!(detail.contains("bytes"), "{what}: {detail}");
        }
        other => panic!("{what} must be refused as InvalidArgument, got {other:?}"),
    }
}

/// THE regression: a ledger with more open items than one 32 KiB `VALUES` holds renders its
/// pages and answers `next`, through the HTTP door and the socket.
#[test]
fn a_ledger_past_the_length_bound_still_renders_and_answers_next() {
    let gonk = Gonk::start();
    gonk.fill(ITEMS);
    for path in ["/", "/l/default", "/iki/ledger/next"] {
        let (status, body) = gonk.get(path);
        assert_eq!(
            status,
            200,
            "GET {path} with {ITEMS} open items: {}",
            body.chars().take(400).collect::<String>()
        );
    }
    let next = gonk
        .over_socket(request(Verb::Source, "urn:iki:ledger:next", &[]))
        .unwrap_or_else(|e| panic!("urn:iki:ledger:next over the socket: {e}"));
    assert!(!next.bytes.is_empty(), "next answered nothing");
}

/// The other half: the length bound still holds for a CALLER's own text — the store's door
/// reached directly at depth 0, and `urn:sparql:*`, which checks before it reads anything.
#[test]
fn a_callers_query_past_the_length_bound_is_still_refused() {
    let gonk = Gonk::start();
    // One item, so the default dataset `urn:sparql:select` reads is not empty.
    gonk.fill(1);
    let query = past_the_length_bound();
    refused_for_length(
        gonk.over_socket(request(
            Verb::Source,
            "urn:iki:store:graph-select",
            &[("query", &query), ("graph", GRAPH)],
        )),
        "graph-select over the socket",
    );
    refused_for_length(
        gonk.over_socket(request(
            Verb::Source,
            "urn:iki:store:select",
            &[("query", &query)],
        )),
        "select over the socket",
    );
    refused_for_length(
        gonk.over_socket(request(
            Verb::Source,
            "urn:sparql:select",
            &[("query", &query)],
        )),
        "urn:sparql:select over the socket",
    );
}
