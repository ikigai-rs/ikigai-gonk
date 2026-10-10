//! `urn:sparql:{select,ask,construct,describe}` — SPARQL as addressable resources, over the
//! store this process holds, confined to the graphs the CALLER may read (ledger
//! [#836](http://localhost:1060/l/default/item/836)).
//!
//! ```text
//! urn:sparql:select     Source  SELECT     →  urn:iki:store:graph-select     urn:cap:store:read:graph:*
//! urn:sparql:ask        Source  ASK        →  urn:iki:store:graph-ask        urn:cap:store:read:graph:*
//! urn:sparql:construct  Source  CONSTRUCT  →  urn:iki:store:graph-construct  urn:cap:store:read:graph:*
//! urn:sparql:describe   Source  DESCRIBE   →  urn:iki:store:graph-describe   urn:cap:store:read:graph:*
//! ```
//!
//! # Why this exists
//!
//! `urn:sparql:*` is the query face clients already speak — `ikigai-web`'s `/sparql` at 8642
//! reaches it through a `web.mount`, agents through the REPL and `ikigai mcp`, and
//! `claude/class/review-queries.md` is written against it. Until this module the only one on
//! the machine was the dev server's, so retiring the dev server would have dropped the face.
//!
//! # A MAPPING, not a second query engine
//!
//! Each form is one hop onto the store's graph-scoped twin, which already enforces
//! `urn:cap:store:read:graph:<iri>` for every graph named and confines the query's dataset BY
//! CONSTRUCTION (`graph=` is set on the prepared query as `FROM … FROM NAMED …`). Nothing
//! here parses, evaluates or serializes SPARQL, and nothing reaches the store's broad doors
//! or a raw `ikigai_sparql::space_with_store` — that is the uncapped face `ikigai-store`'s
//! header warns about, which answers an unrestricted query to whoever reaches it.
//!
//! The one thing that is code, and why a mapping rule could not do it: **the default
//! dataset**. With no `graph` named, the dataset is the union of the graphs the caller may
//! read, and that set is itself a resource (`urn:iki:store:graphs`, answered per caller's
//! grant). Filling one resource's argument from another's answer is a composition with a
//! read in it; an [`AliasTable`](ikigai_core::AliasTable) rewrites the NAME and carries the
//! arguments unchanged, and gonk links no composition language (`ikigai-lisp`) to store one
//! in. So it is a few lines of Rust, and only that: read the set, pass it on.
//!
//! # ★ The default dataset, stated — because the same query text has meant two things
//!
//! Ledger [#378](http://localhost:1060/l/default/item/378): `urn:iki:store:select` reads the
//! store's DEFAULT graph while the dev server's `urn:sparql:select` read the UNION, and a
//! census came out wrong by exactly the 489 vocabulary quads before anyone noticed. This face
//! answers with the union, as the dev server's did, so the clients written against that face
//! keep their meaning — and every form's description carries [`DEFAULT_DATASET`] verbatim, so
//! the difference from the store's broad doors is in the contract rather than in a memory.
//!
//! # Sealed
//!
//! - **No write form.** `urn:sparql:update` is not bound — the dev server bound one over its
//!   shared store, and this face does not inherit it — and `urn:iki:store:update`,
//!   `graph-update` and `load` are never issued from here. An update passed as `query` is not
//!   a query, and the store refuses it before anything runs.
//! - **No graph outside the caller's grant.** The default is computed from the caller's own
//!   grant; a named graph needs its own read token. ★ A named graph the caller cannot read is
//!   `Denied` **whether or not it exists** — the store checks the grant before it looks — so
//!   the face is not an existence oracle. A graph the caller holds a grant for and nothing has
//!   been written to answers empty, which tells it nothing `urn:iki:store:graphs` does not.
//! - **No authority of its own.** Every hop is [`Invocation::issue`], the caller's capability
//!   unchanged; the corridor is the door that admitted the request (root on the socket, a
//!   `client add` grant over QUIC, a passkey's grant over HTTP), and nothing threads a
//!   principal through by hand.
//!
//! # Economics
//!
//! The answer IS the store's answer: cacheable, keyed on the capability fingerprint and hung
//! on the store's write threads exactly when the store says the read is covered (a graph the
//! browse sharer writes is not, by its own promise), plus the readable-set read it was
//! computed from, which hangs on the same threads. Over the socket and QUIC doors it crosses
//! as `Expiry::Always` like every other threaded answer ([`crate::doors::for_the_wire`]).

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri,
    Representation, Request, Resolution, Result, Scope, Space, SpaceEntry, Topology, Verb,
};

