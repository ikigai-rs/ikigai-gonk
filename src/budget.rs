//! The anonymous door's SPARQL TIME budget (ledger [#964](http://localhost:1060/l/default/item/964),
//! [#979](http://localhost:1060/l/default/item/979)).
//!
//! Since `ikigai-store` 0.2.8 every SPARQL evaluation runs within a time budget: the store's
//! 5 s base for a capability holding no `urn:cap:store:budget:<ms>` grant, 120 s for root, and
//! a request's own `budget=` can only LOWER what its capability gets. An anonymous loopback
//! caller of gonk's HTTP door can read the ledgers in `gonk.http.ledger`, so it can send the
//! store a query that costs nothing to write and minutes to answer — a `VALUES` cross product
//! carries its own data and needs no graph at all. At the base it holds a core for 5 s per
//! request. This module gives it a smaller budget, **1 s by default**
//! ([`DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS`], `gonk.http.anonymous_sparql_budget_ms`).
//!
//! # Where: a stamp at the hub's front door, carried by a marker the HTTP door computes
//!
//! **Why not a grant.** The store's budget comes from the capability and is MONOTONE in grants:
//! a grant can only raise it. So "anonymous gets less" through a capability means lowering the
//! store's base for every caller and granting everyone else back up — and the base is also what
//! gonk's OWN ledger queries run under when an anonymous caller opens a page (the listing, the
//! item page, `next`), so they would be capped at the anonymous budget too. The request's
//! `budget=` argument is the one lever that lowers ONE evaluation and nothing else.
//!
//! **Why at [`crate::doors::HubSpace`]'s forward.** Every request any door sends the hub passes
//! through it — the HTTP door's mechanical `/iki/store/…` mapping, the SPARQL page and its
//! protocol face, `/k`, and `urn:sparql:*` — and it knows the target's endpoint by its id (the
//! hub's own description), so the decision is made by WHAT is evaluated, not by how its IRI was
//! spelled. A sub-request the hub's own endpoints make (`ikigai-ledger`'s queries, browse's,
//! the backup's) never passes through it, so gonk's own queries are never stamped. `urn:sparql:*`
//! forwards the stamped `budget` to the store itself ([`crate::sparql`]).
//!
//! **Who is anonymous: a marker the door computes** ([`anonymous_marker`],
//! `urn:iki:gonk:door:anonymous-budget:<ms>`), added by [`crate::doors::http_scopes`] to the
//! capability of an HTTP request that carries no live passkey session. It is under
//! [`crate::admit::DOOR_PREFIX`], so no `grants.json` grant may name it, and a caller of the HTTP
//! door cannot narrow the capability the door computes — so it can neither forge the marker nor
//! drop it. A signed-in caller carries none, and gets the store's base; the owner's socket is
//! root (the ceiling); a QUIC client is authenticated by certificate and gets its grant's
//! budget. No other door is unauthenticated.
//!
//! # The stamp OVERWRITES ([`stamp_budget`])
//!
//! A caller's own `budget=` is kept only when it is an inline whole number of milliseconds no
//! larger than the door's. Anything else — larger, zero, malformed, not UTF-8, or BY REFERENCE —
//! is replaced by the door's. ★ The last is the one that matters: `ikigai-sparql` reads a
//! by-reference `budget=` as no budget at all and falls to its ceiling, so an add-if-missing
//! stamp there is bypassed by sending one (ikigai-cms-web PR 98). `ikigai-store` 0.2.8 instead
//! REFUSES a by-reference `budget=` (`InvalidArgument`, "read by value only"), which is the
//! safer answer, but the stamp does not rely on it.
//!
//! # The answer's SIZE is the store's base, and nothing is stamped for it
//!
//! Since `ikigai-store` 0.2.9 every SPARQL answer is bounded in rows and serialized bytes too:
//! **100,000 rows and 16 MiB** for a capability holding no `urn:cap:store:answer:*` grant,
//! refused past either (`InvalidArgument` on `query`), never truncated (ledger
//! [#970](http://localhost:1060/l/default/item/970), [#993](http://localhost:1060/l/default/item/993)).
//! An anonymous caller holds no such grant, so it gets the base, and gonk stamps no `max_rows=`
//! or `max_bytes=` below it. The reason is what an anonymous caller may legitimately ask for:
//! the largest answer over the ledgers it can read is the whole ledger graph, measured
//! 2026-10-10 on the 2026-10-09 archive restored into RocksDB at 13,375 rows and 4,986,073
//! bytes of SPARQL JSON — inside the base 7.5 times over on rows and 3.4 times on bytes. A lower
//! stamp would refuse that read sooner as the ledger grows while the ledger's pages hand out the
//! same data, and buys little: the base already caps what one request can hold in memory, since
//! the store refuses the write that would cross it. The time budget above is what limits how
//! much work an anonymous caller can make, and `tests/door_budget_964.rs` pins the size refusal
//! on every route. ⚠ The base binds gonk's own ledger reads under an anonymous caller's
//! capability too; they are far smaller than the whole graph, but they grow with it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Bindings, Capability, Endpoint, Error, Invocation, Iri, Representation, Request,
    Resolution, Resolved, Scope, Space, Verb,
};
use ikigai_throttle::{Rate, RateLimit};

