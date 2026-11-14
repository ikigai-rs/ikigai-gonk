//! A dispatcher sees only events it has not acknowledged, under its own durable cursor.
use ikigai_core::{ArgRef, Iri, Kernel, Request, Verb};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;
use serde_json::Value;

fn hub() -> Kernel {
    ikigai_gonk::compose(DurableStore::in_memory().unwrap())
}
fn issue(hub: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Value {
    let request = args
        .iter()
        .fold(Request::new(verb, Iri::parse(iri).unwrap()), |r, (k, v)| {
            r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()))
        })
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
    let (answer, _) = Resolver::issue(hub, request).unwrap();
    serde_json::from_slice(&answer.bytes).unwrap()
}
fn append(hub: &Kernel) {
    issue(
        hub,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "Work\nA body")],
    );
}
fn comment(hub: &Kernel, text: &str, handback: bool) {
    issue(
        hub,
        Verb::Sink,
        "urn:iki:ledger:default:comment",
        &[
            ("item", "1"),
            ("content", text),
            ("handback", if handback { "true" } else { "false" }),
        ],
    );
}
const VIEW: &str = "urn:iki:gonk:dispatch:default";
const ACK: &str = "urn:iki:gonk:dispatch:default:ack";
fn view(hub: &Kernel, reader: &str) -> Value {
    issue(hub, Verb::Source, VIEW, &[("reader", reader)])
}
#[test]
fn dispatcher_reads_only_new_events_and_acknowledges_the_exact_snapshot() {
    let hub = hub();
    append(&hub);
    for _ in 0..73 {
        comment(&hub, "routine conversation", false);
    }
    comment(&hub, "Evidence ready", true);
    // The existing item face repeats the entire comment tail on every read.
    for _ in 0..2 {
        let whole = issue(&hub, Verb::Source, "urn:iki:ledger:default:item:1", &[]);
        assert_eq!(whole["item"]["comments"].as_array().unwrap().len(), 74);
    }
    let first = view(&hub, "dispatcher");
    assert_eq!(first["events"][0]["kind"], "handback");
    assert_eq!(first["events"].as_array().unwrap().len(), 75);
    assert_eq!(
        view(&hub, "dispatcher"),
        first,
        "reading does not acknowledge"
    );
    comment(&hub, "arrived during review", false);
    issue(
        &hub,
        Verb::Sink,
        ACK,
        &[
            ("reader", "dispatcher"),
            ("content", &first["acknowledge"].to_string()),
        ],
    );
    let next = view(&hub, "dispatcher");
    assert_eq!(next["events"].as_array().unwrap().len(), 1);
    assert_eq!(next["events"][0]["text"], "arrived during review");
    assert_eq!(view(&hub, "other")["events"].as_array().unwrap().len(), 76);
}

#[test]
fn item_cursor_and_ledger_cursor_are_independent() {
    let hub = hub();
    append(&hub);
    issue(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "Second")],
    );
    let first = issue(&hub, Verb::Source, VIEW, &[("reader", "d"), ("item", "1")]);
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    issue(
        &hub,
        Verb::Sink,
        ACK,
        &[
            ("reader", "d"),
            ("item", "1"),
            ("content", &first["acknowledge"].to_string()),
        ],
    );
    assert!(
        issue(&hub, Verb::Source, VIEW, &[("reader", "d"), ("item", "1")])["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(view(&hub, "d")["events"].as_array().unwrap().len(), 2);
}

#[test]
fn bounded_batches_do_not_acknowledge_the_unreturned_tail() {
    let hub = hub();
    append(&hub);
    for _ in 0..4 {
        comment(&hub, "comment", false);
    }
    let first = issue(&hub, Verb::Source, VIEW, &[("reader", "d"), ("limit", "2")]);
    assert_eq!(first["acknowledge"].as_array().unwrap().len(), 2);
    assert_eq!(first["more"], true);
    issue(
        &hub,
        Verb::Sink,
        ACK,
        &[
            ("reader", "d"),
            ("content", &first["acknowledge"].to_string()),
        ],
    );
    assert_eq!(view(&hub, "d")["events"].as_array().unwrap().len(), 3);
}

fn raw_update(hub: &Kernel, content: String) {
    Resolver::issue(
        hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:store:graph-update").unwrap(),
        )
        .with_arg(
            "graph",
            ArgRef::Inline(b"urn:iki:ledger:graph:default".to_vec()),
        )
        .with_arg("content", ArgRef::Inline(content.into_bytes())),
    )
    .unwrap();
}

#[test]
fn cursor_survives_reopening_the_durable_store_and_admits_backdated_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store");
    let item;
    {
        let hub = ikigai_gonk::compose(DurableStore::open(&path).unwrap());
        append(&hub);
        let first = view(&hub, "d");
        item = first["events"][0]["item"].as_str().unwrap().to_string();
        issue(
            &hub,
            Verb::Sink,
            ACK,
            &[
                ("reader", "d"),
                ("content", &first["acknowledge"].to_string()),
            ],
        );
    }
    let hub = ikigai_gonk::compose(DurableStore::open(&path).unwrap());
    assert!(view(&hub, "d")["events"].as_array().unwrap().is_empty());
    raw_update(
        &hub,
        format!(
            r#"PREFIX l: <https://ikigai-rs.dev/ns/ledger#>
        INSERT DATA {{ GRAPH <urn:iki:ledger:graph:default> {{
        <urn:iki:ledger:default:comment:backdated> a l:Comment ; l:onItem <{item}> ;
            l:body "backdated import" ; <http://purl.org/dc/terms/created> "2000-01-01T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
        }} }}"#
        ),
    );
    let next = view(&hub, "d");
    assert_eq!(next["events"].as_array().unwrap().len(), 1);
    assert_eq!(next["events"][0]["text"], "backdated import");
}

