//! Ledger [#1710](http://localhost:1062/l/default/item/1710): an edit to `grants.json` or
//! `clients.json` reaches a client's STANDING QUIC connection.
//!
//! `ikigai-quic` runs the [`ikigai_quic::Minter`] once per connection, and a client that keeps
//! one connection (Hermes's MCP mount, a worker) answers the door's keep-alive, so it never
//! idles out and never reconnects. Through 9d4e722 that meant a removed or narrowed grant took
//! effect only when gonk restarted: the door served every later call under the authority it
//! minted at the handshake. gonk's QUIC door now re-reads both files on every call that
//! arrives ([`ikigai_gonk::admit`], rule 5): a removed grant or a removed enrollment is refused
//! on the NEXT call, and a narrowed grant answers that call under the narrower authority. A
//! WIDENED grant cannot take effect mid-connection (a session only narrows), so the door also
//! bounds connection age ([`ikigai_gonk::quic::MAX_CONNECTION_AGE`]) and the client heals onto
//! a connection minted afresh.
//!
//! Every test counts the minter's runs, so "the same connection" is asserted, not assumed.
//! Everything is in process on `127.0.0.1:0` over a scratch config home; nothing touches a
//! live service.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Error, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;

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

/// A scratch config home with one client, `hermes`, enrolled under an authority on the
/// `default` ledger, and gonk's QUIC door serving it (the door kernel and the minter `main`
/// uses), with the client connected.
struct Door {
    client: ikigai_quic::QuicResolver,
    /// How many connections the minter has minted.
    minted: Arc<AtomicUsize>,
    layout: quic::Layout,
    fingerprint: String,
    _home: tempfile::TempDir,
}

/// [`Door`] under `authority`, connection age bounded by `max_age`.
fn door(authority: Authority, max_age: Option<Duration>) -> Door {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let (server, _) = quic::server_identity(&layout).unwrap();
    let hermes = quic::add_client(&layout, "hermes", None, false).unwrap();
    quic::enrol(
        &layout,
        "hermes",
        &hermes.fingerprint,
        &grants_for("default", authority).unwrap(),
        false,
    )
    .unwrap();
    let trusted: Vec<String> = quic::trusted_client_certs(&layout)
        .unwrap()
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    let hub: Arc<Kernel> = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let door = doors::quic_kernel_with(hub, None, layout.clone());
    let minted = Arc::new(AtomicUsize::new(0));
    let (count, gonk) = (Arc::clone(&minted), quic::minter(layout.clone()));
    let minter: ikigai_quic::Minter = Arc::new(move |peer| {
        count.fetch_add(1, Ordering::SeqCst);
        gonk(peer)
    });
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr: SocketAddr = udp.local_addr().unwrap();
    let server_identity = ikigai_quic::Identity {
        cert_pem: server.cert_pem.clone(),
        key_pem: server.key_pem,
    };
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            door,
            udp,
            &server_identity,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            max_age,
        )
    });
    let identity = ikigai_quic::Identity {
        cert_pem: std::fs::read_to_string(hermes.dir.join("client.crt")).unwrap(),
        key_pem: std::fs::read_to_string(hermes.dir.join("client.key")).unwrap(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        if let Ok(client) = ikigai_quic::connect(addr, &identity, &server.cert_pem) {
            break client;
        }
        assert!(
            Instant::now() < deadline,
            "the QUIC door never became ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    Door {
        client,
        minted,
        layout,
        fingerprint: hermes.fingerprint,
        _home: home,
    }
}

impl Door {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        self.client.issue(request).map(|(answer, _)| answer)
    }

    fn minted(&self) -> usize {
        self.minted.load(Ordering::SeqCst)
    }

    /// Rewrite `grants.json` with `hermes` holding `scopes`, or with no `hermes` entry at all.
    fn set_grant(&self, scopes: Option<Vec<String>>) {
        let path = self.layout.grants_json();
        let mut grants: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        match scopes {
            Some(scopes) => grants.insert("hermes".to_string(), serde_json::json!(scopes)),
            None => grants.remove("hermes"),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&grants).unwrap()).unwrap();
    }
}

fn denied(answer: Result<Representation, Error>, what: &str) -> String {
    match answer {
        Err(Error::Denied(why)) => why,
        other => panic!("{what} must be Denied, got {other:?}"),
    }
}

