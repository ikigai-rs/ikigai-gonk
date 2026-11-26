//! A repository's harness, its firings and the questions asked of both, as resources, and the
//! Harness page (ledger [#3721](http://localhost:1062/l/default/item/3721)).
//!
//! ```text
//! urn:iki:harness:control:id:{id}         Source  one control: its triples, and how often it fired
//! urn:iki:harness:guide:id:{id}           Source  one guide, and the controls that state it
//! urn:iki:harness:incident:id:{id}        Source  one incident, and the controls that exist for it
//! urn:iki:harness:coverage:query:{name}   Source  one coverage question's answer (or its text)
//! urn:iki:harness:firing:date:{date}      Source  a CONSTRUCT over the firings: control, outcome, count that day
//! urn:iki:gonk:page:harness               Source  the page at /harness
//! ```
//!
//! Each of the five answers `text/html` as well: a page in gonk's chrome, which the HTTP door
//! hands a browser by its `Accept` (ledger #3777), with the other faces offered as links.
//!
//! Every one takes an optional `root=`, the browse root whose harness it reads. Without one it
//! reads the first root in name order that this caller may read and that carries a harness.
//!
//! # What is read, and how
//!
//! The model is the one every gonk-starter repository carries (vocabulary
//! `https://ikigai-rs.dev/ns/harness#`): `.gonk/harness/harness.ttl`, the controls, and
//! `.gonk/state/harness/firings.log`, the working tree's firing segment, one ikigai-log v1 line
//! per refusal or warning. Both are read through the browse family,
//! `urn:repo:{root}:file:{path}`, issued under the CALLER's capability, so a caller without
//! `urn:cap:browse:read:{root}` reads nothing and is told only that, and nothing here holds a
//! path. Each answer is a composition over those reads, so the kernel hangs it from their
//! threads: an edit to `harness.ttl` cuts its root's thread ([`crate::watch`]) and the next read
//! sees it, with no restart. The firing log is git-ignored, and the watch does not cut for what
//! git ignores, so its read is never cached and nor is anything that reads it.
//!
//! The two files are loaded into a store of the request's own ([`Harness::load`]) and the
//! questions are SPARQL over it: the starter's own four (`src/questions/harness/`, their
//! `GRAPH` wrapper dropped, since this store holds one repository's harness and nothing else)
//! and four more. A number on the page is its question's row count, and links to the resource
//! that answers it, the way the Agents panel's numbers do.
//!
//! ★ **Counts only.** A firing carries a session, a tool and a command digest. None of them is
//! served: a day's resource and every answer say how many, never which command. The page reads
//! the session only to join a firing to the ledger item that session worked on, through the
//! session summaries of the ledgers the caller may read (`ledger:SessionSummary`), and says how
//! many items, never which session.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, Error, Invocation, ReprType, Representation, Result, Verb,
};
use ikigai_fn::html::{attr, html_text, url_segment};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term, Triple};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use serde_json::{json, Value};

use crate::render::{self, envelope};
use crate::web::{self, fetch, request, with, Web};

pub mod filter;

/// The harness vocabulary.
pub const H: &str = "https://ikigai-rs.dev/ns/harness#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const PROV: &str = "http://www.w3.org/ns/prov#";
const LOG: &str = "https://ikigai-rs.dev/ns/log#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RS: &str = "http://www.w3.org/2001/sw/DataAccess/tests/result-set#";

/// The controls, in the repository.
pub const GRAPH_FILE: &str = ".gonk/harness/harness.ttl";
/// The working tree's firing segment, in the repository.
pub const FIRINGS_FILE: &str = ".gonk/state/harness/firings.log";

pub const CONTROL_TEMPLATE: &str = "urn:iki:harness:control:id:{id}";
pub const GUIDE_TEMPLATE: &str = "urn:iki:harness:guide:id:{id}";
pub const INCIDENT_TEMPLATE: &str = "urn:iki:harness:incident:id:{id}";
pub const COVERAGE_TEMPLATE: &str = "urn:iki:harness:coverage:query:{name}";
pub const FIRING_TEMPLATE: &str = "urn:iki:harness:firing:date:{date}";
pub const PAGE_IRI: &str = "urn:iki:gonk:page:harness";
pub const PAGE_PATH: &str = "/harness";

const TURTLE: &str = "text/turtle";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain";
const QUERY_TEXT: &str = "application/sparql-query";
const HTML: &str = web::HTML;

/// How a refused id collision begins (ledger #3786).
const COLLISION: &str = "two harness graphs";

/// How far back the page's join reads: "this week".
const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One coverage question.
pub struct Question {
    /// Its name: `urn:iki:harness:coverage:query:{name}`.
    pub name: &'static str,
    /// What the page calls it.
    pub heading: &'static str,
    /// Its SPARQL text.
    pub text: &'static str,
    /// Whether a row is a gap (a control or guide missing something) rather than a tally.
    pub gap: bool,
    /// What one row is, and what several are: the page says "2 sensors", never "2".
    pub rows: [&'static str; 2],
}

/// The questions, in the page's order.
pub const QUESTIONS: [Question; 8] = [
    Question {
        name: "sensors-never-fired",
        heading: "Sensors never fired",
        text: include_str!("questions/harness/sensors-never-fired.rq"),
        gap: true,
        rows: ["sensor", "sensors"],
    },
    Question {
        name: "guides-without-sensor",
        heading: "Guides without a check",
        text: include_str!("questions/harness/guides-without-sensor.rq"),
        gap: true,
        rows: ["guide", "guides"],
    },
    Question {
        name: "sensors-without-guide",
        heading: "Sensors without a guide",
        text: include_str!("questions/harness/sensors-without-guide.rq"),
        gap: true,
        rows: ["sensor", "sensors"],
    },
    Question {
        name: "incidents-without-sensor",
        heading: "Incidents without a sensor",
        text: include_str!("questions/harness/incidents-without-sensor.rq"),
        gap: true,
        rows: ["incident", "incidents"],
    },
    Question {
        name: "repeated-warnings",
        heading: "Repeated warnings",
        text: include_str!("questions/harness/repeated-warnings.rq"),
        gap: false,
        rows: ["control", "controls"],
    },
    Question {
        name: "top-recurring",
        heading: "Top recurring",
        text: include_str!("questions/harness/top-recurring.rq"),
        gap: false,
        rows: ["control", "controls"],
    },
    Question {
        name: "firings-by-day",
        heading: "Firings by day",
        text: include_str!("questions/harness/firings-by-day.rq"),
        gap: false,
        rows: ["daily tally", "daily tallies"],
    },
    Question {
        name: "firings-by-control",
        heading: "Firings by control",
        text: include_str!("questions/harness/firings-by-control.rq"),
        gap: false,
        rows: ["control", "controls"],
    },
];

/// A day's firings, `DAY` replaced by a checked `YYYY-MM-DD`.
const FIRINGS_ON_DATE: &str = include_str!("questions/harness/firings-on-date.rq");

pub fn question(name: &str) -> Result<&'static Question> {
    QUESTIONS
        .iter()
        .find(|q| q.name == name)
        .ok_or_else(|| Error::NotFound(format!("no harness coverage question is named `{name}`")))
}

/// The question's first `# QUESTION:` line, without the marker.
fn asks(q: &Question) -> &'static str {
    q.text
        .lines()
        .find_map(|l| l.strip_prefix("# QUESTION: "))
        .unwrap_or(q.heading)
}

// ----------------------------------------------------------------------------- the firings

/// One line of a firing segment: a refusal or a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    /// `{segment}:{seq}`, the entry's IRI as ikigai-log transrepts it.
    pub node: String,
    /// `Refusal` or `Warning`.
    pub class: &'static str,
    /// The control's IRI.
    pub control: String,
    /// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    pub at: String,
    pub session: Option<String>,
    pub tool: Option<String>,
    pub cmd: Option<String>,
    /// The browse root the firing names (`repo=`), when it names one: a pack's firings live
    /// outside any tree ([`crate::packs`]). `repo=-` names none.
    pub repo: Option<String>,
}

/// The firings in one ikigai-log v1 segment, in the line grammar `tools/harness.py` writes.
/// An entry of any other class is not a firing and is skipped; a segment that does not begin
/// as one is refused, never half read.
pub fn parse_segment(text: &str) -> std::result::Result<Vec<Firing>, String> {
    let Some(rest) = text.strip_prefix("# ikigai-log v1\n") else {
        return Err("the firing log does not begin with `# ikigai-log v1`".to_string());
    };
    let (head, body) = rest
        .split_once("\n\n")
        .ok_or("no blank line ends the firing log's header")?;
    let mut prefixes = BTreeMap::new();
    let mut name = None;
    for line in head.lines() {
        if let Some(p) = line.strip_prefix("@prefix") {
            let mut words = p.split_whitespace();
            if let (Some(prefix), Some(ns)) = (words.next(), words.next()) {
                let ns = ns.trim_start_matches('<').trim_end_matches('>');
                prefixes.insert(prefix.trim_end_matches(':').to_string(), ns.to_string());
            }
        } else if let Some(n) = line.strip_prefix("@name") {
            name = Some(n.trim().to_string());
        }
    }
    let name = name.ok_or("the firing log's header names no segment (`@name`)")?;
    let mut firings = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(at), Some(curie), Some(control)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let class = match curie.split_once(':') {
            Some((p, local)) if prefixes.get(p).map(String::as_str) == Some(H) => local,
            _ => continue,
        };
        let class = match class {
            "Refusal" => "Refusal",
            "Warning" => "Warning",
            _ => continue,
        };
        if at.len() != 24 || !at.ends_with('Z') || NamedNode::new(control).is_err() {
            continue;
        }
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for word in words {
            if let Some((k, v)) = word.split_once('=') {
                fields.insert(k, v);
            }
        }
        let Some(seq) = fields.get("seq").and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        firings.push(Firing {
            node: format!("{name}:{seq}"),
            class,
            control: control.to_string(),
            at: at.to_string(),
            session: fields.get("session").map(|s| s.to_string()),
            tool: fields.get("tool").map(|s| s.to_string()),
            cmd: fields.get("cmd").map(|s| s.to_string()),
            repo: fields
                .get("repo")
                .filter(|r| **r != "-")
                .map(|s| s.to_string()),
        });
    }
    Ok(firings)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, ikigai-log's fixed-width UTC column, as milliseconds since the
