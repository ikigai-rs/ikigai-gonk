//! Ledger [#1210](http://localhost:1060/l/default/item/1210): an ANONYMOUS ad hoc plan at
//! `urn:plan:eval`, and the dry run of `urn:nl:sparql:check`, are runs: held to the HTTP door's
//! anonymous SPARQL budget and counted against the anonymous run rate, the window a script's run
//! is counted in (ledger [#1175](http://localhost:1060/l/default/item/1175)).
//!
//! ★ The reproduction. Both keep the caller's capability, the door's marker
//! (`urn:iki:gonk:door:anonymous-budget:<ms>`) included, but both evaluate SPARQL in a
//! SUB-request: a plan's step, the check's dry run. A sub-request never passes the hub's front
//! door, where the marker is read and the budget stamped, so before this change both ran a
//! caller's own query at the store's BASE (here 2.5 s; 5 s live), as often as they were sent.
//! Measured through a scratch gonk's HTTP door on main (2c52b64): 5.02 s through a plan against
//! 1.01 s for the same query sent straight to the store, and 45 of 45 plans passed in 0.9 s;
//! the check, 5.05 s and 35 of 35.
//!
//! The slow query carries its own data, `tests/slow/mod.rs`'s product of `VALUES` tables each
//! row of which grows a million-character string, so no size bound can stop it before the clock
//! (ledger [#1730](http://localhost:1062/l/default/item/1730): the `SHA512` cross product it
//! replaces finished inside the store's base in a release build). The check's variant keeps NO
//! row, so the check's own `LIMIT` cannot stop it early: a `LIMIT` bounds rows, not work.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Error, FailureClass, Iri, Kernel, Representation, Request, Verb,
};
use ikigai_gonk::budget::{self, AnonymousRuns};
use ikigai_gonk::grants::{
    anonymous_http_grants, grants_for, script_grants, Authority, ScriptAuthority,
};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::nl::Nl;
use ikigai_gonk::script::Scripts;
use ikigai_gonk::{compose_with, doors, quic, web};
use ikigai_store::budget::TimeBudget;
use ikigai_store::DurableStore;

mod scratch_gonk;
mod slow;

/// The store's base in these servers: smaller than the real 5 s so a test is quick, and well
/// above the door budget.
const BASE: Duration = Duration::from_millis(2_500);

/// The anonymous door's budget in these servers.
const DOOR_MS: u64 = 400;

/// A person's principal, as the HTTP door mints one.
const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

/// A query that answers at once; `n` makes each text distinct, so no answer is a cache hit.
fn fast(n: u32) -> String {
    format!("SELECT*WHERE{{?s?p?o.FILTER(?o!={n})}}")
}

/// `query` reading the default ledger by its own `FROM`, as a draft names its dataset for
/// `urn:nl:sparql:check`.
fn from_ledger(query: &str) -> String {
    query.replacen("SELECT*", "SELECT*FROM<urn:iki:ledger:graph:default>", 1)
}

