//! Scripts as resources (ledger [#939](http://localhost:1060/l/default/item/939)):
//! `ikigai-script` mounted in the hub beside the ledger and browse, so every door reaches it.
//!
//! ```text
//! urn:script:{name}                    Source Exists Sink Delete  fetch WITHOUT running, publish, retire
//! urn:script:{name}:version:{digest}   Source Exists              one immutable version
//! urn:script:{name}:compiled           Source                     the head prepared for its evaluator
//! urn:script:{name}:result             Source                     run it as a READ
//! urn:script:{name}:runs               Sink                       run it for EFFECTS, recorded
//! urn:script:{name}:run:{id}           Source Exists              one recorded run: who, which version, what
//! urn:script:eval                      Sink                       Lisp, ad hoc — which this server binds no evaluator for
//! urn:script:catalog                   Source                     every script the caller may read
//! urn:plan:{eval,validate,requires}                               the plan doors a plan script reaches
//! ```
//!
//! # What gonk decides, and what it composes
//!
//! The library holds two atoms (a script's head and versions, a run) and composes the rest.
//! A host decides four things, and these are gonk's answers:
//!
//! - **Where the atoms live: a directory of plain files** ([`DIR_NAME`] under the data home, or
//!   `gonk.script.dir`), through the library's own `DirBackend`. Not the RDF store: see
//!   [`Scripts::open`] for why.
//! - **The languages: a query and a plan.** `language=sparql` runs through the store's
//!   graph-scoped doors (`SparqlDoor::store`), `language=plan` through `ikigai-engine`'s plan
//!   doors, validated by `urn:shacl:validate`. **No Lisp**: this server binds no `urn:lisp:eval`,
//!   and a Lisp publish is refused by name ([`Rule::Publish`]) rather than stored to fail at
//!   every run.
//! - **The ceiling: the ledgers' reads, and nothing else** ([`CEILING`]). Every run is
//!   `runner ∩ what the publisher held ∩ this`, so no script here writes anything, whoever
//!   publishes or runs it. See [`CEILING`] for why "writes only through the ledger's own doors"
//!   is not a ceiling this server can state.
//! - **Who a request is from: the principal the door minted** into the capability (gonk PR 122,
//!   ledger [#1077](http://localhost:1060/l/default/item/1077)) — the passkey at HTTP, the
//!   client certificate at QUIC, nobody at the owner's socket. The library records it on every
//!   publish and run and makes a draft its author's alone.
//!
//! # Three rules gonk adds, each because a door rule cannot see a sub-request
//!
//! gonk's author and raw-write rules ([`crate::admit`]) run where a request ARRIVES. A plan's
//! steps are sub-requests inside the hub, out of their sight, so a plan that WRITES would carry
//! them past both: a step `Sink urn:iki:store:graph-update` with the caller's ledger write token
//! is a raw write to a ledger graph, and a step `Sink urn:iki:ledger:comment author=<someone's
//! passkey>` is a forged author. So:
//!
//! 1. **A plan that writes runs only as root** ([`Rule::PlanEval`], at every depth): the
//!    owner's socket. A read-only plan runs for anyone, under their own capability — exactly the
//!    requests they could make one at a time.
//! 2. **A script that writes is refused at publish** ([`Rule::Publish`]): a SPARQL UPDATE, or a
//!    plan with a `Sink` or `Delete` step. The ceiling would refuse every run of it anyway; this
//!    says so once, at the moment someone could act on it.
//! 3. **A machine drafts, a person publishes** ([`crate::admit`]'s QUIC rule): a QUIC client
//!    may save a draft (`state=draft`) and may not publish or retire.
//!    The design's rule for agents (an agent drafts, a human publishes), enforced at the one door
//!    whose callers are machines.
//!
//! And one at the door boundary, in [`crate::doors`]: `urn:shacl:*` is never served to a
//! caller (it is linked for the plan doors, and only their sub-requests reach it). The
//! transports' own arguments (`principal`, and a write's `received`, `client`,
//! `content-type`) pass the doors untouched: since `ikigai-script` 0.3.0 a run ignores them
//! (`ikigai_script::TRANSPORT_ARGUMENTS`), so the strip gonk carried through 0.2.x is gone
//! (ledger #1196).
//!
//! # What a caller can and cannot do here
//!
//! - **A plan typed ad hoc** runs at `urn:plan:eval` itself, under the caller's own capability
//!   (read-only below root, rule 1). `urn:script:eval` is the library's paste box for LISP: it
//!   refuses `language=plan` (pointing at `urn:plan:eval`) and `language=sparql`, and declares
//!   `urn:cap:lisp`, which no grant here mints — so in gonk it is the owner's, and answers that
//!   there is no evaluator.
//! - **Grants are per script name or per namespace** ([`crate::grants::script_grants`]):
//!   `urn:cap:script:{act}:{name}` or `urn:cap:script:{act}:{namespace}-*`, decided by the
//!   library's `authority::holds` (ikigai-script 0.3.0). There is no "publish anything" grant
//!   below root: `urn:cap:script:{act}:*` grants nothing.

