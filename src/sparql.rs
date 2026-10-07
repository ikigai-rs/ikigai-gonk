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

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri,
    Representation, Request, Result, Verb,
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
