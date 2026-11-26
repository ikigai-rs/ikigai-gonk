//! What an upgraded install still needs before it matches a fresh one (ledger #4369), said once
//! at startup and kept at [`STATUS`].
//!
//! A release after a breaking change has to upgrade from ANY older release. Two of the steps an
//! older install needs change what someone holds or what the ledger says, so gonk never makes
//! them on its own; it plans them read-only after the stores open and prints ONE line per plan
//! that found something, naming the exact command that makes the change:
//!
//! | plan | what an older install has | what it costs | the command |
//! | ---- | ------------------------- | ------------- | ----------- |
//! | grants | grants minted under `ikigai-ledger` 0.18 to 0.22 lack what newer roles mint (0.22's worker `session`, 0.23's session graph) | a worker's session-summary write is refused | `ikigai-gonk client refresh --write` ([`crate::refresh`]) |
//! | file IRIs | findings filed before #3657 name their file `urn:repo:{root}:file:{path}` | the file page's item list leaves them out | `ikigai-gonk ledger canonical-file-iris --apply` ([`crate::file_iris`]) |
//!
//! On current state both plans are empty and nothing is printed.
//!
//! # Not a plan: a published script's retired exec spelling
//!
//! A published script's head keeps the scopes its publisher held at publish (`Head.granted`), so
//! one published before the exec split may keep `urn:cap:exec:git` or `urn:cap:exec:gh`. That
//! costs it nothing here: every run is `runner ∩ granted ∩ ceiling`, and gonk's ceiling
//! ([`crate::script::CEILING`]) is the ledgers' reads and nothing else, so it admits neither the
//! old spelling nor the new one. There is nothing for a republish to restore, and so no line
//! (`no_script_ever_held_an_exec_scope_so_the_split_costs_none` below).
//!
//! # The resource
//!
//! `Source urn:iki:gonk:upgrade:status` (`as=application/json` for the document) computes the
//! same plans again at each read, so it says what is left after an operator has run a command.
//! It needs `urn:cap:gonk:system:read`, the operator's read, like the lapse status.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Kernel,
    ReprType, Representation, Request, Result, Verb,
};
use ikigai_ledger::Ledger;
use serde_json::{json, Value};

use crate::quic::Layout;

/// What an upgraded install still needs: `Source urn:iki:gonk:upgrade:status`.
pub const STATUS: &str = "urn:iki:gonk:upgrade:status";

const JSON: &str = "application/json";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// One stored grant that lacks tokens its ledger roles mint now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantGap {
    /// The grant's name, a key of `grants.json`.
    pub grant: String,
    /// The tokens `client refresh --write` would add to it.
    pub adds: Vec<String>,
}

/// The commands the lines name, spelled for THIS install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commands {
    /// The config home `grants.json` is under.
    pub config_home: PathBuf,
    /// The owner-only socket the ledger commands go over.
    pub socket: PathBuf,
}

impl Commands {
    /// The command that adds every missing token.
    pub fn refresh(&self) -> String {
        format!(
            "ikigai-gonk client refresh --config-home {} --write",
            self.config_home.display()
        )
    }

    /// The command that rewrites `ledger`'s old-form file IRIs.
    pub fn canonical_file_iris(&self, ledger: &str) -> String {
        format!(
            "ikigai-gonk ledger canonical-file-iris --ledger {ledger} --socket {} --apply",
            self.socket.display()
        )
    }
}

/// Every plan, read-only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plans {
    /// Grants lacking tokens, in grant-name order.
    pub grants: Vec<GrantGap>,
    /// Per ledger with any, how many finding `about`s name a file by the old form.
    pub file_iris: Vec<(String, usize)>,
    /// What could not be checked, and why: said, never fatal.
    pub unchecked: Vec<String>,
}