use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Description, Endpoint, Error, Invocation, Iri, Kernel, Representation, Request,
    Resolution, Result, Scope, Space, SpaceEntry, Topology, Verb, BINDINGS_THREAD,
};
use ikigai_script::authority::{self, Ceiling};
use ikigai_script::sparql::{Form, SparqlDoor};
use ikigai_script::{Backend, DirBackend, MemoryBackend, SpaceConfig};
use ikigai_throttle::Rate;

use crate::budget::AnonymousRuns;

/// The scripts directory's name under the data home: `~/.ikigai/scripts`, gonk's alone (the
/// library's `DirBackend` takes one host process per directory).
pub const DIR_NAME: &str = "scripts";

/// ★ **The ceiling every script runs under: the ledgers' reads, and nothing else.**
///
/// ```
/// use ikigai_gonk::script::ceiling;
/// let ceiling = ceiling();
/// assert!(ceiling.allows("urn:cap:ledger:read:default"));
/// assert!(ceiling.allows("urn:cap:store:read:graph:urn:iki:ledger:graph:default"));
/// // Nothing that writes, nothing outside the ledgers, nothing that spends.
/// assert!(!ceiling.allows("urn:cap:ledger:write:default"));
/// assert!(!ceiling.allows("urn:cap:store:write:graph:urn:iki:ledger:graph:default"));
/// assert!(!ceiling.allows("urn:cap:store:read:graph:urn:iki:browse:graph:default"));
/// assert!(!ceiling.allows("urn:cap:net:localhost"));
/// assert!(!ceiling.allows("urn:cap:lisp"));
/// ```
///
/// # Why "writes only through the ledger's own doors" is not something a ceiling can say
///
/// `ikigai-ledger` writes every append, comment and close as a `urn:iki:store:graph-update`
/// sub-request under the CALLER's capability, so the ledger's own write door needs the ledger
/// graph's store write token — the same token a raw SPARQL UPDATE of that graph needs. A ceiling
/// that admits one admits the other. gonk tells them apart only where a request arrives (the
/// raw-write rule, ledger [#878](http://localhost:1060/l/default/item/878)), and a script's
/// steps never arrive anywhere. So this ceiling admits no write at all, and a script that writes
/// is refused when it is published. Ledger writes from a script need either a ledger write that
/// does not travel as the caller's store token, or a way for gonk to see a sub-request's origin:
/// both are reported up, not built here.
///
/// The graveyards (`urn:iki:ledger:graph:{name}:deleted`) match the prefix too; a run reaches
/// one only when its runner could read it directly, which only a purge grant does.
pub const CEILING: [&str; 2] = [
    "urn:cap:ledger:read:*",
    "urn:cap:store:read:graph:urn:iki:ledger:graph:*",
];

/// [`CEILING`] as the library's type.
pub fn ceiling() -> Ceiling {
    Ceiling::scoped(CEILING)
}

/// What the ANONYMOUS HTTP caller holds of the script family: run and read a script its
/// publisher marked `public=true`. Nothing else — it cannot publish, see a draft or run a
/// private script.
///
/// A public run is cheap to repeat by construction: a query's `…:result` is cached under the
/// store's write threads, so a flood of the same read is answered from the hub's cache. What is
/// NOT cached — new argument values, a run for effects — is bounded twice (ledger
/// [#1175](http://localhost:1060/l/default/item/1175)):
///
/// - **in time**: every SPARQL evaluation of an anonymous run takes the anonymous door's budget
///   (`gonk.http.anonymous_sparql_budget_ms`, 1 s by default), not the store's 5 s base.
///   The run's capability is attenuated to what the script derives, which drops the door's
///   marker, so the budget rides on the TASK instead ([`crate::budget::within_run`]);
/// - **in rate**: anonymous runs share one window ([`crate::budget::AnonymousRuns`], an
///   `ikigai-throttle` `RateLimit`), [`ANONYMOUS_RUNS`] unless the config says otherwise.
///
/// An ad hoc plan at `urn:plan:eval` is a run in both senses too ([`Rule::PlanEval`], ledger
/// [#1210](http://localhost:1060/l/default/item/1210)): before, its steps' SPARQL ran at the
/// store's base and any number of plans could be sent.
pub const ANONYMOUS: [&str; 2] = [authority::CAP_RUN_PUBLIC, authority::CAP_READ_PUBLIC];

