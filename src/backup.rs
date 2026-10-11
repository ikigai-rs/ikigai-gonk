//! Backup and restore — the one export door this dataset has, and the only one it can
//! have.
//!
//! # Why this is a gonk capability and not an ops script
//!
//! RocksDB permits one writer per directory, so on a machine that runs gonk, gonk holds
//! `~/.ikigai/store` and nothing else may open it ([`crate::compose`]'s decision of record).
//! Two consequences, and together they decide the whole design:
//!
//! - **No second process can export the dataset.** There is no `ikigai -c 'source
//!   urn:iki:store:…'` that opens the files; there is only this process.
//! - **`cp -r` of the live directory is a torn snapshot.** SST files and the write-ahead
//!   log mutate under the copy, and the result may not open, or may open and be quietly
//!   short.
//!
//! So the export has to be a face this server offers, and it is [`BACKUP`].
//!
//! # ⚠ The format is N-Quads, and the easy mistake is Turtle
//!
//! **CONSTRUCT returns TRIPLES.** This dataset is partitioned by named graph — a graph per
//! named ledger, its graveyard beside it, `ikigai-browse`'s own graphs, the reserved set —
//! and that partition is what every per-graph capability
//! ([`crate::grants`]) is written against. A backup serialized as Turtle or N-Triples
//! collapses every graph into one: the quad count still matches, every triple round-trips,
//! and the restored dataset has every tenant's statements in the default graph with the
//! tenancy boundary gone. Nothing in a count would tell you.
//!
//! Measured 2026-09-16 on this machine: one named graph,
//! `urn:iki:ledger:graph:default`, holding all 2850 quads, and a default graph holding
//! zero — so today's data would survive a Turtle dump by luck. The format does not depend
//! on today's data shape.
//!
//! # ★ The dump is the engine's own words in both directions
//!
//! A backup is `urn:iki:store:select` over `{ GRAPH ?g { ?s ?p ?o } } UNION { ?s ?p ?o }`,
//! read back with **oxigraph's own SPARQL-results parser** and written with **oxigraph's
//! own N-Quads serializer**. Neither end is string surgery. That matters more than it
//! looks: this dataset holds ledger item bodies with newlines, quotation marks and
//! trailing periods in them, and a hand-rolled `?s ?p ?o .` writer corrupts exactly those
//! rows — in the one file nobody reads until the day they need it.
//!
//! `ORDER BY ?g ?s ?p ?o` is on the query on purpose. `urn:compress:gzip` pins its header
//! fields, so identical bytes compress to identical archives; a stable row order is the
//! other half of making "the data has not changed since the last backup" a digest
//! comparison rather than a belief. The cost is that the whole result is sorted — which is
//! fine at this dataset's scale (megabytes) and is the first thing to revisit if it is not.
//!
//! # Capability posture — the two most dangerous grants in the system
//!
//! - **A backup reads EVERY graph**, which is exactly the authority the per-graph boundary
//!   exists to avoid handing out. [`BACKUP`] therefore declares `urn:cap:store:read` (the
//!   broad token) *and* [`CAP_BACKUP`], which names the second authority it exercises and
//!   `urn:cap:store:read` does not: writing into and pruning gonk's backup directory.
//! - **A restore writes EVERY graph.** [`RESTORE`] declares [`CAP_RESTORE`].
//! - **Neither is reachable from the HTTP door, and that is structural rather than
//!   careful.** The HTTP door's capability is exactly the per-ledger token list
//!   [`crate::grants::grants_for_all`] computes, plus a signed-in passkey's grant; none of
//!   those lists can contain any of these three tokens, because
//!   [`crate::quic::check_grants`] refuses a `grants.json` that names one. So the
//!   privileged half of this feature lives behind the owner-only socket — its own door, not
//!   a flag on the public one — and `tests/doors.rs` proves the public door still cannot
//!   reach even `urn:iki:store:info`.
//!
//! # A backup you have not restored is not a backup
//!
//! [`RESTORE`] is the verifier as well as the restore path. It loads an archive into a
//! **new** store directory and compares the graph set and the per-graph quad counts of the
//! restored dataset against the archive it was built from — not just a total, because a
//! total is exactly the number the Turtle trap preserves. `tests/backup.rs` runs it against
//! the real stopgap archive the hub took before this feature existed.
//!
//! # A merge is not a restore
//!
//! Loading a backup into a non-empty store yields a UNION: deleted items come back, and the
//! result can never be the state that was backed up. So [`RESTORE`] **refuses** a target
//! that already holds quads, and refuses the live store outright. What it produces is a
//! complete store directory beside the live one; swapping them is an operator's act, with
//! the server stopped, which is the one moment when the destructive step is visible.
//!
//! # The scripts are in the backup too (ledger [#1175](http://localhost:1060/l/default/item/1175))
//!
//! `urn:script:*`'s atoms are not in the dataset: they are plain files in the scripts directory
//! (`gonk.script.dir`, `~/.ikigai/scripts`, [`crate::script::Scripts::open`] says why). So each
//! backup takes TWO archives under one stamp — `gonk-store-<stamp>.nq.gz` and
//! `gonk-scripts-<stamp>.json.gz`, every file of that directory's layout (`<name>/head.json`,
//! `<name>/versions/<sha256>.json`, `<name>/runs/<id>.json`) as one JSON document — and one
//! sidecar describing both. They rotate, are read out and are pruned together.
//! [`RESTORE_SCRIPTS`] is that archive's restore AND its verifier, as [`RESTORE`] is the store's:
//! it writes the files into a NEW directory and then reads every one back through
//! `ikigai-script`'s own `DirBackend`, which checks each version against the digest it is named
//! by — so a snapshot whose head points at a version it does not hold, or whose bytes were
//! altered, does not verify.
//!
//! # ★ The schedule is untrustworthy without [`STATUS`]
//!
//! gonk sits inside the accepted heartbeat-coverage gap, and a scheduled job that HANGS
//! reads STALE forever and never reads FAILING. At a 24-hour cadence a wedged backup is
//! invisible for a day at best and permanently at worst. [`STATUS`] is what makes the
//! schedule worth having: it answers from BOTH sides — the newest archive actually on disk
//! with its counts and digests, and the live timer's own health (`runs`, time since the
//! last run, time since the last SUCCESS, failures in a row). A job that has not fired
//! shows as a growing `since_last`; a job that fires and fails shows as
//! `failures_in_a_row`. Neither reads as "fine".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_time::JobRegistry;
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, NamedOrBlankNode, Quad, Term};
use oxigraph::sparql::results::{
    QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput,
};
use sha2::{Digest, Sha256};

/// Take a backup, read [`STATUS`], and read an archive back out.
///
/// One token for all three reads of the family, deliberately: the status readout names
/// every graph in the dataset with its size, which is the tenancy map. A caller who may
/// not read the graphs has no business reading their shape either.
pub const CAP_BACKUP: &str = "urn:cap:gonk:backup";

/// Load an archive into a new store directory.
pub const CAP_RESTORE: &str = "urn:cap:gonk:restore";

/// `ikigai-store`'s whole-dataset read token. Spelled through the crate rather than
/// transcribed, because a grant file elsewhere has to match it exactly.
pub fn cap_store_read() -> &'static str {
    ikigai_store::CAP_READ
}

/// Take a backup now: `Source urn:iki:gonk:backup`.
pub const BACKUP: &str = "urn:iki:gonk:backup";

/// The last successful backup, and the timer's health: `Source urn:iki:gonk:backup:status`.
pub const STATUS: &str = "urn:iki:gonk:backup:status";

/// One archive's bytes, by file name: `Source urn:iki:gonk:backup:archive:{name}`.
pub const ARCHIVE: &str = "urn:iki:gonk:backup:archive:{name}";

/// Load an archive into a new store: `Sink urn:iki:gonk:restore into=<dir>`.
pub const RESTORE: &str = "urn:iki:gonk:restore";

/// Write a scripts archive into a new directory, and verify it:
/// `Sink urn:iki:gonk:restore:scripts into=<dir>`.
pub const RESTORE_SCRIPTS: &str = "urn:iki:gonk:restore:scripts";

/// Every scope the scheduled backup job fires under — and nothing else.
///
/// ★ This is why the timer is a private [`JobRegistry`] rather than a bound
/// `urn:time:schedule`. A registry fires under ONE capability, whatever target it is given;
/// binding the control plane would mean any caller who could reach `urn:time:schedule`
/// could have this server issue any request under this list. The list is narrow, but the
/// principle is that the target set is fixed at startup by this server, not chosen at call
/// time by a caller.
pub const JOB_SCOPES: [&str; 5] = [
    CAP_BACKUP,
    ikigai_store::CAP_READ,
    JOB_BUDGET,
    JOB_ANSWER_ROWS,
    JOB_ANSWER_BYTES,
];

/// How long the scheduled backup's whole-dataset query may run, in milliseconds: **120 s**,
/// which is also `ikigai-store`'s ceiling (ledger [#979](http://localhost:1060/l/default/item/979)).
///
/// ★ Since store 0.2.8 every SPARQL evaluation runs within a time budget, and a capability
/// with no `urn:cap:store:budget:<ms>` grant gets the store's 5 s BASE. The job fires under
/// [`JOB_SCOPES`] — scoped, not root — so without this grant it got 5 s, and its query (every
/// quad, sorted, as SPARQL JSON) does not fit: measured 2026-10-09 on a RocksDB store restored
/// from the live dataset's archive (361,607 quads; `examples/backup-cost.rs`, release build),
/// a whole backup took 14.7–15.4 s on a quiet machine and 27 s beside three runaway
/// evaluations, and the store arc measured the query alone at 24.6 s. Without the grant every
/// run was refused at 5.0 s with a typed `Timeout`.
///
/// Why 120 s and not a tighter multiple: the dataset grows (almost all of it is the browse
/// graph, which grows with every explained file), the backup is the only export this dataset
/// has, and a refused backup is a silent loss of the one guarantee it exists for — while a
/// slow one costs a core for two minutes once a day. The store clamps every grant to its
/// ceiling, so asking for more would buy nothing; this is the most a scoped caller can hold,
/// about five times the slowest measured run.
pub const JOB_BUDGET_MS: u64 = 120_000;

/// The store's budget grant for [`JOB_BUDGET_MS`]:
/// `ikigai_store::budget::cap_budget(JOB_BUDGET_MS)`, spelled as a literal because a `const`
/// cannot call it (a unit test pins that the two agree).
pub const JOB_BUDGET: &str = "urn:cap:store:budget:120000";

/// How many rows the scheduled backup's answer may hold: **10,000,000**, `ikigai-store`'s
/// ceiling (ledger [#993](http://localhost:1060/l/default/item/993)).
///
/// ★ Since store 0.2.9 every SPARQL answer is bounded in SIZE as well as time, and a
/// capability with no `urn:cap:store:answer:*` grant gets the store's BASE: 100,000 rows and
/// 16 MiB. The backup's answer is one row per quad of the WHOLE dataset, as SPARQL JSON:
/// measured 2026-10-10 on a RocksDB store restored from the 2026-10-09 archive (361,607 quads;
/// `examples/backup-cost.rs`, release build), it is refused on BYTES after 13.9–23.4 s under the
/// time grant alone — the `ORDER BY` materializes every row before the first is written, so
/// the refusal costs the whole sort — and the store arc measured the same answer at 121,625,465
/// bytes (ledger #970). Over the row bound too: the dataset is 3.6 times it.
///
/// Why the ceilings and not a measured multiple, for [`JOB_BUDGET_MS`]'s reason: the dataset
/// grows, the backup is the only export this dataset has, and a refused backup is a silent loss
/// of the one guarantee it exists for. The store clamps every grant to its ceiling, so asking
/// for more would buy nothing. ⚠ The byte ceiling (1 GiB) is about nine times today's answer, so
/// it is the first of the three bounds a growing dataset will reach; the backup's status
/// readout names the archive's N-Quads size, which is about two thirds of the answer's.
pub const JOB_ANSWER_ROWS_MAX: u64 = 10_000_000;