/// What an anonymous caller's SPARQL evaluation may take, unless `gonk.http.anonymous_sparql_
/// budget_ms` says otherwise: **1000 ms**, the reading room's choice for the same door shape
/// (ikigai-cms-web PR 98).
///
/// The evidence: a page an anonymous caller asks for sends its own queries under the store's
/// 5 s base, never this (see the module docs), and a caller's own query over a ledger graph —
/// the only data an anonymous caller may read — measured well under this on the live dataset
/// (the README's "How long a SPARQL query may run" has the numbers: 2–8 ms for the SPARQL
/// page's sample queries over the live ledger).
pub const DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS: u64 = 1_000;

/// The most `gonk.http.anonymous_sparql_budget_ms` may say: the store's base, 5000 ms. Above
/// it a stamp could never apply — `budget=` only lowers what the capability gets — so a larger
/// value is refused at startup rather than silently meaning the base.
pub const MAX_ANONYMOUS_SPARQL_BUDGET_MS: u64 = 5_000;

/// The marker's prefix: `urn:iki:gonk:door:anonymous-budget:<ms>`.
pub const ANONYMOUS_BUDGET_PREFIX: &str = "urn:iki:gonk:door:anonymous-budget:";

/// The ids of every endpoint that EVALUATES a caller's SPARQL: the store's eight read forms and
/// two update forms (by `ikigai-store`'s own ids, as [`crate::sparql::STORE_RULES`] names them),
/// and gonk's four `urn:sparql:*` forms, which forward to the store's graph-scoped twins.
pub const SPARQL_IDS: [&str; 14] = [
    "store-select",
    "store-ask",
    "store-construct",
    "store-describe",
    "store-graph-select",
    "store-graph-ask",
    "store-graph-construct",
    "store-graph-describe",
    "store-update",
    "store-graph-update",
    "sparql-select",
    "sparql-ask",
    "sparql-construct",
    "sparql-describe",
];

/// The store's HOST RESERVE grant, `urn:cap:store:reserve`, which every scoped job this server
/// runs on its own behalf holds: the scheduled backup ([`crate::backup::JOB_SCOPES`]), the judge
/// backfill ([`crate::backfill::scopes`]), the armed reviewer
/// ([`crate::trigger::reviewer_capability`]), and a layer binding's mirror and taker
/// ([`crate::mirror::capability`], [`crate::taker::capability`]). Root jobs hold it already.
///
/// ★ Why (ledger [#1598](http://localhost:1062/l/default/item/1598)): from the `ikigai-store`
/// release after 0.2.23 (ikigai-rs/ikigai-store PR 63, ledger #1002) the store's cap on SPARQL
/// evaluations in flight is split into a reserve that only root and holders of this grant may
/// use, a pool every other caller shares, and a per-caller share of that pool, keyed by
/// principal. Every capability that names NO principal is one caller, so without this grant
/// the backup would count against the same share as every anonymous HTTP request: on plasma's
/// default cap of 4 (reserve 1, pool 3, share 1), one anonymous runaway query would refuse it.
///
/// It is a PRIORITY, not an authority. No endpoint requires it and it reaches no resource; it
/// only lets an evaluation the caller may already run use the host's slots, and on store 0.2.23
/// (the published release gonk pins) it is inert. ⚠ Spelled as a literal because 0.2.23 has no
/// constant for it: when gonk takes the store release that adds `ikigai_store::budget::
/// CAP_RESERVE`, use that instead and pin the two equal in a test.
///
/// ★ **Never a grant** (ledger [#1600](http://localhost:1062/l/default/item/1600)). Gonk adds
/// it to its own jobs' capabilities; `quic::grant_refusal` refuses it in `grants.json`, in
/// `client add` and in a passkey invite ([`store_reserve_scopes`]), because on a certificate or
/// a passkey it would put a caller in the store's host class.
///
/// ```
/// assert_eq!(ikigai_gonk::budget::STORE_RESERVE, "urn:cap:store:reserve");
/// ```
pub const STORE_RESERVE: &str = "urn:cap:store:reserve";

