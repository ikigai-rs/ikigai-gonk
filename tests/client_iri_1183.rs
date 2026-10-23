//! Ledger [#1183](http://localhost:1060/l/default/item/1183): the QUIC door names a client
//! `urn:iki:host:gonk:client:<fingerprint>`, every host's convention (Brian, 2026-10-10), and
//! the name gonk minted before it, `urn:iki:gonk:client:<fingerprint>`, is a PERMANENT alias:
//! never minted, never rewritten where it is stored, always recognized.
//!
//! What each test pins:
//!
//! - a new connection's principal, its access line and the author the door fills are the NEW
//!   form, spelled out here as a literal rather than through `quic::client_iri`, so a change to
//!   the minter cannot change the expectation with it;
//! - the OLD spelling of another machine's name is refused as an author, exactly as the new one
//!   is: if it stopped being principal-shaped, a client could pass as a pre-migration machine
//!   in the record. A client's own name in the old spelling is still itself;
//! - a stored old-form author renders as that machine, in the current form, and so does a
//!   new-form one: one actor on the page.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::access::{AccessLog, Door};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

/// The fingerprint every test here names, as `quic::fingerprint_of` writes one.
const FP: &str = "6f1c00aa";
/// Another machine, enrolled before the migration.
const OTHER_FP: &str = "cd34ef56";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn hub() -> Arc<Kernel> {
    Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ))
}

/// The capability a QUIC session carries for `FP`, minted the way the door mints it, and the
/// config home it is enrolled in (the door re-reads it on every call, ledger #1710).
fn session() -> (tempfile::TempDir, Capability) {
    let dir = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(dir.path());
    std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
    quic::enrol(
        &layout,
        "laptop",
        FP,
        &grants_for("default", Authority::Write).unwrap(),
        false,
    )
    .unwrap();
    let enrolment = quic::read_enrolment(&layout.clients_json())
        .unwrap()
        .expect("an enrolment");
    let grants = quic::read_grants(&layout.grants_json()).unwrap();
    let (_grant, capability) = quic::authority(&enrolment, &grants, FP).unwrap();
    (dir, capability)
}

#[test]
fn a_new_connection_is_named_in_the_hosts_convention() {
    let (_home, capability) = session();
    assert_eq!(
        capability.principal(),
        Some("urn:iki:host:gonk:client:6f1c00aa"),
        "the minter names a connection urn:iki:host:gonk:client:<fp>"
    );
    assert_eq!(
        quic::client_iri("6F:1C:00:AA"),
        "urn:iki:host:gonk:client:6f1c00aa"
    );
}

#[test]
fn an_unnamed_quic_write_is_filled_and_logged_with_the_new_form() {
    let hub = hub();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&lines);
    let (home, capability) = session();
    let door = doors::quic_kernel_with(
        Arc::clone(&hub),
        Some(AccessLog::to(
            Door::Quic,
            Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
        )),
        quic::Layout::in_config_home(home.path()),
    );
    block_on(door.issue(
        request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", "from the laptop")],
        ),
        &capability,
    ))
    .expect("the client files an item");
    let read = block_on(hub.issue(
        request(
            Verb::Source,
            "urn:iki:ledger:item:1",
            &[("as", "application/json")],
        ),
        &Capability::root(),
    ))
    .expect("the item");
    let item: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
    assert_eq!(
        item["item"]["author"].as_str(),
        Some("urn:iki:host:gonk:client:6f1c00aa"),
        "{item:#}"
    );
    let logged = lines.lock().unwrap().clone();
    assert_eq!(logged.len(), 1, "{logged:#?}");
    assert!(
        logged[0].contains(" principal=urn:iki:host:gonk:client:6f1c00aa "),
        "{logged:#?}"
    );
}

