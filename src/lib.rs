//! **ikigai-gonk** — a standalone work-ledger server, as a library the binary and its tests
//! share.
//!
//! [`compose`] builds the one kernel this process serves: `ikigai-store`'s durable dataset
//! and `ikigai-ledger`'s named ledgers beside it, behind a Meta renderer and a clock. The
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

pub mod config;
pub mod doors;
pub mod grants;
pub mod quic;

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
/// - **The store is taken by value and never shared.** `DurableStore::open` (not
///   `open_shared`) keeps the handle inside `ikigai-store`, which is what makes its reads
///   cacheable under its write threads.
/// - **A clock.** The ledger stamps every item, comment and tombstone, and refuses to write
///   on a kernel that cannot say when.
/// - **The Meta renderer.** A client mounting gonk reads each endpoint's contract through
///   `Verb::Meta as=application/json`; without a renderer that answer degrades to an
///   anonymous row and named arguments stop routing, with no error anywhere.
pub fn compose(store: DurableStore) -> Kernel {
    let space = Fallback::new(vec![
        Arc::new(ikigai_store::space(store)) as Arc<dyn Space>,
        Arc::new(ikigai_ledger::space()) as Arc<dyn Space>,
    ]);
    Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
}