#[test]
fn claims_are_folded_and_refusals_come_first() {
    let hub = hub();
    append(&hub);
    // Claims return plain text even when JSON is requested.
    Resolver::issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:default:claim").unwrap(),
        )
        .with_arg("item", ArgRef::Inline(b"1".to_vec()))
        .with_arg("content", ArgRef::Inline(b"worker".to_vec())),
    )
    .unwrap();
    comment(&hub, "ready", true);
    issue(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:default:comment",
        &[
            ("item", "1"),
            ("content", "Evidence refused"),
            ("rejects", "true"),
        ],
    );
    let doc = view(&hub, "d");
    assert_eq!(doc["folded"]["claims"], 1);
    assert!(doc["folded"]["renewals"].is_null());
    let kinds: Vec<_> = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(!kinds.contains(&"claim"));
    assert!(kinds[..2].contains(&"handback"));
    assert!(kinds[..2].contains(&"refusal"));
    assert_eq!(
        doc["acknowledge"].as_array().unwrap().len(),
        doc["events"].as_array().unwrap().len() + 1
    );
}

#[test]
fn expired_leases_and_local_and_mirrored_blockers_stay_visible_after_ack() {
    let hub = hub();
    append(&hub);
    let item = view(&hub, "d")["events"][0]["item"]
        .as_str()
        .unwrap()
        .to_string();
    raw_update(
        &hub,
        format!(
            r#"PREFIX l: <https://ikigai-rs.dev/ns/ledger#>
        INSERT DATA {{ GRAPH <urn:iki:ledger:graph:default> {{
          <{item}> l:claimedBy "worker" ; l:leaseExpires "2000-01-01T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
          <urn:iki:ledger:default:item:blocker> a l:Item ; l:number 2 ; l:status l:open ; l:blocks <{item}> .
          <urn:iki:ledger:default:mirror:blocker> a l:Mirror ; l:onItem <{item}> ; l:linkType l:blocks ; l:linkPresent true ; l:subjectOpen true ; l:mirrorOf <urn:iki:ledger:private:item:blocker> .
        }} }}"#
        ),
    );
    let first = issue(&hub, Verb::Source, VIEW, &[("reader", "d"), ("item", "1")]);
    assert_eq!(first["attention"].as_array().unwrap().len(), 3);
    assert_eq!(
        first["events"].as_array().unwrap().len(),
        1,
        "mirrors are conditions, not events"
    );
    issue(
        &hub,
        Verb::Sink,
        ACK,
        &[
            ("reader", "d"),
            ("item", "1"),
            ("content", &first["acknowledge"].to_string()),
        ],
    );
    let next = issue(&hub, Verb::Source, VIEW, &[("reader", "d"), ("item", "1")]);
    assert_eq!(first["attention"], next["attention"]);
    assert!(next["events"].as_array().unwrap().is_empty());
}

#[test]
fn principal_cursors_cannot_be_spoofed_and_reads_require_both_grants() {
    use futures::executor::block_on;
    use ikigai_core::{Capability, Error};
    use ikigai_gonk::grants::{grants_for, Authority};
    let hub = hub();
    append(&hub);
    let request = || {
        Request::new(Verb::Source, Iri::parse(VIEW).unwrap())
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec()))
    };
    let cap = Capability::scoped(grants_for("default", Authority::Read).unwrap())
        .with_principal("urn:reader:one")
        .unwrap();
    let first = block_on(hub.issue(request(), &cap)).unwrap();
    let doc: Value = serde_json::from_slice(&first.bytes).unwrap();
    assert_eq!(doc["reader"], "urn:reader:one");
    let forged = request().with_arg("reader", ArgRef::Inline(b"urn:reader:two".to_vec()));
    assert!(matches!(
        block_on(hub.issue(forged, &cap)),
        Err(Error::InvalidArgument { .. })
    ));
    let ack = Request::new(Verb::Sink, Iri::parse(ACK).unwrap()).with_arg(
        "content",
        ArgRef::Inline(doc["acknowledge"].to_string().into_bytes()),
    );
    assert!(matches!(
        block_on(hub.issue(ack, &cap)),
        Err(Error::Denied(_))
    ));
    for cap in [
        Capability::scoped(["urn:cap:ledger:read:default"]),
        Capability::scoped(["urn:cap:store:read:graph:urn:iki:ledger:graph:default"]),
        Capability::scoped(grants_for("other", Authority::Write).unwrap()),
    ] {
        assert!(matches!(
            block_on(hub.issue(request(), &cap)),
            Err(Error::Denied(_))
        ));
    }
    let unnamed = Capability::scoped(grants_for("default", Authority::Read).unwrap());
    assert!(matches!(
        block_on(hub.issue(
            request().with_arg("reader", ArgRef::Inline(b"spoof".to_vec())),
            &unnamed
        )),
        Err(Error::InvalidArgument { .. })
    ));
}
