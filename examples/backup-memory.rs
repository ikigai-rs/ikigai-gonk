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
//!
//! `restore <archive> <new-store-dir>` restores through gonk's resource, reading a local
//! backup by name to avoid the socket's pipe budget. `verify <source> <restored>` reopens
//! both synthetic stores and checks exact membership of every quad and the total count.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::{compose, compose_with};
use ikigai_store::{DurableStore, SharerWrites};

const USAGE: &str = "usage: backup-memory seed <store-dir> <quads> | backup <store-dir> <rotation-dir> | baseline <store-dir> <rotation-dir> | restore <archive> <new-store-dir> | verify <source-store-dir> <restored-store-dir>";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["seed", dir, quads] => seed(Path::new(dir), quads.parse().expect("a quad count")),
        ["backup", dir, rotation] => run(Path::new(dir), Path::new(rotation), true),
        ["baseline", dir, rotation] => run(Path::new(dir), Path::new(rotation), false),
        ["restore", archive, into] => restore(Path::new(archive), Path::new(into)),
        ["verify", source, restored] => verify(Path::new(source), Path::new(restored)),
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

fn run(dir: &Path, rotation: &Path, take: bool) {
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
    let target = if take { backup::BACKUP } else { backup::STATUS };
    let started = Instant::now();
    let outcome = block_on(Kernel::issue(
        &kernel,
        Request::new(Verb::Source, Iri::parse(target).unwrap()),
        &Capability::scoped(backup::JOB_SCOPES),
    ));
    let elapsed = started.elapsed();
    match outcome {
        Ok(answer) => {
            let text = String::from_utf8_lossy(&answer.bytes);
            let mut lines = text.lines();
            println!(
                "{} in {elapsed:.2?}: {} / {}",
                if take { "backup" } else { "baseline" },
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

fn restore(archive: &Path, into: &Path) {
    let (store, handle) = DurableStore::in_memory_shared().expect("a scratch live store");
    let dataset = backup::Dataset::new(&store, handle);
    let kernel = compose_with(
        store,
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: archive.parent().unwrap().to_path_buf(),
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
    );
    let started = Instant::now();
    let answer = block_on(
        kernel.issue(
            Request::new(Verb::Sink, Iri::parse(backup::RESTORE).unwrap())
                .with_arg(
                    "archive",
                    ArgRef::Inline(
                        archive
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .as_bytes()
                            .to_vec(),
                    ),
                )
                .with_arg(
                    "into",
                    ArgRef::Inline(into.to_str().unwrap().as_bytes().to_vec()),
                ),
            &Capability::root(),
        ),
    )
    .unwrap_or_else(|e| panic!("restore refused in {:.2?}: {e}", started.elapsed()));
    println!(
        "restore in {:.2?}: {}",
        started.elapsed(),
        String::from_utf8_lossy(&answer.bytes)
    );
}

/// Independently reopen both synthetic stores and check every quad, not just counts.
/// This fixture has no blank nodes (whose labels a general RDF import may rename).
fn verify(source: &Path, restored: &Path) {
    let open = |path| {
        oxigraph::store::Store::open_with_options(
            path,
            oxigraph::store::StoreOptions::default().with_max_open_files(64),
        )
        .expect("the store reopens")
    };
    let source = open(source);
    let restored = open(restored);
    let started = Instant::now();
    assert_eq!(source.len().unwrap(), restored.len().unwrap());
    let mut graphs = std::collections::BTreeMap::<String, u64>::new();
    for quad in source.iter() {
        let quad = quad.unwrap();
        assert!(restored.contains(&quad).unwrap(), "missing quad: {quad}");
        *graphs.entry(quad.graph_name.to_string()).or_default() += 1;
    }
    println!(
        "verified every quad in {:.2?}: {graphs:?}",
        started.elapsed()
    );
}
