//! Review of PR 426: the door's self-description and refusal wording after the floor moved.
use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, EndpointSpace, Iri, Kernel, Request, Verb};
use ikigai_gonk::doors;

struct RepoStatus;

#[async_trait::async_trait]
impl ikigai_core::Endpoint for RepoStatus {
    async fn invoke(
        &self,
        _inv: &ikigai_core::Invocation<'_>,
    ) -> ikigai_core::Result<ikigai_core::Representation> {
        Ok(ikigai_core::Representation::new(
            ikigai_core::ReprType::new("text/plain"),
            "clean\n",
        ))
    }
    fn name(&self) -> &str {
        "repo-status"
    }
    fn describe(&self) -> ikigai_core::Description {
        ikigai_core::Description::new(self.name())
            .verb(Verb::Source)
            .requires("urn:cap:exec:read:git")
    }
}

fn repo_hub() -> Arc<Kernel> {
    let space = EndpointSpace::new().bind(ikigai_core::Exact::new("urn:repo:status"), RepoStatus);
    Arc::new(Kernel::new(Arc::new(space)))
}

fn explain(k: &Kernel) -> String {
    let req = Request::new(Verb::Source, Iri::parse("urn:kernel:explain").unwrap())
        .with_arg("target", ArgRef::Inline(b"urn:repo:status".to_vec()))
        .with_arg("scopes", ArgRef::Inline(b"urn:cap:exec:git".to_vec()));
    let rep = futures::executor::block_on(k.issue(req, &Capability::root())).expect("explains");
    String::from_utf8(rep.bytes.to_vec()).unwrap()
}

/// `urn:kernel:explain` promises "the scopes a real request would be Denied for". At a door
/// it must say what the hub's floor will refuse, as the hub's own explanation does.
#[test]
#[ignore = "NOT FIXED (finding 1): core 0.4.2's explain and its floor read the same describe() of \
            the same resolution, so the door cannot keep `requires` for one and not the other"]
fn explain_at_the_door_names_what_the_hub_floor_refuses() {
    let hub = repo_hub();
    let door = doors::door_kernel(Arc::clone(&hub));
    let at_hub = explain(&hub);
    let at_door = explain(&door);
    eprintln!("hub:\n{at_hub}\ndoor:\n{at_door}");
    assert!(at_hub.contains("urn:cap:exec:read:git"), "{at_hub}");
    assert!(
        at_door.contains("urn:cap:exec:read:git"),
        "door explanation lost the floor:\n{at_door}"
    );
}

/// The PR claimed the refusal sentence is the kernel's word for word, for every caller. An
/// HTTP caller navigated from another page carries the foreign-page marker, and gets the
/// kernel's sentence followed by the foreign-page sentence: that new text is pinned here.
#[test]
fn a_foreign_page_floor_refusal_keeps_the_kernels_sentence() {
    let hub = repo_hub();
    let door = doors::http_kernel_with(Arc::clone(&hub), EndpointSpace::new(), None);
    let cap = Capability::scoped([
        "urn:cap:exec:git".to_string(),
        ikigai_gonk::admit::FOREIGN_PAGE.to_string(),
    ]);
    let ask = || Request::new(Verb::Source, Iri::parse("urn:repo:status").unwrap());
    let own = futures::executor::block_on(hub.issue(ask(), &cap)).expect_err("refused");
    let got = futures::executor::block_on(door.issue(ask(), &cap)).expect_err("refused");
    eprintln!("hub: {own}\ndoor: {got}");
    // The new wording is kept (hub review of PR 426, finding 2): the kernel's sentence, then
    // the foreign-page sentence, as a derivation refused under the marker has always read.
    assert_eq!(
        got.to_string(),
        format!("{own}. {}", ikigai_gonk::admit::FOREIGN_PAGE_SENTENCE)
    );
    assert_eq!(got.lacking(), own.lacking());
}

struct Write;

#[async_trait::async_trait]
impl ikigai_core::Endpoint for Write {
    async fn invoke(
        &self,
        _inv: &ikigai_core::Invocation<'_>,
    ) -> ikigai_core::Result<ikigai_core::Representation> {
        Ok(ikigai_core::Representation::new(
            ikigai_core::ReprType::new("text/plain"),
            "written\n",
        ))
    }
    fn name(&self) -> &str {
        "repo-write"
    }
    fn describe(&self) -> ikigai_core::Description {
        ikigai_core::Description::new(self.name())
            .verb(Verb::Sink)
            .verb(Verb::Delete)
            .requires("urn:cap:exec:write:git")
    }
}

fn threads(k: &Kernel) -> String {
    let req = Request::new(Verb::Source, Iri::parse("urn:kernel:threads").unwrap());
    let rep = futures::executor::block_on(k.issue(req, &Capability::root())).expect("threads");
    String::from_utf8(rep.bytes.to_vec()).unwrap()
}

/// A Sink or Delete the hub's floor refuses never ran, so the door kernel must not treat it
/// as a write that ran and failed: it cuts no thread (core: only a caller holding the
/// declared scopes may cause that cut).
#[test]
fn a_refused_write_at_the_door_cuts_no_thread() {
    let space = EndpointSpace::new().bind(ikigai_core::Exact::new("urn:repo:write"), Write);
    let hub = Arc::new(Kernel::new(Arc::new(space)));
    let cap = Capability::scoped(["urn:cap:exec:git".to_string()]);
    for door in [
        doors::door_kernel(Arc::clone(&hub)),
        doors::http_kernel_with(Arc::clone(&hub), EndpointSpace::new(), None),
    ] {
        let before = threads(&door);
        for verb in [Verb::Sink, Verb::Delete] {
            let ask = Request::new(verb, Iri::parse("urn:repo:write").unwrap())
                .with_arg("in", ArgRef::Inline(b"x".to_vec()));
            let err = futures::executor::block_on(door.issue(ask, &cap)).expect_err("refused");
            assert!(matches!(err, ikigai_core::Error::Denied(_)), "{err}");
        }
        let after = threads(&door);
        eprintln!("before:\n{before}\nafter:\n{after}");
        assert!(
            !after.contains("urn:repo:write"),
            "a refused write cut:\n{after}"
        );
        assert_eq!(before, after);
    }
}
