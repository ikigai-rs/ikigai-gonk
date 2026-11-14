//! `cargo run --release --example backup-memory -- seed <store-dir> <quads>` then
//! `/usr/bin/time -l target/release/examples/backup-memory backup <store-dir> <rotation-dir>`:
//! the PEAK MEMORY of one backup over a SYNTHETIC RocksDB store, the measurement behind ledger
//! #1699 (a backup held the dataset several times over: the SPARQL JSON answer, the N-Quads, a
//! copy of them for gzip).
//!
//! `seed` builds the store: `<quads>` quads shaped like the live dataset's (about 93% in one
//! browse graph beside a ledger graph, about 225 bytes of N-Quads each, literals that do not
//! all compress alike), loaded in pieces under the store's own load cap. `backup` opens it,
//! composes the hub as `main` does, and takes ONE backup under exactly `backup::JOB_SCOPES`, the
//! capability the timer fires under; `baseline` does everything `backup` does except the backup,
//! so the difference between their peaks is the backup's. Nothing here opens `~/.ikigai/store`
//! or reads a real archive.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::{compose, compose_with};
use ikigai_store::{DurableStore, SharerWrites};

const USAGE: &str = "usage: backup-memory seed <store-dir> <quads> | backup <store-dir> <rotation-dir> | chunks <store-dir> <rotation-dir> | restore-chunks <rotation-dir> <name> <new-store-dir> | compare <source-store-dir> <restored-store-dir> | baseline <store-dir> <rotation-dir>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["seed", dir, quads] => seed(Path::new(dir), quads.parse().expect("a quad count")),
        ["backup", dir, rotation] => run(Path::new(dir), Path::new(rotation), backup::BACKUP, None),
        ["baseline", dir, rotation] => {
            run(Path::new(dir), Path::new(rotation), backup::STATUS, None)
        }
        ["chunks", dir, rotation] => run(
            Path::new(dir),
            Path::new(rotation),
            backup::BACKUP_CHUNKS,
            None,
        ),
        ["restore-chunks", rotation, name, into] => {
            let scratch = tempfile::tempdir_in(rotation).expect("drill control directory");
            run(
                scratch.path(),
                Path::new(rotation),
                backup::RESTORE_CHUNKS,
                Some((name, into)),
            );
        }
        ["compare", source, restored] => compare(Path::new(source), Path::new(restored)),
        _ => panic!("{USAGE}"),
    }
}

/// One synthetic quad, `n` of them: deterministic, so two seeds of one size are one dataset.
fn quad(n: u64, out: &mut String) {
    let graph = if n.is_multiple_of(14) {
        "urn:iki:ledger:graph:default"
    } else {
        "urn:iki:browse:graph:default"
    };
    let mixed = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    out.push_str(&format!(
        "<urn:item:{}> <urn:ik:p{}> \"{n} {mixed:016x}{:016x} {}\" <{graph}> .\n",
        n / 8,
        n % 8,
        mixed.rotate_left(17) ^ n,
        "x".repeat(120)
    ));
}

fn seed(dir: &Path, quads: u64) {
    let kernel = compose(DurableStore::open(dir).expect("the scratch store opens"));
    let started = Instant::now();
    let mut n = 0;
    while n < quads {
        let mut chunk = String::new();
        for i in n..(n + 200_000).min(quads) {
            quad(i, &mut chunk);
        }
        n = (n + 200_000).min(quads);
        block_on(
            kernel.issue(
                Request::new(Verb::Sink, Iri::parse("urn:iki:store:load").unwrap())
                    .with_arg("content", ArgRef::Inline(chunk.into_bytes()))
                    .with_arg("format", ArgRef::Inline(b"application/n-quads".to_vec())),
                &Capability::root(),
            ),
        )
        .expect("the chunk loads");
    }
    println!("seeded {quads} quads in {:.2?}", started.elapsed());
}

fn run(dir: &Path, rotation: &Path, target: &str, restore: Option<(&str, &str)>) {
    std::fs::create_dir_all(rotation).expect("the rotation directory");
    let (store, handle) =
        DurableStore::open_shared_declaring(dir, SharerWrites::only_the_default_graph())
            .expect("the scratch store opens");
    let dataset = backup::Dataset::new(&store, handle);
    let kernel = Arc::new(compose_with(
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
                scripts: None,
            }),
            jobs: None,
            next: Default::default(),
            dataset,
        }),
        None,
        None,
    ));
    let request = match restore {
        Some((name, into)) => Request::new(Verb::Sink, Iri::parse(target).unwrap())
            .with_arg("content", ArgRef::Inline(name.as_bytes().to_vec()))
            .with_arg("into", ArgRef::Inline(into.as_bytes().to_vec())),
        None => Request::new(Verb::Source, Iri::parse(target).unwrap()),
    };
    let capability = if restore.is_some() {
        Capability::scoped([backup::CAP_RESTORE])
    } else {
        Capability::scoped(backup::JOB_SCOPES)
    };
    let started = Instant::now();
    let outcome = block_on(Kernel::issue(&kernel, request, &capability));
    let elapsed = started.elapsed();
    match outcome {
        Ok(answer) => {
            let text = String::from_utf8_lossy(&answer.bytes);
            let mut lines = text.lines();
            println!(
                "{} in {elapsed:.2?}: {} / {}",
                target,
                lines.next().unwrap_or(""),
                lines.next().unwrap_or("").trim()
            );
        }
        Err(e) => {
            eprintln!("REFUSED in {elapsed:.2?}: {e}");
            std::process::exit(1);
        }
    }
}

/// An independent complete quad comparison: no manifest counts or restore report involved.
/// The store index order depends on quad values, so equal datasets iterate identically.
fn compare(source: &Path, restored: &Path) {
    let started = Instant::now();
    let source = ikigai_store::Store::open(source).expect("source store opens");
    let restored = ikigai_store::Store::open(restored).expect("restored store opens");
    let mut expected = source.iter();
    let mut actual = restored.iter();
    let mut count = 0_u64;
    loop {
        match (expected.next(), actual.next()) {
            (None, None) => break,
            (Some(expected), Some(actual)) => {
                assert_eq!(expected.unwrap(), actual.unwrap(), "quad {count}");
                count += 1;
            }
            _ => panic!("dataset lengths differ after {count} quads"),
        }
    }
    println!(
        "compared all {count} quads: identical in {:.2?}",
        started.elapsed()
    );
}
