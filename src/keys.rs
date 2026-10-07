//! `ikigai-gonk ledger backfill-keys` — give the items the bridges filed before keys existed
//! the key they would carry now (ledger #810).
//!
//! `roborev file` and `kata import` file through the keyed append (`ikigai-ledger` 0.4.0): one
//! request that checks `ledger:key` and files in the same store update. Every item they filed
//! BEFORE that carries its key only as an `about` IRI (`urn:roborev:finding:{hash}`,
//! `urn:kata:issue:{uid}`) and no `ledger:key`, so the keyed append cannot see it, and the
//! first keyed run after an upgrade would file all of it again. This command closes that gap,
//! and it is the only way to: the ledger sets a key at filing and has no resource that sets one
//! on an existing item (its README, "Not built"), so the key is written here as one SPARQL
//! update per item, through the store's graph-scoped door.
//!
//! # What it does, in order
//!
//! 1. **Reads** every item in the ledger's graph `about` a bridge IRI, and every subject (item
//!    or tombstone) that already carries a key.
//! 2. **Plans**, per bridge IRI: an item that already has that key is done; otherwise the
//!    LOWEST-numbered item about it without a key gets it (the first filing), unless some
//!    other subject holds that key already (a keyed run filed it again after the upgrade, or
//!    a tombstone). Every other unkeyed item about the same IRI is a **duplicate** — what the
//!    old check-then-append race (or a keyed run before this backfill) made — and is LISTED,
//!    never closed: closing is a judgment, and the line that does it is printed beside it. An
//!    item about two bridge IRIs is ambiguous and skipped, named.
//! 3. **Writes** (not under `--dry-run`) one conditional update per planned key —
//!    `INSERT … WHERE { FILTER NOT EXISTS { <item> ledger:key ?any } FILTER NOT EXISTS
//!    { ?taken ledger:key "<key>" } }` — so it can never put a second key on an item or a key
//!    on a second subject, even against a bridge filing at the same moment.
//! 4. **Reads back** the keys and reports each planned key that did not land where it was
//!    planned (a concurrent keyed filing took it first) rather than claiming it.
//!
//! Idempotent: a second run plans nothing and writes nothing. It does not touch `dcterms:modified`
//! (a key is a name the item always had, not an edit to it), and it does not key a DELETED
//! item's tombstone: those items were deletable and re-fileable before, and still are.
//!
//! # Where it runs
//!
//! Against a RUNNING gonk, over its owner-only socket — the door whose capability is root,
//! because nothing else may write the store's graph doors directly, and because the store has
//! one writer (the server) so nothing can open it beside a live gonk.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_ledger::vocabulary as v;
use ikigai_ledger::Ledger;
use ikigai_resolve::Resolver;

/// The `about` IRIs the bridges file under, which are also their keys.
pub const BRIDGE_PREFIXES: [&str; 2] = [crate::roborev::KEY_PREFIX, crate::kata::KEY_PREFIX];

const GRAPH_SELECT: &str = "urn:iki:store:graph-select";
const GRAPH_UPDATE: &str = "urn:iki:store:graph-update";

/// Everything `ikigai-gonk ledger backfill-keys` was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillArgs {
    /// `--ledger`: the ledger to backfill. Default `default`.
    pub ledger: String,
    /// `--socket`: gonk's owner-only socket; unset, the one `config.toml` (or `--config`)
    /// names, else `~/.ikigai/gonk.sock` — where `serve` puts it.
    pub socket: Option<PathBuf>,
    /// `--config`: read this file instead of `<config home>/config.toml`, for the socket.
    pub config: Option<PathBuf>,
    /// `--dry-run`: read and plan, write nothing.
    pub dry_run: bool,
}

/// Parse the arguments after `ledger`.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<BackfillArgs, String> {
    match args.next().as_deref() {
        Some("backfill-keys") => {}
        Some(other) => {
            return Err(format!(
                "ledger: `{other}` is not a subcommand (backfill-keys)"
            ))
        }
        None => return Err("ledger: expected `backfill-keys`".to_string()),
    }
    let mut out = BackfillArgs {
        ledger: "default".to_string(),
        socket: None,
        config: None,
        dry_run: false,
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("ledger backfill-keys: {flag} needs a value"))
        };
        match flag.as_str() {
            "--ledger" => out.ledger = value()?,
            "--socket" => out.socket = Some(PathBuf::from(value()?)),
            "--config" => out.config = Some(PathBuf::from(value()?)),
            "--dry-run" => out.dry_run = true,
            other => return Err(format!("ledger backfill-keys: unknown argument `{other}`")),
        }
    }
    Ledger::parse(&out.ledger).map_err(|e| format!("ledger backfill-keys: --ledger: {e}"))?;
    Ok(out)
}

