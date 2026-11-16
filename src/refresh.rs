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

use crate::grants::{grants_for, implied_by, Authority, CAP_RAW_WRITE_PREFIX, LEDGER_GRAPH_PREFIX};
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
    if !write || refreshed.added() == 0 {
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
    for plan in &refreshed.plans {
        let adds = plan.adds();
        if adds.is_empty() {
            continue;
        }
        let entry = doc
            .get_mut(&plan.grant)
            .ok_or_else(|| format!("grant `{}` vanished while it was planned", plan.grant))?;
        let list = match entry {
            Value::Array(list) => list,
            Value::Object(fields) => match fields.get_mut("scopes") {
                Some(Value::Array(list)) => list,
                _ => return Err(format!("grant `{}`: `scopes` must be an array", plan.grant)),
            },
            _ => return Err(format!("grant `{}` must be an array of scopes", plan.grant)),
        };
        list.extend(adds.into_iter().map(Value::String));
    }
    let written = quic::pretty(&Value::Object(doc))?;
    // What is written must be what the server will read, and start on.
    quic::check_grants(&quic::parse_grants(&written)?)?;

    let backup = backup_path(&refreshed.file, now_millis);
    quic::write_private(&backup, &text)?;
    quic::write_private(&refreshed.file, &written)?;
    refreshed.wrote = true;
    refreshed.backup = Some(backup);
    Ok(refreshed)
}

/// `grants.json.bak-2026-11-15T231500Z-refresh` beside `file`, or with `-2`, `-3`, … when a run
/// in the same second already made one.
fn backup_path(file: &Path, now_millis: u64) -> PathBuf {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "grants.json".to_string());
    let stem = format!(
        "{name}.bak-{}-refresh",
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
        out.push_str(&format!(
            "\ngrant `{}`: {}\n",
            plan.grant,
            match adds {
                0 => "nothing to add".to_string(),
                1 => "1 token to add".to_string(),
                n => format!("{n} tokens to add"),
            }
        ));
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
    out.push('\n');
    let grants = refreshed.plans.len();
    if refreshed.wrote {
        out.push_str(&format!(
            "total: added {added} token(s) to {changing} of {grants} grant(s); nothing removed\n"
        ));
        if let Some(backup) = &refreshed.backup {
            out.push_str(&format!("wrote {file}\nbackup {}\n", backup.display()));
        }
    } else if added == 0 {
        out.push_str(&format!(
            "total: {grants} grant(s), nothing to add; nothing written\n"
        ));
    } else {
        out.push_str(&format!(
            "total: {added} token(s) to add to {changing} of {grants} grant(s); nothing removed. \
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