/// The default dataset of every form, in one sentence — what [`Description::summary`] says
/// first and what `tests/sparql.rs` reads back through the Meta face.
///
/// ★ Written for the reader who has just been bitten by ledger
/// [#378](http://localhost:1060/l/default/item/378): it names the set, the idiom for one
/// graph, and the one graph that is never in it.
pub const DEFAULT_DATASET: &str = "Default dataset: with no `graph` named, the query reads the \
     UNION of every named graph this capability may read (the graphs `urn:iki:store:graphs` \
     lists for it), so a bare `{ ?s ?p ?o }` matches a triple in any of them and \
     `GRAPH ?g { … }` binds each one; to read a single graph write \
     `GRAPH <iri> { … }`, or name it in `graph`; the store's own unnamed default graph is never \
     in the dataset.";

/// The four forms: `(form, description id, answers with a graph?)`.
///
/// ★ The ids are `ikigai-sparql`'s own (`sparql-{form}`), so the catalog subject and the MCP
/// tool name a client of the dev server's face knew are the ones it finds here. Nothing in
/// this binary links `ikigai-sparql`, so they cannot collide.
const FORMS: [(&str, &str, bool); 4] = [
    ("select", "sparql-select", false),
    ("ask", "sparql-ask", false),
    ("construct", "sparql-construct", true),
    ("describe", "sparql-describe", true),
];

/// What SELECT and ASK serialize as — the store's faces, default first. The same default as
/// `ikigai-sparql`'s.
const RESULT_FACES: [&str; 4] = [
    "application/sparql-results+json",
    "application/sparql-results+xml",
    "text/csv",
    "text/tab-separated-values",
];

/// What CONSTRUCT and DESCRIBE serialize as — the two the store serves. ★ Turtle FIRST, which
/// is `ikigai-sparql`'s default and not the store's (N-Triples): a client of the dev server's
/// face that sends no `as` keeps getting Turtle. `ikigai-sparql` also offered N-Quads, TriG,
/// RDF/XML and JSON-LD; the store serves neither, and declaring a face nobody serves would make
/// the manifold over-offer.
const GRAPH_FACES: [&str; 2] = ["text/turtle", "application/n-triples"];

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The store resource that lists the graphs a caller may read.
const GRAPHS: &str = "urn:iki:store:graphs";

/// Bind the four forms — composed into the hub by [`crate::compose_with`], and so reachable
/// through all three doors as the store's and the ledger's names are.
pub fn space() -> EndpointSpace {
    FORMS
        .iter()
        .fold(EndpointSpace::new(), |space, &(form, id, graph_shaped)| {
            space.bind(
                Exact::new(format!("urn:sparql:{form}")),
                Form {
                    form,
                    id,
                    graph_shaped,
                },
            )
        })
        .named(crate::spaces::iri(crate::spaces::SPARQL))
}

/// One query form at `urn:sparql:{form}`.
struct Form {
    form: &'static str,
    id: &'static str,
    graph_shaped: bool,
}