/// The scopes in `scopes` that are the store's host reserve ([`STORE_RESERVE`]) or a token in
/// its family (`urn:cap:store:reserve:…`, so a later host token spelled under it is refused
/// too). Asked by `quic::grant_refusal`, and through it by every path that turns a grant into
/// scopes.
///
/// ⚠ No wildcard is listed because none reaches the reserve: the store asks
/// `Capability::allows(CAP_RESERVE)`, an exact match, so a held `urn:cap:store:*` does not put
/// its holder in the host class (`tests/store_reserve_1600.rs` pins that).
///
/// ```
/// use ikigai_gonk::budget::store_reserve_scopes;
/// let scopes = [
///     "urn:cap:ledger:read:default".to_string(),
///     "urn:cap:store:reserve".to_string(),
///     "urn:cap:store:reserve:backup".to_string(),
///     "urn:cap:store:reserved".to_string(),
/// ];
/// assert_eq!(
///     store_reserve_scopes(&scopes),
///     ["urn:cap:store:reserve", "urn:cap:store:reserve:backup"]
/// );
/// ```
pub fn store_reserve_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| {
            scope.as_str() == STORE_RESERVE
                || scope
                    .strip_prefix(STORE_RESERVE)
                    .is_some_and(|rest| rest.starts_with(':'))
        })
        .cloned()
        .collect()
}

/// The marker an anonymous HTTP caller's capability carries.
///
/// ```
/// assert_eq!(
///     ikigai_gonk::budget::anonymous_marker(1000),
///     "urn:iki:gonk:door:anonymous-budget:1000"
/// );
/// ```
pub fn anonymous_marker(millis: u64) -> String {
    format!("{ANONYMOUS_BUDGET_PREFIX}{millis}")
}

/// The anonymous budget `capability` carries, if any: the SMALLEST marker it holds (a door only
/// ever computes one, so this is that one). Root carries none.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_gonk::budget::{anonymous_marker, door_budget};
/// let anonymous = Capability::scoped([anonymous_marker(1000)]);
/// assert_eq!(door_budget(&anonymous), Some(1000));
/// assert_eq!(door_budget(&Capability::scoped(["urn:cap:ledger:read:default"])), None);
/// assert_eq!(door_budget(&Capability::root()), None);
/// ```
pub fn door_budget(capability: &Capability) -> Option<u64> {
    capability
        .scopes()?
        .iter()
        .filter_map(|scope| scope.strip_prefix(ANONYMOUS_BUDGET_PREFIX)?.parse().ok())
        .min()
}

/// `request` as the hub should run it: stamped with the anonymous budget when `capability`
/// carries one ([`door_budget`]) and the endpoint it reaches (`endpoint_id`) evaluates SPARQL
/// ([`SPARQL_IDS`]); otherwise unchanged.
pub fn stamp(request: Request, endpoint_id: &str, capability: &Capability) -> Request {
    match door_budget(capability) {
        Some(door) if SPARQL_IDS.contains(&endpoint_id) => stamp_budget(request, door),
        _ => request,
    }
}

