//! Ledger [#1175](http://localhost:1060/l/default/item/1175), its second half: an ANONYMOUS run
//! of a public script is held to the HTTP door's anonymous SPARQL budget, and anonymous runs are
//! rate-limited.
//!
//! ★ The reproduction. A script run evaluates its query in a SUB-request the script library
//! issues under the run's capability, which is the runner's ATTENUATED to what the script
//! derives — so the door's marker (`urn:iki:gonk:door:anonymous-budget:<ms>`, `crate::budget`)
//! is dropped, and the sub-request never passes the hub's front door where the marker is read.
//! Before this change an anonymous run's query ran to the store's BASE (here 2.5 s; 5 s live),
//! and any number of runs could be sent back to back.
//!
//! The slow script carries its own data, `tests/slow/mod.rs`'s product of `VALUES` tables each
//! row of which grows a million-character string, so it needs no graph to be slow and no size
//! bound can stop it before the clock (ledger [#1730](http://localhost:1062/l/default/item/1730):
//! the `SHA512` cross product it replaces finished inside the store's base in a release build).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{
    anonymous_http_grants, grants_for, script_grants, Authority, ScriptAuthority,
};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::script::Scripts;
use ikigai_gonk::{budget, compose_with, doors, quic, web};
use ikigai_store::budget::TimeBudget;
use ikigai_store::DurableStore;

mod slow;

/// The store's base in these servers: smaller than the real 5 s so a test is quick, and well
/// above the door budget.
const BASE: Duration = Duration::from_millis(2_500);

/// The anonymous door's budget in these servers.
const DOOR_MS: u64 = 400;

/// A person's principal, as the HTTP door mints one.
const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

/// The slow query, reading the default ledger by its own `FROM`.
fn slow_query() -> String {
    slow::rows().replacen("SELECT*", "SELECT*FROM<urn:iki:ledger:graph:default>", 1)
}

/// A query that answers at once, with a parameter so that each run can miss the cache.
const FAST: &str = "\
# Every item's number from a floor up.
# @param floor xsd:integer -- the lowest number to list
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
SELECT ?number
FROM <urn:iki:ledger:graph:default>
WHERE { ?item ledger:number ?number . FILTER(?number >= ?floor) }
";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A person who may publish the fixtures.
fn person() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    for name in ["slow", "fast"] {
        scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    }
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

/// The anonymous loopback caller, as the HTTP door computes its capability: the grants and
/// the door's budget marker.
fn anonymous() -> Capability {
    let mut scopes = anonymous_http_grants(&["default".to_string()]).unwrap();
    scopes.push(budget::anonymous_marker(DOOR_MS));
    Capability::scoped(scopes)
}

/// The hub as `main` composes it with scripts, over a store whose base is [`BASE`], with one
/// item filed and both fixtures published PUBLIC.
fn hub() -> (Arc<Kernel>, Scripts) {
    let scripts = Scripts::in_memory();
    let store = DurableStore::in_memory()
        .unwrap()
        .with_time_budget(TimeBudget::new(BASE).with_max_overdue(64));
    let hub = Arc::new(compose_with(
        store,
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
        None,
    ));
    scripts.attach(&hub);
    block_on(hub.issue(
        request(Verb::Sink, "urn:iki:ledger:append", &[("content", "seed")]),
        &Capability::root(),
    ))
    .expect("a seed item");
    for (name, text) in [("slow", slow_query()), ("fast", FAST.to_string())] {
        block_on(hub.issue(
            request(
                Verb::Sink,
                &format!("urn:script:{name}"),
                &[
                    ("language", "sparql"),
                    ("content", text.as_str()),
                    ("public", "true"),
                ],
            ),
            &person(),
        ))
        .unwrap_or_else(|e| panic!("publishing `{name}`: {e}"));
    }
    (hub, scripts)
}

fn refused_at(result: Result<ikigai_core::Representation, Error>) -> String {
    match result {
        Err(error) => error.to_string(),
        Ok(answer) => panic!(
            "the slow script answered: {}",
            String::from_utf8_lossy(&answer.bytes)
        ),
    }
}

