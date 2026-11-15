//! Dispatcher deltas use a private, acknowledged reader cursor (ledger #3712).
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::compose;
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;
use serde_json::Value;

const VIEW: &str = "urn:iki:gonk:dispatch:default:item:1";
fn issue(
    hub: &Kernel,
    cap: &Capability,
    verb: Verb,
    target: &str,
    args: &[(&str, &str)],
) -> Result<Value, Error> {
    let req = args.iter().fold(
        Request::new(verb, Iri::parse(target).unwrap()),
        |req, (k, v)| req.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    );
    block_on(hub.issue(req, cap)).map(|r| {
        serde_json::from_slice(&r.bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&r.bytes).into_owned()))
    })
}
fn reader(name: &str) -> Capability {
    Capability::scoped(
        ikigai_gonk::grants::grants_for("default", ikigai_gonk::grants::Authority::Read).unwrap(),
    )
    .with_principal(&format!("urn:test:{name}"))
    .unwrap()
}
fn setup() -> Kernel {
    let hub = compose(DurableStore::in_memory().unwrap());
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "A dispatcher task"), ("as", "application/json")],
    )
    .unwrap();
    hub
}
fn view(hub: &Kernel, cap: &Capability) -> Value {
    issue(hub, cap, Verb::Source, VIEW, &[("as", "application/json")]).unwrap()
}
#[test]
fn a_second_look_omits_acknowledged_comments_without_marking_new_work_read() {
    let hub = setup();
    let cap = reader("alice");
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:default:comment",
        &[
            ("item", "1"),
            ("content", "first hand-back"),
            ("handback", "true"),
        ],
    )
    .unwrap();
    let first = view(&hub, &cap);
    assert!(first.to_string().contains("first hand-back"));
    assert_eq!(
        view(&hub, &cap)["items"],
        first["items"],
        "reading alone does not advance a cursor"
    );
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:default:comment",
        &[("item", "1"), ("content", "arrived after the look")],
    )
    .unwrap();
    issue(
        &hub,
        &cap,
        Verb::Sink,
        VIEW,
        &[("cursor", first["cursor"].as_str().unwrap())],
    )
    .unwrap();
    let next = view(&hub, &cap);
    assert!(!next["items"].to_string().contains("first hand-back"));
    assert!(next["items"].to_string().contains("arrived after the look"));
    assert!(view(&hub, &reader("bob"))["items"]
        .to_string()
        .contains("first hand-back"));
}

