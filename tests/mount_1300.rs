//! Ledger [#1300](http://localhost:1060/l/default/item/1300): gonk's mounts (`gonk.mount`, and a
//! task layer's) after one hiccup, and against a peer that never describes itself.
//!
//! - **One `Unavailable` no longer holds the mount off for 30 s.** Through gonk 0.x,
//!   `LazyMount::drop_connection` held off [`mount::RETRY`] after ANY `Unavailable` answer,
//!   including one the peer's own endpoint gave over a healthy connection, so the next call, to
//!   a healthy endpoint on the same peer, was refused as "held off" for half a minute. Now the
//!   next call redials, and only a FAILED dial starts a backoff ([`mount::hold_after`]: 1 s,
//!   doubling to 30 s), never a dial per request.
//! - **A peer that never describes itself is given up on inside the caller's bound, and the
//!   error names it** (ledger [#1288](http://localhost:1060/l/default/item/1288), cli PR 461's
//!   rule): gonk's mounts get two thirds of the deadline gonk's own callers use
//!   ([`mount::DESCRIBE_TIMEOUT`], 20 s against 30 s). At equal deadlines the caller's own
//!   timeout won the race and blamed gonk.
//!
//! Every peer here is in process, on a scratch socket or `127.0.0.1:0`; the one real gonk is a
//! scratch child with its QUIC door off. Nothing touches a live service.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use ikigai_gonk::mount::{self, LazyMount, Target};
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;

mod ready;
mod scratch_gonk;

/// Answers `ok`, or `Unavailable` when built `flaky`: the peer's own endpoint saying its
/// backend is down, over a connection that is perfectly healthy.
struct Answer {
    flaky: bool,
}

#[async_trait]
impl Endpoint for Answer {
    async fn invoke(&self, _: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        if self.flaky {
            return Err(Error::Unavailable("the backend is restarting".to_string()));
        }
        Ok(Representation::new(ReprType::new("text/plain"), "ok"))
    }
    fn name(&self) -> &str {
        "answer"
    }
    fn describe(&self) -> Description {
        Description::new("answer").verb(Verb::Source)
    }
}

