//! `cargo run [--release] --example sparql-depth -- [stack-KiB] [shape,…] [parse,store]` — how deep a SPARQL query may
//! nest before parsing (or evaluating) it overflows a thread's stack and ABORTS the process
//! (ledger [#915](http://localhost:1060/l/default/item/915)).
//!
//! A stack overflow is not a panic: Rust aborts the whole process, so no `catch_unwind` can
//! measure it. Each probe therefore runs in a CHILD process (this same binary, re-executed with
//! `probe` arguments), and the parent binary-searches the largest size that still exits cleanly.
//!
//! Two paths are probed, each on a thread with the stack size given (default 2048 KiB — tokio's
//! worker and blocking threads, and every `std::thread` spawned without a size, get 2 MiB):
//!
//! - `parse`: `spargebra` alone, the parser `web::query_form` runs in gonk's own code;
//! - `store`: the whole read — `urn:iki:store:graph-select` at root over an in-memory store,
//!   so the store's parse, its optimizer and its evaluator all run. Issued to a kernel over
//!   `ikigai_store::space` alone, not gonk's hub, whose bound would refuse the probe.
//!
//! The shapes are the ones that build a DEEP tree. `paren`, `brace` and `bracket` are nesting a
//! bracket scan sees ([`ikigai_gonk::sparql::nesting_depth`]); `plus`, `and`, `union`,
//! `optional` and `path` build an equally deep tree with NO nesting at all — a left-deep chain
//! the grammar reads in a loop and the later stages walk by recursion. Which of those abort is
//! exactly what a bracket bound cannot cover, and why gonk's worker stacks are larger too.
//!
//! ⚠ A probe that does not abort up to the search ceiling prints `> ceiling`, not a number:
//! the shape costs stack per element too slowly to matter at the sizes searched.

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_store::DurableStore;
use std::process::Command;
use std::sync::Arc;

const SHAPES: [&str; 10] = [
    "paren", "brace", "bracket", "plus", "and", "union", "optional", "path", "apath", "alt",
];
const CEILING: usize = 200_000;

/// The query of `shape` at size `n`.
fn query(shape: &str, n: usize) -> String {
    match shape {
        "paren" => format!(
            "SELECT * WHERE {{ FILTER({}1{}) }}",
            "(".repeat(n),
            ")".repeat(n)
        ),
        "brace" => format!("SELECT * WHERE {}{}", "{".repeat(n), "}".repeat(n)),
        "bracket" => format!(
            "SELECT * WHERE {{ ?s <urn:p> {}1{} }}",
            "[ <urn:p> ".repeat(n),
            " ]".repeat(n)
        ),
        "plus" => format!("SELECT * WHERE {{ FILTER(1{}) }}", "+1".repeat(n)),
        "and" => format!("SELECT * WHERE {{ FILTER(true{}) }}", "&&true".repeat(n)),
        "union" => format!("SELECT * WHERE {{ {{}}{} }}", " UNION {}".repeat(n)),
        "optional" => format!("SELECT * WHERE {{ {{}}{} }}", " OPTIONAL {}".repeat(n)),
        "path" => format!("SELECT * WHERE {{ ?s <urn:p>{} ?o }}", "/<urn:p>".repeat(n)),
        // The densest chains: two bytes an element.
        "apath" => format!("SELECT * WHERE {{ ?s a{} ?o }}", "/a".repeat(n)),
        "alt" => format!("SELECT * WHERE {{ ?s a{} ?o }}", "|a".repeat(n)),
        other => panic!("unknown shape {other}"),
    }
}

/// One probe, in this process, on a thread of `stack` bytes. Returns normally or aborts.
fn probe(mode: &str, shape: &str, n: usize, stack: usize) {
    let text = query(shape, n);
    let mode = mode.to_string();
    let outcome = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || match mode.as_str() {
            "parse" => spargebra::SparqlParser::new()
                .parse_query(&text)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            "store" => {
                // The store's own space, NOT gonk's hub: the hub refuses these queries now
                // (`ikigai_gonk::sparql::bounded`), and this measures what that bound protects.
                let hub = Kernel::new(Arc::new(ikigai_store::space(
                    DurableStore::in_memory().unwrap(),
                )));
                let request = Request::new(
                    Verb::Source,
                    Iri::parse("urn:iki:store:graph-select").unwrap(),
                )
                .with_arg("query", ArgRef::Inline(text.into_bytes()))
                .with_arg("graph", ArgRef::Inline(b"urn:g".to_vec()));
                block_on(Kernel::issue(&hub, request, &Capability::root()))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            other => panic!("unknown mode {other}"),
        })
        .unwrap()
        .join()
        .unwrap();
    // A refusal is a clean exit too: the process lived, which is all this measures.
    if let Err(e) = outcome {
        let short: String = e.chars().take(120).collect();
        eprintln!("refused: {short}");
    }
}

/// Whether a child process survives `mode`/`shape` at size `n`.
fn survives(mode: &str, shape: &str, n: usize, stack: usize) -> bool {
    Command::new(std::env::current_exe().unwrap())
        .args(["probe", mode, shape, &n.to_string(), &stack.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("probe") {
        let n = args[3].parse().unwrap();
        let stack = args[4].parse().unwrap();
        probe(&args[1], &args[2], n, stack);
        return;
    }
    let kib: usize = args.first().map_or(2048, |k| k.parse().expect("stack KiB"));
    let shapes: Vec<&str> = match args.get(1) {
        Some(named) => named.split(',').collect(),
        None => SHAPES.to_vec(),
    };
    let modes: Vec<&str> = match args.get(2) {
        Some(named) => named.split(',').collect(),
        None => vec!["parse", "store"],
    };
    let stack = kib * 1024;
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("largest size that does NOT abort, {profile} build, {kib} KiB stack:");
    print!("{:<10}", "shape");
    for mode in &modes {
        print!(" {mode:>12}");
    }
    println!();
    for shape in shapes {
        let mut row = format!("{shape:<10}");
        for mode in &modes {
            // Double until it aborts (or passes the ceiling), then bisect. Invariant while
            // bisecting: `lo` survives, `hi` aborts.
            let mut lo = 0usize;
            let mut hi = 64usize;
            while hi <= CEILING && survives(mode, shape, hi, stack) {
                lo = hi;
                hi *= 2;
            }
            let cell = if hi > CEILING {
                format!("> {lo}")
            } else {
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if survives(mode, shape, mid, stack) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                lo.to_string()
            };
            row.push_str(&format!(" {cell:>12}"));
        }
        println!("{row}");
    }
}
