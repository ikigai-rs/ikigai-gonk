//! Natural language to a DRAFT query (ledger [#1138](http://localhost:1060/l/default/item/1138)):
//! `ikigai-nl` mounted in the hub beside the script family, so every door reaches it.
//!
//! ```text
//! urn:nl:grounding        Source   what a drafter may know, under the CALLER's capability
//! urn:nl:sparql:check     Source   a SPARQL text, validated as the drafter validates it
//! urn:nl:prompt:{name}    Source   the drafting prompts, as templates
//! urn:nl:sparql           Sink     an ask -> a DRAFT script, drafted by the mounted model
//! urn:ikigai:vocab        Source   the ik: vocabulary, which the grounding states
//! ```
//!
//! The model is a **drafter, never an actor**: every step `urn:nl:sparql` takes is a request
//! through the hub under the caller's own capability: the grounding reads only what the caller
//! may read, the model is the mounted `urn:llm:` peer (the one outbound door this binary has,
//! [`crate::mount`]), the check's dry run reads through the store's graph-scoped door, and the
//! valid draft is saved as a `state=draft` script through the script family ([`crate::script`]),
//! where it is its author's alone until a person publishes it.
//!
//! # What gonk decides
//!
//! - **Drafting spends the mounted model, so it takes the grant that reaches it**:
//!   `urn:cap:net:{host}`, the host the `gonk.mount` line dials ([`crate::mount::net_grant`]).
//!   `urn:nl:sparql` DECLARES that scope here ([`DraftGuard`]), so the kernel refuses a caller
//!   without it before any grounding is computed or any model is asked, and the manifold offers
//!   the drafter to exactly the callers who may draft. Without the declaration the drafter
//!   would be offered to everyone and refused only at the mount, after the grounding.
//!   `--nl draft` mints it ([`crate::grants::nl_draft_grants`]); the anonymous HTTP caller never
//!   holds it.
//! - **No mount, no drafter.** A gonk with no `gonk.mount` binds the grounding, the check and
//!   the prompts (none of which spends anything), and not `urn:nl:sparql`, the way it binds no
//!   explain without a peer to explain with.
//! - **Which door it drafts with**: `gonk.nl.ask` (default `urn:llm:ask`, the peer's default
//!   backend), under `urn:llm:` like every provider here.
//! - **Examples from the catalog** ([`ikigai_nl::Examples::Catalog`]): the published SPARQL
//!   scripts a caller may read are the worked examples. That makes the grounding itself live
//!   (`urn:script:catalog` is), with every part beneath it cached; see the README's measurement.
//!
//! # What it reaches, and what it cannot
//!
//! Nothing here holds state or decides authority beyond the declaration above. A draft reads
//! the ledger graphs (and the browse graph) only as far as the caller's own `--ledger` (or
//! `--browse-graph`) grant does, it can be saved only under a script name the caller holds
//! `urn:cap:script:write:{name}` for (`--script <name>=publish`), and it is never published or
//! run by this module: the script family's publish rule and ceiling apply to it as they apply
//! to anything else.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, Invocation, Iri, Representation, Request, Resolution, Result, Scope,
    Space, SpaceEntry, Topology, Verb,
};

use crate::budget::AnonymousRuns;

/// The drafter's endpoint id in `ikigai-nl` (`urn:nl:sparql`).
const DRAFTER_ID: &str = "nl-sparql";

/// The check's endpoint id in `ikigai-nl` (`urn:nl:sparql:check`).
const CHECK_ID: &str = "nl-sparql-check";

/// The model door a draft is asked through when `gonk.nl.ask` says nothing: `ikigai-nl`'s own
/// default, the mounted peer's default backend.
pub const DEFAULT_ASK: &str = "urn:llm:ask";

/// What a host composes for natural language: `ikigai-nl`'s space behind gonk's drafting rule,
/// and the vocabulary its grounding states. Built once in `main` (and in the tests) and handed
/// to [`crate::compose_with`].
pub struct Nl {
    nl: Arc<dyn Space>,
    vocab: Arc<dyn Space>,
}