/// `request` with `budget=` OVERWRITTEN to `door_ms`, unless it already carries an inline
/// whole number of milliseconds from 1 to `door_ms` — the one thing a caller may do with a
/// budget is lower it.
///
/// ```
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::budget::stamp_budget;
/// let select = || Request::new(Verb::Source, Iri::parse("urn:sparql:select").unwrap());
/// let budget = |r: &Request| match r.args.get("budget") {
///     Some(ArgRef::Inline(b)) => String::from_utf8(b.clone()).unwrap(),
///     other => panic!("{other:?}"),
/// };
/// let inline = |v: &str| ArgRef::Inline(v.as_bytes().to_vec());
/// assert_eq!(budget(&stamp_budget(select(), 1000)), "1000");
/// assert_eq!(budget(&stamp_budget(select().with_arg("budget", inline("200")), 1000)), "200");
/// assert_eq!(budget(&stamp_budget(select().with_arg("budget", inline("600000")), 1000)), "1000");
/// assert_eq!(budget(&stamp_budget(select().with_arg("budget", inline("0")), 1000)), "1000");
/// assert_eq!(budget(&stamp_budget(select().with_arg("budget", inline("1s")), 1000)), "1000");
/// let by_reference = ArgRef::Reference(Iri::parse("urn:example:a-large-budget").unwrap());
/// assert_eq!(budget(&stamp_budget(select().with_arg("budget", by_reference), 1000)), "1000");
/// ```
pub fn stamp_budget(request: Request, door_ms: u64) -> Request {
    let door_ms = door_ms.max(1);
    if is_within(&request, door_ms) {
        request
    } else {
        request.with_arg("budget", ArgRef::Inline(door_ms.to_string().into_bytes()))
    }
}

/// `gonk.http.anonymous_sparql_budget_ms`, checked: a whole number of milliseconds from 1 to
/// [`MAX_ANONYMOUS_SPARQL_BUDGET_MS`]; absent means [`DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS`].
///
/// ```
/// use ikigai_gonk::budget::anonymous_budget;
/// assert_eq!(anonymous_budget(None), Ok(1000));
/// assert_eq!(anonymous_budget(Some("300")), Ok(300));
/// assert!(anonymous_budget(Some("0")).is_err());
/// assert!(anonymous_budget(Some("5001")).is_err());
/// assert!(anonymous_budget(Some("1s")).is_err());
/// ```
pub fn anonymous_budget(value: Option<&str>) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS);
    };
    match value.trim().parse::<u64>() {
        Ok(ms) if (1..=MAX_ANONYMOUS_SPARQL_BUDGET_MS).contains(&ms) => Ok(ms),
        _ => Err(format!(
            "gonk.http.anonymous_sparql_budget_ms = `{value}` is not a whole number of \
             milliseconds from 1 to {MAX_ANONYMOUS_SPARQL_BUDGET_MS} (the store's base: a larger \
             budget could never apply, because a request's budget only lowers what its \
             capability gets)"
        )),
    }
}

// ------------------------------------------------------------------ an anonymous script run

tokio::task_local! {
    /// The anonymous budget of the script run this task is inside, if any: set by
    /// [`within_run`] and read by [`run_budget`].
    static RUN_BUDGET: u64;
}

/// `work` with the anonymous budget `door_ms` in force for every SPARQL evaluation it issues
/// ([`run_budget`]) — how an anonymous SCRIPT RUN keeps the door's budget (ledger
/// [#1175](http://localhost:1060/l/default/item/1175)).
///
/// # Why a task-local, and not the capability
///
/// The marker ([`anonymous_marker`]) is in the capability the door computed, and the stamp at
/// the hub's front door ([`stamp`]) reads it there. A script run never reaches the store that
/// way: `ikigai-script` evaluates the script's query in a SUB-request it issues under the run's
/// capability, which is the runner's ATTENUATED to what the script derives — so the marker is
/// dropped, and the sub-request passes no door. It ran under the store's base (5 s) whatever
/// the door said. The run's endpoint is wrapped by gonk ([`crate::script`]), which still sees the
/// runner's own capability, marker and all, and runs the endpoint inside this scope; the
/// store's overlay in the hub ([`crate::sparql::bounded`]) stamps every SPARQL evaluation made
/// within it. The kernel issues a sub-request on the task that issued it (the hub is not a
/// scheduled kernel, so nothing here fans out onto other tasks), which is what carries it.
///
/// ⚠ **Within a run, EVERY evaluation is held to it**, a plan script's ledger reads included:
/// the anonymous caller's run is the work being bounded. Outside a run the stamp is still only
/// [`stamp`]'s, so gonk's own queries under an anonymous PAGE keep the store's base.
///
/// ★ **Three kinds of run, one scope** (ledger [#1210](http://localhost:1060/l/default/item/1210)):
/// a script's `…:result` / `…:runs`, an ad hoc plan at `urn:plan:eval`, and the dry run of
/// `urn:nl:sparql:check`. The last two keep the caller's capability, marker and all, but their
/// SPARQL is a SUB-request the hub's front door never sees, so [`stamp`] never applied to it:
/// both ran a caller's own query at the store's base. Each enters here through
/// [`AnonymousRuns::run`], which also counts it against the anonymous rate.
pub async fn within_run<F: std::future::Future>(door_ms: u64, work: F) -> F::Output {
    RUN_BUDGET.scope(door_ms, work).await
}

