use ikigai_gonk::config::{settings, Flags, Homes};

const WORKFLOW: &str = r#"
@prefix ledger: <https://ikigai-rs.dev/ns/ledger#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:iki:ledger:lifecycle:workflow> a ledger:Lifecycle ;
    owl:versionInfo "1" ;
    ledger:hasState <urn:iki:ledger:lifecycle:workflow:filed>,
                    <urn:iki:ledger:lifecycle:workflow:closed> .
<urn:iki:ledger:lifecycle:workflow:filed> rdfs:label "filed" ;
    ledger:order 0 ; ledger:inFlight false ; ledger:drain "dispatcher" .
<urn:iki:ledger:lifecycle:workflow:closed> rdfs:label "closed" ;
    ledger:order 1 ; ledger:inFlight false ; ledger:drain "archive" .
"#;

#[test]
fn a_named_ledger_can_load_a_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("workflow.ttl"), WORKFLOW).unwrap();
    let homes = Homes {
        home: dir.path().into(),
        config: dir.path().into(),
        data: dir.path().into(),
    };
    settings(
        &Flags::default(),
        "gonk.ledger.lifecycle = \"tasks ~/workflow.ttl\"\n",
        &homes,
    )
    .expect("a named ledger's lifecycle is configurable");
}

mod scratch_gonk;
use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;

fn issue(
    gonk: &scratch_gonk::Gonk,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<Representation, Error> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    ikigai_ipc::connect(&gonk.socket)
        .unwrap()
        .issue(request)
        .map(|(repr, _)| repr)
}

#[test]
fn the_server_enforces_each_ledgers_own_states() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.ttl");
    std::fs::write(&path, WORKFLOW).unwrap();
    let gonk = scratch_gonk::Gonk::start(&format!(
        "gonk.ledger.lifecycle = \"tasks {}\"\n",
        path.display()
    ));
    for ledger in ["tasks", "default"] {
        issue(
            &gonk,
            Verb::Sink,
            &format!("urn:iki:ledger:{ledger}:append"),
            &[("content", "test item")],
        )
        .unwrap();
    }
    let transition = |ledger: &str, to: &str| {
        issue(
            &gonk,
            Verb::Sink,
            &format!("urn:iki:ledger:{ledger}:item:1:state"),
            &[("from", "filed"), ("to", to)],
        )
    };
    assert!(
        matches!(
            transition("tasks", "queued"),
            Err(Error::InvalidArgument { .. })
        ),
        "built-in state must be refused by tasks"
    );
    assert!(
        matches!(
            transition("default", "closed"),
            Err(Error::InvalidArgument { .. })
        ),
        "custom state must be refused by default"
    );
    transition("tasks", "closed").expect("configured state is accepted");
    transition("default", "queued").expect("unassigned ledger keeps the built-in lifecycle");
    let state = issue(
        &gonk,
        Verb::Source,
        "urn:iki:ledger:tasks:item:1:state",
        &[],
    )
    .unwrap();
    assert_eq!(String::from_utf8(state.bytes).unwrap().trim(), "closed");
}

#[test]
fn lifecycle_configuration_is_validated_before_serving() {
    let dir = tempfile::tempdir().unwrap();
    let homes = Homes {
        home: dir.path().into(),
        config: dir.path().into(),
        data: dir.path().into(),
    };
    std::fs::write(dir.path().join("workflow.ttl"), WORKFLOW).unwrap();
    std::fs::write(dir.path().join("bad.ttl"), "not Turtle").unwrap();
    std::fs::write(
        dir.path().join("invalid.ttl"),
        WORKFLOW.replace("ledger:order 0", "ledger:order 1"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("different.ttl"),
        WORKFLOW.replace("archive", "other-owner"),
    )
    .unwrap();
    for (config, expected) in [
        ("tasks", "expected `ledger path`"),
        ("tasks ~/missing.ttl", "missing.ttl"),
        ("Tasks ~/workflow.ttl", "gonk.ledger.lifecycle"),
        ("tasks ~/bad.ttl", "bad.ttl"),
        ("tasks ~/invalid.ttl", "invalid.ttl"),
    ] {
        let error = settings(
            &Flags::default(),
            &format!("gonk.ledger.lifecycle = \"{config}\"\n"),
            &homes,
        )
        .unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    // ikigai-ledger 0.18.0: every transition records the document's version, so it must have one.
    std::fs::write(
        dir.path().join("unversioned.ttl"),
        WORKFLOW.replace("owl:versionInfo \"1\" ;", ""),
    )
    .unwrap();
    let error = settings(
        &Flags::default(),
        "gonk.ledger.lifecycle = \"tasks ~/unversioned.ttl\"\n",
        &homes,
    )
    .unwrap_err();
    assert!(error.contains("declares no version"), "{error}");
    let line = "gonk.ledger.lifecycle = \"tasks ~/workflow.ttl\"\n";
    let error = settings(&Flags::default(), &line.repeat(2), &homes).unwrap_err();
    assert!(error.contains("assigned more than once"), "{error}");
    let shared = format!("{line}gonk.ledger.lifecycle = \"other ~/workflow.ttl\"\n");
    settings(&Flags::default(), &shared, &homes).expect("ledgers may share one vocabulary");
    let conflict = format!("{line}gonk.ledger.lifecycle = \"other ~/different.ttl\"\n");
    let error = settings(&Flags::default(), &conflict, &homes).unwrap_err();
    assert!(error.contains("conflicting definitions"), "{error}");
}