/// How many serialized bytes the scheduled backup's answer may hold: **1 GiB**,
/// `ikigai-store`'s ceiling. See [`JOB_ANSWER_ROWS_MAX`]: a row grant raises rows and nothing
/// else, so a backup holding only it is still refused on bytes, and the job needs both.
pub const JOB_ANSWER_BYTES_MAX: u64 = 1 << 30;

/// The store's row grant for [`JOB_ANSWER_ROWS_MAX`]:
/// `ikigai_store::budget::cap_answer(JOB_ANSWER_ROWS_MAX)`, spelled as a literal for the reason
/// [`JOB_BUDGET`] is (a unit test pins that the two agree).
pub const JOB_ANSWER_ROWS: &str = "urn:cap:store:answer:10000000";

/// The store's byte grant for [`JOB_ANSWER_BYTES_MAX`]:
/// `ikigai_store::budget::cap_answer_bytes(JOB_ANSWER_BYTES_MAX)`, spelled as a literal for the
/// reason [`JOB_BUDGET`] is (a unit test pins that the two agree).
pub const JOB_ANSWER_BYTES: &str = "urn:cap:store:answer:bytes:1073741824";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const N_QUADS: &str = "application/n-quads";
const GZIP: &str = "application/gzip";
const JSON: &str = "application/json";

/// The archive suffix: plain N-Quads, gzipped.
pub const ARCHIVE_SUFFIX: &str = ".nq.gz";
/// The sidecar suffix, carrying one archive's counts and digests.
pub const META_SUFFIX: &str = ".meta.json";

/// The scripts archive's name: `gonk-scripts-<stamp>.json.gz`, the stamp of the store archive
/// it was taken with.
pub const SCRIPTS_PREFIX: &str = "gonk-scripts-";
/// The scripts archive's suffix: one JSON document, gzipped.
pub const SCRIPTS_SUFFIX: &str = ".json.gz";
/// What a scripts archive's document says it is, under `format`.
pub const SCRIPTS_FORMAT: &str = "gonk-scripts/1";

/// The ceiling handed to `urn:decompress:gzip` on a restore: 4 GiB of N-Quads, which is
/// above the module's own 1 GiB ceiling and therefore means "the module's maximum".
///
/// ⚠ The bound REFUSES rather than truncates, which is the property that matters: a
/// truncated restore would load cleanly and be short, and the per-graph comparison below
/// would be the only thing that noticed.
const RESTORE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Where backups land, how many are kept, and how often one is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The rotation directory. gonk owns it and writes nothing else there.
    pub dir: PathBuf,
    /// How many archives to keep. Pruned oldest-first, after a successful write.
    pub keep: usize,
    /// The cadence, or `None` for "no schedule" — the resources are still bound, because
    /// an operator who has turned the timer off still has a dataset to export.
    pub every: Option<Duration>,
    /// The live store's directory, so a restore can refuse to be pointed at it.
    pub store_path: PathBuf,
    /// The live scripts directory (`gonk.script.dir`), snapshotted with every backup and the
    /// directory [`RESTORE_SCRIPTS`] refuses to write into; `None` for a server that binds no
    /// script family (its backups hold the dataset alone).
    pub scripts: Option<PathBuf>,
}

/// What the backup family is composed against: the settings, and the timer whose health
/// [`STATUS`] reports (absent when nothing is scheduled).
#[derive(Clone)]
pub struct Backups {
    /// Where and how often.
    pub settings: Arc<Settings>,
    /// The registry the scheduled job lives in, for the health half of [`STATUS`].
    pub jobs: Option<JobRegistry>,
    /// When the scheduled job fires next, as its [`BackupTimer`] records it — what
    /// [`STATUS`] says is due (ledger #1087). Never written when nothing is scheduled.
    pub next: NextRun,
}

/// The backup family's space: [`BACKUP`], [`STATUS`], [`ARCHIVE`], [`RESTORE`] and
/// [`RESTORE_SCRIPTS`].
pub fn space(backups: Backups) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new(BACKUP),
            TakeBackup {
                backups: backups.clone(),
            },
        )
        .bind(
            Exact::new(STATUS),
            Status {
                backups: backups.clone(),
            },
        )
        .bind(
            UriTemplate::parse(ARCHIVE).expect("a constant template"),
            ReadArchive {
                backups: backups.clone(),
            },
        )
        .bind(
            Exact::new(RESTORE),
            Restore {
                backups: backups.clone(),
            },
        )
        .bind(Exact::new(RESTORE_SCRIPTS), RestoreScripts { backups })
        .named(crate::spaces::iri(crate::spaces::BACKUP))
}

// ---------------------------------------------------------------- the dump

/// One graph's name as it appears in a count table: the IRI, or `""` for the default graph.
const DEFAULT_GRAPH_KEY: &str = "";

/// A dump: the N-Quads bytes, and the per-graph counts measured while writing them.
struct Dump {
    nquads: Vec<u8>,
    graphs: BTreeMap<String, u64>,
}

impl Dump {
    fn quads(&self) -> u64 {
        self.graphs.values().sum()
    }
}

/// The whole dataset, every graph, as N-Quads — through the kernel, under the caller's
/// capability.
///
/// The UNION is not decoration. `{ ?s ?p ?o }` alone reads the DEFAULT graph only (this
/// store does not evaluate a bare pattern over the union of graphs — `ikigai-store`'s
/// `a_property_path_cannot_walk_out_of_the_scope` pins that), and `GRAPH ?g { ?s ?p ?o }`
/// alone never sees the default graph. A backup missing either half is a backup that
/// restores to a different dataset.
async fn dump(inv: &Invocation<'_>) -> Result<Dump> {
    let query = "SELECT ?g ?s ?p ?o WHERE { { GRAPH ?g { ?s ?p ?o } } UNION { ?s ?p ?o } } \
                 ORDER BY ?g ?s ?p ?o";
    let results = inv
        .issue(
            Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                .with_arg(
                    "as",
                    ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                ),
        )
        .await?;
    quads_from_results(&results.bytes)
}

/// `urn:iki:store:select`, the one resource a backup reads.
const BACKUP_SELECT: &str = "urn:iki:store:select";

/// Parse a SPARQL SELECT result set into N-Quads, counting per graph as it goes.
fn quads_from_results(bytes: &[u8]) -> Result<Dump> {
    let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
    let solutions = match parser.for_reader(bytes) {
        Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) => solutions,
        Ok(ReaderQueryResultsParserOutput::Boolean(_)) => {
            return Err(Error::Endpoint(
                "the store answered a boolean to a SELECT — the backup query was rewritten \
                 somewhere it should not have been"
                    .to_string(),
            ))
        }
        Err(e) => return Err(Error::Endpoint(format!("reading the store's results: {e}"))),
    };
    let mut graphs: BTreeMap<String, u64> = BTreeMap::new();
    let mut out: Vec<u8> = Vec::new();
    let mut writer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for solution in solutions {
        let solution = solution.map_err(|e| Error::Endpoint(format!("a result row: {e}")))?;
        let quad = quad_from_solution(&solution)?;
        let key = match &quad.graph_name {
            GraphName::DefaultGraph => DEFAULT_GRAPH_KEY.to_string(),
            GraphName::NamedNode(node) => node.as_str().to_string(),
            GraphName::BlankNode(node) => format!("_:{}", node.as_str()),
        };
        *graphs.entry(key).or_default() += 1;
        writer
            .serialize_quad(&quad)
            .map_err(|e| Error::Endpoint(format!("writing a quad: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| Error::Endpoint(format!("finishing the dump: {e}")))?;
    Ok(Dump {
        nquads: out,
        graphs,
    })
}

/// One `?g ?s ?p ?o` row as a [`Quad`].
///
/// Every refusal here is a shape the store cannot actually produce (a literal subject, a
/// blank-node predicate). They are errors rather than skips because a backup that silently
/// dropped a row would be a backup with a hole in it, and the count would agree with
/// itself.
fn quad_from_solution(solution: &oxigraph::sparql::QuerySolution) -> Result<Quad> {
    let subject = match solution.get("s") {
        Some(Term::NamedNode(node)) => NamedOrBlankNode::NamedNode(node.clone()),
        Some(Term::BlankNode(node)) => NamedOrBlankNode::BlankNode(node.clone()),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's subject is not an IRI or a blank node: {other:?}"
            )))
        }
    };
    let predicate = match solution.get("p") {
        Some(Term::NamedNode(node)) => node.clone(),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's predicate is not an IRI: {other:?}"
            )))
        }
    };
    let object = solution
        .get("o")
        .ok_or_else(|| Error::Endpoint("a row has no object".to_string()))?
        .clone();
    let graph_name = match solution.get("g") {
        None => GraphName::DefaultGraph,
        Some(Term::NamedNode(node)) => GraphName::NamedNode(node.clone()),
        Some(Term::BlankNode(node)) => GraphName::BlankNode(node.clone()),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's graph is not an IRI: {other:?}"
            )))
        }
    };
    Ok(Quad {
        subject,
        predicate,
        object,
        graph_name,
    })
}

/// The per-graph counts of an N-Quads document, measured by parsing it — the independent
/// half of the restore comparison.
///
/// ⚠ It must not come from the same code path that produced the dump. A dump and a
/// comparison that share a counter agree with each other about a dataset neither of them
/// read.
///
/// It counts distinct QUADS, not lines: a dataset is a set, a document may repeat a
/// statement, and the store keeps one — so a line count reports a correct restore as a
/// mismatch (ledger #737).
fn counts_in_nquads(bytes: &[u8]) -> Result<BTreeMap<String, u64>> {
    let mut graphs: BTreeMap<String, u64> = BTreeMap::new();
    let mut seen: std::collections::HashSet<Quad> = std::collections::HashSet::new();
    for quad in RdfParser::from_format(RdfFormat::NQuads).for_reader(bytes) {
        let quad = quad.map_err(|e| Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!("parsing N-Quads: {e}"),
        })?;
        if !seen.insert(quad.clone()) {
            continue;
        }
        let key = match &quad.graph_name {
            GraphName::DefaultGraph => DEFAULT_GRAPH_KEY.to_string(),
            GraphName::NamedNode(node) => node.as_str().to_string(),
            GraphName::BlankNode(node) => format!("_:{}", node.as_str()),
        };
        *graphs.entry(key).or_default() += 1;
    }
    Ok(graphs)
}

// ---------------------------------------------------------------- archives

/// One archive on disk, with whatever its sidecar recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    /// The file name, which is also the `{name}` [`ARCHIVE`] captures.
    pub name: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// The sidecar's JSON text, when there is one.
    pub meta: Option<String>,
}

/// The rotation set, oldest first: gonk's OWN archives and nothing else in the directory.
///
/// ★ **Only names this server mints** — `gonk-store-<stamp>.nq.gz`, the stamp exactly as
/// [`stamp_compact`] spells it. `gonk.backup.dir` is a directory an
/// operator names, so it can hold an export of their own, a copy, a `latest` link; before
/// ledger #737 every `*.nq.gz` in it was rotated, so such a file was DELETED by the prune, or
/// — sorting after `gonk-store-…` — kept as "the newest" while the backup just taken went.
///
/// Among its own names the sort is by the instant each was taken: the stamp in the name IS
/// that instant, at one-second resolution and zero-padded, so the names sort chronologically
/// — one fewer thing depending on a file system's idea of mtime.
pub fn archives(dir: &Path) -> Vec<Archive> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Archive> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_own_archive(&name) {
                return None;
            }
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let meta = std::fs::read_to_string(dir.join(format!("{name}{META_SUFFIX}"))).ok();
            Some(Archive { name, bytes, meta })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Whether `name` is an archive this server writes: `gonk-store-YYYY-MM-DDTHHMMSSZ.nq.gz`,
/// digits where [`stamp_compact`] puts digits, and nothing before or after.
fn is_own_archive(name: &str) -> bool {
    let Some(stamp) = name
        .strip_prefix("gonk-store-")
        .and_then(|rest| rest.strip_suffix(ARCHIVE_SUFFIX))
    else {
        return false;
    };
    let shape = b"dddd-dd-ddTddddddZ";
    stamp.len() == shape.len()
        && stamp.bytes().zip(shape).all(|(byte, want)| match want {
            b'd' => byte.is_ascii_digit(),
            literal => byte == *literal,
        })
}

