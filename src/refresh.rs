//! `ikigai-gonk client refresh` (ledger [#3776](http://localhost:1062/l/default/item/3776)): add
//! to every stored grant what its ledger roles mint NOW, and never take anything away.
//!
//! A grant in `grants.json` is a frozen token list: whatever [`grants_for`] minted on the day it
//! was written. When a library upgrade widens a role (`ikigai-ledger` 0.22.0 added `session` to
//! the worker; 0.23.0 gave every role its ledger's session graph), every grant minted before the
//! upgrade lacks the new tokens, and the operator has to work out the re-mint by hand.
//! `client add … --force` is the wrong tool for that: it rebuilds a grant WHOLE from its flags and
//! drops every hand-added scope.
//!
//! This is the manual form of the roles-and-assignments design's decisions 6 and 10
//! (`docs/grants/roles-and-assignments.md`): an evaluator change recomputed against stored grants,
//! additively. It does not implement those decisions.
//!
//! # How a role is read back from a grant
//!
//! For each ledger a grant names, the roles it holds are read from its own `urn:cap:ledger:*`
//! tokens, matched literally ([`holds`]):
//!
//! | role     | the grant holds, for ledger `L`                                              |
//! | -------- | ---------------------------------------------------------------------------- |
//! | `purge`  | `read:L`, `write:L`, `delete:L`, `purge:L`                                    |
//! | `delete` | `read:L`, `write:L`, `delete:L`                                               |
//! | `write`  | `read:L`, `write:L`                                                           |
//! | `worker` | `read:L`, `claim:L`, `comment:L` (and no need of `write:L`)                   |
//! | `audit`  | `read:L` and the store read token of `L`'s graveyard                          |
//! | `read`   | `read:L`                                                                      |
//!
//! Every role whose row holds is kept, and the grant gains whatever `grants_for(L, role)` mints
//! that it does not hold already ([`crate::grants::implied_by`] counts what `write:L` implies).
//! So `--ledger L=delete --ledger L=audit` refreshes as both. A ledger a grant names whose tokens
//! match no row (a raw `--ledger-graph` grant, a lone `--link-in`, `--mirror-to` or `--scope`
//! token) is left alone and listed, never guessed at.
//!
//! ⚠ `--alias L=S` mints `read:L` and the ledger graph's read token, which is exactly a reader's
//! grant before `ikigai-ledger` 0.23.0, so an alias grant refreshes as a reader of `L` (it gains
//! the session graph's read). That is the authority it already held: the alias tokens grant
//! `read:L` outright.
//!
//! # The exec split: a rename, not an addition (ledger #3820)
//!
//! `ikigai-repo` 0.6.0 retired `urn:cap:exec:git` and `urn:cap:exec:gh` for
//! `urn:cap:exec:read:git` and `urn:cap:exec:read:gh` ([`crate::grants::RETIRED_EXEC`]). A grant
//! naming an old spelling grants nothing there, and every check refuses it, so the token is
//! REWRITTEN in place, at its position in the list ([`respell_exec`]). That is the one change
//! here that takes a token away, and only for the token that replaces it. The server runs it at
//! startup, before its grants check, so the switch needs no operator step; `client refresh`
//! lists the same rewrites and `--write` makes them too.
//!
//! # Where stored token lists live
//!
//! `gonk/grants.json` is the only file that stores a token list for an identity: `clients.json`
//! (certificates and passkeys), `invites.json` and `sessions.json` store grant NAMES, and the
//! onboarding controller stores `{ledger}={role}` entitlements and rewrites its `onb-*` grants
//! from [`grants_for`] when it applies them. A published script's version keeps the part of its
//! declared scopes its publisher held at publish (`ikigai-script`'s no-elevation rule); refresh
//! does not touch that, because widening a published script past what its publisher held when
//! they published is exactly what the rule forbids. Republish such a script under the refreshed
//! grant.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ikigai_ledger::Ledger;
use serde_json::Value;

use crate::grants::{
    exec_respelling, grants_for, implied_by, Authority, CAP_RAW_WRITE_PREFIX, LEDGER_GRAPH_PREFIX,
};
use crate::quic::{self, Layout};

