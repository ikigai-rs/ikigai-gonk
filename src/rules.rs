//! The render rules: **a resource that says which result cells become controls**.
//!
//! A SPARQL result set is terminal by default — numbers a person reads out and types
//! somewhere else. These rules make a cell a link or a next question instead. What they are
//! NOT is a list of blessed column names compiled into the binary: a result set carries
//! whatever variable names its author chose, so a name rule works for today's samples and
//! fails for tomorrow's query.
//!
//! # ★ The rule table is a RESOURCE
//!
//! [`DEFAULT_RULES`] is the table gonk ships, as Turtle; a deployment replaces it wholesale
//! with `<config home>/gonk/render-rules.ttl`. Either way the table in effect is SERVED, at
//! `urn:iki:gonk:render-rules` (HTTP `/render-rules`), and the renderer RESOLVES it rather
//! than reaching into a field — so what the page acted on is exactly what a person, a diff
//! or another kernel can read back. The vocabulary is `urn:iki:gonk:render#`; it is gonk's
//! own and is not published — see `web/render-rules.ttl`, which is also the documentation.
//!
//! # ★★ Matching on the VALUE, and the one place that is not possible
//!
//! | rule | matches | why it is safe |
//! |---|---|---|
//! | item IRI | `urn:iki:ledger:{ledger}:item:{id}` | the shape can be nothing else |
//! | `repo:` label | a literal with that prefix | likewise |
//! | item number | a bare integer **under a named column** | see below |
//!
//! ⚠ **A bare integer is genuinely ambiguous and no value-only rule fixes it.** A SPARQL
//! result carries no provenance, so in `SELECT ?number ?title (COUNT(?comment) AS ?comments)`
//! the renderer sees `244` and `3` as the same kind of thing — one is an item, one is a
//! count, and linking both would send "3 comments" to item #3. Checking that an item with
//! that number EXISTS does not disambiguate either: item 3 exists. So that one rule consults
//! the variable name, its default covers the obvious spellings, and it lives in the table
//! where a deployment can extend it.
//!
//! # ⚠ Two grammars, and the substitution is escaped for whichever it lands in
//!
//! The values come from the store and the store holds text other people wrote. A
//! `render:link` template percent-encodes every substitution ([`percent`]); a
//! `render:query` template escapes it for the SPARQL string-literal grammar
//! ([`sparql_string`]) before the whole rendered query is percent-encoded into the editor's
//! URL. A rule author writes `"{value}"` and cannot opt out of either: the templates carry
//! placeholders, never concatenated text.

use std::collections::BTreeSet;

use crate::render::Graph;

/// The rule table gonk ships, as Turtle. Replaced wholesale by
/// `<config home>/gonk/render-rules.ttl` when that file exists.
pub const DEFAULT_RULES: &str = include_str!("../web/render-rules.ttl");

/// The rule vocabulary. gonk's own; nothing outside this server carries it.
pub const RENDER_NS: &str = "urn:iki:gonk:render#";

/// The resource the table is served at, and the one the renderer resolves.
pub const RULES_IRI: &str = "urn:iki:gonk:render-rules";

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

/// What a matched cell becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// Where the anchor goes, already escaped for a URL.
    pub href: String,
    /// The anchor's accessible name. It always CONTAINS the cell's visible text, so the
    /// visible label stays part of the accessible one (WCAG 2.5.3).
    pub label: String,
    /// `link` or `query` — what kind of thing clicking it does.
    pub kind: &'static str,
}

/// One rule: conditions that must all hold, and the action they produce.
#[derive(Debug, Clone)]
struct Rule {
    iri: String,
    order: i64,
    vars: BTreeSet<String>,
    kinds: BTreeSet<String>,
    prefixes: Vec<String>,
    shapes: BTreeSet<String>,
    template: Template,
    label: Option<String>,
}

#[derive(Debug, Clone)]
enum Template {
    /// A URL the cell links to.
    Link(String),
    /// A SPARQL query the cell runs, in the editor, against the current ledger.
    Query(String),
}