/// ★ The impersonation the alias exists to stop. A client of this server writes, as its
/// author, the OLD spelling of a machine that wrote here before the migration. If the old
/// prefix were no longer principal-shaped, that string would pass as plain text, and the item
/// would carry exactly the author the other machine's own writes carry.
#[test]
fn the_old_spelling_of_another_machine_is_refused_and_ones_own_is_not() {
    let hub = hub();
    let (home, capability) = session();
    let door = doors::quic_kernel_with(
        Arc::clone(&hub),
        None,
        quic::Layout::in_config_home(home.path()),
    );
    let append = |author: &str| {
        block_on(door.issue(
            request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("author", author), ("content", "authored")],
            ),
            &capability,
        ))
    };
    for forged in [
        format!("urn:iki:gonk:client:{OTHER_FP}"),
        format!("urn:iki:host:gonk:client:{OTHER_FP}"),
        // The prefix's case is ignored, as a reader would.
        format!("URN:IKI:GONK:CLIENT:{OTHER_FP}"),
        format!(" urn:iki:gonk:client:{OTHER_FP} "),
    ] {
        match append(&forged) {
            Err(Error::Denied(why)) => assert!(why.contains("names a principal"), "{why}"),
            other => panic!("`{forged}` as an author must be refused: {other:?}"),
        }
    }
    // Its own name, either spelling: the same actor.
    append(&format!("urn:iki:gonk:client:{FP}")).expect("its own name in the old spelling");
    append(&format!("urn:iki:host:gonk:client:{FP}")).expect("its own name");
    // Plain text is plain text.
    append("hermes").expect("a plain-text author");

    // And nothing forged was filed: three items, none naming the other machine.
    let listed = block_on(hub.issue(
        request(
            Verb::Source,
            "urn:iki:ledger:items",
            &[("status", "all"), ("as", "application/json")],
        ),
        &Capability::root(),
    ))
    .expect("the items");
    let text = String::from_utf8_lossy(&listed.bytes);
    assert!(!text.contains(OTHER_FP), "{text}");
}

/// The other door with an author rule: a signed-in or anonymous HTTP caller cannot write the
/// old spelling either. Checked through `admit` directly, the function every door calls.
#[test]
fn no_door_but_the_machines_own_may_name_it_in_the_old_spelling() {
    let anonymous = Capability::scoped(grants_for("default", Authority::Write).unwrap());
    let old = format!("urn:iki:gonk:client:{FP}");
    let append = request(
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("author", old.as_str())],
    );
    assert!(ikigai_gonk::admit::author_refusal(Door::Http, &anonymous, &append).is_some());
    assert!(ikigai_gonk::admit::author_refusal(Door::Socket, &anonymous, &append).is_some());
    assert!(ikigai_gonk::admit::is_principal_iri(&old));
}

// ------------------------------------------------------------------ the face

struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = hub();
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            toolbox: false,
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
            hub,
            _config: config,
        }
    }

    fn page(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\n\
             Sec-Fetch-Site: none\r\nConnection: close\r\n\r\n",
            self.addr.port()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }
}

/// ★ A stored author is never rewritten, and the face shows both spellings of one machine as
/// that machine, in the current form: the record keeps what was true when it was written, and
/// a reader sees one actor.
#[test]
fn a_stored_old_form_author_still_renders_as_that_machine() {
    let server = Server::start();
    // Seeded through the hub as root, the way the store already holds pre-migration writes.
    for (content, author) in [
        (
            "written before the migration",
            format!("urn:iki:gonk:client:{FP}"),
        ),
        ("written after it", format!("urn:iki:host:gonk:client:{FP}")),
    ] {
        block_on(server.hub.issue(
            request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("content", content), ("author", author.as_str())],
            ),
            &Capability::root(),
        ))
        .expect("a seeded item");
    }
    // The store holds what was written.
    let stored = block_on(server.hub.issue(
        request(
            Verb::Source,
            "urn:iki:ledger:item:1",
            &[("as", "application/json")],
        ),
        &Capability::root(),
    ))
    .unwrap();
    let item: serde_json::Value = serde_json::from_slice(&stored.bytes).unwrap();
    assert_eq!(
        item["item"]["author"].as_str(),
        Some("urn:iki:gonk:client:6f1c00aa"),
        "never rewritten: {item:#}"
    );

    let current = format!("urn:iki:host:gonk:client:{FP}");
    for number in [1, 2] {
        let (status, page) = server.page(&format!("/l/default/item/{number}"));
        assert_eq!(status, 200, "{page}");
        assert!(
            page.contains(&format!("by {current}")),
            "item {number} renders its author as the machine, in the current form: {}",
            page.find("by ")
                .map(|at| &page[at..(at + 80).min(page.len())])
                .unwrap_or(&page[..page.len().min(400)])
        );
        assert!(
            !page.contains(&format!("urn:iki:gonk:client:{FP}")),
            "item {number}: the old spelling is not shown beside the current one"
        );
    }
}
