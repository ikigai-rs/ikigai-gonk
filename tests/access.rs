//! The access log, driven the way each door is really driven (ledger #739): raw HTTP against
//! the served door, and a real `ikigai-ipc` client against a real socket. Every line goes to
//! a collector instead of stderr; the line itself is the one `main` writes.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod ready;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::access::{AccessLog, Door};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

/// A sink that keeps every line.
fn collector() -> (Arc<Mutex<Vec<String>>>, ikigai_gonk::access::Sink) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&lines);
    (
        lines,
        Arc::new(move |line: &str| keep.lock().unwrap().push(line.to_string())),
    )
}

fn hub() -> Arc<Kernel> {
    Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ))
}

fn web_face(hub: &Arc<Kernel>, config: &tempfile::TempDir, port: u16) -> Arc<web::Web> {
    Arc::new(web::Web {
        hub: Arc::clone(hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            port,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
    })
}

/// The HTTP door `main` serves, with the access log on and pointed at `sink`.
fn serve_http(sink: ikigai_gonk::access::Sink) -> (SocketAddr, tempfile::TempDir) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let hub = hub();
    let face = web_face(&hub, &config, addr.port());
    let http = Arc::new(doors::http_kernel_with(
        hub,
        web::space(face),
        Some(AccessLog::to(Door::Http, sink)),
    ));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Write).unwrap(),
        port: addr.port(),
        passkeys: None,
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
    (addr, config)
}

fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n\
         Accept: text/html,*/*;q=0.8\r\nCookie: gonk_session=not-a-real-session\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        addr.port(),
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {response}"))
}

/// The line about `subject`, once it has been written: a line is written when its request
/// ENDS, which is after the response bytes this side reads were produced.
fn line_for(lines: &Mutex<Vec<String>>, subject: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = lines
            .lock()
            .unwrap()
            .iter()
            .find(|l| l.split(' ').nth(2) == Some(subject))
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no line for {subject}: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The columns of a line after the subject, as `(key, value)` in order.
fn columns(line: &str) -> Vec<(String, String)> {
    line.split(' ')
        .skip(3)
        .map(|c| {
            let (k, v) = c.split_once('=').expect("key=value");
            (k.to_string(), v.to_string())
        })
        .collect()
}

fn value(line: &str, key: &str) -> String {
    columns(line)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key} in {line}"))
}

/// ★ One line per HTTP request, in `ikigai-log`'s grammar, carrying what the door can see —
/// and NOT carrying what it must never write: the cookie this client sent, or a form body.
#[test]
fn the_http_door_writes_one_line_per_request() {
    let (lines, sink) = collector();
    let (addr, _config) = serve_http(sink);

    // A page that issues sub-requests of its own: still ONE line.
    assert_eq!(http(addr, "GET", "/l/default?status=all", ""), 200);
    let page = line_for(&lines, "urn:iki:gonk:page:ledger:default");
    eprintln!("example (page):  {page}");

    // An anonymous write through the mechanical mapping, with a body that must not appear.
    assert_eq!(
        http(addr, "POST", "/iki/ledger/append", "a body that stays out"),
        200
    );
    let write = line_for(&lines, "urn:iki:ledger:append");
    eprintln!("example (write): {write}");

    // A ledger this grant does not name: refused by the page, logged with its kind.
    assert_eq!(http(addr, "GET", "/l/acme", ""), 403);
    let refused = line_for(&lines, "urn:iki:gonk:page:ledger:acme");

    let lines = lines.lock().unwrap().clone();
    assert_eq!(
        lines.len(),
        3,
        "exactly one line per request, sub-requests included in their page's: {lines:#?}"
    );
    for line in &lines {
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        assert!(
            parts[0].len() == 24 && parts[0].ends_with('Z') && parts[0].as_bytes()[19] == b'.',
            "RFC 3339 with milliseconds first: {line}"
        );
        assert_eq!(parts[1], "gonk:Access", "{line}");
        let keys: Vec<String> = columns(line).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            ["door", "verb", "outcome", "bytes", "dur", "principal", "q"],
            "every line, every key, this order: {line}"
        );
        assert!(
            !line.contains("not-a-real-session") && !line.contains("stays out"),
            "no cookie and no body: {line}"
        );
        assert_eq!(value(line, "door"), "http");
        value(line, "dur")
            .parse::<u64>()
            .expect("dur is milliseconds");
    }

    assert_eq!(value(&page, "verb"), "source");
    assert_eq!(value(&page, "outcome"), "ok");
    assert!(
        value(&page, "bytes").parse::<usize>().unwrap() > 0,
        "{page}"
    );
    assert_eq!(
        value(&page, "principal"),
        "-",
        "a read is never told: {page}"
    );
    // The face the door negotiated is an argument too, so it is in `q` beside the query.
    assert_eq!(value(&page, "q"), "as=text/html&status=all", "{page}");

    assert_eq!(value(&write, "verb"), "sink");
    assert_eq!(value(&write, "principal"), "anon", "{write}");
    assert_eq!(
        value(&write, "q"),
        "-",
        "the body is `content`, never logged: {write}"
    );

    assert_eq!(value(&refused, "outcome"), "denied", "{refused}");
    assert_eq!(value(&refused, "bytes"), "-", "{refused}");
}

/// The socket door: `owner`, no `q`, and the IRI the client asked for.
#[test]
fn the_socket_door_writes_one_line_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("g.sock");
    let (lines, sink) = collector();
    let door = doors::door_kernel_with(hub(), Some(AccessLog::to(Door::Socket, sink)));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    let client = ready::socket(&socket);
    let iri = |s: &str| Iri::parse(s).unwrap();
    client
        .issue(
            Request::new(Verb::Sink, iri("urn:iki:ledger:append"))
                .with_arg("content", ArgRef::Inline(b"over the socket".to_vec())),
        )
        .expect("append");
    client
        .issue(Request::new(Verb::Source, iri("urn:iki:ledger:items")))
        .expect("list");

    let listed = line_for(&lines, "urn:iki:ledger:items");
    eprintln!("example (socket): {listed}");
    let filed = line_for(&lines, "urn:iki:ledger:append");
    for line in [&listed, &filed] {
        assert_eq!(value(line, "door"), "socket", "{line}");
        assert_eq!(value(line, "outcome"), "ok", "{line}");
        assert_eq!(value(line, "principal"), "owner", "{line}");
        assert_eq!(value(line, "q"), "-", "{line}");
        assert!(!line.contains("over the socket"), "{line}");
    }
    assert_eq!(value(&filed, "verb"), "sink");
    assert_eq!(value(&listed, "verb"), "source");
}

/// The log is an overlay with no identity: the arrangement a door reports is the same with it
/// on or off, so `urn:kernel:topology` never shows a node that is only there to time things.
#[test]
fn the_log_changes_no_doors_arrangement() {
    let topology = |kernel: &Kernel| {
        let answer = futures::executor::block_on(Kernel::issue(
            kernel,
            Request::new(Verb::Source, Iri::parse("urn:kernel:topology").unwrap()),
            &Capability::root(),
        ))
        .expect("the topology");
        String::from_utf8(answer.bytes).unwrap()
    };
    let hub = hub();
    let (_, sink) = collector();
    assert_eq!(
        topology(&doors::door_kernel(Arc::clone(&hub))),
        topology(&doors::door_kernel_with(
            Arc::clone(&hub),
            Some(AccessLog::to(Door::Socket, Arc::clone(&sink)))
        )),
    );
    let config = tempfile::tempdir().unwrap();
    let page = |log: Option<AccessLog>| {
        doors::http_kernel_with(
            Arc::clone(&hub),
            web::space(web_face(&hub, &config, 1060)),
            log,
        )
    };
    assert_eq!(
        topology(&page(None)),
        topology(&page(Some(AccessLog::to(Door::Http, sink)))),
    );
}