/// The rule table in effect.
#[derive(Debug, Clone)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Default for Rules {
    /// The shipped table. Used when a deployment names no file of its own — and as the
    /// render-time fallback, since [`Rules::parse`] has already refused a bad override at
    /// startup.
    fn default() -> Rules {
        Rules::parse(DEFAULT_RULES).expect("the shipped rule table parses")
    }
}

impl Rules {
    /// Parse a rule table. Every failure is named rather than skipped: a rule that silently
    /// did nothing would look exactly like one that is in effect.
    pub fn parse(turtle: &str) -> Result<Rules, String> {
        let graph = Graph::from_turtle(turtle.as_bytes())
            .map_err(|e| format!("the rule table is not Turtle: {e}"))?;
        let one = |subject: &str, local: &str| graph.value(subject, &format!("{RENDER_NS}{local}"));
        let many =
            |subject: &str, local: &str| graph.values(subject, &format!("{RENDER_NS}{local}"));
        let mut rules = Vec::new();
        for iri in graph.subjects_of_type(&format!("{RENDER_NS}Rule")) {
            let link = one(&iri, "link");
            let query = one(&iri, "query");
            let template = match (link, query) {
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "{iri}: a rule states both render:link and render:query; it may state one"
                    ))
                }
                (Some(url), None) => Template::Link(url),
                (None, Some(sparql)) => Template::Query(sparql),
                (None, None) => {
                    return Err(format!(
                        "{iri}: a rule with no render:link and no render:query does nothing"
                    ))
                }
            };
            let shapes: BTreeSet<String> = many(&iri, "matchShape").into_iter().collect();
            for shape in &shapes {
                if !matches!(shape.as_str(), "ledgerItemIri" | "integer") {
                    return Err(format!(
                        "{iri}: `{shape}` is not a render:matchShape this renderer knows \
                         (ledgerItemIri, integer)"
                    ));
                }
            }
            let kinds: BTreeSet<String> = many(&iri, "matchKind").into_iter().collect();
            for kind in &kinds {
                if !matches!(kind.as_str(), "uri" | "literal" | "bnode") {
                    return Err(format!(
                        "{iri}: `{kind}` is not a term kind (uri, literal, bnode)"
                    ));
                }
            }
            let order = match one(&iri, "order") {
                Some(text) => text
                    .parse::<i64>()
                    .map_err(|_| format!("{iri}: render:order `{text}` is not an integer"))?,
                None => 100,
            };
            rules.push(Rule {
                order,
                vars: many(&iri, "matchVar").into_iter().collect(),
                kinds,
                prefixes: many(&iri, "matchPrefix"),
                shapes,
                template,
                label: one(&iri, "label").or_else(|| graph.value(&iri, RDFS_LABEL)),
                iri,
            });
        }
        // Lowest order wins, ties broken on the IRI — never on parse order, so the same
        // table always renders the same page.
        rules.sort_by(|a, b| (a.order, &a.iri).cmp(&(b.order, &b.iri)));
        Ok(Rules { rules })
    }

    /// How many rules are in effect.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the table is empty — a table with no rules renders exactly today's page.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The action for one cell, or `None` when no rule matches it.
    ///
    /// `var` is the SPARQL variable the cell sits under, `kind` the term kind the store
    /// reported (`uri`, `literal`, `bnode`), `value` its lexical value as the store returned
    /// it — never the shortened display — and `ledger` the ledger the query ran against.
    pub fn action(&self, var: &str, kind: &str, value: &str, ledger: &str) -> Option<Action> {
        let item = ledger_item_iri(value);
        let bindings: Vec<(&str, String)> = vec![
            ("value", value.to_string()),
            ("ledger", ledger.to_string()),
            (
                "itemLedger",
                item.as_ref()
                    .map(|(l, _)| l.to_string())
                    .unwrap_or_default(),
            ),
            (
                "itemId",
                item.as_ref()
                    .map(|(_, i)| i.to_string())
                    .unwrap_or_default(),
            ),
        ];
        for rule in &self.rules {
            if !rule.vars.is_empty() && !rule.vars.contains(var) {
                continue;
            }
            if !rule.kinds.is_empty() && !rule.kinds.contains(kind) {
                continue;
            }
            if !rule.prefixes.is_empty() && !rule.prefixes.iter().any(|p| value.starts_with(p)) {
                continue;
            }
            if !rule.shapes.iter().all(|shape| match shape.as_str() {
                "ledgerItemIri" => item.is_some(),
                "integer" => is_integer(value),
                _ => false,
            }) {
                continue;
            }
            let (href, kind) = match &rule.template {
                Template::Link(url) => (fill(url, &bindings, percent)?, "link"),
                Template::Query(sparql) => {
                    let query = fill(sparql, &bindings, sparql_string)?;
                    (
                        format!(
                            "/sparql?ledger={}&query={}",
                            percent(ledger),
                            percent(&query)
                        ),
                        "query",
                    )
                }
            };
            // The accessible name falls back to the value itself, which is what the cell
            // already reads as — never to nothing.
            let label = rule
                .label
                .as_ref()
                .and_then(|l| fill(l, &bindings, str::to_string))
                .unwrap_or_else(|| value.to_string());
            return Some(Action { href, label, kind });
        }
        None
    }
}

