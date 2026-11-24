//! Harness packs, applied to browse roots by shape or by explicit adoption, root descriptions
//! that shapes are checked against, and firings read from outside any repository (ledger
//! [#3786](http://localhost:1062/l/default/item/3786)).
//!
//! ```toml
//! gonk.browse.description = "~/.config/gonk/roots.ttl"
//! gonk.harness.pack = "iki ops:claude/harness/harness.ttl"
//! gonk.harness.firings = "iki ~/.local/state/iki/harness"
//! ```
//!
//! # A root's description
//!
//! `urn:repo:{root}:description` (Turtle, JSON), under `urn:cap:browse:read:{root}`, is what a
//! shape is checked against. It never holds a path. It has two parts:
//!
//! - **Derived facts**, computed from what gonk already has or reads through browse under the
//!   caller's capability: `browse:rootName`, `browse:githubOwner` and `browse:githubRepository`
//!   (from a `gonk.browse.github` line), and `browse:topLevelFile` for each of [`TOP_LEVEL`] the
//!   root holds at its top.
//! - **Declared facts**, which a person writes as RDF on `<urn:repo:{root}:description>`:
//!   `browse:team` (a `foaf:Group`), `browse:rootClass`, `browse:osTarget` (several allowed),
//!   `browse:targetType` and `browse:targetCustomer`, and any property the operator's file types
//!   `browse:DescriptionProperty` (single-valued when it is also an `owl:FunctionalProperty`).
//!   They come from the operator's file (`gonk.browse.description`, read once at start) and the
//!   repository's own [`ROOT_FILE`], read through browse, and are unioned. On a single-valued
//!   property the two disagree on, the operator's value is kept and the description says so
//!   (`browse:override`).
//!
//! A property the operator's file gives `browse:valuesFrom <scheme>` takes concepts of that SKOS
//! scheme (`skos:inScheme` or `skos:topConceptOf`) and nothing else. Since the subset validator
//! has no property paths, each declared concept's `skos:broader` ancestors are **materialized**
//! into the description as values of the same property (with the `skos:broader` pairs that
//! justify them), so a shape's `sh:hasValue os:unix` matches a root declared `os:linux`.
//!
//! The operator's file is checked at start, and refused in words: a value that names no concept
//! of its property's scheme, a description of a root gonk does not browse, and a violation of
//! the closed shape [`declared_shape`] builds (an unknown property, two values for a
//! single-valued one, a team that is not a `foaf:Group`).
//!
//! # A pack
//!
//! A harness graph that names itself: `<urn:iki:harness:pack:name:{pack}> a harness:Pack`, with
//! `rdfs:label`, `owl:versionInfo` and, usually, `harness:appliesTo <shape>`, a SHACL node shape
//! over root descriptions in the pack's own file. `gonk.harness.pack = "<pack> <root>:<path>"`
//! binds the name to a file read through `urn:repo:{root}:file:{path}` under the caller's
//! capability, so an edit to it shows on the next read. A pack applies to a root whose
//! description conforms to its shape (checked with `ikigai_shacl::core_shacl`, never served), or
//! whose own harness writes `<> harness:adopts <urn:iki:harness:pack:name:{pack}>`. A guide,
//! control or incident in a pack may narrow further with its own `harness:appliesTo`.
//!
//! # Firings from outside the tree
//!
//! `gonk.harness.firings = "<pack> <directory>"`: a directory of ikigai-log v1 segments
//! (`*.log`, read-only, read on every request since they grow). Only the directory's own regular
//! files are read: no subdirectory, no hidden name and no symbolic link, the rules a face's jail
//! keeps ([`crate::face`]). A firing counts on a root's page when its `repo=` names that root;
//! the rest (`repo=-`, a root this caller cannot read, a root gonk does not browse) count only on
//! the pack's own line.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Bindings, Description, Endpoint, Error, Grammar, Invocation, Iri, ReprType,
    Representation, Result, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use serde_json::json;

use crate::harness::{self, Firing, H};
use crate::web::{self, Web};

/// The browse vocabulary, where a root description's terms live.
pub const BROWSE: &str = "https://ikigai-rs.dev/ns/browse#";
pub const DESCRIPTION_KEY: &str = "gonk.browse.description";
pub const PACK_KEY: &str = "gonk.harness.pack";
pub const FIRINGS_KEY: &str = "gonk.harness.firings";
/// A repository's own declared facts, read through browse.
pub const ROOT_FILE: &str = ".gonk/root.ttl";
/// A pack's resource: `urn:iki:harness:pack:name:{pack}`.
pub const PACK_PREFIX: &str = "urn:iki:harness:pack:name:";
/// The top-level names a description says are present.
pub const TOP_LEVEL: [&str; 5] = [
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "ACCESSIBILITY.md",
    ".gonk/",
];

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const FOAF_GROUP: &str = "http://xmlns.com/foaf/0.1/Group";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const SH: &str = "http://www.w3.org/ns/shacl#";

/// The declared properties gonk knows, `(local name, words, single-valued)`.
const DECLARED: [(&str, &str, bool); 5] = [
    ("team", "owning team", true),
    ("rootClass", "class", true),
    ("osTarget", "OS target", false),
    ("targetType", "target type", true),
    ("targetCustomer", "target customer", true),
];