/// When the newest archive in `dir` was taken, from its sidecar — `None` when there is no
/// archive, or none whose sidecar says.
///
/// This is what `main` asks at startup to decide whether a backup is already overdue. It
/// reads the RECORDED instant rather than the file's mtime, because a rotation directory
/// that was copied, restored or rsynced keeps its names and contents and loses its mtimes —
/// and the wrong answer here is "a backup was taken recently", which is the answer mtime
/// gives on a freshly copied directory.
pub fn newest_taken_at_ms(dir: &Path) -> Option<u64> {
    archives(dir)
        .last()?
        .meta
        .as_deref()
        .and_then(|meta| field(meta, "taken_at_ms"))
        .and_then(|value| value.parse().ok())
}

// ---------------------------------------------------------------- when the timer fires

/// The soonest the scheduled backup's first fire comes after startup: long enough that a
/// restart loop does not take one per restart, short enough that an operator watching a
/// deploy sees it.
pub const CATCH_UP: Duration = Duration::from_secs(60);

/// How long from `now_ms` until the scheduled backup's FIRST fire: one cadence after the
/// newest archive on disk (`newest_ms`, from [`newest_taken_at_ms`]), never sooner than
/// [`CATCH_UP`] and never later than one cadence; [`CATCH_UP`] when there is no archive.
///
/// ★★ **The phase of a daily backup belongs to the ROTATION, not to the process** (ledger
/// #1087). A recurring timer's first tick is one interval after it starts, so a 1-day timer
/// on a server restarted at 23:21 whose newest archive was taken at 12:27 first fires at
/// 23:21 the next day: a 35-hour gap, and every restart before an archive is a day old moved
/// the next backup again. Until this, startup only caught up an archive ALREADY older than
/// the cadence; now every start schedules the first fire for when the newest archive comes
/// due, so a restart costs a backup nothing and the gap between archives stays one cadence.
///
/// A newest archive stamped in the future (a clock stepped back) is answered with one
/// cadence, not a longer wait: the bound is on how long a backup can be put off.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::backup::{first_delay, CATCH_UP};
/// let day = Duration::from_secs(86_400);
/// let hour = 3_600_000;
/// // Restarted 11 h after the newest archive: the first fire is 13 h out, not 24.
/// assert_eq!(first_delay(11 * hour, Some(0), day), Duration::from_secs(13 * 3_600));
/// // Already overdue, or never taken: a catch-up a minute out.
/// assert_eq!(first_delay(30 * hour, Some(0), day), CATCH_UP);
/// assert_eq!(first_delay(30 * hour, None, day), CATCH_UP);
/// ```
pub fn first_delay(now_ms: u64, newest_ms: Option<u64>, every: Duration) -> Duration {
    let Some(newest) = newest_ms else {
        return CATCH_UP;
    };
    let every_ms = u64::try_from(every.as_millis()).unwrap_or(u64::MAX);
    let due = newest.saturating_add(every_ms);
    Duration::from_millis(due.saturating_sub(now_ms))
        .min(every)
        .max(CATCH_UP)
}

/// When the scheduled backup fires next, in milliseconds since the epoch on the clock that
/// stamps it: written by [`BackupTimer`] each time it arms, read by [`STATUS`] against the same
/// clock. `None` while a run is in progress (and before the timer starts).
#[derive(Clone)]
pub struct NextRun {
    due: Arc<std::sync::Mutex<Option<u64>>>,
    clock: Arc<dyn ikigai_core::Clock>,
}

impl Default for NextRun {
    /// On the system wall clock — what the server runs on.
    fn default() -> Self {
        NextRun::with_clock(Arc::new(ikigai_core::SystemClock))
    }
}

impl NextRun {
    /// Stamped and read on `clock` — a fixed one in a test.
    pub fn with_clock(clock: Arc<dyn ikigai_core::Clock>) -> Self {
        NextRun {
            due: Arc::default(),
            clock,
        }
    }

    /// When the next run is due, or `None` while one is running.
    pub fn due_ms(&self) -> Option<u64> {
        *self.due.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The clock's reading now, in milliseconds since the epoch.
    pub fn now_ms(&self) -> u64 {
        self.clock.now().0
    }

    fn set(&self, due: Option<u64>) {
        *self.due.lock().unwrap_or_else(|e| e.into_inner()) = due;
    }

    /// Record the next run as `wait` from now.
    fn arm(&self, wait: Duration) {
        let wait = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        self.set(Some(self.now_ms().saturating_add(wait)));
    }
}

/// The backup registry's timer: `ikigai_time::ThreadTimer`'s sleeping thread, except that
/// the FIRST tick comes after `first` rather than after one interval ([`first_delay`]), and
/// that it records when each next tick is due ([`NextRun`]) so [`STATUS`] can say so.
///
/// ⚠ `first` applies to every job started on this backend. The backup registry holds one job
/// (`urn:iki:gonk:backup`, scheduled by [`start`]), which is the only reason that is right;
/// do not schedule a second job on a registry built over it.
pub struct BackupTimer {
    first: Duration,
    next: NextRun,
}

impl BackupTimer {
    /// A timer whose first tick is `first` from its start, recording each next tick in `next`.
    pub fn new(first: Duration, next: NextRun) -> Self {
        BackupTimer { first, next }
    }
}

impl ikigai_time::TimerBackend for BackupTimer {
    fn start(
        &self,
        interval: Duration,
        recurring: bool,
        on_tick: Arc<dyn Fn() + Send + Sync>,
    ) -> ikigai_time::TimerHandle {
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let (first, next) = (self.first, self.next.clone());
        next.arm(first);
        std::thread::spawn(move || {
            let mut wait = first;
            loop {
                std::thread::sleep(wait);
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                next.set(None);
                on_tick();
                if !recurring {
                    break;
                }
                wait = interval;
                next.arm(wait);
            }
        });
        ikigai_time::TimerHandle::new(move || {
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed)
        })
    }
}

/// Build the registry the scheduled backup fires from, phased to the rotation: its timer's
/// first tick is [`first_delay`] from now. Returns the registry (for [`Backups::jobs`]) and that
/// delay, for the banner. Nothing is scheduled yet — [`start`] does that once the kernel the
/// job resolves through exists.
///
/// The registry's ceiling is [`JOB_SCOPES`] and so is the job's own capability ([`start`]), so
/// the clamp each tick takes (ikigai-time 0.4.0, ledger #79) is exactly `JOB_SCOPES`. `next`
/// carries the clock everything here reads, so a test passes a fixed one.
pub fn registry(settings: &Settings, every: Duration, next: &NextRun) -> (JobRegistry, Duration) {
    let first = first_delay(next.now_ms(), newest_taken_at_ms(&settings.dir), every);
    let jobs = JobRegistry::new(
        Arc::new(BackupTimer::new(first, next.clone())),
        Arc::clone(&next.clock),
    )
    .with_capability(Capability::scoped(JOB_SCOPES));
    (jobs, first)
}

/// Install `resolver` in a [`registry`] and schedule the backup on it, every `every`, under
/// exactly [`JOB_SCOPES`]. Returns the job's id.
pub fn start(
    jobs: &JobRegistry,
    every: Duration,
    resolver: Arc<dyn ikigai_resolve::Resolver>,
) -> std::result::Result<u64, String> {
    jobs.set_resolver(resolver);
    jobs.schedule_persistent(
        BACKUP.to_string(),
        Verb::Source,
        ikigai_time::Schedule::Every(every),
        true,
        Capability::scoped(JOB_SCOPES),
    )
}

/// Reject anything that is not a bare file name in the rotation directory.
///
/// `{name}` arrives from a caller, and the endpoint reads the file it names. A segment with
/// a separator or a `..` in it would make this an arbitrary-file read behind a capability
/// that says "backups".
fn archive_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let bad = name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !(name.ends_with(ARCHIVE_SUFFIX) || name.ends_with(SCRIPTS_SUFFIX));
    if bad {
        return Err(Error::InvalidArgument {
            name: "name".to_string(),
            detail: format!(
                "`{name}` is not an archive in this server's backup directory — a bare file \
                 name ending `{ARCHIVE_SUFFIX}` or `{SCRIPTS_SUFFIX}`, as `{STATUS}` lists them"
            ),
        });
    }
    Ok(dir.join(name))
}

// ---------------------------------------------------------------- taking one

struct TakeBackup {
    backups: Backups,
}