/// One item about a bridge IRI, as the graph holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The item's IRI.
    pub item: String,
    /// Its number.
    pub number: i64,
    /// The bridge IRI it is about.
    pub about: String,
    /// Its key, if it has one.
    pub key: Option<String>,
}

/// A subject that holds a key: an item, or a deleted item's tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The item's IRI, or the tombstone's.
    pub subject: String,
    /// The (deleted) item's number.
    pub number: i64,
    /// Whether it is a tombstone.
    pub tombstone: bool,
}

/// One key to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keying {
    /// The item.
    pub item: String,
    /// Its number.
    pub number: i64,
    /// The key: its bridge IRI.
    pub key: String,
}

/// An unkeyed item about a bridge IRI another item already stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate {
    /// The duplicate.
    pub item: String,
    /// Its number.
    pub number: i64,
    /// The bridge IRI.
    pub about: String,
    /// The number of the item that holds (or will hold) the key.
    pub of: i64,
    /// Whether that holder is a deleted item's tombstone.
    pub of_deleted: bool,
}

/// What a backfill would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Keys to write, in number order.
    pub keyings: Vec<Keying>,
    /// Duplicates, listed for a person to close.
    pub duplicates: Vec<Duplicate>,
    /// Items already carrying their bridge key.
    pub already: usize,
    /// Items skipped, and why.
    pub skipped: Vec<(i64, String)>,
}

