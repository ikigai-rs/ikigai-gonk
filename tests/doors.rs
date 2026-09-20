//! The three doors, each driven the way a real client drives it.
//!
//! - [`a_write_through_a_door_cuts_the_hubs_cache`] — the reason the doors forward to one
//!   hub rather than each owning a kernel.
//! - [`the_socket_door_serves_the_hub`] — a real Unix socket and a real `ikigai-ipc` client,
//!   which is where a nested executor would panic.
//! - [`the_quic_door_admits_by_grant_and_refuses_a_stranger`] — mutual TLS, an enrolled
//!   certificate under a one-ledger grant, and a trusted-but-unenrolled one.
//! - [`the_http_door_grants_its_ledgers_to_loopback_and_nothing_more`] — raw HTTP against
//!   the served door, and the capability function on a non-loopback peer.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

fn hub() -> Arc<Kernel> {
    Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ))
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(result: Result<ikigai_core::Representation, Error>) -> String {
    String::from_utf8_lossy(&result.unwrap_or_else(|e| panic!("{e}")).bytes).into_owned()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} never became ready");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Resolve under root through the kernel's own async `issue`.
///
/// ⚠ Named in full on purpose: with `ikigai_resolve::Resolver` in scope, `hub.issue(…)` on an
/// `Arc<Kernel>` resolves to the SYNCHRONOUS `Resolver::issue` (through `impl Resolver for
/// Arc<R>`) before auto-deref reaches the inherent method.
fn root_issue(kernel: &Kernel, request: Request) -> String {
    text(block_on(Kernel::issue(
        kernel,
        request,
        &Capability::root(),
    )))
}

#[test]
fn a_write_through_a_door_cuts_the_hubs_cache() {
    let hub = hub();
    let door = doors::door_kernel(Arc::clone(&hub));
    let root = Capability::root();
    let items = || request(Verb::Source, "urn:iki:ledger:items", &[]);
    let append = |content| request(Verb::Sink, "urn:iki:ledger:append", &[("content", content)]);

    root_issue(&hub, append("first"));
    assert!(root_issue(&hub, items()).contains("first"));
    assert!(
        Kernel::is_cached(&hub, &items(), &root),
        "the hub caches a ledger read"
    );

    root_issue(&door, append("second"));
    assert!(
        !Kernel::is_cached(&hub, &items(), &root),
        "a write through a door must cut the hub's threads — a door that owned its own kernel \
         would leave this read cached and stale"
    );
    assert!(root_issue(&hub, items()).contains("second"));

    assert!(root_issue(&door, items()).contains("second"));
    assert!(
        !Kernel::is_cached(&door, &items(), &root),
        "a door kernel stores nothing"
    );
}

#[test]
fn the_socket_door_serves_the_hub() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("gonk.sock");
    let hub = hub();
    let (door, path) = (doors::door_kernel(Arc::clone(&hub)), socket.clone());
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    wait_for("the socket", || socket.exists());

    let client = ikigai_ipc::connect(&socket).expect("connect");
    let (filed, _) = client
        .issue(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", "over the socket")],
        ))
        .expect("append over the socket");
    assert!(String::from_utf8_lossy(&filed.bytes).starts_with("#1 "));
    let (listed, _) = client
        .issue(request(Verb::Source, "urn:iki:ledger:items", &[]))
        .expect("list over the socket");
    assert!(String::from_utf8_lossy(&listed.bytes).contains("over the socket"));
    // The same bytes are in the hub, not in some other kernel's store.
    assert!(
        root_issue(&hub, request(Verb::Source, "urn:iki:ledger:items", &[]))
            .contains("over the socket")
    );
}