#[async_trait]
impl Endpoint for Form {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "`urn:sparql:{}` answers Source only: it is a query, and no write is reachable \
                 under `urn:sparql:`",
                self.form
            )));
        }
        let query = inv.inline_str("query")?;
        // Refused here, before the readable set is read. ★ For LENGTH this is the only check
        // on the caller's query: the store's overlay ([`bounded`]) sees it at depth 1, where it
        // checks nesting only (ledger #965), so this line is load-bearing, not an early copy.
        admit("query", query.as_bytes(), Text::Query)?;
        let graph = match optional(inv, "graph")? {
            Some(named) if !named.trim().is_empty() => graph_list(named),
            _ => self.readable(inv).await?,
        };
        let faces: &[&str] = if self.graph_shaped {
            &GRAPH_FACES
        } else {
            &RESULT_FACES
        };
        let face = optional(inv, "as")?.unwrap_or(faces[0]);
        let target =
            Iri::parse(format!("urn:iki:store:graph-{}", self.form)).expect("a constant store IRI");
        let mut request = Request::new(Verb::Source, target)
            .with_arg("query", inline(query))
            .with_arg("graph", inline(&graph))
            .with_arg("as", inline(face));
        if let Some(bindings) = optional(inv, "bindings")? {
            request = request.with_arg("bindings", inline(bindings));
        }
        // ★ The caller's time budget goes with the query (ledger #964/#979): it can only lower
        // what the store gives this capability, and it is how an ANONYMOUS HTTP caller's
        // stamped budget ([`crate::budget`]) reaches the store through this face. Dropping it
        // here would hand that caller the store's 5 s base instead of the door's.
        if let Some(budget) = optional(inv, "budget")? {
            request = request.with_arg("budget", inline(budget));
        }
        inv.issue(request).await
    }

    fn name(&self) -> &str {
        self.id
    }

    fn describe(&self) -> Description {
        let form = self.form.to_uppercase();
        let faces: &[&str] = if self.graph_shaped {
            &GRAPH_FACES
        } else {
            &RESULT_FACES
        };
        let desc = Description::new(self.id)
            .title(format!(
                "SPARQL {form} over the graphs this capability may read"
            ))
            .summary(format!(
                "{DEFAULT_DATASET} Evaluate a SPARQL {form} against this server's store, \
                 read-only. With `graph`, the dataset is exactly the graphs named, each needing \
                 `urn:cap:store:read:graph:<iri>`: one the caller cannot read is refused \
                 (Denied) whether or not it exists, never answered over the rest. A query \
                 carrying its own `FROM` / `FROM NAMED` is refused (name the graphs in `graph` \
                 instead), and so is one whose answer is of the other family (a graph where \
                 this form answers with a result set, or the reverse); SELECT and ASK share a \
                 family, as CONSTRUCT and DESCRIBE do. An update is not a query and is refused; \
                 no write is reachable under `urn:sparql:`. Answered by \
                 `urn:iki:store:graph-{}` under the caller's own capability.",
                self.form
            ))
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(
                ArgSpec::new("query")
                    .summary(format!("A SPARQL {form} query."))
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("graph")
                    .summary(
                        "Optional: the named graphs to read instead of the default dataset — \
                         one IRI, or several separated by commas or whitespace, as \
                         `urn:sparql:*` has always spelled it. Every one needs its read grant. \
                         (A graph whose IRI contains a comma cannot be named here; use \
                         `urn:iki:store:graph-*`, which separates by whitespace only.)",
                    )
                    // A LIST of IRIs, and an ArgSpec has no way to say "many" — the wire's own
                    // class, as `ikigai-sparql` and the store declare for theirs.
                    .class(XSD_STRING)
                    .optional(),
            )
            .input(
                ArgSpec::new("bindings")
                    .summary(
                        "Optional: values for variables in the query, as a JSON object of name \
                         → value, so a value never passes through the SPARQL parser as syntax. \
                         The store's `bindings`, passed through unchanged; its contract at \
                         `urn:iki:store:graph-select` has the term shapes.",
                    )
                    .class(XSD_STRING)
                    .optional(),
            )
            .input(
                ArgSpec::new("budget")
                    .summary(
                        "Optional: a time budget in milliseconds for this evaluation, passed to \
                         the store. It can only LOWER the budget this capability gets there, \
                         never raise it; past it the request is refused with a typed timeout, \
                         never a partial answer. An anonymous HTTP caller's is set by the door.",
                    )
                    .class("http://www.w3.org/2001/XMLSchema#integer")
                    .optional(),
            )
            .input(
                ArgSpec::new("as")
                    .summary(format!(
                        "Result serialization; one of {}. An `as` this form cannot answer in is \
                         refused, never substituted.",
                        faces.join(", ")
                    ))
                    .class(XSD_STRING)
                    .one_of(faces.iter().copied())
                    .default_value(faces[0])
                    .optional(),
            );
        faces.iter().fold(desc, |desc, face| desc.output(*face))
    }
}

impl Form {
    /// The default dataset: every named graph this caller may read, as the store lists them
    /// for this caller's own grant — sorted IRIs, which joined by single spaces is already
    /// the store's canonical spelling of a set, so the read is never reissued.
    ///
    /// ⚠ An EMPTY set is refused, not evaluated, for the reason the store refuses an empty
    /// `graph=`: an empty dataset answers every query with nothing, and a `COUNT` of zero
    /// over it reads like a fact about the data. `InvalidArgument`, as the store's own empty
    /// refusal is, so it is never cached and the first write that makes a graph readable is
    /// seen on the next read.
    async fn readable(&self, inv: &Invocation<'_>) -> Result<String> {
        let target = Iri::parse(GRAPHS).expect("a constant store IRI");
        let answer = inv.issue(Request::new(Verb::Source, target)).await?;
        let listed = String::from_utf8_lossy(&answer.bytes);
        let graphs: Vec<&str> = listed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if graphs.is_empty() {
            return Err(Error::InvalidArgument {
                name: "graph".to_string(),
                detail: format!(
                    "none named, and this capability may read no named graph in this store, so \
                     the default dataset of `urn:sparql:{}` is empty. An empty dataset is \
                     refused rather than evaluated: it would answer every query with nothing. \
                     `{GRAPHS}` lists what this capability may read",
                    self.form
                ),
            });
        }
        Ok(graphs.join(" "))
    }
}

/// A `graph` value as `urn:sparql:*` spells it — commas or whitespace between IRIs — in the
/// whitespace-separated form the store takes. The store canonicalizes order and repetition
/// itself.
///
/// ```
/// assert_eq!(
///     ikigai_gonk::sparql::graph_list("urn:a, urn:b\turn:c,urn:d"),
///     "urn:a urn:b urn:c urn:d"
/// );
/// ```
///
/// ★ A comma is a separator HERE and not at the store, and that is a choice with a cost
/// worth saying: the store reads `urn:a,urn:b` as ONE graph because an IRI may contain a
/// comma, and its refusal says so — but only when the caller lacks that token. Root holds
/// every token, so on the socket the store would answer an empty dataset for a graph nobody
/// wrote, silently. `urn:sparql:*` has always split on commas (`ikigai-sparql`), so a client
/// of that face means a list; the rare graph with a comma in its IRI goes to the store's
/// door directly.
pub fn graph_list(named: &str) -> String {
    named
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// An argument that may be absent, and is refused when present but unreadable — never read
/// as absent, which would change what the query reads.
fn optional<'a>(inv: &Invocation<'a>, name: &str) -> Result<Option<&'a str>> {
    match inv.request.args.get(name) {
        None => Ok(None),
        Some(ArgRef::Inline(bytes)) => {
            std::str::from_utf8(bytes)
                .map(Some)
                .map_err(|e| Error::InvalidArgument {
                    name: name.to_string(),
                    detail: format!("is present but not valid UTF-8 ({e})"),
                })
        }
        Some(_) => Err(Error::InvalidArgument {
            name: name.to_string(),
            detail: "must be given inline".to_string(),
        }),
    }
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

