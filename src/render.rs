//! The HTML face's renderer: a graph face, re-serialized as RDF/XML, through ONE stylesheet.
//!
//! ```text
//! urn:iki:ledger:…:items as=text/turtle      the ledger's own graph face
//!   → + view triples (display strings, hrefs, what this caller may do)
//!   → RDF/XML, each subject's primary rdf:type FIRST, so it is a typed node element
//!   → wrapped in a <view:page> envelope
//!   → web/gonk.xsl (xrust, server-side)   ← `match="ledger:Item"` IS the rdf:type dispatch
//!   → HTML serialization
//! ```
//!
//! # ★ Why the view triples exist: the xrust subset, measured 2026-09-14
//!
//! The stylesheet runs on `xrust` 2.2 through `ikigai_xslt::transform_xml`, and that engine is
//! a SUBSET of XSLT 1.0. Probed directly rather than assumed:
//!
//! | works | silently wrong | refused |
//! |---|---|---|
//! | `xsl:if`, `xsl:choose`, `xsl:attribute`, `call-template`, modes, `apply-templates` + `xsl:sort`, `count()`, filtered `select` paths | attribute value templates beyond `{@name}` (empty), match-PATTERN predicates (ignored), `[1]` path predicates, `position()`, absolute paths from a nested template (empty) | `xsl:variable`, `xsl:sort` inside `for-each`, `xsl:key`, `string-length()` |
//!
//! ⚠ `xsl:sort` being refused inside `for-each` does NOT generalize: measured 2026-09-15,
//! `xsl:if test="@attr"` wrapping an `xsl:attribute` works inside a nested `for-each`
//! (`tests/web.rs::a_ledger_iri_reads_as_its_local_name_only_in_the_html_face` renders the
//! `title` it emits). `xsl:sort` is the exception there, not the rule.
//!
//! So everything a stylesheet would normally COMPUTE — an item's href from its IRI, `#12`,
//! whether this caller may close it — is computed here and handed over as a literal on the
//! subject it describes (`urn:iki:gonk:view#…`). They are presentation triples: they exist
//! only inside a page render, never in the store and never in a graph face.
//!
//! # ★ And the serialization: xrust writes XML, and XML is not HTML
//!
//! An empty element comes out self-closed — `<script src='…'/>`, `<textarea/>`,
//! `<div/>` — and an HTML parser reads `<script …/>` as an OPEN script element that swallows
//! the rest of the document. [`html`] rewrites every self-closed non-void element as an
//! open/close pair. Text and attribute values arrive escaped (`&lt;`, `&apos;`, `&quot;`),
//! which is what makes a `<` in a title safe to serve.

use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};

/// The namespace of the presentation triples — see the module docs. Not a published
/// vocabulary: nothing outside a page render ever carries it.
pub const VIEW_NS: &str = "urn:iki:gonk:view#";

/// The one stylesheet every page and fragment is rendered through.
pub const STYLESHEET: &str = include_str!("../web/gonk.xsl");

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const LEDGER_ITEM: &str = "https://ikigai-rs.dev/ns/ledger#Item";

/// Elements HTML defines as void — the only ones that may be written `<x/>`.
const VOID: [&str; 13] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// A graph under construction: a resource's own graph face plus view triples.
#[derive(Default)]
pub struct Graph {
    triples: Vec<Triple>,
}

impl Graph {
    /// Parse a Turtle graph face.
    pub fn from_turtle(bytes: &[u8]) -> Result<Graph, String> {
        let mut triples = Vec::new();
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(bytes) {
            let quad = quad.map_err(|e| format!("the graph face is not Turtle: {e}"))?;
            triples.push(quad.into());
        }
        Ok(Graph { triples })
    }

    /// Every triple.
    pub fn triples(&self) -> &[Triple] {
        &self.triples
    }

    /// Keep only the triples `keep` accepts.
    ///
    /// ★ **This is a render-cost control, not a filter for correctness.** `xrust` builds a
    /// node for every element in its input and pays again for every element it writes, so a
    /// page whose stylesheet reads ten predicates and is handed forty pays for thirty it
    /// never looks at — see `web::ROW_PREDICATES`.
    pub fn retain(&mut self, keep: impl FnMut(&Triple) -> bool) {
        self.triples.retain(keep);
    }

    /// Subjects carrying `rdf:type <class>`, in first-seen order.
    pub fn subjects_of_type(&self, class: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.triples {
            if t.predicate.as_str() == RDF_TYPE {
                if let (NamedOrBlankNode::NamedNode(s), Term::NamedNode(o)) =
                    (&t.subject, &t.object)
                {
                    if o.as_str() == class && !out.iter().any(|x| x == s.as_str()) {
                        out.push(s.as_str().to_string());
                    }
                }
            }
        }
        out
    }

