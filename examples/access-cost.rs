//! `cargo run --release --example access-cost -- [--rounds N]` — what the access log costs a
//! request (ledger #739).
//!
//! Two reads, each through a door kernel built the way `main` builds it, with the log OFF
//! (not wrapped at all, as `gonk.log.access = false` leaves it) and ON (wrapped, every line
//! formatted and written with one `write_all` to `/dev/null`, which is the syscall a line to
//! stderr costs without a terminal's speed in the number):
//!
//! - **a page**: `urn:iki:gonk:page:ledger:default` through the HTTP door's kernel — a ledger
//!   page with 50 items, sub-requests and an XSLT render, the hot path a browser drives;
//! - **a socket call**: `urn:iki:ledger:items` through the socket door's kernel — the cheapest
//!   read a door serves, where a fixed per-request cost would show most.
//!
//! The two variants are interleaved round by round, so drift on a shared machine lands on
//! both. Each prints min / median / mean in microseconds. Nothing touches `~/.ikigai`: the
//! store is in memory.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::access::{AccessLog, Door};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut rounds = 400usize;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rounds" => rounds = args.next().and_then(|n| n.parse().ok()).expect("a number"),
            other => panic!("unknown argument {other}"),
        }
    }

    let hub = Arc::new(compose(DurableStore::in_memory().expect("a store")));
    for n in 0..50 {
        block_on(Kernel::issue(
            &hub,
            Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
                .with_arg("content", ArgRef::Inline(format!("item {n}").into_bytes())),
            &Capability::root(),
        ))
        .expect("an append");
    }

    let devnull = Arc::new(Mutex::new(
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null"),
    ));
    let log = |door: Door| {
        let out = Arc::clone(&devnull);
        AccessLog::to(
            door,
            Arc::new(move |line: &str| {
                let mut bytes = String::with_capacity(line.len() + 1);
                bytes.push_str(line);
                bytes.push('\n');
                let _ = out.lock().unwrap().write_all(bytes.as_bytes());
            }),
        )
    };

    let config = tempfile::tempdir().expect("a config home");
    let face = || {
        Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new(),
            passkeys: Arc::new(Passkeys::new(
                quic::Layout::in_config_home(config.path()),
                1060,
            )),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        })
    };
    let anonymous = Capability::scoped(grants_for("default", Authority::Write).unwrap());

    let page = Request::new(
        Verb::Source,
        Iri::parse("urn:iki:gonk:page:ledger:default").unwrap(),
    )
    .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
    let http_off = doors::http_kernel_with(Arc::clone(&hub), web::space(face()), None);
    let http_on =
        doors::http_kernel_with(Arc::clone(&hub), web::space(face()), Some(log(Door::Http)));
    compare(
        "page   (http)",
        rounds,
        &page,
        &anonymous,
        &http_off,
        &http_on,
    );

    let items = Request::new(Verb::Source, Iri::parse("urn:iki:ledger:items").unwrap());
    let socket_off = doors::door_kernel_with(Arc::clone(&hub), None);
    let socket_on = doors::door_kernel_with(Arc::clone(&hub), Some(log(Door::Socket)));
    compare(
        "items  (socket)",
        rounds * 10,
        &items,
        &Capability::root(),
        &socket_off,
        &socket_on,
    );
}

fn compare(
    what: &str,
    rounds: usize,
    request: &Request,
    cap: &Capability,
    off: &Kernel,
    on: &Kernel,
) {
    let once = |kernel: &Kernel| {
        let started = Instant::now();
        block_on(Kernel::issue(kernel, request.clone(), cap)).expect("the read");
        started.elapsed().as_nanos() as f64 / 1000.0
    };
    // Warm both: the first render compiles the stylesheet, the first read fills the hub.
    for _ in 0..10 {
        once(off);
        once(on);
    }
    let (mut a, mut b) = (Vec::with_capacity(rounds), Vec::with_capacity(rounds));
    for round in 0..rounds {
        // Alternate which goes first, so neither always pays the other's leftovers.
        if round % 2 == 0 {
            a.push(once(off));
            b.push(once(on));
        } else {
            b.push(once(on));
            a.push(once(off));
        }
    }
    println!(
        "{what}: log off {}  |  log on {}",
        summary(&mut a),
        summary(&mut b)
    );
}

fn summary(samples: &mut [f64]) -> String {
    samples.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    format!(
        "min {:>8.1} µs  median {:>8.1} µs  mean {:>8.1} µs  (n={})",
        samples[0],
        samples[samples.len() / 2],
        mean,
        samples.len()
    )
}