/// Every ledger role, highest first: the order a grant's roles are listed in.
pub const ROLES: [Authority; 6] = [
    Authority::Purge,
    Authority::Delete,
    Authority::Write,
    Authority::Worker,
    Authority::Audit,
    Authority::Read,
];

/// The word `--ledger L=<role>` spells `role` with.
pub fn role_name(role: Authority) -> &'static str {
    match role {
        Authority::Read => "read",
        Authority::Audit => "audit",
        Authority::Worker => "worker",
        Authority::Write => "write",
        Authority::Delete => "delete",
        Authority::Purge => "purge",
    }
}

/// Whether `held` holds `role` on `ledger`, read from its own ledger tokens (the table in the
/// module docs). A name that cannot be a ledger holds nothing.
///
/// ```
/// use ikigai_gonk::grants::{grants_for, Authority};
/// use ikigai_gonk::refresh::holds;
/// let worker = grants_for("acme", Authority::Worker).unwrap();
/// assert!(holds(&worker, "acme", Authority::Worker));
/// assert!(holds(&worker, "acme", Authority::Read));
/// assert!(!holds(&worker, "acme", Authority::Write));
/// assert!(!holds(&worker, "other", Authority::Read));
/// ```
pub fn holds(held: &[String], ledger: &str, role: Authority) -> bool {
    let Ok(ledger) = Ledger::parse(ledger) else {
        return false;
    };
    let has = |token: String| held.contains(&token);
    let reads = has(ledger.cap_read());
    let writes = reads && has(ledger.cap_write());
    let deletes = writes && has(ledger.cap_delete());
    match role {
        Authority::Read => reads,
        Authority::Audit => reads && has(ikigai_store::cap_read_graph(&ledger.deleted_graph())),
        Authority::Worker => reads && has(ledger.cap_claim()) && has(ledger.cap_comment()),
        Authority::Write => writes,
        Authority::Delete => deletes,
        Authority::Purge => deletes && has(ledger.cap_purge()),
    }
}

/// Whether `role`'s tokens include `other`'s, so listing `other` beside it says nothing more.
fn includes(role: Authority, other: Authority) -> bool {
    use Authority::*;
    role == other
        || match role {
            Purge => true,
            Delete | Write => matches!(other, Write | Worker | Read),
            Worker | Audit => other == Read,
            Read => false,
        }
}

/// The roles `held` holds on `ledger`, keeping only those no other held role includes, highest
/// first: `delete` and `audit` for `--ledger L=delete --ledger L=audit`, `purge` alone for a
/// purge grant.
///
/// ```
/// use ikigai_gonk::grants::{grants_for, Authority};
/// use ikigai_gonk::refresh::roles_held;
/// let mut both = grants_for("acme", Authority::Delete).unwrap();
/// both.extend(grants_for("acme", Authority::Audit).unwrap());
/// assert_eq!(roles_held(&both, "acme"), [Authority::Delete, Authority::Audit]);
/// let purge = grants_for("acme", Authority::Purge).unwrap();
/// assert_eq!(roles_held(&purge, "acme"), [Authority::Purge]);
/// ```
pub fn roles_held(held: &[String], ledger: &str) -> Vec<Authority> {
    let matched: Vec<Authority> = ROLES
        .into_iter()
        .filter(|role| holds(held, ledger, *role))
        .collect();
    matched
        .iter()
        .copied()
        .filter(|role| {
            !matched
                .iter()
                .any(|other| other != role && includes(*other, *role))
        })
        .collect()
}

/// The ledger `token` names, if it names one: a `urn:cap:ledger:{verb}:{L}` token, a store token
/// on one of `L`'s graphs, or a raw-write token on its graph.
fn ledger_of(token: &str) -> Option<String> {
    let name = if let Some(rest) = token.strip_prefix("urn:cap:ledger:") {
        rest.rsplit_once(':').map(|(_, name)| name)?
    } else {
        let graph = [
            "urn:cap:store:read:graph:",
            "urn:cap:store:write:graph:",
            CAP_RAW_WRITE_PREFIX,
        ]
        .iter()
        .find_map(|prefix| token.strip_prefix(prefix))?;
        let rest = graph.strip_prefix(LEDGER_GRAPH_PREFIX)?;
        rest.split(':').next()?
    };
    Ledger::parse(name)
        .ok()
        .map(|ledger| ledger.name().to_string())
}

