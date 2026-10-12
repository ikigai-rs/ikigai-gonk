//! Ledger [#1335](http://localhost:1060/l/default/item/1335): a layer host that ACCEPTS a call
//! and never answers it no longer wedges a mirror pass.
//!
//! Before cli 0.1.55 a mounted call had no deadline of its own: only a self-description did, and
//! the QUIC idle timeout (300 s) helped only when the whole connection died. A silent host held
//! a pass for twice the idle timeout (a read is replayed once on a fresh connection), and a host
//! that kept its QUIC stack talking held it for ever. Now every mount carries a call deadline
//! (`ikigai_quic::QuicResolver::with_call_timeout`), and a task layer's door a short one
//! ([`layer::CALL_TIMEOUT`]).
//!
//! The silent host is in process, on `127.0.0.1:0` bound by the server itself. Nothing here
//! touches a live service.

use std::net::SocketAddr;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, Kernel,
    Representation, Request, Verb,
};
use ikigai_gonk::layer::{self, Door};
use ikigai_gonk::mirror::Mirror;
use ikigai_gonk::mount::{LazyMount, Target};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;

/// Accepts every call and never answers it.
struct Silent;

#[async_trait]
impl Endpoint for Silent {
    async fn invoke(&self, _: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        std::future::pending::<()>().await;
        unreachable!("a pending future never completes")
    }
    fn name(&self) -> &str {
        "silent"
    }
    fn describe(&self) -> Description {
        Description::new("silent").verb(Verb::Source)
    }
}

/// A QUIC host whose every named resource is [`Silent`]. Returns its address and the
/// certificate directory a binding presents.
fn silent_host(names: &[&str]) -> (SocketAddr, tempfile::TempDir) {
    let certs = tempfile::tempdir().unwrap();
    let (server, client) = (ikigai_quic::generate(), ikigai_quic::generate());
    for (file, pem) in [
        ("server.crt", &server.cert_pem),
        ("client.crt", &client.cert_pem),
        ("client.key", &client.key_pem),
    ] {
        std::fs::write(certs.path().join(file), pem).unwrap();
    }
    let space = names.iter().fold(EndpointSpace::new(), |space, name| {
        space.bind(Exact::new(*name), Silent)
    });
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let minter: ikigai_quic::Minter = Arc::new(|_| {
        Some(ikigai_quic::Session {
            capability: Capability::root(),
            file_segment: "gonk".to_string(),
            principal: None,
        })
    });
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = udp.local_addr().unwrap();
    let trusted = vec![client.cert_pem];
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            kernel,
            udp,
            &server,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            None,
        )
    });
    (addr, certs)
}

/// Run `work` on a thread and give up after `within`: the bound is the TEST's, so a wedge is a
/// failure here rather than a stuck test binary.
fn bounded<T: Send + 'static>(within: Duration, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(within)
        .unwrap_or_else(|_| panic!("wedged: no answer within {within:?}"))
}

/// ★ The reproduction: a mirror pass through the door a `gonk.layer` binding builds, against a
/// host that accepts the call and never answers. Before: the pass blocked past the test's
/// minute (the transport's 300 s idle timeout, twice). After: it fails at the layer door's
/// call deadline, and the error names the host.
#[test]
fn a_silent_layer_host_fails_the_pass_at_the_doors_deadline() {
    let (addr, certs) = silent_host(&["urn:layer:tasks"]);
    let line = format!(
        "tasks offer quic://{addr} {} ledger=crew",
        certs.path().display()
    );
    let binding = layer::parse(&line, std::path::Path::new("/nowhere")).unwrap();
    let door = layer::mount(std::slice::from_ref(&binding))
        .door
        .expect("a binding mounts a door");
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    let mirror = Mirror::new(binding, door as Arc<dyn Door>, hub).unwrap();

    let started = Instant::now();
    let outcome = bounded(Duration::from_secs(60), move || {
        mirror.pass().map(|report| format!("{report:?}"))
    });
    let elapsed = started.elapsed();
    let why = outcome.expect_err("a pass against a silent host fails");
    assert!(
        why.contains(&format!("quic://{addr}")),
        "the failure names the host: {why}"
    );
    assert!(
        elapsed < layer::CALL_TIMEOUT + Duration::from_secs(10),
        "given up at the layer door's deadline ({:?}), not the transport's: {elapsed:?}",
        layer::CALL_TIMEOUT
    );
}

/// The builder: a mount's call deadline is its own, and it reaches the transport. A model
/// mount keeps the transports' standing bound by default, so no honest model call is cut.
#[test]
fn a_mounts_call_deadline_is_carried_to_the_transport() {
    let (addr, certs) = silent_host(&["urn:llm:ask"]);
    let target = Target::Quic {
        authority: addr.to_string(),
        certs: certs.path().to_path_buf(),
    };
    assert_eq!(
        LazyMount::new(target.clone()).call_timeout(),
        ikigai_gonk::mount::CALL_TIMEOUT
    );
    let mount = Arc::new(LazyMount::new(target).with_call_timeout(Duration::from_millis(500)));
    let started = Instant::now();
    let answer = bounded(Duration::from_secs(30), move || {
        mount.issue(Request::new(
            Verb::Source,
            Iri::parse("urn:llm:ask").unwrap(),
        ))
    });
    match answer {
        Err(Error::Timeout(why)) => assert!(why.contains(&format!("quic://{addr}")), "{why}"),
        other => panic!("a silent call is a Timeout: {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}
