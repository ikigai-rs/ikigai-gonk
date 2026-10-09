//! Ledger [#878](http://localhost:1060/l/default/item/878): a ledger writer reached the
//! store's raw write doors, so it could write any triple into its ledger's graph — an
//! `author` naming someone else's passkey included, which the item page renders as that
//! person. The author rule (ledger #864 R4, PR 96) binds the `author` ARGUMENT; a raw
//! `INSERT DATA` never passes one.
//!
//! Brian's decision (option (b), 2026-10-09): a caller whose grant is a LEDGER grant may not
//! issue raw store writes against a ledger graph; the ledger's own endpoints keep working for
//! the same caller with the same grant. The refusal is the door's ([`ikigai_gonk::admit`]).
//!
//! The first three tests are the reproduction: each asserted the refusal and FAILED on
//! `1d02e53` (HTTP anonymous loopback, the HTTP door's mechanical route, and a real QUIC client
//! enrolled under a ledger write grant — the Hermes shape), because the forged write was
//! accepted and rendered.
//!
//! Nothing here touches a live gonk: every server binds `127.0.0.1:0` over a tempdir config
//! home.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::access::{AccessLog, Door};
use ikigai_gonk::grants::{browse_graph_grants, grants_for, ledger_graph_grants, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{admit, compose, doors, quic, web};
use ikigai_ledger::vocabulary as v;
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

/// A colleague's passkey, as `clients.json` holds it — the identity the forgery names.
const BRIAN_PASSKEY: &str = "urn:iki:gonk:passkey:Q1JFRC1CUklBTg";
const LEDGER_GRAPH: &str = "urn:iki:ledger:graph:default";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// The forgery: one `ledger:author` triple on `item`, naming Brian's passkey.
fn forged_author(item: &str) -> String {
    format!(
        "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <{item}> <{}> \"{BRIAN_PASSKEY}\" . }} }}",
        v::AUTHOR
    )
}

fn item_iri(text: &str) -> String {
    let prefix = "urn:iki:ledger:default:item:";
    let at = text
        .find(prefix)
        .unwrap_or_else(|| panic!("no item IRI in: {text}"));
    let id: String = text[at + prefix.len()..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    format!("{prefix}{id}")
}

/// The author the ledger reports for item #`number`, read as root through the hub.
fn author_of(hub: &Kernel, number: u32) -> Option<String> {
    let read = block_on(Kernel::issue(
        hub,
        request(
            Verb::Source,
            &format!("urn:iki:ledger:item:{number}"),
            &[("as", "application/json")],
        ),
        &Capability::root(),
    ))
    .expect("the item");
    let item: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
    item["item"]["author"].as_str().map(str::to_string)
}

fn log_lines() -> (Arc<Mutex<Vec<String>>>, AccessLog, AccessLog) {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let http = Arc::clone(&lines);
    let quic = Arc::clone(&lines);
    (
        lines,
        AccessLog::to(
            Door::Http,
            Arc::new(move |line: &str| http.lock().unwrap().push(line.to_string())),
        ),
        AccessLog::to(
            Door::Quic,
            Arc::new(move |line: &str| quic.lock().unwrap().push(line.to_string())),
        ),
    )
}

// ------------------------------------------------------------------------------------------
// A real HTTP door, as tests/audit_864.rs starts one.
// ------------------------------------------------------------------------------------------
struct Server {
    addr: SocketAddr,
    hub: Arc<Kernel>,
    _config: tempfile::TempDir,
}

impl Server {
    fn start(access: Option<AccessLog>) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
        std::fs::write(
            layout.clients_json(),
            r#"{"passkeys": {"Q1JFRC1CUklBTg": {"grant": "brian", "label": "Brian Sletten",
                "public_key": "AA", "sign_count": 0}}}"#,
        )
        .unwrap();
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let http = Arc::new(doors::http_kernel_with(
            Arc::clone(&hub),
            web::space(face),
            access,
        ));
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).unwrap(),
            port: addr.port(),
            passkeys: Some(passkeys),
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

    /// One request, the way `curl` sends it from this machine: loopback, no `Origin`.
    fn raw(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.addr.port()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line for {method} {path}: {response}"));
        (status, body.to_string())
    }

    fn page(&self, path: &str) -> String {
        let (status, page) = self.raw(
            "GET",
            path,
            &[("Accept", "text/html"), ("Sec-Fetch-Site", "none")],
            "",
        );
        assert_eq!(status, 200, "{path}: {page}");
        page
    }
}