// ------------------------------------------------------------------------- the bound

/// The deepest nesting gonk lets reach a SPARQL parser: `(`, `{`, `[`, `<<` and the negation `!`,
/// mixed, counted by [`nesting_depth`] (ledger [#915](http://localhost:1060/l/default/item/915)).
///
/// ★ **Why a bound at all.** `spargebra` — the parser behind `oxigraph`, the store, and
/// [`crate::web::query_form`] — reads a bracketed expression by recursion, and a stack overflow
/// is not a panic: Rust ABORTS the process. On `b16f79c` one GET of
/// `/sparql/results?query=SELECT * WHERE { FILTER(((…3000 parens…1…))) }` killed gonk. Measured
/// with `cargo run --release --example sparql-depth` on a 2 MiB thread (the size every tokio
/// worker and every `std::thread` got before [`crate::stack`]): a release build aborts past
/// 885 parentheses, 846 braces or 606 brackets; a DEBUG build past 184, 178 and 99 — the last
/// one through the store, which nests blank-node lists more expensively than the parser alone.
///
/// 64 is under every one of those, so a test at the bound runs in a debug test thread, and it
/// is far above anything gonk's own queries, its sample queries or the gonk Book write (the
/// deepest is under 10). ★ **The store's own pre-parse bound** (ledger #915, `ikigai-store`)
/// supersedes this one for the store's doors when it lands; this scan stays, because it is
/// one linear pass at the edge and it also covers the parses that are not the store's (the
/// HTML editor's [`crate::web::query_form`], the review space's `match`).
pub const MAX_NESTING: usize = 64;

/// The longest SPARQL QUERY text gonk admits: 32 KiB.
///
/// ⚠ **Nesting is not the only way to build a deep tree.** `1+1+1+…`, `true&&true&&…`,
/// `{} UNION {} UNION …`, `{} OPTIONAL {} …` and a path `a/a/a/…` contain no brackets at all,
/// and each becomes a left-deep tree that the store's optimizer and evaluator walk by
/// recursion. On a 2 MiB stack (release) the store aborted on 3,270 `+1`s — 6.5 KB of query —
/// and on 1,127 `UNION`s. No bracket scan sees those. So the bound is in two parts: the stacks
/// are larger ([`crate::stack::THREAD_STACK_BYTES`]) and the text is bounded, and the pair is
/// chosen so that the densest chain measured cannot fill a stack inside the length.
///
/// Applied to QUERIES only — the four read forms and the review space's `match`. An UPDATE is
/// not length-bounded, because the ledger and the browse family write long literals through
/// `urn:iki:store:graph-update` themselves (an explanation is kilobytes of Markdown); an update
/// is nesting-bounded like a query, and reaches the store only under a raw write token.
pub const MAX_QUERY_BYTES: usize = 32 * 1024;

/// What a SPARQL text is, for [`admit`]: a query (nesting and length bounded) or an update
/// (nesting bounded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// SELECT, ASK, CONSTRUCT or DESCRIBE.
    Query,
    /// An update request.
    Update,
}

/// Refuse a SPARQL text that is past [`MAX_NESTING`] or (a query) past [`MAX_QUERY_BYTES`],
/// as `InvalidArgument` on `name` — `400` at the HTTP door, never cached, nothing parsed.
/// Refused, never truncated: a shortened query is a different question.
///
/// ```
/// use ikigai_core::Error;
/// use ikigai_gonk::sparql::{admit, Text, MAX_NESTING};
///
/// // `{` and `FILTER(` are two levels; the rest are parentheses.
/// let nested = |n: usize| format!("SELECT * WHERE {{ FILTER({}1{}) }}", "(".repeat(n), ")".repeat(n));
/// assert!(admit("query", nested(MAX_NESTING - 2).as_bytes(), Text::Query).is_ok());
/// assert!(matches!(
///     admit("query", nested(MAX_NESTING - 1).as_bytes(), Text::Query),
///     Err(Error::InvalidArgument { name, .. }) if name == "query"
/// ));
/// ```
pub fn admit(name: &str, text: &[u8], kind: Text) -> Result<()> {
    if kind == Text::Query {
        admit_length(name, text)?;
    }
    admit_nesting(name, text)
}

