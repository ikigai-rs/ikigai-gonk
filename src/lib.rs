//! **ikigai-gonk** — a standalone work-ledger server, as a library the binary and its tests
//! share.
//!
//! [`compose`] builds the one kernel this process serves: `ikigai-store`'s durable dataset,
//! `ikigai-ledger`'s named ledgers beside it and — when roots are configured —
//! `ikigai-browse`'s repository family over the SAME dataset, with that family's explanation
//! and review layers when a peer serving `urn:llm:*` is mounted in front of them
//! ([`mount`]), behind a Meta renderer and a clock. The
//! binary opens the store, calls [`compose`], and puts the result behind three doors
//! ([`doors`]); `tests/conformance.rs` walks the same function rather than a re-creation of
//! it, because the composition is the only thing in this crate that is its own to get wrong.
//!
//! # Decision of record: gonk HOLDS the store
//!
//! RocksDB permits one writer per directory, and `ikigai-store` refuses a second open even
//! inside one process. So one process on a machine owns `~/.ikigai/store`, and on a machine
//! that runs gonk that process is gonk: every other ikigai process reaches the data by
//! mounting gonk's socket —
//!
//! ```toml
//! mount = "prefer urn:iki:store:=/Users/you/.ikigai/gonk.sock"
//! mount = "prefer urn:iki:ledger:=/Users/you/.ikigai/gonk.sock"
//! ```
//!
//! — never by opening the dataset. A mount claims one prefix, so the ledger takes its own
//! line.
//!
//! # ⚠ This library is internal
//!
//! The crate publishes a binary. What it versions is the set of resource names and the
//! doors behind the socket and ports; every Rust item here exists for `main` and the tests,
//! and changes with the composition without a deprecation.

#![forbid(unsafe_code)]

pub mod access;
pub mod admit;
pub mod backfill;
pub mod backup;
pub mod batch;
pub mod browse;
pub mod checkout;
pub mod config;
pub mod doors;
pub mod grants;
pub mod identity;
pub mod k;
pub mod kata;
pub mod keys;
pub mod mount;
pub mod queue;
pub mod quic;
pub mod render;
pub mod roborev;
pub mod rules;
pub mod sparql;
pub mod stack;
pub mod trigger;
pub mod verdict;
pub mod walk;
pub mod watch;
pub mod web;

use std::sync::Arc;

use ikigai_core::{Fallback, Kernel, Space, SystemClock};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;

/// The identities gonk's own spaces claim — what `urn:kernel:topology` names their nodes by
/// and what a hit through one reports as `Resolved::answered_by` (core 0.1.78, ledger
/// [#546](http://localhost:1060/l/default/item/546)).
///
/// ★ **A name is a claim: same name ⇒ same doors.** Core partitions its cache on the
/// answering space's identity, so two spaces named alike must hold the same doors, and one
/// space named consistently shares one entry however often the process rebuilds it. Every
/// name here is claimed by exactly one composition site, and each holds the doors its
/// module binds and no others — which is why the IRIs live in one place rather than beside
/// each `EndpointSpace::new()`: a second site reusing one by hand would be making the claim
/// for a different set of doors.
///
/// The IRIs follow the process's own convention (`urn:iki:gonk:…`, as `urn:iki:gonk:render`
/// and `urn:iki:gonk:backup` do) under a `space:` segment, so a node in the topology cannot
/// be mistaken for a resource: `urn:iki:gonk:space:hub` is the arrangement,
/// `urn:iki:gonk:render` is a door in it.
pub mod spaces {
    use ikigai_core::Iri;

    /// The hub — the [`Fallback`](ikigai_core::Fallback) [`compose_with`](super::compose_with)
    /// builds, and therefore what every door forwards to. The socket and QUIC doors' kernels
    /// have this as their root (through [`doors::HubSpace`](crate::doors::HubSpace), which
    /// forwards the name rather than claiming one of its own); the HTTP door has it as the
    /// second layer of [`HTTP_DOOR`].
    pub const HUB: &str = "urn:iki:gonk:space:hub";
    /// The HTTP door's root: `Fallback([PAGES, HUB])`. A name neither binds is the kernel's
    /// own `Unresolved`, which `ikigai-web` answers `404` (see [`doors::http_kernel`](crate::doors::http_kernel)).
    pub const HTTP_DOOR: &str = "urn:iki:gonk:space:door:http";
    /// gonk's HTML face ([`web::space`](crate::web::space)) — bound only in the HTTP door.
    pub const PAGES: &str = "urn:iki:gonk:space:pages";
    /// The page renderer's chunk transform ([`render::space`](crate::render::space)).
    pub const RENDER: &str = "urn:iki:gonk:space:render";
    /// The backup family ([`backup::space`](crate::backup::space)).
    pub const BACKUP: &str = "urn:iki:gonk:space:backup";
    /// The `urn:sparql:*` query face over the store ([`sparql::space`](crate::sparql::space)).
    pub const SPARQL: &str = "urn:iki:gonk:space:sparql";
    /// The repository browse family AS GONK COMPOSES IT — `ikigai-browse`'s doors with
    /// this server's cacheable overlay in front ([`browse::cached_reads`](crate::browse::cached_reads)).
    pub const BROWSE: &str = "urn:iki:gonk:space:browse";

