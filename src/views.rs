//! **Named views as data** (ledger [#2006](http://localhost:1062/l/default/item/2006), arc 2):
//! names under `urn:iki:gonk:view:` that an operator declares, changes and removes while gonk
//! runs, each forwarding to a name gonk already serves.
//!
//! ```text
//! urn:iki:gonk:views:declaration     Source Sink  the declaration: Turtle in the ik: arrangement vocabulary
//! urn:iki:gonk:views:check           Source       the rule a candidate must pass (a SPARQL ASK; true refuses)
//! urn:iki:space:dynamic:views        (space)      core's DynamicSpace, rebuilt from the declaration, LAST in the hub
//! urn:iki:space:dynamic:views:status Source       what is in force, and the last refusal and why (core's door)
//! urn:iki:gonk:view:{name}           any verb     a view: an ik:Alias rule marked ik:resolveAt "root"
//! ```
//!
//! # The map, as built
//!
//! - **The declaration is an atom, and the atom is a store graph** (`urn:iki:gonk:views:declaration`,
//!   graph and resource named alike). Its `Source` is the graph's triples, read through
//!   `urn:iki:store:graph-construct` under the CALLER's capability; its `Sink` replaces them
//!   through `urn:iki:store:graph-update`, also under the caller's capability, so the store's
//!   per-graph write token ([`write_grant`]) IS the authority to change the views. No new
//!   capability exists for it.
//! - **A view is a rule, not code**: an `ik:Alias` exact or prefix rule marked
//!   `ik:resolveAt "root"` (core 0.1.110), which forwards the rewritten name from the hub's
//!   root under the caller's own capability. So a view grants nothing its reader could not
//!   already read, and it is its own cache entry hanging from the target's threads.
//! - **The views space is placed LAST in the hub's `Fallback`** ([`crate::compose_serving`]),
//!   so by precedence a view never shadows a name gonk already serves.
//! - **The check is a resource** ([`CHECK`], the query text in `src/views_check.rq`); the only
//!   code is the call that asks it of the candidate's Turtle, because core cannot run SPARQL.
//! - **The rebuild is core's `drive`**, on a thread of its own ([`Views::start`]), listening on
//!   the declaration's thread. A `Sink` to the declaration cuts that thread, so the space hears
//!   it and rebuilds.
//!
//! ⚠ **Write the declaration through its own `Sink`, not through `urn:iki:store:graph-update`
//! directly.** Both write the same graph under the same token, but only a `Sink` to
//! [`DECLARATION`] cuts the thread the rebuild listens on; a raw graph write is heard at the
//! next declaration write or the next start.
//!
//! ⚠ **The `Registry` is empty, deliberately.** A composed view (an arrangement of registered
//! endpoints) would need a door to supply fixed ARGUMENTS to the endpoint it binds, and the
//! `ik:` arrangement vocabulary has no way to say that: a door names an endpoint and a pattern,
//! and every argument comes from the caller. So nothing gonk links would compose anything a
//! plain alias does not. A composition with fixed arguments is a saved script, and a view onto
//! its `urn:script:{name}:result` is the composed view.
//!
//! ⚠ A view is invisible to discovery (ledger [#2065](http://localhost:1062/l/default/item/2065)):
//! its `Meta` describes core's forwarder, and `urn:kernel:catalog` does not list view names.

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Capability, Description, DynamicSpace, Endpoint, EndpointSpace,
    Error, Exact, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, SpaceCheck,
    SystemClock, Topology, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