/// How often ANONYMOUS callers may run, all of them together, unless
/// `gonk.script.anonymous_runs_per_minute` says otherwise: **30 runs a minute** (ledger
/// [#1175](http://localhost:1060/l/default/item/1175); configurable and shared with ad hoc
/// plans and draft dry runs since [#1210](http://localhost:1060/l/default/item/1210)). Past it
/// a run is refused, naming the limit and when to retry, until the oldest run in the window
/// ages out.
///
/// One window for every anonymous caller, because the door cannot tell them apart: the HTTP
/// door is loopback-only and an anonymous request carries no identity. It counts a script's
/// `…:result` / `…:runs`, `urn:plan:eval` and `urn:nl:sparql:check` alike
/// ([`crate::budget::AnonymousRuns`]). A signed-in person, a QUIC client and the owner's socket
/// are not limited by it, and neither is reading a script's source or its catalog, which runs
/// nothing.
///
/// Why 30 a minute: each run may hold a core for up to the anonymous budget (1 s by default),
/// so the limit keeps anonymous runs to about half a core on average whatever is sent, while a
/// person clicking through public scripts in a browser never meets it. A run answered from the
/// hub's cache is never counted: the limit is consulted when the run's endpoint is entered,
/// which a cached answer skips.
pub const ANONYMOUS_RUNS: Rate = Rate {
    max: crate::budget::DEFAULT_ANONYMOUS_RUNS_PER_MINUTE,
    window: std::time::Duration::from_secs(60),
};

/// Whether `target` RUNS a script: `urn:script:{name}:result` or `urn:script:{name}:runs`.
///
/// By the IRI and not by the endpoint's id, because a published script is its own catalog
/// entry: its run answers as `script-{name}-result`, a name the library mints per script.
/// A script name is one segment with no `:` (`ikigai_script::name::validate`), so the part
/// after the first `:` is the whole suffix.
///
/// ```
/// use ikigai_gonk::script::is_run;
/// assert!(is_run("urn:script:open-by-label:result"));
/// assert!(is_run("urn:script:triage:runs"));
/// assert!(!is_run("urn:script:triage"));
/// assert!(!is_run("urn:script:triage:compiled"));
/// assert!(!is_run("urn:script:triage:run:7"));
/// assert!(!is_run("urn:script:catalog"));
/// ```
pub fn is_run(target: &str) -> bool {
    target
        .strip_prefix(ikigai_script::name::PREFIX)
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(_, part)| part == "result" || part == "runs")
}

/// The script family, composed: the library's space behind gonk's publish rule, the plan doors
/// behind the plan rule, and the validator the plan doors compose. Built once in `main` (and in
/// the tests), handed to [`crate::compose_with`], and [`attached`](Scripts::attach) to the hub
/// once it exists.
pub struct Scripts {
    scripts: Arc<dyn Space>,
    plans: Arc<dyn Space>,
    shacl: Arc<dyn Space>,
    /// The hub, once it exists: what the library's change hook cuts `urn:kernel:bindings` on.
    hub: Arc<OnceLock<Weak<Kernel>>>,
}