/// `urn:iki:ledger:{ledger}:item:{id}` → `(ledger, id)`.
///
/// Exact: the ledger name and the item id must each be one segment, so nothing longer or
/// stranger sneaks through the shape test.
fn ledger_item_iri(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix("urn:iki:ledger:")?;
    let (ledger, id) = rest.split_once(":item:")?;
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(ledger) && plain(id)).then_some((ledger, id))
}

/// A bare non-negative decimal integer, short enough to be an item number.
fn is_integer(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 18
        && value.bytes().all(|b| b.is_ascii_digit())
        // A leading zero is not how the ledger writes a number, and `007` in a URL would
        // resolve to something the cell did not say.
        && (value == "0" || !value.starts_with('0'))
}

/// Substitute `{name}` placeholders, each value passed through `escape` for the grammar it
/// is landing in.
///
/// ⚠ **A SPARQL template is full of braces that are not placeholders** — `WHERE { … }`,
/// `OPTIONAL { … }` — so a placeholder is only `{` + an identifier + `}`. Anything else
/// between braces is literal text and is copied through untouched.
///
/// An identifier-shaped placeholder this renderer does not know, or one whose value is
/// empty, yields `None` and the rule produces no action at all: a rule that reaches for
/// `{itemId}` without also matching `ledgerItemIri` emits nothing rather than a broken link.
fn fill(
    template: &str,
    bindings: &[(&str, String)],
    escape: impl Fn(&str) -> String,
) -> Option<String> {
    let identifier =
        |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').map(|close| (&after[..close], close)) {
            Some((name, close)) if identifier(name) => {
                let value = bindings.iter().find(|(k, _)| *k == name)?;
                if value.1.is_empty() {
                    return None;
                }
                out.push_str(&escape(&value.1));
                rest = &after[close + 1..];
            }
            // A brace that opens a SPARQL group, or one that never closes: literal.
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Some(out)
}