#[test]
fn the_quic_door_admits_by_grant_and_refuses_a_stranger() {
    let dir = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(dir.path());
    let (server, _) = quic::server_identity(&layout).unwrap();
    let alpha = quic::add_client(&layout, "alpha", None, false).unwrap();
    quic::enrol(
        &layout,
        "alpha",
        &alpha.fingerprint,
        &grants_for("default", Authority::Write).unwrap(),
        false,
    )
    .unwrap();
    // Trusted at the TLS layer, enrolled nowhere.
    let beta = quic::add_client(&layout, "beta", None, false).unwrap();
    let trusted: Vec<String> = quic::trusted_client_certs(&layout)
        .unwrap()
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    assert_eq!(trusted.len(), 2);

    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (door, minter) = (doors::door_kernel(hub()), quic::minter(layout.clone()));
    let server_identity = ikigai_quic::Identity {
        cert_pem: server.cert_pem.clone(),
        key_pem: server.key_pem,
    };
    std::thread::spawn(move || ikigai_quic::serve(door, addr, &server_identity, &trusted, minter));

    let identity = |bundle: &quic::Bundle| ikigai_quic::Identity {
        cert_pem: std::fs::read_to_string(bundle.dir.join("client.crt")).unwrap(),
        key_pem: std::fs::read_to_string(bundle.dir.join("client.key")).unwrap(),
    };
    let mut connected = None;
    wait_for("the QUIC door", || {
        connected = ikigai_quic::connect(addr, &identity(&alpha), &server.cert_pem).ok();
        connected.is_some()
    });
    let client = connected.unwrap();

    let (filed, _) = client
        .issue(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", "over quic")],
        ))
        .expect("an enrolled certificate may write its ledger");
    assert!(String::from_utf8_lossy(&filed.bytes).starts_with("#1 "));
    let (listed, _) = client
        .issue(request(Verb::Source, "urn:iki:ledger:items", &[]))
        .unwrap();
    assert!(String::from_utf8_lossy(&listed.bytes).contains("over quic"));

    // The grant is ONE ledger: another ledger and the store's broad door are both refused.
    for (verb, iri, args) in [
        (
            Verb::Sink,
            "urn:iki:ledger:acme:append",
            vec![("content", "elsewhere")],
        ),
        (
            Verb::Source,
            "urn:iki:store:select",
            vec![("query", "SELECT * WHERE { ?s ?p ?o }")],
        ),
    ] {
        match client.issue(request(verb, iri, &args)) {
            Err(Error::Denied(_)) => {}
            other => panic!("{iri} under a default-ledger grant must be Denied, got {other:?}"),
        }
    }

    // Trusted by TLS, enrolled nowhere: refused at the connection.
    match ikigai_quic::connect(addr, &identity(&beta), &server.cert_pem) {
        Err(_) => {}
        Ok(stranger) => assert!(
            stranger
                .issue(request(Verb::Source, "urn:iki:ledger:items", &[]))
                .is_err(),
            "an unenrolled certificate must get nothing"
        ),
    }
}

fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {response}"));
    (status, response)
}

#[test]
fn the_http_door_grants_its_ledgers_to_loopback_and_nothing_more() {
    let grants = grants_for("default", Authority::Write).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let hub = hub();
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        quic::Layout::in_config_home(config.path()),
        addr.port(),
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
    });
    let kernel = Arc::new(doors::http_kernel(hub, ikigai_gonk::web::space(face)));
    let door = doors::HttpDoor {
        anonymous: grants.clone(),
        port: addr.port(),
        passkeys: Some(passkeys),
    };
    let cap = doors::http_cap(door.clone());
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            kernel,
            cap,
            listener,
            doors::edge_config(),
        ))
    });

    let (status, response) = http(addr, "POST", "/iki/ledger/append", "Filed over http");
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("#1 "), "{response}");
    let (status, response) = http(addr, "GET", "/iki/ledger/items", "");
    assert_eq!(status, 200, "{response}");
    assert!(response.contains("Filed over http"), "{response}");

    // Outside the grant: another ledger, and the store's whole-dataset door.
    let (status, response) = http(addr, "GET", "/iki/ledger/acme/items", "");
    assert_eq!(status, 403, "{response}");
    let (status, response) = http(
        addr,
        "GET",
        "/iki/store/select?query=ASK%7B%3Fs%20%3Fp%20%3Fo%7D",
        "",
    );
    assert_eq!(status, 403, "{response}");

    // The capability is a function of the peer: a non-loopback one gets nothing.
    let at = |peer: &str| {
        Capability::scoped(doors::http_scopes(
            &door,
            &ikigai_web::HttpRequest {
                method: "GET".into(),
                path: "/iki/ledger/items".into(),
                query: Vec::new(),
                headers: vec![("host".into(), "localhost".into())],
                body: Vec::new(),
                peer: Some(peer.parse::<IpAddr>().unwrap()),
            },
            0,
        ))
    };
    assert!(at("127.0.0.1").allows("urn:cap:ledger:write:default"));
    assert!(at("::1").allows("urn:cap:ledger:write:default"));
    assert!(
        at("::ffff:127.0.0.1").allows("urn:cap:ledger:write:default"),
        "IPv4-mapped loopback"
    );
    assert!(!at("192.168.1.20").allows("urn:cap:ledger:read:default"));
}

