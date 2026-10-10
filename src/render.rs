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
//! Also works, measured 2026-09-26: `xsl:comment` (emitted verbatim — [`slot`] rests on it)
//! and a `[@name = '…']` filter on a `select` path.
//!
//! # ★ And the cost: QUADRATIC in the nodes one transform creates (xrust 2.2.0)
//!
//! Ledger #519, step 3, bisected with `examples/xslbench.rs` and then profiled (`sample`,
//! release build with debug info): a document of 400 queue rows costs 59 ms per row where
//! 10 rows cost 23 — and rows of attributes alone barely grow (7.2 → 7.8 ms/row), rows with
//! a 2 KB body grow no more than that (bytes are free; NODES are not), rows with the decide
//! form and its option lists — some 125 result nodes each — triple. 54% of the samples sit
//! in two functions of xrust's `trees/smite.rs`: `unattached` (`u.borrow().iter().any(..)`
//! before every push onto a document's unattached-node list) and `detach`
//! (`iter().position(..)` then `Vec::remove` on the same list). That list is never
//! emptied: after 200 full rows the result document's holds 47,818 nodes and the parsed
//! source's 7,207 (~240 and ~36 per row; `ItemNode::unattached()` counts them), so every
//! node created pays a scan of every node created before it. Σ over a transform is
//! O(nodes²) — an engine internal, not a stylesheet construct; nothing authored here can
//! avoid it except making each transform small, which is what [`chunk`] does (the same
//! 400 rows as 40 documents of 10 cost 8.7 s instead of 23.6 s). Reported to the hub for
//! the upstream issue; the table is in `examples/xslbench.rs`.
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

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, ReprType,
    Representation, Request, Verb,
};
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

    /// Split this graph into windows of `size` subjects, in the order `subjects` gives —
    /// one graph per window, each holding exactly the triples whose subject is in it.
    /// A triple whose subject is in no window is in no output graph (a listing renders
    /// rows, and a node the row templates never read is input xrust would still pay for).
    pub fn windows(&self, subjects: &[String], size: usize) -> Vec<Graph> {
        subjects
            .chunks(size.max(1))
            .map(|window| Graph {
                triples: self
                    .triples
                    .iter()
                    .filter(|t| {
                        matches!(&t.subject, NamedOrBlankNode::NamedNode(s)
                            if window.iter().any(|w| w == s.as_str()))
                    })
                    .cloned()
                    .collect(),
            })
            .collect()
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

/// How many rows one chunk document carries — see [`chunk`].
///
/// ★ Measured, not chosen (`examples/xslbench.rs`, 2026-09-26, release build, 400 synthetic
/// queue rows through this stylesheet): one document 23.6 s; as documents of 1 row 13.7 s,
/// 2 rows 10.7 s, 5 rows 9.1 s, **10 rows 8.7 s**, 20 rows 8.7 s, 25 rows 8.9 s, 50 rows
/// 9.9 s, 100 rows 11.9 s. The curve is flat between 10 and 25 and rises both ways — below
/// it the per-document overhead shows, above it the superlinear term does. Ten is the low
/// end of the flat part, which is also the finer cache grain (`crate::queue`).
pub const CHUNK_ROWS: usize = 10;

/// The view value of a chunk document: `<view:page view="chunk">` holding rows and nothing
/// else, whose body template applies the row templates and no shell.
pub const CHUNK_VIEW: &str = "chunk";

/// A slot in a shell document: `<view:slot name="…"/>`, which the stylesheet renders as
/// the marker [`splice`] replaces with the rows rendered separately.
pub fn slot(name: &str) -> String {
    element("slot", &[("name", name)], "")
}

/// The marker the stylesheet writes for [`slot`] — an HTML comment, because xrust emits
/// `xsl:comment` verbatim (measured 2026-09-26) and a comment is the one node that can sit
/// inside any element without being markup of its own.
fn marker(name: &str) -> String {
    format!("<!--gonk-slot:{}-->", escape(name))
}

/// A chunk document: the page envelope around `rows` and nothing else.
///
/// # ★ Why a page is rendered in pieces (ledger #519)
///
/// xrust's cost per KB of input is not flat: measured on 2026-09-26, 10 queue rows cost
/// 11.6 ms/KB and 400 rows 33.7 ms/KB in one document — a superlinear term on top of the
/// ~12 ms/KB floor. The row templates (`view:finding`, `view:group`, `ledger:Item`) read
/// nothing outside their own element, so a document of ten rows renders each row exactly as
/// the whole page would; the shell (header, nav, filters, sentences) is rendered once with a
/// [`slot`] where the rows go, and [`splice`] joins the pieces. The HTML is byte-for-byte what
/// one transform produced — `tests/queue.rs` and `tests/web.rs` assert the markup and did not
/// change — and 400 rows cost 8.7 s instead of 23.6 s. ⚠ A row template that started reading
/// an ancestor or a sibling would break this silently; the shell carries every page-level
/// fact as an attribute (`@has-rows` on the ledger page, for one) so none needs to.
pub fn chunk(rows: &str) -> String {
    envelope("page", &[("view", CHUNK_VIEW)], rows)
}

/// `urn:iki:gonk:render` — one chunk document, rendered through [`STYLESHEET`].
pub const RENDER_IRI: &str = "urn:iki:gonk:render";

/// The most a chunk document may be. A chunk of [`CHUNK_ROWS`] queue rows is ~20 KB and
/// the largest batch group on the 2026-09-25 snapshot ~70 KB; anything near this is not a
/// chunk this crate built, and the bound REFUSES rather than spending a minute of xrust on
/// it (the resource is reachable by any caller with a door).
pub const MAX_CHUNK_BYTES: usize = 1 << 20;

/// The deepest a chunk document's elements may nest, counted by [`nesting_depth`] — the page
/// envelope is level 1.
///
/// # ★ Why there is a depth bound at all (ledger [#880](http://localhost:1060/l/default/item/880), S2)
///
/// xrust parses and transforms by RECURSION, one stack frame chain per level, so stack use
/// grows with nesting, not with size. Measured 2026-10-09 on a 2 MiB thread — the stack every
/// tokio worker gets, and the one this resource runs on: a document nested 250 levels
/// overflowed it in a release build, and 24 levels in a debug one. An overflow is not an
/// error a caller sees: it ABORTS THE PROCESS. And this resource requires nothing, so on
/// `77ac767` one GET of `/k?c=source urn:iki:gonk:render content=<view:page><a><a>…` — about
/// 1 KB, from any page in a browser on this machine, with no session — took gonk down.
///
/// gonk's own chunk documents nest a handful of levels (page, group, finding, decide,
/// option). Sixteen is several times that and below the DEBUG overflow, so the test suite can
/// render a document at the bound on an ordinary test thread; a release build has roughly ten
/// times the margin. ⚠ The real fix is one layer down (`ikigai-xslt` handing xrust input it
/// cannot survive), reported to the hub; this bound protects this resource, not every caller
/// of that crate.
pub const MAX_CHUNK_DEPTH: usize = 16;

/// How deeply `document`'s elements nest, read from its tags alone — before any parser sees
/// it, which is the point. A self-closed element counts as a level and closes it; a closing tag
/// ends one. Quotes inside a tag are honored, so a `>` in an attribute value does not end it.
///
/// It may count MORE than a parser would (a `<` inside a comment, which [`Render`] refuses
/// anyway) and never less for a document without declarations, so as a bound it fails closed.
///
/// ```
/// use ikigai_gonk::render::nesting_depth;
/// assert_eq!(nesting_depth("<a><b/><c><d>x</d></c></a>"), 3);
/// assert_eq!(nesting_depth("<a t=\"1>2\"><b></b></a>"), 2);
/// assert_eq!(nesting_depth(&"<a>".repeat(400)), 400);
/// assert_eq!(nesting_depth("text, no tags"), 0);
/// ```
pub fn nesting_depth(document: &str) -> usize {
    let bytes = document.as_bytes();
    let (mut depth, mut deepest, mut at) = (0usize, 0usize, 0usize);
    while let Some(open) = bytes[at..].iter().position(|&b| b == b'<').map(|i| at + i) {
        let mut end = open + 1;
        let mut quote: Option<u8> = None;
        while end < bytes.len() {
            match (quote, bytes[end]) {
                (Some(q), b) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b @ (b'"' | b'\'')) => quote = Some(b),
                (None, b'>') => break,
                (None, _) => {}
            }
            end += 1;
        }
        let tag = &bytes[open + 1..end.min(bytes.len())];
        match tag.first() {
            Some(b'/') => depth = depth.saturating_sub(1),
            Some(b'!' | b'?') => {}
            _ if tag.last() == Some(&b'/') => deepest = deepest.max(depth + 1),
            _ => {
                depth += 1;
                deepest = deepest.max(depth);
            }
        }
        at = end + 1;
        if at >= bytes.len() {
            break;
        }
    }
    deepest
}

