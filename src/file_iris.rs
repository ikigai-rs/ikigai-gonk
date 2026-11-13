//! `ikigai-gonk ledger canonical-file-iris` — rewrite the file IRI of findings filed before
//! the canonical form, and the conversion every filer uses (ledger #3657).
//!
//! A finding names its file as `urn:repo:file:{root}/{percent-encoded path}`: the form
//! `ledger:Finding`'s comment names and `gonk.repository.file` resolves. `roborev file` and
//! `findings file` wrote browse's own IRI, `urn:repo:{root}:file:{path}`, until #3657, so the
//! findings already in a ledger carry that one, and a join on the canonical form
//! (`items about=urn:repo:file:…`) does not find them.
//!
//! # Why a migration and not a join on both forms
//!
//! Nothing in gonk joins items to a file on two forms today, and nothing should have to: every
//! reader that asks "the items about this file" (`items about=`, a saved SPARQL query, a page
//! built later) would carry the old form forever, and each one that forgot would silently drop
//! the older findings. Rewriting them once leaves ONE form in the ledger, and the readers simple.
//!
//! # What it does
//!
//! It only touches **findings**: items that are also about a bridge key (`urn:roborev:finding:`
//! or `urn:iki:finding:`), so an item a person filed about a browse IRI on purpose is left alone.
//! For each `about` of the form `urn:repo:{root}:file:{path}` on such an item it plans the
//! canonical IRI, and:
//!
//! - **by default it is a dry run**: it prints every rewrite it would make and writes nothing;
//! - with `--apply` it writes each one as one SPARQL update through the store's graph-scoped
//!   door — delete the old `ledger:about`, insert the canonical one, conditional on the old
//!   one still being there — then reads back and reports how many old-form IRIs remain.
//!
//! Idempotent: a second run plans nothing. It does not touch `dcterms:modified`: the item names
//! the same file, in the spelling the ledger settled on. It runs over the owner-only socket,
//! like `ledger backfill-keys` ([`crate::keys`]), for the same reasons.

use std::io::Write;
use std::path::PathBuf;

use ikigai_core::{Iri, Request};
use ikigai_ledger::vocabulary as v;
use ikigai_ledger::Ledger;

use crate::keys::{need, number, select, update};

/// The canonical file IRI for browse's `urn:repo:{root}:file:{path}`, or `None` when `iri` is
/// not one. The path is kept exactly as encoded.
///
/// ```
/// use ikigai_gonk::file_iris::canonical;
///
/// assert_eq!(
///     canonical("urn:repo:ikigai-gonk:file:src/a%20b.rs").as_deref(),
///     Some("urn:repo:file:ikigai-gonk/src/a%20b.rs")
/// );
/// assert_eq!(canonical("urn:repo:file:ikigai-gonk/src/a.rs"), None, "already canonical");
/// assert_eq!(canonical("urn:repo:demo:pr:42"), None, "not a file");
/// assert_eq!(canonical("urn:repo:demo:file:../x"), None, "not a repository-relative path");
/// ```
pub fn canonical(iri: &str) -> Option<String> {
    let (root, rest) = iri.strip_prefix("urn:repo:")?.split_once(':')?;
    let path = rest.strip_prefix("file:")?;
    if root == "file" || crate::browse::check_root_name(root).is_err() {
        return None;
    }
    let out = format!("urn:repo:file:{root}/{path}");
    crate::web::repo_file(&out).is_some().then_some(out)
}

/// Everything `ikigai-gonk ledger canonical-file-iris` was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// `--ledger`: the ledger to rewrite. Default `default`.
    pub ledger: String,
    /// `--socket`: gonk's owner-only socket; unset, the one the config names.
    pub socket: Option<PathBuf>,
    /// `--config`: read this file instead of `<config home>/config.toml`, for the socket.
    pub config: Option<PathBuf>,
    /// `--config-home`: the config home whose config.toml supplies the socket.
    pub config_home: Option<PathBuf>,
    /// `--data-home`: the data home containing the default socket.
    pub data_home: Option<PathBuf>,
    /// `--apply`: write. Without it the run is dry: it prints the plan and writes nothing.
    pub apply: bool,
}

/// Parse the arguments after `ledger canonical-file-iris`.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        ledger: "default".to_string(),
        socket: None,
        config: None,
        config_home: None,
        data_home: None,
        apply: false,
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("ledger canonical-file-iris: {flag} needs a value"))
        };
        match flag.as_str() {
            "--ledger" => out.ledger = value()?,
            "--socket" => out.socket = Some(PathBuf::from(value()?)),
            "--config" => out.config = Some(PathBuf::from(value()?)),
            "--config-home" => out.config_home = Some(PathBuf::from(value()?)),
            "--data-home" => out.data_home = Some(PathBuf::from(value()?)),
            "--apply" => out.apply = true,
            "--dry-run" => out.apply = false,
            other => {
                return Err(format!(
                    "ledger canonical-file-iris: unknown argument `{other}`"
                ))
            }
        }
    }
    Ledger::parse(&out.ledger).map_err(|e| format!("ledger canonical-file-iris: --ledger: {e}"))?;
    Ok(out)
}

/// One `about` to rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// The item.
    pub item: String,
    /// Its number.
    pub number: i64,
    /// The old-form file IRI it is about.
    pub from: String,
    /// The canonical one it will be about instead.
    pub to: String,
}