/// The length half of [`admit`]: a query past [`MAX_QUERY_BYTES`] is refused.
fn admit_length(name: &str, text: &[u8]) -> Result<()> {
    if text.len() > MAX_QUERY_BYTES {
        return Err(Error::InvalidArgument {
            name: name.to_string(),
            detail: format!(
                "is {} bytes; this server parses a SPARQL query of at most {MAX_QUERY_BYTES} \
                 bytes (ledger #915: a long enough chain of `+`, `&&`, `UNION` or `/` with no \
                 brackets at all overflows the evaluator's stack and aborts the server)",
                text.len()
            ),
        });
    }
    Ok(())
}

/// The nesting half of [`admit`], for any SPARQL text at any depth: past [`MAX_NESTING`] is
/// refused. This is the half [`bounded`] applies to a sub-request (ledger
/// [#965](http://localhost:1060/l/default/item/965)).
///
/// ```
/// use ikigai_gonk::sparql::{admit_nesting, MAX_QUERY_BYTES};
///
/// // Long and flat is not this half's business: gonk's own ledger query is.
/// let values = format!("SELECT * WHERE {{ VALUES ?i {{ {} }} }}", "<urn:x> ".repeat(MAX_QUERY_BYTES));
/// assert!(admit_nesting("query", values.as_bytes()).is_ok());
/// assert!(admit_nesting("query", "(".repeat(100).as_bytes()).is_err());
/// ```
pub fn admit_nesting(name: &str, text: &[u8]) -> Result<()> {
    let depth = nesting_depth(text);
    if depth > MAX_NESTING {
        return Err(Error::InvalidArgument {
            name: name.to_string(),
            detail: format!(
                "nests {depth} brackets deep; this server parses SPARQL nested at most \
                 {MAX_NESTING} deep (ledger #915: the parser reads nesting by recursion, and \
                 deep enough nesting overflows its stack and aborts the server). Counted are \
                 `(`, `{{`, `[`, `<<` and a negating `!` outside strings and comments"
            ),
        });
    }
    Ok(())
}

/// How deep `text` can nest brackets when a SPARQL parser reads it — an UPPER bound, never an
/// under-count, measured lexically in one pass with no grammar.
///
/// ```
/// use ikigai_gonk::sparql::nesting_depth;
///
/// assert_eq!(nesting_depth(b"SELECT * WHERE { FILTER((1)) }"), 3);
/// // Brackets in a string or a comment are not structure.
/// assert_eq!(nesting_depth(b"SELECT * WHERE { FILTER(REGEX(?x, \"^((((a\")) } # ((((("), 3);
/// // Nor in an http(s) IRI.
/// assert_eq!(nesting_depth(b"SELECT * WHERE { <https://en.wikipedia.org/wiki/A_(b)> ?p ?o }"), 1);
/// // ★ But a `<` the parser may read as LESS-THAN is not skipped as an IRI: this is nested
/// // 4 deep as `?a < (((1)))`, and an IRI reading alone would have said 1.
/// assert_eq!(nesting_depth(b"FILTER(?a <(((1)))> 2)"), 4);
/// ```
///
/// # ★ Why this is an automaton and not "skip strings, IRIs and comments"
///
/// `spargebra` reads `<` two ways: as an IRI (`<` then anything up to the next `>`, checked
/// afterwards) and, after an expression, as the less-than operator — and a PEG parser TRIES
/// readings, so a reading that fails later has already recursed. A scanner that skipped
/// `<…>` as an IRI would miss `FILTER(?a <((((…1…))))> 2)` entirely, and an IRI that holds a
/// `#` or a quote makes the two readings disagree about where a comment or a string is for
/// the REST of the line or text — which is enough to hide unbounded nesting behind closers
/// the other reading never sees. So this runs every lexical reading at once (a set of
/// states, as an NFA is run: linear in the text) and counts
///
/// - an OPEN bracket when ANY reading is in code there — it might be structure;
/// - a CLOSE bracket only when EVERY reading is in code there — only then is it certainly
///   structure.
///
/// That is an upper bound on every reading's depth at every point, so on the parser's. What
/// it costs is an over-count where readings disagree: an open bracket inside a non-`http`
/// IRI (`<urn:x(>`) counts, and a closer on the rest of a line after an IRI like `<urn:a#b>`
/// does not. Two facts keep that rare: a reading in code dies at `//`, which nothing in
/// SPARQL accepts outside a string, IRI or comment, so the less-than reading of every
/// `<http://…>` and `<https://…>` ends at its second character; and every reading converges
/// again at the IRI's `>` when nothing in between opened a string or a comment.
pub fn nesting_depth(text: &[u8]) -> usize {
    use lex::*;
    let at = |i: usize| text.get(i).copied();
    let mut states: u32 = CODE;
    let (mut level, mut deepest) = (0usize, 0usize);
    // The `!`s counted inside each open group, innermost last. A negation's level lasts until
    // the expression it applies to ends, which no lexer can see, so it is released with the
    // group that holds it — later than the parser releases it, never earlier. One outside every
    // group is never released.
    let mut groups: Vec<usize> = Vec::new();
    for (i, &b) in text.iter().enumerate() {
        if states == 0 {
            // Every reading has failed to parse; nothing past here is read by anyone.
            break;
        }
        let code = states & CODE != 0;
        match b {
            b'(' | b'{' | b'[' if code => {
                level += 1;
                deepest = deepest.max(level);
                groups.push(0);
            }
            b'<' if code && at(i + 1) == Some(b'<') => {
                level += 1;
                deepest = deepest.max(level);
                groups.push(0);
            }
            // ★ `!` is the one prefix operator spargebra reads by recursion (`"!" _
            // UnaryExpression()`, refused as double negation only AFTER the recursion), so
            // `!!!!…1` nests with no bracket at all. `!=` is a comparison, not a negation.
            b'!' if code && at(i + 1) != Some(b'=') => {
                level += 1;
                deepest = deepest.max(level);
                if let Some(bangs) = groups.last_mut() {
                    *bangs += 1;
                }
            }
            b')' | b'}' | b']' if states == CODE => {
                let bangs = groups.pop().unwrap_or(0);
                level = level.saturating_sub(1 + bangs);
            }
            _ => {}
        }
        states = (0..STATES)
            .map(|bit| 1u32 << bit)
            .filter(|state| states & state != 0)
            .fold(0, |next, state| next | step(state, b, at(i + 1), at(i + 2)));
    }
    deepest
}