/// `urn:repo:{root}:description`.
pub fn description_iri(root: &str) -> String {
    format!("urn:repo:{root}:description")
}

// ------------------------------------------------------------------------------ the config

/// One `gonk.harness.pack` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSource {
    pub name: String,
    pub root: String,
    pub path: String,
}

/// The operator's declared facts and taxonomies (`gonk.browse.description`), checked at start.
#[derive(Debug, Default)]
pub struct Operator {
    triples: Vec<Triple>,
}

/// What the three keys configure. The default configures nothing, and every harness reads as
/// it did before packs.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub operator: Option<Arc<Operator>>,
    pub packs: Vec<PackSource>,
    /// `(pack, directory)` for each `gonk.harness.firings` line.
    pub firings: Vec<(String, PathBuf)>,
    /// `(root, https://github.com/{owner}/{repo})`, the `gonk.browse.github` lines.
    pub github: Vec<(String, String)>,
}

fn well_formed_name(name: &str) -> bool {
    name.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    }) && name.len() <= 64
}

impl Config {
    /// The three keys' lines, checked: every refusal names its line.
    ///
    /// `roots` are the browse roots, `github` the parsed `gonk.browse.github` lines.
    pub fn parse(
        descriptions: &[String],
        packs: &[String],
        firings: &[String],
        roots: &[(String, PathBuf)],
        github: &[(String, String)],
        home: &Path,
    ) -> std::result::Result<Config, String> {
        let names: Vec<String> = roots.iter().map(|(n, _)| n.clone()).collect();
        let operator = match descriptions {
            [] => None,
            [line] => {
                let path = crate::config::expand_home(line, home);
                let bytes = std::fs::read(&path).map_err(|e| {
                    format!("{DESCRIPTION_KEY} = \"{line}\": the file cannot be read: {e}")
                })?;
                let whence = format!("{DESCRIPTION_KEY} = \"{line}\"");
                Some(Arc::new(Operator::from_turtle(&bytes, &names, &whence)?))
            }
            lines => {
                return Err(format!(
                    "{DESCRIPTION_KEY} is given {} times: one operator's file describes the roots",
                    lines.len()
                ))
            }
        };
        let mut sources: Vec<PackSource> = Vec::new();
        for line in packs {
            let refuse = |why: &str| format!("{PACK_KEY} = \"{line}\": {why}");
            let words: Vec<&str> = line.split_whitespace().collect();
            let [name, source] = words.as_slice() else {
                return Err(refuse(
                    "is not `<pack> <root>:<path>` (for example \
                     `gonk.harness.pack = \"iki ops:claude/harness/harness.ttl\"`)",
                ));
            };
            if !well_formed_name(name) {
                return Err(refuse(&format!(
                    "`{name}` is not a pack name: lowercase letters, digits and `-`"
                )));
            }
            let Some((root, path)) = source.split_once(':') else {
                return Err(refuse("the source is not `<root>:<path>`"));
            };
            let Some((_, dir)) = roots.iter().find(|(n, _)| n == root) else {
                return Err(refuse(&format!(
                    "`{root}` is not a root this server browses (`gonk.browse.root`)"
                )));
            };
            let relative = Path::new(path);
            if path.is_empty()
                || relative.is_absolute()
                || relative
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(refuse(&format!(
                    "`{path}` is not a path inside the root: relative, with no `.` or `..`"
                )));
            }
            if !dir.join(relative).is_file() {
                return Err(refuse(&format!("the root `{root}` has no file `{path}`")));
            }
            if sources.iter().any(|s| s.name == *name) {
                return Err(refuse(&format!(
                    "the pack `{name}` is bound twice: one line per pack"
                )));
            }
            sources.push(PackSource {
                name: name.to_string(),
                root: root.to_string(),
                path: path.to_string(),
            });
        }
        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        for line in firings {
            let refuse = |why: &str| format!("{FIRINGS_KEY} = \"{line}\": {why}");
            let words: Vec<&str> = line.split_whitespace().collect();
            let [name, dir] = words.as_slice() else {
                return Err(refuse("is not `<pack> <directory>`"));
            };
            if !sources.iter().any(|s| s.name == *name) {
                return Err(refuse(&format!(
                    "no {PACK_KEY} line binds the pack `{name}`"
                )));
            }
            let dir = crate::config::expand_home(dir, home);
            if !dir.is_dir() {
                return Err(refuse("the directory is not there"));
            }
            if dirs.iter().any(|(n, _)| n == name) {
                return Err(refuse(&format!(
                    "the pack `{name}` is given two firing directories: one line per pack"
                )));
            }
            dirs.push((name.to_string(), dir));
        }
        Ok(Config {
            operator,
            packs: sources,
            firings: dirs,
            github: github.to_vec(),
        })
    }

    /// Whether anything is configured: with nothing, a harness reads exactly as before.
    pub fn is_empty(&self) -> bool {
        self.operator.is_none() && self.packs.is_empty()
    }
}

// ------------------------------------------------------------------------------ triples

