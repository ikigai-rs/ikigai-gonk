//! `cargo run --release --example lapse-cost -- <scratch-dir> [ledgers] [items-per-ledger] [runs]`
//! — time one lapse sweep (`ikigai_gonk::lapse`, ledger #3790) with nothing to record, over a
//! RocksDB store of `ledgers` ledgers (default 5) of `items-per-ledger` items each (default 800).
//!
//! One item in ten is claimed on an hour's lease (running claims the sweep must look at and
//! pass over), and two items per ledger are claimed on a 1-second lease that runs out and is
//! recorded by a first, untimed sweep. Every timed sweep after that finds those already recorded
//! and writes nothing: the steady state of a server whose workers are healthy. Each is timed as
//! the timer runs it, under each ledger's own sweep capability.
//!
//! `<scratch-dir>` must not exist or must be empty; it is left in place for a second look.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::NextRun;
use ikigai_gonk::lapse::Lapses;
use ikigai_resolve::Resolver;
use ikigai_store::{DurableStore, SharerWrites};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: lapse-cost <scratch-dir> [ledgers] [items-per-ledger] [runs]";
    let scratch = PathBuf::from(args.next().expect(usage));
    let number =
        |arg: Option<String>, default: usize| arg.map_or(default, |n| n.parse().expect("a count"));
    let ledgers = number(args.next(), 5);
    let items = number(args.next(), 800);
    let runs = number(args.next(), 10);

    let (store, handle) = DurableStore::open_shared_declaring(
        scratch.join("store"),
        SharerWrites::only_the_default_graph(),
    )
    .expect("a scratch RocksDB store");
    let kernel = ikigai_gonk::compose(store);

    let started = Instant::now();
    let names: Vec<String> = (0..ledgers).map(|n| format!("ledger{n}")).collect();
    for name in &names {
        for n in 1..=items {
            sink(
                &kernel,
                &format!("urn:iki:ledger:{name}:append"),
                &[("content", "an item")],
            );
            let lease = match n {
                1 | 2 => "1s",
                n if n % 10 == 0 => "1h",
                _ => continue,
            };
            sink(
                &kernel,
                &format!("urn:iki:ledger:{name}:claim"),
                &[
                    ("item", &n.to_string()),
                    ("content", &format!("worker-{n}")),
                    ("lease", lease),
                ],
            );
        }
    }
    println!(
        "filed {} items in {ledgers} ledgers ({} on running leases, {} lapsing) in {:.1?}",
        ledgers * items,
        ledgers * (items / 10),
        ledgers * 2,
        started.elapsed()
    );
    std::thread::sleep(Duration::from_millis(1_500));

    let lapses = Lapses::new(Some(Duration::from_secs(60)), NextRun::default());
    let first = lapses.sweep(&kernel, &handle).expect("the first sweep");
    println!("first sweep (records the lapses): {first}");

    let mut times = Vec::new();
    for _ in 0..runs {
        let started = Instant::now();
        let line = lapses.sweep(&kernel, &handle).expect("a sweep");
        times.push(started.elapsed());
        assert!(line.contains("recorded 0 lapse(s)"), "{line}");
    }
    times.sort();
    println!(
        "{runs} sweeps with nothing to record: min {:.1?}, median {:.1?}, max {:.1?} ({:.1?} a ledger at the median)",
        times[0],
        times[times.len() / 2],
        times[times.len() - 1],
        times[times.len() / 2] / u32::try_from(ledgers.max(1)).unwrap_or(1)
    );
    for (ledger, swept) in lapses.swept() {
        println!(
            "  {ledger}: recorded {}, already recorded {}",
            swept.recorded, swept.already_recorded
        );
    }
}

fn sink(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) {
    let request = args.iter().fold(
        Request::new(Verb::Sink, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    kernel
        .issue_as(request, &Capability::root())
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
}
