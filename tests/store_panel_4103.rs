//! Ledger [#4103](http://localhost:1062/l/default/item/4103): the Dashboard's Store panel read the
//! dataset's quad total from `urn:iki:store:info`, which counts it by scanning every quad, on every
//! 30-second poll of an open tab. On a live store that is a second or more per read, and on an
//! active server every write cuts the threads its cache entry hangs from, so the hub's cache never
//! held it. The count is now taken at most once per [`system::QUADS_EVERY`] and said with its age.
//!
//! `cargo test --release --test store_panel_4103 -- --ignored --nocapture` prints the time per read.

use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::compose_with;
use ikigai_gonk::system::{self, System};
use ikigai_ledger::Ledger;
use ikigai_store::{DurableStore, SharerWrites};
use serde_json::Value;

struct Hub {
    hub: Arc<Kernel>,
    _dirs: Vec<tempfile::TempDir>,
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

impl Hub {
    fn start() -> Hub {
        let store_dir = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        let (store, handle) =
            DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph())
                .unwrap();
        let dataset = backup::Dataset::new(&store, handle);
        let every = std::time::Duration::from_secs(86_400);
        let system = System::new(
            store_dir.path().to_path_buf(),
            backups.path().to_path_buf(),
            Some(every),
        );
        let hub = Arc::new(compose_with(
            store,
            None,
            Vec::new(),
            vec![Arc::new(system.space())],
            Some(Backups {
                settings: Arc::new(backup::Settings {
                    dir: backups.path().to_path_buf(),
                    keep: 5,
                    every: Some(every),
                    store_path: "/nonexistent/live/store".into(),
                    scripts: None,
                }),
                jobs: None,
                next: Default::default(),
                dataset,
            }),
            None,
            None,
        ));
        system.attach(&hub);
        Hub {
            hub,
            _dirs: vec![store_dir, backups],
        }
    }

    fn issue(&self, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Vec<u8> {
        futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(verb, iri, args),
            &Capability::root(),
        ))
        .unwrap_or_else(|e| panic!("{iri}: {e}"))
        .bytes
    }

    fn append(&self, content: &str) {
        let iri = format!("{}append", Ledger::parse("default").unwrap().prefix());
        self.issue(Verb::Sink, &iri, &[("content", content)]);
    }

    /// What the Store panel reads, as an operator's tab reads it.
    fn store(&self) -> Value {
        let bytes = futures::executor::block_on(Kernel::issue(
            &self.hub,
            request(Verb::Source, system::STORE, &[]),
            &Capability::scoped([system::CAP_READ]),
        ))
        .expect("the store row")
        .bytes;
        serde_json::from_slice(&bytes).expect("JSON")
    }

    /// `count` quads in a graph of their own, in one load.
    fn load(&self, count: usize) {
        let mut doc = String::with_capacity(count * 80);
        for n in 0..count {
            doc.push_str(&format!(
                "<urn:example:s{n}> <urn:example:p> \"sample value {n}\" <urn:example:bulk> .\n"
            ));
        }
        self.issue(
            Verb::Sink,
            "urn:iki:store:load",
            &[("content", &doc), ("format", "application/n-quads")],
        );
    }
}

/// A poll after a write does not count the dataset again: the total is the one already taken,
/// said with when it was taken.
#[test]
fn a_poll_reuses_the_quad_count_it_took() {
    let hub = Hub::start();
    hub.append("sample item");
    let first = hub.store();
    let quads = first["quads"].as_u64().expect("a count");
    assert!(quads > 0, "{first}");

    hub.append("another sample item");
    let second = hub.store();
    assert_eq!(
        second["quads"].as_u64(),
        Some(quads),
        "re-counted: {second}"
    );
    let counted_at = first["quads_counted_at_ms"].as_u64();
    assert!(counted_at.is_some(), "when it was counted: {first}");
    assert_eq!(second["quads_counted_at_ms"].as_u64(), counted_at);
    // What is cheap stays live.
    assert!(second["size_bytes"].is_u64(), "{second}");
    assert!(second["backup"]["attention"].is_boolean(), "{second}");
}

/// The time per read of the Store row, the first read (which counts) and the polls after it.
#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn time_per_read() {
    const QUADS: usize = 1_000_000;
    let hub = Hub::start();
    hub.load(QUADS);
    let started = Instant::now();
    let first = hub.store();
    let cold = started.elapsed();
    assert!(first["quads"].as_u64().unwrap() >= QUADS as u64, "{first}");
    let mut polls = Vec::new();
    for n in 0..10 {
        // A write between polls, as an active server has: it cuts the store's threads.
        hub.append(&format!("sample item {n}"));
        let started = Instant::now();
        hub.store();
        polls.push(started.elapsed());
    }
    polls.sort();
    println!(
        "{QUADS} quads: first read {cold:?}; polls after a write: median {:?}, max {:?}",
        polls[polls.len() / 2],
        polls[polls.len() - 1]
    );
}