/// Every ledger `held` names, in name order.
pub fn ledgers_named(held: &[String]) -> BTreeSet<String> {
    held.iter().filter_map(|token| ledger_of(token)).collect()
}

/// What refresh does to one grant on one ledger whose roles it could read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPlan {
    /// The ledger.
    pub ledger: String,
    /// The roles read back from the grant, highest first ([`roles_held`]).
    pub roles: Vec<Authority>,
    /// The tokens it adds for this ledger, in [`grants_for`]'s order. A token another ledger of
    /// the same grant already adds (the participants graph's read) is listed once, there.
    pub adds: Vec<String>,
}

/// What refresh does to one grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPlan {
    /// The grant's name, a key of `grants.json`.
    pub grant: String,
    /// Each ledger whose roles were read, in name order.
    pub ledgers: Vec<LedgerPlan>,
    /// Each ledger the grant names whose tokens match no role, with those tokens: left alone.
    pub unmatched: Vec<(String, Vec<String>)>,
    /// Each retired exec spelling the grant names, with the one that replaces it in place
    /// ([`respell_exec`]), in the grant's order.
    pub respells: Vec<(String, String)>,
}

impl GrantPlan {
    /// Every token this grant gains, in the order they would be appended.
    pub fn adds(&self) -> Vec<String> {
        self.ledgers.iter().flat_map(|l| l.adds.clone()).collect()
    }
}

/// The refresh of one grant holding `held`: per ledger, the roles read back and what the roles
/// mint now that it does not hold. Nothing is removed, ever.
///
/// ```
/// use ikigai_gonk::grants::{grants_for, Authority};
/// use ikigai_gonk::refresh::plan_grant;
/// // A worker grant as ikigai-ledger 0.22.x minted it: no session graph.
/// let old: Vec<String> = grants_for("acme", Authority::Worker)
///     .unwrap()
///     .into_iter()
///     .filter(|t| !t.ends_with(":session"))
///     .collect();
/// let plan = plan_grant("w", &old);
/// assert_eq!(
///     plan.adds(),
///     [
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:acme:session",
///         "urn:cap:store:write:graph:urn:iki:ledger:graph:acme:session",
///     ]
/// );
/// ```
pub fn plan_grant(grant: &str, held: &[String]) -> GrantPlan {
    let mut have: Vec<String> = held.to_vec();
    let mut plan = GrantPlan {
        grant: grant.to_string(),
        ledgers: Vec::new(),
        unmatched: Vec::new(),
        respells: held
            .iter()
            .filter_map(|old| exec_respelling(old).map(|new| (old.clone(), new.to_string())))
            .collect(),
    };
    for ledger in ledgers_named(held) {
        let roles = roles_held(held, &ledger);
        if roles.is_empty() {
            let tokens = held
                .iter()
                .filter(|t| ledger_of(t).as_deref() == Some(ledger.as_str()))
                .cloned()
                .collect();
            plan.unmatched.push((ledger, tokens));
            continue;
        }
        let mut adds = Vec::new();
        for role in &roles {
            // A name `ledgers_named` returned always parses, so this never fails.
            for token in grants_for(&ledger, *role).unwrap_or_default() {
                if !implied_by(&token, &have) {
                    have.push(token.clone());
                    adds.push(token);
                }
            }
        }
        plan.ledgers.push(LedgerPlan {
            ledger,
            roles,
            adds,
        });
    }
    plan
}

/// The refresh of every grant in a parsed `grants.json`, in grant-name order.
pub fn plan(grants: &BTreeMap<String, Vec<String>>) -> Vec<GrantPlan> {
    grants
        .iter()
        .map(|(name, held)| plan_grant(name, held))
        .collect()
}

/// What [`refresh`] found and did.
#[derive(Debug)]
pub struct Refreshed {
    /// The file read.
    pub file: PathBuf,
    /// Whether it existed.
    pub found: bool,
    /// One plan per grant.
    pub plans: Vec<GrantPlan>,
    /// Whether the additions were written.
    pub wrote: bool,
    /// The copy of the file as it was before the write, when there was a write.
    pub backup: Option<PathBuf>,
}

