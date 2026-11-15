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
}

/// The questions, in the page's order.
pub const QUESTIONS: [Question; 8] = [
    Question {
        name: "sensors-never-fired",
        heading: "Sensors never fired",
        text: include_str!("questions/harness/sensors-never-fired.rq"),
        gap: true,
    },
    Question {
        name: "guides-without-sensor",
        heading: "Guides without a check",
        text: include_str!("questions/harness/guides-without-sensor.rq"),
        gap: true,
    },
    Question {
        name: "sensors-without-guide",
        heading: "Sensors without a guide",
        text: include_str!("questions/harness/sensors-without-guide.rq"),
        gap: true,
    },
    Question {
        name: "incidents-without-sensor",
        heading: "Incidents without a sensor",
        text: include_str!("questions/harness/incidents-without-sensor.rq"),
        gap: true,
    },
    Question {
        name: "repeated-warnings",
        heading: "Repeated warnings",
        text: include_str!("questions/harness/repeated-warnings.rq"),
        gap: false,
    },
    Question {
        name: "top-recurring",
        heading: "Top recurring",
        text: include_str!("questions/harness/top-recurring.rq"),
        gap: false,
    },
    Question {
        name: "firings-by-day",
        heading: "Firings by day",
        text: include_str!("questions/harness/firings-by-day.rq"),
        gap: false,
    },
    Question {
        name: "firings-by-control",
        heading: "Firings by control",
        text: include_str!("questions/harness/firings-by-control.rq"),
        gap: false,
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
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("the harness store: {e}"))
}

/// `urn:repo:{root}:file:{path}`, read under the caller's capability. `Ok(None)` when the file
/// is not there.
async fn read_file(inv: &Invocation<'_>, root: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let iri = format!("urn:repo:{root}:file:{path}");
    match fetch(inv, request(Verb::Source, &iri)?).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(Error::NotFound(_)) | Err(Error::Unresolved(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

impl Harness {
    /// The harness `root=` names, or the first readable root that carries one.
    pub async fn load(web: &Web, inv: &Invocation<'_>) -> Result<Harness> {
        let asked = inv
            .inline_str("root")
            .ok()
            .map(str::trim)
            .filter(|r| !r.is_empty());
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
            return Err(Error::Denied(web::Lacking::repositories().plain(signed_in)));
        }
        for root in roots {
            if let Some(turtle) = read_file(inv, &root, GRAPH_FILE).await? {
                return Harness::from_bytes(
                    &root,
                    &turtle,
                    read_file(inv, &root, FIRINGS_FILE).await?,
                );
            }
        }
        Err(Error::NotFound(format!(
            "no repository this grant may read carries a harness (`{GRAPH_FILE}`)"
        )))
    }

    /// A harness from its two files' bytes.
    pub fn from_bytes(root: &str, turtle: &[u8], segment: Option<Vec<u8>>) -> Result<Harness> {
        let store = Store::new().map_err(store_err)?;
        store
            .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), turtle)
            .map_err(|e| {
                Error::Endpoint(format!("`{GRAPH_FILE}` in `{root}` is not Turtle: {e}"))
            })?;
        let firings = match segment {
            Some(bytes) => parse_segment(&String::from_utf8_lossy(&bytes))
                .map_err(|e| Error::Endpoint(format!("`{FIRINGS_FILE}` in `{root}`: {e}")))?,
            None => Vec::new(),
        };
        for firing in &firings {
            for quad in firing_quads(firing) {
                store.insert(&quad).map_err(store_err)?;
            }
        }
        Ok(Harness {
            root: root.to_string(),
            store,
            firings,
        })
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

fn face(inv: &Invocation<'_>, default: &str) -> String {
    inv.inline_str("as")
        .ok()
        .map(|a| a.split(';').next().unwrap_or(a).trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| default.to_string())
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
            "The face: text/turtle, application/json or text/plain.",
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
    pub kind: Kind,
}

#[async_trait]
impl Endpoint for Thing {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = web::binding(inv, "id")?;
        let id = checked_id(&id)?;
        let asked = face(inv, TURTLE);
        if ![TURTLE, JSON, TEXT].contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &[TURTLE, JSON, TEXT]));
        }
        let harness = Harness::load(&self.web, inv).await?;
        let Some(subject) = harness.find(self.kind.class(), id)? else {
            return Err(Error::NotFound(format!(
                "the harness of `{}` has no {} `{id}`",
                harness.root,
                self.kind.word()
            )));
        };
        let described = harness.described()?;
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
        Description::new(self.name())
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
    }
}

