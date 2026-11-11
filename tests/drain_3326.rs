//! Ledger [#3326](http://localhost:1062/l/default/item/3326): SIGTERM and SIGINT drain the QUIC
//! door and exit 0, so a stop no longer ends the calls in flight.
//!
//! Every test runs the real binary in a child process and signals it. The write in progress is
//! a real one: `urn:nl:sparql` saves a draft over the QUIC door, and it waits on a stub
//! `urn:llm:` peer this test holds, so the write is known to be in progress at the signal and
//! ends when the test says, with no race against a clock.

use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use ikigai_gonk::grants::{grants_for, nl_draft_grants, script_grants, Authority, ScriptAuthority};
use ikigai_gonk::quic::{self, DRAIN_CAP};
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

mod ready;
mod scratch_gonk;

use scratch_gonk::Gonk;

/// Room over [`DRAIN_CAP`] for a loaded machine to schedule the exit and for this test to see it.
const EXIT_SLACK: Duration = Duration::from_secs(1);

/// The bound on anything this test waits for that is not under test.
const PATIENCE: Duration = Duration::from_secs(60);

/// What the stub drafts: any query that parses and reads a graph the caller may read.
const GOOD: &str = "SELECT ?number \
    FROM <urn:iki:ledger:graph:default> \
    WHERE { ?item <https://ikigai-rs.dev/ns/ledger#number> ?number }";

/// `urn:llm:ask`: says when an ask has arrived, then holds it until the test releases it.
struct HeldAsk {
    arrived: mpsc::Sender<()>,
    release: Arc<AtomicBool>,
}

#[async_trait]
impl Endpoint for HeldAsk {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let _ = self.arrived.send(());
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let body = serde_json::json!({
            "text": format!("```sparql\n{GOOD}\n```\n"),
            "model": "stub-sparql",
        });
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
    }
    fn name(&self) -> &str {
        "held-ask"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("held-ask")
            .verb(Verb::Source)
            .requires("urn:cap:net:*")
            .input(optional("prompt"))
            .input(optional("as"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

/// `urn:llm:config`: capability-free, as `ikigai-llm`'s is.
struct StubConfig;

#[async_trait]
impl Endpoint for StubConfig {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            br#"{"default":"stub","providers":{}}"#.to_vec(),
        )
        .cacheable())
    }
    fn name(&self) -> &str {
        "stub-config"
    }
    fn describe(&self) -> Description {
        Description::new("stub-config").verb(Verb::Source)
    }
}

/// The stub model, on a socket under a short `/tmp` directory (`sun_path` is short).
struct Peer {
    arrived: Receiver<()>,
    release: Arc<AtomicBool>,
    socket: PathBuf,
    _dir: TempDir,
}