fn turtle_triples(bytes: &[u8], base: &str) -> std::result::Result<Vec<Triple>, String> {
    let parser = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(base)
        .map_err(|e| e.to_string())?;
    parser
        .for_slice(bytes)
        .map(|q| q.map(Triple::from).map_err(|e| e.to_string()))
        .collect()
}

fn to_turtle(triples: &[Triple]) -> String {
    let mut out = RdfSerializer::from_format(RdfFormat::Turtle).for_writer(Vec::new());
    for t in triples {
        let _ = out.serialize_triple(t);
    }
    String::from_utf8(out.finish().unwrap_or_default()).unwrap_or_default()
}

fn iri_of(subject: &NamedOrBlankNode) -> Option<&str> {
    match subject {
        NamedOrBlankNode::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}

fn object_iri(term: &Term) -> Option<&str> {
    match term {
        Term::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}

fn has(triples: &[Triple], s: &str, p: &str, o: &str) -> bool {
    triples.iter().any(|t| {
        iri_of(&t.subject) == Some(s)
            && t.predicate.as_str() == p
            && object_iri(&t.object) == Some(o)
    })
}

fn objects<'a>(triples: &'a [Triple], s: &'a str, p: &'a str) -> impl Iterator<Item = &'a Term> {
    triples
        .iter()
        .filter(move |t| iri_of(&t.subject) == Some(s) && t.predicate.as_str() == p)
        .map(|t| &t.object)
}

fn first_literal(triples: &[Triple], s: &str, p: &str) -> Option<String> {
    objects(triples, s, p).find_map(|o| match o {
        Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    })
}