/// An anonymous run, as a read and for effects, is refused at the DOOR's budget; a signed-in
/// person's run of the same script gets the store's base.
#[test]
fn an_anonymous_run_is_held_to_the_anonymous_budget() {
    let (hub, _scripts) = hub();
    for (verb, iri) in [
        (Verb::Source, "urn:script:slow:result"),
        (Verb::Sink, "urn:script:slow:runs"),
    ] {
        let started = Instant::now();
        let why = refused_at(block_on(hub.issue(request(verb, iri, &[]), &anonymous())));
        let took = started.elapsed();
        assert!(
            why.contains(&format!("time budget of {DOOR_MS} ms")),
            "{iri}: refused at the DOOR's budget, not the store's: {why} after {took:?}"
        );
        assert!(took < BASE, "{iri}: {took:?}");
    }
    let why = refused_at(block_on(hub.issue(
        request(Verb::Source, "urn:script:slow:result", &[]),
        &person(),
    )));
    assert!(
        why.contains(&format!("time budget of {} ms", BASE.as_millis())),
        "a signed-in run gets the store's base: {why}"
    );
}

/// The budget does not reach the hub's own queries when no script runs: an anonymous caller's
/// ledger read is not stamped (the decision `crate::budget` records), and a fast public script
/// runs for the anonymous caller as before.
#[test]
fn a_fast_public_script_still_runs_for_anyone() {
    let (hub, _scripts) = hub();
    let answer = block_on(hub.issue(
        request(
            Verb::Source,
            "urn:script:fast:result",
            &[("floor", "0"), ("as", "text/csv")],
        ),
        &anonymous(),
    ))
    .expect("a fast public run");
    assert!(
        String::from_utf8_lossy(&answer.bytes).contains("number"),
        "{}",
        String::from_utf8_lossy(&answer.bytes)
    );
}

/// ★ Anonymous runs share one rate ([`ikigai_gonk::script::ANONYMOUS_RUNS`], an
/// `ikigai-throttle` `RateLimit`); past it a run is refused and names the limit. A signed-in
/// caller is not limited by it, and neither is an anonymous READ of a script's source.
#[test]
fn anonymous_runs_are_rate_limited_and_nobody_else_is() {
    let (hub, _scripts) = hub();
    let rate = ikigai_gonk::script::ANONYMOUS_RUNS;
    // A different argument value each time, so no answer comes from the hub's cache: a cached
    // answer costs nothing and is not what the limit is for.
    let run = |who: &Capability, n: u32| {
        block_on(hub.issue(
            request(
                Verb::Source,
                "urn:script:fast:result",
                &[("floor", n.to_string().as_str())],
            ),
            who,
        ))
    };
    let mut refused = None;
    for n in 0..=rate.max {
        match run(&anonymous(), n) {
            Ok(_) => {}
            Err(error) => {
                refused = Some((n, error.to_string()));
                break;
            }
        }
    }
    let (at, why) = refused.expect("an anonymous caller is limited");
    assert!(why.contains("rate-limited"), "{why}");
    assert!(
        at <= rate.max,
        "refused after {at} runs, the rate allows {}",
        rate.max
    );
    // A person is not.
    for n in 0..=rate.max {
        run(&person(), n).expect("a signed-in caller is not limited by the anonymous rate");
    }
    // Reading a public script's source is not a run.
    block_on(hub.issue(request(Verb::Source, "urn:script:fast", &[]), &anonymous()))
        .expect("reading the source is not limited");
}

// ------------------------------------------------------------------ over the real HTTP door

/// The same, end to end: an anonymous `GET /script/slow/result` through the HTTP door is
/// answered at the door's budget, the marker computed by the door itself.
#[test]
fn over_http_an_anonymous_run_is_refused_at_the_door_budget() {
    let (hub, _scripts) = hub();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let passkeys = Arc::new(Passkeys::new(layout, addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
    let door = doors::HttpDoor {
        anonymous: anonymous_http_grants(&["default".to_string()]).unwrap(),
        bind: addr,
        passkeys: Some(passkeys),
        anonymous_sparql_budget_ms: DOOR_MS,
    };
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door.clone()),
            listener,
            doors::edge_config(door),
        ))
    });

    let started = Instant::now();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(
        stream,
        "GET /script/slow/result HTTP/1.1\r\nHost: localhost:{}\r\n\
         Accept: application/sparql-results+json\r\nSec-Fetch-Site: none\r\n\
         Connection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let took = started.elapsed();
    assert!(
        response.contains(&format!("time budget of {DOOR_MS} ms")),
        "refused at the DOOR's budget after {took:?}: {}",
        &response[..response.len().min(1_000)]
    );
    assert!(took < BASE, "{took:?}");
    drop(config);
}
