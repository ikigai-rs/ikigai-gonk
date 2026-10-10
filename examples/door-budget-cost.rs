//! `cargo run --release --example door-budget-cost -- <store-dir>` — how long gonk's OWN
//! ledger reads take for an anonymous HTTP caller, over a RocksDB store restored from a live
//! archive (ledger #979): the evidence that the anonymous SPARQL budget (`crate::budget`)
//! would cap them if it applied to them, and that the store's 5 s base does not.
//!
//! ```sh
//! cargo run --release --example backup-cost -- ~/.ikigai/backups/<archive>.nq.gz /tmp/bcost 1
//! cargo run --release --example door-budget-cost -- /tmp/bcost/store
//! ```
//!
//! `<store-dir>` must be a store this process may open (never `~/.ikigai/store`, which the
//! live gonk holds): `backup-cost` leaves one restored from an archive. Each read is issued
//! twice through the HTTP door's kernel under the capability the door computes for an
//! anonymous loopback caller of the `default` ledger: the first is cold (the hub caches
//! nothing yet), the second is what a poll costs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{budget, compose, doors, quic, web};
use ikigai_store::DurableStore;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: door-budget-cost <store-dir>"),
    );
    let store = DurableStore::open(&dir).expect("the store (not one a live gonk holds)");
    let hub = Arc::new(compose(store));
    let config = tempfile::tempdir().expect("a scratch config home");
    let face = Arc::new(web::Web {
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
    });
    let http = doors::http_kernel(Arc::clone(&hub), web::space(face));
    let anonymous = Capability::scoped(
        grants_for("default", Authority::Write)
            .expect("the ledger's tokens")
            .into_iter()
            .chain([budget::anonymous_marker(
                budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
            )]),
    );
    let reads: [(&str, &[(&str, &str)]); 11] = [
        ("urn:iki:ledger:ledgers", &[]),
        ("urn:iki:ledger:items", &[]),
        (
            "urn:iki:ledger:items",
            &[("status", "all"), ("limit", "100000")],
        ),
        (
            "urn:iki:ledger:items",
            &[("status", "open"), ("limit", "100000")],
        ),
        (
            "urn:iki:ledger:items",
            &[
                ("as", "application/json"),
                ("status", "all"),
                ("limit", "100000"),
            ],
        ),
        ("urn:iki:ledger:next", &[]),
        ("urn:iki:ledger:next", &[("limit", "100000")]),
        ("urn:iki:ledger:item:900", &[]),
        ("urn:iki:gonk:page:home", &[("as", "text/html")]),
        ("urn:iki:gonk:page:ledger:default", &[("as", "text/html")]),
        ("urn:iki:gonk:page:item:default:900", &[("as", "text/html")]),
    ];
    // The SPARQL page's own sample queries, as an anonymous caller sends them through
    // `urn:sparql:select` — the queries the door budget DOES apply to — so the budget is set
    // against what a reader of the ledger actually asks.
    for (label, _, query) in web::SAMPLES {
        let started = Instant::now();
        let answer = block_on(
            http.issue(
                Request::new(
                    Verb::Source,
                    Iri::parse("urn:sparql:select").expect("an IRI"),
                )
                .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec())),
                &anonymous,
            ),
        );
        println!(
            "sample {label:<40} {:>9.1?}  {}",
            started.elapsed(),
            match answer {
                Ok(repr) => format!("{} bytes", repr.bytes.len()),
                Err(e) => format!("REFUSED: {e}"),
            }
        );
    }
    for (iri, args) in reads {
        let request = || {
            args.iter().fold(
                Request::new(Verb::Source, Iri::parse(iri).expect("a constant IRI")),
                |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
            )
        };
        let mut times = Vec::new();
        let mut outcome = String::new();
        for _ in 0..2 {
            let started = Instant::now();
            let answer = block_on(http.issue(request(), &anonymous));
            times.push(started.elapsed());
            outcome = match answer {
                Ok(repr) => format!("{} bytes", repr.bytes.len()),
                Err(e) => format!("REFUSED: {e}"),
            };
        }
        let shown: Vec<String> = args.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!(
            "{iri} {:<14} cold {:>9.1?}  warm {:>9.1?}  {outcome}",
            shown.join(" "),
            times[0],
            times[1]
        );
    }
}