#[test]
fn cursors_are_bound_to_identity_scope_and_revision() {
    let hub = setup();
    let alice = reader("alice");
    let bob = reader("bob");
    let first = view(&hub, &alice);
    let token = first["cursor"].as_str().unwrap();
    assert!(matches!(
        issue(&hub, &bob, Verb::Sink, VIEW, &[("cursor", token)]),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        issue(
            &hub,
            &alice,
            Verb::Sink,
            "urn:iki:gonk:dispatch:default",
            &[("cursor", token)]
        ),
        Err(Error::Conflict(_))
    ));
    let second = view(&hub, &alice);
    issue(&hub, &alice, Verb::Sink, VIEW, &[("cursor", token)]).unwrap();
    assert!(matches!(
        issue(
            &hub,
            &alice,
            Verb::Sink,
            VIEW,
            &[("cursor", second["cursor"].as_str().unwrap())]
        ),
        Err(Error::Conflict(_))
    ));
    assert!(view(&hub, &alice)["items"].as_array().unwrap().is_empty());
    assert!(!issue(
        &hub,
        &alice,
        Verb::Source,
        "urn:iki:gonk:dispatch:default",
        &[("as", "application/json")]
    )
    .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn only_a_named_reader_or_the_local_owner_can_keep_a_cursor() {
    use ikigai_gonk::grants::{grants_for, Authority};
    let hub = setup();
    let anonymous = Capability::scoped(grants_for("default", Authority::Read).unwrap());
    assert!(matches!(
        issue(&hub, &anonymous, Verb::Source, VIEW, &[]),
        Err(Error::Denied(_))
    ));
    let read_only = anonymous.with_principal("urn:test:viewer").unwrap();
    let first = view(&hub, &read_only);
    issue(
        &hub,
        &read_only,
        Verb::Sink,
        VIEW,
        &[("cursor", first["cursor"].as_str().unwrap())],
    )
    .unwrap();
    assert!(view(&hub, &read_only)["items"]
        .as_array()
        .unwrap()
        .is_empty());
    let other = Capability::scoped(grants_for("other", Authority::Read).unwrap())
        .with_principal("urn:test:viewer")
        .unwrap();
    assert!(matches!(
        issue(&hub, &other, Verb::Source, VIEW, &[]),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        issue(
            &hub,
            &other,
            Verb::Sink,
            VIEW,
            &[("cursor", first["cursor"].as_str().unwrap())]
        ),
        Err(Error::Denied(_))
    ));
}

fn act(hub: &Kernel, action: &str, args: &[(&str, &str)]) {
    issue(
        hub,
        &Capability::root(),
        Verb::Sink,
        &format!("urn:iki:ledger:default:{action}"),
        args,
    )
    .unwrap();
}
fn ack(hub: &Kernel, cap: &Capability, target: &str, look: &Value) {
    issue(
        hub,
        cap,
        Verb::Sink,
        target,
        &[("cursor", look["cursor"].as_str().unwrap())],
    )
    .unwrap();
}
#[test]
fn important_changes_precede_routine_noise_and_current_blockers_are_visible() {
    let hub = setup();
    let cap = reader("dispatcher");
    act(&hub, "append", &[("content", "Routine work")]);
    act(
        &hub,
        "claim",
        &[("item", "2"), ("content", "worker"), ("lease", "PT30M")],
    );
    act(
        &hub,
        "comment",
        &[
            ("item", "1"),
            ("content", "review this evidence"),
            ("handback", "true"),
        ],
    );
    act(
        &hub,
        "comment",
        &[
            ("item", "1"),
            ("content", "evidence was refused"),
            ("rejects", "true"),
        ],
    );
    act(
        &hub,
        "link",
        &[("item", "2"), ("type", "blocks"), ("content", "1")],
    );
    let target = "urn:iki:gonk:dispatch:default";
    let look = issue(
        &hub,
        &cap,
        Verb::Source,
        target,
        &[("as", "application/json")],
    )
    .unwrap();
    assert_eq!(look["items"][0]["number"], 1);
    let blocker = issue(
        &hub,
        &cap,
        Verb::Source,
        "urn:iki:ledger:default:item:2",
        &[("as", "application/json")],
    )
    .unwrap();
    assert_eq!(look["items"][0]["blockers"][0], blocker["item"]["iri"]);
    let attention = look["items"][0]["attention"].as_array().unwrap();
    assert!(attention.iter().any(|event| event["kind"] == "handback"));
    assert!(attention.iter().any(|event| event["kind"] == "refusal"));
    let routine = look["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["number"] == 2)
        .unwrap();
    assert_eq!(routine["routine"]["claim"], 1);
    assert_eq!(routine["routine"]["claim_update"], 1);
    assert!(!routine["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "claim"));
    let plain = issue(&hub, &cap, Verb::Source, target, &[]).unwrap();
    assert!(plain.as_str().unwrap().contains("handback:"));
    ack(&hub, &cap, target, &look);
    act(
        &hub,
        "claim",
        &[("item", "2"), ("content", "worker"), ("renew", "true")],
    );
    let renewed = issue(
        &hub,
        &cap,
        Verb::Source,
        target,
        &[("as", "application/json")],
    )
    .unwrap();
    assert_eq!(renewed["items"].as_array().unwrap().len(), 1);
    assert_eq!(renewed["items"][0]["routine"]["claim_update"], 1);
    assert!(renewed["items"][0]["attention"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[test]
fn expiry_without_a_write_is_a_new_lapse_and_cursor_writes_leave_the_ledger_untouched() {
    let hub = setup();
    let cap = reader("dispatcher");
    act(
        &hub,
        "claim",
        &[("item", "1"), ("content", "worker"), ("lease", "PT1S")],
    );
    let look = view(&hub, &cap);
    let graph = || {
        issue(
            &hub,
            &Capability::root(),
            Verb::Source,
            "urn:iki:store:graph-select",
            &[
                ("graph", "urn:iki:ledger:graph:default"),
                (
                    "query",
                    "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o",
                ),
                ("as", "application/sparql-results+json"),
            ],
        )
        .unwrap()
    };
    let before = graph();
    ack(&hub, &cap, VIEW, &look);
    assert_eq!(
        graph(),
        before,
        "read receipts are not shared ledger triples"
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let expired = view(&hub, &cap);
    assert_eq!(expired["items"][0]["attention"][0]["kind"], "lapse");
    ack(&hub, &cap, VIEW, &expired);
    assert!(view(&hub, &cap)["items"].as_array().unwrap().is_empty());
}
#[test]
fn a_foreign_blocker_exposes_only_its_mirrored_identity() {
    let hub = setup();
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:private:append",
        &[("content", "A secret title")],
    )
    .unwrap();
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:private:link",
        &[("item", "1"), ("type", "blocks"), ("content", "default#1")],
    )
    .unwrap();
    let look = view(&hub, &reader("dispatcher"));
    let foreign = issue(
        &hub,
        &Capability::root(),
        Verb::Source,
        "urn:iki:ledger:private:item:1",
        &[("as", "application/json")],
    )
    .unwrap();
    assert_eq!(look["items"][0]["blockers"][0], foreign["item"]["iri"]);
    assert!(!look.to_string().contains("A secret title"));
}

#[test]
fn seventy_three_old_comments_do_not_return_on_the_next_look() {
    let hub = setup();
    let cap = reader("dispatcher");
    for n in 0..73 {
        act(
            &hub,
            "comment",
            &[
                ("item", "1"),
                ("content", &format!("old coordination comment {n}")),
            ],
        );
    }
    for _ in 0..2 {
        let full = issue(
            &hub,
            &cap,
            Verb::Source,
            "urn:iki:ledger:default:item:1",
            &[("as", "application/json")],
        )
        .unwrap();
        assert_eq!(
            full["item"]["comments"].as_array().unwrap().len(),
            73,
            "the old item face repeats the full tail on each look"
        );
    }
    let first = view(&hub, &cap);
    assert_eq!(
        first["items"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "comment")
            .count(),
        73
    );
    ack(&hub, &cap, VIEW, &first);
    act(
        &hub,
        "comment",
        &[
            ("item", "1"),
            ("content", "new evidence to decide"),
            ("handback", "true"),
        ],
    );
    let next = view(&hub, &cap);
    assert!(
        !next["items"][0]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "comment"),
        "73 old comments are omitted"
    );
    assert_eq!(next["items"][0]["attention"].as_array().unwrap().len(), 1);
    assert!(next.to_string().contains("new evidence to decide"));
    assert!(!next.to_string().contains("old coordination comment"));
}

mod scratch_gonk;
#[test]
fn the_production_socket_uses_the_private_sidecar_across_restarts() {
    let mut gonk = scratch_gonk::Gonk::with_quic("");
    let over_socket =
        |gonk: &scratch_gonk::Gonk, verb: Verb, target: &str, args: &[(&str, &str)]| {
            let req = args.iter().fold(
                Request::new(verb, Iri::parse(target).unwrap()),
                |req, (k, v)| req.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
            );
            let (answer, _) = ikigai_ipc::connect(&gonk.socket)
                .unwrap()
                .issue(req)
                .unwrap();
            serde_json::from_slice::<Value>(&answer.bytes).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(&answer.bytes).into_owned())
            })
        };
    over_socket(
        &gonk,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "persist this look")],
    );
    let look = over_socket(&gonk, Verb::Source, VIEW, &[("as", "application/json")]);
    over_socket(
        &gonk,
        Verb::Sink,
        VIEW,
        &[("cursor", look["cursor"].as_str().unwrap())],
    );
    let quic = gonk.dial_quic().unwrap();
    let read_request = || {
        Request::new(Verb::Source, Iri::parse(VIEW).unwrap())
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec()))
    };
    let (answer, _) = quic.issue(read_request()).unwrap();
    let quic_look: Value = serde_json::from_slice(&answer.bytes).unwrap();
    assert_eq!(
        quic_look["cursor_reset"], true,
        "the QUIC principal has its own cursor"
    );
    quic.issue(
        Request::new(Verb::Sink, Iri::parse(VIEW).unwrap()).with_arg(
            "cursor",
            ArgRef::Inline(quic_look["cursor"].as_str().unwrap().as_bytes().to_vec()),
        ),
    )
    .unwrap();
    drop(quic);
    gonk.child.kill().unwrap();
    gonk.child.wait().unwrap();
    gonk.restart();
    let (answer, _) = gonk.dial_quic().unwrap().issue(read_request()).unwrap();
    let quic_next: Value = serde_json::from_slice(&answer.bytes).unwrap();
    assert_eq!(quic_next["cursor_reset"], false);
    assert!(quic_next["items"].as_array().unwrap().is_empty());
    let next = over_socket(&gonk, Verb::Source, VIEW, &[("as", "application/json")]);
    assert_eq!(next["cursor_reset"], false);
    assert!(next["items"].as_array().unwrap().is_empty(), "the production composition must use the durable family, without a second in-memory copy shadowing it");
}

#[test]
fn a_pipeline_body_can_acknowledge_a_look_and_ambiguous_inputs_are_refused() {
    let hub = setup();
    let cap = reader("dispatcher");
    let look = view(&hub, &cap);
    let token = look["cursor"].as_str().unwrap();
    assert!(matches!(
        issue(
            &hub,
            &cap,
            Verb::Sink,
            VIEW,
            &[("cursor", token), ("content", token)]
        ),
        Err(Error::InvalidArgument { .. })
    ));
    issue(&hub, &cap, Verb::Sink, VIEW, &[("content", token)]).unwrap();
    assert!(view(&hub, &cap)["items"].as_array().unwrap().is_empty());
}