#[async_trait]
impl Endpoint for TakeBackup {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => self.take(inv).await,
            other => Err(unsupported("gonk-backup", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup")
            .title("Take a backup of the whole dataset")
            .summary(
                "Serializes EVERY graph as N-Quads through `urn:iki:store:select`, gzips it \
                 by resolving `urn:compress:gzip`, writes it into this server's backup \
                 directory with a sidecar recording the per-graph counts and both digests, \
                 and prunes the rotation set oldest-first. Quad-shaped on purpose: a Turtle \
                 or N-Triples dump collapses every named graph into one and destroys the \
                 per-graph tenancy boundary while the quad count still matches.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .requires(ikigai_store::CAP_READ)
            .output("text/plain")
    }
}

impl TakeBackup {
    async fn take(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let settings = &self.backups.settings;
        // The scripts FIRST: a directory that cannot be read fails the backup before anything
        // is written, rather than leaving a store archive whose sidecar says nothing of them.
        let scripts = match &settings.scripts {
            Some(dir) => Some(snapshot_scripts(dir)?),
            None => None,
        };
        let dump = dump(inv).await?;
        let quads = dump.quads();
        let nquads_digest = digest(&dump.nquads);
        let nquads_bytes = dump.nquads.len();

        // Compression through the kernel, not through a gzip crate: the archive is a
        // composition of resources, and `urn:compress:gzip` is where determinism and the
        // decompression bound are argued.
        let archive = inv
            .issue(
                Request::new(Verb::Source, iri("urn:compress:gzip")?)
                    .with_arg("content", ArgRef::Inline(dump.nquads.clone())),
            )
            .await?;
        let archive_digest = digest(&archive.bytes);
        let scripts_archive = match &scripts {
            Some(snapshot) => Some(
                inv.issue(
                    Request::new(Verb::Source, iri("urn:compress:gzip")?)
                        .with_arg("content", ArgRef::Inline(snapshot.json.clone())),
                )
                .await?
                .bytes,
            ),
            None => None,
        };

        // Re-count the store AFTER the dump. A SELECT is one snapshot, so this cannot
        // "fix" a torn dump — it is evidence about whether the dataset was moving while
        // the backup was taken, which is worth recording next to a number a restore will
        // be compared against.
        let after = self.count(inv).await?;

        let now = inv
            .now()
            .map(|t| t.as_millis())
            .ok_or_else(|| Error::Endpoint("no clock: a backup must be able to say when".into()))?;
        let stamp = stamp_compact(now);
        let name = format!("gonk-store-{stamp}{ARCHIVE_SUFFIX}");
        let scripts_name = format!("{SCRIPTS_PREFIX}{stamp}{SCRIPTS_SUFFIX}");
        create_private_dir(&settings.dir)
            .map_err(|e| Error::Endpoint(format!("creating {}: {e}", settings.dir.display())))?;

        let meta = meta_json(&MetaFields {
            archive: &name,
            taken_at: &stamp_iso(now),
            taken_at_ms: now,
            quads,
            graphs: &dump.graphs,
            nquads_bytes,
            nquads_sha256: &nquads_digest,
            archive_bytes: archive.bytes.len(),
            archive_sha256: &archive_digest,
            store_quads_after_dump: after,
            scripts: match (&scripts, &scripts_archive) {
                (Some(snapshot), Some(archive)) => Some(ScriptsMeta {
                    archive: &scripts_name,
                    names: snapshot.names,
                    files: snapshot.files,
                    json_bytes: snapshot.json.len(),
                    json_sha256: digest(&snapshot.json),
                    archive_bytes: archive.len(),
                    archive_sha256: digest(archive),
                }),
                _ => None,
            },
        });

        // Write the archive first and the sidecar second, each through a temporary name.
        // A crash between them leaves an archive without a sidecar, which `STATUS` reports
        // as exactly that; the reverse order would leave a sidecar describing a file that
        // does not exist, which reads as a good backup.
        write_atomic(&settings.dir.join(&name), &archive.bytes)?;
        if let Some(archive) = &scripts_archive {
            write_atomic(&settings.dir.join(&scripts_name), archive)?;
        }
        write_atomic(
            &settings.dir.join(format!("{name}{META_SUFFIX}")),
            meta.as_bytes(),
        )?;

        let pruned = prune(&settings.dir, settings.keep, &name);
        let previous = archives(&settings.dir)
            .iter()
            .rev()
            .filter(|a| a.name != name)
            .find_map(|a| a.meta.as_deref().and_then(|m| field(m, "nquads_sha256")));
        let unchanged = previous.as_deref() == Some(nquads_digest.as_str());

        let mut report = String::new();
        report.push_str(&format!("backup {name}\n"));
        report.push_str(&format!(
            "  {quads} quads in {} graph(s), {nquads_bytes} bytes of N-Quads -> {} bytes gzipped\n",
            dump.graphs.len(),
            archive.bytes.len()
        ));
        for (graph, count) in &dump.graphs {
            let shown = if graph.is_empty() {
                "(default graph)"
            } else {
                graph.as_str()
            };
            report.push_str(&format!("  {count:>8}  {shown}\n"));
        }
        report.push_str(&format!("  sha256 (n-quads) {nquads_digest}\n"));
        report.push_str(&format!("  sha256 (archive) {archive_digest}\n"));
        match (&scripts, &scripts_archive) {
            (Some(snapshot), Some(archive)) => report.push_str(&format!(
                "  scripts {scripts_name}: {} script(s), {} file(s), {} bytes gzipped\n",
                snapshot.names,
                snapshot.files,
                archive.len()
            )),
            _ => report.push_str("  scripts: none bound on this server\n"),
        }
        if after != quads {
            report.push_str(&format!(
                "  ⚠ the store held {after} quads after the dump, not {quads} — the dataset \
                 was written while the backup was taken. The archive is one consistent \
                 snapshot; this is a note about what changed after it.\n"
            ));
        }
        if unchanged {
            report.push_str("  identical to the previous backup (same N-Quads digest)\n");
        }
        report.push_str(&format!("  kept {}", settings.keep));
        if pruned.is_empty() {
            report.push_str(", pruned nothing\n");
        } else {
            report.push_str(&format!(", pruned {}\n", pruned.join(", ")));
        }
        Ok(plain(report))
    }

    /// The store's quad count, through the same door the dump used.
    async fn count(&self, inv: &Invocation<'_>) -> Result<u64> {
        let query = "SELECT (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } UNION \
                     { ?s ?p ?o } }";
        let results = inv
            .issue(
                Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                    .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                    .with_arg(
                        "as",
                        ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                    ),
            )
            .await?;
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
        let Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) =
            parser.for_reader(results.bytes.as_slice())
        else {
            return Err(Error::Endpoint(
                "counting the store: not a solution set".into(),
            ));
        };
        for solution in solutions {
            let solution = solution.map_err(|e| Error::Endpoint(format!("counting: {e}")))?;
            if let Some(Term::Literal(literal)) = solution.get("n") {
                return literal
                    .value()
                    .parse()
                    .map_err(|_| Error::Endpoint("counting: not a number".into()));
            }
        }
        Err(Error::Endpoint("counting the store: no rows".into()))
    }
}

/// Keep the newest `keep` archives, deleting the rest with their sidecars. Returns what
/// went.
///
/// ★ **`taken` — the archive this backup just wrote — is never pruned, and counts as one of
/// the `keep`.** Without that, `keep` archives stamped LATER than the clock (copied in from
/// a machine whose clock ran ahead, or left by a clock stepped back) sort as the newest for
/// ever, and every backup taken here is deleted the moment it is written while the report
/// says "backup gonk-store-…". `keep` is at least 1 ([`crate::config`] refuses 0).
fn prune(dir: &Path, keep: usize, taken: &str) -> Vec<String> {
    let others: Vec<Archive> = archives(dir)
        .into_iter()
        .filter(|archive| archive.name != taken)
        .collect();
    let keep_others = keep.saturating_sub(1);
    if others.len() <= keep_others {
        return Vec::new();
    }
    let mut gone = Vec::new();
    for archive in &others[..others.len() - keep_others] {
        // The sidecar goes FIRST. A sidecar left behind describes a file that is not there,
        // which is the one state that reads like a backup and is not.
        let _ = std::fs::remove_file(dir.join(format!("{}{META_SUFFIX}", archive.name)));
        if let Some(scripts) = scripts_archive_for(&archive.name) {
            let _ = std::fs::remove_file(dir.join(scripts));
        }
        if std::fs::remove_file(dir.join(&archive.name)).is_ok() {
            gone.push(archive.name.clone());
        }
    }
    gone
}

/// Write through a temporary name in the same directory, then rename.
///
/// A backup interrupted halfway is the failure this exists for: a rename is atomic within
/// a directory, so the rotation set never holds a half-written archive that a later restore
/// would discover was short.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension("partial");
    let mut file = create_private(&temporary)
        .map_err(|e| Error::Endpoint(format!("creating {}: {e}", temporary.display())))?;
    file.write_all(bytes)
        .map_err(|e| Error::Endpoint(format!("writing {}: {e}", temporary.display())))?;
    drop(file);
    std::fs::rename(&temporary, path)
        .map_err(|e| Error::Endpoint(format!("renaming into {}: {e}", path.display())))
}

/// A new, empty file that is `0600` FROM THE MOMENT IT EXISTS. A backup is the whole dataset
/// in one file, including every graph the per-graph capabilities exist to separate; it is
/// owner-only or it has undone them — and narrowing the mode after the bytes are written
/// leaves them readable for as long as the write takes.
///
/// A temporary left by a crash is removed first rather than reopened: opening an existing
/// file keeps ITS mode, whatever this one asks for. `create_new` also refuses to follow a
/// symlink planted at the name.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The rotation directory and any parent it needs, created `0700` — no one else lists the
/// archives. A directory that already exists keeps the mode its operator gave it: the
/// files in it are owner-only either way, and narrowing a directory gonk did not create is
/// not this function's call.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

// ---------------------------------------------------------------- the status

struct Status {
    backups: Backups,
}

#[async_trait]
impl Endpoint for Status {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => {
                let json = matches!(inv.inline_str("as"), Ok(JSON));
                if json {
                    Ok(Representation::new(
                        ReprType::new(JSON).with_param("charset", "utf-8"),
                        self.json().into_bytes(),
                    ))
                } else {
                    Ok(plain(self.text()))
                }
            }
            other => Err(unsupported("gonk-backup-status", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup-status"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup-status")
            .title("When the last good backup was, and whether the timer is still alive")
            .summary(
                "Answers from both sides: the newest archive actually on disk with its \
                 per-graph counts and digests, the whole rotation set, and — when a backup \
                 is scheduled — the timer's own health (runs, time since the last run, time \
                 since the last SUCCESS, failures in a row). A job that HANGS reads STALE \
                 forever and never reads FAILING, so a schedule without this manufactures \
                 confidence rather than providing it.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .input(
                ArgSpec::new("as")
                    .summary("the representation to answer in")
                    .class(XSD_STRING)
                    .one_of(["text/plain", JSON])
                    .default_value("text/plain")
                    .optional(),
            )
            .output("text/plain")
            .output(JSON)
    }
}

impl Status {
    fn text(&self) -> String {
        let settings = &self.backups.settings;
        let found = archives(&settings.dir);
        let mut out = format!("backups in {}\n", settings.dir.display());
        match settings.every {
            Some(every) => out.push_str(&format!(
                "  schedule  every {} — keep {}\n",
                humanize(every),
                settings.keep
            )),
            None => out.push_str(&format!(
                "  schedule  OFF (gonk.backup.every = \"off\") — keep {}\n",
                settings.keep
            )),
        }
        match found.last() {
            None => out
                .push_str("  ⚠ NO BACKUP HAS EVER BEEN TAKEN — there is nothing here to restore\n"),
            Some(newest) => {
                out.push_str(&format!("  newest    {}\n", newest.name));
                match &newest.meta {
                    None => out.push_str(
                        "    ⚠ no sidecar: this archive was written but its counts were not \
                         recorded. Treat it as unverified.\n",
                    ),
                    Some(meta) => {
                        for key in [
                            "taken_at",
                            "quads",
                            "nquads_bytes",
                            "archive_bytes",
                            "nquads_sha256",
                        ] {
                            if let Some(value) = field(meta, key) {
                                out.push_str(&format!("    {key:<14} {value}\n"));
                            }
                        }
                        out.push_str(&format!("    graphs         {}\n", graph_line(meta)));
                        match field(meta, "scripts_archive") {
                            Some(scripts) => out.push_str(&format!(
                                "    scripts        {scripts}: {} script(s), {} file(s)\n",
                                field(meta, "scripts_names").unwrap_or_default(),
                                field(meta, "scripts_files").unwrap_or_default()
                            )),
                            None => out.push_str(
                                "    scripts        not in this backup (no script family bound, \
                                 or taken before ledger #1175)\n",
                            ),
                        }
                    }
                }
            }
        }
        out.push_str(&format!("  rotation  {} archive(s)\n", found.len()));
        for archive in &found {
            out.push_str(&format!("    {:>10}  {}\n", archive.bytes, archive.name));
        }
        out.push_str(&self.health_text());
        out
    }

    /// The timer half. Facts, not a verdict — but the facts are chosen so that the two
    /// failure shapes a 24-hour job has are both visible: never fired (`runs 0`, `since
    /// last` growing) and firing but failing (`failures in a row`).
    fn health_text(&self) -> String {
        let Some(jobs) = &self.backups.jobs else {
            return "  timer     not scheduled in this process\n".to_string();
        };
        let health = jobs.health();
        let Some(job) = health.iter().find(|job| job.target == BACKUP) else {
            return "  ⚠ timer  a registry exists but NO JOB fires urn:iki:gonk:backup — \
                    nothing is taking backups\n"
                .to_string();
        };
        let mut out = format!(
            "  timer     job #{} every {} — {} run(s)\n",
            job.id,
            humanize(job.interval),
            job.runs
        );
        out.push_str(&self.next_line());
        match job.since_last {
            None => out.push_str(
                "    has not fired since this server started: the first run is phased to the \
                 newest archive, so `next run` above is when it is due — past that, the timer \
                 is wedged.\n",
            ),
            Some(since) => out.push_str(&format!("    since last run      {}\n", humanize(since))),
        }
        match job.since_last_success {
            None if job.runs > 0 => out.push_str(
                "    ⚠ has NEVER SUCCEEDED — every run so far failed\n"
                    .to_string()
                    .as_str(),
            ),
            None => {}
            Some(since) => out.push_str(&format!("    since last success  {}\n", humanize(since))),
        }
        if job.failures_in_a_row > 0 {
            out.push_str(&format!(
                "    ⚠ {} failure(s) in a row\n",
                job.failures_in_a_row
            ));
        }
        if !job.last_output.is_empty() {
            out.push_str(&format!("    last                {}\n", job.last_output));
        }
        out
    }

    /// When the timer's next run is due, against the server's clock — or that one is running.
    fn next_line(&self) -> String {
        match self.backups.next.due_ms() {
            Some(due) => {
                let now = self.backups.next.now_ms();
                let when = if due >= now {
                    format!("in {}", humanize(Duration::from_millis(due - now)))
                } else {
                    format!("⚠ {} OVERDUE", humanize(Duration::from_millis(now - due)))
                };
                format!("    next run            {} ({when})\n", stamp_iso(due))
            }
            None => "    next run            — a run is in progress\n".to_string(),
        }
    }

