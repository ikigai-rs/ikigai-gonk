//! Ledger [#1576](http://localhost:1062/l/default/item/1576): only a forward that reaches a model
//! spends the mount's model-call window (`gonk.mount.calls_per_hour`).
//!
//! Through gonk 0.x every forward under `urn:llm:` counted, `urn:llm:config`, `urn:llm:models`
//! and `urn:llm:{provider}:model` included. On the cli pin of the time a peer's cacheable answer
//! was cached here, so those reads counted about once per process. From the cli release that
//! carries ikigai-rs/ikigai-cli PR 513 every mounted answer is `Expiry::Always` on this side, so
//! each read is a forward and each forward counted: browse reads `:model` on every explain and
//! every explain-status, so browsing ARCHIVED explanations would spend the window and then refuse
//! derives.
//!
//! The peer is a STUB `urn:llm` kernel served over `ikigai_ipc` on a short socket under `/tmp`,
//! mounted exactly as `gonk.mount` mounts the real one, and every answer it gives is uncacheable,
//! which is what the mount will hand this kernel after PR 513 for every answer. Nothing here
//! touches a live gonk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use ikigai_gonk::{compose_with, mount};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

mod ready;

/// One stub endpoint for every name: answers `ok`, uncacheable, and counts what reaches it.
struct Stub {
    id: &'static str,
    invoked: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for Stub {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.invoked.fetch_add(1, Ordering::SeqCst);
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"ok".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        self.id
    }
    fn describe(&self) -> Description {
        Description::new(self.id).verb(Verb::Source)
    }
}

/// The names `ikigai-llm` binds that answer without calling a model, for one provider.
const MODEL_FREE: [&str; 6] = [
    "urn:llm:config",
    "urn:llm:models",
    "urn:llm:select",
    "urn:llm:coder:model",
    "urn:llm:coder:up",
    "urn:llm:coder:installed",
];

/// The names that call one: the facade and a provider's own.
const MODEL: [&str; 2] = ["urn:llm:ask", "urn:llm:coder:ask"];

struct Peer {
    socket: PathBuf,
    invoked: Arc<AtomicUsize>,
    _dir: TempDir,
}

impl Peer {
    fn start() -> Peer {
        let dir = tempfile::Builder::new()
            .prefix("w6")
            .tempdir_in("/tmp")
            .expect("a short socket directory");
        let socket = dir.path().join("l.sock");
        let invoked = Arc::new(AtomicUsize::new(0));
        let mut space = EndpointSpace::new();
        for (iri, id) in MODEL_FREE
            .iter()
            .chain(MODEL.iter())
            .zip(["c", "m", "s", "cm", "cu", "ci", "a", "ca"])
        {
            space = space.bind(
                Exact::new(*iri),
                Stub {
                    id,
                    invoked: Arc::clone(&invoked),
                },
            );
        }
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let path = socket.clone();
        std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
        ready::socket(&socket);
        Peer {
            socket,
            invoked,
            _dir: dir,
        }
    }
}

/// gonk's kernel with the stub mounted under a window of `max` calls an hour.
fn hub(peer: &Peer, max: u32) -> Kernel {
    let line = format!("prefer urn:llm:={}", peer.socket.display());
    let mount = mount::parse(&line, std::path::Path::new("/home/nobody")).unwrap();
    compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        vec![mount::space(&mount, &ikigai_gonk::budget::model_calls(max))],
        Vec::new(),
        None,
        None,
        None,
    )
}

fn source(hub: &Kernel, iri: &str) -> Result<Representation, Error> {
    let holder = Capability::scoped(["urn:cap:net:localhost".to_string()]);
    block_on(hub.issue(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        &holder,
    ))
}

/// ★ Many reads that call no model leave a small window whole, and an ask still spends it.
#[test]
fn only_a_forward_that_reaches_a_model_spends_the_window() {
    let peer = Peer::start();
    let hub = hub(&peer, 2);

    // Fifty rounds of every model-free read: far past a window of two, and every one is a real
    // forward (the stub's answers are uncacheable), so the peer sees all of them.
    for _ in 0..50 {
        for iri in MODEL_FREE {
            let read = source(&hub, iri);
            assert_eq!(
                read.as_ref()
                    .map(|r| r.bytes.as_slice())
                    .map_err(|e| e.to_string()),
                Ok(b"ok".as_slice()),
                "{iri} is a read that calls no model; it must not spend the window"
            );
        }
    }
    assert_eq!(
        peer.invoked.load(Ordering::SeqCst),
        50 * MODEL_FREE.len(),
        "every read reached the peer: none was answered from a cache here"
    );

    // The window is still whole: both kinds of ask get through…
    for iri in MODEL {
        assert_eq!(source(&hub, iri).unwrap().bytes, b"ok", "{iri}");
    }
    // …and the third is refused, naming the window, without reaching the peer.
    let before = peer.invoked.load(Ordering::SeqCst);
    for iri in MODEL {
        let over = source(&hub, iri).unwrap_err().to_string();
        assert!(
            over.contains("rate-limited") && over.contains(ikigai_gonk::budget::MODEL_CALLS_IRI),
            "{iri}: {over}"
        );
    }
    assert_eq!(
        peer.invoked.load(Ordering::SeqCst),
        before,
        "a refused ask is not forwarded"
    );

    // A model-free read still answers with the window spent: it was never part of it.
    for iri in MODEL_FREE {
        assert_eq!(source(&hub, iri).unwrap().bytes, b"ok", "{iri}");
    }
}
