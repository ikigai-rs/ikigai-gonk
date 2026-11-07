//! The reverse of a link (ledger [#3217](http://localhost:1062/l/default/item/3217)): what
//! links TO an item, read from the item's own ledger graph.
//!
//! A row knows what it blocks, not what blocks it: the ledger stores `<a> ledger:blocks <b>`
//! on `a`, and an edge from another ledger as a `ledger:Mirror` in the target's graph
//! (ikigai-ledger #2829), holding the subject's IRI and whether it is open, and nothing else.
//!
//! ★ **Why gonk reads the graph rather than a ledger resource.** ikigai-ledger 0.20 has no
//! complete reverse resource. `next` names an item's open blockers only when blocked is the
//! ONE reason it gives (a claimed, deferred or handed-back item is excluded as that instead),
//! and an item's face carries its inbound `parent` edges (`children`) and the mirrors
//! (`mirrored`, on the JSON face only), never a same-ledger inbound `blocks` or `related`. So
//! this module asks the ledger's own graph the question those resources would answer, the
//! same triples and mirrors `next` reads, in ONE `urn:iki:store:graph-select` per ledger and
//! page, under the viewer's own capability. A viewer that may not read the graph gets nothing
//! here, which leaves the page as it was. When the ledger grows an inbound field on its item
//! rows, this module is the one place to switch to it.
//!
//! ⚠ `related` across ledgers is not mirrored (nothing in the target ledger reads it), so an
//! item cannot know a `related` link from another ledger without reading every ledger.

use std::collections::BTreeMap;

use ikigai_core::{Invocation, Verb};
use ikigai_ledger::Ledger;
use serde_json::Value;

use crate::web::{fetch, request, with};

const LEDGER_NS: &str = "https://ikigai-rs.dev/ns/ledger#";

/// The link types an item can be the object of.
pub(crate) const RELATIONS: [&str; 3] = ["blocks", "parent", "related"];

/// One edge into an item of the ledger read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inbound {
    /// The item linked to.
    pub item: String,
    /// `blocks`, `parent` or `related`.
    pub rel: String,
    /// The item that links, in this ledger or (through a mirror) another.
    pub subject: String,
    /// Whether that item is open, as this ledger's graph knows it.
    pub open: bool,
    /// Its number, when it is an item of this ledger.
    pub number: Option<i64>,
}

/// An IRI safe to write between `<` and `>` in a query: an item IRI never holds anything
/// else, and anything else is left out rather than escaped.
fn plain_iri(iri: &str) -> bool {
    !iri.is_empty()
        && iri
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'))
}

/// The query for `items` (every one an item of the same ledger): its own graph's edges into
/// them, and the mirrors of edges from other ledgers that are still present.
fn query(items: &[&str], relations: &[&str]) -> String {
    let values = items
        .iter()
        .map(|item| format!("<{item}>"))
        .collect::<Vec<_>>()
        .join(" ");
    let rels = relations
        .iter()
        .map(|rel| format!("l:{rel}"))
        .collect::<Vec<_>>()
        .join(" ");
    // One line, like the ledger's own patterns: the store's parser reads it the same way.
    format!(
        "PREFIX l: <{LEDGER_NS}> SELECT ?item ?rel ?subject ?open ?number WHERE {{ \
         VALUES ?item {{ {values} }} VALUES ?rel {{ {rels} }} \
         {{ ?subject ?rel ?item . ?subject a l:Item . FILTER(?subject != ?item) \
         OPTIONAL {{ ?subject l:number ?number }} \
         BIND(EXISTS {{ ?subject l:status l:open }} AS ?open) }} \
         UNION \
         {{ ?mirror a l:Mirror ; l:onItem ?item ; l:linkType ?rel ; l:mirrorOf ?subject ; \
         l:linkPresent true ; l:subjectOpen ?open . }} }}"
    )
}