    /// The JSON face: the newest sidecar verbatim under `newest`, the rotation set, and the
    /// timer's health. The sidecar is passed through rather than re-rendered so that what a
    /// query reads is exactly what the backup recorded.
    fn json(&self) -> String {
        let settings = &self.backups.settings;
        let found = archives(&settings.dir);
        let newest = found
            .last()
            .and_then(|a| a.meta.clone())
            .unwrap_or_else(|| "null".to_string());
        let rotation: Vec<String> = found
            .iter()
            .map(|a| format!("{{\"name\":{},\"bytes\":{}}}", quote(&a.name), a.bytes))
            .collect();
        let schedule = match settings.every {
            Some(every) => quote(&humanize(every)),
            None => "null".to_string(),
        };
        let timer = match &self.backups.jobs {
            None => "null".to_string(),
            Some(jobs) => match jobs.health().into_iter().find(|job| job.target == BACKUP) {
                None => "null".to_string(),
                Some(job) => format!(
                    "{{\"id\":{},\"runs\":{},\"interval_seconds\":{},\"next_run_due\":{},\
                     \"seconds_since_last_run\":{},\"seconds_since_last_success\":{},\
                     \"failures_in_a_row\":{},\"last_output\":{}}}",
                    job.id,
                    job.runs,
                    job.interval.as_secs(),
                    self.backups
                        .next
                        .due_ms()
                        .map(|due| quote(&stamp_iso(due)))
                        .unwrap_or_else(|| "null".to_string()),
                    job.since_last
                        .map(|d| d.as_secs().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    job.since_last_success
                        .map(|d| d.as_secs().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    job.failures_in_a_row,
                    quote(&job.last_output),
                ),
            },
        };
        format!(
            "{{\"directory\":{},\"keep\":{},\"schedule\":{},\"newest\":{},\
             \"rotation\":[{}],\"timer\":{}}}\n",
            quote(&settings.dir.display().to_string()),
            settings.keep,
            schedule,
            newest,
            rotation.join(","),
            timer
        )
    }
}

// ---------------------------------------------------------------- reading one back

struct ReadArchive {
    backups: Backups,
}

#[async_trait]
impl Endpoint for ReadArchive {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv
            .bindings
            .get("name")
            .ok_or_else(|| Error::MissingArgument("name".to_string()))?;
        match inv.request.verb {
            Verb::Exists => {
                let there = archive_path(&self.backups.settings.dir, name)
                    .map(|path| path.is_file())
                    .unwrap_or(false);
                Ok(plain(format!("{there}\n")))
            }
            Verb::Source => {
                let path = archive_path(&self.backups.settings.dir, name)?;
                let bytes = std::fs::read(&path).map_err(|e| {
                    Error::NotFound(format!(
                        "no archive `{name}` in {}: {e}",
                        self.backups.settings.dir.display()
                    ))
                })?;
                Ok(Representation::new(ReprType::new(GZIP), bytes))
            }
            other => Err(unsupported("gonk-backup-archive", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup-archive"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup-archive")
            .title("One archive's bytes, by name")
            .summary(
                "Reads one archive out of this server's backup directory as \
                 `application/gzip`. This is what makes a backup reachable at all without \
                 linking a filesystem family into a server that holds the dataset: pipe it \
                 into `urn:iki:gonk:restore` to verify it, or off the machine to have a \
                 copy somewhere the disk holding the data is not. `{name}` is a bare file \
                 name in that directory — `urn:iki:gonk:backup:status` lists them.",
            )
            .verb(Verb::Source)
            .verb(Verb::Exists)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .input(
                ArgSpec::new("name")
                    .summary("the archive's file name, as `urn:iki:gonk:backup:status` lists it")
                    .class(XSD_STRING)
                    .binding(),
            )
            .output(GZIP)
            // `Exists` answers `true`/`false` as text, which is a second face and therefore
            // a second declaration: a face the manifold does not announce is one no caller
            // can plan for.
            .output("text/plain")
    }
}

// ---------------------------------------------------------------- restoring

struct Restore {
    backups: Backups,
}

#[async_trait]
impl Endpoint for Restore {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Sink => self.restore(inv).await,
            other => Err(unsupported("gonk-restore", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-restore"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-restore")
            .title("Load a backup into a NEW store directory, and verify it")
            .summary(
                "Expands the piped archive (gzip or plain N-Quads), opens a store at \
                 `into`, loads every quad, and then compares the restored dataset's GRAPH \
                 SET and PER-GRAPH counts against the archive — not just a total, because a \
                 total is exactly what a collapsed-to-one-graph dump preserves. `into` must \
                 not exist or must be empty, and may not be the live store: a merge into a \
                 non-empty dataset is a UNION, which resurrects deleted items and can never \
                 reproduce the state that was backed up. Swapping the restored directory \
                 into place is an operator's act with this server stopped.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .requires(CAP_RESTORE)
            .input(
                ArgSpec::new("content")
                    .summary("the archive: a gzip stream or plain N-Quads; filled from the pipe")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("into")
                    .summary(
                        "the NEW store directory to build — absent or empty, never the live one",
                    )
                    .class(XSD_STRING),
            )
            .output("text/plain")
    }
}

impl Restore {
    async fn restore(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let into = PathBuf::from(inv.inline_str("into")?);
        self.check_target(&into)?;
        let content = inv.inline_arg("content")?;
        let (nquads, how) = if content.starts_with(&[0x1f, 0x8b]) {
            let expanded = inv
                .issue(
                    Request::new(Verb::Source, iri("urn:decompress:gzip")?)
                        .with_arg("content", ArgRef::Inline(content.to_vec()))
                        .with_arg("as", ArgRef::Inline(N_QUADS.as_bytes().to_vec()))
                        .with_arg(
                            "max-bytes",
                            ArgRef::Inline(RESTORE_MAX_BYTES.to_string().into_bytes()),
                        ),
                )
                .await?;
            (expanded.bytes, "gzip")
        } else {
            (content.to_vec(), "plain n-quads")
        };
        let expected = blank_graphs_by_shape(counts_in_nquads(&nquads)?);
        let expected_total: u64 = expected.values().sum();

        std::fs::create_dir_all(&into)
            .map_err(|e| Error::Endpoint(format!("creating {}: {e}", into.display())))?;
        let store = ikigai_store::DurableStore::open(&into)
            .map_err(|e| Error::Endpoint(format!("opening a store at {}: {e}", into.display())))?;
        // A kernel of its own over the NEW store. The restore goes through
        // `urn:iki:store:load` — the same door a bulk load comes in by anywhere else —
        // rather than through a private back entrance, and it runs under exactly the write
        // token that door enforces.
        let restored = Kernel::new(Arc::new(ikigai_store::space(store)));
        let write = Capability::scoped([ikigai_store::CAP_WRITE]);
        restored
            .issue(
                Request::new(Verb::Sink, iri("urn:iki:store:load")?)
                    .with_arg("content", ArgRef::Inline(nquads.clone()))
                    .with_arg("format", ArgRef::Inline(N_QUADS.as_bytes().to_vec())),
                &write,
            )
            .await?;

        // ★ The verifier's count is a whole-dataset query too, so it holds the backup's
        // budget grant (ledger #979): on the restored store's 5 s base, a dataset that grew
        // past it would fail the verification of a restore that succeeded. Measured
        // 2026-10-09: the whole restore of the live dataset, count included, took 2.9 s.
        // It needs no ANSWER grant (ledger #993): its answer is one row per graph, far
        // inside the store's base of 100,000 rows and 16 MiB.
        let read = Capability::scoped([ikigai_store::CAP_READ, JOB_BUDGET]);
        let actual = blank_graphs_by_shape(graph_counts(&restored, &read).await?);
        let actual_total: u64 = actual.values().sum();
        // Drop the store so the RocksDB lock is released before the caller is told the
        // directory is ready to be moved into place.
        drop(restored);

        let mut differences: Vec<String> = Vec::new();
        for (graph, count) in &expected {
            match actual.get(graph) {
                Some(got) if got == count => {}
                Some(got) => {
                    differences.push(format!("{}: archive {count}, restored {got}", shown(graph)))
                }
                None => differences.push(format!("{}: in the archive, MISSING", shown(graph))),
            }
        }
        for graph in actual.keys() {
            if !expected.contains_key(graph) {
                differences.push(format!("{}: restored, NOT in the archive", shown(graph)));
            }
        }
        if !differences.is_empty() {
            return Err(Error::Endpoint(format!(
                "the restored dataset does not match the archive:\n  {}\n  (archive \
                 {expected_total} quads in {} graph(s); restored {actual_total} in {})",
                differences.join("\n  "),
                expected.len(),
                actual.len()
            )));
        }

        let mut report = format!(
            "restored {expected_total} quads into {} ({how}, sha256 {})\n",
            into.display(),
            digest(content)
        );
        for (graph, count) in &actual {
            report.push_str(&format!("  {count:>8}  {}\n", shown(graph)));
        }
        report.push_str(
            "  graph set and per-graph counts match the archive exactly\n  this server still \
             holds the LIVE store; to adopt this one, stop gonk and swap the directories\n",
        );
        Ok(plain(report))
    }

    /// `into` is not the live store, and is not a directory that already holds something.
    fn check_target(&self, into: &Path) -> Result<()> {
        let live = &self.backups.settings.store_path;
        let same = into == live
            || match (std::fs::canonicalize(into), std::fs::canonicalize(live)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
        if same {
            return Err(Error::InvalidArgument {
                name: "into".to_string(),
                detail: format!(
                    "{} is the LIVE store this server holds. A restore replaces; loading an \
                     archive over a live dataset merges, which resurrects deleted items and \
                     can never reproduce the backed-up state. Restore into a new directory \
                     and swap it in with the server stopped.",
                    live.display()
                ),
            });
        }
        if into.is_dir() {
            let empty = std::fs::read_dir(into)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return Err(Error::InvalidArgument {
                    name: "into".to_string(),
                    detail: format!(
                        "{} is not empty. Restoring into a non-empty store is a MERGE, not a \
                         restore: the union of two datasets brings back everything either of \
                         them deleted. Name a directory that does not exist yet.",
                        into.display()
                    ),
                });
            }
        } else if into.exists() {
            return Err(Error::InvalidArgument {
                name: "into".to_string(),
                detail: format!("{} exists and is not a directory", into.display()),
            });
        }
        Ok(())
    }
}

/// The per-graph counts of a dataset, read through `urn:iki:store:select`.
async fn graph_counts(kernel: &Kernel, capability: &Capability) -> Result<BTreeMap<String, u64>> {
    let query = "SELECT ?g (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } UNION \
                 { ?s ?p ?o } } GROUP BY ?g";
    let results = kernel
        .issue(
            Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                .with_arg(
                    "as",
                    ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                ),
            capability,
        )
        .await?;
    let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
    let Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) =
        parser.for_reader(results.bytes.as_slice())
    else {
        return Err(Error::Endpoint(
            "counting the restored graphs: not a solution set".into(),
        ));
    };
    let mut counts = BTreeMap::new();
    for solution in solutions {
        let solution = solution.map_err(|e| Error::Endpoint(format!("counting: {e}")))?;
        let key = match solution.get("g") {
            None => DEFAULT_GRAPH_KEY.to_string(),
            Some(Term::NamedNode(node)) => node.as_str().to_string(),
            Some(Term::BlankNode(node)) => format!("_:{}", node.as_str()),
            other => {
                return Err(Error::Endpoint(format!(
                    "a graph that is not an IRI: {other:?}"
                )))
            }
        };
        let Some(Term::Literal(literal)) = solution.get("n") else {
            return Err(Error::Endpoint("a count that is not a literal".into()));
        };
        let count: u64 = literal
            .value()
            .parse()
            .map_err(|_| Error::Endpoint("a count that is not a number".into()))?;
        counts.insert(key, count);
    }
    Ok(counts)
}

/// Re-key blank-node graphs by what survives a load: their number and their sizes.
///
/// A blank-node graph label is local to the document that wrote it, and the store mints its
/// own on load, so a comparison keyed by label could never match (ledger #737). Each side's
/// blank graphs are renamed `_:blank-graph-1…n` in order of size, which makes the per-graph
/// comparison a comparison of the two multisets of sizes; named and default graphs are
/// untouched.
fn blank_graphs_by_shape(counts: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut named = BTreeMap::new();
    let mut sizes: Vec<u64> = Vec::new();
    for (graph, count) in counts {
        if graph.starts_with("_:") {
            sizes.push(count);
        } else {
            named.insert(graph, count);
        }
    }
    sizes.sort_unstable();
    for (n, count) in sizes.into_iter().enumerate() {
        named.insert(format!("_:blank-graph-{}", n + 1), count);
    }
    named
}

// ---------------------------------------------------------------- the scripts

/// The scripts archive taken with the store archive `store_archive`, by name, or `None` for a
/// name that is not one of this server's store archives.
///
/// ```
/// use ikigai_gonk::backup::scripts_archive_for;
/// assert_eq!(
///     scripts_archive_for("gonk-store-2026-10-10T120000Z.nq.gz").as_deref(),
///     Some("gonk-scripts-2026-10-10T120000Z.json.gz")
/// );
/// assert_eq!(scripts_archive_for("export.nq.gz"), None);
/// ```
pub fn scripts_archive_for(store_archive: &str) -> Option<String> {
    let stamp = store_archive
        .strip_prefix("gonk-store-")?
        .strip_suffix(ARCHIVE_SUFFIX)?;
    Some(format!("{SCRIPTS_PREFIX}{stamp}{SCRIPTS_SUFFIX}"))
}

/// One file of `ikigai-script`'s `DirBackend` layout, by its place in it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptFile {
    /// `<name>/head.json`.
    Head,
    /// `<name>/versions/<64 hex>.json`: the version `sha256:<hex>`.
    Version(String),
    /// `<name>/runs/<id>.json`.
    Run(u64),
}

/// Where `path` (relative, `/`-separated) sits in the scripts layout, or `None` for anything
/// else — a temporary (`*.json.tmp`), an editor's backup, a name that is no script's, a `..`.
///
/// ★ This is the ONLY gate between a caller's archive and the file system on a restore: every
/// path a snapshot holds must be one of the three shapes, each segment checked, so no path can
/// name a file outside `<into>/<script name>/`.
///
/// ```
/// use ikigai_gonk::backup::is_script_file;
/// let hex = "a".repeat(64);
/// assert!(is_script_file("triage/head.json"));
/// assert!(is_script_file(&format!("triage/versions/{hex}.json")));
/// assert!(is_script_file("triage/runs/12.json"));
/// assert!(!is_script_file("triage/head.json.tmp"));
/// assert!(!is_script_file("../triage/head.json"));
/// assert!(!is_script_file("Triage/head.json"));
/// assert!(!is_script_file("triage/runs/012.json"));
/// assert!(!is_script_file("triage/versions/abc.json"));
/// assert!(!is_script_file("/etc/passwd"));
/// ```
pub fn is_script_file(path: &str) -> bool {
    script_file(path).is_some()
}

fn script_file(path: &str) -> Option<(String, ScriptFile)> {
    let parts: Vec<&str> = path.split('/').collect();
    let name = parts.first()?;
    ikigai_script::name::validate(name).ok()?;
    let file = match parts.as_slice() {
        [_, "head.json"] => ScriptFile::Head,
        [_, "versions", file] => {
            let hex = file.strip_suffix(".json")?;
            let ok = hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !ok {
                return None;
            }
            ScriptFile::Version(hex.to_string())
        }
        [_, "runs", file] => {
            let stem = file.strip_suffix(".json")?;
            let id: u64 = stem.parse().ok()?;
            if id.to_string() != stem {
                return None;
            }
            ScriptFile::Run(id)
        }
        _ => return None,
    };
    Some((name.to_string(), file))
}

/// The scripts directory as one JSON document, with what it holds.
struct ScriptsSnapshot {
    json: Vec<u8>,
    /// Scripts with a head.
    names: usize,
    /// Files in the document.
    files: usize,
}

/// Every file of the scripts layout under `dir`, as `{"format": …, "files": {path: text}}`,
/// paths sorted, so an unchanged directory gives identical bytes and the digest says so.
///
/// Per script the head is read BEFORE its versions: `ikigai-script` writes a version before it
/// points a head at it, and never deletes one, so every version a head read here names is on
/// disk by the time the versions are listed. Every write there is a rename, so a file is read
/// whole or not at all. A file outside the layout (a temporary, an editor's backup) is left
/// out; a file inside it that cannot be read fails the backup by name.
fn snapshot_scripts(dir: &Path) -> Result<ScriptsSnapshot> {
    let failed = |path: &Path, e: std::io::Error| {
        Error::Endpoint(format!(
            "backing up the scripts: reading {}: {e}",
            path.display()
        ))
    };
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut names = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // Never created: no script was ever published. An empty snapshot says exactly that.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScriptsSnapshot {
                json: scripts_document(&files),
                names: 0,
                files: 0,
            })
        }
        Err(e) => return Err(failed(dir, e)),
    };
    let mut scripts: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| ikigai_script::name::validate(name).is_ok())
        .collect();
    scripts.sort();
    for name in scripts {
        let head = dir.join(&name).join("head.json");
        match std::fs::read_to_string(&head) {
            Ok(text) => {
                files.insert(format!("{name}/head.json"), text);
                names += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(&head, e)),
        }
        for sub in ["versions", "runs"] {
            let place = dir.join(&name).join(sub);
            let listed = match std::fs::read_dir(&place) {
                Ok(listed) => listed,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(failed(&place, e)),
            };
            for entry in listed.flatten() {
                let Some(file) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let relative = format!("{name}/{sub}/{file}");
                if !is_script_file(&relative) {
                    continue;
                }
                let text =
                    std::fs::read_to_string(entry.path()).map_err(|e| failed(&entry.path(), e))?;
                files.insert(relative, text);
            }
        }
    }
    Ok(ScriptsSnapshot {
        json: scripts_document(&files),
        names,
        files: files.len(),
    })
}