/// epoch.
pub fn stamp_ms(at: &str) -> Option<i64> {
    let b = at.as_bytes();
    if b.len() != 24 || b[23] != b'Z' {
        return None;
    }
    let n = |from: usize, to: usize| at.get(from..to)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss, ms) = (n(11, 13)?, n(14, 16)?, n(17, 19)?, n(20, 23)?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((((days * 24 + hh) * 60 + mm) * 60 + ss) * 1000 + ms)
}

fn nn(iri: &str) -> Option<NamedNode> {
    NamedNode::new(iri).ok()
}

fn firing_quads(f: &Firing) -> Vec<Quad> {
    let (Some(node), Some(control)) = (nn(&f.node), nn(&f.control)) else {
        return Vec::new();
    };
    let p = |iri: String| NamedNode::new_unchecked(iri);
    let quad = |pred: NamedNode, o: Term| Quad::new(node.clone(), pred, o, GraphName::DefaultGraph);
    let mut out = vec![
        quad(
            p(RDF_TYPE.into()),
            NamedNode::new_unchecked(format!("{H}{}", f.class)).into(),
        ),
        quad(p(format!("{H}firedControl")), control.clone().into()),
        quad(p(format!("{LOG}subject")), control.into()),
        quad(
            p(format!("{PROV}startedAtTime")),
            Literal::new_typed_literal(&f.at, NamedNode::new_unchecked(format!("{XSD}dateTime")))
                .into(),
        ),
    ];
    if let Some(session) = f.session.as_deref().and_then(nn) {
        out.push(quad(p(format!("{H}session")), session.into()));
    }
    if let Some(tool) = &f.tool {
        out.push(quad(
            p(format!("{H}tool")),
            Literal::new_simple_literal(tool).into(),
        ));
    }
    if let Some(cmd) = &f.cmd {
        out.push(quad(
            p(format!("{H}commandDigest")),
            Literal::new_simple_literal(cmd).into(),
        ));
    }
    out
}

// ------------------------------------------------------------------------------ the store

/// One repository's harness and firings, loaded for one request.
pub struct Harness {
    pub root: String,
    store: Store,
    pub firings: Vec<Firing>,
    /// The packs loaded beside the root's own graph (ledger #3786), in config order.
    pub packs: Vec<crate::packs::Applied>,
    /// Which pack each guide, control and incident came from: its index in [`Harness::packs`].
    pub origins: BTreeMap<String, usize>,
    /// What the page states in words about the packs: unbound adoption, a refused shape.
    pub notes: Vec<String>,
    /// Whether the root has a graph of its own, beside its packs.
    pub own: bool,
    /// The rules the request asked to see (ledger #3787): the page draws them, and every link
    /// of a page carries them.
    pub filters: filter::Filters,
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("the harness store: {e}"))
}

/// `urn:repo:{root}:file:{path}`, read under the caller's capability. `Ok(None)` when the file
/// is not there.
pub(crate) async fn read_file(
    inv: &Invocation<'_>,
    root: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let iri = format!("urn:repo:{root}:file:{path}");
    match fetch(inv, request(Verb::Source, &iri)?).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Error::NotFound(_)) | Err(Error::Unresolved(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

impl Harness {
    /// The harness `root=` names, or the first readable root that carries one, with no packs.
    pub async fn load(web: &Web, inv: &Invocation<'_>) -> Result<Harness> {
        Harness::load_with(web, &crate::packs::Config::default(), inv).await
    }

    /// The harness `root=` names, or the first readable root that carries one: its own graph
    /// and every pack that applies to it ([`crate::packs::assemble`]). A root with no graph of
    /// its own carries a harness when a pack applies to it.
    pub async fn load_with(
        web: &Web,
        packs: &crate::packs::Config,
        inv: &Invocation<'_>,
    ) -> Result<Harness> {
        let filters = filter::Filters::from_inv(inv)?;
        let asked = inv
            .optional_str("root")?
            .map(str::trim)
            .filter(|r| !r.is_empty());
        if let Some(root) = asked {
            // A root this caller may not read is refused before anything of it is read, so
            // `root=` cannot reach one the picker does not offer.
            if !crate::k::reads_root(inv.capability, root) {
                let signed_in = inv.capability.principal().is_some();
                return Err(Error::Denied(
                    web::Lacking::repositories().plain(signed_in).into(),
                ));
            }
        }
        let roots = match asked {
            Some(root) => vec![root.to_string()],
            None => crate::k::readable_roots(web, inv.capability),
        };
        if roots.is_empty() && web.browse_roots.is_empty() {
            return Err(Error::NotFound(
                "this server has no browse root configured (`gonk.browse.root`), so it has no \
                 harness to read"
                    .to_string(),
            ));
        }
        if roots.is_empty() {
            let signed_in = inv.capability.principal().is_some();
            return Err(Error::Denied(
                web::Lacking::repositories().plain(signed_in).into(),
            ));
        }
        for root in roots {
            let own = read_file(inv, &root, GRAPH_FILE).await?;
            if packs.is_empty() {
                if let Some(turtle) = own {
                    let mut harness = Harness::from_bytes(
                        &root,
                        &turtle,
                        read_file(inv, &root, FIRINGS_FILE).await?,
                    )?;
                    harness.filters = filters;
                    return Ok(harness);
                }
                continue;
            }
            let assembly = crate::packs::assemble(web, packs, inv, &root, own.as_deref()).await?;
            if own.is_none() && assembly.packs.is_empty() {
                continue;
            }
            let segment = read_file(inv, &root, FIRINGS_FILE).await?;
            let mut harness = Harness::from_parts(&root, own.as_deref(), segment, assembly)?;
            harness.filters = filters;
            return Ok(harness);
        }
        Err(Error::NotFound(format!(
            "no repository this grant may read carries a harness (`{GRAPH_FILE}`)"
        )))
    }

    /// A harness from its two files' bytes.
    pub fn from_bytes(root: &str, turtle: &[u8], segment: Option<Vec<u8>>) -> Result<Harness> {
        Harness::from_parts(
            root,
            Some(turtle),
            segment,
            crate::packs::Assembly::default(),
        )
    }

    /// A harness from the root's own graph (if any), its tree's firing segment, and the packs
    /// that apply to it. Two graphs that give the same kind and id to different IRIs are refused,
    /// naming both.
    pub fn from_parts(
        root: &str,
        turtle: Option<&[u8]>,
        segment: Option<Vec<u8>>,
        assembly: crate::packs::Assembly,
    ) -> Result<Harness> {
        let store = Store::new().map_err(store_err)?;
        if let Some(turtle) = turtle {
            let base = format!("urn:repo:{root}:file:{GRAPH_FILE}");
            store
                .load_from_slice(
                    RdfParser::from_format(RdfFormat::Turtle)
                        .with_base_iri(&base)
                        .map_err(store_err)?,
                    turtle,
                )
                .map_err(|e| {
                    Error::Endpoint(format!("`{GRAPH_FILE}` in `{root}` is not Turtle: {e}"))
                })?;
        }
        let mut origins = BTreeMap::new();
        for (at, pack) in assembly.packs.iter().enumerate() {
            for t in &pack.triples {
                store
                    .insert(&Quad::new(
                        t.subject.clone(),
                        t.predicate.clone(),
                        t.object.clone(),
                        GraphName::DefaultGraph,
                    ))
                    .map_err(store_err)?;
                let typed = t.predicate.as_str() == RDF_TYPE
                    && ["Guide", "Control", "Incident"]
                        .iter()
                        .any(|c| value(&t.object) == format!("{H}{c}"));
                if typed {
                    if let oxigraph::model::NamedOrBlankNode::NamedNode(n) = &t.subject {
                        origins.entry(n.as_str().to_string()).or_insert(at);
                    }
                }
            }
        }
        let mut firings = match segment {
            Some(bytes) => parse_segment(&String::from_utf8_lossy(&bytes))
                .map_err(|e| Error::Endpoint(format!("`{FIRINGS_FILE}` in `{root}`: {e}")))?,
            None => Vec::new(),
        };
        firings.extend(assembly.firings);
        for firing in &firings {
            for quad in firing_quads(firing) {
                store.insert(&quad).map_err(store_err)?;
            }
        }
        let harness = Harness {
            root: root.to_string(),
            store,
            firings,
            packs: assembly.packs,
            origins,
            notes: assembly.notes,
            own: turtle.is_some(),
            filters: filter::Filters::default(),
        };
        harness.refuse_collisions()?;
        Ok(harness)
    }

    /// The query each link of a page of this harness carries: the root and the filters.
    pub fn here(&self) -> String {
        self.filters.query(&self.root)
    }

    /// Refuse two IRIs of one kind that share an id: the resource names would be ambiguous.
    fn refuse_collisions(&self) -> Result<()> {
        let mut seen: BTreeMap<(&str, String), String> = BTreeMap::new();
        for (s, props) in self.described()? {
            for kind in [Kind::Control, Kind::Guide, Kind::Incident] {
                if !is_a(&props, kind.class()) {
                    continue;
                }
                let id = local(&s).to_string();
                match seen.get(&(kind.word(), id.clone())) {
                    Some(other) if *other != s => {
                        return Err(Error::Endpoint(format!(
                            "{COLLISION} loaded for `{}` give the {} id `{id}` to \
                             different IRIs, <{other}> and <{s}>, so neither can be named",
                            self.root,
                            kind.word()
                        )))
                    }
                    _ => {
                        seen.insert((kind.word(), id), s.clone());
                    }
                }
            }
        }
        Ok(())
    }

    /// The pack `iri` came from, and how: `None` for the root's own graph.
    pub fn origin(&self, iri: &str) -> Option<(&crate::packs::Applied, Option<&str>)> {
        let pack = self.packs.get(*self.origins.get(iri)?)?;
        let words = match pack.narrowed.get(iri) {
            Some(words) => words.as_deref(),
            None => match &pack.how {
                crate::packs::How::Shape(words) => words.as_deref(),
                crate::packs::How::Adopted => None,
            },
        };
        Some((pack, words))
    }

    fn query(&self, text: &str) -> Result<QueryResults<'_>> {
        SparqlEvaluator::new()
            .parse_query(text)
            .map_err(|e| Error::Endpoint(format!("a harness question is not SPARQL: {e}")))?
            .on_store(&self.store)
            .execute()
            .map_err(|e| Error::Endpoint(format!("a harness question did not run: {e}")))
    }

    /// A SELECT's variables and rows, each cell its term's value.
    pub fn select(&self, text: &str) -> Result<Rows> {
        let QueryResults::Solutions(solutions) = self.query(text)? else {
            return Err(Error::Endpoint("a harness question is not a SELECT".into()));
        };
        let vars: Vec<String> = solutions
            .variables()
            .iter()
            .map(|v| v.as_str().to_string())
            .collect();
        let mut rows = Vec::new();
        for solution in solutions {
            let solution = solution.map_err(store_err)?;
            rows.push(
                vars.iter()
                    .map(|v| solution.get(v.as_str()).cloned())
                    .collect(),
            );
        }
        Ok(Rows { vars, rows })
    }

    /// A CONSTRUCT's triples.
    pub fn construct(&self, text: &str) -> Result<Vec<Triple>> {
        let QueryResults::Graph(triples) = self.query(text)? else {
            return Err(Error::Endpoint("a harness read is not a CONSTRUCT".into()));
        };
        triples.map(|t| t.map_err(store_err)).collect()
    }

    /// Every subject's properties, `predicate -> values`, in IRI order.
    fn described(&self) -> Result<BTreeMap<String, BTreeMap<String, Vec<Term>>>> {
        let mut out: BTreeMap<String, BTreeMap<String, Vec<Term>>> = BTreeMap::new();
        for quad in self.store.iter() {
            let quad = quad.map_err(store_err)?;
            let subject = match &quad.subject {
                oxigraph::model::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                _ => continue,
            };
            out.entry(subject)
                .or_default()
                .entry(quad.predicate.as_str().to_string())
                .or_default()
                .push(quad.object);
        }
        Ok(out)
    }

    /// The subject of class `class` whose IRI ends in `id` (after its last `:`, `/` or `#`).
    fn find(&self, class: &str, id: &str) -> Result<Option<String>> {
        let described = self.described()?;
        Ok(described.into_iter().find_map(|(s, props)| {
            let typed = props
                .get(RDF_TYPE)
                .is_some_and(|t| t.iter().any(|t| value(t) == format!("{H}{class}")));
            let tail = s.rsplit([':', '/', '#']).next().unwrap_or("");
            (tail == id && (typed || class == "Incident" && self.cited(&s))).then_some(s)
        }))
    }

    fn cited(&self, iri: &str) -> bool {
        self.store
            .quads_for_pattern(
                None,
                Some(NamedNode::new_unchecked(format!("{H}existsFor")).as_ref()),
                nn(iri).as_ref().map(|n| n.as_ref().into()),
                None,
            )
            .next()
            .is_some()
    }
}

/// A SELECT's answer.
pub struct Rows {
    pub vars: Vec<String>,
    pub rows: Vec<Vec<Option<Term>>>,
}

/// A term's value: an IRI's text, a literal's lexical form.
pub fn value(term: &Term) -> String {
    match term {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    }
}

fn cell(row: &[Option<Term>], vars: &[String], name: &str) -> Option<String> {
    let at = vars.iter().position(|v| v == name)?;
    row.get(at)?.as_ref().map(value)
}

// ------------------------------------------------------------------------------ the faces

fn face(inv: &Invocation<'_>, default: &str) -> Result<String> {
    Ok(inv
        .optional_str("as")?
        .map(|a| a.split(';').next().unwrap_or(a).trim().to_string())
        .unwrap_or_else(|| default.to_string()))
}

fn refuse_face(asked: &str, served: &[&str]) -> Error {
    Error::InvalidArgument {
        name: "as".to_string(),
        detail: format!(
            "`{asked}` is not a face this resource serves: {}",
            served.join(", ")
        ),
    }
}

fn answer(media: &str, bytes: Vec<u8>) -> Representation {
    Representation::new(ReprType::new(media).with_param("charset", "utf-8"), bytes).cacheable()
}

fn turtle(triples: &[Triple]) -> Result<Vec<u8>> {
    let mut out = RdfSerializer::from_format(RdfFormat::Turtle)
        .with_prefix("harness", H)
        .map_err(store_err)?
        .with_prefix("rdfs", "http://www.w3.org/2000/01/rdf-schema#")
        .map_err(store_err)?
        .for_writer(Vec::new());
    for t in triples {
        out.serialize_triple(t).map_err(store_err)?;
    }
    out.finish().map_err(store_err)
}

/// A predicate's short name: its local part in the harness and RDFS vocabularies.
fn short(predicate: &str) -> String {
    for (ns, prefix) in [
        (H, ""),
        ("http://www.w3.org/2000/01/rdf-schema#", ""),
        ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", ""),
        ("http://purl.org/dc/terms/", ""),
    ] {
        if let Some(local) = predicate.strip_prefix(ns) {
            return format!("{prefix}{local}");
        }
    }
    predicate.to_string()
}

fn args() -> [ArgSpec; 2] {
    [
        web::arg(
            "root",
            "The browse root whose harness to read; the first readable one that has one if absent.",
        )
        .optional(),
        web::arg(
            "as",
            "The face: text/turtle, application/json, text/plain, or text/html (the page a \
             browser is given).",
        )
        .optional(),
    ]
}

fn id_arg() -> ArgSpec {
    ArgSpec::new("id")
        .summary("The id: the last segment of its IRI.")
        .class(format!("{XSD}string"))
        .binding()
}

/// A stable, lowercase kebab-case id, the starter's rule.
fn checked_id(id: &str) -> Result<&str> {
    let ok = id.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    }) && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id.len() <= 128;
    ok.then_some(id).ok_or_else(|| Error::InvalidArgument {
        name: "id".to_string(),
        detail: format!("`{id}` is not a harness id (lowercase kebab-case)"),
    })
}

