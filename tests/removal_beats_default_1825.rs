//! Ledger [#1825](http://localhost:1062/l/default/item/1825), gonk's half: removal beats
//! `default`. The rule, in one sentence: **`default` applies only to an enrolled client that
//! names no grant.**
//!
//! Through 0.1.x `default` was the grant for every TRUSTED certificate without an entry, and a
//! certificate's trust is read at startup, so `client remove` under a `default` moved the
//! client to the default grant instead of refusing it: at its next call on the connection it
//! held, and at every new connection until a restart.
//!
//! Everything is in process on `127.0.0.1:0` over a scratch config home (the door kernel and
//! the minter `main` uses); nothing touches a live service.
//!
//! - [`a_removed_client_is_refused_whatever_default_says`]
//! - [`an_enrolled_client_that_names_no_grant_gets_the_default`]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Error, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;
use serde_json::{json, Value};

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn append(content: &str) -> Request {
    request(Verb::Sink, "urn:iki:ledger:append", &[("content", content)])
}

fn items() -> Request {
    request(Verb::Source, "urn:iki:ledger:items", &[])
}

/// A scratch config home with one client bundle, `hermes`, and gonk's QUIC door serving it.
struct Door {
    addr: SocketAddr,
    identity: ikigai_quic::Identity,
    server_cert: String,
    layout: quic::Layout,
    fingerprint: String,
    _home: tempfile::TempDir,
}

/// Edit a JSON object file in place.
fn edit(path: &std::path::Path, change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let mut doc: serde_json::Map<String, Value> = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap(),
        Err(_) => serde_json::Map::new(),
    };
    change(&mut doc);
    std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
}

/// [`Door`], with `enroll` writing `clients.json` and `grants.json` before it serves. The grant
/// `shared` (read on `default`) is always in `grants.json`, and `clients.json` always names it
/// as the `default`.
fn door(enroll: impl FnOnce(&quic::Layout, &str)) -> Door {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let (server, _) = quic::server_identity(&layout).unwrap();
    let hermes = quic::add_client(&layout, "hermes", None, false).unwrap();
    edit(&layout.grants_json(), |grants| {
        grants.insert(
            "shared".to_string(),
            json!(grants_for("default", Authority::Read).unwrap()),
        );
    });
    enroll(&layout, &hermes.fingerprint);
    edit(&layout.clients_json(), |clients| {
        clients.insert("default".to_string(), json!("shared"));
    });
    let trusted: Vec<String> = quic::trusted_client_certs(&layout)
        .unwrap()
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    let hub: Arc<Kernel> = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let kernel = doors::quic_kernel_with(hub, None, layout.clone());
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr: SocketAddr = udp.local_addr().unwrap();
    let server_identity = ikigai_quic::Identity {
        cert_pem: server.cert_pem.clone(),
        key_pem: server.key_pem,
    };
    let minter = quic::minter(layout.clone());
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            kernel,
            udp,
            &server_identity,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            None,
        )
    });
    Door {
        addr,
        identity: ikigai_quic::Identity {
            cert_pem: std::fs::read_to_string(hermes.dir.join("client.crt")).unwrap(),
            key_pem: std::fs::read_to_string(hermes.dir.join("client.key")).unwrap(),
        },
        server_cert: server.cert_pem,
        layout,
        fingerprint: hermes.fingerprint,
        _home: home,
    }
}

impl Door {
    /// A NEW connection as `hermes` (the handshake is retried until the door is up).
    fn connect(&self) -> ikigai_quic::QuicResolver {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(client) = ikigai_quic::connect(self.addr, &self.identity, &self.server_cert) {
                return client;
            }
            assert!(
                Instant::now() < deadline,
                "the QUIC door never became ready"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn call(client: &ikigai_quic::QuicResolver, request: Request) -> Result<Representation, Error> {
    client.issue(request).map(|(answer, _)| answer)
}

/// ★ **A removed client is refused**, on the connection it holds and on a new one, though the
/// file sets a `default` and its certificate is still trusted at the handshake. Fails on main:
/// the call after the removal is answered under `shared`.
#[test]
fn a_removed_client_is_refused_whatever_default_says() {
    let door = door(|layout, fingerprint| {
        quic::enrol(
            layout,
            "hermes",
            fingerprint,
            &grants_for("default", Authority::Write).unwrap(),
            false,
        )
        .unwrap();
    });
    let client = door.connect();
    call(&client, append("before the removal")).expect("an enrolled client may write");
    call(&client, items()).expect("and read");

    // What `ikigai-gonk client remove --fingerprint <fp>` runs.
    assert!(quic::unenrol(&door.layout, &door.fingerprint).unwrap());
    match call(&client, items()) {
        Err(Error::Denied(why)) => {
            assert!(why.contains("this certificate is not enrolled"), "{why}")
        }
        other => panic!("the next call on the held connection must be Denied, got {other:?}"),
    }
    assert!(
        call(&door.connect(), items()).is_err(),
        "a new connection from the removed client is refused at the minter"
    );
    assert!(quic::current_authority(&door.layout, &door.fingerprint).is_err());
}

/// ★ **An enrolled client that names no grant gets `default`**, and only what `default`
/// grants. Fails on main: an entry with no `grant` made the whole `clients.json` unparsable,
/// so the minter refused the client.
#[test]
fn an_enrolled_client_that_names_no_grant_gets_the_default() {
    let door = door(|layout, fingerprint| {
        edit(&layout.clients_json(), |clients| {
            clients.insert(
                "clients".to_string(),
                json!({ fingerprint: { "label": "hermes" } }),
            );
        });
    });
    let client = door.connect();
    call(&client, items()).expect("the default grant reads");
    match call(&client, append("the default does not write")) {
        Err(Error::Denied(_)) => {}
        other => panic!("a write under the read-only default must be Denied, got {other:?}"),
    }
    let (grant, _) = quic::current_authority(&door.layout, &door.fingerprint).unwrap();
    assert_eq!(grant, "shared");

    // Removed, the same client is refused: the default covered its ENTRY, not its certificate.
    assert!(quic::unenrol(&door.layout, &door.fingerprint).unwrap());
    assert!(matches!(call(&client, items()), Err(Error::Denied(_))));
}