/// Render each of `documents` (see [`chunk`]) through the kernel and concatenate the HTML.
///
/// # ★ Why through the kernel (ledger #519, step 2)
///
/// Each chunk is resolved as `urn:iki:gonk:render content=<document>` under the caller's
/// capability, and [`Render`] answers `.cacheable()` with **no golden thread**: the HTML is
/// a pure function of the document, so the cache key — the content-addressed request,
/// which hashes the inline argument — IS the invalidation. A poll that finds the queue
/// unchanged builds the same documents and every chunk is a hit; a decision changes one
/// row, so one document changes and one chunk is recomputed. Nothing has to cut anything.
///
/// This is deliberately not a chunk that READS the findings itself and caches under their
/// threads: `ikigai-browse` 0.13.0 answers `urn:repo:{root}:findings` `Expiry::Always`
/// (`tests/queue.rs::a_findings_read_is_not_cacheable_so_the_chunks_key_on_content`), and
/// expiry propagates, so such a chunk would be `Always` too and cache nothing. The reads
/// stay in the page, uncached and cheap; the render — the expensive half — is what is keyed.
///
/// The door kernels store nothing (`crate::doors::NoCache`), so the resource is bound in
/// the HUB ([`space`]), where the one cache in the process is.
pub async fn rendered_chunks(
    inv: &Invocation<'_>,
    documents: &[String],
) -> ikigai_core::Result<String> {
    let mut out = String::new();
    for document in documents {
        let request = Request::new(
            Verb::Source,
            ikigai_core::Iri::parse(RENDER_IRI).map_err(|e| Error::Endpoint(e.to_string()))?,
        )
        .with_arg("content", ArgRef::Inline(document.as_bytes().to_vec()));
        let html = inv.issue(request).await?;
        out.push_str(&String::from_utf8_lossy(&html.bytes));
    }
    Ok(out)
}

