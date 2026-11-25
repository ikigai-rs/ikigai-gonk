//! What an upgraded install still needs to match a fresh one (ledger #4369), said at startup and
//! served as [`STATUS`].
//!
//! A release after a breaking change has to upgrade from ANY older one. Three things an older
//! install keeps are not rewritten by the server on its own, each for a reason of its own, and
//! each has an operator command that fixes it:
//!
//! - **Grants missing what their roles mint now.** A grant in `grants.json` is the token list
//!   minted the day it was written; a grant from before `ikigai-ledger` 0.22 (the worker's
//!   session graph) or 0.23 (every role's session-graph read) lacks the newer tokens, so a
//!   worker's session-summary writes are refused. `ikigai-gonk client refresh --write` adds
//!   them ([`crate::refresh`]); it is never run on the operator's behalf, because it widens
//!   authority.
//! - **Published scripts granted a retired exec spelling.** A script's head keeps the part of
//!   its `requires` its publisher held at publish (`ikigai-script`'s `Head::granted`). One
//!   published before the exec split names `urn:cap:exec:git` / `urn:cap:exec:gh`, which grant
//!   nothing since `ikigai-repo` 0.6.0 ([`crate::grants::RETIRED_EXEC`]). Only a republish
//!   under a grant holding the new spelling fixes it: widening a head past what its publisher
//!   held is what the no-elevation rule forbids.
//! - **Findings naming a file in browse's old form.** A finding filed before ledger #3657 is
//!   about `urn:repo:{root}:file:{path}`, so the file page's item list, which joins on the
//!   canonical IRI, does not show it. `ikigai-gonk ledger canonical-file-iris --apply` rewrites
//!   them ([`crate::file_iris`]).
//!
//! # What this does, and does not
//!
//! [`Upgrade::check`] computes each as a READ-ONLY plan: a dry `client refresh`, a read of every
//! script head, and the dry run of `canonical-file-iris` over every ledger. The server prints
//! ONE line per plan that is not empty ([`Status::lines`]), naming what it found and the exact
//! command that fixes it, and nothing on an install that needs nothing. It never applies one.
//! [`STATUS`] answers the same, computed afresh on each read, so a person or an agent can check
//! the fix took. It needs the System tab's [`crate::system::CAP_READ`], the operator's read: it
//! names grants and scripts.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri,
    Kernel, ReprType, Representation, Request, Result, Verb,
};
use ikigai_ledger::Ledger;
use ikigai_script::Backend;
use serde_json::{json, Value};

use crate::quic::Layout;

/// What an upgraded install still needs: `Source urn:iki:gonk:upgrade:status`.
pub const STATUS: &str = "urn:iki:gonk:upgrade:status";

const JSON: &str = "application/json";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// One grant that lacks tokens its roles mint now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantNeed {
    /// The grant's name, a key of `grants.json`.
    pub grant: String,
    /// The tokens `client refresh --write` would add.
    pub adds: Vec<String>,
}

/// One script whose head grants a retired exec spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptNeed {
    /// The script's name.
    pub name: String,
    /// Each retired spelling its head grants, with the one that replaced it.
    pub retired: Vec<(String, String)>,
}

/// One ledger with findings about a file in browse's old form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerNeed {
    /// The ledger.
    pub ledger: String,
    /// The old-form `about`s `canonical-file-iris --apply` would rewrite.
    pub rewrites: usize,
}

/// The three plans, and any that could not be computed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Grants to refresh, in grant-name order.
    pub grants: Vec<GrantNeed>,
    /// Scripts to republish, in name order.
    pub scripts: Vec<ScriptNeed>,
    /// Ledgers whose findings' file IRIs to rewrite, in name order.
    pub file_iris: Vec<LedgerNeed>,
    /// A plan that could not be read, said as a line of its own rather than as "nothing".
    pub errors: Vec<String>,
}

/// How [`Status::lines`] names the commands: this server's config home and socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commands {
    /// The config home `grants.json` lives in.
    pub config_home: PathBuf,
    /// The owner-only socket this server binds.
    pub socket: PathBuf,
}

