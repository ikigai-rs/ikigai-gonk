//! Ledger [#3959](http://localhost:1062/l/default/item/3959): the item lens and the Dashboard lens
//! (ikigai-ui `docs/design/workspace.md` section 7, proposals 14 and 15). Each lens is read from
//! the hub, its query prepared by ikigai-ui (read-only, no SERVICE), and run as the lens runs it:
//! `urn:sparql:select` with the focus bound as a term, never spliced. Neutral sample items.

use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_store::DurableStore;
use serde_json::Value;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |r, (name, value)| r.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn ask(hub: &Arc<Kernel>, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    let (hub, read) = (Arc::clone(hub), request(verb, iri, args));
    std::thread::Builder::new()
        .stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
        .spawn(move || futures::executor::block_on(hub.issue(read, &Capability::root())))
        .unwrap()
        .join()
        .unwrap()
        .map(|answer| String::from_utf8(answer.bytes).unwrap())
        .unwrap_or_else(|e| panic!("{iri}: {e}"))
}

/// The lens's rows for `focus`, each `var=value` joined by ` | `, as ikigai-ui's lens runs it.
fn lens_rows(hub: &Arc<Kernel>, name: &str, focus: &str) -> Vec<String> {
    let lens: Value = serde_json::from_str(&ask(
        hub,
        Verb::Source,
        &format!("urn:iki:ui:lens:name:{name}"),
        &[],
    ))
    .unwrap();
    let source = ask(
        hub,
        Verb::Source,
        &format!("urn:iki:ui:lens:name:{name}:query"),
        &[],
    );
    let prepared = ikigai_ui::lens::prepare(&source).expect("a read-only lens query");
    let bound = lens["bound"].as_u64().unwrap();
    let query = format!("{}\nLIMIT {}", prepared.query, bound + 1);
    let bindings = serde_json::json!({"focus": {"type": "uri", "value": focus}}).to_string();
    let answer: Value = serde_json::from_str(&ask(
        hub,
        Verb::Source,
        "urn:sparql:select",
        &[("query", &query), ("bindings", &bindings)],
    ))
    .unwrap();
    let vars: Vec<String> = answer["head"]["vars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    answer["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            vars.iter()
                .filter(|v| !matches!(v.as_str(), "at" | "focus"))
                .map(|v| format!("{v}={}", row[v]["value"].as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect()
}

fn iri(answer: &str) -> String {
    answer.split_whitespace().nth(1).unwrap().to_string()
}

#[test]
fn the_item_and_dashboard_lenses_answer_from_the_ledger_graph() {
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    let sink = |iri: &str, args: &[(&str, &str)]| ask(&hub, Verb::Sink, iri, args);
    let parent = iri(&sink(
        "urn:iki:ledger:default:append",
        &[("content", "Parent item")],
    ));
    let child = iri(&sink(
        "urn:iki:ledger:default:append",
        &[("content", "Child item")],
    ));
    let other = iri(&sink(
        "urn:iki:ledger:default:append",
        &[("content", "Open item")],
    ));
    sink(
        "urn:iki:ledger:default:link",
        &[("item", "2"), ("type", "parent"), ("content", "1")],
    );
    sink(
        "urn:iki:ledger:default:comment",
        &[("item", "1"), ("content", "A comment")],
    );
    sink(
        "urn:iki:ledger:default:claim",
        &[("item", "2"), ("content", "worker-a")],
    );

    // The Dashboard lens over the default ledger's graph: open items, the in-flight one first.
    let board = lens_rows(&hub, "gonk-dashboard", "urn:iki:ledger:graph:default");
    assert_eq!(
        board,
        [
            format!(
                "item={child} | number=2 | title=Child item | state=in flight | holder=worker-a"
            ),
            format!("item={parent} | number=1 | title=Parent item | state=open | holder="),
            format!("item={other} | number=3 | title=Open item | state=open | holder="),
        ]
    );

    // The dispatcher's close, with its evidence: the item's verdict (decision 17).
    sink(
        "urn:iki:ledger:default:close",
        &[
            ("item", "1"),
            ("reason", "done"),
            ("content", "Checked the evidence"),
            ("evidence", "https://example.org/pull/1"),
        ],
    );
    let item = lens_rows(&hub, "gonk-item", &parent);
    let parts: Vec<&str> = item
        .iter()
        .map(|row| row.split(" | ").next().unwrap())
        .collect();
    assert_eq!(
        parts,
        [
            "part=item",
            "part=state",
            "part=child",
            "part=comment",
            "part=comment",
            "part=close",
            "part=evidence"
        ],
        "{item:#?}"
    );
    assert_eq!(
        item[0],
        format!("part=item | resource={parent} | text=Parent item")
    );
    assert_eq!(item[1], "part=state | resource= | text=closed");
    assert_eq!(
        item[2],
        format!("part=child | resource={child} | text=#2 Child item (open)")
    );
    assert!(item[3].ends_with("text=A comment"), "{item:#?}");
    assert!(item[4].ends_with("text=Checked the evidence"), "{item:#?}");
    assert!(item[5].ends_with("text=closed done"), "{item:#?}");
    assert_eq!(
        item[6],
        "part=evidence | resource=https://example.org/pull/1 | text=https://example.org/pull/1"
    );

    // Closed, it leaves the Dashboard; the child, in flight, shows who holds it.
    let board = lens_rows(&hub, "gonk-dashboard", "urn:iki:ledger:graph:default");
    assert_eq!(board.len(), 2, "{board:#?}");
    let child_rows = lens_rows(&hub, "gonk-item", &child);
    assert!(
        child_rows
            .contains(&"part=state | resource= | text=in flight, held by worker-a".to_string()),
        "{child_rows:#?}"
    );
    assert!(!child_rows.iter().any(|r| r.starts_with("part=close")));

    // Reopened, the close is no longer the item's verdict.
    sink("urn:iki:ledger:default:reopen", &[("item", "1")]);
    let reopened = lens_rows(&hub, "gonk-item", &parent);
    assert!(
        !reopened
            .iter()
            .any(|r| r.starts_with("part=close") || r.starts_with("part=evidence")),
        "{reopened:#?}"
    );
}

#[test]
fn a_ledger_item_lists_the_item_lens() {
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    let listed = ask(&hub, Verb::Source, ikigai_gonk::lenses::ITEM_TYPE, &[]);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&listed).unwrap(),
        ["gonk-item"]
    );
}