/// `?graph=<iri>`, percent-encoded for a query string.
fn graph_query(graph: &str) -> String {
    format!("?graph={}", graph.replace(':', "%3A"))
}

// ------------------------------------------------------------------------------------------
// The reproduction, HTTP: the anonymous loopback grant (any local process) files an item,
// then inserts an author naming Brian's passkey through the store's graph door. On
// `1d02e53` the insert answered 200 and the item list rendered "by Brian Sletten".
// ------------------------------------------------------------------------------------------
#[test]
fn an_anonymous_loopback_caller_cannot_forge_an_author_through_the_store() {
    let (lines, http_log, _) = log_lines();
    let server = Server::start(Some(http_log));
    let (status, filed) = server.raw("POST", "/iki/ledger/append", &[], "Ship it");
    assert_eq!(status, 200, "{filed}");
    let item = item_iri(&filed);

    let (status, body) = server.raw(
        "POST",
        &format!("/iki/store/graph-update{}", graph_query(LEDGER_GRAPH)),
        &[],
        &forged_author(&item),
    );
    let page = server.page("/l/default/item/1");
    assert!(
        status == 403 && !page.contains("Brian Sletten"),
        "a raw store write by the anonymous ledger grant answered {status} ({}) and the item \
         page renders: {}",
        body.trim(),
        page.find("Brian Sletten")
            .map(|at| &page[at.saturating_sub(120)..(at + 40).min(page.len())])
            .unwrap_or("(no forged label)")
    );
    assert_eq!(author_of(&server.hub, 1), None, "nothing was written");
    // ★ Logged: the refusal is the door's, inside the kernel, so the access line records it.
    // An anonymous caller is nobody the door authenticated: on an HTTP write the log names
    // it `anon` (a read, which the transport stamps nothing on, is `-`).
    let logged = lines.lock().unwrap().clone();
    let refused = logged
        .iter()
        .find(|line| line.contains("graph-update"))
        .unwrap_or_else(|| panic!("no access line for the refused write: {logged:#?}"));
    assert!(
        refused.contains(" outcome=denied") && refused.contains(" principal=anon "),
        "{refused}"
    );
}

// ------------------------------------------------------------------------------------------
// The reproduction, QUIC: a client enrolled under the default ledger's WRITE grant, carrying
// that grant as its own capability (`ikigai mcp --grant hermes`, the Hermes shape), over a
// real mutual-TLS connection. On `1d02e53` the insert succeeded and the item's author read
// back as Brian's passkey.
// ------------------------------------------------------------------------------------------
struct QuicDoor {
    client: ikigai_quic::QuicResolver,
    hub: Arc<Kernel>,
    me: String,
    _dir: tempfile::TempDir,
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} never became ready");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn quic_door(grant: &[String], access: Option<AccessLog>) -> QuicDoor {
    let dir = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(dir.path());
    let (server, _) = quic::server_identity(&layout).unwrap();
    let hermes = quic::add_client(&layout, "hermes", None, false).unwrap();
    quic::enrol(&layout, "hermes", &hermes.fingerprint, grant, false).unwrap();
    let trusted: Vec<String> = quic::trusted_client_certs(&layout)
        .unwrap()
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let door = doors::quic_kernel_with(Arc::clone(&hub), access);
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let minter = quic::minter(layout.clone());
    let server_identity = ikigai_quic::Identity {
        cert_pem: server.cert_pem.clone(),
        key_pem: server.key_pem,
    };
    std::thread::spawn(move || ikigai_quic::serve(door, addr, &server_identity, &trusted, minter));
    let identity = ikigai_quic::Identity {
        cert_pem: std::fs::read_to_string(hermes.dir.join("client.crt")).unwrap(),
        key_pem: std::fs::read_to_string(hermes.dir.join("client.key")).unwrap(),
    };
    let mut connected = None;
    wait_for("the QUIC door", || {
        connected = ikigai_quic::connect(addr, &identity, &server.cert_pem).ok();
        connected.is_some()
    });
    QuicDoor {
        client: connected.unwrap(),
        hub,
        me: quic::client_iri(&hermes.fingerprint),
        _dir: dir,
    }
}