/// Decide every key: no store, so the rules are testable on their own.
///
/// `holders` is every key in the ledger and the subject holding it.
pub fn plan(rows: &[Row], holders: &BTreeMap<String, Holder>) -> Plan {
    let mut plan = Plan::default();
    // An item about two bridge IRIs cannot be given one key without guessing which.
    let mut abouts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in rows {
        abouts.entry(&row.item).or_default().insert(&row.about);
    }
    let mut by_about: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    let mut skipped = BTreeSet::new();
    for row in rows {
        if abouts[row.item.as_str()].len() > 1 {
            if skipped.insert(row.item.as_str()) {
                plan.skipped.push((
                    row.number,
                    format!(
                        "about {} bridge IRIs ({}), so which key it should carry is a guess",
                        abouts[row.item.as_str()].len(),
                        abouts[row.item.as_str()]
                            .iter()
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            continue;
        }
        by_about.entry(&row.about).or_default().push(row);
    }
    for (about, mut group) in by_about {
        group.sort_by_key(|row| row.number);
        if let Err(e) = ikigai_ledger::model::parse_key(about) {
            for row in group {
                plan.skipped
                    .push((row.number, format!("`{about}` is not a legal key: {e}")));
            }
            continue;
        }
        let mut unkeyed = Vec::new();
        for row in group {
            match row.key.as_deref() {
                Some(key) if key == about => plan.already += 1,
                Some(other) => plan.skipped.push((
                    row.number,
                    format!("already keyed `{other}`, which is not its bridge IRI `{about}`"),
                )),
                None => unkeyed.push(row),
            }
        }
        // Who stands for this IRI: whoever holds the key already, else the first filing.
        let (of, of_deleted, rest) = match holders.get(about) {
            Some(holder) => (holder.number, holder.tombstone, &unkeyed[..]),
            None => match unkeyed.split_first() {
                Some((first, rest)) => {
                    plan.keyings.push(Keying {
                        item: first.item.clone(),
                        number: first.number,
                        key: about.to_string(),
                    });
                    (first.number, false, rest)
                }
                None => continue,
            },
        };
        for row in rest {
            plan.duplicates.push(Duplicate {
                item: row.item.clone(),
                number: row.number,
                about: about.to_string(),
                of,
                of_deleted,
            });
        }
    }
    plan.keyings.sort_by_key(|k| k.number);
    plan.duplicates.sort_by_key(|d| d.number);
    plan.skipped.sort();
    plan
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The plan it carried out (or, dry, would have).
    pub plan: Plan,
    /// Keys written and read back where planned.
    pub keyed: usize,
    /// Planned keys that did not land where planned: a concurrent filing took the key.
    pub lost: Vec<Keying>,
}

/// Run a backfill over `issue` — one request in, the representation's bytes out.
///
/// `issue` is the socket door in production ([`over_socket`]) and the hub itself in tests;
/// either way it must carry a capability that may read and write the ledger's graph through
/// the store's graph doors (the socket door's root does).
pub fn run(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    ledger: &Ledger,
    dry_run: bool,
    out: &mut impl Write,
) -> Result<Report, String> {
    let graph = ledger.graph();
    let rows = select(issue, &graph, &rows_query(&graph))?
        .into_iter()
        .map(|row| {
            Ok(Row {
                item: need(&row, "item")?,
                number: number(&row)?,
                about: need(&row, "about")?,
                key: row.get("key").cloned(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let holders = key_holders(issue, &graph)?;
    let plan = plan(&rows, &holders);
    let mut report = Report {
        plan: plan.clone(),
        ..Report::default()
    };
    let would = if dry_run { "would key" } else { "keying" };
    for keying in &plan.keyings {
        let _ = writeln!(
            out,
            "{would}  {}  {}",
            ledger.number(keying.number),
            keying.key
        );
        if !dry_run {
            update(issue, &graph, &insert_key(&graph, keying)?)?;
        }
    }
    if !dry_run && !plan.keyings.is_empty() {
        // Read back, rather than claim: a keyed filing between the plan and the update makes
        // the conditional insert a no-op, and then the key belongs to that filing.
        let after = key_holders(issue, &graph)?;
        for keying in &plan.keyings {
            match after.get(&keying.key) {
                Some(holder) if holder.subject == keying.item => report.keyed += 1,
                _ => report.lost.push(keying.clone()),
            }
        }
        for lost in &report.lost {
            let holder = after
                .get(&lost.key)
                .map(|h| ledger.number(h.number))
                .unwrap_or_else(|| "nothing".to_string());
            let _ = writeln!(
                out,
                "lost     {}  {}: the key is held by {holder} now (a filing raced this run); \
                 {} is a duplicate of it",
                ledger.number(lost.number),
                lost.key,
                ledger.number(lost.number)
            );
        }
    }
    let close = ledger.resource("close");
    for dup in &plan.duplicates {
        let _ = writeln!(
            out,
            "duplicate {} of {}{}  {}\n    close it: sink {close} item={} reason=duplicate",
            ledger.number(dup.number),
            ledger.number(dup.of),
            if dup.of_deleted { " (deleted)" } else { "" },
            dup.about,
            dup.number
        );
    }
    for (number, why) in &plan.skipped {
        let _ = writeln!(out, "skipped  {}: {why}", ledger.number(*number));
    }
    let _ = writeln!(
        out,
        "\n{}{} key(s) {}, {} item(s) already keyed, {} duplicate(s) listed, {} skipped{}",
        if dry_run {
            "dry run, nothing written: "
        } else {
            ""
        },
        if dry_run {
            plan.keyings.len()
        } else {
            report.keyed
        },
        if dry_run {
            "would be written"
        } else {
            "written"
        },
        plan.already,
        plan.duplicates.len(),
        plan.skipped.len(),
        if report.lost.is_empty() {
            String::new()
        } else {
            format!(", {} lost to a concurrent filing", report.lost.len())
        }
    );
    Ok(report)
}

/// Every item in `graph` about a bridge IRI, with its key when it has one.
fn rows_query(graph: &str) -> String {
    let filter = BRIDGE_PREFIXES
        .iter()
        .map(|prefix| format!("STRSTARTS(STR(?about), \"{prefix}\")"))
        .collect::<Vec<_>>()
        .join(" || ");
    format!(
        "SELECT ?item ?number ?about ?key WHERE {{ GRAPH <{graph}> {{ \
         ?item <{type_}> <{item}> ; <{number}> ?number ; <{about}> ?about . \
         FILTER({filter}) OPTIONAL {{ ?item <{key}> ?key }} }} }} ORDER BY ?number ?about",
        type_ = v::ext::TYPE,
        item = v::ITEM_CLASS,
        number = v::NUMBER,
        about = v::ABOUT,
        key = v::KEY,
    )
}

/// Every key in `graph`, and the item or tombstone holding it.
fn key_holders(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    graph: &str,
) -> Result<BTreeMap<String, Holder>, String> {
    let query = format!(
        "SELECT ?s ?class ?number ?key WHERE {{ GRAPH <{graph}> {{ \
         ?s <{key}> ?key ; <{type_}> ?class ; <{number}> ?number . \
         VALUES ?class {{ <{item}> <{tombstone}> }} }} }} ORDER BY ?key ?class ?s",
        key = v::KEY,
        type_ = v::ext::TYPE,
        number = v::NUMBER,
        item = v::ITEM_CLASS,
        tombstone = v::TOMBSTONE_CLASS,
    );
    let mut holders = BTreeMap::new();
    for row in select(issue, graph, &query)? {
        // The first per key wins, in the ledger's own order (an item before a tombstone).
        holders.entry(need(&row, "key")?).or_insert(Holder {
            subject: need(&row, "s")?,
            number: number(&row)?,
            tombstone: need(&row, "class")? == v::TOMBSTONE_CLASS,
        });
    }
    Ok(holders)
}

/// The one conditional update that gives `keying.item` its key.
fn insert_key(graph: &str, keying: &Keying) -> Result<String, String> {
    // The key's shape is the ledger's own (letters, digits, `-._~:`), so it needs no escaping
    // inside a literal; the item's IRI came from the store, and is checked anyway.
    let key = ikigai_ledger::model::parse_key(&keying.key).map_err(|e| e.to_string())?;
    let item = Iri::parse(keying.item.clone())
        .map_err(|e| format!("the store named an item `{}`: {e}", keying.item))?;
    if keying.item.contains(['<', '>', '"', ' ']) {
        return Err(format!("the store named an item `{}`", keying.item));
    }
    Ok(format!(
        "INSERT {{ GRAPH <{graph}> {{ <{item}> <{key_p}> \"{key}\" }} }} WHERE {{ \
         GRAPH <{graph}> {{ <{item}> <{type_}> <{class}> . \
         FILTER NOT EXISTS {{ <{item}> <{key_p}> ?any }} \
         FILTER NOT EXISTS {{ ?taken <{key_p}> \"{key}\" }} }} }}",
        item = item.as_str(),
        key_p = v::KEY,
        type_ = v::ext::TYPE,
        class = v::ITEM_CLASS,
    ))
}

fn select(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    graph: &str,
    query: &str,
) -> Result<Vec<BTreeMap<String, String>>, String> {
    let bytes = issue(
        Request::new(
            Verb::Source,
            Iri::parse(GRAPH_SELECT).expect("a constant IRI"),
        )
        .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
        .with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec())),
    )?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("the store's SELECT answer is not JSON: {e}"))?;
    let bindings = json
        .pointer("/results/bindings")
        .and_then(serde_json::Value::as_array)
        .ok_or("the store's SELECT answer has no results.bindings")?;
    Ok(bindings
        .iter()
        .filter_map(serde_json::Value::as_object)
        .map(|binding| {
            binding
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.clone(), value.get("value")?.as_str()?.to_string()))
                })
                .collect()
        })
        .collect())
}

fn update(
    issue: &dyn Fn(Request) -> Result<Vec<u8>, String>,
    graph: &str,
    update: &str,
) -> Result<(), String> {
    issue(
        Request::new(
            Verb::Sink,
            Iri::parse(GRAPH_UPDATE).expect("a constant IRI"),
        )
        .with_arg("content", ArgRef::Inline(update.as_bytes().to_vec()))
        .with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec())),
    )
    .map(|_| ())
}