/// Every edge of `relations` into `items`, as `ledger`'s graph holds them, sorted by item,
/// relation and subject. Items of another ledger are skipped; a refused or failed read is no
/// edges, so a page that cannot tell says nothing rather than guess.
pub(crate) async fn read(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    items: &[String],
    relations: &[&str],
) -> Vec<Inbound> {
    let items: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|item| plain_iri(item))
        .filter(|item| Ledger::item_iri(item).is_some_and(|(of, _)| of.name() == ledger.name()))
        .collect();
    if items.is_empty() || relations.is_empty() {
        return Vec::new();
    }
    let Ok(select) = request(Verb::Source, "urn:iki:store:graph-select") else {
        return Vec::new();
    };
    let select = with(
        with(
            with(select, "graph", &ledger.graph()),
            "query",
            &query(&items, relations),
        ),
        "as",
        "application/sparql-results+json",
    );
    let Ok(bytes) = fetch(inv, select).await else {
        return Vec::new();
    };
    let Ok(answer) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let term = |row: &Value, name: &str| -> Option<String> {
        row.get(name)?.get("value")?.as_str().map(str::to_string)
    };
    let mut edges: Vec<Inbound> = answer["results"]["bindings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| {
            let rel = term(row, "rel")?;
            Some(Inbound {
                item: term(row, "item")?,
                rel: rel.strip_prefix(LEDGER_NS)?.to_string(),
                subject: term(row, "subject")?,
                open: term(row, "open").is_some_and(|open| open == "true" || open == "1"),
                number: term(row, "number").and_then(|n| n.parse().ok()),
            })
        })
        .collect();
    edges.sort_by(|a, b| (&a.item, &a.rel, &a.subject).cmp(&(&b.item, &b.rel, &b.subject)));
    edges.dedup();
    edges
}

/// How a blocker is named on a badge: its number in this ledger (`#3`), `{ledger} {id}` in
/// another the viewer may read (the words an outgoing link uses), and its IRI otherwise,
/// which is all the mirror holds of it.
pub(crate) fn blocker_words(
    edge: &Inbound,
    ledger: &Ledger,
    readable: impl Fn(&Ledger) -> bool,
) -> String {
    match Ledger::item_iri(&edge.subject) {
        Some((of, _)) if of.name() == ledger.name() => match edge.number {
            Some(number) => ledger.number(number),
            None => edge.subject.clone(),
        },
        Some((of, _)) if readable(&of) => {
            let id = edge
                .subject
                .rsplit_once(":item:")
                .map(|(_, id)| id)
                .unwrap_or("");
            format!("{} {id}", of.name())
        }
        _ => edge.subject.clone(),
    }
}

/// Each item's open blockers, named for its badge, in the order [`read`] sorts them.
pub(crate) async fn blockers(
    inv: &Invocation<'_>,
    ledger: &Ledger,
    items: &[String],
) -> BTreeMap<String, Vec<String>> {
    let mut edges: Vec<Inbound> = read(inv, ledger, items, &["blocks"])
        .await
        .into_iter()
        .filter(|edge| edge.open)
        .collect();
    // This ledger's blockers first, by number, then the other ledgers', by IRI.
    edges.sort_by(|a, b| {
        (&a.item, a.number.is_none(), a.number, &a.subject).cmp(&(
            &b.item,
            b.number.is_none(),
            b.number,
            &b.subject,
        ))
    });
    let mut named: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in &edges {
        let words = blocker_words(edge, ledger, |of| inv.capability.allows(&of.cap_read()));
        named.entry(edge.item.clone()).or_default().push(words);
    }
    named
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_iri_reaches_the_query() {
        assert!(plain_iri("urn:iki:ledger:default:item:01abc"));
        for hostile in ["", "urn:a> } DROP ALL {", "urn:a b", "urn:a#b"] {
            assert!(!plain_iri(hostile), "{hostile}");
        }
    }

    #[test]
    fn a_blocker_is_named_by_number_here_by_id_elsewhere_and_by_iri_when_unreadable() {
        let ledger = Ledger::parse("default").unwrap();
        let edge = |subject: &str, number| Inbound {
            item: "urn:iki:ledger:default:item:01t".to_string(),
            rel: "blocks".to_string(),
            subject: subject.to_string(),
            open: true,
            number,
        };
        let readable = |of: &Ledger| of.name() == "crew";
        assert_eq!(
            blocker_words(
                &edge("urn:iki:ledger:default:item:01a", Some(3)),
                &ledger,
                readable
            ),
            "#3"
        );
        assert_eq!(
            blocker_words(
                &edge("urn:iki:ledger:crew:item:01b", None),
                &ledger,
                readable
            ),
            "crew 01b"
        );
        assert_eq!(
            blocker_words(
                &edge("urn:iki:ledger:private:item:01c", None),
                &ledger,
                readable
            ),
            "urn:iki:ledger:private:item:01c"
        );
    }
}
