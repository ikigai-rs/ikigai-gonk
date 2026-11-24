//! The activity graph: gonk's door log as RDF, an hourly rollup per caller, door, verb, outcome
//! and resource (ledger [#4104](http://localhost:1062/l/default/item/4104), from the spike in
//! #3391).
//!
//! ```text
//! urn:iki:gonk:activity:segment:{stamp}   Source  the stored CONSTRUCT over closed `urn:log:{stamp}`, Turtle
//! urn:iki:gonk:activity                   Source  the union over every closed segment, Turtle
//! ```
//!
//! ★ **The query is a resource, not code.** [`SEGMENT_QUERY`] is `src/questions/activity/segment.rq`,
//! the spike's CONSTRUCT verbatim; its header says what the three node kinds are, what is never
//! carried and why the rollup is hourly. This module only feeds it one segment and serializes
//! what it answers. Its terms are in the provisional `urn:iki:gonk:activity:vocab#`.
//!
//! ★ **Run here, not through `urn:sparql:construct`.** That face reads the STORE's graphs, and a
//! segment is not in the store: it is Turtle at its own IRI. So the segment is read through the
//! kernel, under the caller's capability, loaded into a scratch in-memory store as the named graph
//! `urn:log:{stamp}`, and the CONSTRUCT runs over that alone.
//!
//! ★ **Owner only.** Both resources need `urn:cap:log:read` ([`ikigai_log::endpoints::CAP_READ`]),
//! as the segments do: a rollup names every authenticated caller and every resource. Anonymous
//! traffic is in it only as per-day counts, never a resource or a sequence (the query's rule 3).
//!
//! ★ **Closed segments only, and cached for good.** A segment's own read is cacheable only once it
//! is finished (sealed after `log:ProcessStop` or `log:Rotation`), and its expiry propagates onto
//! the rollup, so a closed segment's rollup is computed once. A segment whose read is NOT
//! cacheable is live (this process's open one, or one whose process died without a stop), and is
//! refused until the live-segment read conflict is fixed; the union skips it.
//!
//! ⚠ **The union recomputes its merge on every read.** Its listing, `urn:log:segments`, is
//! uncacheable (a directory has no golden thread), so the union is too; each segment's rollup
//! under it is a cached read.

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Error, Exact, Expiry, Invocation, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{Graph, GraphNameRef, NamedNode, Triple, TripleRef};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

use crate::web::request;

/// The union of every closed segment's rollup.
pub const ACTIVITY_IRI: &str = "urn:iki:gonk:activity";
/// One closed segment's rollup.
pub const SEGMENT_TEMPLATE: &str = "urn:iki:gonk:activity:segment:{stamp}";
/// The prefix of [`SEGMENT_TEMPLATE`].
pub const SEGMENT_PREFIX: &str = "urn:iki:gonk:activity:segment:";
/// The provisional vocabulary the query's terms are in.
pub const VOCAB: &str = "urn:iki:gonk:activity:vocab#";
/// The capability both resources need: the segments'.
pub const CAP_READ: &str = ikigai_log::endpoints::CAP_READ;
/// The stored CONSTRUCT, verbatim from the spike.
pub const SEGMENT_QUERY: &str = include_str!("questions/activity/segment.rq");

const TURTLE: &str = "text/turtle";
const LOG_PREFIX: &str = "urn:log:";

/// The two resources, as a space for the hub.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new(ACTIVITY_IRI), Union)
        .bind(
            UriTemplate::parse(SEGMENT_TEMPLATE).expect("a constant template"),
            Segment,
        )
}

fn source_only(inv: &Invocation<'_>, what: &str) -> Result<()> {
    if inv.request.verb != Verb::Source {
        return Err(Error::Endpoint(format!(
            "{what} answers Source, not {:?}",
            inv.request.verb
        )));
    }
    if !inv.capability.allows(CAP_READ) {
        return Err(Error::Denied(
            format!("the activity graph requires `{CAP_READ}`: it names every caller and resource")
                .into(),
        ));
    }
    Ok(())
}