    /// One of the constants above as an [`Iri`].
    pub fn iri(name: &str) -> Iri {
        Iri::parse(name).expect("a constant space IRI")
    }
}

/// The kernel this process serves — the **hub**, and the only kernel with a cache.
///
/// Store first, ledger second, behind a [`Fallback`]: the order `ikigai-ledger`'s own tests
/// and README compose in. The two grammars do not overlap, so the order decides nothing
/// today; agreeing with the crate that is bound is one fewer difference to reason about.
///
/// Three things are not optional here, and each fails silently rather than loudly:
///
/// - **How the store was OPENED, which this function cannot see and does not decide.**
///   `DurableStore::open` keeps the handle inside `ikigai-store` and every read is
///   cacheable under its write threads. When the handle must leave — the browse
///   composition below — `main` opens with `open_shared_declaring` and names the graphs
///   the sharer may write, and the store answers freshness per read from that promise.
///   Either way this function takes the store as it was handed over and wraps nothing:
///   there is exactly one place that decides what is fresh, and it is not here.
/// - **A clock.** The ledger stamps every item, comment and tombstone, and refuses to write
///   on a kernel that cannot say when.
/// - **The Meta renderer.** A client mounting gonk reads each endpoint's contract through
///   `Verb::Meta as=application/json`; without a renderer that answer degrades to an
///   anonymous row and named arguments stop routing, with no error anywhere.
pub fn compose(store: DurableStore) -> Kernel {
    compose_with(store, None, Vec::new(), Vec::new(), None)
}

