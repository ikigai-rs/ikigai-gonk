//! The kinds of layer, as the vocabulary says them (ledger
//! [#3883](http://localhost:1062/l/default/item/3883)): the Layers page's tabs.
//!
//! A layer's definition carries `ik:layerKind`, one concept of `ik:LayerKinds` (ikigai-layers
//! 0.11, ledger #3882). Each concept's `skos:notation` is its key (`/layers/kind/{key}`) and its
//! `skos:prefLabel` the tab's name, and the tabs come in the order of `ik:LayerKindOrder`'s
//! `skos:memberList`. All three are read from [`ikigai_vocab::VOCABULARY`] once, so no kind is
//! named in this crate: a kind the vocabulary adds is a tab without a change here. A layer whose
//! definition names no kind, or a concept the vocabulary does not list, is under [`OTHER_KEY`],
//! the last tab, shown only when such a layer exists.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{NamedOrBlankNode, Term};

const IK: &str = "https://ikigai-rs.dev/ns#";
const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The tab of a layer whose definition names no kind the vocabulary lists.
pub const OTHER_KEY: &str = "other";

/// That tab's name.
pub const OTHER_LABEL: &str = "Other";

/// One concept of `ik:LayerKinds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kind {
    /// The concept's IRI, what a definition's `ik:layerKind` names.
    pub iri: String,
    /// Its `skos:notation`: the tab's key.
    pub key: String,
    /// Its `skos:prefLabel`: the tab's name.
    pub label: String,
}

/// `ik:layerKind`'s IRI.
pub fn layer_kind_predicate() -> String {
    format!("{IK}layerKind")
}

/// Every kind, in `ik:LayerKindOrder`'s order.
pub fn order() -> &'static [Kind] {
    static ORDER: OnceLock<Vec<Kind>> = OnceLock::new();
    ORDER.get_or_init(|| read(ikigai_vocab::VOCABULARY))
}

/// The kind a definition's `ik:layerKind` names, when the vocabulary lists it.
pub fn of_iri(iri: &str) -> Option<&'static Kind> {
    order().iter().find(|k| k.iri == iri)
}

/// The kind whose key is `key`, when the vocabulary lists it.
pub fn of_key(key: &str) -> Option<&'static Kind> {
    order().iter().find(|k| k.key == key)
}

/// The kinds `turtle` orders: `ik:LayerKindOrder`'s list, each member's notation and English
/// (or untagged) label. A member without both is left out; a list that does not parse is empty,
/// and every layer is then under [`OTHER_KEY`], never hidden.
fn read(turtle: &str) -> Vec<Kind> {
    let mut by: BTreeMap<String, Vec<(String, Term)>> = BTreeMap::new();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        let Ok(quad) = quad else {
            return Vec::new();
        };
        let subject = match &quad.subject {
            NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
            NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
        };
        by.entry(subject)
            .or_default()
            .push((quad.predicate.as_str().to_string(), quad.object));
    }
    let object = |subject: &str, predicate: &str| -> Option<String> {
        by.get(subject)?
            .iter()
            .find(|(p, _)| p == predicate)
            .map(|(_, o)| match o {
                Term::NamedNode(n) => n.as_str().to_string(),
                Term::BlankNode(b) => format!("_:{}", b.as_str()),
                Term::Literal(l) => l.value().to_string(),
                #[allow(unreachable_patterns)]
                _ => String::new(),
            })
    };
    let label = |subject: &str| -> Option<String> {
        let labels: Vec<&oxigraph::model::Literal> = by
            .get(subject)?
            .iter()
            .filter(|(p, _)| *p == format!("{SKOS}prefLabel"))
            .filter_map(|(_, o)| match o {
                Term::Literal(l) => Some(l),
                _ => None,
            })
            .collect();
        labels
            .iter()
            .find(|l| l.language().is_some_and(|t| t.starts_with("en")))
            .or_else(|| labels.iter().find(|l| l.language().is_none()))
            .or(labels.first())
            .map(|l| l.value().to_string())
    };
    let mut kinds: Vec<Kind> = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut cell = object(&format!("{IK}LayerKindOrder"), &format!("{SKOS}memberList"));
    while let Some(at) = cell.filter(|c| *c != format!("{RDF}nil")) {
        // A list that loops is read once round.
        if !visited.insert(at.clone()) {
            break;
        }
        if let Some(iri) = object(&at, &format!("{RDF}first")) {
            if let (Some(key), Some(label)) =
                (object(&iri, &format!("{SKOS}notation")), label(&iri))
            {
                if !kinds.iter().any(|k| k.iri == iri || k.key == key) && key != OTHER_KEY {
                    kinds.push(Kind { iri, key, label });
                }
            }
        }
        cell = object(&at, &format!("{RDF}rest"));
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_orders_five_kinds_with_their_labels() {
        let keys: Vec<(&str, &str)> = order()
            .iter()
            .map(|k| (k.key.as_str(), k.label.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("talk", "Talk"),
                ("task", "Tasks"),
                ("feed", "Feed"),
                ("inquiry", "Inquiries"),
                ("join", "Join")
            ]
        );
        assert_eq!(of_key("talk").unwrap().iri, format!("{IK}TalkKind"));
        assert_eq!(of_iri(&format!("{IK}JoinKind")).unwrap().key, "join");
    }

    #[test]
    fn the_order_is_the_lists_not_the_schemes() {
        let turtle = r#"@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
ik:A skos:notation "a" ; skos:prefLabel "Ay"@en , "Ah"@fr .
ik:B skos:notation "b" ; skos:prefLabel "Bee" .
ik:C skos:notation "c" .
ik:LayerKindOrder skos:memberList ik:o1 .
ik:o1 rdf:first ik:B ; rdf:rest ik:o2 .
ik:o2 rdf:first ik:C ; rdf:rest ik:o3 .
ik:o3 rdf:first ik:A ; rdf:rest ik:o1 .
"#;
        let keys: Vec<(String, String)> =
            read(turtle).into_iter().map(|k| (k.key, k.label)).collect();
        assert_eq!(
            keys,
            [("b".into(), "Bee".into()), ("a".into(), "Ay".into())],
            "a member without a label is left out, and a looping list is read once"
        );
        assert!(read("not turtle").is_empty());
    }
}
