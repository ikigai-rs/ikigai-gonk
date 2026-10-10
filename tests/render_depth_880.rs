//! Ledger [#880](http://localhost:1060/l/default/item/880), S2: `urn:iki:gonk:render` is open
//! to any page — and what that reached was a CRASH.
//!
//! The resource requires nothing, so any caller reaches it, and the `/k` adapter forwards a
//! `content=` argument from a GET's command line. xrust (through `ikigai_xslt::transform_xml`)
//! parses and transforms by recursion, so a document nested a few hundred levels deep
//! overflowed the tokio worker's 2 MiB stack and ABORTED THE PROCESS: on `77ac767` a single GET
//! of `/k?c=source urn:iki:gonk:render content=<view:page><a><a>…` (about 1 KB, no cookie, no
//! namespace declaration needed) killed this test binary with `fatal runtime error: stack
//! overflow`. A page on any site could send it as an image.
//!
//! This file holds only these tests, so the reproduction aborting takes nothing else down with
//! it. Nothing here touches a live gonk.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::render::{self, MAX_CHUNK_DEPTH};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

struct Server {
    addr: SocketAddr,
    _config: tempfile::TempDir,
}

fn server() -> Server {
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let passkeys = Arc::new(Passkeys::new(
        quic::Layout::in_config_home(config.path()),
        addr.port(),
    ));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Write).unwrap(),
        bind: addr,
        passkeys: Some(passkeys),
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
        _config: config,
    }
}

impl Server {
    /// A GET the way a page at another localhost port sends it: as an `<img>` (`no-cors`,
    /// `image`), or by navigating the window there (`navigate`, `document`) — which, since the
    /// rest of ledger #880, is the only way such a page reaches gonk at all.
    fn get_from_another_page(&self, path: &str, mode: &str, dest: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nSec-Fetch-Site: same-site\r\n\
             Sec-Fetch-Mode: {mode}\r\nSec-Fetch-Dest: {dest}\r\n\
             Referer: http://localhost:8090/innocent.html\r\nConnection: close\r\n\r\n",
            self.addr.port()
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line for GET {path}: {response:?}"));
        (status, body.to_string())
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

/// The reproduction. On `77ac767` the first request — sent then as an `<img>` — aborted the
/// process. An image load from another page is now refused at the edge before anything runs
/// (`admit::REFUSED_FOREIGN_LOAD`), so the bound is exercised here by the path that is still
/// open to such a page: navigating the window there (`location = …`, no click needed).
#[test]
fn a_deeply_nested_document_from_another_page_is_refused_and_the_server_lives() {
    let server = server();
    let bomb = format!("<view:page>{}", "<a>".repeat(400));
    let path = format!(
        "/k?c={}",
        encoded(&format!("source urn:iki:gonk:render content={bomb}"))
    );
    let (status, body) = server.get_from_another_page(&path, "no-cors", "image");
    assert_eq!(status, 403, "an image load is refused at the edge: {body}");
    let (status, body) = server.get_from_another_page(&path, "navigate", "document");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("nest"), "{body}");
    // Still serving.
    let (status, body) = server.get_from_another_page("/static/gonk.css", "navigate", "document");
    assert_eq!(status, 200, "{body}");
}

fn render_request(document: &str) -> Request {
    Request::new(Verb::Source, Iri::parse(render::RENDER_IRI).unwrap())
        .with_arg("content", ArgRef::Inline(document.as_bytes().to_vec()))
}

fn nested(depth: usize) -> String {
    // The envelope is the first level; `depth - 1` more inside it.
    render::chunk(&format!(
        "{}{}",
        "<view:x>".repeat(depth - 1),
        "</view:x>".repeat(depth - 1)
    ))
}

/// The bound itself, through the hub, from every caller (root included — the bound protects
/// the process, which no capability makes safe to crash): a document at the bound renders, one
/// level deeper is refused before any parser sees it.
#[test]
fn the_bound_is_exact_and_holds_for_root() {
    let hub = compose(DurableStore::in_memory().unwrap());
    assert_eq!(
        render::nesting_depth(&nested(MAX_CHUNK_DEPTH)),
        MAX_CHUNK_DEPTH
    );
    block_on(Kernel::issue(
        &hub,
        render_request(&nested(MAX_CHUNK_DEPTH)),
        &Capability::root(),
    ))
    .expect("a document at the bound renders");
    match block_on(Kernel::issue(
        &hub,
        render_request(&nested(MAX_CHUNK_DEPTH + 1)),
        &Capability::root(),
    )) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "content");
            assert!(detail.contains("nests"), "{detail}");
        }
        other => panic!("one level past the bound: {other:?}"),
    }
}

/// A declaration can carry nesting the tag count cannot see (an entity whose replacement text
/// is markup), and gonk's chunk documents carry none — so `<!` is refused outright.
#[test]
fn a_document_with_a_declaration_is_refused() {
    let hub = compose(DurableStore::in_memory().unwrap());
    let doctype = format!(
        "<view:page xmlns:view=\"urn:iki:gonk:view#\"><!DOCTYPE x [<!ENTITY e \"{}\">]>&e;</view:page>",
        "&lt;a&gt;".repeat(10)
    );
    for document in [doctype, render::chunk("<!-- a comment -->")] {
        match block_on(Kernel::issue(
            &hub,
            render_request(&document),
            &Capability::root(),
        )) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "content"),
            other => panic!("{document}: {other:?}"),
        }
    }
}