fn nn(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

fn short(iri: &str) -> String {
    for (ns, prefix) in [
        (BROWSE, "browse:"),
        (SKOS, "skos:"),
        (H, "harness:"),
        (RDFS_LABEL.trim_end_matches("label"), "rdfs:"),
    ] {
        if let Some(local) = iri.strip_prefix(ns) {
            return format!("{prefix}{local}");
        }
    }
    iri.to_string()
}

// ------------------------------------------------------------------------------ declared facts

impl Operator {
    /// The operator's file, checked as [`check_declared`] says, `whence` naming its line.
    pub fn from_turtle(
        bytes: &[u8],
        roots: &[String],
        whence: &str,
    ) -> std::result::Result<Operator, String> {
        let triples = turtle_triples(bytes, "urn:iki:gonk:operator:description")
            .map_err(|e| format!("{whence}: the file is not Turtle: {e}"))?;
        check_declared(&triples, &triples, roots, whence)?;
        Ok(Operator { triples })
    }

    /// The operator's triples whose subject is `iri`.
    fn facts_of(&self, iri: &str) -> Vec<Triple> {
        self.triples
            .iter()
            .filter(|t| iri_of(&t.subject) == Some(iri))
            .cloned()
            .collect()
    }
}

/// The declared properties `vocab` admits: `(IRI, single-valued)`.
fn properties(vocab: &[Triple]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = DECLARED
        .iter()
        .map(|(l, _, one)| (format!("{BROWSE}{l}"), *one))
        .collect();
    for t in vocab {
        if t.predicate.as_str() == RDF_TYPE
            && object_iri(&t.object) == Some(&format!("{BROWSE}DescriptionProperty"))
        {
            if let Some(p) = iri_of(&t.subject) {
                if !out.iter().any(|(q, _)| q == p) {
                    let one = has(vocab, p, RDF_TYPE, &format!("{OWL}FunctionalProperty"));
                    out.push((p.to_string(), one));
                }
            }
        }
    }
    out
}

fn words_of(p: &str) -> String {
    DECLARED
        .iter()
        .find(|(l, _, _)| p.strip_prefix(BROWSE) == Some(*l))
        .map(|(_, w, _)| w.to_string())
        .unwrap_or_else(|| short(p))
}

/// The closed SHACL shape the declared facts of `targets` are checked against: only the
/// properties gonk knows and the operator declares (and `rdf:type`, `rdfs:label`,
/// `rdfs:comment`), at most one value of a single-valued one, each value an IRI, and a team a
/// `foaf:Group`.
pub fn declared_shape(vocab: &[Triple], targets: &[String]) -> String {
    let mut out = format!(
        "@prefix sh: <{SH}> .\n<urn:iki:gonk:shape:root-description> a sh:NodeShape ;\n  \
         sh:closed true ;\n  sh:ignoredProperties ( <{RDF_TYPE}> <{RDFS_LABEL}> <{RDFS_COMMENT}> ) ;\n"
    );
    for t in targets {
        let _ = writeln!(out, "  sh:targetNode <{t}> ;");
    }
    for (p, one) in properties(vocab) {
        let _ = write!(out, "  sh:property [ sh:path <{p}> ; sh:nodeKind sh:IRI");
        if one {
            out.push_str(" ; sh:maxCount 1");
        }
        if p == format!("{BROWSE}team") {
            let _ = write!(out, " ; sh:class <{FOAF_GROUP}>");
        }
        out.push_str(" ] ;\n");
    }
    out.push_str("  .\n");
    out
}

/// A root named by a description IRI, `urn:repo:{root}:description`.
fn described_root(iri: &str) -> Option<&str> {
    iri.strip_prefix("urn:repo:")?
        .strip_suffix(":description")
        .filter(|r| !r.is_empty() && !r.contains(':'))
}

/// Check the declared facts in `facts`, against the schemes and properties `vocab` declares:
/// every described root is browsed, every value of a property with a `browse:valuesFrom` scheme
/// is a concept of it, and the closed shape [`declared_shape`] holds. `whence` names the source.
pub fn check_declared(
    facts: &[Triple],
    vocab: &[Triple],
    roots: &[String],
    whence: &str,
) -> std::result::Result<(), String> {
    let mut targets: BTreeSet<String> = BTreeSet::new();
    for t in facts {
        if let Some(root) = iri_of(&t.subject).and_then(described_root) {
            if !roots.iter().any(|r| r == root) {
                return Err(format!(
                    "{whence}: it describes `{root}` (<{}>), which is not a root this server \
                     browses",
                    description_iri(root)
                ));
            }
            targets.insert(description_iri(root));
        }
    }
    for target in &targets {
        for (p, _) in properties(vocab) {
            let values_from = format!("{BROWSE}valuesFrom");
            let schemes: Vec<&str> = objects(vocab, &p, &values_from)
                .filter_map(object_iri)
                .collect();
            if schemes.is_empty() {
                continue;
            }
            for value in objects(facts, target, &p) {
                let ok = object_iri(value).is_some_and(|v| {
                    schemes.iter().any(|s| {
                        has(vocab, v, &format!("{SKOS}inScheme"), s)
                            || has(vocab, v, &format!("{SKOS}topConceptOf"), s)
                    })
                });
                if !ok {
                    return Err(format!(
                        "{whence}: <{target}> gives {} the value {}, which names no concept of \
                         the scheme <{}>",
                        short(&p),
                        match value {
                            Term::NamedNode(n) => format!("<{}>", n.as_str()),
                            other => other.to_string(),
                        },
                        schemes.join("> or <")
                    ));
                }
            }
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    let targets: Vec<String> = targets.into_iter().collect();
    let shape = declared_shape(vocab, &targets);
    // The data is the facts and what they name (a team's `foaf:Group` type is in the vocabulary).
    // Not the vocabulary's own descriptions: a repository's file is checked on its own.
    let mut data: Vec<Triple> = facts.to_vec();
    data.extend(
        vocab
            .iter()
            .filter(|t| iri_of(&t.subject).and_then(described_root).is_none())
            .cloned(),
    );
    let report = ikigai_shacl::core_shacl::validate_report(&to_turtle(&data), &shape)
        .map_err(|e| format!("{whence}: the declared facts cannot be checked: {e}"))?;
    if report.conforms {
        return Ok(());
    }
    let sentences: Vec<String> = report
        .violations
        .iter()
        .map(|v| {
            let path = v.path.as_deref().map(short).unwrap_or_default();
            let value = v.value.as_ref().map(|t| t.lexical()).unwrap_or_default();
            let component = v.component.rsplit('#').next().unwrap_or("");
            match component {
                "ClosedConstraintComponent" => format!(
                    "<{}> gives {path}, which is not a property a root description takes \
                     (declare it `a browse:DescriptionProperty` in the operator's file)",
                    v.focus_node
                ),
                "MaxCountConstraintComponent" => format!(
                    "<{}> gives more than one {}; a root has one",
                    v.focus_node,
                    words_of(v.path.as_deref().unwrap_or(""))
                ),
                "ClassConstraintComponent" => format!(
                    "<{}> gives {path} the value <{value}>, which is not a foaf:Group",
                    v.focus_node
                ),
                "NodeKindConstraintComponent" => format!(
                    "<{}> gives {path} the value \"{value}\", which is not an IRI",
                    v.focus_node
                ),
                other => format!("<{}> breaks {other} on {path}", v.focus_node),
            }
        })
        .collect();
    Err(format!(
        "{whence}: the declared facts do not hold: {}",
        sentences.join("; ")
    ))
}

// ------------------------------------------------------------------------------ a description

/// A root's description, as the triples a shape is checked against.
#[derive(Debug, Clone)]
pub struct RootDescription {
    pub root: String,
    pub iri: String,
    pub triples: Vec<Triple>,
    /// What the description says about itself: an override, in words.
    pub notes: Vec<String>,
}

fn reads_root(inv: &Invocation<'_>, root: &str) -> bool {
    inv.capability
        .allows(&format!("{}{root}", ikigai_browse::CAP_PREFIX))
        || inv.capability.allows(ikigai_browse::CAP_WILDCARD)
}

fn lacking(root: &str) -> Error {
    Error::Denied(
        format!(
            "this grant does not read the repository `{root}` (`{}{root}`)",
            ikigai_browse::CAP_PREFIX
        )
        .into(),
    )
}

async fn present(inv: &Invocation<'_>, root: &str, name: &str) -> bool {
    let iri = match name.strip_suffix('/') {
        Some(dir) => format!("urn:repo:{root}:tree:{dir}"),
        None => format!("urn:repo:{root}:file:{name}"),
    };
    match web::request(Verb::Source, &iri) {
        Ok(req) => web::fetch(inv, req).await.is_ok(),
        Err(_) => false,
    }
}

/// `root`'s description, read under the caller's capability: refused, naming nothing about the
/// root, when the caller may not read it or gonk does not browse it.
pub async fn describe(
    web: &Web,
    cfg: &Config,
    inv: &Invocation<'_>,
    root: &str,
) -> Result<RootDescription> {
    if !reads_root(inv, root) || !web.browse_roots.names().iter().any(|r| r == root) {
        return Err(lacking(root));
    }
    let iri = description_iri(root);
    let me = nn(&iri);
    let triple = |p: &str, o: Term| Triple::new(me.clone(), nn(p), o);
    let mut triples = vec![
        triple(RDF_TYPE, nn(&format!("{BROWSE}RootDescription")).into()),
        triple(
            &format!("{BROWSE}rootName"),
            Literal::new_simple_literal(root).into(),
        ),
    ];
    if let Some((_, url)) = cfg.github.iter().find(|(r, _)| r == root) {
        let mut parts = url
            .trim_start_matches("https://github.com/")
            .splitn(2, '/')
            .map(str::to_string);
        if let (Some(owner), Some(repo)) = (parts.next(), parts.next()) {
            triples.push(triple(
                &format!("{BROWSE}githubOwner"),
                Literal::new_simple_literal(owner).into(),
            ));
            triples.push(triple(
                &format!("{BROWSE}githubRepository"),
                Literal::new_simple_literal(repo).into(),
            ));
        }
    }
    for name in TOP_LEVEL {
        if present(inv, root, name).await {
            triples.push(triple(
                &format!("{BROWSE}topLevelFile"),
                Literal::new_simple_literal(name).into(),
            ));
        }
    }
    // The declared facts: the operator's, then the repository's own where they do not collide.
    let empty = Operator::default();
    let operator = cfg.operator.as_deref().unwrap_or(&empty);
    let mut declared = operator.facts_of(&iri);
    let mut notes = Vec::new();
    if let Some(bytes) = harness::read_file(inv, root, ROOT_FILE).await? {
        let own = turtle_triples(&bytes, &iri).map_err(|e| {
            Error::Endpoint(format!("`{ROOT_FILE}` in `{root}` is not Turtle: {e}"))
        })?;
        let own: Vec<Triple> = own
            .into_iter()
            .filter(|t| iri_of(&t.subject) == Some(iri.as_str()))
            .collect();
        check_declared(
            &own,
            &operator.triples,
            &[root.to_string()],
            &format!("`{ROOT_FILE}` in `{root}`"),
        )
        .map_err(Error::Endpoint)?;
        let single: BTreeSet<String> = properties(&operator.triples)
            .into_iter()
            .filter(|(_, one)| *one)
            .map(|(p, _)| p)
            .collect();
        for t in own {
            let p = t.predicate.as_str().to_string();
            let theirs: Vec<&Term> = declared
                .iter()
                .filter(|d| d.predicate.as_str() == p)
                .map(|d| &d.object)
                .collect();
            if theirs.contains(&&t.object) {
                continue;
            }
            if single.contains(&p) && !theirs.is_empty() {
                let note = format!(
                    "The operator's file gives {} the value {}; the repository's {ROOT_FILE} gave \
                     {}, which is set aside.",
                    short(&p),
                    term_words(theirs[0]),
                    term_words(&t.object)
                );
                notes.push(note);
                continue;
            }
            declared.push(t);
        }
    }
    for note in &notes {
        triples.push(triple(
            &format!("{BROWSE}override"),
            Literal::new_simple_literal(note).into(),
        ));
    }
    // Each concept value's ancestors, materialized as values of the same property, with the
    // `skos:broader` pairs and the labels that say why.
    let broader = format!("{SKOS}broader");
    let mut extra: Vec<Triple> = Vec::new();
    for t in &declared {
        let Some(value) = object_iri(&t.object) else {
            continue;
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut todo = vec![value.to_string()];
        while let Some(concept) = todo.pop() {
            if !seen.insert(concept.clone()) {
                continue;
            }
            if let Some(label) =
                first_literal(&operator.triples, &concept, &format!("{SKOS}prefLabel"))
                    .or_else(|| first_literal(&operator.triples, &concept, RDFS_LABEL))
            {
                extra.push(Triple::new(
                    nn(&concept),
                    nn(&format!("{SKOS}prefLabel")),
                    Literal::new_simple_literal(label),
                ));
            }
            for up in objects(&operator.triples, &concept, &broader).filter_map(object_iri) {
                extra.push(Triple::new(nn(value), nn(&broader), nn(up)));
                extra.push(Triple::new(me.clone(), t.predicate.clone(), nn(up)));
                todo.push(up.to_string());
            }
        }
    }
    triples.extend(declared);
    for t in extra {
        if !triples.contains(&t) {
            triples.push(t);
        }
    }
    Ok(RootDescription {
        root: root.to_string(),
        iri,
        triples,
        notes,
    })
}

fn term_words(term: &Term) -> String {
    match term {
        Term::NamedNode(n) => format!("<{}>", n.as_str()),
        Term::Literal(l) => format!("\"{}\"", l.value()),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------------------ shapes

/// Whether `description` conforms to the node shape `shape` in `graph`: the shape and what it
/// reaches (its blank nodes, lists and the shapes it names), its own targets dropped, aimed at the
/// description alone. `Err` says, in words, why the subset validator refused it.
pub fn conforms(
    graph: &[Triple],
    shape: &str,
    description: &RootDescription,
) -> std::result::Result<bool, String> {
    let is_shape = |iri: &str| {
        has(graph, iri, RDF_TYPE, &format!("{SH}NodeShape"))
            || has(graph, iri, RDF_TYPE, &format!("{SH}PropertyShape"))
    };
    if !is_shape(shape) {
        return Err(format!(
            "<{shape}> is not a SHACL node shape (`a sh:NodeShape`) in the pack's file"
        ));
    }
    let mut seen: Vec<NamedOrBlankNode> = Vec::new();
    let mut todo: Vec<NamedOrBlankNode> = vec![nn(shape).into()];
    let mut picked: Vec<Triple> = Vec::new();
    while let Some(node) = todo.pop() {
        if seen.contains(&node) {
            continue;
        }
        seen.push(node.clone());
        for t in graph.iter().filter(|t| t.subject == node) {
            if t.predicate.as_str().starts_with(&format!("{SH}target")) {
                continue;
            }
            picked.push(t.clone());
            match &t.object {
                Term::BlankNode(b) => todo.push(b.clone().into()),
                Term::NamedNode(n) if is_shape(n.as_str()) => todo.push(n.clone().into()),
                _ => {}
            }
        }
    }
    picked.push(Triple::new(
        nn(shape),
        nn(&format!("{SH}targetNode")),
        nn(&description.iri),
    ));
    match ikigai_shacl::core_shacl::validate_report(
        &to_turtle(&description.triples),
        &to_turtle(&picked),
    ) {
        Ok(report) => Ok(report.conforms),
        Err(e) => Err(format!(
            "the shape <{shape}> is one the subset validator refuses: {e}"
        )),
    }
}

/// "applies to roots that …": the shape's `rdfs:label`, else its `sh:message`.
fn shape_words(graph: &[Triple], shape: &str) -> Option<String> {
    first_literal(graph, shape, RDFS_LABEL)
        .or_else(|| first_literal(graph, shape, &format!("{SH}message")))
}

// ------------------------------------------------------------------------------ packs

/// How a pack came to a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// By its shape, and the shape's words.
    Shape(Option<String>),
    /// The root's own harness adopts it.
    Adopted,
}

/// A pack that applies to a root, its rules narrowed to those that also apply.
#[derive(Debug, Clone)]
pub struct Applied {
    pub name: String,
    pub label: String,
    pub version: Option<String>,
    pub how: How,
    /// The pack's triples, less every rule whose own shape the root does not match.
    pub triples: Vec<Triple>,
    /// Per rule that came by its own narrower shape: that shape's words.
    pub narrowed: BTreeMap<String, Option<String>>,
    /// The pack's firings outside any root this caller may read.
    pub outside: usize,
}

/// What a root's page loads beside its own graph.
#[derive(Debug, Default)]
pub struct Assembly {
    pub packs: Vec<Applied>,
    pub firings: Vec<Firing>,
    /// What the page states in words: unbound adoption, a refused shape, an unreadable pack.
    pub notes: Vec<String>,
}

/// The packs `own` (the root's own `harness.ttl`, if any) adopts.
pub fn adopted(own: &[u8], root: &str) -> Vec<String> {
    let base = format!("urn:repo:{root}:file:{}", harness::GRAPH_FILE);
    turtle_triples(own, &base)
        .unwrap_or_default()
        .iter()
        .filter(|t| t.predicate.as_str() == format!("{H}adopts"))
        .filter_map(|t| {
            object_iri(&t.object)?
                .strip_prefix(PACK_PREFIX)
                .map(str::to_string)
        })
        .collect()
}

/// The packs that apply to `root`, their rules narrowed, and their firings for it.
pub async fn assemble(
    web: &Web,
    cfg: &Config,
    inv: &Invocation<'_>,
    root: &str,
    own: Option<&[u8]>,
) -> Result<Assembly> {
    let mut out = Assembly::default();
    if cfg.is_empty() {
        return Ok(out);
    }
    let adopts = own.map(|o| adopted(o, root)).unwrap_or_default();
    for name in &adopts {
        if !cfg.packs.iter().any(|p| &p.name == name) {
            out.notes.push(format!(
                "This repository's harness adopts the pack \u{201c}{name}\u{201d}, which no \
                 {PACK_KEY} line binds, so it is not loaded."
            ));
        }
    }
    let description = match describe(web, cfg, inv, root).await {
        Ok(d) => Some(d),
        Err(Error::Endpoint(why)) => {
            out.notes.push(format!(
                "This repository's description cannot be read, so no shape matches it: {why}."
            ));
            None
        }
        Err(e) => return Err(e),
    };
    let readable: BTreeSet<String> = crate::k::readable_roots(web, inv.capability)
        .into_iter()
        .collect();
    for source in &cfg.packs {
        let pack = PACK_PREFIX.to_string() + &source.name;
        let bytes = match harness::read_file(inv, &source.root, &source.path).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => {
                if adopts.contains(&source.name) {
                    out.notes.push(format!(
                        "The pack \u{201c}{}\u{201d} cannot be read under your grant.",
                        source.name
                    ));
                }
                continue;
            }
        };
        let graph = match turtle_triples(
            &bytes,
            &format!("urn:repo:{}:file:{}", source.root, source.path),
        ) {
            Ok(g) => g,
            Err(e) => {
                out.notes.push(format!(
                    "The pack \u{201c}{}\u{201d} is not Turtle: {e}.",
                    source.name
                ));
                continue;
            }
        };
        if !has(&graph, &pack, RDF_TYPE, &format!("{H}Pack")) {
            out.notes.push(format!(
                "The file bound to the pack \u{201c}{}\u{201d} does not name itself: it has no \
                 <{pack}> a harness:Pack.",
                source.name
            ));
            continue;
        }
        let label = first_literal(&graph, &pack, RDFS_LABEL).unwrap_or_else(|| source.name.clone());
        let version = first_literal(&graph, &pack, &format!("{OWL}versionInfo"));
        let shape = objects(&graph, &pack, &format!("{H}appliesTo"))
            .find_map(object_iri)
            .map(str::to_string);
        let how = if let (Some(shape), Some(d)) = (&shape, &description) {
            match conforms(&graph, shape, d) {
                Ok(true) => Some(How::Shape(shape_words(&graph, shape))),
                Ok(false) => None,
                Err(why) => {
                    out.notes.push(format!(
                        "The pack \u{201c}{label}\u{201d} cannot be matched against this \
                         repository: {why}."
                    ));
                    None
                }
            }
        } else {
            None
        };
        let how = match how {
            Some(how) => how,
            None if adopts.contains(&source.name) => How::Adopted,
            None => continue,
        };
        // Narrow: a rule with its own shape stays only if the root matches that shape too.
        let mut dropped: BTreeSet<String> = BTreeSet::new();
        let mut narrowed = BTreeMap::new();
        for t in &graph {
            let Some(rule) = iri_of(&t.subject) else {
                continue;
            };
            if t.predicate.as_str() != format!("{H}appliesTo") || rule == pack {
                continue;
            }
            let Some(rule_shape) = object_iri(&t.object) else {
                continue;
            };
            let keep = match &description {
                Some(d) => match conforms(&graph, rule_shape, d) {
                    Ok(keep) => keep,
                    Err(why) => {
                        out.notes.push(format!(
                            "A rule of the pack \u{201c}{label}\u{201d}, <{rule}>, is left out: \
                             {why}."
                        ));
                        false
                    }
                },
                None => false,
            };
            if keep {
                narrowed.insert(rule.to_string(), shape_words(&graph, rule_shape));
            } else {
                dropped.insert(rule.to_string());
            }
        }
        let triples: Vec<Triple> = graph
            .into_iter()
            .filter(|t| !iri_of(&t.subject).is_some_and(|s| dropped.contains(s)))
            .collect();
        let mut outside = 0;
        if let Some((_, dir)) = cfg.firings.iter().find(|(n, _)| n == &source.name) {
            let (firings, skipped) = read_firings(dir);
            if skipped > 0 {
                out.notes.push(format!(
                    "{} in the firings of the pack \u{201c}{label}\u{201d} {} not an ikigai-log v1 \
                     segment and {} skipped.",
                    if skipped == 1 { "1 file".to_string() } else { format!("{skipped} files") },
                    if skipped == 1 { "is" } else { "are" },
                    if skipped == 1 { "was" } else { "were" },
                ));
            }
            for f in firings {
                match f.repo.as_deref() {
                    Some(r) if r == root => out.firings.push(f),
                    Some(r) if readable.contains(r) => {}
                    _ => outside += 1,
                }
            }
        }
        out.packs.push(Applied {
            name: source.name.clone(),
            label,
            version,
            how,
            triples,
            narrowed,
            outside,
        });
    }
    Ok(out)
}

/// Every firing in `dir`'s own `*.log` segments, in name order, and how many files were not
/// segments. Read-only, and never past the directory: a subdirectory, a hidden name and a
/// symbolic link are not read.
pub fn read_firings(dir: &Path) -> (Vec<Firing>, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };
    let mut names: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let kind = std::fs::symlink_metadata(e.path()).ok()?.file_type();
            (kind.is_file() && !name.starts_with('.') && name.ends_with(".log"))
                .then(|| (name, e.path()))
        })
        .collect();
    names.sort();
    let (mut out, mut skipped) = (Vec::new(), 0);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (_, path) in names {
        let Ok(bytes) = std::fs::read(&path) else {
            skipped += 1;
            continue;
        };
        match harness::parse_segment(&String::from_utf8_lossy(&bytes)) {
            Ok(firings) => {
                for mut f in firings {
                    // Two segments that share a name keep their entries apart.
                    let mut n = 1;
                    let node = f.node.clone();
                    while !seen.insert(f.node.clone()) {
                        n += 1;
                        f.node = format!("{node}:{n}");
                    }
                    out.push(f);
                }
            }
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

// ------------------------------------------------------------------------------ the resource

/// `urn:repo:{root}:description`, a root that is a bare name: never a file's or a tree's name
/// that happens to end in `:description`.
pub struct DescriptionGrammar;

impl Grammar for DescriptionGrammar {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        let root = described_root(iri.as_str())?;
        let mut b = Bindings::new();
        b.insert("root".to_string(), root.to_string());
        Some(b)
    }

    fn pattern(&self) -> String {
        "urn:repo:{root}:description".to_string()
    }
}

/// `urn:repo:{root}:description`.
pub struct DescriptionEndpoint {
    pub web: Arc<Web>,
    pub packs: Arc<Config>,
}

const TURTLE: &str = "text/turtle";
const JSON: &str = "application/json";

#[async_trait]
impl Endpoint for DescriptionEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let root = web::binding(inv, "root")?;
        let asked = inv
            .inline_str("as")
            .ok()
            .map(|a| a.split(';').next().unwrap_or(a).trim().to_string())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| TURTLE.to_string());
        if asked != TURTLE && asked != JSON {
            return Err(Error::InvalidArgument {
                name: "as".to_string(),
                detail: format!("`{asked}` is not a face this resource serves: {TURTLE}, {JSON}"),
            });
        }
        let d = describe(&self.web, &self.packs, inv, &root).await?;
        let bytes = if asked == TURTLE {
            let mut out = RdfSerializer::from_format(RdfFormat::Turtle)
                .with_prefix("browse", BROWSE)
                .map_err(|e| Error::Endpoint(e.to_string()))?
                .with_prefix("skos", SKOS)
                .map_err(|e| Error::Endpoint(e.to_string()))?
                .for_writer(Vec::new());
            for t in &d.triples {
                out.serialize_triple(t)
                    .map_err(|e| Error::Endpoint(e.to_string()))?;
            }
            out.finish().map_err(|e| Error::Endpoint(e.to_string()))?
        } else {
            serde_json::to_vec_pretty(&description_json(&d))
                .map_err(|e| Error::Endpoint(e.to_string()))?
        };
        Ok(Representation::new(
            ReprType::new(&asked).with_param("charset", "utf-8"),
            bytes,
        ))
    }

    fn name(&self) -> &str {
        "gonk-root-description"
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title("A browse root's description")
            .summary(
                "What a harness pack's shape is checked against: the derived facts (name, GitHub \
                 owner and repository, which top-level files exist) and the declared ones (team, \
                 class, OS targets, target type, target customer) with their broader concepts. \
                 Never a path; under browse read on the root.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("root")
                    .summary("The browse root.")
                    .class("http://www.w3.org/2001/XMLSchema#string")
                    .binding(),
            )
            .input(
                web::arg("as", "The face: text/turtle or application/json.")
                    .one_of([TURTLE, JSON])
                    .optional(),
            )
            .output(TURTLE)
            .output(JSON)
    }
}