fn scripts_document(files: &BTreeMap<String, String>) -> Vec<u8> {
    let mut document = serde_json::Map::new();
    document.insert("format".to_string(), SCRIPTS_FORMAT.into());
    document.insert(
        "files".to_string(),
        serde_json::Value::Object(
            files
                .iter()
                .map(|(path, text)| (path.clone(), serde_json::Value::from(text.as_str())))
                .collect(),
        ),
    );
    let mut json = serde_json::to_vec(&serde_json::Value::Object(document))
        .expect("a map of strings serializes");
    json.push(b'\n');
    json
}

/// A scripts archive's document, read back: every path checked against the layout.
fn parse_scripts(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let bad = |detail: String| Error::InvalidArgument {
        name: "content".to_string(),
        detail,
    };
    let document: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| bad(format!("not a scripts archive (not JSON): {e}")))?;
    if document["format"].as_str() != Some(SCRIPTS_FORMAT) {
        return Err(bad(format!(
            "not a scripts archive: `format` is not \"{SCRIPTS_FORMAT}\" (a store archive \
             restores at {RESTORE})"
        )));
    }
    let listed = document["files"]
        .as_object()
        .ok_or_else(|| bad("a scripts archive's `files` must be an object".to_string()))?;
    let mut files = BTreeMap::new();
    for (path, text) in listed {
        if !is_script_file(path) {
            return Err(bad(format!(
                "`{path}` is not a file of the scripts layout (`<name>/head.json`, \
                 `<name>/versions/<sha256>.json`, `<name>/runs/<id>.json`); nothing was written"
            )));
        }
        let text = text
            .as_str()
            .ok_or_else(|| bad(format!("`{path}` is not text")))?;
        files.insert(path.clone(), text.to_string());
    }
    Ok(files)
}

struct RestoreScripts {
    backups: Backups,
}

