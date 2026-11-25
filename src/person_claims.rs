//! Claims held under a passkey IRI before people (ledger
//! [#4305](http://localhost:1062/l/default/item/4305)) move to the passkey's person.
//!
//! Since people, the HTTP door stamps a signed-in request with its passkey's PERSON IRI
//! ([`crate::identity::person_iri`]). `ikigai-ledger` records a claim's principal
//! (`ledger:claimPrincipal`) and lets only that exact principal renew, re-take or release it
//! (`same_claimant`), so a claim taken under `urn:iki:gonk:passkey:<id>` before the upgrade
//! would be refused to its own holder afterwards. gonk cannot hand that crate an equivalence,
//! so it MIGRATES the key instead: at every start, each LIVE item whose `ledger:claimPrincipal`
//! is an enrolled passkey's IRI is moved to that passkey's person.
//!
//! ★ Only the item's current claim, which is state. The history stays as written: authors,
//! and the principal a release or takeover EVENT copied, keep the passkey IRI (the ledger is
//! append-only), and the faces read either IRI as the person's label. A passkey IRI that no
//! enrolled passkey names is left alone, and so is every other principal. A second run finds
//! nothing to move.

use std::collections::BTreeMap;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_ledger::vocabulary as v;

use crate::identity::PASSKEY_IRI_PREFIX;

/// The dataset-wide SELECT the plan reads.
const SELECT: &str = "urn:iki:store:select";

/// One claim to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    /// The graph (one ledger's) the item is in.
    pub graph: String,
    /// The item.
    pub item: String,
    /// The passkey IRI it is claimed under.
    pub from: String,
    /// The person IRI it will be claimed under.
    pub to: String,
}

/// Every live claim held under a passkey IRI `people` maps (passkey IRI → person IRI,
/// [`crate::identity::passkey_people`]).
pub fn plan(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    people: &BTreeMap<String, String>,
) -> Result<Vec<Move>, String> {
    if people.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        "SELECT ?g ?item ?from WHERE {{ GRAPH ?g {{ ?item <{type_}> <{class}> ; \
         <{principal}> ?from . FILTER(isIRI(?from) && STRSTARTS(STR(?from), \"{prefix}\")) }} }} \
         ORDER BY ?g ?item",
        type_ = v::ext::TYPE,
        class = v::ITEM_CLASS,
        principal = v::CLAIM_PRINCIPAL,
        prefix = PASSKEY_IRI_PREFIX,
    );
    let bytes = issue(
        Request::new(Verb::Source, Iri::parse(SELECT).expect("a constant IRI"))
            .with_arg("query", ArgRef::Inline(query.into_bytes()))
            .with_arg(
                "as",
                ArgRef::Inline(b"application/sparql-results+json".to_vec()),
            ),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("the store's SELECT answer is not JSON: {e}"))?;
    let rows = json
        .pointer("/results/bindings")
        .and_then(serde_json::Value::as_array)
        .ok_or("the store's SELECT answer has no results.bindings")?;
    let value = |row: &serde_json::Value, name: &str| {
        row.pointer(&format!("/{name}/value"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("a store row with no `{name}`"))
    };
    let mut moves = Vec::new();
    for row in rows {
        let from = value(row, "from")?;
        let Some(to) = people.get(&from) else {
            continue;
        };
        moves.push(Move {
            graph: value(row, "g")?,
            item: value(row, "item")?,
            from,
            to: to.clone(),
        });
    }
    Ok(moves)
}

/// The one conditional update that moves `m.item`'s claim principal from the passkey IRI to
/// the person IRI. Refused for an IRI that cannot sit inside `<…>`.
pub fn move_update(m: &Move) -> Result<String, String> {
    for iri in [&m.graph, &m.item, &m.from, &m.to] {
        Iri::parse(iri.clone()).map_err(|e| format!("the store named `{iri}`: {e}"))?;
        if iri.contains(['<', '>', '"', ' ', '{', '}', '\\']) {
            return Err(format!("`{iri}` cannot be written inside a SPARQL IRI"));
        }
    }
    Ok(format!(
        "DELETE {{ GRAPH <{graph}> {{ <{item}> <{p}> <{from}> }} }} \
         INSERT {{ GRAPH <{graph}> {{ <{item}> <{p}> <{to}> }} }} \
         WHERE {{ GRAPH <{graph}> {{ <{item}> <{p}> <{from}> }} }}",
        graph = m.graph,
        item = m.item,
        p = v::CLAIM_PRINCIPAL,
        from = m.from,
        to = m.to,
    ))
}

/// Move every claim [`plan`] finds; returns how many moved.
pub fn run(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    people: &BTreeMap<String, String>,
) -> Result<usize, String> {
    let moves = plan(issue, people)?;
    for m in &moves {
        crate::keys::update(issue, &m.graph, &move_update(m)?)?;
    }
    Ok(moves.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_update_is_conditional_on_the_passkey_iri() {
        let m = Move {
            graph: "urn:iki:ledger:graph:default".into(),
            item: "urn:iki:ledger:default:item:a".into(),
            from: "urn:iki:gonk:passkey:bGFwdG9w".into(),
            to: "urn:iki:gonk:person:id:AAAAAAAAAAAA".into(),
        };
        let u = move_update(&m).unwrap();
        assert!(u.starts_with("DELETE { GRAPH <urn:iki:ledger:graph:default>"));
        assert!(u.contains(
            "WHERE { GRAPH <urn:iki:ledger:graph:default> { <urn:iki:ledger:default:item:a>"
        ));
        assert!(u.contains("<urn:iki:gonk:person:id:AAAAAAAAAAAA>"));
        let bad = Move {
            to: "urn:x:a>b".into(),
            ..m
        };
        assert!(move_update(&bad).is_err());
    }
}