/// The JSON face: the derived facts, the declared ones, the materialized ancestors and the notes.
pub fn description_json(d: &RootDescription) -> serde_json::Value {
    let me = d.iri.as_str();
    let lits = |p: &str| -> Vec<String> {
        objects(&d.triples, me, p)
            .filter_map(|o| match o {
                Term::Literal(l) => Some(l.value().to_string()),
                _ => None,
            })
            .collect()
    };
    let broader = format!("{SKOS}broader");
    let ancestors: BTreeSet<String> = d
        .triples
        .iter()
        .filter(|t| t.predicate.as_str() == broader)
        .filter_map(|t| object_iri(&t.object).map(str::to_string))
        .collect();
    let mut declared = serde_json::Map::new();
    let mut under = serde_json::Map::new();
    for t in d.triples.iter().filter(|t| iri_of(&t.subject) == Some(me)) {
        let Some(v) = object_iri(&t.object) else {
            continue;
        };
        if t.predicate.as_str() == RDF_TYPE {
            continue;
        }
        let key = short(t.predicate.as_str());
        let key = key.strip_prefix("browse:").unwrap_or(&key).to_string();
        // A value is materialized when it is some declared value's ancestor and was not declared.
        let declared_too = d
            .triples
            .iter()
            .any(|u| iri_of(&u.subject) == Some(v) && u.predicate.as_str() == broader)
            || !ancestors.contains(v);
        let map = if declared_too {
            &mut declared
        } else {
            &mut under
        };
        map.entry(key)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("an array")
            .push(json!(v));
    }
    let github = lits(&format!("{BROWSE}githubOwner")).first().map(|owner| {
        json!({
            "owner": owner,
            "repository": lits(&format!("{BROWSE}githubRepository")).first(),
        })
    });
    json!({
        "iri": d.iri,
        "root": d.root,
        "derived": {
            "name": d.root,
            "github": github,
            "files": lits(&format!("{BROWSE}topLevelFile")),
        },
        "declared": declared,
        "under": under,
        "notes": d.notes,
    })
}