#[async_trait]
impl Endpoint for RestoreScripts {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Sink => self.restore(inv).await,
            other => Err(unsupported("gonk-restore-scripts", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-restore-scripts"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-restore-scripts")
            .title("Write a scripts archive into a NEW directory, and verify it")
            .summary(
                "Expands the piped scripts archive (gzip or plain JSON), checks every path \
                 against the scripts layout, writes the files into `into`, and then reads every \
                 script back through ikigai-script's own DirBackend: each head, every version \
                 it or its history names, every version file (checked against the digest it \
                 is named by) and every run. `into` must not exist or must be empty, and may \
                 not be the live scripts directory. Swapping it into place is an operator's \
                 act with this server stopped.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .requires(CAP_RESTORE)
            .input(
                ArgSpec::new("content")
                    .summary("the scripts archive: gzip or plain JSON; filled from the pipe")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("into")
                    .summary(
                        "the NEW scripts directory to build — absent or empty, never the live one",
                    )
                    .class(XSD_STRING),
            )
            .output("text/plain")
    }
}

impl RestoreScripts {
    async fn restore(&self, inv: &Invocation<'_>) -> Result<Representation> {
        use ikigai_script::Backend as _;
        let into = PathBuf::from(inv.inline_str("into")?);
        self.check_target(&into)?;
        let content = inv.inline_arg("content")?;
        let json = if content.starts_with(&[0x1f, 0x8b]) {
            inv.issue(
                Request::new(Verb::Source, iri("urn:decompress:gzip")?)
                    .with_arg("content", ArgRef::Inline(content.to_vec()))
                    .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()))
                    .with_arg(
                        "max-bytes",
                        ArgRef::Inline(RESTORE_MAX_BYTES.to_string().into_bytes()),
                    ),
            )
            .await?
            .bytes
        } else {
            content.to_vec()
        };
        // Every path is checked before the first byte is written.
        let files = parse_scripts(&json)?;
        for (path, text) in &files {
            let target = into.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::Endpoint(format!("creating {}: {e}", parent.display())))?;
            }
            std::fs::write(&target, text)
                .map_err(|e| Error::Endpoint(format!("writing {}: {e}", target.display())))?;
        }

        // ★ The verification reads through the library, not through what this function wrote:
        // a head the library cannot parse, a version whose bytes do not match its digest, a
        // head naming a version the archive does not hold all fail here, by name.
        let backend = ikigai_script::DirBackend::open(&into)
            .map_err(|e| Error::Endpoint(format!("opening {}: {e}", into.display())))?;
        let mut expected: BTreeMap<String, Vec<ScriptFile>> = BTreeMap::new();
        for path in files.keys() {
            if let Some((name, file)) = script_file(path) {
                expected.entry(name).or_default().push(file);
            }
        }
        let with_heads: Vec<&String> = expected
            .iter()
            .filter(|(_, held)| held.contains(&ScriptFile::Head))
            .map(|(name, _)| name)
            .collect();
        let listed = backend.names()?;
        if listed.iter().collect::<Vec<_>>() != with_heads {
            return Err(Error::Endpoint(format!(
                "the restored directory lists the scripts {listed:?}, the archive holds heads \
                 for {with_heads:?}"
            )));
        }
        let (mut versions, mut runs) = (0, 0);
        let mut problems: Vec<String> = Vec::new();
        for (name, held) in &expected {
            if held.contains(&ScriptFile::Head) {
                match backend.head(name) {
                    Ok(Some(head)) => {
                        let named = std::iter::once(head.version.clone())
                            .chain(head.history.iter().map(|event| event.version.clone()));
                        for digest in named {
                            match backend.version(name, &digest) {
                                Ok(Some(_)) => {}
                                Ok(None) => problems.push(format!(
                                    "{name}: its head names {digest}, which the archive does not hold"
                                )),
                                Err(e) => problems.push(format!("{name}: {digest}: {e}")),
                            }
                        }
                    }
                    Ok(None) => problems.push(format!("{name}: the head did not restore")),
                    Err(e) => problems.push(format!("{name}: {e}")),
                }
            }
            for file in held {
                match file {
                    ScriptFile::Head => {}
                    ScriptFile::Version(hex) => {
                        match backend.version(name, &format!("sha256:{hex}")) {
                            Ok(Some(_)) => versions += 1,
                            Ok(None) => problems.push(format!("{name}: version {hex} is missing")),
                            Err(e) => problems.push(format!("{name}: version {hex}: {e}")),
                        }
                    }
                    ScriptFile::Run(id) => match backend.run(name, *id) {
                        Ok(Some(_)) => runs += 1,
                        Ok(None) => problems.push(format!("{name}: run {id} is missing")),
                        Err(e) => problems.push(format!("{name}: run {id}: {e}")),
                    },
                }
            }
        }
        if !problems.is_empty() {
            return Err(Error::Endpoint(format!(
                "the restored scripts do not verify:\n  {}\n  (the files are in {}; nothing \
                 live was touched)",
                problems.join("\n  "),
                into.display()
            )));
        }
        Ok(plain(format!(
            "restored {} script(s) into {} (sha256 {})\n  {} file(s): {} head(s), {versions} \
             version(s), {runs} run(s), every one read back through the script library\n  this \
             server still holds the LIVE scripts directory; to adopt this one, stop gonk and \
             swap the directories\n",
            with_heads.len(),
            into.display(),
            digest(content),
            files.len(),
            with_heads.len(),
        )))
    }

    /// `into` is not the live scripts directory, and is not a directory that holds something.
    fn check_target(&self, into: &Path) -> Result<()> {
        if let Some(live) = &self.backups.settings.scripts {
            let same = into == live
                || match (std::fs::canonicalize(into), std::fs::canonicalize(live)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                };
            if same {
                return Err(Error::InvalidArgument {
                    name: "into".to_string(),
                    detail: format!(
                        "{} is the LIVE scripts directory this server holds. Restore into a new \
                         directory and swap it in with the server stopped.",
                        live.display()
                    ),
                });
            }
        }
        if into.is_dir() {
            let empty = std::fs::read_dir(into)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return Err(Error::InvalidArgument {
                    name: "into".to_string(),
                    detail: format!(
                        "{} is not empty. A restore into a directory that holds scripts would \
                         mix two histories; name a directory that does not exist yet.",
                        into.display()
                    ),
                });
            }
        } else if into.exists() {
            return Err(Error::InvalidArgument {
                name: "into".to_string(),
                detail: format!("{} exists and is not a directory", into.display()),
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- odds and ends

/// How a graph name reads in a report.
fn shown(graph: &str) -> &str {
    if graph.is_empty() {
        "(default graph)"
    } else {
        graph
    }
}

fn iri(text: &str) -> Result<Iri> {
    Iri::parse(text).map_err(|e| Error::Endpoint(format!("`{text}` is not an IRI: {e}")))
}

fn plain(text: String) -> Representation {
    Representation::new(
        ReprType::new("text/plain").with_param("charset", "utf-8"),
        text.into_bytes(),
    )
}

fn unsupported(id: &str, verb: Verb) -> Error {
    Error::Endpoint(format!("{id} does not support the {verb:?} verb"))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A duration as an operator reads it.
fn humanize(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 => "less than a second".to_string(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// The fields a sidecar records.
struct MetaFields<'a> {
    archive: &'a str,
    taken_at: &'a str,
    taken_at_ms: u64,
    quads: u64,
    graphs: &'a BTreeMap<String, u64>,
    nquads_bytes: usize,
    nquads_sha256: &'a str,
    archive_bytes: usize,
    archive_sha256: &'a str,
    store_quads_after_dump: u64,
    scripts: Option<ScriptsMeta<'a>>,
}

/// What a sidecar records of the scripts archive taken with the store's.
struct ScriptsMeta<'a> {
    archive: &'a str,
    names: usize,
    files: usize,
    json_bytes: usize,
    json_sha256: String,
    archive_bytes: usize,
    archive_sha256: String,
}

fn meta_json(fields: &MetaFields<'_>) -> String {
    let graphs: Vec<String> = fields
        .graphs
        .iter()
        .map(|(graph, count)| format!("{}:{count}", quote(graph)))
        .collect();
    // Flat keys, after the store's, so `field` reads each one by name.
    let scripts = match &fields.scripts {
        Some(scripts) => format!(
            ",\"scripts_archive\":{},\"scripts_names\":{},\"scripts_files\":{},\
             \"scripts_json_bytes\":{},\"scripts_json_sha256\":{},\
             \"scripts_archive_bytes\":{},\"scripts_archive_sha256\":{}",
            quote(scripts.archive),
            scripts.names,
            scripts.files,
            scripts.json_bytes,
            quote(&scripts.json_sha256),
            scripts.archive_bytes,
            quote(&scripts.archive_sha256),
        ),
        None => String::new(),
    };
    format!(
        "{{\"archive\":{},\"taken_at\":{},\"taken_at_ms\":{},\"quads\":{},\
         \"graphs\":{{{}}},\"nquads_bytes\":{},\"nquads_sha256\":{},\
         \"archive_bytes\":{},\"archive_sha256\":{},\"store_quads_after_dump\":{}{}}}\n",
        quote(fields.archive),
        quote(fields.taken_at),
        fields.taken_at_ms,
        fields.quads,
        graphs.join(","),
        fields.nquads_bytes,
        quote(fields.nquads_sha256),
        fields.archive_bytes,
        quote(fields.archive_sha256),
        fields.store_quads_after_dump,
        scripts,
    )
}

/// A JSON string literal. The only values that reach it are IRIs, file names, digests and
/// one line of a job's last output, but the last of those is an error message from
/// anywhere, so it is escaped properly rather than trusted to be tame.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One scalar field out of a sidecar, by name — enough to render a status line without
/// taking a JSON parser as a dependency for four keys this module wrote itself.
///
/// ⚠ It reads gonk's OWN sidecars, which this module is the only writer of. It is not a
/// JSON parser and must not be pointed at anyone else's document.
fn field(meta: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let rest = &meta[meta.find(&needle)? + needle.len()..];
    let rest = rest.trim_start();
    if let Some(body) = rest.strip_prefix('"') {
        let end = body.find('"')?;
        Some(body[..end].to_string())
    } else {
        let end = rest.find([',', '}'])?;
        Some(rest[..end].trim().to_string())
    }
}

/// The `graphs` object of a sidecar, rendered as one line.
fn graph_line(meta: &str) -> String {
    let Some(start) = meta.find("\"graphs\":{") else {
        return "(not recorded)".to_string();
    };
    let body = &meta[start + "\"graphs\":{".len()..];
    let Some(end) = body.find('}') else {
        return "(not recorded)".to_string();
    };
    let body = &body[..end];
    if body.is_empty() {
        return "(none)".to_string();
    }
    body.replace("\":", "\" ").replace(',', ", ")
}

// ---------------------------------------------------------------- the clock

/// `2026-09-16T172813Z` — a file name that sorts chronologically and has no colons in it,
/// because a colon in a file name is a path separator on one platform and an escape on
/// several tools.
pub fn stamp_compact(millis: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(millis);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `2026-09-16T17:28:13Z` — the same instant as an operator and an `xsd:dateTime` read it.
pub fn stamp_iso(millis: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(millis);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `2026-09-16T17:28:13.042Z` — [`stamp_iso`] with its milliseconds, the RFC 3339 instant an
/// `ikigai-log` line opens with (and the access log's first column, [`crate::access`]).
pub fn stamp_iso_ms(millis: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(millis);
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}Z",
        millis % 1000
    )
}

/// Milliseconds since the epoch → UTC civil time.
///
/// ⚠ **This is the THIRD copy of Howard Hinnant's `civil_from_days` in the ecosystem**
/// (`ikigai-ledger`'s `sparql.rs` and `ikigai-log`'s `line.rs` hold the others), and it is
/// a copy rather than a shared seam on purpose: three correct copies of a closed-form
/// algorithm with no drift is count without drift, not duplication that justifies an API.
/// What would justify one is a branch none of them has taken. Reported to the hub instead
/// of fixed sideways.
fn civil(millis: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (millis / 86_400_000) as i64;
    let rest = millis % 86_400_000;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        m,
        d,
        rest / 3_600_000,
        (rest % 3_600_000) / 60_000,
        (rest % 60_000) / 1000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The literal grant is the store's own spelling of [`JOB_BUDGET_MS`], and the job's scope
    /// list holds it (ledger #979).
    /// ★ Ledger #1087's numbers: an archive at 12:27, a restart at 23:21, a 1-day cadence.
    #[test]
    fn the_first_backup_is_due_one_cadence_after_the_newest_archive() {
        let day = Duration::from_secs(86_400);
        let archive = 1_760_012_820_000; // 2026-10-09T12:27:00Z
        let restart = 1_760_052_060_000; // 2026-10-09T23:21:00Z
        assert_eq!(
            first_delay(restart, Some(archive), day),
            Duration::from_millis(archive + 86_400_000 - restart),
            "13h06m, not the 24h a timer started at the restart waits"
        );
        // Overdue or never taken: a catch-up, never sooner than CATCH_UP.
        assert_eq!(
            first_delay(restart, Some(restart - 90_000_000), day),
            CATCH_UP
        );
        assert_eq!(first_delay(restart, None, day), CATCH_UP);
        // Due in ten seconds: still a minute out, so a restart loop takes one per minute at most.
        assert_eq!(
            first_delay(restart, Some(restart - 86_390_000), day),
            CATCH_UP
        );
        // An archive stamped in the future (a clock stepped back) waits one cadence, no more.
        assert_eq!(
            first_delay(restart, Some(restart + 5 * 86_400_000), day),
            day
        );
    }

    /// The timer's first tick comes after `first`, not after the interval, and it records
    /// when each next tick is due on the clock it was given.
    #[test]
    fn the_backup_timer_fires_first_after_its_phase_and_records_the_next_run() {
        use ikigai_time::TimerBackend;
        let clock = Arc::new(ikigai_core::FixedClock::at(1_000_000));
        let next = NextRun::with_clock(clock);
        let timer = BackupTimer::new(Duration::from_millis(20), next.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = timer.start(
            Duration::from_secs(3_600),
            true,
            Arc::new(move || {
                let _ = tx.send(());
            }),
        );
        assert_eq!(
            next.due_ms(),
            Some(1_000_020),
            "armed for its phase, not the interval"
        );
        rx.recv_timeout(Duration::from_secs(30))
            .expect("the first tick comes after 20 ms, not after an hour");
        // Re-armed for one interval after the tick (the clock is fixed, so exactly this).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while next.due_ms() != Some(1_000_000 + 3_600_000) {
            assert!(std::time::Instant::now() < deadline, "{:?}", next.due_ms());
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.cancel();
    }

    #[test]
    fn the_job_budget_grant_is_the_stores_spelling_of_its_milliseconds() {
        assert_eq!(JOB_BUDGET, ikigai_store::budget::cap_budget(JOB_BUDGET_MS));
        assert!(JOB_SCOPES.contains(&JOB_BUDGET));
        let job = Capability::scoped(JOB_SCOPES);
        assert_eq!(
            ikigai_store::budget::TimeBudget::default().for_capability(&job),
            Duration::from_millis(JOB_BUDGET_MS),
            "the store gives the job exactly its grant: at or below the default ceiling"
        );
    }

    /// The two answer grants are the store's own spellings of their bounds, the job's scope
    /// list holds both, and under the store's DEFAULT answer budget they lift the job to the
    /// ceiling on rows AND bytes (ledger #993). The store's base is the bound a job without
    /// them gets, which the dataset is past on both.
    #[test]
    fn the_job_answer_grants_are_the_stores_spellings_and_reach_the_ceiling() {
        use ikigai_store::budget::{cap_answer, cap_answer_bytes, AnswerBound, AnswerBudget};
        assert_eq!(JOB_ANSWER_ROWS, cap_answer(JOB_ANSWER_ROWS_MAX));
        assert_eq!(JOB_ANSWER_BYTES, cap_answer_bytes(JOB_ANSWER_BYTES_MAX));
        assert!(JOB_SCOPES.contains(&JOB_ANSWER_ROWS));
        assert!(JOB_SCOPES.contains(&JOB_ANSWER_BYTES));
        let store = AnswerBudget::default();
        assert_eq!(
            store.for_capability(&Capability::scoped(JOB_SCOPES)),
            AnswerBound::CEILING,
            "the job gets the store's ceiling on both measures"
        );
        assert_eq!(
            AnswerBound::new(JOB_ANSWER_ROWS_MAX, JOB_ANSWER_BYTES_MAX).ok(),
            Some(AnswerBound::CEILING),
            "the grants ask for exactly the ceiling: no more, which the store would clamp"
        );
        let without = |dropped: &str| {
            Capability::scoped(JOB_SCOPES.iter().copied().filter(|s| *s != dropped))
        };
        assert_eq!(
            store.for_capability(&without(JOB_ANSWER_BYTES)).bytes(),
            store.base().bytes(),
            "a row grant alone leaves the byte bound at the base"
        );
        assert_eq!(
            store.for_capability(&without(JOB_ANSWER_ROWS)).rows(),
            store.base().rows(),
            "a byte grant alone leaves the row bound at the base"
        );
    }

    /// ★ The stamp is a file name an operator reads and a sort key the rotation depends
    /// on, so it is pinned at literals rather than round-tripped against itself.
    #[test]
    fn a_stamp_is_the_instant_it_claims() {
        // 2026-09-16T17:28:13Z — the instant the hub's stopgap archive was taken.
        let millis = 1_789_579_693_000;
        assert_eq!(stamp_iso(millis), "2026-09-16T17:28:13Z");
        assert_eq!(stamp_compact(millis), "2026-09-16T172813Z");
        assert_eq!(stamp_iso(0), "1970-01-01T00:00:00Z");
        // A leap day, which is where a civil-time algorithm goes wrong if it is going to.
        assert_eq!(stamp_iso(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }

    /// Archive names sort chronologically as plain strings — which is what makes
    /// "prune the oldest" a sort rather than a stat of every file's mtime.
    #[test]
    fn archive_names_sort_by_time() {
        let mut names = [
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_789_579_693_000)
            ),
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_700_000_000_000)
            ),
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_789_666_093_000)
            ),
        ];
        names.sort();
        assert_eq!(names[0], "gonk-store-2023-11-14T221320Z.nq.gz");
        assert_eq!(names[2], "gonk-store-2026-09-17T172813Z.nq.gz");
    }

    /// The rotation's notion of "ours" is exactly the name [`TakeBackup`] mints.
    #[test]
    fn only_the_names_this_server_mints_are_its_archives() {
        let minted = format!(
            "gonk-store-{}{ARCHIVE_SUFFIX}",
            stamp_compact(1_789_579_693_000)
        );
        assert!(is_own_archive(&minted), "{minted}");
        for foreign in [
            "a-export.nq.gz",
            "zz-export.nq.gz",
            "gonk-store-latest.nq.gz",
            "gonk-store-2026-09-16T172813Z.nq",
            "gonk-store-2026-09-16T172813Z.copy.nq.gz",
            "old-gonk-store-2026-09-16T172813Z.nq.gz",
            "gonk-store-2026-09-16T17281Z.nq.gz",
            "gonk-store-2026-09-16 172813Z.nq.gz",
            "gonk-store-2026-09-16T172813Z.nq.gz.meta.json",
        ] {
            assert!(!is_own_archive(foreign), "{foreign}");
        }
    }

    /// The archive just written stays, even when every other one sorts after it.
    #[test]
    fn pruning_never_takes_the_archive_just_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let taken = format!("gonk-store-2026-10-03T000000Z{ARCHIVE_SUFFIX}");
        for stamp in ["2099-01-01T000000Z", "2099-01-02T000000Z"] {
            let name = format!("gonk-store-{stamp}{ARCHIVE_SUFFIX}");
            std::fs::write(dir.path().join(&name), b"x").unwrap();
        }
        std::fs::write(dir.path().join(&taken), b"x").unwrap();
        let gone = prune(dir.path(), 2, &taken);
        assert_eq!(gone, ["gonk-store-2099-01-01T000000Z.nq.gz"]);
        assert!(dir.path().join(&taken).exists());
        assert_eq!(archives(dir.path()).len(), 2);
    }

    /// ★ `{name}` comes from a caller and names a file this endpoint reads. Every escape
    /// out of the backup directory is refused, not sanitized.
    #[test]
    fn an_archive_name_cannot_leave_the_backup_directory() {
        let dir = Path::new("/tmp/backups");
        assert!(archive_path(dir, "../../etc/passwd").is_err());
        assert!(archive_path(dir, "a/b.nq.gz").is_err());
        assert!(archive_path(dir, "..nq.gz").is_err(), "contains ..");
        assert!(archive_path(dir, "").is_err());
        assert!(archive_path(dir, "notes.txt").is_err(), "not an archive");
        assert_eq!(
            archive_path(dir, "gonk-store-2026-09-16T172813Z.nq.gz").unwrap(),
            dir.join("gonk-store-2026-09-16T172813Z.nq.gz")
        );
    }

    #[test]
    fn a_sidecar_reads_back_the_fields_it_wrote() {
        let mut graphs = BTreeMap::new();
        graphs.insert("urn:iki:ledger:graph:default".to_string(), 2850u64);
        graphs.insert(DEFAULT_GRAPH_KEY.to_string(), 0u64);
        let json = meta_json(&MetaFields {
            archive: "gonk-store-2026-09-16T172813Z.nq.gz",
            taken_at: "2026-09-16T17:28:13Z",
            taken_at_ms: 1_789_579_693_000,
            quads: 2850,
            graphs: &graphs,
            nquads_bytes: 784_085,
            nquads_sha256: "abc",
            archive_bytes: 91_234,
            archive_sha256: "def",
            store_quads_after_dump: 2850,
            scripts: Some(ScriptsMeta {
                archive: "gonk-scripts-2026-09-16T172813Z.json.gz",
                names: 3,
                files: 11,
                json_bytes: 4_096,
                json_sha256: "fed".to_string(),
                archive_bytes: 1_024,
                archive_sha256: "cba".to_string(),
            }),
        });
        assert_eq!(field(&json, "quads").as_deref(), Some("2850"));
        assert_eq!(
            field(&json, "taken_at").as_deref(),
            Some("2026-09-16T17:28:13Z")
        );
        assert_eq!(field(&json, "nquads_sha256").as_deref(), Some("abc"));
        assert_eq!(field(&json, "archive_bytes").as_deref(), Some("91234"));
        assert!(graph_line(&json).contains("urn:iki:ledger:graph:default"));
        // The scripts' keys sit after the store's and never shadow them.
        assert_eq!(
            field(&json, "archive").as_deref(),
            Some("gonk-store-2026-09-16T172813Z.nq.gz")
        );
        assert_eq!(
            field(&json, "scripts_archive").as_deref(),
            Some("gonk-scripts-2026-09-16T172813Z.json.gz")
        );
        assert_eq!(field(&json, "scripts_files").as_deref(), Some("11"));
        assert_eq!(
            field(&json, "scripts_archive_bytes").as_deref(),
            Some("1024")
        );
        serde_json::from_str::<serde_json::Value>(&json).expect("a sidecar is JSON");
    }

    /// A ledger body carries newlines, quotation marks and trailing periods; a JSON writer
    /// that did not escape them would write a sidecar nothing could read.
    #[test]
    fn a_json_string_escapes_what_it_must() {
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
    }

    /// ★ The N-Quads writer is oxigraph's, and this is the row it exists for: a literal
    /// holding a quotation mark, a newline and a trailing period, in a named graph.
    #[test]
    fn a_literal_with_a_trailing_period_survives_the_dump() {
        let results = br#"{"head":{"vars":["g","s","p","o"]},"results":{"bindings":[
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:default"},
             "s":{"type":"uri","value":"urn:iki:ledger:default:item:1"},
             "p":{"type":"uri","value":"urn:ik:body"},
             "o":{"type":"literal","value":"he said \"stop\".\nThen ."}},
            {"s":{"type":"uri","value":"urn:a"},
             "p":{"type":"uri","value":"urn:b"},
             "o":{"type":"uri","value":"urn:c"}}]}}"#;
        let dump = quads_from_results(results).expect("parses");
        assert_eq!(dump.quads(), 2);
        assert_eq!(dump.graphs["urn:iki:ledger:graph:default"], 1);
        assert_eq!(dump.graphs[DEFAULT_GRAPH_KEY], 1, "the default-graph row");
        // The round trip is the assertion: re-reading the bytes gives the same shape back.
        let back = counts_in_nquads(&dump.nquads).expect("re-parses");
        assert_eq!(back, dump.graphs);
        let text = String::from_utf8(dump.nquads.clone()).unwrap();
        assert_eq!(text.lines().count(), 2, "one line per quad: {text}");
        assert!(
            text.contains("<urn:iki:ledger:graph:default> ."),
            "the named-graph row is graph-qualified: {text}"
        );
    }

    /// The whole point of the format, as one test: a graph-qualified dump keeps the
    /// partition, and the count alone would not have noticed if it had not.
    #[test]
    fn the_dump_keeps_the_graph_partition_that_a_triple_format_would_collapse() {
        let results = br#"{"head":{"vars":["g","s","p","o"]},"results":{"bindings":[
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:a"},
             "s":{"type":"uri","value":"urn:x"},"p":{"type":"uri","value":"urn:p"},
             "o":{"type":"uri","value":"urn:y"}},
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:b"},
             "s":{"type":"uri","value":"urn:x"},"p":{"type":"uri","value":"urn:p"},
             "o":{"type":"uri","value":"urn:y"}}]}}"#;
        let dump = quads_from_results(results).expect("parses");
        assert_eq!(dump.quads(), 2);
        assert_eq!(dump.graphs.len(), 2, "two tenants, two graphs");
        let back = counts_in_nquads(&dump.nquads).expect("re-parses");
        assert_eq!(back.len(), 2, "and the file still says so");
    }

    /// ★ The temporary is owner-only from the moment it exists, before a byte is in it —
    /// and a stale one a crash left at `0644` is replaced, not reopened with its old mode.
    #[cfg(unix)]
    #[test]
    fn the_temporary_is_owner_only_before_a_byte_is_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join(format!("gonk-store-x{ARCHIVE_SUFFIX}"));
        let stale = target.with_extension("partial");
        std::fs::write(&stale, b"left by a crash").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
        let file = create_private(&stale).expect("created");
        let mode = file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the temporary was {mode:o} before anything was written"
        );
        assert_eq!(
            file.metadata().unwrap().len(),
            0,
            "the stale bytes are gone"
        );
        drop(file);
        write_atomic(&target, b"the archive").expect("written");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!stale.exists(), "the temporary was renamed into place");
    }

    #[test]
    fn pruning_keeps_the_newest_and_takes_the_sidecar_with_the_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        for stamp in [
            "2026-09-10T000000Z",
            "2026-09-11T000000Z",
            "2026-09-12T000000Z",
        ] {
            let name = format!("gonk-store-{stamp}{ARCHIVE_SUFFIX}");
            std::fs::write(dir.path().join(&name), b"x").unwrap();
            std::fs::write(dir.path().join(format!("{name}{META_SUFFIX}")), b"{}").unwrap();
        }
        assert_eq!(archives(dir.path()).len(), 3);
        let gone = prune(dir.path(), 2, "gonk-store-2026-09-12T000000Z.nq.gz");
        assert_eq!(gone, ["gonk-store-2026-09-10T000000Z.nq.gz"]);
        let left = archives(dir.path());
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].name, "gonk-store-2026-09-11T000000Z.nq.gz");
        assert!(
            !dir.path()
                .join(format!(
                    "gonk-store-2026-09-10T000000Z{ARCHIVE_SUFFIX}{META_SUFFIX}"
                ))
                .exists(),
            "the sidecar goes with its archive — one left behind reads like a backup"
        );
    }

    /// An archive with no sidecar is listed and flagged, never counted as verified.
    #[test]
    fn an_archive_without_a_sidecar_is_reported_as_unverified() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path()
                .join(format!("gonk-store-2026-09-10T000000Z{ARCHIVE_SUFFIX}")),
            b"x",
        )
        .unwrap();
        let status = Status {
            backups: Backups {
                settings: Arc::new(Settings {
                    dir: dir.path().to_path_buf(),
                    keep: 5,
                    every: Some(Duration::from_secs(86_400)),
                    store_path: PathBuf::from("/nonexistent/store"),
                    scripts: None,
                }),
                jobs: None,
                next: Default::default(),
            },
        };
        let text = status.text();
        assert!(text.contains("no sidecar"), "{text}");
        assert!(text.contains("every 1d0h"), "{text}");
    }

    #[test]
    fn an_empty_backup_directory_says_so_loudly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = Status {
            backups: Backups {
                settings: Arc::new(Settings {
                    dir: dir.path().to_path_buf(),
                    keep: 5,
                    every: None,
                    store_path: PathBuf::from("/nonexistent/store"),
                    scripts: None,
                }),
                jobs: None,
                next: Default::default(),
            },
        };
        let text = status.text();
        assert!(text.contains("NO BACKUP HAS EVER BEEN TAKEN"), "{text}");
        assert!(text.contains("schedule  OFF"), "{text}");
        let json = status.json();
        assert!(json.contains("\"newest\":null"), "{json}");
        assert!(json.contains("\"timer\":null"), "{json}");
    }
}
