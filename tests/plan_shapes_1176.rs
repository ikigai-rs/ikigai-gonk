//! Ledger [#1176](http://localhost:1062/l/default/item/1176): gonk links `ikigai-shacl` with
//! its `rudof` feature OFF, so the installed binary validates plans with the subset validator
//! (`ikigai_shacl::core_shacl`), which links no network stack and recurses over neither graph.
//!
//! ⚠ **`cargo test` does not build that binary's validator into the hub.** `ikigai-layers`, a
//! dev-dependency, takes `ikigai-shacl` with default features, and a test build unifies
//! dev-dependency features, so the hub every other test composes validates through rudof. This
//! file covers the gap three ways:
//!
//! - [`a_plan_that_breaks_a_shape_is_refused_with_the_report`]: through gonk's own hub, a plan
//!   that breaks a shape is refused by `urn:plan:validate` with the validator's report, and
//!   `urn:plan:eval` will not run it (whichever validator the build linked).
//! - [`the_subset_agrees_with_the_linked_validator_on_gonk_plans`]: the subset, called
//!   directly over THIS lock's `ikigai_vocab::SHAPES`, gives the same outcome as the linked
//!   validator on every fixture, conforming and not.
//! - [`the_plan_door_validates_through_the_subset`]: the plan doors gonk links
//!   (`ikigai_engine::plan_space`), composed over the subset bound at `urn:shacl:validate`, as
//!   the installed binary composes them.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Iri,
    Kernel, ReprType, Representation, Request, Space, Verb,
};
use ikigai_gonk::compose_with;
use ikigai_gonk::script::Scripts;
use ikigai_shacl::core_shacl;
use ikigai_store::DurableStore;
use ikigai_vocab::{TurtleRenderer, SHAPES};

/// A plan the shapes accept: an input, an argument bound by reference, and a pipe.
const GOOD: &str = "\
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
<urn:plan:good> a ik:Process ;
    ik:input <urn:plan:good:input:label> ;
    ik:step <urn:plan:good:step:1> , <urn:plan:good:step:2> ;
    ik:result <urn:plan:good:step:2> .
<urn:plan:good:input:label> ik:inputName \"label\" ;
    ik:summary \"the label an item must carry\" ;
    ik:class xsd:string ;
    ik:required true .
<urn:plan:good:step:1> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:items> ;
    ik:argument <urn:plan:good:step:1:arg:labels> .
<urn:plan:good:step:1:arg:labels> a ik:Argument ;
    ik:inputName \"labels\" ;
    ik:ref <urn:plan:good:var:label> .
<urn:plan:good:step:2> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:next> ;
    ik:pipeFrom <urn:plan:good:step:1> .
";

/// A plan the shapes accept: two reads joined by a fork.
const FORK: &str = "\
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:plan:fork> a ik:Process ;
    ik:step <urn:plan:fork:step:1> , <urn:plan:fork:step:2> ;
    ik:result <urn:plan:fork:fork:1> .
<urn:plan:fork:fork:1> a ik:Fork ; ik:join \"newline\" .
<urn:plan:fork:step:1> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:items> ;
    ik:forkOf <urn:plan:fork:fork:1> ;
    ik:order 1 .
<urn:plan:fork:step:2> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:next> ;
    ik:forkOf <urn:plan:fork:fork:1> ;
    ik:order 2 .
";

/// One step whose verb is not one of the five: `sh:in` on `urn:ikigai:shape:step:verb`.
fn bad_verb() -> String {
    GOOD.replacen("ik:verb \"Source\"", "ik:verb \"Frobnicate\"", 1)
}