impl QuicDoor {
    fn issue(&self, request: Request, capability: &Capability) -> Result<Representation, Error> {
        self.client
            .issue_as(request, capability)
            .map(|(answer, _)| answer)
    }
}

#[test]
fn an_enrolled_quic_ledger_writer_cannot_forge_an_author_through_the_store() {
    let (lines, _, quic_log) = log_lines();
    let write = grants_for("default", Authority::Write).unwrap();
    let door = quic_door(&write, Some(quic_log));
    let narrowed = Capability::scoped(write);
    let filed = door
        .issue(
            request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("content", "Ship it")],
            ),
            &narrowed,
        )
        .expect("a ledger writer files an item");
    let item = item_iri(&String::from_utf8_lossy(&filed.bytes));

    let forged = door.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("graph", LEDGER_GRAPH), ("content", &forged_author(&item))],
        ),
        &narrowed,
    );
    let author = author_of(&door.hub, 1);
    assert!(
        matches!(forged, Err(Error::Denied(_))) && author.as_deref() == Some(door.me.as_str()),
        "a raw store write by a QUIC ledger writer answered {forged:?}; the item's author \
         reads {author:?}"
    );
    // Logged with the principal the door stamped: the client it authenticated.
    let logged = lines.lock().unwrap().clone();
    let refused = logged
        .iter()
        .find(|line| line.contains("graph-update"))
        .unwrap_or_else(|| panic!("no access line for the refused write: {logged:#?}"));
    assert!(
        refused.contains(" door=quic ")
            && refused.contains(" outcome=denied")
            && refused.contains(&format!(" principal={} ", door.me)),
        "{refused}"
    );
}

/// The graveyard is a ledger graph too: a DELETE grant holds its store write token (the
/// ledger archives a deleted item there), and a raw write into it is the same forgery one
/// recovery away.
#[test]
fn the_graveyard_is_refused_like_the_ledger_graph() {
    let delete = grants_for("default", Authority::Delete).unwrap();
    let door = quic_door(&delete, None);
    let narrowed = Capability::scoped(delete);
    let graveyard = "urn:iki:ledger:graph:default:deleted";
    let forged = door.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", graveyard),
                (
                    "content",
                    &format!("INSERT DATA {{ GRAPH <{graveyard}> {{ <urn:x> <urn:y> \"z\" }} }}"),
                ),
            ],
        ),
        &narrowed,
    );
    assert!(matches!(forged, Err(Error::Denied(_))), "{forged:?}");
}

/// Every raw write door, over HTTP, for the anonymous ledger grant: `graph-update` against the
/// graveyard as well as the ledger graph, and the two whole-dataset doors.
#[test]
fn every_raw_write_door_answers_403_over_http() {
    let server = Server::start(None);
    for (path, body) in [
        (
            format!(
                "/iki/store/graph-update{}",
                graph_query("urn:iki:ledger:graph:default:deleted")
            ),
            "INSERT DATA { GRAPH <urn:iki:ledger:graph:default:deleted> { <urn:x> <urn:y> \"z\" } }"
                .to_string(),
        ),
        (
            "/iki/store/update".to_string(),
            forged_author("urn:iki:ledger:default:item:x"),
        ),
        (
            format!("/iki/store/load{}", graph_query(LEDGER_GRAPH)),
            format!("<urn:x> <{}> \"{BRIAN_PASSKEY}\" .", v::AUTHOR),
        ),
    ] {
        let (status, said) = server.raw("POST", &path, &[], &body);
        assert_eq!(status, 403, "{path}: {said}");
    }
}

// ------------------------------------------------------------------------------------------
// What still works: the ledger, for the same caller with the same grant.
// ------------------------------------------------------------------------------------------