fn need(row: &BTreeMap<String, String>, name: &str) -> Result<String, String> {
    row.get(name)
        .cloned()
        .ok_or_else(|| format!("a store row with no `{name}`"))
}

fn number(row: &BTreeMap<String, String>) -> Result<i64, String> {
    let text = need(row, "number")?;
    text.parse()
        .map_err(|_| format!("a ledger:number `{text}` that is not an integer"))
}

/// Issue each request through gonk's owner-only socket at `path`.
pub fn over_socket(
    path: &std::path::Path,
) -> Result<impl Fn(Request) -> Result<Vec<u8>, String>, String> {
    let client = ikigai_ipc::connect(path).map_err(|e| {
        format!(
            "cannot reach gonk's socket at {}: {e} (is gonk running? `--socket` names another)",
            path.display()
        )
    })?;
    Ok(move |request: Request| {
        let iri = request.target.as_str().to_string();
        client
            .issue(request)
            .map(|(repr, _)| repr.bytes)
            .map_err(|e| format!("{iri}: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RV: &str = "urn:roborev:finding:00112233445566778899aabbccddeeff";
    const KT: &str = "urn:kata:issue:01K6Z000000000000000000001";

    fn row(number: i64, about: &str, key: Option<&str>) -> Row {
        Row {
            item: format!("urn:iki:ledger:default:item:i{number}"),
            number,
            about: about.to_string(),
            key: key.map(str::to_string),
        }
    }

    #[test]
    fn the_first_filing_takes_the_key_and_the_rest_are_listed() {
        let rows = [row(7, RV, None), row(3, RV, None), row(9, RV, None)];
        let plan = plan(&rows, &BTreeMap::new());
        assert_eq!(plan.keyings.len(), 1);
        assert_eq!(
            plan.keyings[0].number, 3,
            "the lowest number is the first filing"
        );
        assert_eq!(plan.keyings[0].key, RV);
        let dups: Vec<(i64, i64)> = plan.duplicates.iter().map(|d| (d.number, d.of)).collect();
        assert_eq!(dups, [(7, 3), (9, 3)]);
    }

    #[test]
    fn a_key_already_held_keys_nothing_and_names_the_holder() {
        let mut holders = BTreeMap::new();
        holders.insert(
            KT.to_string(),
            Holder {
                subject: "urn:iki:ledger:default:item:i12".into(),
                number: 12,
                tombstone: false,
            },
        );
        // #12 was filed keyed after an upgrade that ran before this backfill; #4 is the old one.
        let rows = [row(4, KT, None), row(12, KT, Some(KT))];
        let plan = plan(&rows, &holders);
        assert!(plan.keyings.is_empty());
        assert_eq!(plan.already, 1);
        assert_eq!(plan.duplicates.len(), 1);
        assert_eq!((plan.duplicates[0].number, plan.duplicates[0].of), (4, 12));
    }

    #[test]
    fn done_ambiguous_and_odd_items_are_left_alone() {
        let rows = [
            row(1, RV, Some(RV)),
            row(2, KT, None),
            row(2, RV, None),
            row(5, KT, Some("urn:something:else")),
        ];
        let plan = plan(&rows, &BTreeMap::new());
        assert_eq!(plan.already, 1);
        assert!(plan.keyings.is_empty(), "{plan:?}");
        assert!(plan.duplicates.is_empty(), "{plan:?}");
        assert_eq!(plan.skipped.len(), 2, "{plan:?}");
        assert!(plan.skipped[0].1.contains("2 bridge IRIs"), "{plan:?}");
        assert!(plan.skipped[1].1.contains("already keyed"), "{plan:?}");
    }

    #[test]
    fn the_update_is_conditional_on_both_sides() {
        let update = insert_key(
            "urn:iki:ledger:graph:default",
            &Keying {
                item: "urn:iki:ledger:default:item:a".into(),
                number: 1,
                key: KT.into(),
            },
        )
        .unwrap();
        assert!(update.contains(&format!(
            "FILTER NOT EXISTS {{ <urn:iki:ledger:default:item:a> <{}> ?any }}",
            v::KEY
        )));
        assert!(update.contains(&format!(
            "FILTER NOT EXISTS {{ ?taken <{}> \"{KT}\" }}",
            v::KEY
        )));
        assert!(insert_key(
            "g",
            &Keying {
                item: "urn:x".into(),
                number: 1,
                key: "has a space".into(),
            }
        )
        .is_err());
    }

    #[test]
    fn the_arguments() {
        let parsed = parse_args(["backfill-keys"].into_iter().map(String::from)).unwrap();
        assert_eq!(parsed.ledger, "default");
        assert!(!parsed.dry_run);
        let parsed = parse_args(
            [
                "backfill-keys",
                "--ledger",
                "reviews",
                "--dry-run",
                "--socket",
                "/tmp/g.s",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(parsed.ledger, "reviews");
        assert!(parsed.dry_run);
        assert_eq!(parsed.socket, Some(PathBuf::from("/tmp/g.s")));
        assert!(parse_args(["backfill"].into_iter().map(String::from)).is_err());
        assert!(parse_args(
            ["backfill-keys", "--ledger", "Not A Ledger"]
                .into_iter()
                .map(String::from)
        )
        .is_err());
    }
}