/// The longest one draft at `urn:nl:sparql` may spend on its model calls, ALL of them together
/// (up to three: a draft, a repair, an escalation): **300 s**, the same bound the QUIC door's
/// transport puts on one call ([`crate::mount::CALL_TIMEOUT`]). Ledger
/// [#1203](http://localhost:1062/l/default/item/1203): the HTTP door has no request deadline,
/// so before this a draft over HTTP could hold its request for three of those, fifteen minutes.
/// It bounds the draft at every door, since the drafter is the same resource behind all three;
/// [`crate::mount::within`] is how.
///
/// ```
/// assert_eq!(ikigai_gonk::nl::DRAFT_DEADLINE, ikigai_gonk::mount::CALL_TIMEOUT);
/// ```
pub const DRAFT_DEADLINE: std::time::Duration = crate::mount::CALL_TIMEOUT;

impl Nl {
    /// The family. `ask` is the model door a draft goes to (`gonk.nl.ask`); `mount_host` is
    /// the host the `gonk.mount` line dials ([`crate::config::Settings::mount_host`]), or
    /// `None` when there is no mount, and then `urn:nl:sparql` is not bound. `runs` is the
    /// anonymous run window an anonymous check's dry run counts against ([`CheckGuard`]),
    /// the script family's own in `main`.
    pub fn new(ask: &str, mount_host: Option<&str>, runs: AnonymousRuns) -> Nl {
        Nl::with_draft_deadline(ask, mount_host, runs, DRAFT_DEADLINE)
    }

    /// The family with `deadline` as a draft's overall bound instead of [`DRAFT_DEADLINE`] —
    /// for a test that cannot wait five minutes.
    pub fn with_draft_deadline(
        ask: &str,
        mount_host: Option<&str>,
        runs: AnonymousRuns,
        deadline: std::time::Duration,
    ) -> Nl {
        let config = ikigai_nl::SpaceConfig::new().llm(ikigai_nl::Llm {
            ask: ask.to_string(),
            ..ikigai_nl::Llm::default()
        });
        Nl {
            nl: Arc::new(Guarded {
                inner: Arc::new(ikigai_nl::space(config)),
                draft: mount_host.map(|host| format!("urn:cap:net:{host}")),
                deadline,
                runs,
            }),
            vocab: Arc::new(ikigai_vocab::space()),
        }
    }

    /// The spaces [`crate::compose_with`] binds, in the hub's order.
    pub fn spaces(&self) -> Vec<Arc<dyn Space>> {
        vec![Arc::clone(&self.nl), Arc::clone(&self.vocab)]
    }
}

/// `ikigai-nl`'s space with the drafter declared to need the mount's net grant, or removed
/// when there is no mount, and the check's dry run held to the anonymous budget and rate
/// ([`CheckGuard`]). An overlay with no identity or structure of its own.
struct Guarded {
    inner: Arc<dyn Space>,
    /// `urn:cap:net:{host}`, or `None` for no drafter at all.
    draft: Option<String>,
    /// A draft's overall deadline ([`DRAFT_DEADLINE`]).
    deadline: std::time::Duration,
    runs: AnonymousRuns,
}