// ------------------------------------------------------------------------------ one thing

/// Which of the three keyed things a resource is.
#[derive(Clone, Copy)]
pub enum Kind {
    Control,
    Guide,
    Incident,
}

impl Kind {
    fn class(self) -> &'static str {
        match self {
            Kind::Control => "Control",
            Kind::Guide => "Guide",
            Kind::Incident => "Incident",
        }
    }
    fn word(self) -> &'static str {
        match self {
            Kind::Control => "control",
            Kind::Guide => "guide",
            Kind::Incident => "incident",
        }
    }
    /// The predicate that points at one of these from a control, if any.
    fn cited_by(self) -> Option<&'static str> {
        match self {
            Kind::Control => None,
            Kind::Guide => Some("statedBy"),
            Kind::Incident => Some("existsFor"),
        }
    }
}

/// `urn:iki:harness:{control,guide,incident}:id:{id}`.
pub struct Thing {
    pub web: Arc<Web>,
    /// The packs this server binds (ledger #3786); the default binds none.
    pub packs: Arc<crate::packs::Config>,
    pub kind: Kind,
}

#[async_trait]
impl Endpoint for Thing {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = web::binding(inv, "id")?;
        let id = checked_id(&id)?;
        let asked = face(inv, TURTLE)?;
        if ![TURTLE, JSON, TEXT, HTML].contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &[TURTLE, JSON, TEXT, HTML]));
        }
        let harness = Harness::load_with(&self.web, &self.packs, inv).await?;
        let Some(subject) = harness.find(self.kind.class(), id)? else {
            return Err(Error::NotFound(format!(
                "the harness of `{}` has no {} `{id}`",
                harness.root,
                self.kind.word()
            )));
        };
        let described = harness.described()?;
        if asked == HTML {
            let (title, html) = match self.kind {
                Kind::Control => control_html(&harness, &described, &subject),
                Kind::Guide => guide_html(inv, &harness, &described, &subject).await,
                Kind::Incident => incident_html(&harness, &described, &subject),
            };
            let what = match self.kind {
                Kind::Control => direction_word(described.get(&subject)),
                other => other.word(),
            };
            let message = format!(
                "A {what} in the harness of the repository \"{}\".",
                harness.root
            );
            let iri = format!("urn:iki:harness:{}:id:{id}", self.kind.word());
            let from = from_pack(&harness, &subject)
                .map(|f| format!("<p class=\"harness-facts\">{}</p>", html_text(&f)))
                .unwrap_or_default();
            let html = format!(
                "{}{from}{html}{}",
                back(&harness),
                other_formats(
                    &iri,
                    &harness.root,
                    &[(TEXT, "Text"), (TURTLE, "Turtle"), (JSON, "JSON")]
                )
            );
            return harness_page(&self.web, inv, &title, &message, Some(html));
        }
        let props = described.get(&subject).cloned().unwrap_or_default();
        let node = NamedNode::new_unchecked(subject.clone());
        let mut triples: Vec<Triple> = props
            .iter()
            .flat_map(|(p, os)| {
                let node = node.clone();
                os.iter().map(move |o| {
                    Triple::new(node.clone(), NamedNode::new_unchecked(p.clone()), o.clone())
                })
            })
            .collect();
        // Who cites it, and how often it fired: the two facts a reader of one thing asks next.
        let citing: Vec<String> = match self.kind.cited_by() {
            Some(p) => described
                .iter()
                .filter(|(_, ps)| {
                    ps.get(&format!("{H}{p}"))
                        .is_some_and(|os| os.iter().any(|o| value(o) == subject))
                })
                .map(|(s, _)| s.clone())
                .collect(),
            None => Vec::new(),
        };
        let fired = harness
            .firings
            .iter()
            .filter(|f| f.control == subject)
            .count();
        if matches!(self.kind, Kind::Control) {
            triples.push(Triple::new(
                node.clone(),
                NamedNode::new_unchecked(format!("{H}entryCount")),
                Literal::from(fired as i64),
            ));
        }
        let bytes = match asked.as_str() {
            TURTLE => turtle(&triples)?,
            JSON => {
                let mut properties = serde_json::Map::new();
                for (p, os) in &props {
                    properties.insert(short(p), os.iter().map(value).collect());
                }
                let mut doc = json!({
                    "iri": subject,
                    "id": id,
                    "root": harness.root,
                    "properties": properties,
                });
                if matches!(self.kind, Kind::Control) {
                    doc["firings"] = json!(fired);
                }
                if let Some((pack, words)) = harness.origin(&subject) {
                    doc["pack"] = json!({
                        "name": pack.name,
                        "label": pack.label,
                        "version": pack.version,
                        "appliesTo": words,
                    });
                }
                if let Some(p) = self.kind.cited_by() {
                    doc[format!("{p}By")] = json!(citing);
                }
                serde_json::to_vec_pretty(&doc).map_err(store_err)?
            }
            _ => {
                let mut out = String::new();
                let label = props
                    .get(RDFS_LABEL)
                    .and_then(|l| l.first())
                    .map(value)
                    .unwrap_or_else(|| id.to_string());
                let _ = writeln!(out, "{label}\n{subject}");
                for (p, os) in &props {
                    for o in os {
                        let _ = writeln!(out, "{}: {}", short(p), value(o));
                    }
                }
                if matches!(self.kind, Kind::Control) {
                    let _ = writeln!(out, "firings: {fired}");
                }
                if let Some(from) = from_pack(&harness, &subject) {
                    let _ = writeln!(out, "{from}");
                }
                if let Some(p) = self.kind.cited_by() {
                    for c in &citing {
                        let _ = writeln!(out, "{p} by: {c}");
                    }
                }
                out.into_bytes()
            }
        };
        Ok(answer(&asked, bytes))
    }

    fn name(&self) -> &str {
        match self.kind {
            Kind::Control => "gonk-harness-control",
            Kind::Guide => "gonk-harness-guide",
            Kind::Incident => "gonk-harness-incident",
        }
    }

    fn describe(&self) -> Description {
        let [root, face] = args();
        filter::described(Description::new(self.name()), false)
            .title(format!("One harness {}", self.kind.word()))
            .summary(
                "Its triples in the repository's `.gonk/harness/harness.ttl`, read through the \
                 browse family under the caller's capability; a control also carries how often it \
                 fired (harness:entryCount), a guide or an incident the controls that cite it.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(id_arg())
            .input(root)
            .input(face)
            .output(TURTLE)
            .output(JSON)
            .output(TEXT)
            .output(HTML)
    }
}

