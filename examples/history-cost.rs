//! `cargo run --release --example history-cost -- <name>=<path> [--weeks N]` — what a browse
//! root's weekly git-history series costs to read (ledger #1631): cold, warm, and after the
//! root watch cuts the root's threads.
//!
//! ```sh
//! cargo run --release --example history-cost -- ikigai-cli=$HOME/git-personal/ikigai-cli
//! ```
//!
//! The composition is the one `main` builds for one WATCHED browse root, over an in-memory
//! store, so nothing opens `~/.ikigai/store` and the repository is only ever read. Three reads
//! of `urn:iki:gonk:history:{name}:weekly` per face (`text/csv`, `text/plain`):
//!
//! - **cold**: `git status` for the HEAD, then the two `git log` passes;
//! - **warm**: a cache hit;
//! - **after a cut**: both root threads cut by hand, as a working-tree edit or a commit cuts
//!   them — the HEAD probe recomputes, and the series as at an unchanged HEAD is a hit.
//!
//! The plain face is a view of the CSV one (one count per commit and window), so its "cold"
//! is a fresh `git status` and a hit on the series. Last, the series as at the HEAD over the
//! maximum window, cold: the bound's worst case for this repository.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::watch::{root_thread, wide_thread, RootWatch};
use ikigai_gonk::{browse, compose_with, history};
use ikigai_store::DurableStore;

fn read(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(Kernel::issue(kernel, request, &Capability::root()))
        .unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"));
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .expect("usage: history-cost <name>=<path> [--weeks N]");
    let (name, path) = root.split_once('=').expect("a root is <name>=<path>");
    let mut weeks = history::DEFAULT_WEEKS.to_string();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--weeks" => weeks = args.next().expect("--weeks N"),
            other => panic!("unknown flag {other}"),
        }
    }
    let roots = vec![(name.to_string(), PathBuf::from(path))];
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("an in-memory store");
    let (watch, refused) = RootWatch::start(&roots);
    assert!(
        refused.is_empty(),
        "the root could not be watched: {refused:?}"
    );
    let wired = browse::wire(
        roots,
        store.shared_writer().expect("the store's writer"),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    );
    let iri = history::weekly_iri(name);
    for face in ["text/csv", "text/plain"] {
        let args = [("as", face), ("weeks", weeks.as_str())];
        let started = Instant::now();
        let cold_answer = read(&hub, &iri, &args);
        let cold = started.elapsed();
        let started = Instant::now();
        let warm_answer = read(&hub, &iri, &args);
        let warm = started.elapsed();
        assert_eq!(cold_answer, warm_answer);
        hub.cut(root_thread(name).as_str());
        hub.cut(wide_thread(name).as_str());
        let started = Instant::now();
        read(&hub, &iri, &args);
        let recut = started.elapsed();
        println!("{face:<10}  cold {cold:>12.2?}  warm {warm:>12.2?}  after a cut {recut:>12.2?}");
        if face == "text/csv" {
            let commits: u64 = cold_answer
                .lines()
                .skip(1)
                .filter_map(|row| row.split(',').nth(1)?.parse::<u64>().ok())
                .sum();
            println!("            {commits} commits in {weeks} weeks");
        }
    }
    let state = read(&hub, &format!("urn:repo:{name}:state"), &[]);
    let head = state.split_whitespace().next().expect("a HEAD");
    let window = history::MAX_WEEKS.to_string();
    let started = Instant::now();
    read(
        &hub,
        &history::weekly_at_iri(name, head),
        &[("weeks", window.as_str()), ("as", "text/csv")],
    );
    println!(
        "as at {head}, weeks={window}: cold {:.2?}",
        started.elapsed()
    );
    drop(watch);
}
