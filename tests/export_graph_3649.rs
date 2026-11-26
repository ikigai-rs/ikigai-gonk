//! Ledger [#3649](http://localhost:1062/l/default/item/3649): a tenant exports its whole ledger
//! graph past the 100,000-quad, 16 MiB bound of a scoped answer, streamed under its own grant.
//!
//! The first test is the reproduction: a tenant holding exactly its ledger's read grant can
//! read its graph only through the store's scoped doors, whose answers are bounded, and the
//! store's streaming dump needs the broad read no tenant may hold. Over a graph of 120,000
//! quads and about 20 MiB of N-Quads, both refuse. The rest is the fix,
//! `urn:iki:gonk:export:graph`: the same graph, whole, under the same grant, as a stream, at the
//! hub and through a real QUIC door; another graph refused; a window past the time budget
//! refused rather than truncated.
//!
//! The 5,000,000-quad run is `#[ignore]`d (minutes in a debug build, beside a suite CI already
//! spends half an hour on); run it with
//! `cargo test --release --test export_graph_3649 -- --ignored`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use futures::StreamExt as _;
use ikigai_core::{ArgRef, Body, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose_with, doors, quic};
use ikigai_store::{DurableStore, SharerWrites};

const ACME: &str = "urn:iki:ledger:graph:acme";
const OTHER: &str = "urn:iki:ledger:graph:other";
const EXPORT: &str = "urn:iki:gonk:export:graph";

/// A hub with the backup family (and so the export) over `store`.
fn hub_over(store: DurableStore, handle: Arc<ikigai_store::Store>) -> Arc<Kernel> {
    let dataset = backup::Dataset::new(&store, handle);
    let dir = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(compose_with(
        store,
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir,
                keep: 5,
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
    ))
}

fn shared() -> (DurableStore, Arc<ikigai_store::Store>) {
    DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph())
        .expect("an in-memory store")
}

fn hub() -> Arc<Kernel> {
    let (store, handle) = shared();
    hub_over(store, handle)
}

/// `quads` quads in `graph`, each about 180 bytes of N-Quads.
fn nquads(graph: &str, quads: u64, from: u64) -> String {
    let padding = "x".repeat(120);
    let mut out = String::with_capacity(quads as usize * 190);
    for n in from..from + quads {
        out.push_str(&format!(
            "<urn:item:{n}> <urn:ik:title> \"{padding} {n}\" <{graph}> .\n"
        ));
    }
    out
}

fn load(hub: &Kernel, doc: &str) {
    let request = Request::new(Verb::Sink, Iri::parse("urn:iki:store:load").unwrap())
        .with_arg("content", ArgRef::Inline(doc.as_bytes().to_vec()))
        .with_arg("format", ArgRef::Inline(b"application/n-quads".to_vec()));
    block_on(hub.issue(request, &Capability::root())).expect("the load");
}

/// The hub of [`hub`], seeded: 120,000 quads in acme (past both of the base's bounds) and a
/// few in another tenant's graph.
fn seeded() -> Arc<Kernel> {
    let hub = hub();
    load(&hub, &nquads(ACME, 120_000, 0));
    load(&hub, &nquads(OTHER, 10, 1_000_000));
    hub
}

/// The tenant: exactly what `grants_for(acme, Read)` gives a certificate or a passkey.
fn tenant() -> Capability {
    Capability::scoped(grants_for("acme", Authority::Read).unwrap())
}

fn export_request(graph: &str) -> Request {
    Request::new(Verb::Source, Iri::parse(EXPORT).unwrap())
        .with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()))
}

/// What a drained export held: its bytes, per-graph counts and the largest chunk.
#[derive(Debug, Default)]
struct Drained {
    bytes: u64,
    graphs: BTreeMap<String, u64>,
    largest_chunk: usize,
    lines: u64,
}

/// Read `body` a chunk at a time, counting as it goes and holding no more than a chunk and a
/// partial line; its error if it fails.
fn drain(body: Body) -> Result<Drained, Error> {
    let mut stream = body.into_stream();
    let mut drained = Drained::default();
    let mut partial: Vec<u8> = Vec::new();
    block_on(async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            drained.bytes += chunk.len() as u64;
            drained.largest_chunk = drained.largest_chunk.max(chunk.len());
            partial.extend_from_slice(&chunk);
            let end = partial
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |at| at + 1);
            count(&partial[..end], &mut drained);
            partial.drain(..end);
        }
        Ok::<_, Error>(())
    })?;
    assert!(partial.is_empty(), "the export ends between quads");
    Ok(drained)
}

