//! Ledger [#1203](http://localhost:1062/l/default/item/1203): one draft at `urn:nl:sparql` makes
//! up to three model calls, and nothing bounded them together: each was bounded only by the
//! mount's per-call deadline (300 s), so a draft through the HTTP door, which has no request
//! deadline, could hold its request for fifteen minutes.
//!
//! A STUB `urn:llm:ask` peer on a short socket under `/tmp`, mounted as `gonk.mount` mounts the
//! real one, that answers every ask SLOWLY with a draft that never parses, so the drafter makes
//! every call it may. Nothing here touches a live gonk or a real model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use ikigai_gonk::nl::Nl;
use ikigai_gonk::script::Scripts;
use ikigai_gonk::{compose_with, mount};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

mod ready;

/// How long the stub takes to answer its FIRST ask.
const SLOW: Duration = Duration::from_millis(800);

/// How long it takes to answer every ask after the first: far past any deadline here, so a call
/// that is not cut at what is left of the draft's deadline shows as this, never as a few
/// hundred milliseconds a loaded machine could also cost.
const LATER: Duration = Duration::from_secs(30);

struct SlowAsk {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for SlowAsk {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::sleep(if n == 1 { SLOW } else { LATER });
        let body = serde_json::json!({
            "text": format!("```sparql\nSELECT ?x{n} WHERE {{ ?x{n} ?p }}\n```"),
            "model": "stub-slow",
        });
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(&body).unwrap(),
        ))
    }
    fn name(&self) -> &str {
        "stub-ask"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("stub-ask")
            .verb(Verb::Source)
            .requires("urn:cap:net:*")
            .input(optional("prompt"))
            .input(optional("as"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

struct Peer {
    calls: Arc<AtomicUsize>,
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
        let calls = Arc::new(AtomicUsize::new(0));
        let space = EndpointSpace::new().bind(
            Exact::new("urn:llm:ask"),
            SlowAsk {
                calls: Arc::clone(&calls),
            },
        );
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let path = socket.clone();
        std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
        ready::socket(&socket);
        Peer {
            calls,
            socket,
            _dir: dir,
        }
    }
}

fn hub(peer: &Peer, nl: impl Fn(&str) -> Nl) -> Arc<Kernel> {
    let line = format!("prefer urn:llm:={}", peer.socket.display());
    let mount = mount::parse(&line, std::path::Path::new("/home/nobody")).unwrap();
    let nl = nl(&mount.target.net_host());
    let scripts = Scripts::in_memory();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        vec![mount::space(
            &mount,
            &ikigai_gonk::budget::model_calls(ikigai_gonk::budget::DEFAULT_MODEL_CALLS_PER_HOUR),
        )],
        Vec::new(),
        None,
        Some(&scripts),
        Some(&nl),
    ));
    scripts.attach(&hub);
    hub
}

fn draft(hub: &Kernel) -> Result<Representation, Error> {
    let request = Request::new(Verb::Sink, Iri::parse("urn:nl:sparql").unwrap())
        .with_arg("content", ArgRef::Inline(b"anything at all".to_vec()));
    block_on(hub.issue(request, &Capability::root()))
}

/// ★ The draft's model calls share ONE deadline: a hopeless draft whose first call takes 0.8 s
/// is cut at 1.2 s, its second call bounded by what was left and its third never sent, where it
/// used to make all three (2.4 s at 0.8 s each).
///
/// ★ Ledger #1730: the second call used to take 0.8 s too, and the bound was `took < 2 s`. A
/// mount that did not cut the second call at what was left of the deadline took 1.6 s and
/// PASSED, and a loaded machine had only 0.8 s of slack. Now every call after the first takes
/// [`LATER`], so "cut" is a matter of seconds against half a minute. And it is only judged when
/// the second call was made at all: on a machine so loaded that the FIRST call ran out the
/// deadline, nothing is said about the second, and the deadline is doubled.
#[test]
fn a_drafts_model_calls_share_one_deadline() {
    let deadlines = [1_200, 2_400, 4_800].map(Duration::from_millis);
    for deadline in deadlines {
        let peer = Peer::start();
        let hub = hub(&peer, |host| {
            Nl::with_draft_deadline(
                ikigai_gonk::nl::DEFAULT_ASK,
                Some(host),
                Default::default(),
                deadline,
            )
        });
        let started = Instant::now();
        let answer = draft(&hub);
        let took = started.elapsed();
        match answer {
            Err(Error::Timeout(_)) => {}
            other => panic!("refused as a timeout: {other:?}"),
        }
        let calls = peer.calls.load(Ordering::SeqCst);
        assert!(
            calls <= 2,
            "no call starts past the deadline: {calls} calls"
        );
        if calls < 2 {
            println!("{deadline:?}: the first call ran out the deadline ({took:?}); trying longer");
            continue;
        }
        assert!(
            took < LATER / 2,
            "the second call was cut at what was left of the {deadline:?} deadline, not run to \
             its end: {took:?}"
        );
        return;
    }
    panic!("no deadline in {deadlines:?} let a first call of {SLOW:?} finish");
}