// ------------------------------------------------------------------------------ coverage

/// `urn:iki:harness:coverage:query:{name}`.
pub struct Coverage {
    pub web: Arc<Web>,
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
        let asked = face(inv, JSON);
        let served = [JSON, TURTLE, TEXT, QUERY_TEXT];
        if !served.contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &served));
        }
        if asked == QUERY_TEXT {
            return Ok(answer(QUERY_TEXT, q.text.as_bytes().to_vec()));
        }
        let harness = Harness::load(&self.web, inv).await?;
        let rows = harness.select(q.text)?;
        let iri = format!("urn:iki:harness:coverage:query:{}", q.name);
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
        Description::new(self.name())
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
                    .one_of([JSON, TURTLE, TEXT, QUERY_TEXT])
                    .default_value(JSON)
                    .optional(),
            )
            .output(JSON)
            .output(TURTLE)
            .output(TEXT)
            .output(QUERY_TEXT)
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
        let asked = face(inv, TURTLE);
        if ![TURTLE, JSON, TEXT].contains(&asked.as_str()) {
            return Err(refuse_face(&asked, &[TURTLE, JSON, TEXT]));
        }
        let harness = Harness::load(&self.web, inv).await?;
        let triples = harness.construct(&FIRINGS_ON_DATE.replace("DAY", date))?;
        let rows = day_rows(&triples);
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
        Description::new(self.name())
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