/// The lexical states [`nesting_depth`] runs, one bit each, and the step between them. Only
/// the distinctions that decide whether a byte is code are kept.
mod lex {
    pub const CODE: u32 = 1 << 0;
    const CODE_ESC: u32 = 1 << 1; // after `\` in code (PN_LOCAL_ESC): the next byte is a name
    const COMMENT: u32 = 1 << 2;
    const IRI: u32 = 1 << 3; // `<` … up to the next `>`, newlines included, as spargebra reads it
    const SHORT1: u32 = 1 << 4; // '…'
    const SHORT1_ESC: u32 = 1 << 5;
    const SHORT2: u32 = 1 << 6; // "…"
    const SHORT2_ESC: u32 = 1 << 7;
    const LONG1: u32 = 1 << 8; // '''…'''
    const LONG1_ESC: u32 = 1 << 9;
    const LONG2: u32 = 1 << 10; // """…"""
    const LONG2_ESC: u32 = 1 << 11;
    const ENTER1_2: u32 = 1 << 12; // the 2nd and 3rd quote of an opening '''
    const ENTER1_1: u32 = 1 << 13;
    const ENTER2_2: u32 = 1 << 14; // … of an opening """
    const ENTER2_1: u32 = 1 << 15;
    const EXIT_2: u32 = 1 << 16; // the 2nd and 3rd quote of a closing ''' or """
    const EXIT_1: u32 = 1 << 17;
    /// How many states there are: the bits above.
    pub const STATES: u32 = 18;

    /// The states after `state` reads byte `b`, with the two bytes after it for lookahead. An
    /// empty set is a reading that cannot parse past here.
    pub fn step(state: u32, b: u8, b1: Option<u8>, b2: Option<u8>) -> u32 {
        let newline = b == b'\n' || b == b'\r';
        match state {
            CODE => match b {
                b'#' => COMMENT,
                // An opening ''' is a long string, or an empty short string and then the
                // start of another: both are kept, and the lexing the parser chose is one.
                b'\'' if b1 == Some(b'\'') && b2 == Some(b'\'') => SHORT1 | ENTER1_2,
                b'"' if b1 == Some(b'"') && b2 == Some(b'"') => SHORT2 | ENTER2_2,
                b'\'' => SHORT1,
                b'"' => SHORT2,
                // An IRI, or less-than (`<`, `<=`, `<<`) and code goes on.
                b'<' => CODE | IRI,
                b'\\' => CODE_ESC,
                // `//` is no SPARQL production outside a string, IRI or comment.
                b'/' if b1 == Some(b'/') => 0,
                _ => CODE,
            },
            CODE_ESC => CODE,
            COMMENT if newline => CODE,
            COMMENT => COMMENT,
            IRI if b == b'>' => CODE,
            IRI => IRI,
            SHORT1 | SHORT2 => {
                let (quote, esc) = if state == SHORT1 {
                    (b'\'', SHORT1_ESC)
                } else {
                    (b'"', SHORT2_ESC)
                };
                match b {
                    b'\\' => esc,
                    _ if b == quote => CODE,
                    // A short string cannot hold a line break: this reading fails.
                    _ if newline => 0,
                    _ => state,
                }
            }
            SHORT1_ESC => SHORT1,
            SHORT2_ESC => SHORT2,
            LONG1 | LONG2 => {
                let (quote, esc) = if state == LONG1 {
                    (b'\'', LONG1_ESC)
                } else {
                    (b'"', LONG2_ESC)
                };
                match b {
                    b'\\' => esc,
                    _ if b == quote && b1 == Some(quote) && b2 == Some(quote) => EXIT_2,
                    _ => state,
                }
            }
            LONG1_ESC => LONG1,
            LONG2_ESC => LONG2,
            ENTER1_2 => ENTER1_1,
            ENTER1_1 => LONG1,
            ENTER2_2 => ENTER2_1,
            ENTER2_1 => LONG2,
            EXIT_2 => EXIT_1,
            EXIT_1 => CODE,
            _ => 0,
        }
    }
}