impl Refreshed {
    /// Every token added (or, on a dry run, to be added), over every grant.
    pub fn added(&self) -> usize {
        self.plans.iter().map(|p| p.adds().len()).sum()
    }

    /// Every retired exec spelling rewritten (or, on a dry run, to be), over every grant.
    pub fn respelled(&self) -> usize {
        self.plans.iter().map(|p| p.respells.len()).sum()
    }

    /// Every ledger any grant names, in name order: the ledgers whose summaries may need moving.
    pub fn ledgers(&self) -> BTreeSet<String> {
        self.plans
            .iter()
            .flat_map(|p| {
                p.ledgers
                    .iter()
                    .map(|l| l.ledger.clone())
                    .chain(p.unmatched.iter().map(|(l, _)| l.clone()))
            })
            .collect()
    }
}

/// Plan the refresh of `layout`'s `grants.json` and, with `write`, apply it: back the file up
/// beside itself as `grants.json.bak-{stamp}-refresh` (`stamp` from `now_millis`, UTC), then
/// write the whole file to a temporary and rename it over, under the grants lock. Nothing is
/// written when nothing is to be added, so a second run is a no-op that makes no backup.
///
/// A grant stored as an object (`{"scopes": [...]}`) keeps its other fields; the additions go
/// into its `scopes`.
///
/// # Errors
///
/// An unreadable or unparsable file, a result [`quic::check_grants`] would refuse (the server
/// would not start on it), or a failed write. On any error nothing is written.
pub fn refresh(layout: &Layout, write: bool, now_millis: u64) -> Result<Refreshed, String> {
    let file = layout.grants_json();
    // The lock is taken only to write: a dry run leaves the directory exactly as it found it.
    let _lock = if write {
        Some(layout.lock_grants()?)
    } else {
        None
    };
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {}: {e}", file.display())),
    };
    let Some(text) = text else {
        return Ok(Refreshed {
            file,
            found: false,
            plans: Vec::new(),
            wrote: false,
            backup: None,
        });
    };
    let grants = quic::parse_grants(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let plans = plan(&grants);
    let mut refreshed = Refreshed {
        file,
        found: true,
        plans,
        wrote: false,
        backup: None,
    };
    if !write || (refreshed.added() == 0 && refreshed.respelled() == 0) {
        return Ok(refreshed);
    }

    let Value::Object(mut doc) =
        serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?
    else {
        return Err(format!(
            "{}: must be a JSON object",
            refreshed.file.display()
        ));
    };
    respell_doc(&mut doc)?;
    for plan in &refreshed.plans {
        let adds = plan.adds();
        if adds.is_empty() {
            continue;
        }
        let entry = doc
            .get_mut(&plan.grant)
            .ok_or_else(|| format!("grant `{}` vanished while it was planned", plan.grant))?;
        scopes_of(&plan.grant, entry)?.extend(adds.into_iter().map(Value::String));
    }
    let written = quic::pretty(&Value::Object(doc))?;
    // What is written must be what the server will read, and start on.
    quic::check_grants(&quic::parse_grants(&written)?)?;

    let backup = backup_path(&refreshed.file, now_millis, "refresh");
    quic::write_private(&backup, &text)?;
    quic::write_private(&refreshed.file, &written)?;
    refreshed.wrote = true;
    refreshed.backup = Some(backup);
    Ok(refreshed)
}

/// The scope list of one grant's entry in `grants.json`: the entry itself, or its `scopes`.
fn scopes_of<'a>(grant: &str, entry: &'a mut Value) -> Result<&'a mut Vec<Value>, String> {
    match entry {
        Value::Array(list) => Ok(list),
        Value::Object(fields) => match fields.get_mut("scopes") {
            Some(Value::Array(list)) => Ok(list),
            _ => Err(format!("grant `{grant}`: `scopes` must be an array")),
        },
        _ => Err(format!("grant `{grant}` must be an array of scopes")),
    }
}

/// One retired exec spelling rewritten in one grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Respelling {
    /// The grant's name, a key of `grants.json`.
    pub grant: String,
    /// The retired spelling, which is gone from the grant.
    pub from: String,
    /// The spelling that replaced it, at its position (or already there, then kept once).
    pub to: String,
}

