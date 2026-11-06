//! `cargo run --example axe_site -- <out-dir>` — gonk's anonymous pages as a static site, for
//! `ikigai-ui`'s axe checker and its reusable `axe.yml` workflow (UI conventions D18, ledger #2834).
//!
//! It serves the pages through the same HTTP door `main` wires, reads them as a browser would and
//! writes them with [`ikigai_ui::testkit::site`], each under a name ending in `.html`, with the
//! files they link at the paths they link them. The checker opens every `.html` file under a
//! same-origin CSP and fails on an axe violation, a page error or a console error, so an
//! SRI value or a script the CSP would refuse is a failure here too.
//!
//! ⚠ Anonymous only: a static site has no sign-in, and a signed-in header polls paths a file
//! server cannot answer. `ACCESSIBILITY.md` says what covers the signed-in pages.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

const PAGES: [(&str, &str); 13] = [
    ("home.html", "/"),
    ("ledgers.html", "/l"),
    ("dashboard.html", "/dashboard"),
    ("ledger.html", "/l/default"),
    (
        "label-filter.html",
        "/l/default?labels=needs-person&without=blocked",
    ),
    (
        "label-empty.html",
        "/l/default?labels=missing&without=blocked",
    ),
    ("closed.html", "/l/default?status=closed"),
    ("item.html", "/l/default/item/1"),
    ("named-ledger.html", "/l/crew"),
    ("named-item.html", "/l/crew/item/1"),
    ("lifecycle.html", "/l/default/lifecycle"),
    ("doctor.html", "/l/default/doctor"),
    ("sparql.html", "/sparql"),
    ("browse.html", "/browse"),
    ("queue.html", "/queue"),
];

const FILES: [(&str, &str); 5] = [
    ("static/tokens.css", "text/css"),
    ("static/gonk.css", "text/css"),
    ("static/shell.js", "text/javascript"),
    ("static/htmx.min.js", "text/javascript"),
    ("static/gonk.js", "text/javascript"),
];

/// A browser's GET of `path`: its status and its body.
fn get(addr: SocketAddr, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: text/html,*/*;q=0.8\r\n\
         Sec-Fetch-Site: none\r\nSec-Fetch-Mode: navigate\r\nConnection: close\r\n\r\n",
        port = addr.port()
    );
    stream.write_all(head.as_bytes()).expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read");
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a head");
    let status = String::from_utf8_lossy(&response[..split])
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    (status, response[split + 4..].to_vec())
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: axe_site <out-dir>"));
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("a loopback port");
    let addr = listener.local_addr().expect("its address");
    let config = tempfile::tempdir().expect("a scratch config home");
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(DurableStore::in_memory().expect("a store")));
    block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:append").expect("an IRI"),
        )
        .with_arg(
            "content",
            ArgRef::Inline(b"An item for the checker to read".to_vec()),
        )
        .with_arg(
            "labels",
            ArgRef::Inline(b"needs-person,book:ikigai".to_vec()),
        ),
        &Capability::root(),
    ))
    .expect("the scratch item");
    block_on(Kernel::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:crew:append").expect("an IRI"),
        )
        .with_arg("content", ArgRef::Inline(b"A crew item".to_vec())),
        &Capability::root(),
    ))
    .expect("the named-ledger scratch item");
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string(), "crew".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    let http = Arc::new(doors::http_kernel_serving(
        hub,
        web::space(face),
        Vec::new(),
        None,
    ));
    let door = doors::HttpDoor {
        anonymous: anonymous_http_grants(&["default".to_string(), "crew".to_string()])
            .expect("the anonymous grant"),
        bind: addr,
        passkeys: Some(passkeys),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    let edge = doors::edge_config(door.clone());
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door),
            listener,
            edge,
        ))
    });

    let mut bodies: Vec<(String, &str, Vec<u8>)> = Vec::new();
    for (name, path) in PAGES {
        let (status, body) = get(addr, path);
        assert_eq!(status, 200, "{path}");
        bodies.push((name.to_string(), "text/html; charset=utf-8", body));
    }
    for (name, media) in FILES {
        let (status, body) = get(addr, &format!("/{name}"));
        assert_eq!(status, 200, "/{name}");
        bodies.push((name.to_string(), media, body));
    }
    // A browser asks for one on every first load; the checker fails on a 404 in the console.
    bodies.push(("favicon.ico".to_string(), "image/x-icon", Vec::new()));
    let files: Vec<(&str, &str, &[u8])> = bodies
        .iter()
        .map(|(name, media, body)| (name.as_str(), *media, body.as_slice()))
        .collect();
    ikigai_ui::testkit::site::write(&out, &files).expect("the site");
    println!("{} pages in {}", PAGES.len(), out.display());
}