/// The endpoint behind [`RENDER_IRI`]: a chunk document in, its HTML out, cacheable and
/// pure. Bound in the hub by [`space`]; see [`rendered_chunks`] for why.
pub struct Render;

/// The space that binds [`Render`] — composed into the hub by `crate::compose_with`.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new(RENDER_IRI), Render)
        .named(crate::spaces::iri(crate::spaces::RENDER))
}

#[async_trait]
impl Endpoint for Render {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "`{RENDER_IRI}` answers Source only"
            )));
        }
        let document = inv.inline_str("content")?;
        if document.len() > MAX_CHUNK_BYTES {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: format!(
                    "{} bytes is more than the {MAX_CHUNK_BYTES} a chunk document may be",
                    document.len()
                ),
            });
        }
        // ★ Before any parser sees it (see [`MAX_CHUNK_DEPTH`]): a declaration can carry
        // nesting the tag count cannot see — an entity whose replacement text is markup — and
        // gonk's chunk documents carry no declaration, comment or CDATA section, so `<!` is
        // refused outright rather than parsed around.
        if document.contains("<!") {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: "a chunk document carries no `<!` (declaration, comment or CDATA): \
                         gonk builds none, and a declaration can nest what the depth bound \
                         cannot count"
                    .to_string(),
            });
        }
        let depth = nesting_depth(document);
        if depth > MAX_CHUNK_DEPTH {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: format!(
                    "the document nests {depth} levels deep, more than the {MAX_CHUNK_DEPTH} \
                     a chunk document may: the renderer recurses once per level, and a deep \
                     enough document overflows its stack"
                ),
            });
        }
        if !document.trim_start().starts_with("<view:page") {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: "not a chunk document: it does not start with `<view:page`".to_string(),
            });
        }
        let html = render(document, false).map_err(Error::Endpoint)?;
        Ok(Representation::new(
            ReprType::new("text/html").with_param("charset", "utf-8"),
            html.into_bytes(),
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "gonk-render"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-render")
            .title("Render one chunk of a gonk page")
            .summary(
                "One chunk document — a `<view:page view=\"chunk\">` envelope holding queue \
                 rows, batch groups or ledger items and nothing else — through gonk's one \
                 stylesheet, as HTML. A pure function of its input and cacheable with no \
                 golden thread: the content-addressed request is the key, so a page that \
                 builds the same chunk again is served from the cache and a page whose rows \
                 changed recomputes exactly the chunks that hold them. Refuses a document \
                 over 1 MiB or one that is not a view:page envelope.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("content")
                    .summary("the chunk document (see `render::chunk`); a pipe fills it")
                    .class("http://www.w3.org/2001/XMLSchema#string"),
            )
            .output("text/html")
    }
}