impl Space for Guarded {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.inner.resolve(request, scope);
        if matches!(&resolution, Resolution::Hit(hit) if hit.endpoint.name() == CHECK_ID) {
            let runs = self.runs.clone();
            return resolution.map_endpoint(|endpoint| {
                Arc::new(CheckGuard {
                    inner: endpoint,
                    runs,
                }) as Arc<dyn Endpoint>
            });
        }
        let drafter =
            matches!(&resolution, Resolution::Hit(hit) if hit.endpoint.name() == DRAFTER_ID);
        if !drafter {
            return resolution;
        }
        match &self.draft {
            None => Resolution::Miss,
            Some(scope) => resolution.map_endpoint(|endpoint| {
                Arc::new(DraftGuard {
                    inner: endpoint,
                    scope: scope.clone(),
                    deadline: self.deadline,
                }) as Arc<dyn Endpoint>
            }),
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        let entries = self.inner.entries()?;
        Some(match self.draft {
            Some(_) => entries,
            None => entries
                .into_iter()
                .filter(|entry| entry.pattern != ikigai_nl::SPARQL_IRI)
                .collect(),
        })
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

/// The drafter, declaring the net grant its model call needs on every action it offers.
///
/// ★ **Declared = enforced, in both directions.** The mount refuses a call without the exact
/// host grant ([`crate::mount::net_grant`]), so the drafter always needed it; declaring it
/// moves the refusal to the kernel's floor, before the grounding is computed, and stops the
/// manifold offering a drafter the caller could only be refused by.
///
/// And it bounds every model call the draft makes by ONE overall deadline ([`DRAFT_DEADLINE`],
/// ledger #1203), through [`crate::mount::within`].
pub struct DraftGuard {
    inner: Arc<dyn Endpoint>,
    scope: String,
    deadline: std::time::Duration,
}

#[async_trait]
impl Endpoint for DraftGuard {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let by = std::time::Instant::now() + self.deadline;
        crate::mount::within(by, self.inner.invoke(inv)).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        declare(self.inner.describe(), &self.scope)
    }

    fn confinement(&self) -> Option<Topology> {
        self.inner.confinement()
    }
}

/// `urn:nl:sparql:check` for an ANONYMOUS caller: its dry run is a RUN (ledger
/// [#1210](http://localhost:1060/l/default/item/1210)).
///
/// The check evaluates the caller's own query, under a small `LIMIT`, through the store's
/// graph-scoped door, in a sub-request the hub's front door never sees — so the door's stamp
/// ([`crate::budget::stamp`]) never reached it, and a `LIMIT` bounds rows, not work: a filter
/// that keeps nothing scans everything. Before this an anonymous `GET /k?c=source
/// urn:nl:sparql:check content=…` ran a caller's query to the store's 5 s base, as often as it
/// was sent. Now it is counted against the anonymous window and runs within the door's budget,
/// exactly as a script's run does ([`crate::budget::AnonymousRuns::run`]). Anyone signed in is
/// untouched.
pub struct CheckGuard {
    inner: Arc<dyn Endpoint>,
    runs: AnonymousRuns,
}

#[async_trait]
impl Endpoint for CheckGuard {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match crate::budget::door_budget(inv.capability) {
            Some(door_ms) => self.runs.run(inv, door_ms, self.inner.invoke(inv)).await,
            None => self.inner.invoke(inv).await,
        }
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

/// `description` with `scope` required by every action it offers: added to each per-verb
/// action when it has any (an action's `requires` REPLACES the flat one for its verb, so the
/// flat form would be dead beside them), else to the flat form.
///
/// ```
/// use ikigai_core::{ActionSpec, Description, Verb};
/// use ikigai_gonk::nl::declare;
/// let drafter = Description::new("d").verb(Verb::Meta).action(ActionSpec::new(Verb::Sink));
/// let declared = declare(drafter, "urn:cap:net:localhost");
/// let actions = declared.action_specs();
/// assert_eq!(actions.len(), 1, "Meta is no action");
/// assert_eq!(actions[0].requires, ["urn:cap:net:localhost"]);
/// let flat = declare(Description::new("f").verb(Verb::Sink), "urn:cap:net:localhost");
/// assert_eq!(flat.action_specs()[0].requires, ["urn:cap:net:localhost"]);
/// ```
pub fn declare(mut description: Description, scope: &str) -> Description {
    let add = |requires: &mut Vec<String>| {
        if !requires.iter().any(|s| s == scope) {
            requires.push(scope.to_string());
        }
    };
    if description.actions.is_empty() {
        add(&mut description.requires);
    }
    for action in &mut description.actions {
        if action.verb != Verb::Meta {
            add(&mut action.requires);
        }
    }
    description
}