/// ★★ **The backup family is not reachable from the public door, and this is the proof the
/// brief asked for.** `urn:iki:gonk:backup` reads every graph in the dataset and
/// `urn:iki:gonk:restore` builds a store from bytes a caller supplies: between them they are
/// the two most dangerous grants in this server. The posture that must survive the feature
/// is the one the ledger recorded on 2026-09-16 — the loopback HTTP door would not serve
/// even `urn:iki:store:info` — and it survives structurally rather than carefully, because
/// the HTTP door's capability is exactly a list of per-ledger tokens.
#[test]
fn the_public_http_door_cannot_reach_the_backup_family() {
    let backups = tempfile::tempdir().unwrap();
    let grants = ikigai_gonk::grants::grants_for_all(
        &["default".to_string()],
        ikigai_gonk::grants::Authority::Write,
    )
    .unwrap();
    // Every token an anonymous loopback caller holds, and a signed-in passkey's grant can
    // only be another ledger's: none of these three is mintable by this server at all.
    for token in [
        ikigai_gonk::backup::CAP_BACKUP,
        ikigai_gonk::backup::CAP_RESTORE,
        ikigai_store::CAP_READ,
    ] {
        assert!(
            !grants.contains(&token.to_string()),
            "`{token}` must never be in what the HTTP door hands out: {grants:?}"
        );
    }

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let hub = Arc::new(ikigai_gonk::compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Some(ikigai_gonk::backup::Backups {
            settings: Arc::new(ikigai_gonk::backup::Settings {
                dir: backups.path().to_path_buf(),
                keep: 5,
                every: Some(Duration::from_secs(86_400)),
                store_path: backups.path().join("nonexistent-store"),
            }),
            jobs: None,
        }),
    ));
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        quic::Layout::in_config_home(config.path()),
        addr.port(),
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
    });
    let kernel = Arc::new(doors::http_kernel(
        Arc::clone(&hub),
        ikigai_gonk::web::space(face),
    ));
    let door = doors::HttpDoor {
        anonymous: grants,
        port: addr.port(),
        passkeys: Some(passkeys),
    };
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            kernel,
            doors::http_cap(door),
            listener,
            doors::edge_config(),
        ))
    });

    // The ledger still works over the public door — otherwise "denied" would prove nothing.
    let (status, response) = http(addr, "GET", "/iki/ledger/items", "");
    assert_eq!(status, 200, "{response}");

    for (method, path) in [
        ("GET", "/iki/gonk/backup"),
        ("GET", "/iki/gonk/backup/status"),
        ("GET", "/iki/gonk/backup/archive/anything.nq.gz"),
        ("POST", "/iki/gonk/restore"),
        // The posture the ledger recorded, re-checked with the family bound.
        ("GET", "/iki/store/info"),
    ] {
        let (status, response) = http(
            addr,
            method,
            path,
            "into=/tmp/gonk-restore-should-not-happen",
        );
        assert_eq!(
            status, 403,
            "{method} {path} must be denied on the public door, got {status}: {response}"
        );
    }
    assert!(
        !std::path::Path::new("/tmp/gonk-restore-should-not-happen").exists(),
        "a denied restore must not have built a store"
    );
    assert!(
        ikigai_gonk::backup::archives(backups.path()).is_empty(),
        "a denied backup must not have written an archive"
    );
}

/// And the same two tokens cannot be attached to an identity either: `grants.json` is
/// refused at startup and again per connection, so no certificate and no passkey can carry
/// them onto a network door.
#[test]
fn a_grant_naming_the_backup_tokens_is_refused() {
    for token in [
        ikigai_gonk::backup::CAP_BACKUP,
        ikigai_gonk::backup::CAP_RESTORE,
    ] {
        let grants = std::collections::BTreeMap::from([(
            "laptop".to_string(),
            vec!["urn:cap:ledger:read:default".to_string(), token.to_string()],
        )]);
        let refusal = quic::check_grants(&grants)
            .expect_err(&format!("`{token}` must not be grantable to an identity"));
        assert!(refusal.contains(token), "{refusal}");
        assert!(refusal.contains("owner-only socket"), "{refusal}");
    }
}
