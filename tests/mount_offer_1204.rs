//! Ledger [#1204](http://localhost:1062/l/default/item/1204): the `urn:llm:` mount offers what it
//! then refuses.
//!
//! The mount enforces `urn:cap:net:{host}` on every call it forwards
//! ([`ikigai_gonk::mount::net_grant`], ledger #805), but the manifold offered each mounted
//! resource under the PEER's own declaration: `urn:llm:config` declares nothing (as
//! `ikigai-llm`'s does), so an anonymous caller was offered it and then denied; and
//! `urn:llm:ask` declares the offering wildcard `urn:cap:net:*`, so a grant naming ANOTHER host
//! was offered it and then denied. The manifold must say what enforcement does.
//!
//! The peer is a STUB `urn:llm` kernel served over `ikigai_ipc` on a short socket under `/tmp`,
//! mounted exactly as `gonk.mount` mounts the real one. Nothing here touches a live gonk.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ActionQuery, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact,
    Invocation, Iri, Kernel, ReprType, Representation, Request, Verb,
};
use ikigai_gonk::grants::anonymous_http_grants;
use ikigai_gonk::{compose_with, mount};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

mod ready;

/// `urn:llm:ask`, declaring the offering wildcard, as `ikigai-llm`'s does.
struct StubAsk;

#[async_trait]
impl Endpoint for StubAsk {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"stub".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "stub-ask"
    }
    fn describe(&self) -> Description {
        Description::new("stub-ask")
            .verb(Verb::Source)
            .requires("urn:cap:net:*")
            .input(ArgSpec::new("prompt").optional())
    }
}

/// `urn:llm:config`, declaring nothing, as `ikigai-llm`'s does.
struct StubConfig;

#[async_trait]
impl Endpoint for StubConfig {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            b"{}".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "stub-config"
    }
    fn describe(&self) -> Description {
        Description::new("stub-config").verb(Verb::Source)
    }
}

struct Peer {
    socket: PathBuf,
    _dir: TempDir,
}

impl Peer {
    fn start() -> Peer {
        let dir = tempfile::Builder::new()
            .prefix("m4")
            .tempdir_in("/tmp")
            .expect("a short socket directory");
        let socket = dir.path().join("l.sock");
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:llm:ask"), StubAsk)
            .bind(Exact::new("urn:llm:config"), StubConfig);
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
        let path = socket.clone();
        std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
        ready::socket(&socket);
        Peer { socket, _dir: dir }
    }
}

fn hub(peer: &Peer) -> Kernel {
    let line = format!("prefer urn:llm:={}", peer.socket.display());
    let mount = mount::parse(&line, std::path::Path::new("/home/nobody")).unwrap();
    compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        vec![mount::space(&mount)],
        Vec::new(),
        None,
        None,
        None,
    )
}

/// The `urn:llm:` rows `who` is offered.
fn offered(hub: &Kernel, who: &Capability) -> Vec<String> {
    let query = ActionQuery {
        capability: Some(who),
        ..Default::default()
    };
    hub.select_actions(&query)
        .into_iter()
        .map(|m| m.endpoint)
        .filter(|endpoint| endpoint.starts_with("urn:llm:"))
        .collect()
}

fn source(hub: &Kernel, who: &Capability, iri: &str) -> Result<Representation, Error> {
    block_on(hub.issue(Request::new(Verb::Source, Iri::parse(iri).unwrap()), who))
}

/// ★ Offered exactly when served: every mounted row a caller is offered, it may read, and every
/// one it is refused, it is not offered.
#[test]
fn the_mount_offers_exactly_what_it_serves() {
    let peer = Peer::start();
    let hub = hub(&peer);
    let anonymous = Capability::scoped(anonymous_http_grants(&["default".to_string()]).unwrap());
    let elsewhere = Capability::scoped(["urn:cap:net:127.0.0.1".to_string()]);
    let here = Capability::scoped(["urn:cap:net:localhost".to_string()]);

    for (who, name) in [(&anonymous, "anonymous"), (&elsewhere, "another host")] {
        for iri in ["urn:llm:config", "urn:llm:ask"] {
            match source(&hub, who, iri) {
                Err(Error::Denied(_)) => {}
                other => panic!("{name} {iri}: {other:?}"),
            }
        }
        assert_eq!(
            offered(&hub, who),
            Vec::<String>::new(),
            "{name} is offered nothing the mount refuses"
        );
    }

    let mut rows = offered(&hub, &here);
    rows.sort();
    assert_eq!(rows, ["urn:llm:ask", "urn:llm:config"]);
    for iri in ["urn:llm:config", "urn:llm:ask"] {
        source(&hub, &here, iri).unwrap_or_else(|e| panic!("{iri}: {e}"));
    }
}