/// The anonymous budget of the script run this task is inside ([`within_run`]), or `None`.
pub fn run_budget() -> Option<u64> {
    RUN_BUDGET.try_with(|ms| *ms).ok()
}

/// Whether `request` already carries an inline `budget=` from 1 to `door_ms` — what
/// [`stamp_budget`] keeps rather than overwrites.
///
/// ```
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::budget::{is_within, stamp_budget};
/// let select = Request::new(Verb::Source, Iri::parse("urn:iki:store:graph-select").unwrap());
/// assert!(!is_within(&select, 1000));
/// assert!(is_within(&stamp_budget(select.clone(), 1000), 1000));
/// let loose = select.with_arg("budget", ArgRef::Inline(b"5000".to_vec()));
/// assert!(!is_within(&loose, 1000));
/// ```
pub fn is_within(request: &Request, door_ms: u64) -> bool {
    let door_ms = door_ms.max(1);
    match request.args.get("budget") {
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .is_some_and(|ms| (1..=door_ms).contains(&ms)),
        _ => false,
    }
}

// ------------------------------------------------------------------ the anonymous run rate

/// How many runs a minute ANONYMOUS callers get, all of them together, unless
/// `gonk.script.anonymous_runs_per_minute` says otherwise: **30** (ledger
/// [#1175](http://localhost:1060/l/default/item/1175), configurable since
/// [#1210](http://localhost:1060/l/default/item/1210)). [`crate::script::ANONYMOUS_RUNS`] says why.
pub const DEFAULT_ANONYMOUS_RUNS_PER_MINUTE: u32 = 30;

/// The name the anonymous rate is counted under, and the one its refusal names:
/// `rate-limited: `urn:iki:gonk:anonymous:runs` is capped at 30/60s — retry after …`. It names
/// no resource anyone can request; it is the window's key.
pub const ANONYMOUS_RUNS_IRI: &str = "urn:iki:gonk:anonymous:runs";

/// `gonk.script.anonymous_runs_per_minute`, checked: a whole number of runs a minute, `0` for
/// none at all; absent means [`DEFAULT_ANONYMOUS_RUNS_PER_MINUTE`].
///
/// ```
/// use ikigai_gonk::budget::anonymous_runs_per_minute;
/// assert_eq!(anonymous_runs_per_minute(None), Ok(30));
/// assert_eq!(anonymous_runs_per_minute(Some("120")), Ok(120));
/// assert_eq!(anonymous_runs_per_minute(Some("0")), Ok(0));
/// assert!(anonymous_runs_per_minute(Some("-1")).is_err());
/// assert!(anonymous_runs_per_minute(Some("30/min")).is_err());
/// ```
pub fn anonymous_runs_per_minute(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_ANONYMOUS_RUNS_PER_MINUTE);
    };
    value.trim().parse::<u32>().map_err(|_| {
        format!(
            "gonk.script.anonymous_runs_per_minute = `{value}` is not a whole number of runs a \
             minute (0 for none: anonymous callers then run nothing)"
        )
    })
}

