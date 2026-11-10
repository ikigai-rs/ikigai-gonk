//! The config-home scoped-copy bound reaches the store behind the running server.
use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_resolve_transport::Resolver;

mod scratch_gonk;

#[test]
fn configured_scoped_copy_bound_reaches_the_served_store() {
    for (config, expected) in [
        ("", 250_000),
        ("gonk.store.max_scoped_copy_quads = 1000000\n", 1_000_000),
        ("gonk.store.max_scoped_copy_quads = 1\n", 1),
    ] {
        let gonk = scratch_gonk::Gonk::start(config);
        let client = ikigai_ipc::connect(&gonk.socket).unwrap();
        let request = Request::new(
            Verb::Source,
            Iri::parse("urn:iki:store:graph-info").unwrap(),
        )
        .with_arg(
            "graph",
            ArgRef::Inline(b"urn:iki:ledger:graph:default".to_vec()),
        );
        let (answer, _) = client.issue(request).unwrap();
        let text = String::from_utf8(answer.bytes).unwrap();
        let bound = format!("graph-update copy bound: {expected}");
        assert!(text.lines().any(|line| line == bound), "{text}");
    }
}