/// `inner` with [`admit`] in front of every endpoint that parses caller SPARQL: for each
/// `(endpoint, argument, kind)` in `rules`, a request to an endpoint whose
/// [`Endpoint::name`] is `endpoint` (any endpoint in `inner`, for `None`) carrying an inline
/// `argument` is refused before the endpoint runs when that text is past the bound.
///
/// An overlay with no identity or structure of its own, like [`crate::admit::Admitting`]: it
/// forwards `id`, `topology` and `entries`, so `urn:kernel:topology` reads the same with it
/// or without it. Composed in the HUB, around the store and the review space, so it holds
/// for every door — the socket, QUIC, HTTP's `/iki/…` and `/k`, and `urn:sparql:*`, which
/// reaches the store through the hub.
///
/// # ★ Nesting at every depth; length only for a CALLER's text (ledger #965)
///
/// The NESTING bound holds at every depth: no text nested past [`MAX_NESTING`] reaches a
/// parser, whoever wrote it. The LENGTH bound applies only at depth 0 — a request a door
/// issued, which is a caller's own text: every door reaches the hub through
/// [`crate::doors::HubSpace`], which issues at the hub's depth 0 — and `urn:sparql:*` checks
/// its caller's query itself before it issues anything.
///
/// A sub-request's text was written by an endpoint in this process, and gonk's own are long
/// and flat: `ikigai-ledger` reads labels, links and comments for every open item in one
/// `VALUES`, about 51 bytes an item, so a ledger of some 650 open items crossed 32 KiB. With
/// the length bound at every depth (PR 101, `eeeeeaf`) the home page, `/l/default` and
/// `urn:iki:ledger:next` all answered `400`. A flat `VALUES` builds no deep tree; the chains
/// the length bound exists for (`+`, `&&`, `UNION`, `/`) are not what an endpoint here
/// writes, and the 64 MiB request stacks ([`crate::stack`]) are the margin under them.
///
/// ⚠ So an endpoint that forwards a CALLER's text to the store in a sub-request must run
/// [`admit`] on it itself before it issues, as `urn:sparql:*` and the SPARQL page do — this
/// overlay sees that text at depth 1 and checks only its nesting.
pub fn bounded(inner: Arc<dyn Space>, rules: &'static [Rule]) -> Arc<dyn Space> {
    Arc::new(Bounded { inner, rules })
}

/// One [`bounded`] rule: `(endpoint name, or any; argument; kind)`.
pub type Rule = (Option<&'static str>, &'static str, Text);

/// The store's SPARQL-parsing endpoints, by `ikigai-store`'s own ids: the eight read forms take
/// `query`, the two update forms take `content` (the engine routes a pipe there). `load`
/// parses RDF, not SPARQL, and is not listed.
pub const STORE_RULES: &[Rule] = &[
    (Some("store-select"), "query", Text::Query),
    (Some("store-ask"), "query", Text::Query),
    (Some("store-construct"), "query", Text::Query),
    (Some("store-describe"), "query", Text::Query),
    (Some("store-graph-select"), "query", Text::Query),
    (Some("store-graph-ask"), "query", Text::Query),
    (Some("store-graph-construct"), "query", Text::Query),
    (Some("store-graph-describe"), "query", Text::Query),
    (Some("store-update"), "content", Text::Update),
    (Some("store-graph-update"), "content", Text::Update),
];

/// The review space's (`ikigai-intray`'s `urn:space:{name}`): `rd` and `take` filter by an ASK
/// in `match`, which it parses with `spargebra`.
pub const SPACE_RULES: &[Rule] = &[(None, "match", Text::Query)];

struct Bounded {
    inner: Arc<dyn Space>,
    rules: &'static [Rule],
}

impl Space for Bounded {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let rules = self.rules;
        self.inner
            .resolve(request, scope)
            .map_endpoint(|endpoint| Arc::new(Guarded { endpoint, rules }) as Arc<dyn Endpoint>)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

struct Guarded {
    endpoint: Arc<dyn Endpoint>,
    rules: &'static [Rule],
}

#[async_trait]
impl Endpoint for Guarded {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = self.endpoint.name();
        for &(endpoint, argument, kind) in self.rules {
            if endpoint.is_some_and(|e| e != name) {
                continue;
            }
            // Only an inline value is ever parsed: every endpoint listed refuses any other.
            if let Some(ArgRef::Inline(text)) = inv.request.args.get(argument) {
                if inv.depth() == 0 {
                    admit(argument, text, kind)?;
                } else {
                    admit_nesting(argument, text)?;
                }
            }
        }
        self.endpoint.invoke(inv).await
    }