/// The anonymous runs' ONE window, shared by every kind of run ([`within_run`]): a script's
/// run, an ad hoc plan and a SPARQL draft's dry run all count against the same rate, because
/// the door cannot tell anonymous callers apart and they all cost the same thing, a caller's
/// own query evaluated.
///
/// A [`Window`] keyed [`ANONYMOUS_RUNS_IRI`]. Built once in `main` and handed to both the script
/// family and the natural-language family, so they share it; a clone shares the window.
#[derive(Clone)]
pub struct AnonymousRuns {
    window: Window,
}

impl AnonymousRuns {
    /// A window of `max` runs a minute.
    pub fn per_minute(max: u32) -> AnonymousRuns {
        AnonymousRuns::new(Rate::new(max, Duration::from_secs(60)))
    }

    /// A window of `rate`.
    pub fn new(rate: Rate) -> AnonymousRuns {
        AnonymousRuns {
            window: Window::new(ANONYMOUS_RUNS_IRI, rate),
        }
    }

    /// The rate this window holds.
    pub fn rate(&self) -> Rate {
        self.window.rate()
    }

    /// `work`, an anonymous caller's run, counted against the window and run within the door's
    /// budget `door_ms` ([`within_run`]) — or the limiter's refusal, naming the rate and when to
    /// retry, without running anything.
    pub async fn run<F>(
        &self,
        inv: &Invocation<'_>,
        door_ms: u64,
        work: F,
    ) -> ikigai_core::Result<Representation>
    where
        F: std::future::Future<Output = ikigai_core::Result<Representation>>,
    {
        if let Some(refused) = self.window.admit(inv).await {
            return refused;
        }
        within_run(door_ms, work).await
    }
}

/// ONE sliding window over many callers and many names: an `ikigai-throttle` `RateLimit` over
/// a space holding one name, `key`. The limiter keys its window by the PREFIX it matched, so
/// two families limited by their own prefixes would be two windows; counting every admission
/// under one fixed name is what makes it one. A clone shares the window.
///
/// ★ **Consulted on INVOKE, never at resolution** ([`Window::admit`]). The kernel checks a
/// caller's capability against the resolved endpoint's declaration after resolution and
/// before the invoke, so a window consulted in the invoke counts only callers the kernel
/// let through. Counted at resolution — `RateLimit` composed as a space — a caller refused
/// for want of a grant would still spend a slot, and anyone who could reach the door could
/// empty the window for everyone who may use it.
#[derive(Clone)]
pub struct Window {
    limiter: Arc<RateLimit<Gate>>,
    pass: Arc<dyn Endpoint>,
    key: &'static str,
    rate: Rate,
}

impl Window {
    /// A window of `rate`, counted and refused under the name `key` (an IRI that names no
    /// resource anyone can request: it is the window's key, and the refusal names it).
    ///
    /// # Panics
    ///
    /// When `key` is not an IRI — a constant of this crate, so a programming error.
    pub fn new(key: &'static str, rate: Rate) -> Window {
        Iri::parse(key).expect("a window's key is an IRI");
        let pass: Arc<dyn Endpoint> = Arc::new(Pass);
        Window {
            limiter: Arc::new(
                RateLimit::new(Gate {
                    pass: Arc::clone(&pass),
                })
                .limit(key, rate),
            ),
            pass,
            key,
            rate,
        }
    }

    /// The rate this window holds.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Count one admission. `None` when it is under the rate (and now counted); otherwise the
    /// limiter's refusal — `rate-limited: `{key}` is capped at {max}/{window}s — retry after
    /// {n}s` — which the caller returns instead of doing the work.
    pub async fn admit(&self, inv: &Invocation<'_>) -> Option<ikigai_core::Result<Representation>> {
        let count = Request::new(
            Verb::Source,
            Iri::parse(self.key).expect("checked in Window::new"),
        );
        if let Resolution::Hit(hit) = self.limiter.resolve(&count, &Scope::empty()) {
            // Over the rate the limiter answers its stand-in, which refuses whatever it is
            // handed; under it, the gate's own endpoint, which is never invoked.
            if !Arc::ptr_eq(&hit.endpoint, &self.pass) {
                return Some(hit.endpoint.invoke(inv).await);
            }
        }
        None
    }
}

impl Default for AnonymousRuns {
    fn default() -> AnonymousRuns {
        AnonymousRuns::per_minute(DEFAULT_ANONYMOUS_RUNS_PER_MINUTE)
    }
}