impl Scripts {
    /// The family over the directory `dir`, created if it is missing, its anonymous runs
    /// counted in `runs` (shared with [`crate::nl::Nl`] in `main`).
    ///
    /// # Why plain files, and not gonk's store
    ///
    /// The ledger's atoms are quads in the store. The script atoms are not, for three reasons
    /// this server would have to pay for:
    ///
    /// - **The record IS the authority.** A version stores what its publisher held at publish,
    ///   and the runs are the provenance. A store-backed atom is a named graph, and every
    ///   per-graph token this server mints (and the `urn:cap:store:read:graph:*` an operator may
    ///   have written into `grants.json`) would have to be refused for it, at startup and per
    ///   connection, or a grant could rewrite a snapshot or forge a run. A live `grants.json`
    ///   that names that wildcard would then stop the server at its next start.
    /// - **It would have to leave by the raw handle.** The store's own doors check the caller's
    ///   capability, and the library writes on nobody's behalf, so a backend would write
    ///   through the `Arc<Store>` that only `open_shared_declaring` hands out — making every
    ///   gonk a SHARED store (today only one with browse roots is), whose whole-dataset reads
    ///   are never cached.
    /// - **The library already argues it** (`ikigai-script`'s README, "Storage"), and its
    ///   `DirBackend` is the backend it tests: compare-and-set heads, digest-checked versions,
    ///   a broken head failing that script alone.
    ///
    /// The cost, and how it is paid: a store backup is the dataset's N-Quads, so it cannot hold
    /// these files. The backup family snapshots this directory beside every store archive, under
    /// the same stamp, and restores and verifies it on its own (`crate::backup`, ledger
    /// [#1175](http://localhost:1060/l/default/item/1175)).
    ///
    /// # Errors
    ///
    /// When the directory cannot be created.
    pub fn open(dir: &Path, runs: AnonymousRuns) -> std::result::Result<Scripts, String> {
        let backend = DirBackend::open(dir)
            .map_err(|e| format!("cannot open the scripts directory {}: {e}", dir.display()))?;
        Ok(Scripts::over(Arc::new(backend), runs))
    }

    /// The family in memory, at the default anonymous rate ([`ANONYMOUS_RUNS`]): for tests,
    /// and for nothing else this server runs.
    pub fn in_memory() -> Scripts {
        Scripts::in_memory_with(AnonymousRuns::default())
    }

    /// The family in memory, its anonymous runs counted in `runs`.
    pub fn in_memory_with(runs: AnonymousRuns) -> Scripts {
        Scripts::over(Arc::new(MemoryBackend::new()), runs)
    }

    fn over(backend: Arc<dyn Backend>, runs: AnonymousRuns) -> Scripts {
        let hub: Arc<OnceLock<Weak<Kernel>>> = Arc::new(OnceLock::new());
        let cell = Arc::clone(&hub);
        let config = SpaceConfig::new(backend, authority::same_for_all(ceiling()))
            .sparql(SparqlDoor::store())
            // ★ A published query or plan is its OWN catalog entry, with its parameters as
            // arguments; the kernel caches descriptions under `urn:kernel:bindings`, which only
            // the host can cut. Unwired, a new script runs at once but the catalog, MCP and the
            // engine's argument routing describe the old set.
            .on_change(Arc::new(move |_name| {
                if let Some(hub) = cell.get().and_then(Weak::upgrade) {
                    hub.cut(BINDINGS_THREAD);
                }
            }));
        let library: Arc<dyn Space> = Arc::new(ikigai_script::space(config));
        // ★ The anonymous window is consulted from inside the run's endpoint, where the
        // caller's capability is known: a space overlay resolves before any capability exists.
        Scripts {
            scripts: Arc::new(Guarded {
                inner: library,
                rule: Rule::Publish,
                runs: runs.clone(),
            }),
            plans: Arc::new(Guarded {
                inner: Arc::new(ikigai_engine::plan_space::space()),
                rule: Rule::PlanEval,
                runs,
            }),
            shacl: Arc::new(ikigai_shacl::space()),
            hub,
        }
    }

    /// The spaces [`crate::compose_with`] binds, in the hub's order.
    pub fn spaces(&self) -> Vec<Arc<dyn Space>> {
        vec![
            Arc::clone(&self.scripts),
            Arc::clone(&self.plans),
            Arc::clone(&self.shacl),
        ]
    }

    /// Hand the family the hub it is bound in, so a publish or a retire cuts
    /// `urn:kernel:bindings` there. Called once, after the hub is built; a second call is a
    /// no-op.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }
}

/// Which of gonk's rules a guarded space applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// On the script family: a publish (`Sink urn:script:{name}`) of Lisp, of a SPARQL UPDATE or
    /// of a plan that writes is refused. See [`publish_refusal`].
    Publish,
    /// On the plan doors: `urn:plan:eval` of a plan that writes is refused below root, at every
    /// depth. See [`plan_refusal`].
    PlanEval,
}