impl Plans {
    /// Whether nothing is left to do and nothing went unchecked.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty() && self.file_iris.is_empty() && self.unchecked.is_empty()
    }

    /// One line per non-empty plan (and per check that failed), each naming the fix. Empty on
    /// current state.
    pub fn lines(&self, commands: &Commands) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.grants.is_empty() {
            let tokens: usize = self.grants.iter().map(|g| g.adds.len()).sum();
            let names: Vec<&str> = self.grants.iter().map(|g| g.grant.as_str()).collect();
            lines.push(format!(
                "ikigai-gonk: upgrade: {} grant(s) lack {tokens} token(s) their ledger roles mint \
                 now ({}), so the requests those tokens allow are refused; add them with `{}` \
                 (without --write it lists them)",
                self.grants.len(),
                names.join(", "),
                commands.refresh()
            ));
        }
        for (ledger, count) in &self.file_iris {
            lines.push(format!(
                "ikigai-gonk: upgrade: ledger `{ledger}` has {count} finding about(s) naming a \
                 file by the old urn:repo:{{root}}:file: form, which the file page's item list \
                 leaves out; rewrite them with `{}` (without --apply it lists them)",
                commands.canonical_file_iris(ledger)
            ));
        }
        for why in &self.unchecked {
            lines.push(format!("ikigai-gonk: upgrade: not checked: {why}"));
        }
        lines
    }

    /// The text [`STATUS`] answers.
    pub fn text(&self, commands: &Commands) -> String {
        if self.is_empty() {
            return "Upgrade: nothing to do; this install matches a fresh one.\n".to_string();
        }
        let mut out = String::new();
        for line in self.lines(commands) {
            out.push_str(line.trim_start_matches("ikigai-gonk: "));
            out.push('\n');
        }
        out
    }

    /// The document [`STATUS`] answers with `as=application/json`.
    pub fn json(&self, commands: &Commands) -> Value {
        json!({
            "grants": {
                "grants": self.grants.iter().map(|g| json!({
                    "grant": g.grant,
                    "adds": g.adds,
                })).collect::<Vec<_>>(),
                "command": (!self.grants.is_empty()).then(|| commands.refresh()),
            },
            "file_iris": self.file_iris.iter().map(|(ledger, count)| json!({
                "ledger": ledger,
                "old_form": count,
                "command": commands.canonical_file_iris(ledger),
            })).collect::<Vec<_>>(),
            "unchecked": self.unchecked,
        })
    }
}

/// The grants plan: every stored grant `client refresh` would add to, from a dry run of it
/// (which takes no lock and writes nothing).
///
/// # Errors
///
/// An unreadable or unparsable `grants.json`.
pub fn grant_gaps(layout: &Layout) -> std::result::Result<Vec<GrantGap>, String> {
    let refreshed = crate::refresh::refresh(layout, false, 0)?;
    Ok(refreshed
        .plans
        .iter()
        .filter_map(|plan| {
            let adds = plan.adds();
            (!adds.is_empty()).then(|| GrantGap {
                grant: plan.grant.clone(),
                adds,
            })
        })
        .collect())
}

/// The file-IRI plan over each of `ledgers`, issued through `issue` (the hub, as root):
/// the ledgers with any old-form `about`, and what could not be read.
pub fn file_iri_counts(
    issue: &dyn Fn(Request) -> std::result::Result<Vec<u8>, String>,
    ledgers: &[String],
) -> (Vec<(String, usize)>, Vec<String>) {
    let mut found = Vec::new();
    let mut unchecked = Vec::new();
    for name in ledgers {
        let Ok(ledger) = Ledger::parse(name) else {
            continue;
        };
        match crate::file_iris::plan(issue, &ledger.graph()) {
            Ok(rewrites) if rewrites.is_empty() => {}
            Ok(rewrites) => found.push((name.clone(), rewrites.len())),
            Err(e) => unchecked.push(format!("the file IRIs of ledger `{name}`: {e}")),
        }
    }
    (found, unchecked)
}

/// Every plan, over `layout`'s grants and `hub`'s ledgers (read from `store`'s graphs), issued
/// as root. Blocking: it runs its own runtime, so it is never called on a runtime thread.
pub fn plan(layout: &Layout, store: &ikigai_store::Store, hub: Option<&Arc<Kernel>>) -> Plans {
    let mut plans = Plans::default();
    match grant_gaps(layout) {
        Ok(gaps) => plans.grants = gaps,
        Err(e) => plans.unchecked.push(format!("the stored grants: {e}")),
    }
    let Some(hub) = hub else {
        plans
            .unchecked
            .push("the file IRIs: the hub is not built yet".to_string());
        return plans;
    };
    let ledgers = match crate::lapse::ledgers(store) {
        Ok(ledgers) => ledgers,
        Err(e) => {
            plans.unchecked.push(format!("the file IRIs: {e}"));
            return plans;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            plans
                .unchecked
                .push(format!("the file IRIs: no runtime: {e}"));
            return plans;
        }
    };
    let issue = |request: Request| {
        let iri = request.target.as_str().to_string();
        runtime
            .block_on(hub.issue(request, &Capability::root()))
            .map(|answer| answer.bytes.to_vec())
            .map_err(|e| format!("{iri}: {e}"))
    };
    let (found, unchecked) = file_iri_counts(&issue, &ledgers);
    plans.file_iris = found;
    plans.unchecked.extend(unchecked);
    plans
}

/// The family behind [`STATUS`]: what it plans over, and the hub once it is built.
pub struct Upgrade {
    layout: Layout,
    store: Arc<ikigai_store::Store>,
    commands: Commands,
    hub: OnceLock<Weak<Kernel>>,
}

impl Upgrade {
    /// The family over `layout`'s grants and `store`'s ledgers, naming `commands`.
    pub fn new(layout: Layout, store: Arc<ikigai_store::Store>, commands: Commands) -> Arc<Self> {
        Arc::new(Upgrade {
            layout,
            store,
            commands,
            hub: OnceLock::new(),
        })
    }

    /// Hand the family the hub it is bound in. A second call is a no-op.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }

    /// Every plan, now. Blocking ([`plan`]).
    pub fn plans(&self) -> Plans {
        let hub = self.hub.get().and_then(Weak::upgrade);
        plan(&self.layout, &self.store, hub.as_ref())
    }

    /// The lines startup prints: one per non-empty plan, none on current state.
    pub fn lines(&self) -> Vec<String> {
        self.plans().lines(&self.commands)
    }

    /// [`STATUS`] as a space for the hub.
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
        if !inv.capability.allows(crate::system::CAP_READ) {
            return Err(Error::Denied(
                format!(
                    "{STATUS} names this install's grants and ledgers, so it needs `{}`",
                    crate::system::CAP_READ
                )
                .into(),
            ));
        }
        let upgrade = Arc::clone(&self.0);
        // ★ On a thread of its own, as the backup's dump is: the plan runs its own runtime to
        // issue through the hub, which a door's worker must not block on.
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("gonk-upgrade-plan".to_string())
            .spawn(move || {
                let _ = tx.send(upgrade.plans());
            })
            .map_err(|e| Error::Endpoint(format!("{STATUS}: no thread for the plan: {e}")))?;
        let plans = rx
            .await
            .map_err(|_| Error::Endpoint(format!("{STATUS}: the plan panicked")))?;
        if matches!(inv.inline_str("as"), Ok(JSON)) {
            let mut bytes = serde_json::to_vec(&plans.json(&self.0.commands)).unwrap_or_default();
            bytes.push(b'\n');
            Ok(Representation::new(
                ReprType::new(JSON).with_param("charset", "utf-8"),
                bytes,
            ))
        } else {
            Ok(Representation::new(
                ReprType::new("text/plain").with_param("charset", "utf-8"),
                plans.text(&self.0.commands).into_bytes(),
            ))
        }
    }

    fn name(&self) -> &str {
        "gonk-upgrade-status"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-upgrade-status")
            .title("What this install still needs after an upgrade")
            .summary(
                "The steps an install upgraded from an older release still needs to match a \
                 fresh one, each with the exact command that makes it: stored grants that lack \
                 tokens their roles mint now (`client refresh --write`), and findings that name \
                 their file by the old IRI form (`ledger canonical-file-iris --apply`). Planned \
                 read-only at each read; nothing is applied.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{grants_for, Authority};

    fn commands() -> Commands {
        Commands {
            config_home: PathBuf::from("/cfg"),
            socket: PathBuf::from("/run/gonk.sock"),
        }
    }

    fn layout_with(grants: &Value) -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        std::fs::create_dir_all(layout.grants_json().parent().unwrap()).unwrap();
        std::fs::write(layout.grants_json(), grants.to_string()).unwrap();
        (dir, layout)
    }

    #[test]
    fn a_worker_grant_minted_under_0_22_is_found_and_named() {
        let old: Vec<String> = grants_for("acme", Authority::Worker)
            .unwrap()
            .into_iter()
            .filter(|t| !t.ends_with(":session"))
            .collect();
        let (_dir, layout) = layout_with(&json!({ "w": old }));
        let gaps = grant_gaps(&layout).unwrap();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].grant, "w");
        let plans = Plans {
            grants: gaps,
            ..Plans::default()
        };
        let lines = plans.lines(&commands());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("1 grant(s) lack 2 token(s)"),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("`ikigai-gonk client refresh --config-home /cfg --write`"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn current_grants_plan_nothing_and_print_nothing() {
        let current = grants_for("acme", Authority::Worker).unwrap();
        let (_dir, layout) = layout_with(&json!({ "w": current }));
        assert!(grant_gaps(&layout).unwrap().is_empty());
        assert!(Plans::default().lines(&commands()).is_empty());
        assert!(Plans::default().text(&commands()).contains("nothing to do"));
    }

    #[test]
    fn old_form_file_iris_name_the_ledger_and_the_command() {
        let plans = Plans {
            file_iris: vec![("default".to_string(), 3)],
            ..Plans::default()
        };
        let lines = plans.lines(&commands());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("ledger `default` has 3 finding about(s)"));
        assert!(lines[0].contains(
            "`ikigai-gonk ledger canonical-file-iris --ledger default --socket /run/gonk.sock \
             --apply`"
        ));
        assert_eq!(
            plans.json(&commands())["file_iris"][0]["old_form"],
            json!(3)
        );
    }

    /// Claim 2 of ledger #4369 does not hold: the script ceiling admits no exec scope in either
    /// spelling, so a published head keeping `urn:cap:exec:git` loses nothing to the split.
    #[test]
    fn no_script_ever_held_an_exec_scope_so_the_split_costs_none() {
        let ceilings = [
            crate::script::ceiling(),
            crate::script::ceiling_over(["default", "acme"]),
        ];
        for ceiling in &ceilings {
            for (old, new) in crate::grants::RETIRED_EXEC {
                assert!(!ceiling.allows(old), "{old}");
                assert!(!ceiling.allows(new), "{new}");
            }
        }
    }
}