    /// The lexical values of `subject predicate ?o`, literals as their value and IRIs as
    /// the IRI.
    pub fn values(&self, subject: &str, predicate: &str) -> Vec<String> {
        self.triples
            .iter()
            .filter(|t| t.predicate.as_str() == predicate)
            .filter(
                |t| matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if s.as_str() == subject),
            )
            .map(|t| match &t.object {
                Term::NamedNode(n) => n.as_str().to_string(),
                Term::Literal(l) => l.value().to_string(),
                other => other.to_string(),
            })
            .collect()
    }

    /// The first value of `subject predicate ?o`.
    pub fn value(&self, subject: &str, predicate: &str) -> Option<String> {
        self.values(subject, predicate).into_iter().next()
    }

    /// Add `subject view:{local} "value"`.
    pub fn view(&mut self, subject: &str, local: &str, value: impl Into<String>) {
        if let (Ok(s), Ok(p)) = (
            NamedNode::new(subject),
            NamedNode::new(format!("{VIEW_NS}{local}")),
        ) {
            self.triples
                .push(Triple::new(s, p, Literal::new_simple_literal(value.into())));
        }
    }

    /// Add `subject rdf:type <class>` — for a view node the stylesheet dispatches on.
    pub fn typed(&mut self, subject: &str, class: &str) {
        if let (Ok(s), Ok(p), Ok(o)) = (
            NamedNode::new(subject),
            NamedNode::new(RDF_TYPE),
            NamedNode::new(class),
        ) {
            self.triples.push(Triple::new(s, p, o));
        }
    }

    /// RDF/XML, without the XML declaration (it is embedded in an envelope).
    ///
    /// ★ **Each subject's primary type is its FIRST triple**, because that is what makes
    /// `oxrdfxml` write it as a typed node element — `<ledger:Item rdf:about=…>` rather than
    /// `<rdf:Description>` — and a typed node element is what lets a template match the type
    /// by NAME, which is the one kind of dispatch xrust gets right. `ledger:Item` wins when a
    /// subject has several types (an item with a level asserts both).
    pub fn rdfxml(&self) -> Result<String, String> {
        let mut triples = self.triples.clone();
        triples.sort_by_cached_key(|t| {
            let rank = if t.predicate.as_str() == RDF_TYPE {
                match &t.object {
                    Term::NamedNode(n) if n.as_str() == LEDGER_ITEM => 0,
                    _ => 1,
                }
            } else {
                2
            };
            (
                t.subject.to_string(),
                rank,
                t.predicate.to_string(),
                t.object.to_string(),
            )
        });
        triples.dedup();
        let mut serializer = RdfSerializer::from_format(RdfFormat::RdfXml);
        for (prefix, iri) in [
            ("ledger", "https://ikigai-rs.dev/ns/ledger#"),
            ("dcterms", "http://purl.org/dc/terms/"),
            ("view", VIEW_NS),
        ] {
            serializer = serializer
                .with_prefix(prefix, iri)
                .map_err(|e| format!("prefix {prefix}: {e}"))?;
        }
        let mut writer = serializer.for_writer(Vec::new());
        for triple in &triples {
            writer
                .serialize_triple(triple)
                .map_err(|e| format!("RDF/XML: {e}"))?;
        }
        let bytes = writer.finish().map_err(|e| format!("RDF/XML: {e}"))?;
        let text = String::from_utf8(bytes).map_err(|e| format!("RDF/XML: {e}"))?;
        Ok(strip_declaration(&text).to_string())
    }
}

fn strip_declaration(xml: &str) -> &str {
    let trimmed = xml.trim_start();
    match trimmed.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map(|(_, body)| body).unwrap_or(rest),
        None => trimmed,
    }
}

/// Escape text for an XML element or a double-quoted attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot carry most C0 controls at all, even escaped; a ledger title
            // containing one would make the whole page fail to parse.
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// An envelope element: `<view:{name} a="…">children</view:{name}>`, namespaces declared.
pub fn envelope(name: &str, attributes: &[(&str, &str)], children: &str) -> String {
    let mut out = format!(
        "<view:{name} xmlns:view=\"{VIEW_NS}\" \
         xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\""
    );
    for (key, value) in attributes {
        out.push_str(&format!(" {key}=\"{}\"", escape(value)));
    }
    out.push('>');
    out.push_str(children);
    out.push_str(&format!("</view:{name}>"));
    out
}

/// A child element in the view namespace carrying ELEMENTS rather than text — for a view
/// node the stylesheet walks into (`view:finding/view:decide/view:severity-option`).
///
/// ⚠ `children` is markup and is **not** escaped: build it from [`element`] or from this,
/// never from a caller's string. [`element`] is the leaf, and it escapes.
pub fn wrap(name: &str, attributes: &[(&str, &str)], children: &str) -> String {
    let mut out = format!("<view:{name}");
    for (key, value) in attributes {
        out.push_str(&format!(" {key}=\"{}\"", escape(value)));
    }
    out.push('>');
    out.push_str(children);
    out.push_str(&format!("</view:{name}>"));
    out
}