// ------------------------------------------------------------------------------ coverage

/// `urn:iki:harness:coverage:query:{name}`.
pub struct Coverage {
    pub web: Arc<Web>,
    /// The packs this server binds (ledger #3786); the default binds none.
    pub packs: Arc<crate::packs::Config>,
}

fn result_json(rows: &Rows) -> Value {
    let bindings: Vec<Value> = rows
        .rows
        .iter()
        .map(|row| {
            let mut b = serde_json::Map::new();
            for (var, term) in rows.vars.iter().zip(row) {
                let Some(term) = term else { continue };
                let kind = if matches!(term, Term::NamedNode(_)) {
                    "uri"
                } else {
                    "literal"
                };
                b.insert(var.clone(), json!({ "type": kind, "value": value(term) }));
            }
            Value::Object(b)
        })
        .collect();
    json!({ "head": { "vars": rows.vars }, "results": { "bindings": bindings } })
}

/// The answer as a W3C result set, its solutions and bindings skolemized under `iri`.
fn result_turtle(iri: &str, q: &Question, rows: &Rows) -> Result<Vec<u8>> {
    let node = |s: String| NamedNode::new_unchecked(s);
    let p = |ns: &str, l: &str| NamedNode::new_unchecked(format!("{ns}{l}"));
    let set = node(iri.to_string());
    let mut t = vec![
        Triple::new(set.clone(), p(RDF_TYPE, ""), p(RS, "ResultSet")),
        Triple::new(set.clone(), p(RDF_TYPE, ""), p(H, "Answer")),
        Triple::new(
            set.clone(),
            p(RDFS_COMMENT, ""),
            Literal::new_simple_literal(asks(q)),
        ),
        Triple::new(
            set.clone(),
            p(H, "rowCount"),
            Literal::from(rows.rows.len() as i64),
        ),
    ];
    for var in &rows.vars {
        t.push(Triple::new(
            set.clone(),
            p(RS, "resultVariable"),
            Literal::new_simple_literal(var),
        ));
    }
    for (i, row) in rows.rows.iter().enumerate() {
        let solution = node(format!("{iri}:solution:{}", i + 1));
        t.push(Triple::new(
            set.clone(),
            p(RS, "solution"),
            solution.clone(),
        ));
        t.push(Triple::new(
            solution.clone(),
            p(RS, "index"),
            Literal::from(i as i64 + 1),
        ));
        for (var, term) in rows.vars.iter().zip(row) {
            let Some(term) = term else { continue };
            let binding = node(format!("{iri}:solution:{}:binding:{var}", i + 1));
            t.push(Triple::new(
                solution.clone(),
                p(RS, "binding"),
                binding.clone(),
            ));
            t.push(Triple::new(
                binding.clone(),
                p(RS, "variable"),
                Literal::new_simple_literal(var),
            ));
            t.push(Triple::new(binding, p(RS, "value"), term.clone()));
        }
    }
    turtle(&t)
}

fn result_text(q: &Question, rows: &Rows) -> String {
    let mut out = format!(
        "{}\n{} rows\n\n{}\n",
        asks(q),
        rows.rows.len(),
        rows.vars.join("\t")
    );
    for row in &rows.rows {
        let cells: Vec<String> = row
            .iter()
            .map(|t| t.as_ref().map(value).unwrap_or_default())
            .collect();
        let _ = writeln!(out, "{}", cells.join("\t"));
    }
    out
}

#[async_trait]
impl Endpoint for Coverage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let q = question(&web::binding(inv, "name")?)?;
        let asked = face(inv, JSON)?;
        let served = [JSON, TURTLE, TEXT, QUERY_TEXT, HTML];
        if !served.contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &served));
        }
        if asked == QUERY_TEXT {
            return Ok(answer(QUERY_TEXT, q.text.as_bytes().to_vec()));
        }
        let harness = Harness::load_with(&self.web, &self.packs, inv).await?;
        let rows = harness.select(q.text)?;
        let iri = format!("urn:iki:harness:coverage:query:{}", q.name);
        if asked == HTML {
            // The page a filtered index links to answers over the same rules (ledger #3787).
            let all = harness.described()?;
            let sel = filter::Selection::new(&harness, &all)?;
            let status = if sel.filtered() && !sel.whole(q) {
                sel.summary()
            } else {
                String::new()
            };
            let rows = sel.restrict(&harness, q, rows);
            let html = format!(
                "{}{}{}",
                back(&harness),
                coverage_html(&harness, q, &rows)?,
                other_formats(
                    &iri,
                    &harness.root,
                    &[
                        (TEXT, "Text"),
                        (TURTLE, "Turtle"),
                        (JSON, "SPARQL JSON"),
                        (QUERY_TEXT, "The SPARQL query"),
                    ]
                )
            );
            return page_with(&self.web, inv, q.heading, asks(q), "", &status, Some(html));
        }
        let bytes = match asked.as_str() {
            JSON => serde_json::to_vec_pretty(&result_json(&rows)).map_err(store_err)?,
            TURTLE => result_turtle(&iri, q, &rows)?,
            _ => result_text(q, &rows).into_bytes(),
        };
        Ok(answer(&asked, bytes))
    }

    fn name(&self) -> &str {
        "gonk-harness-coverage"
    }

    fn describe(&self) -> Description {
        let [root, _] = args();
        filter::described(Description::new(self.name()), false)
            .title("One harness coverage question")
            .summary(
                "A stored SPARQL question asked of a repository's harness and its firings: its \
                 result set as SPARQL JSON, as an rs:ResultSet in Turtle, as text, or the \
                 question's own text (application/sparql-query). Counts only: no command digest \
                 or session is selected.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("name")
                    .summary("Which question.")
                    .class(format!("{XSD}string"))
                    .one_of(QUESTIONS.iter().map(|q| q.name))
                    .binding(),
            )
            .input(root)
            .input(
                web::arg("as", "The face.")
                    .one_of([JSON, TURTLE, TEXT, QUERY_TEXT, HTML])
                    .default_value(JSON)
                    .optional(),
            )
            .output(JSON)
            .output(TURTLE)
            .output(TEXT)
            .output(QUERY_TEXT)
            .output(HTML)
    }
}

// ------------------------------------------------------------------------------ a day

/// `YYYY-MM-DD`, or a refusal.
fn checked_date(date: &str) -> Result<&str> {
    let b = date.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    ok.then_some(date).ok_or_else(|| Error::InvalidArgument {
        name: "date".to_string(),
        detail: format!("`{date}` is not a UTC day, YYYY-MM-DD"),
    })
}

/// `urn:iki:harness:firing:date:{date}`.
pub struct FiringDay {
    pub web: Arc<Web>,
    /// The packs this server binds (ledger #3786); the default binds none.
    pub packs: Arc<crate::packs::Config>,
}

/// One day's rows: `(control, outcome word, count)`.
pub fn day_rows(triples: &[Triple]) -> Vec<(String, String, i64)> {
    let mut by: BTreeMap<String, (String, String, i64)> = BTreeMap::new();
    for t in triples {
        let row = by.entry(t.subject.to_string()).or_default();
        let o = value(&t.object);
        match t.predicate.as_str().strip_prefix(H) {
            Some("firedControl") => row.0 = o,
            Some("outcome") => row.1 = o.strip_prefix(H).unwrap_or(&o).to_string(),
            Some("entryCount") => row.2 = o.parse().unwrap_or(0),
            _ => {}
        }
    }
    let mut rows: Vec<_> = by.into_values().collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    rows
}

#[async_trait]
impl Endpoint for FiringDay {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let date = web::binding(inv, "date")?;
        let date = checked_date(&date)?;
        let asked = face(inv, TURTLE)?;
        if ![TURTLE, JSON, TEXT, HTML].contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &[TURTLE, JSON, TEXT, HTML]));
        }
        let harness = Harness::load_with(&self.web, &self.packs, inv).await?;
        let triples = harness.construct(&FIRINGS_ON_DATE.replace("DAY", date))?;
        let rows = day_rows(&triples);
        if asked == HTML {
            let iri = format!("urn:iki:harness:firing:date:{date}");
            let html = format!(
                "{}{}{}",
                back(&harness),
                day_html(&harness, date, &rows)?,
                other_formats(
                    &iri,
                    &harness.root,
                    &[(TEXT, "Text"), (TURTLE, "Turtle"), (JSON, "JSON")]
                )
            );
            return harness_page(
                &self.web,
                inv,
                &format!("Harness firings on {date}"),
                "Each control that refused or warned on this day (UTC), and how many times. Counts \
                 only: no command or session is shown.",
                Some(html),
            );
        }
        let bytes = match asked.as_str() {
            TURTLE => turtle(&triples)?,
            JSON => serde_json::to_vec_pretty(&json!({
                "date": date,
                "root": harness.root,
                "rows": rows.iter().map(|(c, o, n)| json!({"control": c, "outcome": o, "count": n})).collect::<Vec<_>>(),
            }))
            .map_err(store_err)?,
            _ => {
                let mut out = format!("Harness firings on {date}\n\ncontrol\toutcome\tcount\n");
                for (c, o, n) in &rows {
                    let _ = writeln!(out, "{c}\t{o}\t{n}");
                }
                out.into_bytes()
            }
        };
        Ok(answer(&asked, bytes))
    }

    fn name(&self) -> &str {
        "gonk-harness-firing-date"
    }

    fn describe(&self) -> Description {
        let [root, face] = args();
        filter::described(Description::new(self.name()), false)
            .title("A day's harness firings")
            .summary(
                "A CONSTRUCT over the repository's firing segment: for each control that fired \
                 that UTC day, its outcome (harness:refuse or harness:warn) and how many times \
                 (harness:entryCount). Counts only.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("date")
                    .summary("A UTC day, YYYY-MM-DD.")
                    .class(format!("{XSD}string"))
                    .binding(),
            )
            .input(root)
            .input(face)
            .output(TURTLE)
            .output(JSON)
            .output(TEXT)
            .output(HTML)
    }
}

