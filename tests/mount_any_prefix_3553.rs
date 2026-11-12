//! Ledger #3553: a `gonk.mount` line mounts ANY prefix the config names, not only `urn:llm:`,
//! with the same `prefer` semantics, transports and describe deadline — and refuses, at
//! startup and naming the line, a prefix gonk serves itself.
//!
//! The peer is in process, on a scratch socket; the gonk is a scratch child with its QUIC door
//! off. Nothing touches a live service.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Exact, Invocation, Iri, Kernel, ReprType, Representation,
    Request, Verb,
};
use ikigai_resolve::Resolver;
use ikigai_vocab::TurtleRenderer;

mod ready;
mod scratch_gonk;

/// Answers a fixed text.
struct Says(&'static str);

#[async_trait]
impl Endpoint for Says {
    async fn invoke(&self, _: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(ReprType::new("text/plain"), self.0))
    }
    fn name(&self) -> &str {
        "says"
    }
    fn describe(&self) -> Description {
        Description::new("says").verb(Verb::Source)
    }
}

/// A scratch peer on `path` serving `urn:x-dash:3553:panel` under a made-up prefix.
fn socket_peer(path: &Path) {
    let space =
        EndpointSpace::new().bind(Exact::new("urn:x-dash:3553:panel"), Says("from the peer"));
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let serving = path.to_path_buf();
    std::thread::spawn(move || ikigai_ipc::serve(kernel, &serving));
    drop(ready::socket(path));
}

/// ★ The reproduction: before #3553 gonk refused to START with this line ("this server mounts
/// `urn:llm:` only"). Now the peer's resource answers through gonk's own socket door.
#[test]
fn a_peer_mounted_under_a_made_up_prefix_answers_through_gonks_door() {
    let short = tempfile::Builder::new()
        .prefix("mp")
        .tempdir_in("/tmp")
        .unwrap();
    let peer = short.path().join("peer.sock");
    socket_peer(&peer);
    let gonk = scratch_gonk::Gonk::start(&format!(
        "gonk.mount = \"prefer urn:x-dash:3553:={}\"\n",
        peer.display()
    ));
    let door = ikigai_ipc::connect(&gonk.socket).expect("gonk's socket");
    let (answer, _) = door
        .issue(Request::new(
            Verb::Source,
            Iri::parse("urn:x-dash:3553:panel").unwrap(),
        ))
        .unwrap_or_else(|e| panic!("the mounted peer answers: {e:?}\n{}", gonk.log()));
    assert_eq!(answer.bytes, b"from the peer");
    // gonk's own names still answer beside the mount: it claimed only its prefix.
    assert!(
        door.issue(Request::new(
            Verb::Source,
            Iri::parse("urn:iki:ledger:default:items").unwrap(),
        ))
        .is_ok(),
        "the ledger is still gonk's\n{}",
        gonk.log()
    );
}

/// A mount over one of gonk's own prefixes is refused at startup, naming the line and what
/// gonk serves there.
#[test]
fn a_mount_over_gonks_own_prefix_is_refused_at_startup_naming_the_line() {
    for (prefix, what) in [
        ("urn:iki:ledger:", "the ledger"),
        ("urn:iki:store:", "the store"),
        ("urn:repo:", "repository browse"),
        ("urn:iki:gonk:", "gonk's pages"),
        // Wider than one of gonk's own claims it too.
        ("urn:iki:", "the ledger"),
    ] {
        let line = format!("prefer {prefix}=/tmp/nobody-3553.sock");
        let refused = scratch_gonk::Gonk::try_start(&format!("gonk.mount = \"{line}\"\n"))
            .err()
            .unwrap_or_else(|| panic!("gonk started with `{line}`"));
        assert!(
            refused.contains(&format!("gonk.mount `{line}`"))
                && refused.contains(&format!("which gonk serves itself — {what}")),
            "names the line and what gonk serves there: {refused}"
        );
    }
}