/// An endpoint whose SELF-DESCRIPTION never comes while `hang` is set (until `release`, or a
/// minute): the peer is up, the connection completes, and a `Meta` or an enumeration is never
/// answered. Its `invoke` answers at once.
struct Silent {
    hang: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

#[async_trait]
impl Endpoint for Silent {
    async fn invoke(&self, _: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(ReprType::new("text/plain"), "ok"))
    }
    fn name(&self) -> &str {
        "silent"
    }
    fn describe(&self) -> Description {
        if self.hang.load(Ordering::SeqCst) {
            let until = Instant::now() + Duration::from_secs(60);
            while !self.release.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Description::new("silent").verb(Verb::Source)
    }
}

fn request(verb: Verb, iri: &str) -> Request {
    Request::new(verb, Iri::parse(iri).unwrap())
}

/// The socket peer: `urn:llm:ask` answers, `urn:llm:flaky` answers `Unavailable`.
fn socket_peer(path: &Path) {
    let space = EndpointSpace::new()
        .bind(Exact::new("urn:llm:ask"), Answer { flaky: false })
        .bind(Exact::new("urn:llm:flaky"), Answer { flaky: true });
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let serving = path.to_path_buf();
    std::thread::spawn(move || ikigai_ipc::serve(kernel, &serving));
    drop(ready::socket(path));
}

/// ★ The reproduction: one `Unavailable` answer from a healthy peer, then a call to a healthy
/// endpoint on the same peer. gonk 0.x refused the second as "held off for up to 30s".
#[test]
fn one_unavailable_answer_does_not_hold_the_mount_off() {
    let short = tempfile::Builder::new()
        .prefix("gk")
        .tempdir_in("/tmp")
        .unwrap();
    let path = short.path().join("p");
    socket_peer(&path);
    let mount = LazyMount::new(Target::Socket(path.clone()));

    assert_eq!(
        String::from_utf8_lossy(
            &mount
                .issue(request(Verb::Source, "urn:llm:ask"))
                .unwrap()
                .0
                .bytes
        ),
        "ok",
        "the mount reaches the peer"
    );
    match mount.issue(request(Verb::Source, "urn:llm:flaky")) {
        Err(Error::Unavailable(why)) => assert!(
            why.contains(&path.display().to_string()) && why.contains("restarting"),
            "the transient names the peer and keeps its reason: {why}"
        ),
        other => panic!("the flaky endpoint is Unavailable: {other:?}"),
    }
    // At once, no sleep: the next call redials and answers.
    let next = mount.issue(request(Verb::Source, "urn:llm:ask"));
    assert_eq!(
        next.as_ref()
            .map(|(answer, _)| String::from_utf8_lossy(&answer.bytes).into_owned())
            .map_err(|e| format!("{e:?}")),
        Ok("ok".to_string()),
        "one Unavailable answer must not hold the mount off"
    );
}

/// A peer that is really DOWN is still held off, by a backoff that starts short and grows:
/// held at once after a failed dial, dialled again after [`mount::FIRST_HOLD`], and held for
/// longer after that second failure. Never a dial per request.
#[test]
fn a_down_peer_is_held_off_by_a_growing_backoff() {
    let short = tempfile::Builder::new()
        .prefix("gk")
        .tempdir_in("/tmp")
        .unwrap();
    let mount = LazyMount::new(Target::Socket(short.path().join("nobody")));
    let call = || match mount.issue(request(Verb::Source, "urn:llm:ask")) {
        Err(Error::Unavailable(why)) => why,
        other => panic!("a down peer is Unavailable: {other:?}"),
    };

    let first = call();
    assert!(!first.contains("held off"), "the first call dials: {first}");
    let held = call();
    assert!(held.contains("held off"), "then it is held off: {held}");

    std::thread::sleep(mount::FIRST_HOLD + Duration::from_millis(200));
    let redialled = call();
    assert!(
        !redialled.contains("held off"),
        "after the first hold the mount dials again, rather than waiting out 30 s: {redialled}"
    );
    let longer = call();
    assert!(
        longer.contains("held off") && longer.contains("2 dial(s) failed"),
        "the second failure holds longer: {longer}"
    );
    assert_eq!(mount::hold_after(2), 2 * mount::FIRST_HOLD);
}

/// The QUIC peer: `urn:llm:ask` is [`Silent`]. Returns its address and the certificate
/// directory a mount presents, laid out as `gonk.mount` wants it.
fn silent_quic_peer(
    hang: &Arc<AtomicBool>,
    release: &Arc<AtomicBool>,
) -> (SocketAddr, tempfile::TempDir) {
    let certs = tempfile::tempdir().unwrap();
    let (server, client) = (ikigai_quic::generate(), ikigai_quic::generate());
    for (file, pem) in [
        ("server.crt", &server.cert_pem),
        ("client.crt", &client.cert_pem),
        ("client.key", &client.key_pem),
    ] {
        std::fs::write(certs.path().join(file), pem).unwrap();
    }
    let space = EndpointSpace::new().bind(
        Exact::new("urn:llm:ask"),
        Silent {
            hang: Arc::clone(hang),
            release: Arc::clone(release),
        },
    );
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

/// Run `work` on a thread and give up after `within`: the bound is the TEST's, so a hang is a
/// failure here rather than a stuck test binary.
fn bounded<T: Send + 'static>(within: Duration, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(within)
        .unwrap_or_else(|_| panic!("no answer within {within:?}"))
}

/// A peer that completes the connection and never describes itself: the mount gives up at its
/// own describe deadline, with a `Timeout` that NAMES the peer.
#[test]
fn a_silent_peer_times_out_at_the_mounts_deadline_naming_the_peer() {
    let (hang, release) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (addr, certs) = silent_quic_peer(&hang, &release);
    let mount = Arc::new(
        LazyMount::new(Target::Quic {
            authority: addr.to_string(),
            certs: certs.path().to_path_buf(),
        })
        .with_describe_timeout(Duration::from_millis(500)),
    );
    // Connected and answering before it falls silent.
    let first = Arc::clone(&mount);
    bounded(Duration::from_secs(30), move || {
        first.issue(request(Verb::Source, "urn:llm:ask"))
    })
    .expect("the peer answers a read");

    hang.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let silent = Arc::clone(&mount);
    let described = bounded(Duration::from_secs(20), move || {
        silent.issue(request(Verb::Meta, "urn:llm:ask"))
    });
    let elapsed = started.elapsed();
    release.store(true, Ordering::SeqCst);
    match described {
        Err(Error::Timeout(why)) => assert!(
            why.contains(&format!("quic://{addr}")) && why.contains("did not describe itself"),
            "the timeout names the peer: {why}"
        ),
        other => panic!("a silent peer's Meta is a Timeout: {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(10),
        "given up at the mount's deadline, not a transport's: {elapsed:?}"
    );
}

/// What every mount gonk builds starts with: two thirds of what its callers allow.
#[test]
fn every_mount_takes_two_thirds_of_the_callers_describe_deadline() {
    let mount = LazyMount::new(Target::Socket("/nonexistent/x".into()));
    assert_eq!(mount.describe_timeout(), mount::DESCRIBE_TIMEOUT);
    assert_eq!(
        mount::DESCRIBE_TIMEOUT * 3,
        ikigai_ipc::DEFAULT_DESCRIBE_TIMEOUT * 2
    );
    let layer = LazyMount::fresh(Target::Socket("/nonexistent/x".into()));
    assert_eq!(layer.describe_timeout(), mount::DESCRIBE_TIMEOUT);
}

/// ★ The race of ledger #1288, through a REAL gonk at the default deadlines: a socket client
/// (30 s describe deadline) asks gonk for `urn:llm:ask`'s contract, gonk's `gonk.mount` peer
/// never describes itself, and the client hears GONK's answer naming the peer, inside its own
/// bound. At gonk 0.x's equal deadlines the client's own timeout fired first and blamed gonk.
#[test]
fn gonk_answers_for_a_silent_peer_inside_its_callers_deadline() {
    let (hang, release) = (
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    );
    let (addr, certs) = silent_quic_peer(&hang, &release);
    let gonk = scratch_gonk::Gonk::start(&format!(
        "gonk.mount = \"prefer urn:llm:=quic://{addr} {}\"\n",
        certs.path().display()
    ));
    let socket = gonk.socket.clone();
    let started = Instant::now();
    let answer = bounded(Duration::from_secs(60), move || {
        ikigai_ipc::connect(&socket)
            .expect("gonk's socket")
            .issue(request(Verb::Meta, "urn:llm:ask"))
    });
    let elapsed = started.elapsed();
    release.store(true, Ordering::SeqCst);
    // A mount answers a failed `Meta` with a "contract unavailable" PLACEHOLDER that carries
    // the refusal (`MountedRemote`), so what reaches the client is gonk's answer, not an error.
    let why = match answer {
        Ok((placeholder, _)) => String::from_utf8_lossy(&placeholder.bytes).into_owned(),
        Err(Error::Timeout(why)) | Err(Error::Unavailable(why)) => why,
        other => panic!(
            "a silent peer's contract is a transient: {other:?}\ngonk's log:\n{}",
            gonk.log()
        ),
    };
    assert!(
        why.contains(&format!("quic://{addr}")) && why.contains("did not describe itself"),
        "GONK answered, naming the peer, not the client's own deadline blaming gonk \
         (after {elapsed:?}): {why}\ngonk's log:\n{}",
        gonk.log()
    );
    assert!(
        elapsed < ikigai_ipc::DEFAULT_DESCRIBE_TIMEOUT,
        "inside the caller's bound: {elapsed:?}"
    );
}