/// The triples the stored query answers over `turtle`, the segment named `segment`.
pub fn construct(segment: &str, turtle: &[u8]) -> std::result::Result<Vec<Triple>, String> {
    let store = Store::new().map_err(|e| format!("a scratch store: {e}"))?;
    let graph = NamedNode::new(segment).map_err(|e| format!("`{segment}`: {e}"))?;
    store
        .load_from_slice(
            RdfParser::from_format(RdfFormat::Turtle)
                .with_default_graph(GraphNameRef::NamedNode(graph.as_ref())),
            turtle,
        )
        .map_err(|e| format!("`{segment}` is not Turtle: {e}"))?;
    let results = SparqlEvaluator::new()
        .parse_query(SEGMENT_QUERY)
        .map_err(|e| format!("the stored activity query: {e}"))?
        .on_store(&store)
        .execute()
        .map_err(|e| format!("the activity query over `{segment}`: {e}"))?;
    match results {
        QueryResults::Graph(triples) => triples
            .map(|t| t.map_err(|e| format!("the activity query over `{segment}`: {e}")))
            .collect(),
        _ => Err("the stored activity query is not a CONSTRUCT".to_string()),
    }
}

fn turtle<'a>(triples: impl IntoIterator<Item = TripleRef<'a>>) -> Result<Vec<u8>> {
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle);
    for (prefix, iri) in [
        ("act", VOCAB),
        ("log", "https://ikigai-rs.dev/ns/log#"),
        ("prov", "http://www.w3.org/ns/prov#"),
    ] {
        serializer = serializer
            .with_prefix(prefix, iri)
            .map_err(|e| Error::Endpoint(format!("prefix {prefix}: {e}")))?;
    }
    let mut writer = serializer.for_writer(Vec::new());
    for triple in triples {
        writer
            .serialize_triple(triple)
            .map_err(|e| Error::Endpoint(format!("serializing the activity graph: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| Error::Endpoint(format!("serializing the activity graph: {e}")))
}

/// Read the segment `urn:log:{stamp}` and answer its rollup, refusing a live segment.
async fn rollup(inv: &Invocation<'_>, stamp: &str) -> Result<Vec<Triple>> {
    let segment = format!("{LOG_PREFIX}{stamp}");
    let read = inv.issue(request(Verb::Source, &segment)?).await?;
    if read.expiry == Expiry::Always {
        return Err(Error::Endpoint(format!(
            "`{segment}` is live (still open, or its process stopped without a stop marker): \
             the activity graph reads closed segments only"
        )));
    }
    construct(&segment, &read.bytes).map_err(Error::Endpoint)
}

/// `urn:iki:gonk:activity:segment:{stamp}`.
pub struct Segment;

#[async_trait]
impl Endpoint for Segment {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        source_only(inv, SEGMENT_TEMPLATE)?;
        let stamp = crate::web::binding(inv, "stamp")?;
        let triples = rollup(inv, &stamp).await?;
        Ok(Representation::new(
            ReprType::new(TURTLE),
            turtle(triples.iter().map(Triple::as_ref))?,
        )
        .cacheable())
    }

    fn name(&self) -> &str {
        "gonk-activity-segment"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("One closed log segment's activity graph")
            .summary(
                "The stored CONSTRUCT (urn:iki:gonk:activity:vocab#) over the closed segment \
                 urn:log:{stamp}: an hourly rollup per authenticated caller, door, verb, outcome \
                 and resource, one node per authenticated write, and anonymous load as per-day \
                 counts only. Owner only (urn:cap:log:read). A live segment is refused. Cached \
                 for as long as the segment's own read is, which for a closed one is for good.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .output(TURTLE)
    }
}

/// `urn:iki:gonk:activity`.
pub struct Union;

#[async_trait]
impl Endpoint for Union {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        source_only(inv, ACTIVITY_IRI)?;
        let listing = inv
            .issue(request(Verb::Source, ikigai_log::SEGMENTS_IRI)?)
            .await?;
        let listing = String::from_utf8_lossy(&listing.bytes).into_owned();
        let mut triples = Graph::new();
        for segment in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let Some(stamp) = segment.strip_prefix(LOG_PREFIX) else {
                continue;
            };
            let read = inv
                .issue(request(Verb::Source, &format!("{SEGMENT_PREFIX}{stamp}"))?)
                .await;
            let Ok(read) = read else {
                // The live segment, or one that does not read: not part of the union.
                continue;
            };
            let parsed = RdfParser::from_format(RdfFormat::Turtle).for_slice(&read.bytes);
            for quad in parsed {
                let quad = quad.map_err(|e| Error::Endpoint(format!("`{segment}`: {e}")))?;
                triples.insert(&Triple::from(quad));
            }
        }
        Ok(Representation::new(
            ReprType::new(TURTLE),
            turtle(triples.iter())?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-activity"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("The activity graph")
            .summary(
                "The union of urn:iki:gonk:activity:segment:{stamp} over every closed segment \
                 urn:log:segments lists; the live segment is left out. Owner only \
                 (urn:cap:log:read). Not cached: the listing is not.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .output(TURTLE)
    }
}