/// A space whose endpoints answer gonk's [`Rule`] before they run — an overlay with no
/// identity or structure of its own, like [`crate::admit::Admitting`] — and hold an anonymous
/// RUN (a script's, or an ad hoc plan's) to the door's budget and the anonymous rate.
struct Guarded {
    inner: Arc<dyn Space>,
    rule: Rule,
    runs: AnonymousRuns,
}

impl Space for Guarded {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let rule = self.rule;
        let runs = self.runs.clone();
        self.inner.resolve(request, scope).map_endpoint(|endpoint| {
            Arc::new(Guard {
                inner: endpoint,
                rule,
                runs,
            }) as Arc<dyn Endpoint>
        })
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

struct Guard {
    inner: Arc<dyn Endpoint>,
    rule: Rule,
    runs: AnonymousRuns,
}

impl Guard {
    /// Whether `request` RUNS something here: a script's `…:result` / `…:runs` ([`is_run`]) on
    /// the script family, `urn:plan:eval` on the plan doors (validating or deriving a plan's
    /// requirements runs none of its steps).
    fn is_a_run(&self, request: &Request) -> bool {
        match self.rule {
            Rule::Publish => is_run(request.target.as_str()),
            Rule::PlanEval => self.inner.name() == PLAN_EVAL_ID,
        }
    }
}

#[async_trait]
impl Endpoint for Guard {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let refusal = match self.rule {
            Rule::Publish => publish_refusal(self.inner.name(), inv.request),
            Rule::PlanEval => plan_refusal(self.inner.name(), inv.request, inv.capability),
        };
        if let Some(error) = refusal {
            return Err(error);
        }
        // ★ An ANONYMOUS run (ledgers #1175, #1210): the caller's capability carries the HTTP
        // door's budget marker. A script run's own capability is attenuated and drops it, and a
        // plan's steps keep it but are sub-requests the hub's front door never stamps — either
        // way the store would see the base. So the window is consulted here, once per run that
        // is not answered from the cache, and the run goes on inside the door's budget, which
        // the store's overlay stamps on every evaluation it makes.
        if let Some(door_ms) = crate::budget::door_budget(inv.capability) {
            if self.is_a_run(inv.request) {
                return self.runs.run(inv, door_ms, self.inner.invoke(inv)).await;
            }
        }
        self.inner.invoke(inv).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }

    fn confinement(&self) -> Option<Topology> {
        self.inner.confinement()
    }
}

/// The `ikigai-script` endpoint a publish reaches (`urn:script:{name}`).
const SCRIPT_ID: &str = "script";
/// The plan door that runs a plan (`urn:plan:eval`).
const PLAN_EVAL_ID: &str = "plan-eval";

