//! Ledger [#4456](http://localhost:1060/l/default/item/4456): gonk links ikigai-script 0.6.1,
//! which serves a script's compiled form (`urn:script:{name}:compiled`, which carries the
//! PROGRAM) under the script's READ grant only, and keeps a publisher's exclusions that are not
//! about the script out of the compiled form and the run records.
//!
//! On 0.6.0 both tests below are red: a caller holding only the run grant read the program
//! through `…:compiled`, and the compiled form named every exclusion its publisher held, about
//! the script or not.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::compose_with;
use ikigai_gonk::grants::{grants_for, script_grants, Authority, ScriptAuthority};
use ikigai_gonk::script::Scripts;
use ikigai_store::DurableStore;

/// A query over the default ledger's graph, with a marker a reader of the program would see.
const QUERY: &str = "\
# The program's own text: SECRET-MARKER-4456
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
SELECT ?number
FROM <urn:iki:ledger:graph:default>
WHERE { ?item a ledger:Item ; ledger:number ?number . }
";

/// An exclusion in a family the script holds no grant in: the publisher's own policy.
const UNRELATED: &str = "urn:cap:net:-internal.example";

const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

fn hub() -> (std::sync::Arc<Kernel>, Scripts) {
    let scripts = Scripts::in_memory();
    let hub = std::sync::Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
        None,
    ));
    scripts.attach(&hub);
    (hub, scripts)
}

fn issue(
    kernel: &Kernel,
    capability: &Capability,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<Representation, Error> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    block_on(kernel.issue(request, capability))
}

fn text(repr: &Representation) -> String {
    String::from_utf8_lossy(&repr.bytes).into_owned()
}

/// The publisher: writes the default ledger, publishes `counted`, and holds an exclusion about
/// nothing the script does.
fn publisher() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    scopes.extend(script_grants("counted", ScriptAuthority::Publish).unwrap());
    scopes.push(UNRELATED.to_string());
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

fn published() -> std::sync::Arc<Kernel> {
    let (hub, _scripts) = hub();
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item")],
    )
    .expect("append");
    issue(
        &hub,
        &publisher(),
        Verb::Sink,
        "urn:script:counted",
        &[("language", "sparql"), ("content", QUERY)],
    )
    .expect("the publisher publishes the query");
    hub
}

#[test]
fn a_runner_without_the_read_grant_runs_the_script_but_cannot_read_its_program() {
    let hub = published();
    // Hand-written: gonk's own `--script counted=run` also mints the read, so only a raw
    // grant holds the run alone.
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push("urn:cap:script:run:counted".to_string());
    let runner = Capability::scoped(scopes);

    match issue(
        &hub,
        &runner,
        Verb::Source,
        "urn:script:counted:compiled",
        &[],
    ) {
        Err(Error::Denied(_)) => {}
        Ok(repr) => panic!("a runner read the program: {}", text(&repr)),
        Err(other) => panic!("not a refusal by grant: {other:?}"),
    }
    // …and still runs it: the run prepares its program without the runner reading it.
    let answer = issue(
        &hub,
        &runner,
        Verb::Source,
        "urn:script:counted:result",
        &[],
    )
    .expect("a runner runs the script");
    assert!(!text(&answer).contains("SECRET-MARKER-4456"));
}

#[test]
fn the_compiled_form_names_no_exclusion_that_is_not_about_the_script() {
    let hub = published();
    let compiled = issue(
        &hub,
        &publisher(),
        Verb::Source,
        "urn:script:counted:compiled",
        &[],
    )
    .expect("the publisher reads the compiled form");
    let body = text(&compiled);
    assert!(body.contains("SECRET-MARKER-4456"), "the program: {body}");
    assert!(
        !body.contains(UNRELATED),
        "an exclusion about nothing the script does: {body}"
    );
}