/// The cases that break a shape, each with what the report must say: the rule, or for a
/// SPARQL constraint its message.
fn broken() -> Vec<(&'static str, String, &'static str)> {
    vec![
        // A constraint component: `sh:in`.
        ("bad verb", bad_verb(), "urn:ikigai:shape:step:verb"),
        // A constraint component: `sh:maxCount`.
        (
            "two targets",
            GOOD.replacen(
                "ik:resolves <urn:iki:ledger:items> ;",
                "ik:resolves <urn:iki:ledger:items> , <urn:iki:ledger:next> ;",
                1,
            ),
            "urn:ikigai:shape:step:resolves",
        ),
        // A constraint component: `sh:pattern`.
        (
            "bad name",
            GOOD.replacen(
                "ik:pipeFrom <urn:plan:good:step:1> .",
                "ik:pipeFrom <urn:plan:good:step:1> ;\n    ik:binds \"9 lives\" .",
                1,
            ),
            "urn:ikigai:shape:step:binds",
        ),
        // A SHACL-SPARQL constraint: a reference to a name the plan never binds. Both
        // validators report it under its shape (`urn:ikigai:shape:process`) with no
        // `sh:sourceConstraint`, so the constraint is named by its message.
        (
            "unbound ref",
            GOOD.replacen("<urn:plan:good:var:label>", "<urn:plan:good:var:nobody>", 1),
            "urn:plan:good:var:nobody references a name this plan never binds",
        ),
    ]
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<String, Error> {
    block_on(kernel.issue(request(Verb::Source, iri, args), &Capability::root()))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
}

/// gonk's hub, as `main` composes it, with the script family (and so the plan doors) bound.
fn hub() -> Arc<Kernel> {
    let scripts = Scripts::in_memory();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
        None,
    ));
    scripts.attach(&hub);
    hub
}

/// The plan doors over the subset validator at `urn:shacl:validate`: what the installed binary
/// composes, without the rest of the hub.
fn subset_kernel() -> Kernel {
    let validate = FnEndpoint::new("shacl-validate-subset", |inv| {
        let data = inv.inline_str("data")?;
        let shapes = inv.inline_str("shapes")?;
        Ok(Representation::new(
            ReprType::new("text/turtle"),
            core_shacl::validate_turtle(data, shapes)?.into_bytes(),
        ))
    })
    .with_description(Description::new("shacl-validate-subset").verb(Verb::Source));
    let spaces: Vec<Arc<dyn Space>> = vec![
        Arc::new(ikigai_engine::plan_space::space()),
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:shacl:validate"), validate)),
    ];
    Kernel::with_meta_renderer(Arc::new(Fallback::new(spaces)), Arc::new(TurtleRenderer))
}

/// What `urn:plan:validate` says of each fixture on `kernel`: the good ones conform, and each
/// broken one is refused, and its report says why.
fn assert_the_plan_door_validates(kernel: &Kernel) {
    for (name, plan) in [("good", GOOD), ("fork", FORK)] {
        let summary = issue(kernel, "urn:plan:validate", &[("in", plan)]).expect(name);
        assert!(summary.contains(" conforms: "), "{name}: {summary}");
    }
    for (name, plan, rule) in broken() {
        let summary = issue(kernel, "urn:plan:validate", &[("in", &plan)]).expect(name);
        assert!(summary.contains("does not conform"), "{name}: {summary}");
        assert!(summary.contains(rule), "{name} says {rule}: {summary}");
        let report = issue(
            kernel,
            "urn:plan:validate",
            &[("in", &plan), ("as", "text/turtle")],
        )
        .expect(name);
        assert!(
            report.contains("sh:ValidationReport") && report.contains("sh:conforms false"),
            "{name}: {report}"
        );
        assert!(
            report.contains(rule),
            "{name}'s report says {rule}: {report}"
        );
    }
}

#[test]
fn a_plan_that_breaks_a_shape_is_refused_with_the_report() {
    let hub = hub();
    assert_the_plan_door_validates(&hub);
    // And `urn:plan:eval` runs exactly what validates: the broken plan is refused before any
    // step is issued, naming the rule.
    match issue(&hub, "urn:plan:eval", &[("in", &bad_verb())]) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "in");
            assert!(
                detail.contains("does not validate")
                    && detail.contains("urn:ikigai:shape:step:verb"),
                "{detail}"
            );
        }
        other => panic!("a plan that breaks a shape does not run: {other:?}"),
    }
}

#[test]
fn the_subset_agrees_with_the_linked_validator_on_gonk_plans() {
    let mut cases = vec![("good", GOOD.to_string()), ("fork", FORK.to_string())];
    cases.extend(broken().into_iter().map(|(name, plan, _)| (name, plan)));
    for (name, plan) in cases {
        let subset = core_shacl::validate_report(&plan, SHAPES)
            .unwrap_or_else(|e| panic!("{name}: the subset refused the vocabulary's shapes: {e}"))
            .outcome();
        let linked = ikigai_shacl::validate_outcome(&plan, SHAPES).expect(name);
        assert_eq!(subset, linked, "{name}");
        assert_eq!(
            subset.conforms,
            name == "good" || name == "fork",
            "{name}: {subset:?}"
        );
    }
}

#[test]
fn the_plan_door_validates_through_the_subset() {
    assert_the_plan_door_validates(&subset_kernel());
}