/// Put rendered rows into a rendered shell: each `(name, html)` replaces the marker its
/// [`slot`] left. A slot the shell did not render is an error, never a page missing its
/// rows — the stylesheet forgot to apply `view:slot` where that view puts it. ⚠ Except
/// when there is nothing to put there: a shell with no rows may leave the list out
/// altogether (the ledger page says "No items match." instead), and an empty slot then has
/// nowhere to go and nothing to lose.
pub fn splice(shell: String, slots: &[(String, String)]) -> Result<String, String> {
    let mut page = shell;
    for (name, html) in slots {
        let marker = marker(name);
        match page.find(&marker) {
            Some(at) => page.replace_range(at..at + marker.len(), html),
            None if html.is_empty() => {}
            None => {
                return Err(format!(
                    "the shell rendered no slot `{name}` for the rows to go into (the \
                     stylesheet does not apply view:slot in this view)"
                ))
            }
        }
    }
    Ok(page)
}

/// Transform an envelope through [`STYLESHEET`] and serialize the result as HTML. A full
/// page gets its doctype here, because xrust does not write one.
pub fn render(document: &str, full_page: bool) -> Result<String, String> {
    let xml = ikigai_xslt::transform_xml(document, STYLESHEET, false)?;
    let body = linkify(&html(&xml));
    Ok(if full_page {
        format!("<!DOCTYPE html>\n{body}")
    } else {
        body
    })
}

/// The attribute the stylesheet puts on an element whose TEXT is prose a person wrote — an
/// item's body, a comment's, a finding's — naming the ledger its `ledger #N` references
/// resolve in. [`linkify`] finds it in the rendered HTML.
pub const LINKIFY_ATTR: &str = "data-linkify";

/// Make the http(s) URLs and the `ledger #N` references in prose clickable, and NOTHING else
/// (ledger [#989](http://localhost:1060/l/default/item/989)).
///
/// # Why here and not in the stylesheet
///
/// A link inside a text node is a tokenizer: find a scheme, find where the URL ends, give back
/// the trailing `)`, `.` and `,` that belong to the sentence. XSLT 2.0 would do it with
/// `xsl:analyze-string`; xrust is a subset of 1.0 (the module docs' table) and refuses both
/// `xsl:variable` and `string-length()`, so even a recursive `substring-before` template could
/// not hold the URL it is trimming. And xrust's cost is quadratic in the nodes a transform
/// creates, so a node per word would be the slow way to do it. So the stylesheet MARKS the
/// element (`data-linkify='<ledger>'` on the element holding the text), and this pass rewrites
/// that element's text in the HTML the transform produced.
///
/// # Why it is safe on that HTML
///
/// It runs on xrust's serialization, where every `<` in text arrives as `&lt;`, `'` as
/// `&apos;` and `"` as `&quot;` ([`html`]'s docs say so and its tests pin it). So the marked
/// element's text runs to the next `<` and contains no markup; a URL stops at whitespace or
/// at any of those entities, so it can neither close the `href` attribute nor open a tag;
/// and the link text is the same escaped bytes, unchanged. A marker can only come from the
/// stylesheet: in text, the `'` it needs is `&apos;`.
///
/// - **Only `http://` and `https://`** become links; `javascript:`, `data:` and the rest stay
///   text.
/// - **A trailing `.` `,` `;` `:` `!` `?` and an unbalanced `)` stay outside the link**:
///   `(see https://a.example/x).` links `https://a.example/x`.
/// - **`ledger #N`** links to `/l/<ledger>/item/N` (gonk's convention for an item reference in
///   prose, field guide 9i).
/// - Everything else is text: no markdown, no HTML.
///
/// ```
/// use ikigai_gonk::render::linkify;
/// let html = "<div data-linkify='default' class='body'>see https://a.example/x?y=1&amp;z=2). \
///             and ledger #12, not javascript:alert(1) or &lt;b&gt;</div>";
/// assert_eq!(
///     linkify(html),
///     "<div data-linkify='default' class='body'>see \
///      <a href='https://a.example/x?y=1&amp;z=2'>https://a.example/x?y=1&amp;z=2</a>). \
///      and <a href='/l/default/item/12'>ledger #12</a>, not javascript:alert(1) or \
///      &lt;b&gt;</div>"
/// );
/// // Text outside a marked element is never touched.
/// assert_eq!(linkify("<p>https://a.example</p>"), "<p>https://a.example</p>");
/// ```
pub fn linkify(html: &str) -> String {
    let marker = format!("{LINKIFY_ATTR}='");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(&marker) {
        let value_start = at + marker.len();
        let Some(value_len) = rest[value_start..].find('\'') else {
            break;
        };
        let ledger = &rest[value_start..value_start + value_len];
        // The end of this tag: the attributes after the marker are quoted, so track quotes.
        let mut quote: Option<char> = None;
        let mut tag_end = None;
        for (i, c) in rest[value_start + value_len + 1..].char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"') | (None, '\'') => quote = Some(c),
                (None, '>') => {
                    tag_end = Some(value_start + value_len + 1 + i);
                    break;
                }
                _ => {}
            }
        }
        let Some(tag_end) = tag_end else { break };
        let text_start = tag_end + 1;
        let text_len = rest[text_start..]
            .find('<')
            .unwrap_or(rest.len() - text_start);
        out.push_str(&rest[..text_start]);
        out.push_str(&linkify_text(
            &rest[text_start..text_start + text_len],
            ledger,
        ));
        rest = &rest[text_start + text_len..];
    }
    out.push_str(rest);
    out
}