/// Rewrite every retired exec spelling in a parsed `grants.json` to the one that replaced it, at
/// the same position; a grant already holding the new spelling just loses the old one, so it is
/// held once. Every other field and token is untouched. Returns what moved, in grant order.
fn respell_doc(doc: &mut serde_json::Map<String, Value>) -> Result<Vec<Respelling>, String> {
    let mut moved = Vec::new();
    for (grant, entry) in doc.iter_mut() {
        let list = scopes_of(grant, entry)?;
        let mut kept: Vec<Value> = Vec::with_capacity(list.len());
        for token in list.drain(..) {
            let respelled = token.as_str().and_then(exec_respelling);
            if let (Some(old), Some(new)) = (token.as_str(), respelled) {
                moved.push(Respelling {
                    grant: grant.clone(),
                    from: old.to_string(),
                    to: new.to_string(),
                });
            }
            let token = respelled.map_or(token, |new| Value::String(new.to_string()));
            // The new spelling held twice (beside the old, or already respelled): keep the first.
            let new_spelling = token
                .as_str()
                .is_some_and(|t| crate::grants::RETIRED_EXEC.iter().any(|(_, new)| *new == t));
            if !(new_spelling && kept.contains(&token)) {
                kept.push(token);
            }
        }
        *list = kept;
    }
    Ok(moved)
}

/// ★ **The exec split's switch** (ledger #3820): rewrite every stored grant naming
/// `urn:cap:exec:git` or `urn:cap:exec:gh` to `urn:cap:exec:read:git` / `urn:cap:exec:read:gh`,
/// in place, before anything reads the grants as authority. The server calls it at startup,
/// before [`quic::check_grants`], which refuses the old spellings from then on.
///
/// Under the grants lock: the file is read, rewritten, backed up beside itself as
/// `grants.json.bak-{stamp}-exec-split` (`stamp` from `now_millis`, UTC) and written whole
/// through a temporary renamed over it. A file with no old spelling, or none at all, is not
/// touched and makes no backup, so every later start is a no-op. `Ok(None)` then; otherwise what
/// moved and the backup.
///
/// # Errors
///
/// An unreadable file, one that is not a JSON object of grants, or a failed write. On any error
/// nothing is written, and the startup check that follows refuses what it finds.
pub fn respell_exec(
    layout: &Layout,
    now_millis: u64,
) -> Result<Option<(Vec<Respelling>, PathBuf)>, String> {
    let file = layout.grants_json();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", file.display())),
    };
    // The cheap test first, so an ordinary start takes no lock and parses nothing twice.
    if !crate::grants::RETIRED_EXEC
        .iter()
        .any(|(old, _)| text.contains(&format!("\"{old}\"")))
    {
        return Ok(None);
    }
    let _lock = layout.lock_grants()?;
    // Read again under the lock: a writer may have finished between the two reads.
    let text =
        std::fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let Value::Object(mut doc) = serde_json::from_str(&text)
        .map_err(|e| format!("{}: not valid JSON: {e}", file.display()))?
    else {
        return Err(format!("{}: must be a JSON object", file.display()));
    };
    let moved = respell_doc(&mut doc).map_err(|e| format!("{}: {e}", file.display()))?;
    if moved.is_empty() {
        return Ok(None);
    }
    let written = quic::pretty(&Value::Object(doc))?;
    let backup = backup_path(&file, now_millis, "exec-split");
    quic::write_private(&backup, &text)?;
    quic::write_private(&file, &written)?;
    Ok(Some((moved, backup)))
}

/// `grants.json.bak-2026-11-15T231500Z-{why}` beside `file`, or with `-2`, `-3`, … when a run
/// in the same second already made one.
fn backup_path(file: &Path, now_millis: u64, why: &str) -> PathBuf {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "grants.json".to_string());
    let stem = format!(
        "{name}.bak-{}-{why}",
        crate::backup::stamp_compact(now_millis)
    );
    let mut candidate = file.with_file_name(&stem);
    let mut n = 2;
    while candidate.exists() {
        candidate = file.with_file_name(format!("{stem}-{n}"));
        n += 1;
    }
    candidate
}