// ------------------------------------------------------------------------------ the page

/// The HTTP routes, `(path, IRI template)`: each resource at a path of its own under
/// [`PAGE_PATH`], so a number on the page is an ordinary link.
pub const ROUTES: [(&str, &str); 6] = [
    (PAGE_PATH, PAGE_IRI),
    ("/harness/control/{id}", CONTROL_TEMPLATE),
    ("/harness/guide/{id}", GUIDE_TEMPLATE),
    ("/harness/incident/{id}", INCIDENT_TEMPLATE),
    ("/harness/coverage/{name}", COVERAGE_TEMPLATE),
    ("/harness/firing/{date}", FIRING_TEMPLATE),
];

/// The path of `iri`'s route under [`PAGE_PATH`], or the page's own path.
fn route_path(iri: &str) -> String {
    ROUTES[1..]
        .iter()
        .find_map(|(path, template)| {
            let prefix = template.split('{').next()?;
            let key = iri.strip_prefix(prefix)?;
            Some(format!("{}/{}", path.rsplit_once('/')?.0, url_segment(key)))
        })
        .unwrap_or_else(|| PAGE_PATH.to_string())
}

/// The link to `iri` with `here` as the query (the root and the filters, [`Harness::here`]) and
/// no face, so a browser is answered its page (its `Accept` names `text/html`) and any other
/// client the resource's own default.
fn page_href(iri: &str, here: &str) -> String {
    format!("{}?{here}", route_path(iri))
}

/// The link to `iri`'s `face`: its route, with the root and the face as the query.
fn k_href(iri: &str, root: &str, face: &str) -> String {
    format!(
        "{}?root={}&as={}",
        route_path(iri),
        url_segment(root),
        url_segment(face)
    )
}

/// The resource of the control, guide or incident `iri` names: `urn:iki:harness:{kind}:id:{id}`.
fn thing_iri(kind: Kind, iri: &str) -> String {
    format!("urn:iki:harness:{}:id:{}", kind.word(), local(iri))
}

fn local(iri: &str) -> &str {
    iri.rsplit([':', '/', '#']).next().unwrap_or(iri)
}

/// A vocabulary term's word: `toolCall` → `tool-call`, as the vocabulary labels it.
fn term_word(iri: &str) -> String {
    match iri.strip_prefix(H).unwrap_or(iri) {
        "toolCall" => "tool-call".into(),
        "preCommit" => "pre-commit".into(),
        "postCommit" => "post-commit".into(),
        "ci" => "CI".into(),
        "dailyDrift" => "daily drift".into(),
        "architectureFitness" => "architecture fitness".into(),
        "authoritySafety" => "authority/safety".into(),
        other => other.to_string(),
    }
}

/// The order stages come in a change's life.
const STAGES: [&str; 8] = [
    "toolCall",
    "preCommit",
    "postCommit",
    "ci",
    "merge",
    "dailyDrift",
    "periodic",
    "runtime",
];

fn stage_rank(iri: &str) -> usize {
    let l = iri.strip_prefix(H).unwrap_or(iri);
    STAGES.iter().position(|s| *s == l).unwrap_or(STAGES.len())
}

type Props = BTreeMap<String, Vec<Term>>;

fn vals(props: &Props, local: &str) -> Vec<String> {
    props
        .get(&format!("{H}{local}"))
        .map(|v| v.iter().map(value).collect())
        .unwrap_or_default()
}

fn label_of(all: &BTreeMap<String, Props>, iri: &str) -> String {
    all.get(iri)
        .and_then(|p| p.get(RDFS_LABEL))
        .and_then(|l| l.first())
        .map(value)
        .unwrap_or_else(|| local(iri).to_string())
}

fn link(href: &str, text: &str) -> String {
    format!("<a href=\"{}\">{}</a>", attr(href), html_text(text))
}

/// `1 firing`, `2 firings`: a count as words.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn is_a(props: &Props, class: &str) -> bool {
    props
        .get(RDF_TYPE)
        .is_some_and(|t| t.iter().any(|t| value(t) == format!("{H}{class}")))
}

/// What a control is, by its direction: a sensor checks after the act, a guide steers before it.
fn direction_word(props: Option<&Props>) -> &'static str {
    let directions = props.map(|p| vals(p, "direction")).unwrap_or_default();
    if directions.iter().any(|d| d == &format!("{H}feedback")) {
        "sensor"
    } else if directions.iter().any(|d| d == &format!("{H}feedforward")) {
        "guide"
    } else {
        "control"
    }
}

/// The words of every value of `local`, joined, or `None` when it has none.
fn words(props: &Props, local: &str) -> Option<String> {
    let mut values = vals(props, local);
    if local == "stage" {
        values.sort_by_key(|s| stage_rank(s));
    }
    let words: Vec<String> = values.iter().map(|v| term_word(v)).collect();
    (!words.is_empty()).then(|| words.join(", "))
}

/// What a control does when it fires, in words.
fn outcome_word(props: &Props) -> Option<String> {
    let words: Vec<&str> = vals(props, "outcome")
        .iter()
        .map(|o| match o.strip_prefix(H).unwrap_or(o) {
            "refuse" => "refuses the action",
            "warn" => "warns and lets the action through",
            _ => "fires",
        })
        .collect();
    (!words.is_empty()).then(|| words.join(", "))
}

/// A list of links, each an item, or a sentence for none.
fn link_list(items: &[(String, String)], none: &str) -> String {
    if items.is_empty() {
        return format!("<p>{}</p>", html_text(none));
    }
    let mut out = String::from("<ul class=\"harness-links\">");
    for (href, text) in items {
        let _ = write!(out, "<li>{}</li>", link(href, text));
    }
    out.push_str("</ul>");
    out
}

/// The facts of a description list, `(term, definition as HTML)`, the item page's shape.
fn facts(rows: &[(&str, String)]) -> String {
    let mut out = String::from("<dl class=\"meta harness-meta\">");
    for (term, definition) in rows {
        let _ = write!(out, "<dt>{}</dt><dd>{definition}</dd>", html_text(term));
    }
    out.push_str("</dl>");
    out
}