impl Peer {
    fn start() -> Peer {
        let dir = tempfile::Builder::new()
            .prefix("d3")
            .tempdir_in("/tmp")
            .expect("a short socket directory");
        let socket = dir.path().join("l.sock");
        let (arrived, arrivals) = mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:llm:ask"),
                HeldAsk {
                    arrived,
                    release: Arc::clone(&release),
                },
            )
            .bind(Exact::new("urn:llm:config"), StubConfig);
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let path = socket.clone();
        std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
        ready::socket(&socket);
        Peer {
            arrived: arrivals,
            release,
            socket,
            _dir: dir,
        }
    }

    fn config(&self) -> String {
        format!(
            "gonk.mount = \"prefer urn:llm:={}\"\n",
            self.socket.display()
        )
    }

    fn release(&self) {
        self.release.store(true, Ordering::SeqCst);
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A gonk whose client `alpha` may draft and save the script `drained`, over QUIC.
fn drafting_gonk(peer: &Peer) -> Gonk {
    let gonk = Gonk::with_quic(&peer.config());
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.extend(nl_draft_grants(Some("localhost")).unwrap());
    scopes.extend(script_grants("drained", ScriptAuthority::Publish).unwrap());
    quic::enrol(
        &gonk.layout,
        "alpha",
        &gonk.client.fingerprint,
        &scopes,
        true,
    )
    .expect("re-enroll alpha");
    gonk
}

/// The draft call, begun on its own thread over a connection of its own; the handle answers
/// with the call's result.
fn begin_write(gonk: &Gonk) -> std::thread::JoinHandle<Result<Representation, Error>> {
    let client = gonk.dial_quic().expect("QUIC connect");
    std::thread::spawn(move || {
        let write = request(
            Verb::Sink,
            "urn:nl:sparql",
            &[
                ("ask", "the item numbers"),
                ("save", "drained"),
                ("as", "application/json"),
            ],
        );
        client.issue(write).map(|(repr, _)| repr)
    })
}

fn signal(gonk: &Gonk, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(gonk.child.id().to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name}");
}

/// Wait until the child's log says it has begun to drain. A child that dies first fails here.
fn until_draining(gonk: &mut Gonk) {
    let deadline = Instant::now() + PATIENCE;
    while !gonk.log().contains("draining the QUIC door") {
        if let Some(status) = gonk.child.try_wait().unwrap() {
            panic!("gonk stopped without draining ({status}):\n{}", gonk.log());
        }
        assert!(Instant::now() < deadline, "no drain:\n{}", gonk.log());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for the child to exit; how it exited and how long after `since`.
fn exit_of(gonk: &mut Gonk, since: Instant) -> (ExitStatus, Duration) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = gonk.child.try_wait().unwrap() {
            return (status, since.elapsed());
        }
        assert!(Instant::now() < deadline, "no exit:\n{}", gonk.log());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the newest log segment ends as an orderly stop: its last entry is `log:ProcessStop`
/// (ledger #383), written by `access::Record::close`. A process that exits without closing
/// the segment leaves it reading as a crash (ledger #3414).
fn ends_with_a_stop(gonk: &Gonk) -> bool {
    let segments = gonk.segments();
    let last = segments
        .lines()
        .rev()
        // An entry starts with its time; `segments()` also reads the instance's `.lock`,
        // which holds a pid, and the `#seal` lines come after the last entry.
        .find(|l| {
            l.split(' ')
                .next()
                .is_some_and(|t| t.contains('T') && t.ends_with('Z'))
        })
        .unwrap_or("");
    let class = last.split(' ').nth(1).unwrap_or("");
    class == "log:ProcessStop" || class == ikigai_log::PROCESS_STOP_CLASS
}

/// ★ The item's first requirement: the write in progress at SIGTERM finishes, its client sees
/// the reply, the write is there after a restart, and the process exits 0 within the cap.
#[test]
fn a_quic_write_in_progress_at_sigterm_completes_and_gonk_exits_zero() {
    let peer = Peer::start();
    let mut gonk = drafting_gonk(&peer);
    let write = begin_write(&gonk);
    peer.arrived
        .recv_timeout(PATIENCE)
        .expect("the write reached the model");

    let stopped = Instant::now();
    signal(&gonk, "TERM");
    until_draining(&mut gonk);
    peer.release();

    let reply = write.join().unwrap().expect("the client sees the reply");
    let reply: serde_json::Value = serde_json::from_slice(&reply.bytes).unwrap();
    assert_eq!(reply["status"], "saved", "{reply}");

    let (status, took) = exit_of(&mut gonk, stopped);
    assert_eq!(status.code(), Some(0), "{status}:\n{}", gonk.log());
    assert!(took < DRAIN_CAP + EXIT_SLACK, "took {took:?}");
    assert!(ends_with_a_stop(&gonk), "{}", gonk.segments());

    gonk.restart();
    let saved = gonk.dial_quic().expect("QUIC connect").issue(request(
        Verb::Source,
        "urn:script:drained",
        &[],
    ));
    saved.expect("the write the client saw acknowledged is still there");
}

/// A call still running at the cap does not hold the process: it exits 0 at the cap, says how
/// many connections it cut off, and that client sees an error rather than hanging.
#[test]
fn a_call_still_running_at_the_cap_is_cut_off_and_gonk_exits_zero() {
    let peer = Peer::start();
    let mut gonk = drafting_gonk(&peer);
    let write = begin_write(&gonk);
    peer.arrived
        .recv_timeout(PATIENCE)
        .expect("the write reached the model");

    let stopped = Instant::now();
    signal(&gonk, "TERM");
    let (status, took) = exit_of(&mut gonk, stopped);
    peer.release();
    assert_eq!(status.code(), Some(0), "{status}:\n{}", gonk.log());
    assert!(took < DRAIN_CAP + EXIT_SLACK, "took {took:?}");
    assert!(
        gonk.log().contains("cut off 1 connection"),
        "{}",
        gonk.log()
    );
    write
        .join()
        .unwrap()
        .expect_err("a call cut off at the cap is no success");
}

/// A second signal during the drain does not wait for the call or the cap.
#[test]
fn a_second_signal_during_the_drain_exits_at_once() {
    let peer = Peer::start();
    let mut gonk = drafting_gonk(&peer);
    let _write = begin_write(&gonk);
    peer.arrived
        .recv_timeout(PATIENCE)
        .expect("the write reached the model");

    signal(&gonk, "TERM");
    until_draining(&mut gonk);
    let again = Instant::now();
    signal(&gonk, "INT");
    let (status, took) = exit_of(&mut gonk, again);
    peer.release();
    assert_eq!(status.code(), Some(128 + 2), "{status}:\n{}", gonk.log());
    assert!(
        took < DRAIN_CAP,
        "the second signal waited for the drain: {took:?}"
    );
    // Stopped as asked, so the segment says so (ledger #3414): through a2af1ec this exit
    // skipped `log:ProcessStop` and the segment read as a crash.
    assert!(ends_with_a_stop(&gonk), "{}", gonk.segments());
}

/// SIGINT drains exactly as SIGTERM does.
#[test]
fn sigint_drains_an_idle_door_and_exits_zero() {
    let mut gonk = Gonk::with_quic("");
    let stopped = Instant::now();
    signal(&gonk, "INT");
    let (status, took) = exit_of(&mut gonk, stopped);
    assert_eq!(status.code(), Some(0), "{status}:\n{}", gonk.log());
    assert!(took < DRAIN_CAP + EXIT_SLACK, "took {took:?}");
    assert!(
        gonk.log().contains("draining the QUIC door"),
        "{}",
        gonk.log()
    );
    assert!(ends_with_a_stop(&gonk), "{}", gonk.segments());
}

/// With no QUIC door there is nothing to drain, and a stop is still a clean one.
#[test]
fn a_gonk_with_no_quic_door_exits_zero_on_sigterm() {
    let mut gonk = Gonk::start("");
    let stopped = Instant::now();
    signal(&gonk, "TERM");
    let (status, took) = exit_of(&mut gonk, stopped);
    assert_eq!(status.code(), Some(0), "{status}:\n{}", gonk.log());
    assert!(took < DRAIN_CAP, "took {took:?}");
    assert!(ends_with_a_stop(&gonk), "{}", gonk.segments());
}

/// The drain waits for calls, not work: a claim stands through the stop exactly as it was, with
/// its holder and its lease, and nothing re-queues it.
#[test]
fn a_claim_keeps_its_holder_and_lease_across_a_drain_and_restart() {
    let mut gonk = Gonk::with_quic("");
    let socket = |gonk: &Gonk, request: Request| {
        ikigai_ipc::connect(&gonk.socket)
            .expect("the socket")
            .issue(request)
            .map(|(repr, _)| repr)
    };
    socket(
        &gonk,
        request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", "Hold me through a restart")],
        ),
    )
    .expect("append");
    socket(
        &gonk,
        request(
            Verb::Sink,
            "urn:iki:ledger:claim",
            &[("item", "1"), ("content", "w1"), ("lease", "1h")],
        ),
    )
    .expect("claim");
    let claim = |gonk: &Gonk| -> serde_json::Value {
        let read = socket(
            gonk,
            request(
                Verb::Source,
                "urn:iki:ledger:item:1",
                &[("as", "application/json")],
            ),
        )
        .expect("the item");
        let doc: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        doc["item"]["claim"].clone()
    };
    let before = claim(&gonk);
    assert_eq!(before["holder"].as_str(), Some("w1"), "{before}");
    assert_eq!(before["lease"].as_str(), Some("PT1H"), "{before}");
    assert!(before["expires"].is_string(), "{before}");

    let stopped = Instant::now();
    signal(&gonk, "TERM");
    let (status, took) = exit_of(&mut gonk, stopped);
    assert_eq!(status.code(), Some(0), "{status}:\n{}", gonk.log());
    assert!(took < DRAIN_CAP + EXIT_SLACK, "took {took:?}");
    assert!(ends_with_a_stop(&gonk), "{}", gonk.segments());

    gonk.restart();
    assert_eq!(claim(&gonk), before, "the claim moved across the restart");
}
