//! `cargo run --release --example dashboard-poll-cost [-- <items> <agents> <polls>]`: what one poll
//! of an open Dashboard costs gonk, on an idle ledger and on a busy one (ledger #2967), by the tab
//! that asks it, and what a switch to each tab costs (ledger #2971).
//!
//! One ledger over real HTTP, its items spread over 300 days so the chart is a real one, and
//! `agents` holders who each hold a claim. A poll is one fragment asked with the validator of its
//! last answer, as `web/gonk.js` asks. A round of the Ledgers tab is one chart per ledger; of the
//! Agents tab, one table per ledger and the sentence over them. A switch is one panel, asked with
//! no validator. The tab not shown asks for nothing, so it has no row.
//!
//! - **idle**: nothing writes between polls.
//! - **busy**: one write before every poll, the worst a ledger can do to a poll: at the 30 s cadence
//!   that is a ledger written at least twice a minute, for all of them.
//!
//! Prints the median and the slowest answer, the bytes on the wire and the status of the polls, and
//! the store queries and the transforms they made, per poll. ⚠ A table with agents also changes
//! once a minute with no write (its links name the minute): a run shorter than a minute may or may
//! not show that, and the page's 30 s cadence makes it one `200` in two.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, TraceEvent, Tracer, Verb};
use ikigai_gonk::grants::{grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_ledger::Ledger;
use ikigai_store::DurableStore;

const LEDGER: &str = "default";
/// Items are filed in this many runs, each dated a day apart.
const DAYS: usize = 300;

#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);

impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn root_issue(hub: &Arc<Kernel>, verb: Verb, iri: &str, args: &[(&str, &str)]) {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    futures::executor::block_on(Kernel::issue(hub, request, &Capability::root()))
        .unwrap_or_else(|e| panic!("{iri}: {e}"));
}

/// One GET: `(status, bytes on the wire, ETag)`.
fn poll(addr: SocketAddr, path: &str, tag: &str) -> (u16, usize, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let validator = if tag.is_empty() {
        String::new()
    } else {
        format!("If-None-Match: {tag}\r\n")
    };
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: text/html\r\n\
         Sec-Fetch-Site: same-origin\r\n{validator}Connection: close\r\n\r\n",
        addr.port()
    );
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let text = String::from_utf8_lossy(&response);
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let etag = text
        .lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("etag")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    (status, response.len(), etag)
}

fn measure(
    label: &str,
    addr: SocketAddr,
    hub: &Arc<Kernel>,
    path: &str,
    polls: usize,
    conditional: bool,
    write: Option<&dyn Fn(usize)>,
) {
    let (_, _, first) = poll(addr, path, "");
    let mut tag = if conditional { first } else { String::new() };
    let (mut took, mut bytes, mut ok, mut not_modified) = (Vec::new(), 0, 0, 0);
    let (mut queries, mut transforms) = (0, 0);
    for n in 0..polls {
        if let Some(write) = write {
            write(n);
        }
        let trace = Arc::new(Collect::default());
        hub.set_tracer(trace.clone());
        let started = Instant::now();
        let (status, size, etag) = poll(addr, path, &tag);
        took.push(started.elapsed().as_secs_f64() * 1000.0);
        hub.clear_tracer();
        for event in trace.0.lock().unwrap().iter() {
            queries += usize::from(event.target.starts_with("urn:iki:store:"));
            transforms +=
                usize::from(event.target == ikigai_gonk::render::RENDER_IRI && !event.cache_hit);
        }
        bytes += size;
        match status {
            200 => ok += 1,
            304 => not_modified += 1,
            other => panic!("{path} answered {other}"),
        }
        if conditional {
            tag = etag;
        }
    }
    took.sort_by(f64::total_cmp);
    println!(
        "{label:<14} {path:<28} median {:>8.2} ms   max {:>8.2} ms   {:>7} bytes/poll   \
         {:>3} 200, {:>3} 304   {:>5.1} queries/poll   {:>4.2} transforms/poll",
        took[took.len() / 2],
        took[took.len() - 1],
        bytes / polls,
        ok,
        not_modified,
        queries as f64 / polls as f64,
        transforms as f64 / polls as f64,
    );
}

fn measure_both(
    addr: SocketAddr,
    hub: &Arc<Kernel>,
    path: &str,
    polls: usize,
    conditional: bool,
    write: &dyn Fn(usize),
) {
    measure("idle", addr, hub, path, polls, conditional, None);
    measure("busy", addr, hub, path, polls, conditional, Some(write));
}

fn main() {
    let arg = |n: usize, default: usize| {
        std::env::args()
            .nth(n)
            .map_or(default, |value| value.parse().expect("a number"))
    };
    let (items, agents, polls) = (arg(1, 3000), arg(2, 10), arg(3, 30));

    let runtime = tokio::runtime::Builder::new_multi_thread()
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
    let names = [LEDGER.to_string()];
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: names.to_vec(),
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
    let door = doors::HttpDoor {
        anonymous: grants_for_all(&names, Authority::Read).unwrap(),
        bind: addr,
        passkeys: Some(passkeys),
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

    let ledger = Ledger::parse(LEDGER).unwrap();
    let prefix = ledger.prefix();
    let graph = ledger.graph();
    let append = format!("{prefix}append");
    let started = Instant::now();
    for day in 0..DAYS {
        for n in 0..items / DAYS {
            let title = format!("sample item {day}.{n}");
            root_issue(&hub, Verb::Sink, &append, &[("content", &title)]);
        }
        // This run was filed just now, and every earlier one is already a day or more back.
        let back = DAYS - day;
        let update = format!(
            "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>\n\
             PREFIX dcterms: <http://purl.org/dc/terms/>\n\
             PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n\
             WITH <{graph}> DELETE {{ ?item dcterms:created ?at }} \
             INSERT {{ ?item dcterms:created ?then }} \
             WHERE {{ ?item a ledger:Item ; dcterms:created ?at . \
             FILTER(?at > NOW() - \"PT1H\"^^xsd:dayTimeDuration) \
             BIND(NOW() - \"P{back}D\"^^xsd:dayTimeDuration AS ?then) }}"
        );
        root_issue(
            &hub,
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[("graph", graph.as_str()), ("content", &update)],
        );
    }
    for n in 0..agents {
        let item = (n + 1).to_string();
        let holder = format!("sample-agent-{n}");
        root_issue(
            &hub,
            Verb::Sink,
            &format!("{prefix}claim"),
            &[("item", &item), ("content", &holder)],
        );
    }
    println!(
        "{items} items over {DAYS} days, {agents} agents: seeded in {:.1} s",
        started.elapsed().as_secs_f64()
    );

    let write = |n: usize| {
        let title = format!("another sample {n}");
        root_issue(&hub, Verb::Sink, &append, &[("content", &title)]);
    };
    println!("A poll of the Ledgers tab, one per ledger:");
    measure_both(addr, &hub, "/dashboard/chart/default", polls, true, &write);
    println!("A poll of the Agents tab, one per ledger, and the sentence once:");
    measure_both(addr, &hub, "/dashboard/agents/default", polls, true, &write);
    measure_both(addr, &hub, "/dashboard/agents-note", polls, true, &write);
    println!("A switch to each tab, once:");
    measure_both(addr, &hub, "/dashboard/panel/ledgers", polls, false, &write);
    measure_both(addr, &hub, "/dashboard/panel/agents", polls, false, &write);
}