/// [`compose`], plus the repository browse family when this server is configured for one.
///
/// `browse` is [`crate::browse::wire`]'s space — the family over the SAME dataset, with
/// gonk's cacheable overlay in front. It arrives with `ikigai-repo`'s facades, which are
/// bound **only** alongside it: browse's pull-request rows resolve `urn:repo:pr:*` through
/// the kernel, and without a browse face those facades would be process execution served
/// behind three doors for nothing.
///
/// `mounted` is [`crate::mount::space`] per configured peer — composed IN FRONT of
/// everything local, claiming exactly its prefix. It is what makes `urn:llm:{provider}:ask`
/// resolve, and therefore what the explanation families derive through; a gonk with no mount
/// binds none of them ([`crate::config::Settings::explains`]).
///
/// ★ **The store space is never wrapped FOR FRESHNESS, whichever way the store was opened.**
/// (Two overlays are in front of it, and neither decides freshness: one observes its writes
/// for the header badge, and one refuses a SPARQL text past [`sparql::admit`]'s bound before
/// the store parses it — see the body.) A shared
/// store used to make every `ikigai-store` read `Expiry::Always` — which propagates into
/// every ledger read — and this server recovered the scoped reads from outside, in a
/// `freshness` module that re-declared four IRIs it had transcribed by hand. `ikigai-store`
/// 0.2.4 takes the promise directly (`open_shared_declaring`, on the line in `main` where the
/// handle is handed out) and answers freshness per read, so the wrapper is gone and there is
/// ONE place that says what is fresh. Two would be the hazard, not the belt and braces: the
/// day this server gives browse a named graph of its own ([`crate::browse::Graph`]), a
/// wrapper here would go on declaring a graph the sharer writes as cacheable, silently.
///
/// `backups` is [`crate::backup::space`]'s family — `urn:iki:gonk:backup`, its status, its
/// archives and `urn:iki:gonk:restore` — together with the compression module they reach
/// gzip through. Bound TOGETHER, and only together, because `urn:compress:*` is linked for
/// them: a backup compresses by resolving `urn:compress:gzip` through this kernel rather
/// than by calling a gzip crate, which is the whole reason a module was built instead of a
/// helper. A gonk composed without them serves exactly the catalog it served before.
///
/// ⚠ **The timer's control plane is NOT bound, and that is a security decision rather than
/// an omission.** `ikigai-time` is linked and the backup job runs in a [`JobRegistry`]
/// ([`crate::backup::Backups`]) — but `urn:time:schedule` fires an ARBITRARY target under
/// the registry's own capability, so binding it would put "have this server issue any
/// request as itself" behind every door this binary opens. The target set is fixed at
/// startup by `main`, not chosen at call time by a caller.
///
/// [`JobRegistry`]: ikigai_time::JobRegistry
/// (continued) `trigger` is [`crate::trigger::space`]'s three — the review queue
/// (`urn:space:{name}`), the pass in front of it (`urn:iki:gonk:review:pass`) and the depth
/// behind it (`urn:iki:gonk:review:depth`) — bound only when a `gonk.review.space` line
/// configured one, the same switch shape as a mount.
///
/// ⚠ **Binding the queue is still not arming the trigger**, and composing it never was:
/// arming happens in `main`, after this function, and takes `gonk.review.arm = true` plus a
/// `gonk.review.grant` that `grants.json` can honour ([`crate::trigger::arm`]). What changed
/// on 2026-09-20 is that arming became safe rather than possible: `ikigai-browse` 0.5.0 made
/// a review pass produce PENDING findings and stop declaring `urn:cap:annotate`, so a
/// headless reviewer **cannot publish** — Brian's rule, *"Nothing gets published to Gonk
/// except by the human"*, is arithmetic now. See [`crate::trigger`] and ledger
/// [#466](http://localhost:1060/l/default/item/466).
pub fn compose_with(
    store: DurableStore,
    browse: Option<Arc<crate::browse::CachedReads>>,
    mounted: Vec<Arc<dyn Space>>,
    trigger: Vec<Arc<dyn Space>>,
    backups: Option<crate::backup::Backups>,
) -> Kernel {
    // ★ Bounded before anything parses caller SPARQL (ledger #915): a query nested or chained
    // deep enough overflows the parser's or the evaluator's stack, and that ABORTS the
    // process. In the hub, so it holds for every door: nesting at every depth, length for a
    // caller's own text only, at depth 0 (ledger #965, `crate::sparql::bounded`).
    let store = crate::sparql::bounded(
        Arc::new(ikigai_store::space(store)),
        crate::sparql::STORE_RULES,
    );
    // ★ Observed, not wrapped for freshness: a write through the store that can reach
    // browse's graph touches the header badge's epochs after it runs (ledger #667,
    // [`crate::browse::CachedReads::observe_store_writes`]). Every answer is the store's own.
    let store = match &browse {
        Some(browse) => browse.observe_store_writes(store),
        None => store,
    };
    // The mounts go FIRST — an override forwards its prefix unchanged, and precedence is
    // half of what makes it an override. Nothing local is shadowed by that today (this
    // server binds nothing under `urn:llm:`), and `crate::mount` is where the prefix a
    // mount may claim is decided.
    let mut spaces: Vec<Arc<dyn Space>> = mounted;
    spaces.push(store);
    spaces.push(Arc::new(ikigai_ledger::space()));
    // ★ The page renderer's chunk resource (`urn:iki:gonk:render`, ledger #519) lives HERE
    // and not in the HTTP door's page space, because the hub holds the one cache in the
    // process (`crate::doors`): a chunk is a pure, cacheable function of its document, and
    // a cache the door kernels do not have would make it a transform that recomputes every
    // poll. It is the one `gonk-` id the socket and QUIC doors serve — a render of the
    // caller's own bytes, gated by nothing because it reads nothing.
    spaces.push(Arc::new(render::space()));
    // ★ `urn:sparql:*` (ledger #836): the query face clients already speak, as a mapping onto
    // the store's graph-scoped forms with the caller's readable graphs as the default dataset
    // (`crate::sparql`). Bound with the store, always — it reaches nothing the store's own
    // scoped doors do not, and it is what lets `web.mount` prefer this socket.
    spaces.push(Arc::new(sparql::space()));
    // The root to read the verdict set through, kept before the family is moved in.
    let verdict_root = browse
        .as_ref()
        .and_then(|b| b.first_root().map(str::to_string));
    if let Some(browse) = browse {
        spaces.push(browse as Arc<dyn Space>);
        spaces.push(Arc::new(ikigai_repo::space()));
    }
    spaces.extend(trigger);
    if let Some(backups) = backups {
        spaces.push(Arc::new(backup::space(backups)));
        spaces.push(Arc::new(ikigai_compress::space()));
    }
    // Named, so `urn:kernel:topology` renders the hub as `urn:iki:gonk:space:hub` from every
    // door (the door kernels forward this name) and a hit reports it as `answered_by` when
    // no space inside claims one — today none of the linked crates' spaces do.
    let hub = Fallback::new(spaces).named(self::spaces::iri(self::spaces::HUB));
    let kernel = Kernel::with_meta_renderer(Arc::new(hub), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock));
    // ★ The judge's verdict words, adopted from the browse family this hub composes (ledger
    // #702 item 1): the Queue orders, folds and hides by them, and spells none of them. An
    // error here is not swallowed so much as deferred — `main` adopts again at start and
    // stops on it, naming the browse floor; a hub that adopted nothing orders nothing.
    if let Some(root) = verdict_root {
        let _ = crate::verdict::adopt(&kernel, &root);
    }
    kernel
}
