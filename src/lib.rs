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

pub mod browse;
pub mod config;
pub mod doors;
pub mod grants;
pub mod identity;
pub mod mount;
pub mod quic;
pub mod render;
pub mod rules;
pub mod watch;
pub mod web;

use std::sync::Arc;

use ikigai_core::{Fallback, Kernel, Space, SystemClock};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;

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
    compose_with(store, None, Vec::new())
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
/// ★ **The store space is never wrapped, whichever way the store was opened.** A shared
/// store used to make every `ikigai-store` read `Expiry::Always` — which propagates into
/// every ledger read — and this server recovered the scoped reads from outside, in a
/// `freshness` module that re-declared four IRIs it had transcribed by hand. `ikigai-store`
/// 0.2.4 takes the promise directly (`open_shared_declaring` + `SharerWrites`, on the line
/// in `main` where the handle is handed out) and answers freshness per read, so the wrapper
/// is gone and there is ONE place that says what is fresh. Two would be the hazard, not the
/// belt and braces: the day `ikigai-browse` gains a named graph of its own, a wrapper here
/// would go on declaring a graph the sharer writes as cacheable, silently.
pub fn compose_with(
    store: DurableStore,
    browse: Option<Arc<dyn Space>>,
    mounted: Vec<Arc<dyn Space>>,
) -> Kernel {
    let store: Arc<dyn Space> = Arc::new(ikigai_store::space(store));
    // The mounts go FIRST — an override forwards its prefix unchanged, and precedence is
    // half of what makes it an override. Nothing local is shadowed by that today (this
    // server binds nothing under `urn:llm:`), and `crate::mount` is where the prefix a
    // mount may claim is decided.
    let mut spaces: Vec<Arc<dyn Space>> = mounted;
    spaces.push(store);
    spaces.push(Arc::new(ikigai_ledger::space()));
    if let Some(browse) = browse {
        spaces.push(browse);
        spaces.push(Arc::new(ikigai_repo::space()));
    }
    Kernel::with_meta_renderer(Arc::new(Fallback::new(spaces)), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
}