fn count(lines: &[u8], drained: &mut Drained) {
    let parser = oxigraph::io::RdfParser::from_format(oxigraph::io::RdfFormat::NQuads);
    for quad in parser.for_slice(lines) {
        let quad = quad.expect("the export is N-Quads");
        *drained
            .graphs
            .entry(quad.graph_name.to_string())
            .or_default() += 1;
        drained.lines += 1;
    }
}

/// ★ The reproduction. Every way a tenant could read its whole graph before #3649 is refused
/// once the graph is past the base: the scoped CONSTRUCT on rows (or bytes), and the dump on
/// authority.
#[test]
fn a_tenant_cannot_read_its_whole_graph_through_the_scoped_doors() {
    let hub = seeded();
    let construct = Request::new(
        Verb::Source,
        Iri::parse("urn:iki:store:graph-construct").unwrap(),
    )
    .with_arg("graph", ArgRef::Inline(ACME.as_bytes().to_vec()))
    .with_arg(
        "query",
        ArgRef::Inline(b"CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }".to_vec()),
    );
    let refused = block_on(hub.issue(construct, &tenant()));
    assert!(
        matches!(
            refused,
            Err(Error::InvalidArgument { .. } | Error::OverBudget { .. })
        ),
        "the scoped answer is bounded: {refused:?}"
    );
    let dump = Request::new(Verb::Source, Iri::parse("urn:iki:store:dump").unwrap());
    let refused = block_on(hub.issue_body(dump, &tenant()));
    assert!(
        matches!(refused, Err(Error::Denied(_))),
        "the dump needs the broad read: {:?}",
        refused.map(|_| ())
    );
}

/// ★ The fix: the same tenant, the same grant, the whole graph — every quad of acme and none
/// of another graph, past both bounds, as a stream of small chunks.
#[test]
fn a_tenant_exports_its_whole_graph_past_the_base_as_a_stream() {
    let hub = seeded();
    let answer = block_on(hub.issue_body(export_request(ACME), &tenant())).expect("the export");
    assert_eq!(answer.repr_type.media_type, "application/n-quads");
    assert!(answer.bytes.is_one_shot(), "the export is streamed");
    let drained = drain(answer.bytes).expect("the whole export");
    assert_eq!(
        drained.graphs,
        BTreeMap::from([(format!("<{ACME}>"), 120_000)])
    );
    assert!(drained.bytes > 16 << 20, "{} bytes", drained.bytes);
    assert!(
        drained.largest_chunk <= 64 * 1024 + 1024,
        "chunks are bounded: {}",
        drained.largest_chunk
    );
}

/// The export reads exactly the graph whose token the caller holds.
#[test]
fn a_tenant_cannot_export_another_graph() {
    let hub = seeded();
    for graph in [OTHER, "urn:iki:ledger:graph:acme:deleted"] {
        let refused = block_on(hub.issue_body(export_request(graph), &tenant()));
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{graph}: {:?}",
            refused.map(|_| ())
        );
    }
    let nothing = Capability::scoped(Vec::<String>::new());
    let refused = block_on(hub.issue_body(export_request(ACME), &nothing));
    assert!(
        matches!(refused, Err(Error::Denied(_))),
        "{:?}",
        refused.map(|_| ())
    );
}

/// A window the store's time budget cannot produce is refused, and the stream ends with the
/// error: it is never a short, clean end.
#[test]
fn a_window_past_the_time_budget_ends_the_stream_with_an_error() {
    let (store, handle) = shared();
    let store = store.with_time_budget(ikigai_store::budget::TimeBudget::new(
        Duration::from_millis(1),
    ));
    let hub = hub_over(store, handle);
    load(&hub, &nquads(ACME, 120_000, 0));
    let answer = block_on(hub.issue_body(export_request(ACME), &tenant())).expect("opened");
    let failed = drain(answer.bytes);
    assert!(matches!(failed, Err(Error::Timeout(_))), "{failed:?}");
}