/// The ledger's own writes are `graph-update`s too — issued by `ikigai-ledger` inside the hub,
/// under the caller's capability — and the door does not see them. Append, comment, link,
/// edit and close, over HTTP for the anonymous grant.
#[test]
fn the_ledger_still_works_over_http_for_the_same_grant() {
    let server = Server::start(None);
    for (path, body) in [
        ("/iki/ledger/append", "First"),
        ("/iki/ledger/append", "Second"),
        ("/iki/ledger/comment?item=1", "a comment"),
        ("/iki/ledger/link?item=1&type=blocks", "2"),
        ("/iki/ledger/item/1?priority=1", "First, edited"),
        ("/iki/ledger/close?item=2", "done"),
    ] {
        let (status, said) = server.raw("POST", path, &[], body);
        assert_eq!(status, 200, "{path}: {said}");
    }
    let page = server.page("/l/default/item/1");
    assert!(
        page.contains("First, edited") && page.contains("a comment"),
        "{page}"
    );
}

/// The same five writes over QUIC, for a client enrolled under the ledger's WRITE grant and
/// carrying it (the Hermes shape) — each attributed to that client by the door.
#[test]
fn the_ledger_still_works_over_quic_for_the_same_grant() {
    let write = grants_for("default", Authority::Write).unwrap();
    let door = quic_door(&write, None);
    let narrowed = Capability::scoped(write);
    for (iri, args) in [
        ("urn:iki:ledger:append", vec![("content", "First")]),
        ("urn:iki:ledger:append", vec![("content", "Second")]),
        (
            "urn:iki:ledger:comment",
            vec![("item", "1"), ("content", "a comment")],
        ),
        (
            "urn:iki:ledger:link",
            vec![("item", "1"), ("content", "2"), ("type", "blocks")],
        ),
        (
            "urn:iki:ledger:item:1",
            vec![("content", "First, edited"), ("priority", "1")],
        ),
        ("urn:iki:ledger:close", vec![("item", "2")]),
    ] {
        door.issue(request(Verb::Sink, iri, &args), &narrowed)
            .unwrap_or_else(|e| panic!("{iri}: {e}"));
    }
    assert_eq!(author_of(&door.hub, 1).as_deref(), Some(door.me.as_str()));
}

/// Reads are not the rule's: a ledger READER queries its graph through the store's
/// graph-scoped doors exactly as before.
#[test]
fn raw_reads_of_a_ledger_graph_are_unaffected() {
    let read = grants_for("default", Authority::Read).unwrap();
    let door = quic_door(&read, None);
    let reader = Capability::scoped(read);
    let selected = door
        .issue(
            request(
                Verb::Source,
                "urn:iki:store:graph-select",
                &[
                    ("graph", LEDGER_GRAPH),
                    ("query", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1"),
                ],
            ),
            &reader,
        )
        .expect("graph-select");
    assert!(!selected.bytes.is_empty());
    door.issue(
        request(
            Verb::Source,
            "urn:iki:store:graph-ask",
            &[("graph", LEDGER_GRAPH), ("query", "ASK { ?s ?p ?o }")],
        ),
        &reader,
    )
    .expect("graph-ask");
}

// ------------------------------------------------------------------------------------------
// Who can still write raw.
// ------------------------------------------------------------------------------------------

/// An operator's STORE grant for the ledger's graph (`--ledger-graph default`) writes it raw
/// through the QUIC door — the explicit role, and the only one a network door honors.
#[test]
fn an_operators_ledger_graph_grant_writes_raw() {
    let mut grant = grants_for("default", Authority::Write).unwrap();
    grant.extend(ledger_graph_grants("default").unwrap());
    let door = quic_door(&grant, None);
    let operator = Capability::scoped(grant);
    let filed = door
        .issue(
            request(Verb::Sink, "urn:iki:ledger:append", &[("content", "First")]),
            &operator,
        )
        .expect("append");
    let item = item_iri(&String::from_utf8_lossy(&filed.bytes));
    let repair = format!(
        "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <{item}> <{}> \"migrated\" . }} }}",
        v::AUTHOR
    );
    door.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("graph", LEDGER_GRAPH), ("content", &repair)],
        ),
        &operator,
    )
    .expect("the raw grant writes the ledger graph");
    // ★ One graph: the graveyard is still the socket's.
    let graveyard = door.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", "urn:iki:ledger:graph:default:deleted"),
                (
                    "content",
                    "CLEAR GRAPH <urn:iki:ledger:graph:default:deleted>",
                ),
            ],
        ),
        &operator,
    );
    assert!(matches!(graveyard, Err(Error::Denied(_))), "{graveyard:?}");
}