/// A read-only plan whose every step sends `query` to the store's graph-scoped select: one
/// step, or (`forked`) two in a fork, joined.
fn plan(query: &str, forked: bool) -> String {
    let step = |n: u32, fork: &str| {
        format!(
            "<urn:plan:x:step:{n}> a ik:Step ; ik:verb \"Source\" ;\n\
             \x20   ik:resolves <urn:iki:store:graph-select> ;{fork}\n\
             \x20   ik:argument <urn:plan:x:step:{n}:graph> , <urn:plan:x:step:{n}:query> .\n\
             <urn:plan:x:step:{n}:graph> a ik:Argument ; ik:inputName \"graph\" ;\n\
             \x20   ik:value \"urn:iki:ledger:graph:default\" .\n\
             <urn:plan:x:step:{n}:query> a ik:Argument ; ik:inputName \"query\" ;\n\
             \x20   ik:value \"\"\"{query}\"\"\" .\n"
        )
    };
    let head = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n";
    if forked {
        format!(
            "{head}<urn:plan:x> a ik:Process ;\n\
             \x20   ik:step <urn:plan:x:step:1> , <urn:plan:x:step:2> ;\n\
             \x20   ik:result <urn:plan:x:fork> .\n\
             <urn:plan:x:fork> a ik:Fork ; ik:join \"newline\" .\n{}{}",
            step(1, " ik:forkOf <urn:plan:x:fork> ; ik:order 1 ;"),
            step(2, " ik:forkOf <urn:plan:x:fork> ; ik:order 2 ;"),
        )
    } else {
        format!(
            "{head}<urn:plan:x> a ik:Process ; ik:step <urn:plan:x:step:1> ;\n\
             \x20   ik:result <urn:plan:x:step:1> .\n{}",
            step(1, "")
        )
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(answer: &Representation) -> String {
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// A person who may publish the `fast` fixture.
fn person() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    scopes.extend(script_grants("fast", ScriptAuthority::Publish).unwrap());
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

/// The anonymous loopback caller, as the HTTP door computes its capability: the grants and
/// the door's budget marker.
fn anonymous() -> Capability {
    let mut scopes = anonymous_http_grants(&["default".to_string()]).unwrap();
    scopes.push(budget::anonymous_marker(DOOR_MS));
    Capability::scoped(scopes)
}

/// The hub as `main` composes it, scripts and natural language sharing `runs`, over a store
/// whose base is [`BASE`], with one item filed and a fast script published PUBLIC.
fn hub(runs: AnonymousRuns) -> Arc<Kernel> {
    let scripts = Scripts::in_memory_with(runs.clone());
    let nl = Nl::new(ikigai_gonk::nl::DEFAULT_ASK, None, runs);
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
        Some(&nl),
    ));
    scripts.attach(&hub);
    block_on(hub.issue(
        request(Verb::Sink, "urn:iki:ledger:append", &[("content", "seed")]),
        &Capability::root(),
    ))
    .expect("a seed item");
    let script = "\
# Every item's number from a floor up.
# @param floor xsd:integer -- the lowest number to list
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
SELECT ?number
FROM <urn:iki:ledger:graph:default>
WHERE { ?item ledger:number ?number . FILTER(?number >= ?floor) }
";
    block_on(hub.issue(
        request(
            Verb::Sink,
            "urn:script:fast",
            &[
                ("language", "sparql"),
                ("content", script),
                ("public", "true"),
            ],
        ),
        &person(),
    ))
    .expect("publishing `fast`");
    hub
}

fn eval(hub: &Kernel, plan: &str, who: &Capability) -> ikigai_core::Result<Representation> {
    block_on(hub.issue(request(Verb::Source, "urn:plan:eval", &[("in", plan)]), who))
}

fn check(hub: &Kernel, query: &str, who: &Capability) -> ikigai_core::Result<Representation> {
    block_on(hub.issue(
        request(Verb::Source, "urn:nl:sparql:check", &[("content", query)]),
        who,
    ))
}

/// An anonymous plan's step is held to the DOOR's budget, a forked step included (the budget
/// rides the task, and the hub runs a fork's branches on it); a signed-in person's plan gets
/// the store's base.
#[test]
fn an_anonymous_plan_is_held_to_the_anonymous_budget() {
    let hub = hub(AnonymousRuns::default());
    for forked in [false, true] {
        let started = Instant::now();
        let why = eval(&hub, &plan(&slow::rows(), forked), &anonymous())
            .map(|answer| text(&answer))
            .expect_err("the slow plan answered");
        let took = started.elapsed();
        assert!(
            why.to_string()
                .contains(&format!("time budget of {DOOR_MS} ms")),
            "forked={forked}: refused at the DOOR's budget, not the store's: {why} after {took:?}"
        );
        assert!(took < BASE, "forked={forked}: {took:?}");
    }
    let why = eval(&hub, &plan(&slow::rows(), false), &person())
        .map(|answer| text(&answer))
        .expect_err("the slow plan answered a person");
    assert!(
        why.to_string()
            .contains(&format!("time budget of {} ms", BASE.as_millis())),
        "a signed-in plan gets the store's base: {why}"
    );
}

/// The check's dry run, for an anonymous caller, is held to the door's budget. The check
/// reports a failed dry run as a VALUE, so the answer is a success that names the budget.
#[test]
fn an_anonymous_draft_check_dry_run_is_held_to_the_anonymous_budget() {
    let hub = hub(AnonymousRuns::default());
    let started = Instant::now();
    let answer =
        text(&check(&hub, &from_ledger(&slow::no_rows()), &anonymous()).expect("a check answers"));
    let took = started.elapsed();
    assert!(
        answer.contains(&format!("time budget of {DOOR_MS} ms")),
        "the dry run stopped at the DOOR's budget after {took:?}: {answer}"
    );
    assert!(took < BASE, "{took:?}");
    let answer =
        text(&check(&hub, &from_ledger(&slow::no_rows()), &person()).expect("a check answers"));
    assert!(
        answer.contains(&format!("time budget of {} ms", BASE.as_millis())),
        "a signed-in check gets the store's base: {answer}"
    );
}

/// ★ ONE window: a script's run, a plan and a check count against the same anonymous rate, and
/// past it each is refused naming it. Validating a plan or deriving what it needs runs none of
/// its steps and is not counted; a signed-in caller is not limited.
#[test]
fn scripts_plans_and_checks_share_one_anonymous_window() {
    let hub = hub(AnonymousRuns::per_minute(3));
    for _ in 0..3 {
        for door in ["urn:plan:validate", "urn:plan:requires"] {
            block_on(hub.issue(
                request(Verb::Source, door, &[("in", &plan(&fast(0), false))]),
                &anonymous(),
            ))
            .unwrap_or_else(|e| panic!("{door} is not a run: {e}"));
        }
    }
    block_on(hub.issue(
        request(Verb::Source, "urn:script:fast:result", &[("floor", "0")]),
        &anonymous(),
    ))
    .expect("run 1 of 3: a script");
    eval(&hub, &plan(&fast(1), false), &anonymous()).expect("run 2 of 3: a plan");
    check(&hub, &from_ledger(&fast(2)), &anonymous()).expect("run 3 of 3: a check");
    for refused in [
        eval(&hub, &plan(&fast(3), false), &anonymous()),
        check(&hub, &from_ledger(&fast(4)), &anonymous()),
        block_on(hub.issue(
            request(Verb::Source, "urn:script:fast:result", &[("floor", "5")]),
            &anonymous(),
        )),
    ] {
        let why = refused
            .map(|answer| text(&answer))
            .expect_err("over the rate");
        // ★ Typed since ikigai-throttle 0.2.0 (ledger #1546): a `RetryAfter` carrying the delay,
        // class `retry-after`, so a door can say when; `is_transient()` does NOT cover it, and
        // nothing above the window re-issues the run on its own.
        assert!(
            matches!(why, Error::RetryAfter { delay: Some(_), .. }),
            "{why:?}"
        );
        assert_eq!(why.class(), FailureClass::RetryAfter);
        assert!(!why.is_transient(), "{why:?}");
        assert!(
            why.to_string().starts_with("retry after ")
                && why.to_string().contains("rate-limited")
                && why.to_string().contains(budget::ANONYMOUS_RUNS_IRI)
                && why.to_string().contains("capped at 3 per 60s"),
            "{why}"
        );
    }
    for n in 10..20 {
        eval(&hub, &plan(&fast(n), false), &person())
            .expect("a signed-in plan is not limited by the anonymous rate");
    }
}

// ------------------------------------------------------------------ over the real HTTP door

fn get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nAccept: */*\r\n\
         Sec-Fetch-Site: none\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", common_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode everything but the unreserved characters.
fn common_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// End to end through the HTTP door in process, the marker computed by the door itself: an
/// anonymous `GET /plan/eval` and `GET /k?c=source urn:nl:sparql:check content=…` stop at the
/// door's budget.
#[test]
fn over_http_an_anonymous_plan_and_check_stop_at_the_door_budget() {
    let hub = hub(AnonymousRuns::default());
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
        browse_roots: Vec::new(),
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

    let plan = plan(&slow::rows(), false);
    let command = format!(
        "source urn:nl:sparql:check content={}",
        from_ledger(&slow::no_rows())
    );
    for path in [
        format!("/plan/eval?{}", query(&[("in", &plan)])),
        format!("/k?{}", query(&[("c", &command)])),
    ] {
        let started = Instant::now();
        let response = get(addr, &path);
        let took = started.elapsed();
        assert!(
            response.contains(&format!("time budget of {DOOR_MS} ms")),
            "{}: refused at the DOOR's budget after {took:?}: {}",
            &path[..path.len().min(40)],
            &response[..response.len().min(1_000)]
        );
        assert!(took < BASE, "{took:?}");
    }
    drop(config);
}

/// `gonk.script.anonymous_runs_per_minute` sets the window, through a REAL gonk (the binary, in
/// a child process over scratch homes, on a port it picks): at 2 a minute, the third anonymous
/// plan is refused, and the banner says the rate it serves.
///
/// ★ And the refusal is a **`429 Too Many Requests` with `Retry-After`** (ledger
/// [#1216](http://localhost:1062/l/default/item/1216) part 1,
/// [#1273](http://localhost:1062/l/default/item/1273)). Through ikigai-throttle 0.1.11 the window
/// refused with a permanent `Error::Endpoint("rate-limited …")`, which `ikigai-web` could only
/// answer `500 Internal Server Error`, with no header saying when to come back. Since throttle
/// 0.2.0 the refusal is `Error::RetryAfter { delay, .. }` (core 0.1.108's class `retry-after`),
/// and `ikigai-web` 0.1.66 answers that `429` with `Retry-After` in whole seconds, rounded up and
/// never 0; the body is the error's text, which names the window and the cap. This is the FLOOR
/// TEST for both pins: on the 0.1.62 door the third answer is a `500` with no `Retry-After`.
#[test]
fn the_anonymous_run_rate_is_configurable() {
    let gonk = scratch_gonk::Gonk::start("gonk.script.anonymous_runs_per_minute = 2\n");
    assert!(
        gonk.log().contains("read-only plans, 2 a minute in all"),
        "{}",
        gonk.log()
    );
    let answers: Vec<String> = (0..3)
        .map(|n| {
            get(
                gonk.http,
                &format!("/plan/eval?{}", query(&[("in", &plan(&fast(n), false))])),
            )
        })
        .collect();
    for answer in &answers[..2] {
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }
    let refused = &answers[2];
    assert!(
        refused.starts_with("HTTP/1.1 429 Too Many Requests\r\n"),
        "a rate-limited run is a 429, not a 500: {refused}"
    );
    let (head, body) = refused.split_once("\r\n\r\n").expect("a head and a body");
    let retry_after: u64 = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("Retry-After")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_else(|| panic!("a 429 carries Retry-After: {refused}"))
        .parse()
        .expect("Retry-After is delay-seconds, a whole number");
    // The window is a minute and the third run came within moments of the first, so the wait
    // is most of it; rounded UP and never 0, so a client told the number never retries at once.
    assert!(
        (1..=60).contains(&retry_after),
        "Retry-After: {retry_after} s: {refused}"
    );
    assert!(
        body.contains("rate-limited")
            && body.contains(budget::ANONYMOUS_RUNS_IRI)
            && body.contains("capped at 2 per 60s"),
        "{refused}"
    );
}