/// The link to `iri`'s `face`: its route, with the root and the face as the query.
fn k_href(iri: &str, root: &str, face: &str) -> String {
    let path = ROUTES[1..]
        .iter()
        .find_map(|(path, template)| {
            let prefix = template.split('{').next()?;
            let key = iri.strip_prefix(prefix)?;
            Some(format!("{}/{}", path.rsplit_once('/')?.0, url_segment(key)))
        })
        .unwrap_or_else(|| PAGE_PATH.to_string());
    format!("{path}?root={}&as={}", url_segment(root), url_segment(face))
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

/// A list of links, or a dash for none.
fn cell_links(items: &[(String, String)]) -> String {
    if items.is_empty() {
        return "<span class=\"note\">none</span>".to_string();
    }
    items
        .iter()
        .map(|(href, text)| link(href, text))
        .collect::<Vec<_>>()
        .join(", ")
}

fn cell_text(items: &[String]) -> String {
    if items.is_empty() {
        return "<span class=\"note\">none</span>".to_string();
    }
    items
        .iter()
        .map(|t| format!("<code>{}</code>", html_text(t)))
        .collect::<Vec<_>>()
        .join(", ")
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
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
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

/// The page's body, as HTML: the join, the gaps, the days, the controls.
async fn body(web: &Web, inv: &Invocation<'_>, harness: &Harness) -> Result<String> {
    let root = harness.root.as_str();
    let all = harness.described()?;
    let mut out = String::new();
    if let Some(sentence) = joined(web, inv, harness).await {
        let _ = write!(
            out,
            "<p class=\"harness-summary\">{}</p>",
            html_text(&sentence)
        );
    }

    // Coverage: each number its question's row count, linked to the question's answer.
    out.push_str(
        "<h2 id=\"harness-coverage\">Coverage</h2><div class=\"table-wrap\"><table \
         aria-labelledby=\"harness-coverage\"><thead><tr><th scope=\"col\">Question</th><th \
         scope=\"col\">Rows</th><th scope=\"col\">Asks</th></tr></thead><tbody>",
    );
    for q in &QUESTIONS {
        let rows = harness.select(q.text)?;
        let iri = format!("urn:iki:harness:coverage:query:{}", q.name);
        let _ = write!(
            out,
            "<tr><th scope=\"row\">{}</th><td>{}</td><td>{} {}</td></tr>",
            html_text(q.heading),
            link(&k_href(&iri, root, TEXT), &rows.rows.len().to_string()),
            html_text(asks(q)),
            link(&k_href(&iri, root, QUERY_TEXT), "Query"),
        );
    }
    out.push_str("</tbody></table></div>");

    // Firings per day.
    let days = harness.select(question("firings-by-day")?.text)?;
    let mut per_day: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for row in &days.rows {
        let (Some(day), Some(outcome)) = (
            cell(row, &days.vars, "day"),
            cell(row, &days.vars, "outcome"),
        ) else {
            continue;
        };
        let n: i64 = cell(row, &days.vars, "firings")
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
        out.push_str("<p>Nothing has fired in this repository's firing log.</p>");
    } else {
        out.push_str(
            "<div class=\"table-wrap\"><table aria-labelledby=\"harness-days\"><thead><tr><th \
             scope=\"col\">Day (UTC)</th><th scope=\"col\">Refused</th><th \
             scope=\"col\">Warned</th></tr></thead><tbody>",
        );
        for (day, (refused, warned)) in per_day.iter().rev() {
            let href = k_href(&format!("urn:iki:harness:firing:date:{day}"), root, TEXT);
            let _ = write!(
                out,
                "<tr><th scope=\"row\">{}</th><td>{}</td><td>{}</td></tr>",
                html_text(day),
                link(&href, &refused.to_string()),
                link(&href, &warned.to_string()),
            );
        }
        out.push_str("</tbody></table></div>");
    }

    // The controls, by stage and then by category.
    let mut fired: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &harness.firings {
        *fired.entry(f.control.as_str()).or_default() += 1;
    }
    let is = |props: &Props, class: &str| {
        props
            .get(RDF_TYPE)
            .is_some_and(|t| t.iter().any(|t| value(t) == format!("{H}{class}")))
    };
    let mut grouped: BTreeMap<(usize, String), BTreeMap<String, Vec<&String>>> = BTreeMap::new();
    for (iri, props) in &all {
        if !(is(props, "Control") || is(props, "Guide")) {
            continue;
        }
        let stages = vals(props, "stage");
        let categories = vals(props, "category");
        for stage in if stages.is_empty() {
            vec![String::new()]
        } else {
            stages
        } {
            for category in if categories.is_empty() {
                vec![String::new()]
            } else {
                categories.clone()
            } {
                grouped
                    .entry((stage_rank(&stage), stage.clone()))
                    .or_default()
                    .entry(term_word(&category))
                    .or_default()
                    .push(iri);
            }
        }
    }
    out.push_str("<h2 id=\"harness-controls\">Controls</h2>");
    if grouped.is_empty() {
        out.push_str("<p>The harness describes no control.</p>");
    }
    for ((_, stage), categories) in &grouped {
        let stage_word = if stage.is_empty() {
            "no stage stated".to_string()
        } else {
            term_word(stage)
        };
        let sid = format!("harness-stage-{}", local(stage).to_lowercase());
        let _ = write!(
            out,
            "<section aria-labelledby=\"{sid}\"><h3 id=\"{sid}\">Stage: {}</h3>",
            html_text(&stage_word)
        );
        for (category, controls) in categories {
            let category = if category.is_empty() {
                "no category stated"
            } else {
                category
            };
            let _ = write!(
                out,
                "<div class=\"table-wrap\"><table><caption>{}, at {}</caption><thead><tr><th \
                 scope=\"col\">Control</th><th scope=\"col\">Kind</th><th \
                 scope=\"col\">Guide</th><th scope=\"col\">Incident</th><th \
                 scope=\"col\">Proof</th><th scope=\"col\">Code</th><th \
                 scope=\"col\">Fired</th></tr></thead><tbody>",
                html_text(category),
                html_text(&stage_word)
            );
            for iri in controls {
                let props = &all[iri.as_str()];
                let guide = is(props, "Guide");
                let kind = if guide {
                    "guide"
                } else if vals(props, "direction")
                    .iter()
                    .any(|d| d == &format!("{H}feedback"))
                {
                    "sensor"
                } else {
                    "control"
                };
                let execution = vals(props, "execution")
                    .first()
                    .map(|e| format!(", {}", term_word(e)))
                    .unwrap_or_default();
                let own = if guide {
                    format!("urn:iki:harness:guide:id:{}", local(iri))
                } else {
                    format!("urn:iki:harness:control:id:{}", local(iri))
                };
                let guides: Vec<(String, String)> = vals(props, "statedBy")
                    .iter()
                    .map(|g| {
                        (
                            k_href(
                                &format!("urn:iki:harness:guide:id:{}", local(g)),
                                root,
                                TEXT,
                            ),
                            label_of(&all, g),
                        )
                    })
                    .collect();
                let incidents: Vec<(String, String)> = vals(props, "existsFor")
                    .iter()
                    .map(|i| {
                        (
                            k_href(
                                &format!("urn:iki:harness:incident:id:{}", local(i)),
                                root,
                                TEXT,
                            ),
                            label_of(&all, i),
                        )
                    })
                    .collect();
                let (proof, code, count) = if guide {
                    let section = vals(props, "section");
                    let file = vals(props, "statedIn");
                    let at: Vec<String> = file
                        .iter()
                        .map(|f| match section.first() {
                            Some(s) => format!("{f}: {s}"),
                            None => f.clone(),
                        })
                        .collect();
                    (
                        "<span class=\"note\">a guide</span>".to_string(),
                        cell_text(&at),
                        String::new(),
                    )
                } else {
                    let n = fired.get(iri.as_str()).copied().unwrap_or(0);
                    (
                        html_text(&proof(props)),
                        cell_text(&vals(props, "enforcedAt")),
                        link(&k_href(&own, root, TEXT), &n.to_string()),
                    )
                };
                let _ = write!(
                    out,
                    "<tr><th scope=\"row\">{}</th><td>{}{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    link(&k_href(&own, root, TEXT), &label_of(&all, iri)),
                    kind,
                    html_text(&execution),
                    if guide { "<span class=\"note\">itself</span>".to_string() } else { cell_links(&guides) },
                    cell_links(&incidents),
                    proof,
                    code,
                    if count.is_empty() { "<span class=\"note\">a guide does not fire</span>".to_string() } else { count },
                );
            }
            out.push_str("</tbody></table></div>");
        }
        out.push_str("</section>");
    }
    Ok(out)
}

/// The slot the page's body goes into.
const BODY_SLOT: &str = "harness";

/// `urn:iki:gonk:page:harness`, at `/harness`.
pub struct HarnessPage {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for HarnessPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint("a page answers Source only".to_string()));
        }
        web::html_only(inv)?;
        let mut children = web::nav(
            &self.web,
            inv.capability,
            &web::readable_ledgers(&self.web, inv.capability),
            None,
        );
        let (message, html) = match Harness::load(&self.web, inv).await {
            Ok(harness) => {
                let html = body(&self.web, inv, &harness).await?;
                children.push_str(&render::slot(BODY_SLOT));
                (
                    format!(
                        "The rules the agents in the repository \"{}\" follow: its \
                         .gonk/harness/harness.ttl and its firing log, read under your grant. \
                         Every number is a link to the question that answers it.",
                        harness.root
                    ),
                    html,
                )
            }
            Err(Error::Denied(_)) => {
                children.push_str(&web::Lacking::repositories().element());
                (String::new(), String::new())
            }
            Err(Error::NotFound(why)) => (format!("{why}."), String::new()),
            Err(e) => return Err(e),
        };
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
                ("title", "Harness"),
                ("message", &message),
            ],
            &children,
        );
        let shell = render::render(&doc, true).map_err(web::render_err)?;
        let page =
            render::splice(shell, &[(BODY_SLOT.to_string(), html)]).map_err(web::render_err)?;
        Ok(web::html(page).with_expiry(ikigai_core::Expiry::Always))
    }

    fn name(&self) -> &str {
        "gonk-harness-page"
    }

    fn describe(&self) -> Description {
        let [root, _] = args();
        Description::new(self.name())
            .title("Harness")
            .summary(
                "A repository's harness: its controls by stage and category, each with its guide, \
                 incident, proof and code site; the coverage questions' answers; and the firings \
                 per day and per control, each number a link to its resource. Read under the \
                 caller's capability; counts only.",
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