/// The same export through the QUIC door a tenant's certificate reaches, read off the wire as
/// a stream: the admission overlay judges it as a raw read and hands it on unbuffered. Through
/// the overlay's buffered path this graph is refused `OverBudget`.
#[test]
fn a_tenant_exports_its_whole_graph_through_the_quic_door() {
    let dir = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(dir.path());
    let (server, _) = quic::server_identity(&layout).unwrap();
    let client = quic::add_client(&layout, "acme", None, false).unwrap();
    let grant = grants_for("acme", Authority::Read).unwrap();
    quic::enrol(&layout, "acme", &client.fingerprint, &grant, false).unwrap();
    let trusted: Vec<String> = quic::trusted_client_certs(&layout)
        .unwrap()
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    // Past the 64 MiB a scoped caller's run may collect (`STREAM_MEMORY_BASE`), so the answer
    // arrives whole only if every layer between the endpoint and the wire streams it.
    let hub = hub();
    load(&hub, &nquads(ACME, 200_000, 0));
    load(&hub, &nquads(ACME, 200_000, 200_000));
    load(&hub, &nquads(OTHER, 10, 1_000_000));
    let door = doors::quic_kernel_with(Arc::clone(&hub), None, layout.clone());
    let udp = ikigai_quic::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr: SocketAddr = udp.local_addr().unwrap();
    let minter = quic::minter(layout.clone());
    let server_identity = ikigai_quic::Identity {
        cert_pem: server.cert_pem.clone(),
        key_pem: server.key_pem,
    };
    std::thread::spawn(move || {
        ikigai_quic::serve_bounded_on(
            door,
            udp,
            &server_identity,
            &trusted,
            minter,
            ikigai_quic::DEFAULT_IDLE_TIMEOUT,
            None,
        )
    });
    let identity = ikigai_quic::Identity {
        cert_pem: std::fs::read_to_string(client.dir.join("client.crt")).unwrap(),
        key_pem: std::fs::read_to_string(client.dir.join("client.key")).unwrap(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let resolver = loop {
        if let Ok(connected) = ikigai_quic::connect(addr, &identity, &server.cert_pem) {
            break connected;
        }
        assert!(
            Instant::now() < deadline,
            "the QUIC door never became ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut streamed = resolver
        .issue_streamed(
            export_request(ACME),
            &tenant(),
            &ikigai_resolve::RunContext::default(),
            None,
        )
        .expect("the export over QUIC");
    let mut out = Vec::new();
    streamed.copy_to(&mut out).expect("the whole stream");
    let mut drained = Drained::default();
    count(&out, &mut drained);
    assert_eq!(
        drained.graphs,
        BTreeMap::from([(format!("<{ACME}>"), 400_000)])
    );
    assert!(
        out.len() as u64 > ikigai_gonk::STREAM_MEMORY_BASE,
        "{} bytes",
        out.len()
    );
    // And another tenant's graph is refused at the door, as a raw read is.
    let refused = resolver.issue_streamed(
        export_request(OTHER),
        &tenant(),
        &ikigai_resolve::RunContext::default(),
        None,
    );
    assert!(refused.is_err(), "another tenant's graph is refused");
}

/// The measurement the item asks for: 5,000,000 quads in one tenant's graph (about 900 MiB of
/// N-Quads, on RocksDB), exported whole under the tenant's read grant alone. Loaded in pieces, since the
/// store's load is bounded too.
#[test]
#[ignore = "5M quads: run in release, `-- --ignored`"]
fn a_tenant_exports_five_million_quads() {
    // On RocksDB, as the live store is: the in-memory backing would hold the dataset in RAM.
    let data = tempfile::tempdir().expect("tempdir");
    let (store, handle) =
        DurableStore::open_shared_declaring(data.path(), SharerWrites::only_the_default_graph())
            .expect("a durable store");
    let hub = hub_over(store, handle);
    let total: u64 = 5_000_000;
    let piece: u64 = 500_000;
    let loading = Instant::now();
    for from in (0..total).step_by(piece as usize) {
        load(&hub, &nquads(ACME, piece, from));
    }
    eprintln!("loaded {total} quads in {:?}", loading.elapsed());
    let exporting = Instant::now();
    let answer = block_on(hub.issue_body(export_request(ACME), &tenant())).expect("the export");
    let drained = drain(answer.bytes).expect("the whole export");
    eprintln!(
        "exported {} quads, {} bytes, in {:?}",
        drained.lines,
        drained.bytes,
        exporting.elapsed()
    );
    assert_eq!(
        drained.graphs,
        BTreeMap::from([(format!("<{ACME}>"), total)])
    );
}