/// The report an operator reads: per grant, the roles read back per ledger and what is (or would
/// be) added; the ledgers left alone; a total; the backup; then the session summaries' move and
/// whether a running gonk sees the change.
pub fn render(refreshed: &Refreshed) -> String {
    let mut out = String::new();
    let file = refreshed.file.display();
    if !refreshed.found {
        out.push_str(&format!(
            "no grants stored: {file} does not exist; nothing to refresh\n"
        ));
        return out;
    }
    out.push_str(&format!("grants: {file}\n"));
    for plan in &refreshed.plans {
        let adds = plan.adds().len();
        let respells = match plan.respells.len() {
            0 => String::new(),
            1 => ", 1 exec spelling to rewrite".to_string(),
            n => format!(", {n} exec spellings to rewrite"),
        };
        out.push_str(&format!(
            "\ngrant `{}`: {}{respells}\n",
            plan.grant,
            match adds {
                0 => "nothing to add".to_string(),
                1 => "1 token to add".to_string(),
                n => format!("{n} tokens to add"),
            }
        ));
        for (old, new) in &plan.respells {
            out.push_str(&format!("  rewrites {old} -> {new}\n"));
        }
        for ledger in &plan.ledgers {
            let roles: Vec<&str> = ledger.roles.iter().map(|r| role_name(*r)).collect();
            out.push_str(&format!(
                "  ledger {}  {}\n",
                ledger.ledger,
                roles.join(" + ")
            ));
            for token in &ledger.adds {
                out.push_str(&format!("    adds     {token}\n"));
            }
        }
        for (ledger, tokens) in &plan.unmatched {
            out.push_str(&format!(
                "  ledger {ledger}  no role: left alone ({})\n",
                tokens.join(", ")
            ));
        }
        if plan.ledgers.is_empty() && plan.unmatched.is_empty() {
            out.push_str("  names no ledger\n");
        }
    }
    let changing = refreshed
        .plans
        .iter()
        .filter(|p| !p.adds().is_empty())
        .count();
    let added = refreshed.added();
    let respelled = refreshed.respelled();
    let removed = if respelled > 0 {
        "nothing removed but the retired spellings"
    } else {
        "nothing removed"
    };
    out.push('\n');
    let grants = refreshed.plans.len();
    if respelled > 0 {
        out.push_str(&format!(
            "exec split (ikigai-repo 0.6.0): {} {respelled} retired exec spelling(s) to the \
             read spelling, in place\n",
            if refreshed.wrote {
                "rewrote"
            } else {
                "would rewrite"
            }
        ));
    }
    if refreshed.wrote {
        out.push_str(&format!(
            "total: added {added} token(s) to {changing} of {grants} grant(s); {removed}\n"
        ));
        if let Some(backup) = &refreshed.backup {
            out.push_str(&format!("wrote {file}\nbackup {}\n", backup.display()));
        }
    } else if added == 0 && respelled == 0 {
        out.push_str(&format!(
            "total: {grants} grant(s), nothing to add; nothing written\n"
        ));
    } else if added == 0 {
        out.push_str(&format!(
            "total: {grants} grant(s), nothing to add. Dry run: nothing written. Run again \
             with --write to rewrite them (a restart of ikigai-gonk does too)\n"
        ));
    } else {
        out.push_str(&format!(
            "total: {added} token(s) to add to {changing} of {grants} grant(s); {removed}. \
             Dry run: nothing written. Run again with --write to add them\n"
        ));
    }

    let ledgers = refreshed.ledgers();
    if !ledgers.is_empty() {
        out.push_str(
            "\nthen, once per ledger, the owner moves the session summaries ikigai-ledger 0.22 \
             wrote into the ledger graph (`plan` lists them and writes nothing; `move` moves \
             them), through the owner's socket:\n",
        );
        for ledger in &ledgers {
            for word in ["plan", "move"] {
                out.push_str(&format!(
                    "  printf {word} | ikigai -c 'sink urn:iki:ledger:{ledger}:sessions'\n"
                ));
            }
        }
    }
    out.push_str(
        "\na running gonk needs no restart: the HTTP door (passkeys) re-reads grants.json on \
         every request, and the QUIC door on every new connection. A QUIC connection already \
         open keeps the authority it was minted with until it reconnects (at most 10 minutes, \
         when the door closes it); an armed review queue's grant (gonk.review.arm) is read at \
         startup and needs one\n",
    );
    out
}
