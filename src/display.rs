//! Deployment display rules, read as data. No rule can supply markup, CSS or a query.
//! The closed look vocabulary uses the page's measured theme tokens in every theme.
use crate::render::{self, Graph};
use oxigraph::model::{NamedOrBlankNode, Term};

pub const NS: &str = "urn:iki:gonk:display#";
pub const IRI: &str = "urn:iki:gonk:display-rule";
pub const DEFAULT: &str = include_str!("../web/display-rules.ttl");
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// A reversible label partition key. URL-safe base64 keeps arbitrary label text out of
/// the resource-name grammar (including commas, spaces, slashes and fragment marks).
pub fn group_key(label: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(label)
}

pub fn group_label(key: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key)
        .map_err(|_| "The label partition is not URL-safe base64.".to_string())?;
    String::from_utf8(bytes).map_err(|_| "The label partition is not UTF-8.".to_string())
}

#[derive(Clone, Debug)]
pub struct Rule {
    iri: String,
    pub kind: String,
    pub value: String,
    pub ledger: Option<String>,
    pub badge: String,
    pub order: i64,
    pub counts: bool,
    pub group: bool,
    priority: i64,
}

#[derive(Clone, Debug)]
pub struct Rules {
    pub rules: Vec<Rule>,
    pub turtle: std::sync::Arc<str>,
}
impl Default for Rules {
    fn default() -> Self {
        Self::parse(DEFAULT).expect("shipped display rules parse")
    }
}
impl Rules {
    /// Reject unknown predicates, mistyped values, repeated scalar fields and unsupported
    /// combinations. A typo must stop startup rather than quietly changing presentation.
    pub fn parse(turtle: &str) -> Result<Self, String> {
        let graph = Graph::from_turtle(turtle.as_bytes())?;
        let subjects = graph.subjects_of_type(&format!("{NS}Rule"));
        for triple in graph.triples() {
            let NamedOrBlankNode::NamedNode(subject) = &triple.subject else {
                return Err("display rules must have named subjects".into());
            };
            if !subjects.iter().any(|s| s == subject.as_str()) {
                return Err(format!("{subject}: expected a display:Rule"));
            }
            let predicate = triple.predicate.as_str();
            if predicate == RDF_TYPE {
                if triple.object
                    != Term::NamedNode(oxigraph::model::NamedNode::new_unchecked(format!(
                        "{NS}Rule"
                    )))
                {
                    return Err(format!("{subject}: unsupported rule type"));
                }
                continue;
            }
            let local = predicate.strip_prefix(NS).unwrap_or("");
            if predicate != RDFS_LABEL
                && !matches!(
                    local,
                    "label"
                        | "prefix"
                        | "ledger"
                        | "level"
                        | "state"
                        | "badge"
                        | "order"
                        | "priority"
                        | "counts"
                        | "group"
                )
            {
                return Err(format!("{subject}: unknown display predicate {predicate}"));
            }
            let Term::Literal(value) = &triple.object else {
                return Err(format!("{subject}: {predicate} must be a literal"));
            };
            let datatype = match local {
                "order" | "priority" => "integer",
                "counts" | "group" => "boolean",
                _ => "string",
            };
            if value.language().is_some()
                || value.datatype().as_str()
                    != format!("http://www.w3.org/2001/XMLSchema#{datatype}")
            {
                return Err(format!("{subject}: {predicate} requires xsd:{datatype}"));
            }
        }
        let mut rules = Vec::new();
        for iri in subjects {
            let get = |local: &str| -> Result<Option<String>, String> {
                let values = graph.values(&iri, &format!("{NS}{local}"));
                if values.len() > 1 {
                    return Err(format!("{iri}: repeated display:{local}"));
                }
                Ok(values.into_iter().next())
            };
            let mut selectors = Vec::new();
            for kind in ["label", "prefix", "level", "state"] {
                if let Some(value) = get(kind)? {
                    selectors.push((kind.to_string(), value));
                }
            }
            let ledger = get("ledger")?;
            if let Some(name) = &ledger {
                ikigai_ledger::Ledger::parse(name).map_err(|e| e.to_string())?;
            }
            if selectors.is_empty() {
                if let Some(value) = &ledger {
                    selectors.push(("ledger".into(), value.clone()));
                }
            }
            if selectors.len() != 1 {
                return Err(format!("{iri}: specify one of label, prefix, ledger, level or state; ledger may also scope another selector"));
            }
            let (kind, value) = selectors.remove(0);
            if value.trim().is_empty() {
                return Err(format!("{iri}: an empty selector is not supported"));
            }
            if kind == "level" && oxigraph::model::NamedNode::new(&value).is_err() {
                return Err(format!("{iri}: level must name an absolute item class IRI"));
            }
            let boolean = |local: &str, default: bool| -> Result<bool, String> {
                match get(local)?.as_deref() {
                    None => Ok(default),
                    Some("true" | "1") => Ok(true),
                    Some("false" | "0") => Ok(false),
                    _ => Err(format!("{iri}: {local} is not a boolean")),
                }
            };
            let integer = |local: &str, default: i64| -> Result<i64, String> {
                get(local)?
                    .map(|s| {
                        s.parse()
                            .map_err(|_| format!("{iri}: {local} is not an integer"))
                    })
                    .unwrap_or(Ok(default))
            };
            let badge = get("badge")?.unwrap_or_else(|| "plain".into());
            if !matches!(badge.as_str(), "plain" | "strong" | "outline") {
                return Err(format!("{iri}: badge must be plain, strong or outline"));
            }
            let counts = boolean("counts", true)?;
            let group = boolean("group", false)?;
            if (get("counts")?.is_some() && !matches!(kind.as_str(), "label" | "prefix"))
                || (group && kind != "prefix")
            {
                return Err(format!(
                    "{iri}: counts applies to labels; grouping requires a prefix"
                ));
            }
            rules.push(Rule {
                order: integer("order", 100)?,
                priority: integer("priority", 100)?,
                iri,
                kind,
                value,
                ledger,
                badge,
                counts,
                group,
            });
        }
        rules.sort_by(|a, b| (a.priority, &a.iri).cmp(&(b.priority, &b.iri)));
        Ok(Self {
            rules,
            turtle: turtle.into(),
        })
    }
    pub fn matching(&self, kind: &str, value: &str, ledger: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| {
            r.ledger.as_deref().is_none_or(|l| l == ledger)
                && ((r.kind == kind && r.value == value)
                    || (kind == "label" && r.kind == "prefix" && value.starts_with(&r.value)))
        })
    }
    pub fn grouped(&self, label: &str, ledger: &str) -> bool {
        self.matching("label", label, ledger)
            .is_some_and(|r| r.group)
    }
    /// Add safe attributes to native chips without changing their text or destinations.
    pub fn chips(&self, graph: &Graph, item: &str, labels: &str) -> String {
        let view = |name| {
            graph
                .value(item, &format!("{}{name}", render::VIEW_NS))
                .unwrap_or_default()
        };
        let ledger = view("ledger");
        let mut output = String::new();
        // Label chip XML is generated by gonk; parse it so arbitrary label text stays data.
        let mut reader = quick_xml::Reader::from_str(labels);
        let mut writer = quick_xml::Writer::new(Vec::new());
        loop {
            use quick_xml::events::Event;
            let event = reader.read_event().expect("gonk chip XML");
            if matches!(event, Event::Eof) {
                break;
            }
            let mut event = event.into_owned();
            if let Event::Start(tag) | Event::Empty(tag) = &mut event {
                if tag.name().as_ref() == b"view:label-chip" {
                    let term = tag
                        .attributes()
                        .filter_map(Result::ok)
                        .find(|a| a.key.as_ref() == b"term")
                        .map(|a| a.unescape_value().expect("escaped label").into_owned())
                        .unwrap_or_default();
                    let rule = self.matching("label", &term, &ledger);
                    tag.push_attribute(("look", rule.map_or("plain", |r| r.badge.as_str())));
                    let order = rule.map_or(100, |r| r.order).to_string();
                    tag.push_attribute(("order", order.as_str()));
                }
            }
            writer.write_event(event).expect("writing to memory");
        }
        output.push_str(&String::from_utf8(writer.into_inner()).expect("chip UTF-8"));
        for kind in ["state", "ledger", "level"] {
            let value = match kind {
                "state" => view("state"),
                "ledger" => ledger.clone(),
                _ => view("kind"),
            };
            let rule = self.matching(kind, &value, &ledger);
            if kind != "state" && rule.is_none() {
                continue;
            }
            let in_flight = kind == "state" && view("stateInFlight") == "true";
            let text = format!(
                "{kind}: {value}{}",
                if in_flight { " · in flight" } else { "" }
            );
            output.push_str(&render::element(
                "display-chip",
                &[
                    ("kind", kind),
                    ("look", rule.map_or("plain", |r| r.badge.as_str())),
                    ("order", &rule.map_or(100, |r| r.order).to_string()),
                    ("in-flight", if in_flight { "true" } else { "false" }),
                ],
                &text,
            ));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table(body: &str) -> String {
        format!("@prefix d: <{NS}> . <urn:iki:gonk:display-rule:test> a d:Rule; {body} .")
    }
    #[test]
    fn exact_and_prefix_rules_have_deterministic_priority_and_ledger_scope() {
        let rules = Rules::parse(&format!("{}\n<urn:iki:gonk:display-rule:exact> a <{NS}Rule>; <{NS}label> \"repo:one\"; <{NS}priority> 1; <{NS}badge> \"strong\" .", table("d:prefix \"repo:\"; d:ledger \"default\"; d:group true; d:counts false; d:order -5"))).unwrap();
        assert_eq!(
            rules
                .matching("label", "repo:one", "default")
                .unwrap()
                .badge,
            "strong"
        );
        assert!(!rules.grouped("repo:one", "default"));
        assert!(rules.grouped("repo:two", "default"));
        assert!(!rules.grouped("repo:two", "private"));
        assert!(
            !rules
                .matching("label", "repo:two", "default")
                .unwrap()
                .counts
        );
        assert_eq!(
            rules
                .matching("label", "repo:two", "default")
                .unwrap()
                .order,
            -5
        );
    }
    #[test]
    fn unsupported_or_ambiguous_tables_are_refused() {
        for body in [
            "d:prefix \"\"",
            "d:label \"a\"; d:group true",
            "d:state \"filed\"; d:counts false",
            "d:label \"x\"; d:badge \"red\"",
            "d:label \"x\"; d:colour \"red\"",
            "d:label \"a\"; d:label \"b\"",
            "d:label \"a\"; d:order \"2\"",
            "d:label <urn:a>",
            "d:label \"a\"; d:counts \"false\"",
            "d:label \"a\"; d:prefix \"a\"",
            "d:ledger \"bad name\"",
            "d:label \"a\"; d:group 2",
        ] {
            assert!(Rules::parse(&table(body)).is_err(), "accepted {body}");
        }
        assert!(Rules::parse("<urn:typo> <urn:typo> \"x\" .").is_err());
        assert!(Rules::parse("").unwrap().rules.is_empty());
        assert!(Rules::parse(DEFAULT).is_ok());
    }
    #[test]
    fn ties_break_on_rule_iri_not_parse_order() {
        let a = format!("<urn:iki:gonk:display-rule:a> a <{NS}Rule>; <{NS}label> \"x\"; <{NS}badge> \"outline\" .");
        let b = table("d:label \"x\"; d:badge \"strong\"");
        for text in [format!("{a}{b}"), format!("{b}{a}")] {
            assert_eq!(
                Rules::parse(&text)
                    .unwrap()
                    .matching("label", "x", "default")
                    .unwrap()
                    .badge,
                "outline"
            );
        }
    }
}