/// The space a [`Window`]'s limiter counts in: every request resolves to [`Pass`].
struct Gate {
    pass: Arc<dyn Endpoint>,
}

impl Space for Gate {
    fn resolve(&self, _request: &Request, _scope: &Scope) -> Resolution {
        Resolution::Hit(Resolved::new(Arc::clone(&self.pass), Bindings::default()))
    }
}

/// The gate's answer under the rate. Only ever compared, never invoked.
struct Pass;

#[async_trait]
impl Endpoint for Pass {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Err(Error::Endpoint(
            "a rate window's gate is a counter, not a resource".to_string(),
        ))
    }
}

// ------------------------------------------------------------------ the model-call rate

/// How many calls an hour reach the mounted model through this server, every caller and every
/// family together, unless `gonk.mount.calls_per_hour` says otherwise: **2,000** (ledger
/// [#308](http://localhost:1062/l/default/item/308)).
///
/// ★ **Why a rate at all.** Before this, nothing counted spend over time: a call's
/// `max_tokens` bounds ONE call, the archive makes a re-read of the same content free, and a
/// net grant decides who may spend — so a caller holding one could walk new content versions,
/// or ask `urn:llm:ask` directly (neither archived nor bounded by the explain ceilings), with
/// no aggregate limit anywhere.
///
/// ★ **Why counted at the mount, not at each derivation.** Every derivation — a file or
/// directory explanation, a review pass, a pull request's layers, the judge, a natural-language
/// draft — reaches the model through the one `urn:llm:` mount, and so does a direct ask. A rate
/// on the derivation endpoints would count archive hits (a re-read costs nothing) and miss the
/// direct ask; a rate on the mount counts exactly the calls that spend. ⚠ Only the forwards
/// that reach a model, though: the mount also carries reads that call none (`urn:llm:config`,
/// a provider's `:model`, which browse reads on every explain), and those spend nothing
/// ([`crate::mount::reaches_a_model`], ledger [#1576](http://localhost:1062/l/default/item/1576)).
///
/// ★ **Why 2,000.** The busiest hour on record is 728 derivation activities (`prov:Activity`
/// in browse's graph, 2026-10-02 19:00 UTC, during the review-value experiments), and the
/// busiest minute of the three busiest hours held 16. An activity can be more than one call (a review pass over a long file is
/// several), so the ceiling is about 2.7 times that hour: it does not bind the heaviest work
/// this server has done, and it does bind a runaway, which can issue a short call every
/// second or two. ⚠ It is a ceiling, not a meter: an operator who pays per token sets their
/// own (`0` refuses every call).
pub const DEFAULT_MODEL_CALLS_PER_HOUR: u32 = 2_000;

/// The name the model-call rate is counted under, and the one its refusal names:
/// `rate-limited: `urn:iki:gonk:mount:calls` is capped at 2000/3600s — retry after …`.
pub const MODEL_CALLS_IRI: &str = "urn:iki:gonk:mount:calls";

/// `gonk.mount.calls_per_hour`, checked: a whole number of calls an hour, `0` for none;
/// absent means [`DEFAULT_MODEL_CALLS_PER_HOUR`].
///
/// ```
/// use ikigai_gonk::budget::model_calls_per_hour;
/// assert_eq!(model_calls_per_hour(None), Ok(2000));
/// assert_eq!(model_calls_per_hour(Some("500")), Ok(500));
/// assert_eq!(model_calls_per_hour(Some("0")), Ok(0));
/// assert!(model_calls_per_hour(Some("-1")).is_err());
/// assert!(model_calls_per_hour(Some("2000/h")).is_err());
/// ```
pub fn model_calls_per_hour(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_MODEL_CALLS_PER_HOUR);
    };
    value.trim().parse::<u32>().map_err(|_| {
        format!(
            "gonk.mount.calls_per_hour = `{value}` is not a whole number of calls an hour (0 \
             for none: nothing then reaches the mounted model)"
        )
    })
}

/// The model-call window: [`Window`] keyed [`MODEL_CALLS_IRI`], `max` calls an hour.
pub fn model_calls(max: u32) -> Window {
    Window::new(MODEL_CALLS_IRI, Rate::new(max, Duration::from_secs(3600)))
}