fn inline<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    match request.args.get(name) {
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// Why a publish through the script family is refused here, or `None` to let the library
/// decide. Only a `Sink` to `urn:script:{name}` (the endpoint `script`) is looked at, and only
/// three things are refused, each by what the TEXT is:
///
/// ```
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::script::publish_refusal;
/// let publish = |language: &str, content: &str| {
///     Request::new(Verb::Sink, Iri::parse("urn:script:x").unwrap())
///         .with_arg("language", ArgRef::Inline(language.as_bytes().to_vec()))
///         .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()))
/// };
/// let query = "SELECT ?s FROM <urn:iki:ledger:graph:default> WHERE { ?s ?p ?o }";
/// assert!(publish_refusal("script", &publish("sparql", query)).is_none());
/// let update = "INSERT DATA { GRAPH <urn:iki:ledger:graph:default> { <urn:a> <urn:b> <urn:c> } }";
/// assert!(publish_refusal("script", &publish("sparql", update)).is_some());
/// assert!(publish_refusal("script", &publish("lisp", "(+ 1 2)")).is_some());
/// // No language says Lisp, the library's default.
/// let bare = Request::new(Verb::Sink, Iri::parse("urn:script:x").unwrap())
///     .with_arg("content", ArgRef::Inline(b"(+ 1 2)".to_vec()));
/// assert!(publish_refusal("script", &bare).is_some());
/// // A read of the same name, or another endpoint, is not this rule's.
/// assert!(publish_refusal("script-runs", &publish("sparql", update)).is_none());
/// ```
///
/// A text the library would refuse anyway (it does not parse, or is no plan) passes here
/// untouched, so the refusal a caller reads is the library's own.
pub fn publish_refusal(endpoint: &str, request: &Request) -> Option<Error> {
    if endpoint != SCRIPT_ID || request.verb != Verb::Sink {
        return None;
    }
    let language = inline(request, "language").map_or("lisp", str::trim);
    let content = inline(request, "content").unwrap_or("");
    match language {
        "lisp" => Some(Error::InvalidArgument {
            name: "language".to_string(),
            detail: "this server runs `sparql` and `plan` scripts and binds no Lisp evaluator \
                     (`urn:lisp:eval`); a Lisp script published here could never run. \
                     (`lisp` is the library's default when no `language` is given.)"
                .to_string(),
        }),
        "sparql" => match ikigai_script::sparql::analyze(content, &SparqlDoor::store()) {
            Ok(analysis) if analysis.form == Form::Update => Some(read_only("a SPARQL UPDATE")),
            _ => None,
        },
        "plan" => match ikigai_script::plan::analyze(content) {
            Ok(analysis) if analysis.mutates() => {
                Some(read_only("a plan with a `Sink` or `Delete` step"))
            }
            _ => None,
        },
        _ => None,
    }
}

fn read_only(what: &str) -> Error {
    Error::Denied(format!(
        "{what} cannot be published here: this server's scripts are READ-ONLY. Every run is \
         held to a ceiling of the ledgers' reads (`{}`), because the ledger's own write door \
         and a raw write of its graph need the same store token, and gonk tells them apart \
         only where a request arrives, which a script's steps never do. Write through the \
         ledger's own doors (`urn:iki:ledger:*`)",
        CEILING.join("`, `")
    ))
}

/// Why `urn:plan:eval` (the endpoint `plan-eval`) may not run the plan in `in` under
/// `capability`, or `None`.
///
/// ```text
/// root (the owner's socket)                            → runs, whatever the plan does
/// a plan whose every step reads                        → runs, under the caller's capability
/// a plan with a Sink or Delete step anywhere           → refused
/// a plan that is not given inline, or not readable     → refused (it cannot be checked)
/// ```
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::script::plan_refusal;
/// let plan = |verb: &str| format!(
///     "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
///      <urn:plan:p> a ik:Process ; ik:step <urn:plan:p:step:1> ; ik:result <urn:plan:p:step:1> .\n\
///      <urn:plan:p:step:1> a ik:Step ; ik:verb \"{verb}\" ; ik:resolves <urn:iki:ledger:items> .\n");
/// let eval = |text: String| Request::new(Verb::Source, Iri::parse("urn:plan:eval").unwrap())
///     .with_arg("in", ArgRef::Inline(text.into_bytes()));
/// let caller = Capability::scoped(["urn:cap:ledger:write:default"]);
/// assert!(plan_refusal("plan-eval", &eval(plan("Source")), &caller).is_none());
/// assert!(plan_refusal("plan-eval", &eval(plan("Sink")), &caller).is_some());
/// assert!(plan_refusal("plan-eval", &eval(plan("Sink")), &Capability::root()).is_none());
/// // Validation and derivation run nothing: not this rule's.
/// assert!(plan_refusal("plan-validate", &eval(plan("Sink")), &caller).is_none());
/// ```
pub fn plan_refusal(
    endpoint: &str,
    request: &Request,
    capability: &ikigai_core::Capability,
) -> Option<Error> {
    if endpoint != PLAN_EVAL_ID || capability.is_root() {
        return None;
    }
    let refuse = |why: String| {
        Some(Error::Denied(format!(
            "{why}. A plan's steps run inside this server as sub-requests, out of sight of the \
             author and raw-write rules every door applies, so below root (the owner's socket) \
             only a plan whose every step reads may run here"
        )))
    };
    match request.args.get("in") {
        // No plan: the door refuses that itself, naming the argument.
        None => None,
        Some(ArgRef::Inline(bytes)) => match std::str::from_utf8(bytes)
            .map_err(|e| e.to_string())
            .and_then(|text| ikigai_script::plan::analyze(text).map_err(|e| e.to_string()))
        {
            Ok(analysis) if analysis.mutates() => {
                refuse("this plan has a `Sink` or `Delete` step".to_string())
            }
            Ok(_) => None,
            Err(e) => refuse(format!("this plan could not be read to check it ({e})")),
        },
        Some(_) => refuse("a plan run here must be given inline".to_string()),
    }
}
