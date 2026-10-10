//! Thread stacks large enough that a request cannot abort the process by recursion
//! (ledger [#915](http://localhost:1060/l/default/item/915)).
//!
//! # Why
//!
//! A stack overflow in Rust is not a panic: it ABORTS the whole process, every door and every
//! request with it. The SPARQL parser and the store's optimizer and evaluator all recurse on
//! the shape of a query, and so do the XML parser and transform under [`crate::render`]. The
//! front line is [`crate::sparql::admit`] (and `render`'s own depth bound); this is the line
//! behind it, for a recursion the bounds do not know about. A larger stack does not REMOVE such
//! a recursion — it makes the input that reaches it proportionally longer: everything measured
//! by `cargo run --release --example sparql-depth` scales linearly with the stack (885
//! parentheses at 2 MiB, 7,084 at 16 MiB).
//!
//! # Which threads
//!
//! Every thread that serves a request, and none of them is spawned by gonk alone:
//!
//! - the HTTP door's tokio workers and blocking pool — gonk's own runtime, sized explicitly in
//!   `main` with [`THREAD_STACK_BYTES`];
//! - the socket door's thread PER CONNECTION, spawned inside `ikigai_ipc::serve` with
//!   `std::thread::Builder::new()` and no size;
//! - the QUIC door's runtime, built inside `ikigai_quic::serve` with `Runtime::new()`.
//!
//! The last two take no stack size from their caller, so the only lever this binary has over
//! them is the standard library's default for a thread spawned without one, which it reads ONCE,
//! from `RUST_MIN_STACK`, at the first such spawn and caches for the life of the process.
//! [`enlarge_default`] sets that variable, spawns one thread so the default is read and cached,
//! and then puts the environment back as it found it — so nothing this process spawns (git,
//! `gh`) inherits the setting, and no operator configures it: it is a fact of this binary, not a
//! setting. ⚠ It must run before ANY other thread exists — `std::env::set_var` beside a running
//! thread is a data race — which is why it is the first line of `main`.
//!
//! ⚠ **The real fix is one layer down**: `ikigai-ipc` and `ikigai-quic` should take a stack size
//! (or a thread builder) from their caller. Reported to the hub rather than patched around twice.
//!
//! # What it costs
//!
//! A thread's stack is RESERVED address space, not memory: pages are committed only when a
//! recursion first touches them. An idle 64 MiB thread costs what an idle 2 MiB one does. The
//! cost is after a deep request: the pages it touched stay with that thread until the thread
//! exits — so the worst case, with every serving thread driven to its limit, is
//! `THREAD_STACK_BYTES` × the threads serving at once (one tokio worker per core, the blocking
//! pool's threads in use, and one thread per open socket connection). The query length bound
//! ([`crate::sparql::MAX_QUERY_BYTES`]) keeps the deepest query the measurements know about
//! well under the limit, so in practice a request touches a small fraction of it.

/// The stack every request-serving thread gets: 64 MiB, 32 times the 2 MiB default.
///
/// Chosen against the measurement, not rounded up from a guess. Every serving thread gets it,
/// rather than a scoped thread spawned per parse, because the recursion that aborts is not only
/// gonk's own parse (`web::query_form`) but the store's parse, its graph-confinement walk and its
/// evaluation, which run inside `ikigai-store` on whatever thread issued the request: the densest chain measured
/// (`?s a/a/a/… ?o`, two bytes an element) costs the store about 1.5 KB of stack an element in a
/// release build, so a query at [`crate::sparql::MAX_QUERY_BYTES`] needs about 24 MiB. 64 MiB
/// leaves more than twice that.
///
/// ⚠ Since `ikigai-store` 0.2.7 the store parses, plans and evaluates on a thread of ITS OWN,
/// sized by the store (16 MiB plus 512 bytes per byte of query), so this stack no longer
/// carries the store's recursion — it carries gonk's own parse, the render, and everything a
/// request does on its way to the store. Measured 2026-10-09 with store 0.2.8
/// (`examples/sparql-depth.rs`, `store`): a release build holds every chain shape up to the
/// store's algebra bound; a DEBUG build aborts past 405 terms of `1*1*…` or `1+1+…` on the
/// store's thread whatever this constant says (ledger #979, reported to the store).
pub const THREAD_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Make [`THREAD_STACK_BYTES`] the default stack of every thread this process spawns without
/// naming a size — see the module doc. Call it first in `main`, before any thread exists.
///
/// An operator's own `RUST_MIN_STACK` that asks for MORE is kept; one that asks for less is
/// overridden, because a smaller stack is exactly the abort this exists to prevent.
pub fn enlarge_default() {
    const VAR: &str = "RUST_MIN_STACK";
    let before = std::env::var_os(VAR);
    let asked = before
        .as_ref()
        .and_then(|v| v.to_str())
        .and_then(|v| v.parse::<usize>().ok());
    if asked.is_some_and(|bytes| bytes >= THREAD_STACK_BYTES) {
        return;
    }
    std::env::set_var(VAR, THREAD_STACK_BYTES.to_string());
    // The first spawn without a size reads and caches the default; nothing reads it again.
    let _ = std::thread::spawn(|| ()).join();
    match before {
        Some(value) => std::env::set_var(VAR, value),
        None => std::env::remove_var(VAR),
    }
}