/// One run of escaped text, linked. See [`linkify`].
fn linkify_text(text: &str, ledger: &str) -> String {
    let ledger_ok = !ledger.is_empty()
        && ledger
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let boundary = |at: usize| {
        text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    };
    while i < text.len() {
        let here = &text[i..];
        if boundary(i) {
            if let Some(len) = url_at(here) {
                let url = &here[..len];
                out.push_str(&format!("<a href='{url}'>{url}</a>"));
                i += len;
                continue;
            }
            if ledger_ok {
                if let Some(len) = ledger_ref_at(here) {
                    let number = here[..len].rsplit('#').next().unwrap_or("");
                    out.push_str(&format!(
                        "<a href='/l/{ledger}/item/{number}'>{}</a>",
                        &here[..len]
                    ));
                    i += len;
                    continue;
                }
            }
        }
        let c = here.chars().next().expect("not at the end");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The length of the http(s) URL that `text` starts with, trailing punctuation given back to
/// the sentence; `None` when it starts with none.
fn url_at(text: &str) -> Option<usize> {
    let scheme = ["https://", "http://"].into_iter().find(|scheme| {
        text.len() >= scheme.len() && text[..scheme.len()].eq_ignore_ascii_case(scheme)
    })?;
    // Where the URL ends: whitespace, markup (never present here), or an entity for a
    // character no URL in prose carries unescaped (`<`, `>`, `"`, `'`). `&amp;` stays: it is
    // a query string's `&`, and it is already the attribute's escaping of one.
    let mut end = text.len();
    for (i, c) in text.char_indices() {
        let tail = &text[i..];
        if c.is_whitespace()
            || c == '<'
            || c == '"'
            || c == '\''
            || ["&lt;", "&gt;", "&quot;", "&apos;", "&#"]
                .iter()
                .any(|entity| tail.starts_with(entity))
        {
            end = i;
            break;
        }
    }
    let mut url = &text[..end];
    loop {
        let trimmed = match url.chars().next_back() {
            Some('.' | ',' | ';' | ':' | '!' | '?') => &url[..url.len() - 1],
            Some(')') if url.matches(')').count() > url.matches('(').count() => {
                &url[..url.len() - 1]
            }
            _ => break,
        };
        url = trimmed;
    }
    (url.len() > scheme.len()).then_some(url.len())
}

/// The length of the `ledger #N` reference that `text` starts with, if it does.
fn ledger_ref_at(text: &str) -> Option<usize> {
    const WORD: &str = "ledger #";
    if text.len() < WORD.len() || !text[..WORD.len()].eq_ignore_ascii_case(WORD) {
        return None;
    }
    let digits = text[WORD.len()..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let end = WORD.len() + digits;
    let after_ok = text[end..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric());
    (digits > 0 && digits <= 12 && after_ok).then_some(end)
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