/// The dynamic space's name segment: the space is `urn:iki:space:dynamic:views` (Brian's
/// decision on ledger #2006).
pub const SPACE_NAME: &str = "views";
/// The dynamic space's fixed name, which is also the thread every view's answer hangs from.
pub const SPACE: &str = "urn:iki:space:dynamic:views";
/// Core's status door for the space: what is in force, and the last refusal.
pub const STATUS: &str = "urn:iki:space:dynamic:views:status";
/// The declaration: a store graph and the resource that reads and writes it, named alike.
pub const DECLARATION: &str = "urn:iki:gonk:views:declaration";
/// The rule a candidate declaration must pass, as a resource (a SPARQL ASK; true refuses).
pub const CHECK: &str = "urn:iki:gonk:views:check";
/// Where every view is named.
pub const VIEW_PREFIX: &str = "urn:iki:gonk:view:";
/// The text [`CHECK`] serves.
pub const CHECK_QUERY: &str = include_str!("views_check.rq");
/// The largest declaration a `Sink` accepts, in bytes. A declaration is a handful of rules; a
/// bound refuses the accident long before `Topology`'s own node bound would.
pub const MAX_DECLARATION_BYTES: usize = 256 * 1024;

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const GRAPH_CONSTRUCT: &str = "urn:iki:store:graph-construct";
const GRAPH_UPDATE: &str = "urn:iki:store:graph-update";
const N_TRIPLES: &str = "application/n-triples";

/// The store token that READS the declaration — the views' read authority.
///
/// ```
/// assert_eq!(
///     ikigai_gonk::views::read_grant(),
///     "urn:cap:store:read:graph:urn:iki:gonk:views:declaration"
/// );
/// ```
pub fn read_grant() -> String {
    ikigai_store::cap_read_graph(DECLARATION)
}

/// The store token that WRITES the declaration — the authority to change the views, and the
/// only one (Brian's decision on ledger #2006).
///
/// ```
/// assert_eq!(
///     ikigai_gonk::views::write_grant(),
///     "urn:cap:store:write:graph:urn:iki:gonk:views:declaration"
/// );
/// ```
pub fn write_grant() -> String {
    ikigai_store::cap_write_graph(DECLARATION)
}

/// The views: core's [`DynamicSpace`] for the hub, and the declaration and check resources it
/// is built from. Built before the hub ([`crate::compose_serving`] places both), started after
/// ([`Views::start`]), as the script family is attached after.
pub struct Views {
    dynamic: Arc<DynamicSpace>,
    /// The hub, set by [`start`](Self::start): what the check reads [`CHECK`] through. Weak,
    /// because the hub holds this space and the space holds the check.
    hub: Arc<OnceLock<Weak<Kernel>>>,
}

impl Default for Views {
    fn default() -> Self {
        Views::new()
    }
}

impl Views {
    /// The views, with gonk's check ([`CHECK`]) run on every candidate.
    pub fn new() -> Self {
        let hub = Arc::new(OnceLock::new());
        let dynamic = DynamicSpace::new(SPACE_NAME, iri(DECLARATION), ikigai_core::Registry::new())
            .with_check(check(Arc::clone(&hub)))
            .with_clock(Arc::new(SystemClock));
        Views {
            dynamic: Arc::new(dynamic),
            hub,
        }
    }

    /// The views WITHOUT the check — for a test that has to show what the PLACEMENT alone
    /// does (a view never shadows a name the hub serves), which the check would refuse to let
    /// it declare. `main` never builds this.
    pub fn unchecked() -> Self {
        let dynamic = DynamicSpace::new(SPACE_NAME, iri(DECLARATION), ikigai_core::Registry::new())
            .with_clock(Arc::new(SystemClock));
        Views {
            dynamic: Arc::new(dynamic),
            hub: Arc::new(OnceLock::new()),
        }
    }

    /// The dynamic space, for the LAST place in the hub.
    pub fn dynamic(&self) -> Arc<DynamicSpace> {
        Arc::clone(&self.dynamic)
    }

    /// The declaration and check resources (`urn:iki:gonk:space:views`), for anywhere in the
    /// hub: their names are gonk's own and no other space claims them.
    pub fn space(&self) -> EndpointSpace {
        EndpointSpace::new()
            .bind(Exact::new(DECLARATION), DeclarationEndpoint)
            .bind(Exact::new(CHECK), CheckEndpoint)
            .named(crate::spaces::iri(crate::spaces::VIEWS))
    }

