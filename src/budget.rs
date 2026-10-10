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

use ikigai_core::{ArgRef, Capability, Request};

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
    let tighter = match request.args.get("budget") {
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .is_some_and(|ms| (1..=door_ms).contains(&ms)),
        _ => false,
    };
    if tighter {
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