/// The browse graph's write grant (`--browse-graph write`) is minted only as raw quad
/// authority, so it is not this rule's and keeps writing raw — the reviewer grant's shape.
#[test]
fn the_browse_graph_write_grant_still_writes_raw() {
    let grant = browse_graph_grants(Authority::Write).unwrap();
    let door = quic_door(&grant, None);
    let graph = ikigai_gonk::browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    door.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", &graph),
                (
                    "content",
                    &format!("INSERT DATA {{ GRAPH <{graph}> {{ <urn:x> <urn:y> \"z\" }} }}"),
                ),
            ],
        ),
        &Capability::scoped(grant),
    )
    .expect("the browse graph's write grant writes it raw");
}

/// The socket door has no admission layer: its caller is the owner, and that is where
/// migrations and repairs run (`ledger backfill-keys`, under root). Pinned so a change to it is
/// a decision, not a side effect.
///
/// ⚠ **Both halves pinned, the second deliberately.** The socket passes the caller's
/// capability (the transport clamps root to whatever the caller carries), so an owner process
/// that NARROWED itself to a ledger grant — `cap seal`, `ikigai mcp --grant` mounted on the
/// socket — can still write its ledger graph raw there. That is the #878 forgery on the one
/// door this arc left alone; closing it means giving the socket an admission layer that runs
/// this rule and none of the author rules (the owner may name any author). Reported to the
/// hub rather than decided here.
#[test]
fn the_socket_door_is_the_owners_raw_write() {
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let socket = doors::door_kernel(Arc::clone(&hub));
    let forge = || {
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", LEDGER_GRAPH),
                ("content", &forged_author("urn:x")),
            ],
        )
    };
    block_on(Kernel::issue(&socket, forge(), &Capability::root()))
        .expect("the owner writes any graph raw over the socket");
    let narrowed = Capability::scoped(grants_for("default", Authority::Write).unwrap());
    block_on(Kernel::issue(&socket, forge(), &narrowed))
        .expect("and so does an owner process narrowed to a ledger grant (see the doc)");
}

/// The whole-dataset doors need the store's broad token, which no grant may carry — so this
/// is the in-process door with that token handed over directly, and the door refuses it on
/// its own account.
#[test]
fn the_whole_dataset_doors_are_refused_at_a_network_door() {
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let quic = doors::quic_kernel_with(Arc::clone(&hub), None);
    let broad = Capability::scoped([ikigai_store::CAP_WRITE, ikigai_store::CAP_READ]);
    for (iri, args) in [
        (
            "urn:iki:store:update",
            vec![("content", forged_author("urn:x"))],
        ),
        (
            "urn:iki:store:load",
            vec![
                ("content", "<urn:x> <urn:y> \"z\" .".to_string()),
                ("graph", LEDGER_GRAPH.to_string()),
            ],
        ),
    ] {
        let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match block_on(Kernel::issue(
            &quic,
            request(Verb::Sink, iri, &args),
            &broad,
        )) {
            Err(Error::Denied(why)) => assert!(why.contains("WHOLE dataset"), "{iri}: {why}"),
            other => panic!("{iri}: {other:?}"),
        }
    }
}

/// ★ The rule matches the RESOLVED endpoint's id; this pins that the hub describes each raw
/// write IRI with the id the rule names, so a rename in `ikigai-store` fails here instead of
/// silently switching the rule off.
#[test]
fn the_raw_write_doors_are_the_ids_the_rule_matches() {
    let hub = compose(DurableStore::in_memory().unwrap());
    let ids: Vec<String> = [
        "urn:iki:store:update",
        "urn:iki:store:graph-update",
        "urn:iki:store:load",
    ]
    .iter()
    .map(|iri| {
        hub.describe(&Iri::parse(*iri).unwrap())
            .unwrap_or_else(|| panic!("the hub binds {iri}"))
            .id
    })
    .collect();
    assert_eq!(ids, admit::RAW_WRITE_IDS);
}