/// ★ The defect: a grant deleted from `grants.json` kept answering on the connection that was
/// open when it was deleted. Fails through 9d4e722 at the first `denied` below: the read
/// succeeds.
#[test]
fn a_removed_grant_refuses_the_next_call_on_the_same_connection() {
    let door = door(Authority::Write, None);
    door.call(append("before the edit"))
        .expect("an enrolled client may write its ledger");
    door.call(items()).expect("and read it");

    door.set_grant(None);
    let why = denied(door.call(items()), "a read after the grant was removed");
    assert!(
        why.contains("check grants.json"),
        "the refusal says where to look: {why}"
    );
    denied(
        door.call(append("after")),
        "a write after the grant was removed",
    );
    assert_eq!(door.minted(), 1, "every call rode the one connection");

    // Put back, the same connection answers again: the check reads the files, it does not
    // latch.
    door.set_grant(Some(grants_for("default", Authority::Write).unwrap()));
    door.call(items()).expect("the restored grant answers");
    assert_eq!(door.minted(), 1);
}

/// A client removed from `clients.json` is refused on the next call, though the grant it was
/// enrolled under is still there.
#[test]
fn a_removed_enrollment_refuses_the_next_call_on_the_same_connection() {
    let door = door(Authority::Write, None);
    door.call(items()).expect("an enrolled client may read");
    assert!(quic::unenrol(&door.layout, &door.fingerprint).unwrap());
    denied(door.call(items()), "a read after the client was unenrolled");
    assert_eq!(door.minted(), 1, "every call rode the one connection");
}

/// A grant NARROWED from write to read answers the next call under the narrower authority:
/// the write is refused, the read still answers, on the connection minted under write.
#[test]
fn a_narrowed_grant_narrows_the_next_call_on_the_same_connection() {
    let door = door(Authority::Write, None);
    door.call(append("while it may write"))
        .expect("an enrolled writer may write");

    door.set_grant(Some(grants_for("default", Authority::Read).unwrap()));
    denied(
        door.call(append("after the narrowing")),
        "a write under a read grant",
    );
    let listed = door.call(items()).expect("a read grant still reads");
    let listed = String::from_utf8_lossy(&listed.bytes).into_owned();
    assert!(listed.contains("while it may write"), "{listed}");
    assert!(!listed.contains("after the narrowing"), "{listed}");
    assert_eq!(door.minted(), 1, "every call rode the one connection");
}

/// A WIDENED grant cannot reach a connection minted under the narrower one (a session only
/// narrows), so it takes effect when the door's age bound closes that connection and the
/// client heals onto a new one, minted from the files as they are then. Scaled down from
/// [`quic::MAX_CONNECTION_AGE`] to one second.
///
/// ★ Ledger #1730: "inside the bound" is a claim about the clock, so it is only asserted when
/// it held. The clock starts BEFORE the door is built (so the connection is never older than
/// it says), and when building it and making three calls took longer than the bound, as it can
/// on a loaded machine, that attempt says nothing and the next one doubles the bound; none
/// conclusive is a failure. Past the bound, a READ (replayed on a new connection) drives the
/// heal, rather than a sleep and the hope that the door has closed the old connection by then.
#[test]
fn a_widened_grant_takes_effect_when_the_connection_ages_out() {
    let bounds = [1, 2, 4, 8].map(Duration::from_secs);
    for bound in bounds {
        let started = Instant::now();
        let door = door(Authority::Read, Some(bound));
        door.call(items()).expect("a reader reads");
        let too_soon = door.call(append("too soon"));

        door.set_grant(Some(grants_for("default", Authority::Write).unwrap()));
        let still_too_soon = door.call(append("still too soon"));
        let minted = door.minted();
        if started.elapsed() >= bound {
            println!(
                "{bound:?}: the first calls took {:?}, not inside the bound; trying a longer one",
                started.elapsed()
            );
            continue;
        }
        denied(too_soon, "a write under a read grant");
        // Inside the bound the connection still holds the authority it was minted with.
        denied(
            still_too_soon,
            "a write on the connection minted under read",
        );
        assert_eq!(minted, 1);

        std::thread::sleep(bound.saturating_sub(started.elapsed()));
        let healing = Instant::now();
        while door.minted() < 2 {
            assert!(
                healing.elapsed() < Duration::from_secs(30),
                "the client never healed onto a second connection"
            );
            door.call(items())
                .expect("a read heals onto a new connection");
        }
        door.call(append("after the age bound"))
            .expect("a write on the connection minted under the widened grant");
        assert_eq!(
            door.minted(),
            2,
            "the client healed onto a second connection"
        );
        return;
    }
    panic!("no bound in {bounds:?} held three calls, so nothing was measured");
}