/// What a run planned, and (applied) how many old-form IRIs were left after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Every rewrite, in number order.
    pub rewrites: Vec<Rewrite>,
    /// Old-form file IRIs still on findings after the writes; `None` on a dry run.
    pub remaining: Option<usize>,
}

/// Run over `issue` (the socket door in production, the hub in tests).
pub fn run(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    ledger: &Ledger,
    apply: bool,
    out: &mut impl Write,
) -> Result<Report, String> {
    let graph = ledger.graph();
    let rewrites = plan(issue, &graph)?;
    let verb = if apply { "rewriting" } else { "would rewrite" };
    for r in &rewrites {
        let _ = writeln!(
            out,
            "{verb}  {}  about {}  ->  {}",
            ledger.number(r.number),
            r.from,
            r.to
        );
        if apply {
            update(issue, &graph, &rewrite_update(&graph, r)?)?;
        }
    }
    let remaining = if apply {
        Some(plan(issue, &graph)?.len())
    } else {
        None
    };
    match remaining {
        None => {
            let _ = writeln!(
                out,
                "\ndry run, nothing written: {} about(s) would be rewritten; \
                 run again with --apply to write them",
                rewrites.len()
            );
        }
        Some(left) => {
            let _ = writeln!(
                out,
                "\n{} about(s) rewritten, {left} old-form file IRI(s) left on findings",
                rewrites.len() - left.min(rewrites.len())
            );
        }
    }
    Ok(Report {
        rewrites,
        remaining,
    })
}

/// Every old-form file `about` on a finding in `graph`, with its canonical IRI.
fn plan(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    graph: &str,
) -> Result<Vec<Rewrite>, String> {
    let bridges = [crate::roborev::KEY_PREFIX, crate::queue::FINDING_PREFIX]
        .iter()
        .map(|p| format!("STRSTARTS(STR(?bridge), \"{p}\")"))
        .collect::<Vec<_>>()
        .join(" || ");
    let query = format!(
        "SELECT DISTINCT ?item ?number ?about WHERE {{ GRAPH <{graph}> {{ \
         ?item <{type_}> <{item}> ; <{number}> ?number ; <{about}> ?about ; <{about}> ?bridge . \
         FILTER(STRSTARTS(STR(?about), \"urn:repo:\")) FILTER({bridges}) }} }} \
         ORDER BY ?number ?about",
        type_ = v::ext::TYPE,
        item = v::ITEM_CLASS,
        number = v::NUMBER,
        about = v::ABOUT,
    );
    let mut rewrites = Vec::new();
    for row in select(issue, graph, &query)? {
        let from = need(&row, "about")?;
        if let Some(to) = canonical(&from) {
            rewrites.push(Rewrite {
                item: need(&row, "item")?,
                number: number(&row)?,
                from,
                to,
            });
        }
    }
    Ok(rewrites)
}

/// The one conditional update that moves `r.item`'s `about` from the old form to the new.
fn rewrite_update(graph: &str, r: &Rewrite) -> Result<String, String> {
    for iri in [&r.item, &r.from, &r.to] {
        Iri::parse(iri.clone()).map_err(|e| format!("the store named `{iri}`: {e}"))?;
        if iri.contains(['<', '>', '"', ' ', '{', '}', '\\']) {
            return Err(format!("`{iri}` cannot be written inside a SPARQL IRI"));
        }
    }
    Ok(format!(
        "DELETE {{ GRAPH <{graph}> {{ <{item}> <{about}> <{from}> }} }} \
         INSERT {{ GRAPH <{graph}> {{ <{item}> <{about}> <{to}> }} }} \
         WHERE {{ GRAPH <{graph}> {{ <{item}> <{about}> <{from}> }} }}",
        item = r.item,
        about = v::ABOUT,
        from = r.from,
        to = r.to,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_browse_file_iris_convert() {
        assert_eq!(
            canonical("urn:repo:demo:file:src/lib.rs").as_deref(),
            Some("urn:repo:file:demo/src/lib.rs")
        );
        for not in [
            "urn:repo:demo:tree:src",
            "urn:repo:demo:file:",
            "urn:repo:file:demo/src/lib.rs",
            "urn:roborev:finding:00",
            "urn:repo:demo:file:a//b",
        ] {
            assert_eq!(canonical(not), None, "{not}");
        }
    }

    #[test]
    fn dry_run_is_the_default() {
        let parsed = parse_args(std::iter::empty()).unwrap();
        assert!(!parsed.apply);
        assert_eq!(parsed.ledger, "default");
        let parsed =
            parse_args(["--apply", "--ledger", "x"].map(String::from).into_iter()).unwrap();
        assert!(parsed.apply);
        assert!(parse_args(["--bogus"].map(String::from).into_iter()).is_err());
    }

    #[test]
    fn the_update_is_conditional_on_the_old_about() {
        let r = Rewrite {
            item: "urn:iki:ledger:default:item:a".into(),
            number: 1,
            from: "urn:repo:demo:file:a.rs".into(),
            to: "urn:repo:file:demo/a.rs".into(),
        };
        let u = rewrite_update("urn:iki:ledger:graph:default", &r).unwrap();
        assert!(u.contains(
            "WHERE { GRAPH <urn:iki:ledger:graph:default> { <urn:iki:ledger:default:item:a>"
        ));
        assert!(u.contains("INSERT") && u.contains("<urn:repo:file:demo/a.rs>"));
    }
}
