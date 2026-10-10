//! `cargo run --release --example backup-cost -- <backup.nq.gz> <scratch-dir> [runs]` — time
//! gonk's backup and restore over a RocksDB copy of a live dataset, the measurement behind
//! `backup::JOB_BUDGET_MS` (ledger #979) and the answer grants in `backup::JOB_SCOPES` (ledger #993).
//!
//! ```sh
//! cargo run --release --example backup-cost -- ~/.ikigai/backups/gonk-store-2026-10-09T122720Z.nq.gz /tmp/bcost
//! ```
//!
//! Nothing here opens `~/.ikigai/store`. The archive is RESTORED, through gonk's own
//! `urn:iki:gonk:restore`, into `<scratch-dir>/store` — a RocksDB store built from the bytes
//! the live server wrote, so the timing is RocksDB's, as the scheduled backup's is, and not a
//! torn `cp -r` of a directory a live process holds. Then a backup of that store is taken,
//! `runs` times (default 3), under four capabilities: exactly `backup::JOB_SCOPES` (what the
//! timer fires under), the same WITHOUT the budget grant (what it fired under through store
//! 0.2.6, and what it would get at the store's 5 s base), the same WITHOUT the two answer
//! grants (what it fired under through gonk's move to store 0.2.9, ledger #993, and what it
//! would get at the store's 100,000-row, 16 MiB answer base), and root (the owner's socket).
//!
//! `<scratch-dir>` must not exist or must be empty; it is left in place for a second look.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::compose_with;
use ikigai_store::DurableStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: backup-cost <backup.nq.gz> <scratch-dir> [runs]";
    let archive = PathBuf::from(args.next().expect(usage));
    let scratch = PathBuf::from(args.next().expect(usage));
    let runs: usize = args
        .next()
        .map_or(3, |n| n.parse().expect("runs is a number"));
    let store_dir = scratch.join("store");
    let rotation = scratch.join("rotation");
    std::fs::create_dir_all(&rotation).expect("the rotation directory");

    // 1. Restore, through the resource, into a RocksDB store — timed, because the verifier's
    //    per-graph count is a whole-dataset query too.
    let bytes = std::fs::read(&archive).expect("the archive");
    let loader = hub(DurableStore::in_memory().expect("in-memory"), &rotation);
    let started = Instant::now();
    let report = issue(
        &loader,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", bytes.as_slice()),
            ("into", store_dir.to_str().expect("a UTF-8 path").as_bytes()),
        ],
        &Capability::root(),
    );
    println!(
        "restore (root, verifier under its own scoped read): {:>8.2?}  {}",
        started.elapsed(),
        report
            .as_ref()
            .map(|r| r.lines().next().unwrap_or("").to_string())
            .unwrap_or_else(|e| format!("FAILED: {e}"))
    );
    drop(loader);
    if report.is_err() {
        return;
    }

    // 2. Back it up, under each capability.
    let store = DurableStore::open(&store_dir).expect("the restored store");
    let kernel = hub(store, &rotation);
    let without_grant: Vec<&str> = backup::JOB_SCOPES
        .iter()
        .copied()
        .filter(|scope| !scope.starts_with("urn:cap:store:budget:"))
        .collect();
    let without_answer: Vec<&str> = backup::JOB_SCOPES
        .iter()
        .copied()
        .filter(|scope| !scope.starts_with("urn:cap:store:answer:"))
        .collect();
    let cases: [(&str, Capability); 4] = [
        ("JOB_SCOPES", Capability::scoped(backup::JOB_SCOPES)),
        (
            "JOB_SCOPES without the grant",
            Capability::scoped(without_grant),
        ),
        (
            "JOB_SCOPES without the answer grants",
            Capability::scoped(without_answer),
        ),
        ("root", Capability::root()),
    ];
    for (name, capability) in &cases {
        for run in 1..=runs {
            let started = Instant::now();
            let outcome = issue(&kernel, Verb::Source, backup::BACKUP, &[], capability);
            println!(
                "backup {name:<38} run {run}: {:>8.2?}  {}",
                started.elapsed(),
                match outcome {
                    Ok(text) => text.lines().next().unwrap_or("").to_string(),
                    Err(e) => format!("REFUSED: {e}"),
                }
            );
        }
    }
}

fn hub(store: DurableStore, rotation: &Path) -> Arc<Kernel> {
    Arc::new(compose_with(
        store,
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: rotation.to_path_buf(),
                keep: 100,
                every: None,
                store_path: PathBuf::from("/nonexistent/live/store"),
            }),
            jobs: None,
        }),
    ))
}

fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &[u8])],
    capability: &Capability,
) -> Result<String, ikigai_core::Error> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a constant IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    );
    block_on(kernel.issue(request, capability))
        .map(|repr| String::from_utf8_lossy(&repr.bytes).into_owned())
}