/// Percent-encode for a URL — path segment or query value alike. Everything but the
/// unreserved set is encoded, so the result cannot add a segment, a parameter or a fragment
/// to the template that carried it.
///
/// ```
/// use ikigai_gonk::rules::percent;
/// assert_eq!(percent("repo:a b/c?d#e&f=g"), "repo%3Aa%20b%2Fc%3Fd%23e%26f%3Dg");
/// assert_eq!(percent("plain-Name_1.2~3"), "plain-Name_1.2~3");
/// ```
pub fn percent(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Escape for the inside of a SPARQL double-quoted string literal (no quotes added).
///
/// ⚠ **This is the dangerous grammar**, because a label that changed the SHAPE of the query
/// it runs would be running someone else's query under the caller's capability. A quote, a
/// backslash, a newline and every other control character are escaped by the SPARQL
/// `ECHAR` / `\uXXXX` forms, so the value can only ever be a string.
///
/// ```
/// use ikigai_gonk::rules::sparql_string;
/// assert_eq!(sparql_string(r#"a"b\c"#), r#"a\"b\\c"#);
/// assert_eq!(sparql_string("one\ntwo"), "one\\ntwo");
/// assert_eq!(sparql_string("{ ?s ?p ?o }"), "{ ?s ?p ?o }");
/// ```
pub fn sparql_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::default()
    }

    #[test]
    fn the_shipped_table_parses_and_is_ordered() {
        let rules = rules();
        assert_eq!(rules.len(), 3, "{rules:?}");
        let orders: Vec<i64> = rules.rules.iter().map(|r| r.order).collect();
        assert_eq!(orders, vec![10, 20, 30]);
    }

    /// ★ The general half: an item IRI links whatever the column is called, because the
    /// shape can be nothing else.
    #[test]
    fn an_item_iri_links_under_any_variable_name() {
        let rules = rules();
        for var in ["item", "s", "whatever_the_author_called_it"] {
            let action = rules
                .action(var, "uri", "urn:iki:ledger:default:item:01m2kw53", "other")
                .unwrap_or_else(|| panic!("no action under `{var}`"));
            // The ledger in the IRI wins over the ledger being queried: the IRI says where
            // the item actually lives.
            assert_eq!(action.href, "/l/default/item/01m2kw53");
            assert_eq!(action.kind, "link");
            assert!(
                action
                    .label
                    .contains("urn:iki:ledger:default:item:01m2kw53"),
                "the visible text is part of the accessible name: {action:?}"
            );
        }
        // Not an item IRI: the ledger's own vocabulary, and a near-miss shape.
        assert!(rules
            .action(
                "status",
                "uri",
                "https://ikigai-rs.dev/ns/ledger#open",
                "default"
            )
            .is_none());
        assert!(rules
            .action("item", "uri", "urn:iki:ledger:default:item:a/b", "default")
            .is_none());
        assert!(rules
            .action("item", "uri", "urn:iki:ledger:default:item:", "default")
            .is_none());
    }

    /// ⚠ The ambiguous half, stated as a test: `number` links and `comments` does not, and
    /// the ONLY thing separating them is the column name. This is the counterexample from
    /// the screenshot that prompted the work — `244 … 3` must not make 3 a link.
    #[test]
    fn a_bare_integer_links_only_under_a_column_that_names_items() {
        let rules = rules();
        let linked = rules.action("number", "literal", "244", "default").unwrap();
        assert_eq!(linked.href, "/l/default/item/244");
        assert_eq!(linked.label, "Open item 244 in default");
        assert!(rules
            .action("comments", "literal", "3", "default")
            .is_none());
        assert!(rules
            .action("priority", "literal", "1", "default")
            .is_none());
        // Not an integer at all.
        assert!(rules
            .action("number", "literal", "12a", "default")
            .is_none());
        assert!(rules.action("number", "literal", "-1", "default").is_none());
        assert!(rules
            .action("number", "literal", "007", "default")
            .is_none());
        assert!(rules.action("number", "literal", "", "default").is_none());
    }

    /// ★ The repo label leaves the current result set: it runs a FRESH query for the OPEN
    /// items with that label, so an item the first query never returned still appears.
    #[test]
    fn a_repo_label_runs_the_open_items_for_that_repo() {
        let rules = rules();
        let action = rules
            .action("repo", "literal", "repo:ikigai-gonk", "default")
            .unwrap();
        assert_eq!(action.kind, "query");
        assert!(
            action.href.starts_with("/sparql?ledger=default&query="),
            "{action:?}"
        );
        assert_eq!(action.label, "Open items labelled repo:ikigai-gonk");
        let query = decode(action.href.split("query=").nth(1).unwrap());
        assert!(query.contains("ledger:status ledger:open"), "{query}");
        assert!(
            query.contains(r#"ledger:label "repo:ikigai-gonk""#),
            "{query}"
        );
        // A label is a literal; the same text as an IRI is not a repo label.
        assert!(rules
            .action("repo", "uri", "repo:ikigai-gonk", "default")
            .is_none());
        assert!(rules
            .action("repo", "literal", "security", "default")
            .is_none());
    }

    /// ⚠ The escaping that matters: a label holding quotes, braces and a newline cannot
    /// change the SHAPE of the query it runs — it stays one string literal.
    #[test]
    fn a_hostile_label_cannot_reshape_the_query_it_runs() {
        let rules = rules();
        let hostile = "repo:x\" } INSERT DATA { <a:b> <a:c> \"pwned\" } # ";
        let action = rules.action("repo", "literal", hostile, "default").unwrap();
        let query = decode(action.href.split("query=").nth(1).unwrap());
        // The hostile text is present — as CONTENT. Every quote in it is escaped, so the
        // literal on that line still opens and closes exactly twice and the `}` never
        // terminates the WHERE group.
        assert!(
            query.contains(r#"\" } INSERT DATA { <a:b> <a:c> \"pwned\" }"#),
            "{query}"
        );
        let line = query
            .lines()
            .find(|l| l.contains("ledger:label"))
            .expect("the label line");
        assert_eq!(
            line.replace("\\\"", "").matches('"').count(),
            2,
            "unescaped quotes in {line}"
        );
        // ★ And it is interpolated in exactly ONE place — the literal. A comment is a
        // weaker container, so the template must not carry the value into one.
        assert_eq!(
            query.matches("INSERT DATA").count(),
            1,
            "the value reached more than the string literal: {query}"
        );
        // A newline in a label stays inside the literal rather than ending the line.
        let multi = rules
            .action("repo", "literal", "repo:a\nrepo:b", "default")
            .unwrap();
        let query = decode(multi.href.split("query=").nth(1).unwrap());
        assert!(
            query.contains(r#"ledger:label "repo:a\nrepo:b""#),
            "{query}"
        );
    }

    /// The link grammar: nothing a value contains can add a path segment or a parameter.
    #[test]
    fn a_hostile_value_cannot_escape_the_url_it_is_substituted_into() {
        let rules = rules();
        // The item-IRI shape refuses a slash outright, so the value never reaches a URL.
        assert!(rules
            .action("item", "uri", "urn:iki:ledger:a/../b:item:x", "default")
            .is_none());
        // And where a value DOES reach one — the ledger a caller may name in the query
        // string — the encoder is what stands between it and the path.
        assert_eq!(percent("../../etc"), "..%2F..%2Fetc");
        assert_eq!(percent("a&b=c"), "a%26b%3Dc");
        let escaped = rules
            .action("number", "literal", "244", "../../etc")
            .unwrap();
        assert_eq!(escaped.href, "/l/..%2F..%2Fetc/item/244");
    }

    #[test]
    fn a_rule_reaching_for_an_unmatched_binding_emits_nothing() {
        let table = Rules::parse(
            r#"@prefix render: <urn:iki:gonk:render#> .
               <urn:x:r> a render:Rule ; render:link "/l/{itemLedger}/item/{itemId}" ."#,
        )
        .unwrap();
        // The rule states no shape, so it is TRIED on a plain literal — and produces
        // nothing, because the bindings it names are empty.
        assert!(table.action("n", "literal", "hello", "default").is_none());
        assert!(table
            .action("n", "uri", "urn:iki:ledger:d:item:x", "default")
            .is_some());
    }

    #[test]
    fn a_table_that_cannot_be_obeyed_is_refused_rather_than_skipped() {
        let bad = |ttl: &str| Rules::parse(ttl).unwrap_err();
        let prefix = "@prefix render: <urn:iki:gonk:render#> .\n";
        assert!(bad(&format!("{prefix}<urn:x:r> a render:Rule .")).contains("does nothing"));
        assert!(bad(&format!(
            "{prefix}<urn:x:r> a render:Rule ; render:link \"/a\" ; render:query \"SELECT\" ."
        ))
        .contains("may state one"));
        assert!(bad(&format!(
            "{prefix}<urn:x:r> a render:Rule ; render:link \"/a\" ; render:matchShape \"colour\" ."
        ))
        .contains("matchShape"));
        assert!(bad(&format!(
            "{prefix}<urn:x:r> a render:Rule ; render:link \"/a\" ; render:matchKind \"iri\" ."
        ))
        .contains("term kind"));
        assert!(bad(&format!(
            "{prefix}<urn:x:r> a render:Rule ; render:link \"/a\" ; render:order \"soon\" ."
        ))
        .contains("render:order"));
        assert!(bad("not turtle at all").contains("not Turtle"));
        // An empty table is legal: it renders exactly the page gonk rendered before.
        assert!(Rules::parse("").unwrap().is_empty());
    }

    fn decode(encoded: &str) -> String {
        let bytes = encoded.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }
}