/// A child element in the view namespace (the namespace is declared on the envelope).
pub fn element(name: &str, attributes: &[(&str, &str)], text: &str) -> String {
    let mut out = format!("<view:{name}");
    for (key, value) in attributes {
        out.push_str(&format!(" {key}=\"{}\"", escape(value)));
    }
    out.push('>');
    out.push_str(&escape(text));
    out.push_str(&format!("</view:{name}>"));
    out
}

/// Transform an envelope through [`STYLESHEET`] and serialize the result as HTML. A full
/// page gets its doctype here, because xrust does not write one.
pub fn render(document: &str, full_page: bool) -> Result<String, String> {
    let xml = ikigai_xslt::transform_xml(document, STYLESHEET, false)?;
    let body = html(&xml);
    Ok(if full_page {
        format!("<!DOCTYPE html>\n{body}")
    } else {
        body
    })
}

/// XML serialization → HTML serialization: every self-closed NON-void element becomes an
/// open/close pair; the XML declaration is dropped. See the module docs for why.
///
/// ```
/// use ikigai_gonk::render::html;
/// assert_eq!(
///     html("<p><script src='/x.js'/><br/><textarea name='t'/></p>"),
///     "<p><script src='/x.js'></script><br/><textarea name='t'></textarea></p>"
/// );
/// ```
///
/// Correct on xrust's output because every literal `<` in text and in attribute values
/// arrives escaped, so a `<` always opens markup; a `>` inside a quoted attribute value is
/// skipped by tracking the quote.
pub fn html(xml: &str) -> String {
    let xml = strip_declaration(xml);
    let mut out = String::with_capacity(xml.len() + 64);
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tag = &rest[open..];
        let mut quote: Option<char> = None;
        let mut end = None;
        for (i, c) in tag.char_indices().skip(1) {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"') | (None, '\'') => quote = Some(c),
                (None, '>') => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            out.push_str(tag);
            return out;
        };
        let whole = &tag[..=end];
        let self_closed = whole.ends_with("/>")
            && !whole.starts_with("</")
            && !whole.starts_with("<!")
            && !whole.starts_with("<?");
        if self_closed {
            let name: String = whole[1..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
                .collect();
            if VOID.contains(&name.to_ascii_lowercase().as_str()) {
                out.push_str(whole);
            } else {
                out.push_str(whole[..whole.len() - 2].trim_end());
                out.push_str(&format!("></{name}>"));
            }
        } else {
            out.push_str(whole);
        }
        rest = &tag[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_closed_non_void_elements_are_opened_and_closed() {
        let got = html(
            "<?xml version='1.0'?><div class='a'/><input type='text' value='x/>y'/><meta charset='utf-8'/><span title='a &gt; b'/>",
        );
        assert_eq!(
            got,
            "<div class='a'></div><input type='text' value='x/>y'/><meta charset='utf-8'/><span title='a &gt; b'></span>"
        );
    }

    /// The stylesheet compiles under xrust and renders an empty page — so a stylesheet xrust
    /// refuses fails HERE, not as a 500 on every page.
    #[test]
    fn the_stylesheet_compiles_and_renders_a_page() {
        let doc = envelope(
            "page",
            &[
                ("view", "empty"),
                ("full", "true"),
                ("title", "T & <co>"),
                ("message", "m"),
            ],
            "",
        );
        let page = render(&doc, true).expect("the stylesheet renders");
        assert!(
            page.starts_with("<!DOCTYPE html>\n<html lang='en'>"),
            "{page}"
        );
        assert!(
            page.contains("<title>T &amp; &lt;co&gt; · gonk</title>"),
            "{page}"
        );
        assert!(page.contains("<main id='main'>"), "{page}");
    }

    #[test]
    fn escaping_covers_markup_quotes_and_controls() {
        assert_eq!(
            escape("<a href=\"x\">'&'\u{1}</a>"),
            "&lt;a href=&quot;x&quot;&gt;&apos;&amp;&apos;\u{FFFD}&lt;/a&gt;"
        );
    }

    /// ★ The dispatch depends on this: the primary type is the typed node element even
    /// when the graph face listed it after other predicates, and a second type stays a child.
    #[test]
    fn the_primary_type_becomes_the_element_name() {
        let turtle = br#"@prefix ledger: <https://ikigai-rs.dev/ns/ledger#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
<urn:iki:ledger:default:item:a> dcterms:title "T" ; a <urn:x:Level> , ledger:Item ."#;
        let mut graph = Graph::from_turtle(turtle).unwrap();
        graph.view("urn:iki:ledger:default:item:a", "short", "#1");
        let xml = graph.rdfxml().unwrap();
        assert!(
            xml.contains("<ledger:Item rdf:about=\"urn:iki:ledger:default:item:a\">"),
            "{xml}"
        );
        assert!(
            xml.contains("<rdf:type rdf:resource=\"urn:x:Level\"/>"),
            "{xml}"
        );
        assert!(xml.contains("<view:short>#1</view:short>"), "{xml}");
        assert!(!xml.contains("<?xml"), "{xml}");
    }
}
