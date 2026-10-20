//! Ledger [#1128](http://localhost:1060/l/default/item/1128) and
//! [#1198](http://localhost:1060/l/default/item/1198): what one render of a ledger's page asks
//! the hub, and what the hub asks the store.
//!
//! The page needs two things from the ledger: how many items its filter matches, and the rows
//! of this page. Through ikigai-ledger 0.8.0 the number came from `items limit=1
//! as=application/json`, whose JSON face always runs the corpus check (`model::defects`, two
//! scans of the whole graph) to fill an `unreadable` list this page never reads. 0.9.0 names
//! the number on its own, `urn:iki:ledger:{L}:count`: the same filter, the same COUNT query,
//! and nothing else.
//!
//! - [`one_render_reads_the_count_and_one_page_of_rows`]
//! - [`a_worker_may_read_the_count`]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

/// Every invocation a kernel records while this is installed.
#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl Collect {
    fn take(&self) -> Vec<TraceEvent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A hub over an in-memory store holding `items` open items, and the HTTP door's kernel over
/// it with the page face bound — `main`'s shape without a listener.
fn served(
    items: usize,
) -> (
    tokio::runtime::Runtime,
    Arc<Kernel>,
    Kernel,
    tempfile::TempDir,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let config = tempfile::tempdir().unwrap();
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    for n in 0..items {
        runtime
            .block_on(hub.issue(
                request(
                    Verb::Sink,
                    "urn:iki:ledger:append",
                    &[("content", &format!("item {n}"))],
                ),
                &Capability::root(),
            ))
            .expect("a seed item");
    }
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let http = doors::http_kernel(Arc::clone(&hub), web::space(face));
    (runtime, hub, http, config)
}

/// What one render of `/l/default` asked the hub: each ledger read the page issued (a root of
/// the hub's trace), with how many store queries it ran. Cache hits are reads that ran none.
fn one_render(runtime: &tokio::runtime::Runtime, hub: &Kernel, http: &Kernel) -> Vec<Read> {
    let hub_trace = Arc::new(Collect::default());
    hub.set_tracer(hub_trace.clone());
    let page = request(Verb::Source, "urn:iki:gonk:page:ledger:default", &[]);
    let reader = Capability::scoped(grants_for("default", Authority::Read).unwrap());
    let html = runtime
        .block_on(http.issue_traced(page, &reader, Arc::new(Collect::default())))
        .expect("the ledger page renders");
    hub.clear_tracer();
    assert!(
        String::from_utf8_lossy(&html.bytes).contains("open items"),
        "the page states its count"
    );
    let events = hub_trace.take();
    let mut queries: BTreeMap<u64, usize> = BTreeMap::new();
    for event in &events {
        if event.target.starts_with("urn:iki:store:") {
            if let Some(parent) = event.parent {
                *queries.entry(parent).or_default() += 1;
            }
        }
    }
    let mut reads: Vec<Read> = events
        .iter()
        .filter(|e| e.parent.is_none() && e.target.starts_with("urn:iki:ledger:"))
        .map(|e| Read {
            target: e.target.clone(),
            store_queries: queries.get(&e.span).copied().unwrap_or(0),
            cache_hit: e.cache_hit,
        })
        .collect();
    reads.sort_by(|a, b| a.target.cmp(&b.target));
    reads
}

#[derive(Debug, PartialEq, Eq)]
struct Read {
    target: String,
    store_queries: usize,
    cache_hit: bool,
}

fn read(target: &str, store_queries: usize, cache_hit: bool) -> Read {
    Read {
        target: target.to_string(),
        store_queries,
        cache_hit,
    }
}

#[test]
fn one_render_reads_the_count_and_one_page_of_rows() {
    let (runtime, hub, http, _config) = served(5);
    // ★ BEFORE (ikigai-ledger 0.8.0, gonk through PR #129), measured by this test: the total
    // was `items limit=1` as JSON, and a cold render read
    //     [items: 7 store queries, items: 3 store queries]
    // — the total's seven being the COUNT, the one row, and the corpus check's scans.
    // AFTER: the total is one COUNT.
    // ⚠ The page of rows is FOUR store queries since ikigai-ledger 0.12.0 (ledger #1454), three
    // through 0.11.1: loading a set of items now runs one query of its own for the INBOUND
    // `parent` edges, so every face of an item can name its children (`<child> ledger:parent
    // <item>` in the Turtle this page reads). The count is unchanged at one.
    let cold = one_render(&runtime, &hub, &http);
    assert_eq!(
        cold,
        vec![
            read("urn:iki:ledger:default:count", 1, false),
            read("urn:iki:ledger:default:items", 4, false),
        ]
    );
    // Both reads are cacheable: the next render asks the hub twice and the store nothing…
    assert_eq!(
        one_render(&runtime, &hub, &http),
        vec![
            read("urn:iki:ledger:default:count", 0, true),
            read("urn:iki:ledger:default:items", 0, true),
        ]
    );
    // …until a write cuts the ledger's thread, when the count is recomputed, and agrees.
    runtime
        .block_on(hub.issue(
            request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("content", "one more")],
            ),
            &Capability::root(),
        ))
        .expect("a sixth item");
    assert_eq!(
        one_render(&runtime, &hub, &http),
        vec![
            read("urn:iki:ledger:default:count", 1, false),
            read("urn:iki:ledger:default:items", 4, false),
        ]
    );
}

/// The worker grant (claim, renew, release, comment and read; never close) holds the read row,
/// and `count` declares exactly the read row `items` does, so a worker may count what it may
/// list. Nothing new to mint for it.
#[test]
fn a_worker_may_read_the_count() {
    let (runtime, hub, _http, _config) = served(3);
    let worker = Capability::scoped(grants_for("default", Authority::Worker).unwrap());
    let count = |filter: &str| {
        let answer = runtime
            .block_on(hub.issue(
                request(
                    Verb::Source,
                    "urn:iki:ledger:default:count",
                    &[("status", filter)],
                ),
                &worker,
            ))
            .expect("a worker counts the ledger");
        String::from_utf8_lossy(&answer.bytes).trim().to_string()
    };
    assert_eq!(count("open"), "3");
    assert_eq!(count("closed"), "0");
    // And the reader that cannot see the ledger cannot count it.
    let other = Capability::scoped(grants_for("other", Authority::Read).unwrap());
    assert!(runtime
        .block_on(hub.issue(
            request(Verb::Source, "urn:iki:ledger:default:count", &[]),
            &other,
        ))
        .is_err());
}