impl Status {
    /// Whether the install needs nothing (and every plan was read).
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
            && self.scripts.is_empty()
            && self.file_iris.is_empty()
            && self.errors.is_empty()
    }

    /// One line per plan that is not empty, each naming what it found and the command that
    /// fixes it; none at all on an install that needs nothing.
    pub fn lines(&self, commands: &Commands) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.grants.is_empty() {
            let tokens: usize = self.grants.iter().map(|g| g.adds.len()).sum();
            lines.push(format!(
                "upgrade: {} grant(s) lack {tokens} token(s) their roles mint now ({}); run \
                 `ikigai-gonk client refresh --write --config-home {}`",
                self.grants.len(),
                names(self.grants.iter().map(|g| g.grant.as_str())),
                commands.config_home.display(),
            ));
        }
        if !self.scripts.is_empty() {
            let spellings: BTreeSet<&str> = self
                .scripts
                .iter()
                .flat_map(|s| s.retired.iter().map(|(_, new)| new.as_str()))
                .collect();
            lines.push(format!(
                "upgrade: {} published script(s) grant a retired exec spelling, which grants \
                 nothing since the exec split ({}); republish each (`sink urn:script:{{name}}` with \
                 its content, over {}) from a grant holding {}",
                self.scripts.len(),
                names(self.scripts.iter().map(|s| s.name.as_str())),
                commands.socket.display(),
                spellings.into_iter().collect::<Vec<_>>().join(" and "),
            ));
        }
        for need in &self.file_iris {
            lines.push(format!(
                "upgrade: ledger `{}` has {} finding file IRI(s) in the old form, missing from \
                 the file page's item list; run `ikigai-gonk ledger canonical-file-iris --ledger \
                 {} --socket {} --apply`",
                need.ledger,
                need.rewrites,
                need.ledger,
                commands.socket.display(),
            ));
        }
        for error in &self.errors {
            lines.push(format!("upgrade: could not check {error}"));
        }
        lines
    }

    /// The same, as JSON: each plan's findings, and the lines.
    pub fn json(&self, commands: &Commands) -> Value {
        json!({
            "needs_nothing": self.is_empty(),
            "grants": self.grants.iter().map(|g| json!({
                "grant": g.grant,
                "adds": g.adds,
            })).collect::<Vec<_>>(),
            "scripts": self.scripts.iter().map(|s| json!({
                "name": s.name,
                "retired": s.retired.iter().map(|(old, new)| json!({
                    "granted": old,
                    "replaced_by": new,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "file_iris": self.file_iris.iter().map(|l| json!({
                "ledger": l.ledger,
                "rewrites": l.rewrites,
            })).collect::<Vec<_>>(),
            "errors": self.errors,
            "lines": self.lines(commands),
        })
    }
}

/// Names as a line lists them: the first eight, then how many more.
fn names<'a>(all: impl Iterator<Item = &'a str>) -> String {
    let all: Vec<&str> = all.collect();
    let mut shown = all
        .iter()
        .take(8)
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if all.len() > 8 {
        shown.push_str(&format!(" and {} more", all.len() - 8));
    }
    shown
}

/// The grants `client refresh` would add to, read from `layout` with a dry run: nothing is
/// locked or written.
pub fn grants_plan(layout: &Layout) -> std::result::Result<Vec<GrantNeed>, String> {
    let refreshed = crate::refresh::refresh(layout, false, 0)?;
    Ok(refreshed
        .plans
        .into_iter()
        .filter_map(|plan| {
            let adds = plan.adds();
            (!adds.is_empty()).then_some(GrantNeed {
                grant: plan.grant,
                adds,
            })
        })
        .collect())
}

/// The scripts whose head grants a retired exec spelling, read from `backend`. A head that
/// cannot be read is skipped here: the script family reports a broken head itself.
pub fn scripts_plan(backend: &dyn Backend) -> std::result::Result<Vec<ScriptNeed>, String> {
    let mut names = backend
        .names()
        .map_err(|e| format!("the scripts could not be listed: {e}"))?;
    names.sort();
    let mut needs = Vec::new();
    for name in names {
        let Ok(Some(head)) = backend.head(&name) else {
            continue;
        };
        let retired: Vec<(String, String)> = head
            .granted
            .iter()
            .filter_map(|old| {
                crate::grants::exec_respelling(old).map(|new| (old.clone(), new.to_string()))
            })
            .collect();
        if !retired.is_empty() {
            needs.push(ScriptNeed { name, retired });
        }
    }
    Ok(needs)
}

/// The ledgers with findings about a file in browse's old form, asked of `hub`: the dry run of
/// `canonical-file-iris` over every ledger in the store. The graph list is read under the
/// store's read, and each ledger's graph under that graph's read alone; nothing is written.
pub async fn file_iris_plan(hub: &Kernel) -> std::result::Result<Vec<LedgerNeed>, String> {
    let capability = &Capability::scoped([ikigai_store::CAP_READ]);
    let graphs = Iri::parse("urn:iki:store:graphs").expect("a constant IRI");
    let listed = hub
        .issue(Request::new(Verb::Source, graphs), capability)
        .await
        .map_err(|e| format!("the store's graphs could not be listed: {e}"))?;
    let mut needs = Vec::new();
    for name in crate::script::ledgers_in(&String::from_utf8_lossy(&listed.bytes)) {
        let Ok(ledger) = Ledger::parse(&name) else {
            continue;
        };
        let graph = ledger.graph();
        let query = crate::file_iris::plan_query(&graph);
        let answer = hub
            .issue(
                crate::keys::select_request(&graph, &query),
                &Capability::scoped([ikigai_store::cap_read_graph(&graph)]),
            )
            .await
            .map_err(|e| format!("ledger `{name}`'s findings could not be read: {e}"))?;
        let rewrites = crate::keys::parse_select(&answer.bytes)
            .and_then(crate::file_iris::rewrites)
            .map_err(|e| format!("ledger `{name}`'s findings: {e}"))?;
        if !rewrites.is_empty() {
            needs.push(LedgerNeed {
                ledger: name,
                rewrites: rewrites.len(),
            });
        }
    }
    Ok(needs)
}

/// Where the three plans are read from, and the resource that serves them.
pub struct Upgrade {
    layout: Layout,
    scripts: Arc<dyn Backend>,
    commands: Commands,
    hub: OnceLock<Weak<Kernel>>,
}

impl Upgrade {
    /// Over `layout`'s `grants.json`, the script atoms in `scripts`, and the store the hub
    /// [`Upgrade::attach`] hands it holds; `commands` name this server's config home and socket.
    pub fn new(layout: Layout, scripts: Arc<dyn Backend>, commands: Commands) -> Arc<Upgrade> {
        Arc::new(Upgrade {
            layout,
            scripts,
            commands,
            hub: OnceLock::new(),
        })
    }

    /// Hand it the hub it is bound in, for the store's plan. A second call is a no-op.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }

    /// The commands its lines name.
    pub fn commands(&self) -> &Commands {
        &self.commands
    }

    /// Compute every plan, reading only. A plan that fails is said in [`Status::errors`].
    pub async fn check(&self) -> Status {
        let mut status = Status::default();
        match grants_plan(&self.layout) {
            Ok(grants) => status.grants = grants,
            Err(e) => status.errors.push(format!("the grants: {e}")),
        }
        match scripts_plan(self.scripts.as_ref()) {
            Ok(scripts) => status.scripts = scripts,
            Err(e) => status.errors.push(format!("the scripts: {e}")),
        }
        match self.hub.get().and_then(Weak::upgrade) {
            Some(hub) => match file_iris_plan(&hub).await {
                Ok(needs) => status.file_iris = needs,
                Err(e) => status.errors.push(format!("the findings' file IRIs: {e}")),
            },
            None => status
                .errors
                .push("the findings' file IRIs: the hub is not attached yet".to_string()),
        }
        status
    }

    /// [`STATUS`], as a space for the hub.
    pub fn space(self: &Arc<Self>) -> EndpointSpace {
        EndpointSpace::new().bind(Exact::new(STATUS), StatusEndpoint(Arc::clone(self)))
    }
}

struct StatusEndpoint(Arc<Upgrade>);

#[async_trait]
impl Endpoint for StatusEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "{STATUS} answers Source, not {:?}",
                inv.request.verb
            )));
        }
        crate::system::require(inv.capability)?;
        let status = self.0.check().await;
        let commands = self.0.commands();
        if matches!(inv.inline_str("as"), Ok(JSON)) {
            let mut bytes = serde_json::to_vec(&status.json(commands)).unwrap_or_default();
            bytes.push(b'\n');
            Ok(Representation::new(
                ReprType::new(JSON).with_param("charset", "utf-8"),
                bytes,
            ))
        } else {
            let lines = status.lines(commands);
            let text = if lines.is_empty() {
                "upgrade: nothing to do; this install matches a fresh one\n".to_string()
            } else {
                lines.join("\n") + "\n"
            };
            Ok(Representation::new(
                ReprType::new("text/plain").with_param("charset", "utf-8"),
                text.into_bytes(),
            ))
        }
    }

    fn name(&self) -> &str {
        "gonk-upgrade-status"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-upgrade-status")
            .title("What an upgraded install still needs to match a fresh one")
            .summary(
                "Read-only, computed on each read: the grants that lack tokens their roles \
                 mint now (`client refresh --write`), the published scripts granted a retired \
                 exec spelling (republish them), and the ledgers whose findings name a file in \
                 browse's old form (`ledger canonical-file-iris --apply`). One line per plan \
                 that is not empty, with the command that fixes it; nothing is applied.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(crate::system::CAP_READ)
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
