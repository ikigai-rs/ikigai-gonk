//! Ledger [#1287](http://localhost:1060/l/default/item/1287) (the root cause of
//! [#1262](http://localhost:1060/l/default/item/1262)): a REAL gonk that mounts `urn:llm:` on a
//! QUIC peer keeps answering when that peer RESTARTS on the same port.
//!
//! The incident, 2026-10-11 on plasma: the inference peer gonk mounts was restarted by
//! `just reregister`, and from then on every `urn:llm:` request through gonk hung ("did not
//! describe itself within 30s") until gonk itself was restarted. gonk's lock pinned
//! `ikigai-quic` 0.1.44, from before cli 0.1.46 (ikigai-rs/ikigai-cli PR 442, ledger
//! [#1092](http://localhost:1060/l/default/item/1092)).
//!
//! The shape is ikigai-cli's `crates/ikigai-cli/tests/quic_redial.rs` (ikigai-rs/ikigai-cli PR
//! 457), with gonk as the host: the gonk binary in a child process over a scratch config home
//! (`scratch_gonk`), its `gonk.mount` pointed at a stub `urn:llm:` peer, and the test asking
//! gonk over its owner-only socket. The peer is THIS test binary re-executed as a child
//! ([`peer_process`]), so it can be killed outright (no close frame, as a launchd restart
//! kills it) and replaced on the SAME port by number with the SAME identity.
//!
//! Why this heals, so a failure here can be read (the cli test's account, for this graph):
//! the peer derives its stateless-reset key and its connection-ID key from its identity, so
//! the replacement resets the connection its predecessor held; gonk's next round trip fails
//! at once, and `QuicResolver` redials and replays the read. Both halves come from the
//! `ikigai-quic` this crate resolves, the server half in the peer and the client half in gonk.
//!
//! Measured on this test before the move: on `ikigai-quic` 0.1.44 (both halves), the ask after
//! the restart had NO answer for 300 s, when the socket client's own deadline gave up ("no
//! response from the kernel server"): gonk was still holding the dead connection. On 0.1.52
//! the whole test, gonk's startup and both peers' included, takes about a second.
//!
//! Nothing here touches a live service: the peer binds `127.0.0.1:0`, and gonk binds HTTP
//! port 0 with its QUIC door off.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;

mod scratch_gonk;

use scratch_gonk::Gonk;

/// How long a peer gets to print its banner. A watchdog, not a performance assertion.
const PATIENCE: Duration = Duration::from_secs(60);

/// The bound on the healed request. Generous for a loaded CI runner; what it rules out is the
/// incident: a describe deadline (30 s) or the idle timeout (300 s) paid because gonk kept the
/// dead connection.
const HEALED_WITHIN: Duration = Duration::from_secs(20);

/// The environment that turns [`peer_process`] from a no-op into the peer.
const PEER_DIR: &str = "GONK_1287_PEER_DIR";
const PEER_BIND: &str = "GONK_1287_PEER_BIND";
const PEER_NAME: &str = "GONK_1287_PEER_NAME";

// ------------------------------------------------------------------ the peer (a child process)

/// `urn:llm:ask` answering `{peer name}:{prompt}`, so the test can tell WHICH peer answered and
/// no answer can be a cached copy of another (each ask carries its own prompt).
struct NamedAsk {
    name: String,
}

