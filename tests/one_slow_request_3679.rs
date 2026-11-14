//! Ledger [#3679](http://localhost:1062/l/default/item/3679): one long HTTP request must not
//! stall every other one.
//!
//! The claim: while one Dashboard request ran thousands of store queries in a row, no other HTTP
//! request was answered, static files included. This builds the HTTP door as `main` does (a
//! multi-thread runtime, the accept loop under `block_on`), adds one endpoint that does what the
//! Agents panel did — budgeted store queries through `urn:sparql:select`, one after another, for
//! seconds — and times `/static/gonk.css` while it runs.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Description, Endpoint, EndpointSpace, Exact, Invocation, Iri, Representation, Request,
    Result, Space, Verb,
};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

/// How long the slow request keeps querying.
const SLOW_FOR: Duration = Duration::from_secs(6);

/// What a static file may take while the slow request runs: generous for a loaded CI host, and
/// far below `SLOW_FOR`, so an answer that waited for the slow request fails.
const PATIENCE: Duration = Duration::from_secs(2);

const SLOW_IRI: &str = "urn:iki:gonk:page:slow3679";

/// Store queries back to back for [`SLOW_FOR`], each awaited as the Agents panel awaits its own.
struct Slow;

#[async_trait]
impl Endpoint for Slow {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let started = Instant::now();
        let mut asked = 0u64;
        while started.elapsed() < SLOW_FOR {
            let read = request(
                "urn:sparql:select",
                &[
                    ("query", "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"),
                    ("graph", "urn:iki:ledger:graph:default"),
                ],
            );
            inv.issue(read).await?;
            asked += 1;
        }
        Ok(Representation::new(
            ikigai_core::ReprType::new("text/plain"),
            format!("{asked} queries").into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "slow-3679"
    }

    fn describe(&self) -> Description {
        Description::new("slow-3679").verb(Verb::Source)
    }
}

fn request(iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// The HTTP door as `main` builds it, with [`Slow`] bound in front of the pages. Returns the
/// address and the runtime (kept alive by the caller).
fn start() -> (SocketAddr, tempfile::TempDir) {
    let anonymous = grants_for_all(&["default".to_string()], Authority::Read).unwrap();
    // `main`'s runtime takes tokio's default worker count; `W3679=n` pins it, for the record
    // (one worker stalls, as any blocking endpoint on one thread would).
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(workers) = std::env::var("W3679").ok().and_then(|w| w.parse().ok()) {
        builder.worker_threads(workers);
    }
    let runtime = builder
        .thread_stack_size(ikigai_gonk::stack::THREAD_STACK_BYTES)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        page_assets: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    let slow: Arc<dyn Space> = Arc::new(EndpointSpace::new().bind(Exact::new(SLOW_IRI), Slow));
    let http = Arc::new(doors::http_kernel_serving(
        Arc::clone(&hub),
        web::space(face),
        vec![slow],
        None,
    ));
    let door = doors::HttpDoor {
        anonymous,
        bind: addr,
        passkeys: Some(passkeys),
        public: Vec::new(),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door.clone()),
            listener,
            doors::edge_config(door),
        ))
    });
    (addr, config)
}

/// `GET path` over its own connection: `(status, body)`, or `None` when no answer came within
/// `within`.
fn get(addr: SocketAddr, path: &str, within: Duration) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(within)).unwrap();
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nSec-Fetch-Site: none\r\n\
         Connection: close\r\n\r\n",
        addr.port()
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, body.to_string()))
}

#[test]
fn a_static_file_is_answered_while_one_request_queries_the_store_for_seconds() {
    let (addr, _config) = start();
    // The door is up, and a static file is quick when nothing else runs.
    let (status, _) = get(addr, "/static/gonk.css", PATIENCE).expect("gonk.css when idle");
    assert_eq!(status, 200);

    let slow = std::thread::spawn(move || {
        get(addr, "/iki/gonk/page/slow3679", SLOW_FOR * 4).expect("the slow request's answer")
    });
    // Let the slow request start querying.
    std::thread::sleep(Duration::from_millis(500));

    let mut waits = Vec::new();
    while !slow.is_finished() {
        let asked = Instant::now();
        let answer = get(addr, "/static/gonk.css", SLOW_FOR * 2);
        let waited = asked.elapsed();
        assert!(
            matches!(answer, Some((200, _))),
            "gonk.css while the slow request ran: {answer:?} after {waited:?}"
        );
        waits.push(waited);
        std::thread::sleep(Duration::from_millis(100));
    }
    let (status, body) = slow.join().unwrap();
    assert_eq!(status, 200, "the slow request: {body}");
    eprintln!("slow: {body}; waits: {waits:?}");
    let worst = waits.iter().max().copied().unwrap_or_default();
    assert!(
        worst < PATIENCE,
        "a static file waited {worst:?} while one request queried the store ({body}); \
         every wait: {waits:?}"
    );
}