    /// Start the rebuild: core's `drive` on a thread of its own, under `capability`.
    /// The first read happens there, so this returns at once; the status
    /// ([`STATUS`]) says what bound, or why nothing did.
    ///
    /// ⚠ `main` passes ROOT, and that is ledger [#2048](http://localhost:1062/l/default/item/2048):
    /// a listener under a narrowed capability hears a cut only when an entry cached under that
    /// capability rests on the thread, so an uncacheable declaration (a store opened shared,
    /// whose freshness for this graph it does not promise) would never be heard. Root hears
    /// every cut it listens for. The declaration's READ is still the caller's on every other
    /// path: only the rebuild reads as the host.
    ///
    /// # Errors
    ///
    /// When the thread or its runtime cannot be started.
    pub fn start(
        &self,
        hub: &Arc<Kernel>,
        capability: Capability,
    ) -> std::result::Result<(), String> {
        let _ = self.hub.set(Arc::downgrade(hub));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("the views will not rebuild (no runtime: {e})"))?;
        let (dynamic, hub) = (Arc::clone(&self.dynamic), Arc::clone(hub));
        std::thread::Builder::new()
            .name("gonk-views".to_string())
            .spawn(move || {
                let error = runtime.block_on(dynamic.drive(&hub, &capability));
                eprintln!("ikigai-gonk: the views stopped rebuilding: {error}");
            })
            .map(|_| ())
            .map_err(|e| format!("the views will not rebuild (no thread: {e})"))
    }
}

fn iri(name: &str) -> Iri {
    Iri::parse(name).expect("a constant IRI")
}

/// gonk's [`SpaceCheck`]: read [`CHECK`] through the hub and ask it of the candidate's Turtle.
/// Fail closed: no hub, an unreadable rule or a rule that does not run all refuse.
fn check(hub: Arc<OnceLock<Weak<Kernel>>>) -> SpaceCheck {
    Arc::new(move |candidate: Topology| {
        let hub = hub.get().and_then(Weak::upgrade);
        Box::pin(async move {
            let hub = hub.ok_or("no hub to read the check through, so nothing binds")?;
            // The rule reads nothing, so it is read under a capability that holds nothing.
            let none = Capability::root().attenuate(Vec::<String>::new());
            let rule = hub
                .issue(Request::new(Verb::Source, iri(CHECK)), &none)
                .await
                .map_err(|e| format!("the check {CHECK} could not be read: {e}"))?;
            let rule = String::from_utf8(rule.bytes)
                .map_err(|_| format!("the check {CHECK} is not UTF-8 text"))?;
            if refuses(&rule, &candidate.to_turtle())? {
                Err(format!(
                    "it declares a name outside {VIEW_PREFIX}, so it would claim a name that \
                     is not a view's (the rule is {CHECK})"
                ))
            } else {
                Ok(())
            }
        })
    })
}

/// Ask `rule` (a SPARQL ASK) of `turtle`: whether it refuses.
fn refuses(rule: &str, turtle: &str) -> std::result::Result<bool, String> {
    let store = Store::new().map_err(|e| format!("the check has no store: {e}"))?;
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), turtle.as_bytes())
        .map_err(|e| format!("the candidate's Turtle did not load: {e}"))?;
    let results = SparqlEvaluator::new()
        .parse_query(rule)
        .map_err(|e| format!("the check {CHECK} is not SPARQL: {e}"))?
        .on_store(&store)
        .execute()
        .map_err(|e| format!("the check {CHECK} did not run: {e}"))?;
    match results {
        QueryResults::Boolean(answer) => Ok(answer),
        _ => Err(format!("the check {CHECK} is not an ASK query")),
    }
}

/// [`DECLARATION`]: the graph's triples (`Source`), or a new set of them (`Sink`).
struct DeclarationEndpoint;