#[async_trait]
impl Endpoint for NamedAsk {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let prompt = match inv.request.args.get("prompt") {
            Some(ArgRef::Inline(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
            _ => String::new(),
        };
        Ok(Representation::new(
            ReprType::new("text/plain"),
            format!("{}:{prompt}", self.name),
        ))
    }
    fn name(&self) -> &str {
        "named-ask"
    }
    fn describe(&self) -> Description {
        // What `ikigai-llm`'s ask declares that matters to the mount: a net requirement.
        Description::new("named-ask")
            .verb(Verb::Source)
            .requires("urn:cap:net:*")
            .input(ArgSpec::new("prompt").optional())
    }
}

/// The peer, when this binary is re-executed with [`PEER_DIR`] set: serve `urn:llm:ask` over
/// QUIC on [`PEER_BIND`] with the identity in [`PEER_DIR`], print `serving on <addr>` to
/// stderr, and serve until killed. Without the environment (any ordinary `--ignored` run) it
/// returns at once.
#[test]
#[ignore = "the peer half of this file's test, run only as its child process"]
fn peer_process() {
    let (Ok(dir), Ok(bind), Ok(name)) = (
        std::env::var(PEER_DIR),
        std::env::var(PEER_BIND),
        std::env::var(PEER_NAME),
    ) else {
        return;
    };
    let dir = Path::new(&dir);
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).expect("a peer file");
    let identity = ikigai_quic::Identity {
        cert_pem: read("server.crt"),
        key_pem: read("server.key"),
    };
    let client = read("client.crt");
    let space = EndpointSpace::new().bind(Exact::new("urn:llm:ask"), NamedAsk { name });
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let minter: ikigai_quic::Minter = Arc::new(|_| {
        Some(ikigai_quic::Session {
            capability: Capability::root(),
            file_segment: "gonk".to_string(),
            principal: None,
        })
    });
    let addr: SocketAddr = bind.parse().expect("a bind address");
    // ★ Bound BEFORE the banner (ledger #1188), so `127.0.0.1:0` names the port the kernel
    // chose, and the replacement binds that port by number. A taken port fails here, before
    // any banner, which is what [`peer`] reads as "try again".
    let socket = match ikigai_quic::bind(addr) {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!("the peer could not bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "serving on {}",
        socket.local_addr().expect("a bound address")
    );
    let served = ikigai_quic::serve_bounded_on(
        kernel,
        socket,
        &identity,
        &[client],
        minter,
        ikigai_quic::DEFAULT_IDLE_TIMEOUT,
        None,
    );
    eprintln!("the peer stopped: {served:?}");
    std::process::exit(1);
}

/// A peer child, killed and reaped when dropped.
struct Peer(Child);

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start a peer named `name` on `bind`; `Ok` with the address its banner names, or `Err` with
/// what it printed when it died first (a replacement that found the port still held).
fn peer(dir: &Path, bind: &str, name: &str) -> Result<(Peer, SocketAddr), String> {
    let mut child = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "peer_process",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(PEER_DIR, dir)
        .env(PEER_BIND, bind)
        .env(PEER_NAME, name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the peer starts");
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    // Keep draining after the banner, so the peer's stderr is a pipe someone reads.
    std::thread::spawn(move || {
        let mut seen = String::new();
        let mut lines = BufReader::new(stderr).lines().map_while(Result::ok);
        for line in lines.by_ref() {
            seen.push_str(&line);
            seen.push('\n');
            if line.contains("serving on") {
                break;
            }
        }
        let _ = tx.send(seen);
        lines.for_each(drop);
    });
    let peer = Peer(child);
    let seen = rx
        .recv_timeout(PATIENCE)
        .unwrap_or_else(|e| panic!("the peer `{name}` never printed its banner: {e}"));
    match seen
        .lines()
        .find_map(|line| line.split("serving on ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|addr| addr.parse().ok())
    {
        Some(addr) => Ok((peer, addr)),
        None => Err(seen),
    }
}

// ------------------------------------------------------------------ the host (a real gonk)

/// Ask gonk `urn:llm:ask prompt=<prompt>` over its socket, bounded by `within`: the answer,
/// or `Err` naming the error or the bound.
fn ask(gonk: &Gonk, prompt: &str, within: Duration) -> Result<String, String> {
    let socket = gonk.socket.clone();
    let request = Request::new(Verb::Source, Iri::parse("urn:llm:ask").unwrap())
        .with_arg("prompt", ArgRef::Inline(prompt.as_bytes().to_vec()));
    let (tx, rx) = mpsc::channel();
    // The bound is the test's, never the transport's: a hung call is the failure under test.
    std::thread::spawn(move || {
        let answer = ikigai_ipc::connect(&socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))
            .and_then(|client| client.issue(request))
            .map(|(repr, _)| String::from_utf8_lossy(&repr.bytes).into_owned())
            .map_err(|e| format!("{e:?}"));
        let _ = tx.send(answer);
    });
    rx.recv_timeout(within)
        .unwrap_or_else(|_| Err(format!("no answer within {within:?}")))
}

/// ★ The incident, through gonk: a mounted peer answers, is killed and replaced on the SAME
/// port, and gonk's next `urn:llm:` request answers from the NEW peer, promptly.
#[test]
fn a_restarted_llm_peer_is_redialled_by_gonk() {
    // Both identities, laid out as `gonk.mount`'s certificate directory wants them (the
    // client.crt/client.key gonk presents, the server.crt it pins), plus the peer's key.
    let certs = tempfile::tempdir().expect("a certificate directory");
    let server = ikigai_quic::generate();
    let client = ikigai_quic::generate();
    for (file, pem) in [
        ("server.crt", &server.cert_pem),
        ("server.key", &server.key_pem),
        ("client.crt", &client.cert_pem),
        ("client.key", &client.key_pem),
    ] {
        std::fs::write(certs.path().join(file), pem).unwrap();
    }

    let (first, address) =
        peer(certs.path(), "127.0.0.1:0", "first").expect("the first peer serves");
    assert_ne!(address.port(), 0, "the banner names the BOUND port");

    let gonk = Gonk::start(&format!(
        "gonk.mount = \"prefer urn:llm:=quic://{address} {}\"\n",
        certs.path().display()
    ));
    let before = ask(&gonk, "one", PATIENCE).expect("the first ask");
    assert_eq!(before, "first:one", "the mount reaches the peer");

    // Kill the peer outright (SIGKILL: no close frame), then serve again on the SAME port with
    // the SAME identity. The kill and the wait are synchronous, so the port is free; a bounded
    // retry covers a platform that releases it a beat late.
    drop(first);
    let mut tries = 0;
    let _second = loop {
        match peer(certs.path(), &address.to_string(), "second") {
            Ok((door, _)) => break door,
            Err(_) if tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(seen) => panic!("the replacement never served on {address}:\n{seen}"),
        }
    };

    // Native-only: a test of the binary over a real QUIC door, and elapsed wall time is the
    // measurement. Bound to a `let` so the opt-out covers this call alone.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let after = ask(&gonk, "two", HEALED_WITHIN);
    let elapsed = start.elapsed();
    assert_eq!(
        after.as_deref(),
        Ok("second:two"),
        "gonk redialled the NEW peer (after {elapsed:?}); gonk's log:\n{}",
        gonk.log()
    );
    assert!(
        elapsed < HEALED_WITHIN,
        "healed by a reset and a redial, not a deadline: {elapsed:?}"
    );
}