fn code_list(items: &[String]) -> String {
    items
        .iter()
        .map(|t| format!("<code>{}</code>", html_text(t)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `2 refusals, 1 warning`, or `none`.
fn firing_words(refused: usize, warned: usize) -> String {
    let mut parts = Vec::new();
    if refused > 0 {
        parts.push(plural(refused, "refusal", "refusals"));
    }
    if warned > 0 {
        parts.push(plural(warned, "warning", "warnings"));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// The link back to the Harness page of the root, with the filters this page came from.
fn back(harness: &Harness) -> String {
    let words = if harness.filters.is_empty() {
        format!("Back to the harness of \"{}\"", harness.root)
    } else {
        format!("Back to the filtered harness of \"{}\"", harness.root)
    };
    format!(
        "<p class=\"harness-back\">{}</p>",
        link(&page_href(PAGE_IRI, &harness.here()), &words)
    )
}

/// The resource's other faces, each a link: never the default, only on offer.
fn other_formats(iri: &str, root: &str, faces: &[(&str, &str)]) -> String {
    let links: Vec<String> = faces
        .iter()
        .map(|(face, name)| link(&k_href(iri, root, face), name))
        .collect();
    format!(
        "<p class=\"note harness-formats\">Other formats: {}</p>",
        links.join(", ")
    )
}
/// The proof a sensor can fire, in words.
fn proof(props: &Props) -> String {
    let tests = vals(props, "firedBy");
    if !tests.is_empty() {
        return format!(
            "fired by a test: {}",
            tests
                .iter()
                .map(|t| local(t).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(why) = vals(props, "noFiringTest").first() {
        return format!("cannot fire in a test: {why}");
    }
    let examples = vals(props, "example").len();
    if examples > 0 {
        return format!("{examples} example inputs");
    }
    "none stated".to_string()
}

/// How many refusals of the last week have a session whose summary names an item, and how many
/// items, read from the session summaries of the ledgers this caller may read.
async fn joined(web: &Web, inv: &Invocation<'_>, harness: &Harness) -> Option<String> {
    let now = crate::agents::now_ms(inv).ok()? as i64;
    let since = now - WEEK_MS;
    let recent: Vec<&Firing> = harness
        .firings
        .iter()
        .filter(|f| f.class == "Refusal")
        .filter(|f| stamp_ms(&f.at).is_some_and(|at| at >= since))
        .collect();
    if recent.is_empty() {
        return Some("In the last 7 days the harness refused no action.".to_string());
    }
    let sessions: BTreeSet<&str> = recent
        .iter()
        .filter_map(|f| f.session.as_deref())
        .filter_map(|s| s.strip_prefix("urn:agents:session:"))
        .collect();
    let mut items: BTreeMap<String, String> = BTreeMap::new();
    // ★ Summaries live in each ledger's SESSION graph since `ikigai-ledger` 0.23.0 (ledger
    // #3661), never in the ledger graph. A grant minted before then carries the ledger's read and
    // not that graph's, and one graph the caller cannot read refuses the whole query, so only the
    // session graphs this caller holds the token for are asked.
    let graphs: Vec<String> = web::readable_ledgers(web, inv.capability)
        .iter()
        .map(|l| l.session_graph())
        .filter(|g| inv.capability.allows(&ikigai_store::cap_read_graph(g)))
        .collect();
    if !sessions.is_empty() && !graphs.is_empty() {
        let query = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
                     SELECT ?id ?item WHERE { ?s a ledger:SessionSummary ; \
                     ledger:sessionId ?id ; ledger:sessionItem ?item }";
        let read = with(
            request(Verb::Source, "urn:sparql:select").ok()?,
            "query",
            query,
        );
        let read = with(read, "as", JSON);
        if let Ok(answer) = inv.issue(with(read, "graph", &graphs.join(" "))).await {
            let doc: Value = serde_json::from_slice(&answer.bytes).unwrap_or(Value::Null);
            for b in doc
                .pointer("/results/bindings")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let (Some(id), Some(item)) = (
                    b.pointer("/id/value").and_then(Value::as_str),
                    b.pointer("/item/value").and_then(Value::as_str),
                ) {
                    if sessions.contains(id) {
                        items.insert(id.to_string(), item.to_string());
                    }
                }
            }
        }
    }
    let on_items: BTreeSet<&String> = items.values().collect();
    let matched = recent
        .iter()
        .filter(|f| {
            f.session
                .as_deref()
                .and_then(|s| s.strip_prefix("urn:agents:session:"))
                .is_some_and(|s| items.contains_key(s))
        })
        .count();
    let mut sentence = format!(
        "In the last 7 days the harness refused {}",
        plural(recent.len(), "action", "actions")
    );
    if on_items.is_empty() {
        sentence.push_str(", none of them in a session a ledger you may read has a summary of.");
    } else {
        let _ = write!(
            sentence,
            ", {} of them on {} you may read.",
            matched,
            plural(on_items.len(), "ledger item", "ledger items")
        );
    }
    Some(sentence)
}

/// Each control that `p` points at `iri` from, in label order: `(control IRI, its props)`.
fn citing<'a>(
    all: &'a BTreeMap<String, Props>,
    p: &str,
    iri: &str,
) -> Vec<(&'a String, &'a Props)> {
    let mut out: Vec<(&String, &Props)> = all
        .iter()
        .filter(|(_, props)| is_a(props, "Control") && vals(props, p).iter().any(|o| o == iri))
        .collect();
    out.sort_by_key(|(c, props)| {
        (
            vals(props, "stage").iter().map(|s| stage_rank(s)).min(),
            label_of(all, c),
        )
    });
    out
}

/// One control as a list item: its name, linked, then a line of what it is and how often it fired.
fn control_item(
    harness: &Harness,
    all: &BTreeMap<String, Props>,
    here: &str,
    fired: &BTreeMap<&str, usize>,
    iri: &str,
) -> String {
    let props = &all[iri];
    let href = page_href(&thing_iri(Kind::Control, iri), here);
    let mut line: Vec<String> = vec![html_text(direction_word(Some(props)))];
    if let Some(execution) = words(props, "execution") {
        line.push(html_text(&execution));
    }
    if let Some(stage) = words(props, "stage") {
        line.push(html_text(&format!("at {stage}")));
    }
    match vals(props, "outcome")
        .first()
        .map(|o| o.strip_prefix(H).unwrap_or(o).to_string())
        .as_deref()
    {
        Some("refuse") => line.push("refuses".to_string()),
        Some("warn") => line.push("warns".to_string()),
        _ => {}
    }
    line.push(match fired.get(iri).copied().unwrap_or(0) {
        0 => "never fired".to_string(),
        n => link(
            &format!("{href}#harness-firings"),
            &plural(n, "firing", "firings"),
        ),
    });
    let incidents: Vec<String> = vals(props, "existsFor")
        .iter()
        .map(|i| {
            link(
                &page_href(&thing_iri(Kind::Incident, i), here),
                &label_of(all, i),
            )
        })
        .collect();
    let incidents = if incidents.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"harness-facts\">Exists for {}</p>",
            incidents.join(", ")
        )
    };
    let from = from_pack(harness, iri)
        .map(|f| format!("<p class=\"harness-facts\">{}</p>", html_text(&f)))
        .unwrap_or_default();
    format!(
        "<li>{}<p class=\"harness-facts\">{}</p>{incidents}{from}</li>",
        link(&href, &label_of(all, iri)),
        line.join(" · ")
    )
}

/// Where a guide is stated, in words: `AGENTS.md, section "1. Claim …"`.
fn stated_at(props: &Props) -> Option<String> {
    let file = vals(props, "statedIn").into_iter().next()?;
    Some(match vals(props, "section").first() {
        Some(section) => format!("{file}, section \u{201c}{section}\u{201d}"),
        None => file,
    })
}

/// The 1-based line of `section`'s heading in `text`: a Markdown heading whose words are the
/// section's, else the first line that holds them.
fn section_line(text: &str, section: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .position(|l| l.starts_with('#') && l.trim_start_matches('#').trim() == section)
        .or_else(|| lines.iter().position(|l| l.contains(section)))
        .map(|at| at + 1)
}

/// "From the pack “…” 1.0, which applies to roots that …": where a rule came from, or `None` for
/// the root's own.
fn from_pack(harness: &Harness, iri: &str) -> Option<String> {
    let (pack, words) = harness.origin(iri)?;
    let mut out = format!("From the pack \u{201c}{}\u{201d}", pack.label);
    if let Some(version) = &pack.version {
        let _ = write!(out, " {version}");
    }
    match (words, &pack.how) {
        (Some(words), _) => {
            let _ = write!(out, ", which applies to roots that {words}");
        }
        (None, crate::packs::How::Adopted) => out.push_str(", adopted by this repository"),
        (None, _) => {}
    }
    Some(out)
}

/// The packs loaded for this root, one line each with its firings outside any root, and what
/// could not be loaded, in words. Empty when no pack is configured.
fn packs_html(harness: &Harness) -> String {
    if harness.packs.is_empty() && harness.notes.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<h2 id=\"harness-packs\">Packs</h2><p>A pack is a harness that applies to many \
         repositories: by the shape of their description, or because a repository adopts it.</p>",
    );
    if harness.packs.is_empty() {
        out.push_str("<p>No pack applies to this repository.</p>");
    } else {
        out.push_str("<ul class=\"harness-list\">");
        for pack in &harness.packs {
            let mut name = format!("\u{201c}{}\u{201d}", pack.label);
            if let Some(version) = &pack.version {
                let _ = write!(name, " {version}");
            }
            let how = match &pack.how {
                crate::packs::How::Shape(Some(words)) => format!("applies to roots that {words}"),
                crate::packs::How::Shape(None) => "applies to this repository by its shape".into(),
                crate::packs::How::Adopted => "adopted by this repository".into(),
            };
            let _ = write!(
                out,
                "<li>{}<p class=\"harness-facts\">{} · {} outside any repository you may \
                 read</p></li>",
                html_text(&name),
                html_text(&how),
                html_text(&plural(pack.outside, "firing", "firings")),
            );
        }
        out.push_str("</ul>");
    }
    if !harness.notes.is_empty() {
        out.push_str("<ul class=\"harness-links harness-notes\">");
        for note in &harness.notes {
            let _ = write!(out, "<li>{}</li>", html_text(note));
        }
        out.push_str("</ul>");
    }
    out
}

/// The page's body, as HTML, and the sentence for #status when it is filtered: the join, the
/// repository picker and the filters, the coverage, the guides with the controls that check
/// them, and the days, each of the last three over the rules the filters show (ledger #3787).
async fn body(
    web: &Web,
    packs: &crate::packs::Config,
    inv: &Invocation<'_>,
    harness: &Harness,
) -> Result<(String, String)> {
    let here = harness.here();
    let all = harness.described()?;
    let sel = filter::Selection::new(harness, &all)?;
    let filtered = sel.filtered();
    let mut out = String::new();
    if let Some(sentence) = joined(web, inv, harness).await {
        let _ = write!(
            out,
            "<p class=\"harness-summary\">{}</p>",
            html_text(&sentence)
        );
    }

    let roots = filter::roots(web, packs, inv, &harness.root).await;
    out.push_str(&sel.form_html(harness, &roots));

    out.push_str(&packs_html(harness));

    // Coverage: each answer its question's row count, in words, linked to the answer's page.
    let _ = write!(
        out,
        "<h2 id=\"harness-coverage\">Coverage</h2><div class=\"table-wrap\"><table \
         class=\"harness-table\"><caption>Each coverage question, and how many answers it \
         has{}</caption><thead><tr><th scope=\"col\">Question</th><th \
         scope=\"col\">Answers</th></tr></thead><tbody>",
        if filtered {
            " among the filtered rules"
        } else {
            ""
        }
    );
    for q in &QUESTIONS {
        let n = sel.restrict(harness, q, harness.select(q.text)?).rows.len();
        let iri = format!("urn:iki:harness:coverage:query:{}", q.name);
        let whole = if sel.whole(q) {
            " (incidents are not rules: not filtered)"
        } else {
            ""
        };
        let _ = write!(
            out,
            "<tr><th scope=\"row\">{}{}</th><td>{}</td></tr>",
            html_text(q.heading),
            whole,
            link(&page_href(&iri, &here), &plural(n, q.rows[0], q.rows[1])),
        );
    }
    out.push_str("</tbody></table></div>");

    // The guides, each with the controls that state it; then the controls no guide states.
    // Filtered, a guide is drawn when it is shown or a control under it is.
    let mut fired: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &harness.firings {
        *fired.entry(f.control.as_str()).or_default() += 1;
    }
    let mut guides: Vec<&String> = all
        .iter()
        .filter(|(_, p)| is_a(p, "Guide"))
        .map(|(g, _)| g)
        .filter(|g| {
            sel.shows(g)
                || citing(&all, "statedBy", g)
                    .iter()
                    .any(|(c, _)| sel.shows(c))
        })
        .collect();
    guides.sort_by_key(|g| label_of(&all, g));
    let _ = write!(
        out,
        "<h2 id=\"harness-guides\">Guides and the controls that check them</h2><p>A guide is a \
         rule the agents are told; under each are the controls that state it.{}</p>",
        if filtered {
            " Only the filtered rules are listed."
        } else {
            ""
        }
    );
    if guides.is_empty() {
        out.push_str(if filtered {
            "<p>No guide is in the filtered rules.</p>"
        } else {
            "<p>The harness states no guide.</p>"
        });
    }
    let reasons_asked = sel
        .filters
        .values(filter::Facet::Gap)
        .iter()
        .any(|g| g == "unchecked-guide");
    for guide in &guides {
        let props = &all[guide.as_str()];
        let gid = format!("harness-guide-{}", local(guide));
        let _ =
            write!(
            out,
            "<section class=\"harness-group\" aria-labelledby=\"{gid}\"><h3 id=\"{gid}\">{}</h3>",
            link(&page_href(&thing_iri(Kind::Guide, guide), &here), &label_of(&all, guide)),
        );
        if !sel.shows(guide) {
            out.push_str(
                "<p class=\"harness-facts\">This guide is not in the filtered rules; it is shown \
                 for the controls under it that are.</p>",
            );
        }
        if let Some(at) = stated_at(props) {
            let _ = write!(
                out,
                "<p class=\"harness-facts\">Stated in {}</p>",
                html_text(&at)
            );
        }
        if let Some(from) = from_pack(harness, guide) {
            let _ = write!(out, "<p class=\"harness-facts\">{}</p>", html_text(&from));
        }
        let every = citing(&all, "statedBy", guide);
        let reason = unchecked_because(props);
        match &reason {
            Some(reason) => {
                let _ = write!(out, "<p>Unchecked because: {}</p>", html_text(reason));
            }
            None if every.is_empty() => out.push_str("<p>No control checks this guide.</p>"),
            None => {}
        }
        // In the view of the unchecked guides, a gap nobody explained says so.
        if reason.is_none() && reasons_asked && sel.unchecked(guide) {
            out.push_str(
                "<p class=\"harness-facts\">No reason is stated for leaving it unchecked.</p>",
            );
        }
        let controls: Vec<&String> = every
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| sel.shows(c))
            .collect();
        if controls.is_empty() && !every.is_empty() {
            let _ = write!(
                out,
                "<p>No control in the filtered rules checks this guide; {} outside them {}.</p>",
                plural(every.len(), "control", "controls"),
                if every.len() == 1 { "does" } else { "do" }
            );
        } else if !controls.is_empty() {
            out.push_str("<ul class=\"harness-list\">");
            for c in controls {
                out.push_str(&control_item(harness, &all, &here, &fired, c));
            }
            out.push_str("</ul>");
        }
        out.push_str("</section>");
    }
    let mut unstated: Vec<&String> = all
        .iter()
        .filter(|(c, p)| {
            is_a(p, "Control")
                && sel.shows(c)
                && !vals(p, "statedBy")
                    .iter()
                    .any(|g| all.get(g).is_some_and(|gp| is_a(gp, "Guide")))
        })
        .map(|(c, _)| c)
        .collect();
    unstated.sort_by_key(|c| {
        (
            vals(&all[c.as_str()], "stage")
                .iter()
                .map(|s| stage_rank(s))
                .min(),
            label_of(&all, c),
        )
    });
    if !unstated.is_empty() {
        out.push_str(
            "<section class=\"harness-group\" aria-labelledby=\"harness-unstated\"><h3 \
             id=\"harness-unstated\">Controls no guide states</h3><ul class=\"harness-list\">",
        );
        for c in unstated {
            out.push_str(&control_item(harness, &all, &here, &fired, c));
        }
        out.push_str("</ul></section>");
    }

    // Firings per day, newest first.
    let by_day = question("firings-by-day")?;
    let days = sel.restrict(harness, by_day, harness.select(by_day.text)?);
    let mut per_day: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for row in &days.rows {
        let (Some(day), Some(outcome)) = (
            cell(row, &days.vars, "day"),
            cell(row, &days.vars, "outcome"),
        ) else {
            continue;
        };
        let n: usize = cell(row, &days.vars, "firings")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let entry = per_day.entry(day).or_default();
        if outcome == "Refusal" {
            entry.0 += n;
        } else {
            entry.1 += n;
        }
    }
    out.push_str("<h2 id=\"harness-days\">Firings by day</h2>");
    if per_day.is_empty() {
        out.push_str(if filtered {
            "<p>No filtered rule has fired in this repository's firing log.</p>"
        } else {
            "<p>Nothing has fired in this repository's firing log.</p>"
        });
    } else {
        out.push_str(&days_table(
            &here,
            if filtered {
                "Refusals and warnings of the filtered rules per day (UTC), newest first"
            } else {
                "Refusals and warnings per day (UTC), newest first"
            },
            &per_day,
        ));
    }
    let status = if filtered {
        sel.summary()
    } else {
        String::new()
    };
    Ok((out, status))
}

/// Why a guide is left unchecked on purpose (`harness:uncheckedBecause`, vocabulary 0.5.0).
fn unchecked_because(props: &Props) -> Option<String> {
    vals(props, "uncheckedBecause")
        .into_iter()
        .next()
        .filter(|r| !r.trim().is_empty())
}

/// A table of days, each a row whose count links to that day's page.
fn days_table(here: &str, caption: &str, per_day: &BTreeMap<String, (usize, usize)>) -> String {
    let mut out = format!(
        "<div class=\"table-wrap\"><table class=\"harness-table\"><caption>{}</caption><thead><tr><th \
         scope=\"col\">Day (UTC)</th><th scope=\"col\">Firings</th></tr></thead><tbody>",
        html_text(caption)
    );
    for (day, (refused, warned)) in per_day.iter().rev() {
        let href = page_href(&format!("urn:iki:harness:firing:date:{day}"), here);
        let _ = write!(
            out,
            "<tr><th scope=\"row\">{}</th><td>{}</td></tr>",
            html_text(day),
            link(&href, &firing_words(*refused, *warned)),
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

/// A control's page: what it is, the guides and incidents it answers to, its proof and its days.
fn control_html(
    harness: &Harness,
    all: &BTreeMap<String, Props>,
    subject: &str,
) -> (String, String) {
    let here = harness.here();
    let empty = Props::new();
    let props = all.get(subject).unwrap_or(&empty);
    let none = || "<span class=\"note\">none stated</span>".to_string();
    let enforced = vals(props, "enforcedAt");
    let mut per_day: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for f in harness.firings.iter().filter(|f| f.control == subject) {
        let entry = per_day.entry(f.at[..10].to_string()).or_default();
        if f.class == "Refusal" {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    let total = per_day.values().map(|(r, w)| r + w).sum::<usize>();
    let mut out = facts(&[
        ("Direction", html_text(direction_word(Some(props)))),
        (
            "Execution",
            words(props, "execution")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "Lifecycle stage",
            words(props, "stage")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "Category",
            words(props, "category")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "When it fires",
            outcome_word(props)
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "Enforced at",
            if enforced.is_empty() {
                none()
            } else {
                code_list(&enforced)
            },
        ),
        ("Proof it can fire", html_text(&proof(props))),
        ("Fired", html_text(&plural(total, "time", "times"))),
    ]);
    let guides: Vec<(String, String)> = vals(props, "statedBy")
        .iter()
        .map(|g| {
            (
                page_href(&thing_iri(Kind::Guide, g), &here),
                label_of(all, g),
            )
        })
        .collect();
    let incidents: Vec<(String, String)> = vals(props, "existsFor")
        .iter()
        .map(|i| {
            (
                page_href(&thing_iri(Kind::Incident, i), &here),
                label_of(all, i),
            )
        })
        .collect();
    let _ = write!(
        out,
        "<h2 id=\"harness-guides\">Guides that state it</h2>{}\
         <h2 id=\"harness-incidents\">Incidents it exists for</h2>{}\
         <h2 id=\"harness-firings\">Firings by day</h2>",
        link_list(&guides, "No guide states this control."),
        link_list(&incidents, "No incident is named for this control."),
    );
    if per_day.is_empty() {
        out.push_str("<p>It has not fired in this repository's firing log.</p>");
    } else {
        out.push_str(&days_table(
            &here,
            "This control's refusals and warnings per day (UTC), newest first",
            &per_day,
        ));
    }
    (label_of(all, subject), out)
}

/// A guide's page: what it says, where it is stated, and the controls that state it.
async fn guide_html(
    inv: &Invocation<'_>,
    harness: &Harness,
    all: &BTreeMap<String, Props>,
    subject: &str,
) -> (String, String) {
    let root = harness.root.as_str();
    let here = harness.here();
    let empty = Props::new();
    let props = all.get(subject).unwrap_or(&empty);
    let says = props
        .get(RDFS_COMMENT)
        .and_then(|c| c.first())
        .map(value)
        .unwrap_or_else(|| label_of(all, subject));
    // Where it is stated, linked to browse's view of the file at the section's line when this
    // caller may read it there.
    let at = match (stated_at(props), vals(props, "statedIn").first()) {
        (Some(words), Some(file)) => {
            let target = format!("urn:repo:file:{root}/{file}");
            let href = match read_file(inv, root, file).await {
                Ok(Some(bytes)) => {
                    crate::web::repo_file_href(&target, inv.capability).map(|href| {
                        let line = vals(props, "section")
                            .first()
                            .and_then(|s| section_line(&String::from_utf8_lossy(&bytes), s));
                        match line {
                            Some(n) => format!("{href}#L{n}"),
                            None => href,
                        }
                    })
                }
                _ => None,
            };
            match href {
                Some(href) => link(&href, &words),
                None => html_text(&words),
            }
        }
        _ => "<span class=\"note\">not stated</span>".to_string(),
    };
    let none = || "<span class=\"note\">none stated</span>".to_string();
    let mut out = facts(&[
        ("What it says", html_text(&says)),
        ("Where it is stated", at),
        (
            "Execution",
            words(props, "execution")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "Lifecycle stage",
            words(props, "stage")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
        (
            "Category",
            words(props, "category")
                .map(|w| html_text(&w))
                .unwrap_or_else(none),
        ),
    ]);
    let controls: Vec<(String, String)> = citing(all, "statedBy", subject)
        .into_iter()
        .map(|(c, _)| {
            (
                page_href(&thing_iri(Kind::Control, c), &here),
                label_of(all, c),
            )
        })
        .collect();
    // A deliberate gap says why (`harness:uncheckedBecause`); without one the sentence stays.
    let reason = unchecked_because(props);
    let none = match &reason {
        Some(reason) => format!("Unchecked because: {reason}"),
        None => "No control checks this guide.".to_string(),
    };
    let _ = write!(
        out,
        "<h2 id=\"harness-controls\">Controls that enforce it</h2>{}",
        link_list(&controls, &none)
    );
    if let (Some(reason), false) = (&reason, controls.is_empty()) {
        let _ = write!(out, "<p>Unchecked because: {}</p>", html_text(reason));
    }
    (label_of(all, subject), out)
}

/// An incident's page: what happened, and the controls that exist for it.
fn incident_html(
    harness: &Harness,
    all: &BTreeMap<String, Props>,
    subject: &str,
) -> (String, String) {
    let here = harness.here();
    let empty = Props::new();
    let props = all.get(subject).unwrap_or(&empty);
    let mut rows = vec![("What happened", html_text(&label_of(all, subject)))];
    if let Some(comment) = props.get(RDFS_COMMENT).and_then(|c| c.first()) {
        rows.push(("More", html_text(&value(comment))));
    }
    let mut out = facts(&rows);
    let controls: Vec<(String, String)> = citing(all, "existsFor", subject)
        .into_iter()
        .map(|(c, _)| {
            (
                page_href(&thing_iri(Kind::Control, c), &here),
                label_of(all, c),
            )
        })
        .collect();
    let _ = write!(
        out,
        "<h2 id=\"harness-controls\">Controls that exist for it</h2>{}",
        link_list(
            &controls,
            "No control exists for this incident: the harness has learned from it only in prose."
        )
    );
    (label_of(all, subject), out)
}

/// A day's page: each control that fired, linked, its outcome and how many times.
fn day_html(harness: &Harness, date: &str, rows: &[(String, String, i64)]) -> Result<String> {
    let here = harness.here();
    let all = harness.described()?;
    let (mut refused, mut warned) = (0, 0);
    for (_, outcome, n) in rows {
        if outcome == "refuse" {
            refused += *n as usize;
        } else {
            warned += *n as usize;
        }
    }
    if rows.is_empty() {
        return Ok(format!(
            "<p>Nothing fired on {} (UTC).</p>",
            html_text(date)
        ));
    }
    let mut out = format!(
        "<p>On {} (UTC): {}.</p><div class=\"table-wrap\"><table class=\"harness-table\"><caption>The \
         controls that fired on {}</caption><thead><tr><th scope=\"col\">Control</th><th \
         scope=\"col\">Outcome</th><th scope=\"col\">Times</th></tr></thead><tbody>",
        html_text(date),
        html_text(&firing_words(refused, warned)),
        html_text(date)
    );
    for (control, outcome, n) in rows {
        let _ = write!(
            out,
            "<tr><th scope=\"row\">{}</th><td>{}</td><td>{}</td></tr>",
            link(
                &page_href(&thing_iri(Kind::Control, control), &here),
                &label_of(&all, control)
            ),
            match outcome.as_str() {
                "refuse" => "refused",
                "warn" => "warned",
                other => other,
            },
            html_text(&plural(*n as usize, "time", "times")),
        );
    }
    out.push_str("</tbody></table></div>");
    Ok(out)
}

/// A coverage question's page: its answer as a table, each control, guide or incident linked.
fn coverage_html(harness: &Harness, q: &Question, rows: &Rows) -> Result<String> {
    let here = harness.here();
    let all = harness.described()?;
    let mut out = format!(
        "<p>{}.</p>",
        html_text(&plural(rows.rows.len(), q.rows[0], q.rows[1]))
    );
    if rows.rows.is_empty() {
        return Ok(out);
    }
    let _ = write!(
        out,
        "<div class=\"table-wrap\"><table class=\"harness-table\"><caption>{}</caption><thead><tr>",
        html_text(q.heading)
    );
    for var in &rows.vars {
        let _ = write!(out, "<th scope=\"col\">{}</th>", html_text(var));
    }
    out.push_str("</tr></thead><tbody>");
    for row in &rows.rows {
        out.push_str("<tr>");
        for term in row {
            let Some(term) = term else {
                out.push_str("<td></td>");
                continue;
            };
            let text = value(term);
            let kind = all.get(&text).and_then(|p| {
                [Kind::Control, Kind::Guide, Kind::Incident]
                    .into_iter()
                    .find(|k| is_a(p, k.class()))
            });
            let html = match kind {
                Some(kind) => link(
                    &page_href(&thing_iri(kind, &text), &here),
                    &label_of(&all, &text),
                ),
                None => html_text(&term_word(&text)),
            };
            let _ = write!(out, "<td>{html}</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    Ok(out)
}

/// The slot a harness page's body goes into.
const BODY_SLOT: &str = "harness";

/// A harness page in gonk's chrome: the header, the theme, `title` as its `h1`, `message` under
/// it, and `html` spliced into the body. `None` draws the page without a body, and `extra` is
/// drawn before it (the sign-in notice).
fn harness_page(
    web: &Web,
    inv: &Invocation<'_>,
    title: &str,
    message: &str,
    html: Option<String>,
) -> Result<Representation> {
    page_with(web, inv, title, message, "", "", html)
}

fn page_with(
    web: &Web,
    inv: &Invocation<'_>,
    title: &str,
    message: &str,
    extra: &str,
    status: &str,
    html: Option<String>,
) -> Result<Representation> {
    let mut children = web::nav(
        web,
        inv.capability,
        &web::readable_ledgers(web, inv.capability),
        None,
    );
    children.push_str(extra);
    if html.is_some() {
        children.push_str(&render::slot(BODY_SLOT));
    }
    let doc = envelope(
        "page",
        &[
            ("view", "harness"),
            ("full", "true"),
            ("ui-theme", render::theme_of(inv)),
            (
                "safe",
                crate::page_assets::flag(crate::page_assets::safe_of(inv)),
            ),
            ("title", title),
            ("message", message),
            // What a filtered page shows, in #status, the page's one live region (ledger #3787).
            ("page-status", status),
        ],
        &children,
    );
    let shell = render::render(&doc, true).map_err(web::render_err)?;
    let page = render::splice(shell, &[(BODY_SLOT.to_string(), html.unwrap_or_default())])
        .map_err(web::render_err)?;
    Ok(web::html(page).with_expiry(ikigai_core::Expiry::Always))
}

/// `urn:iki:gonk:page:harness`, at `/harness`.
pub struct HarnessPage {
    pub web: Arc<Web>,
    /// The packs this server binds (ledger #3786); the default binds none.
    pub packs: Arc<crate::packs::Config>,
}

#[async_trait]
impl Endpoint for HarnessPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint("a page answers Source only".to_string()));
        }
        web::html_only(inv)?;
        match Harness::load_with(&self.web, &self.packs, inv).await {
            Ok(harness) => {
                let (html, status) = body(&self.web, &self.packs, inv, &harness).await?;
                let message = if harness.packs.is_empty() {
                    format!(
                        "The rules the agents in the repository \"{}\" follow: its \
                         .gonk/harness/harness.ttl and its firing log, read under your grant. \
                         Each guide, control, incident, question and day has a page of its own.",
                        harness.root
                    )
                } else {
                    format!(
                        "The rules the agents in the repository \"{}\" follow: its own harness, \
                         if it has one, the packs that apply to it, and their firings, read under \
                         your grant. Each guide, control, incident, question and day has a page of \
                         its own.",
                        harness.root
                    )
                };
                page_with(&self.web, inv, "Harness", &message, "", &status, Some(html))
            }
            Err(Error::Denied(_)) => page_with(
                &self.web,
                inv,
                "Harness",
                "",
                &web::Lacking::repositories().element(),
                "",
                None,
            ),
            Err(Error::NotFound(why)) => {
                harness_page(&self.web, inv, "Harness", &format!("{why}."), None)
            }
            // Two graphs that name one thing twice: the page is refused, in words.
            Err(Error::Endpoint(why)) if why.starts_with(COLLISION) => {
                harness_page(&self.web, inv, "Harness", &format!("{why}."), None)
            }
            Err(e) => Err(e),
        }
    }

    fn name(&self) -> &str {
        "gonk-harness-page"
    }

    fn describe(&self) -> Description {
        let [root, _] = args();
        filter::described(Description::new(self.name()), true)
            .title("Harness")
            .summary(
                "A repository's harness: the coverage questions' answers, each guide with the \
                 controls that state it, and the firings per day, each count a link to the page \
                 of the resource that answers it, over the rules the filters choose (all of them \
                 when none is given), each choice with how many rules it would match. Read under \
                 the caller's capability; counts only.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(root)
            .input(web::as_html_arg())
            .output(web::HTML)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEGMENT: &str = "# ikigai-log v1\n@prefix harness: <https://ikigai-rs.dev/ns/harness#> .\n\
        @prefix log:     <https://ikigai-rs.dev/ns/log#> .\n@name     urn:log:harness:tree-t1\n\
        @instance urn:iki:harness:tree:t1\n@level    log:info\n@started  2026-11-10T00:00:00.000Z\n\
        @prev     genesis\n# a comment\n\n\
        2026-11-10T01:00:00.000Z harness:Refusal urn:x:control:a seq=1 cmd=sha256:abc session=urn:agents:session:s1\n\
        2026-11-10T02:00:00.000Z harness:Warning urn:x:control:b seq=2 cmd=sha256:def\n\
        2026-11-10T03:00:00.000Z log:Note urn:x:control:b seq=3\n\
        2026-11-11T03:00:00.000Z harness:Warning urn:x:control:b seq=4 cmd=sha256:def\n";

    #[test]
    fn a_segment_reads_its_refusals_and_warnings_and_skips_the_rest() {
        let firings = parse_segment(SEGMENT).unwrap();
        assert_eq!(firings.len(), 3);
        assert_eq!(firings[0].node, "urn:log:harness:tree-t1:1");
        assert_eq!(firings[0].class, "Refusal");
        assert_eq!(firings[0].session.as_deref(), Some("urn:agents:session:s1"));
        assert!(parse_segment("not a log\n\n").is_err());
    }

    #[test]
    fn the_questions_parse_and_a_day_counts_per_control_and_outcome() {
        let turtle = b"@prefix harness: <https://ikigai-rs.dev/ns/harness#> .\n\
            <urn:x:control:b> a harness:Control ; harness:direction harness:feedback .\n";
        let h = Harness::from_bytes("r", turtle, Some(SEGMENT.as_bytes().to_vec())).unwrap();
        for q in &QUESTIONS {
            h.select(q.text)
                .unwrap_or_else(|e| panic!("{}: {e}", q.name));
        }
        let rows = day_rows(
            &h.construct(&FIRINGS_ON_DATE.replace("DAY", "2026-11-10"))
                .unwrap(),
        );
        assert_eq!(
            rows,
            vec![
                ("urn:x:control:a".into(), "refuse".into(), 1),
                ("urn:x:control:b".into(), "warn".into(), 1)
            ]
        );
        let repeated = h
            .select(question("repeated-warnings").unwrap().text)
            .unwrap();
        assert_eq!(repeated.rows.len(), 1);
        assert!(checked_date("2026-11-1x").is_err());
        assert_eq!(stamp_ms("1970-01-02T00:00:01.002Z"), Some(86_401_002));
        assert_eq!(
            stamp_ms("2026-11-15T00:00:00.000Z"),
            Some(1_794_700_800_000)
        );
        assert!(checked_id("Bad_id").is_err());
        assert!(checked_id("claim-before-write").is_ok());
    }
}