    fn name(&self) -> &str {
        self.endpoint.name()
    }

    fn describe(&self) -> Description {
        self.endpoint.describe()
    }

    fn is_limiter(&self) -> bool {
        self.endpoint.is_limiter()
    }

    fn confinement(&self) -> Option<Topology> {
        self.endpoint.confinement()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth(text: &str) -> usize {
        nesting_depth(text.as_bytes())
    }

    #[test]
    fn strings_comments_and_http_iris_hold_no_structure() {
        assert_eq!(depth("SELECT * WHERE { ?s ?p '''((((((''' }"), 1);
        assert_eq!(depth(r#"SELECT * WHERE { ?s ?p "a\"((((((" }"#), 1);
        assert_eq!(depth("SELECT * WHERE { ?s ?p ?o } # don't (((((("), 1);
        // A comment's apostrophe opens no string: the nesting after it is still seen.
        let after = format!(
            "# don't\nSELECT * WHERE {{ FILTER({}1{}) }}",
            "(".repeat(5),
            ")".repeat(5)
        );
        assert_eq!(depth(&after), 7);
        // One line, an `https` namespace with a `#`: the less-than reading dies at `//`, so
        // nothing after it is held back.
        let one_line =
            "PREFIX l: <https://ikigai-rs.dev/ns/ledger#> SELECT * WHERE { FILTER((1)) } \
                        SELECT * WHERE { FILTER((1)) }";
        assert_eq!(depth(one_line), 3);
    }

    /// `<` read as less-than: the parser recurses into what an IRI reading would skip.
    #[test]
    fn an_iri_that_may_be_less_than_is_counted() {
        let text = format!(
            "SELECT * WHERE {{ FILTER(?a <{}1{}> 2) }}",
            "(".repeat(100),
            ")".repeat(100)
        );
        assert_eq!(depth(&text), 102);
        assert!(admit("query", text.as_bytes(), Text::Query).is_err());
    }

    /// ★ Readings that disagree about a comment or a string cannot cancel nesting: a closer is
    /// counted only where EVERY reading is in code. Each of these is 80 deep under the
    /// less-than reading of its `<…>`, and 40 under the IRI reading a naive scanner would take.
    #[test]
    fn readings_that_disagree_cannot_hide_nesting() {
        let (open, close) = ("(".repeat(40), ")".repeat(40));
        // `#` in the IRI is a comment to the end of the line under the less-than reading, so
        // the closers after the IRI are inside it.
        let comment = format!("{open} ?a <urn:x#y> {close}\n{open}1");
        assert_eq!(depth(&comment), 80);
        // `'` in the IRI opens a string under the less-than reading, closed after the closers.
        let string = format!("{open} ?a <urn:it's> {close} '{open}1");
        assert_eq!(depth(&string), 80);
    }

    #[test]
    fn a_negation_counts_until_its_group_closes_and_a_comparison_never() {
        let bangs = format!("SELECT * WHERE {{ FILTER({}1) }}", "!".repeat(100));
        assert_eq!(depth(&bangs), 102);
        // Whitespace between them changes nothing: the grammar skips it.
        assert_eq!(depth(&format!("FILTER({}1)", "! ".repeat(100))), 101);
        // Released with the group: a hundred negated FILTERs in sequence stay shallow.
        let many = "FILTER(!BOUND(?a)) ".repeat(100);
        assert_eq!(depth(&format!("SELECT * WHERE {{ {many} }}")), 4);
        let unequal = format!(
            "SELECT * WHERE {{ FILTER(?a != ?b{}) }}",
            " && ?a != ?b".repeat(100)
        );
        assert_eq!(depth(&unequal), 2);
    }

    #[test]
    fn a_reified_triple_counts_as_nesting() {
        let text = format!(
            "SELECT * WHERE {{ {}?s ?p ?o{} ?q ?r }}",
            "<< ".repeat(70),
            " >>".repeat(70)
        );
        assert!(depth(&text) > MAX_NESTING);
    }

    #[test]
    fn the_length_bound_is_for_queries_only() {
        let long = format!(
            "SELECT * WHERE {{ ?s ?p \"{}\" }}",
            "x".repeat(MAX_QUERY_BYTES)
        );
        assert!(admit("query", long.as_bytes(), Text::Query).is_err());
        let update = format!(
            "INSERT DATA {{ <urn:s> <urn:p> \"{}\" }}",
            "x".repeat(MAX_QUERY_BYTES)
        );
        assert!(admit("content", update.as_bytes(), Text::Update).is_ok());
    }
}