impl DeclarationEndpoint {
    async fn read(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let query =
            format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{DECLARATION}> {{ ?s ?p ?o }} }}");
        let read = Request::new(Verb::Source, iri(GRAPH_CONSTRUCT))
            .with_arg("graph", ArgRef::Inline(DECLARATION.as_bytes().to_vec()))
            .with_arg("query", ArgRef::Inline(query.into_bytes()))
            .with_arg("as", ArgRef::Inline(N_TRIPLES.as_bytes().to_vec()));
        let triples = inv.issue(read).await?;
        if triples.bytes.iter().all(u8::is_ascii_whitespace) {
            // The absence is the GRAPH's state, so it hangs from the store's write threads as
            // well as this name's own (ledger #1079).
            for thread in [
                ikigai_store::UPDATE_THREAD,
                ikigai_store::LOAD_THREAD,
                ikigai_store::GRAPH_UPDATE_THREAD,
            ] {
                inv.depends_on(thread);
            }
            return Err(Error::NotFound(format!(
                "no views are declared: the graph {DECLARATION} is empty (Sink Turtle to it to \
                 declare some)"
            )));
        }
        Ok(triples)
    }

    async fn write(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let content = inv.inline_arg("content")?;
        if content.len() > MAX_DECLARATION_BYTES {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: format!(
                    "a views declaration is at most {MAX_DECLARATION_BYTES} bytes; this one is {}",
                    content.len()
                ),
            });
        }
        let triples = n_triples(content)?;
        let update = if triples.trim().is_empty() {
            format!("CLEAR SILENT GRAPH <{DECLARATION}>")
        } else {
            format!(
                "CLEAR SILENT GRAPH <{DECLARATION}> ;\nINSERT DATA {{ GRAPH <{DECLARATION}> {{\n{triples}}} }}"
            )
        };
        // Under the caller's capability: the store's per-graph write token decides.
        let write = Request::new(Verb::Sink, iri(GRAPH_UPDATE))
            .with_arg("graph", ArgRef::Inline(DECLARATION.as_bytes().to_vec()))
            .with_arg("content", ArgRef::Inline(update.into_bytes()));
        inv.issue(write).await?;
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            format!(
                "declared; {SPACE} rebuilds when it hears the write, and {STATUS} says what is \
                 in force or why it was refused\n"
            )
            .into_bytes(),
        ))
    }
}

/// `content` (Turtle) as N-Triples, by the engine's own parser and serializer.
fn n_triples(content: &[u8]) -> Result<String> {
    let refused = |detail: String| Error::InvalidArgument {
        name: "content".to_string(),
        detail,
    };
    let mut out = RdfSerializer::from_format(RdfFormat::NTriples).for_writer(Vec::new());
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(content) {
        let quad = quad.map_err(|e| refused(format!("a views declaration is Turtle: {e}")))?;
        out.serialize_quad(&quad)
            .map_err(|e| Error::Endpoint(format!("views: {e}")))?;
    }
    let bytes = out
        .finish()
        .map_err(|e| Error::Endpoint(format!("views: {e}")))?;
    String::from_utf8(bytes).map_err(|e| Error::Endpoint(format!("views: {e}")))
}

#[async_trait]
impl Endpoint for DeclarationEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Sink => self.write(inv).await,
            _ => self.read(inv).await,
        }
    }

    fn name(&self) -> &str {
        "gonk-views-declaration"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-views-declaration")
            .title("The views declaration")
            .summary(
                "The named views gonk serves under urn:iki:gonk:view:, as Turtle in the ik: \
                 arrangement vocabulary: an ik:Alias whose ik:RewriteRule nodes are marked \
                 ik:resolveAt \"root\", over an empty ik:EndpointSpace. Source: the \
                 declaration in the store. Sink: replace it (content=Turtle); the views space \
                 rebuilds when it hears the write, and urn:iki:space:dynamic:views:status \
                 says what is in force or why a declaration was refused.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("the declaration, as N-Triples")
                    .requires(read_grant())
                    .output(N_TRIPLES),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("replace the declaration")
                    .requires(write_grant())
                    .input(
                        ArgSpec::new("content")
                            .class(XSD_STRING)
                            .summary("the whole declaration, as Turtle"),
                    )
                    .output("text/plain;charset=utf-8"),
            )
            .verb(Verb::Meta)
    }
}

/// [`CHECK`]: the rule's text. A constant, so cacheable.
struct CheckEndpoint;

#[async_trait]
impl Endpoint for CheckEndpoint {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/sparql-query"),
            CHECK_QUERY.as_bytes().to_vec(),
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "gonk-views-check"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-views-check")
            .title("The views check")
            .summary(
                "The SPARQL ASK a candidate views declaration must answer false before it \
                 binds: true means it declares a name outside urn:iki:gonk:view:.",
            )
            .verb(Verb::Source)
            .output("application/sparql-query")
    }
}
