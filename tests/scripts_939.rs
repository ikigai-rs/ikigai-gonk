//! Ledger [#939](http://localhost:1060/l/default/item/939): gonk mounts `ikigai-script`, and
//! every door reaches it with the principal it minted.
//!
//! Two fixtures, seeded in each test and never into a live store: a SPARQL query over the
//! ledger graph ([`OPEN_BY_LABEL`], open items carrying a label) and a plan that composes two
//! ledger reads ([`TRIAGE`]). The first half of this file runs against the hub in process
//! (fast, and the only place a cache can be read); the second against a REAL gonk — the binary,
//! in a child process, over a scratch config home, data home, store and socket, on a port the
//! child picks — through its HTTP, QUIC and socket doors.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Expiry, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::grants::{
    anonymous_http_grants, grants_for, script_grants, Authority, ScriptAuthority,
};
use ikigai_gonk::script::Scripts;
use ikigai_gonk::{compose_with, doors};
use ikigai_store::DurableStore;

/// The SPARQL fixture: open items carrying a label, lowest number first.
pub const OPEN_BY_LABEL: &str = "\
# Open items carrying a label, lowest number first.
# @param label xsd:string -- the label an item must carry
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>
SELECT ?number ?title
FROM <urn:iki:ledger:graph:default>
WHERE {
  ?item a ledger:Item ;
        ledger:status ledger:open ;
        ledger:label ?label ;
        ledger:number ?number ;
        dcterms:title ?title .
}
ORDER BY ?number
";

/// The plan fixture: two ledger reads joined, the open items carrying a label and what the
/// ledger says to do next.
pub const TRIAGE: &str = "\
# Open items carrying a label, then the ledger's next.
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
<urn:plan:triage> a ik:Process ;
    ik:input <urn:plan:triage:input:label> ;
    ik:step <urn:plan:triage:step:1> , <urn:plan:triage:step:2> ;
    ik:result <urn:plan:triage:fork:1> .
<urn:plan:triage:input:label> ik:inputName \"label\" ;
    ik:summary \"the label an item must carry\" ;
    ik:class xsd:string ;
    ik:required true .
<urn:plan:triage:fork:1> a ik:Fork ; ik:join \"newline\" .
<urn:plan:triage:step:1> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:items> ;
    ik:forkOf <urn:plan:triage:fork:1> ;
    ik:order 1 ;
    ik:argument <urn:plan:triage:step:1:arg:labels> .
<urn:plan:triage:step:1:arg:labels> a ik:Argument ;
    ik:inputName \"labels\" ;
    ik:ref <urn:plan:triage:var:label> .
<urn:plan:triage:step:2> a ik:Step ;
    ik:verb \"Source\" ;
    ik:resolves <urn:iki:ledger:next> ;
    ik:forkOf <urn:plan:triage:fork:1> ;
    ik:order 2 .
";

/// A passkey's principal, as the HTTP door mints it.
const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

// ------------------------------------------------------------------ in process: the hub

fn hub() -> (Arc<Kernel>, Scripts) {
    let scripts = Scripts::in_memory();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        None,
        Some(&scripts),
    ));
    scripts.attach(&hub);
    (hub, scripts)
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(
    kernel: &Kernel,
    capability: &Capability,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
) -> Result<Representation, Error> {
    block_on(kernel.issue(request(verb, iri, args), capability))
}

fn text(repr: &Representation) -> String {
    String::from_utf8_lossy(&repr.bytes).into_owned()
}

/// A person signed in with a passkey whose grant writes the default ledger and publishes the
/// fixtures (a script grant names exactly one script).
fn person() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    for name in ["open-by-label", "triage", "elsewhere"] {
        scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    }
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

/// The anonymous loopback caller of the HTTP door.
fn anonymous() -> Capability {
    Capability::scoped(anonymous_http_grants(&["default".to_string()]).unwrap())
}

/// Three items in the default ledger, two labeled `triage` (one of them closed).
fn seed(hub: &Kernel) {
    let root = Capability::root();
    for (content, label) in [
        ("first triage item", "triage"),
        ("not this one", "other"),
        ("second triage item", "triage"),
    ] {
        issue(
            hub,
            &root,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content), ("labels", label)],
        )
        .expect("append");
    }
}

fn publish(
    hub: &Kernel,
    who: &Capability,
    name: &str,
    language: &str,
    content: &str,
    extra: &[(&str, &str)],
) -> Result<String, Error> {
    let mut args = vec![("language", language), ("content", content)];
    args.extend_from_slice(extra);
    issue(hub, who, Verb::Sink, &format!("urn:script:{name}"), &args).map(|r| text(&r))
}

#[test]
fn a_published_query_runs_as_a_read_and_its_answer_is_cached() {
    let (hub, _scripts) = hub();
    seed(&hub);
    let version = publish(
        &hub,
        &person(),
        "open-by-label",
        "sparql",
        OPEN_BY_LABEL,
        &[("public", "true")],
    )
    .expect("a person publishes the query");
    assert!(
        version.starts_with("urn:script:open-by-label:version:sha256:"),
        "{version}"
    );

    let run = |who: &Capability| {
        let started = Instant::now();
        let answer = issue(
            &hub,
            who,
            Verb::Source,
            "urn:script:open-by-label:result",
            &[("label", "triage"), ("as", "text/csv")],
        )
        .expect("a run as a read");
        (answer, started.elapsed())
    };
    let (first, cold) = run(&anonymous());
    let body = text(&first);
    assert!(body.contains("first triage item"), "{body}");
    assert!(body.contains("second triage item"), "{body}");
    assert!(!body.contains("not this one"), "{body}");
    assert_ne!(
        first.expiry,
        Expiry::Always,
        "a query's result is cacheable as the store's answer is: {:?}",
        first.expiry
    );
    let (second, warm) = run(&anonymous());
    assert_eq!(text(&second), body);
    println!("open-by-label:result, anonymous: cold {cold:?}, warm {warm:?}");
    let cached: Vec<String> = text(
        &issue(
            &hub,
            &Capability::root(),
            Verb::Source,
            "urn:kernel:cache",
            &[],
        )
        .unwrap(),
    )
    .lines()
    .map(str::to_string)
    .collect();
    assert!(
        cached.iter().any(|line| line
            .trim_start()
            .starts_with("urn:script:open-by-label:result ")),
        "the run's answer is in the hub's cache: {cached:#?}"
    );
    // ★ And a write to the ledger recomputes it: the answer hangs from the store's threads.
    issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "third triage item"), ("labels", "triage")],
    )
    .unwrap();
    let (third, _) = run(&anonymous());
    assert!(
        text(&third).contains("third triage item"),
        "{}",
        text(&third)
    );
}

#[test]
fn a_published_plan_composes_two_ledger_reads() {
    let (hub, _scripts) = hub();
    seed(&hub);
    publish(
        &hub,
        &person(),
        "triage",
        "plan",
        TRIAGE,
        &[("public", "true")],
    )
    .expect("a person publishes the plan");
    let answer = issue(
        &hub,
        &anonymous(),
        Verb::Source,
        "urn:script:triage:result",
        &[("label", "triage")],
    )
    .expect("the plan runs as a read");
    let body = text(&answer);
    // The first branch is the labeled items, the second the ledger's ranking of every open
    // item: joined in order, one after the other.
    let (items, next) = body
        .split_once("2 item(s)")
        .expect("the items branch first");
    assert!(
        items.contains("first triage item") && items.contains("second triage item"),
        "{body}"
    );
    assert!(!items.contains("not this one"), "{body}");
    assert!(
        next.contains("not this one"),
        "next ranks every open item: {body}"
    );
    println!("triage:result expiry {:?}\n{body}", answer.expiry);
}

#[test]
fn a_publisher_cannot_name_a_graph_it_could_not_read() {
    let (hub, _scripts) = hub();
    let elsewhere = OPEN_BY_LABEL.replace("graph:default", "graph:acme");
    match publish(&hub, &person(), "elsewhere", "sparql", &elsewhere, &[]) {
        Err(Error::Denied(why)) => assert!(
            why.contains("urn:cap:store:read:graph:urn:iki:ledger:graph:acme"),
            "{why}"
        ),
        other => panic!("no elevation: {other:?}"),
    }
}

#[test]
fn a_script_that_writes_or_is_lisp_is_refused_at_publish() {
    let (hub, _scripts) = hub();
    let update = "INSERT DATA { GRAPH <urn:iki:ledger:graph:default> { <urn:a> <urn:b> <urn:c> } }";
    for (language, content, says) in [
        ("sparql", update, "READ-ONLY"),
        (
            "plan",
            &TRIAGE.replace(
                "\"Source\" ;\n    ik:resolves <urn:iki:ledger:next>",
                "\"Sink\" ;\n    ik:resolves <urn:iki:ledger:comment>",
            ) as &str,
            "READ-ONLY",
        ),
        ("lisp", "(+ 1 2)", "no Lisp"),
    ] {
        match publish(&hub, &Capability::root(), "w", language, content, &[]) {
            Err(Error::Denied(why)) | Err(Error::InvalidArgument { detail: why, .. }) => {
                assert!(why.contains(says), "{language}: {why}")
            }
            other => panic!("{language}: {other:?}"),
        }
    }
    // `urn:script:eval` is the library's ad hoc door for Lisp, and nothing here evaluates Lisp.
    match issue(
        &hub,
        &Capability::root(),
        Verb::Sink,
        "urn:script:eval",
        &[("content", "(+ 1 2)")],
    ) {
        Err(Error::Unresolved(target)) => assert_eq!(target.as_str(), "urn:lisp:eval"),
        other => panic!("eval of Lisp: {other:?}"),
    }
}

#[test]
fn a_plan_that_writes_runs_only_as_root() {
    let (hub, _scripts) = hub();
    seed(&hub);
    let writes = "\
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:plan:w> a ik:Process ; ik:step <urn:plan:w:step:1> ; ik:result <urn:plan:w:step:1> .
<urn:plan:w:step:1> a ik:Step ; ik:verb \"Sink\" ; ik:resolves <urn:iki:store:graph-update> ;
    ik:argument <urn:plan:w:step:1:arg:graph> , <urn:plan:w:step:1:arg:update> .
<urn:plan:w:step:1:arg:graph> a ik:Argument ; ik:inputName \"graph\" ;
    ik:value \"urn:iki:ledger:graph:default\" .
<urn:plan:w:step:1:arg:update> a ik:Argument ; ik:inputName \"update\" ;
    ik:value \"INSERT DATA { <urn:forged> <urn:p> <urn:o> }\" .
";
    // The anonymous caller holds the ledger graph's store write token (the ledger's own
    // writes need it), so without the rule this plan is a raw write past the door's rule.
    match issue(
        &hub,
        &anonymous(),
        Verb::Source,
        "urn:plan:eval",
        &[("in", writes)],
    ) {
        Err(Error::Denied(why)) => assert!(why.contains("`Sink` or `Delete` step"), "{why}"),
        other => panic!("a writing plan below root: {other:?}"),
    }
    // A read-only plan runs for anyone, as themselves.
    let reads = issue(
        &hub,
        &anonymous(),
        Verb::Source,
        "urn:plan:eval",
        &[("in", TRIAGE), ("label", "triage")],
    )
    .expect("a read-only plan runs below root");
    assert!(text(&reads).contains("first triage item"));
}

/// Exactly what the script family adds to the hub's catalog: the library's eight, the plan
/// doors' three and the validator they compose. A dependency bump that widens it is a red test,
/// as `tests/conformance.rs` pins the rest.
const FAMILY: [&str; 12] = [
    "urn:plan:eval",
    "urn:plan:requires",
    "urn:plan:validate",
    "urn:script:catalog",
    "urn:script:eval",
    "urn:script:{name}",
    "urn:script:{name}:compiled",
    "urn:script:{name}:result",
    "urn:script:{name}:run:{id}",
    "urn:script:{name}:runs",
    "urn:script:{name}:version:{digest}",
    "urn:shacl:validate",
];

fn patterns(kernel: &Kernel) -> BTreeSet<String> {
    kernel
        .entries()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.pattern)
        .collect()
}

#[test]
fn the_family_is_in_the_hub_and_the_validator_is_behind_no_door() {
    let (hub, _scripts) = hub();
    let without = Arc::new(ikigai_gonk::compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ));
    let added: BTreeSet<String> = patterns(&hub)
        .difference(&patterns(&without))
        .cloned()
        .collect();
    let family: BTreeSet<String> = FAMILY.iter().map(|s| s.to_string()).collect();
    assert_eq!(added, family, "the script family, exactly");

    // Every door serves the family but the validator, which it does not even list.
    let door = doors::door_kernel(Arc::clone(&hub));
    let at_door = patterns(&door);
    for pattern in FAMILY {
        assert_eq!(
            at_door.contains(pattern),
            pattern != "urn:shacl:validate",
            "{pattern} at a door"
        );
    }
    match issue(
        &door,
        &Capability::root(),
        Verb::Source,
        "urn:shacl:validate",
        &[("data", ""), ("shapes", "")],
    ) {
        Err(Error::Unresolved(_)) => {}
        other => panic!("the validator is not served, even to root at the socket: {other:?}"),
    }
}

// ------------------------------------------------------------- a real gonk: every door

mod common;
mod scratch_gonk;

use std::io::{Read, Write};
use std::net::TcpStream;

use common::Authenticator;
use ikigai_gonk::{identity, quic};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

impl Gonk {
    fn origin(&self) -> String {
        format!("http://localhost:{}", self.http.port())
    }

    /// One HTTP exchange: `(status, body)`.
    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(self.http).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.http.port()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    /// The response head of an anonymous GET.
    fn head(&self, path: &str) -> String {
        let mut stream = TcpStream::connect(self.http).expect("connect");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
            self.http.port()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let response = String::from_utf8_lossy(&raw).into_owned();
        response
            .split_once("\r\n\r\n")
            .map_or(response.clone(), |(head, _)| head.to_string())
    }

    fn json(&self, path: &str, body: &str) -> (u16, String) {
        self.raw(
            "POST",
            path,
            &[
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", self.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    /// Enroll a passkey under `scopes`, sign in, and return the session token.
    fn sign_in(&self, scopes: &[String]) -> String {
        let authenticator = Authenticator::new();
        let invite = identity::invite(
            &self.layout,
            "person",
            scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let challenge = |op: &str| {
            let (status, body) = self.json(&format!("/auth/{op}"), "{}");
            assert_eq!(status, 200, "{body}");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            v["challenge"].as_str().unwrap().to_string()
        };
        let c = challenge("register-options");
        let (status, body) = self.json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(status, 200, "{body}");
        let c = challenge("login-options");
        let (status, body) = self.json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    /// The mechanical route over HTTP: `method path`, `body` as the content, signed in when
    /// `session` is given.
    fn http(&self, method: &str, path: &str, session: Option<&str>, body: &str) -> (u16, String) {
        let mut headers = vec![("Content-Type", "text/plain".to_string())];
        if let Some(token) = session {
            headers.push(("Cookie", format!("{}={token}", identity::SESSION_COOKIE)));
        }
        self.raw(method, path, &headers, body)
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_quic(&self, request: Request) -> Result<Representation, Error> {
        let client = self
            .dial_quic()
            .unwrap_or_else(|e| panic!("QUIC connect: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }

    /// Enroll this test's QUIC client `alpha` under exactly `scopes`; `grants.json` is read per
    /// connection, so the next dial runs under them.
    fn regrant_alpha(&self, scopes: &[String]) {
        quic::enrol(
            &self.layout,
            "alpha",
            &self.client.fingerprint,
            scopes,
            true,
        )
        .expect("re-enroll alpha");
    }

    /// A run record, as its JSON, read over the socket.
    fn run_record(&self, run: &str) -> serde_json::Value {
        let read = self
            .over_socket(request(Verb::Source, run, &[("as", "application/json")]))
            .unwrap_or_else(|e| panic!("{run}: {e}"));
        serde_json::from_slice(&read.bytes).unwrap()
    }
}

/// The run IRI a `…:runs` answer names (its first `urn:script:…:run:N`).
fn run_iri(answer: &str) -> String {
    answer
        .split_whitespace()
        .find(|word| word.starts_with("urn:script:") && word.contains(":run:"))
        .unwrap_or_else(|| panic!("a run IRI in {answer:?}"))
        .trim_end_matches(['.', ','])
        .to_string()
}

#[test]
fn every_door_reaches_the_scripts_and_each_run_names_its_principal() {
    let gonk = Gonk::with_quic("");
    for (content, label) in [
        ("first triage item", "triage"),
        ("not this one", "other"),
        ("second triage item", "triage"),
    ] {
        gonk.over_socket(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content), ("labels", label)],
        ))
        .expect("append over the socket");
    }

    // ── HTTP, signed in: a PERSON publishes. The grant writes the default ledger and may
    // publish three named scripts; `elsewhere` names a graph the grant cannot read.
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    for name in ["open-by-label", "triage", "elsewhere"] {
        scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    }
    scopes.extend(script_grants("drafted", ScriptAuthority::Read).unwrap());
    let session = gonk.sign_in(&scopes);
    let (status, body) = gonk.http(
        "POST",
        "/script/open-by-label?language=sparql&public=true",
        Some(&session),
        OPEN_BY_LABEL,
    );
    assert_eq!(status, 200, "a person publishes the query: {body}");
    assert!(
        body.contains("urn:script:open-by-label:version:sha256:"),
        "{body}"
    );
    let (status, body) = gonk.http(
        "POST",
        "/script/triage?language=plan&public=true",
        Some(&session),
        TRIAGE,
    );
    assert_eq!(status, 200, "a person publishes the plan: {body}");

    // ── No elevation: a graph the publisher could not read is refused AT PUBLISH.
    let elsewhere = OPEN_BY_LABEL.replace("graph:default", "graph:acme");
    let (status, body) = gonk.http(
        "POST",
        "/script/elsewhere?language=sparql",
        Some(&session),
        &elsewhere,
    );
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("urn:cap:store:read:graph:urn:iki:ledger:graph:acme"),
        "{body}"
    );

    // ── HTTP, anonymous: a PUBLIC query and a public plan, run as reads.
    let (status, body) = gonk.http(
        "GET",
        "/script/open-by-label/result?label=triage&as=text%2Fcsv",
        None,
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("first triage item") && body.contains("second triage item"),
        "{body}"
    );
    assert!(!body.contains("not this one"), "{body}");
    let (status, body) = gonk.http("GET", "/script/triage/result?label=triage", None, "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("2 item(s)"), "{body}");
    // ★ Both answers are CACHEABLE, which the HTTP door says in its `Cache-Control` (`no-store`
    // for an answer the hub may not keep): the hub every door shares keeps them, so the next
    // anonymous read of either is a hit until a ledger write or a republish cuts it. (The hub's
    // `urn:kernel:cache` cannot be read through a door: a door kernel answers its own, empty.)
    for path in [
        "/script/open-by-label/result?label=triage",
        "/script/triage/result?label=triage",
    ] {
        let head = gonk.head(path);
        assert!(
            head.contains("Cache-Control: ") && !head.contains("no-store"),
            "{path} is cacheable: {head}"
        );
    }

    // The same read twice through the door: the second is the hub's cache.
    let mut took = Vec::new();
    for _ in 0..2 {
        let started = Instant::now();
        let (status, _) = gonk.http("GET", "/script/open-by-label/result?label=other", None, "");
        assert_eq!(status, 200);
        took.push(started.elapsed());
    }
    println!(
        "open-by-label:result through the HTTP door: cold {:?}, warm {:?}",
        took[0], took[1]
    );

    // ── HTTP, signed in: a run for EFFECTS, recorded as the passkey. `ikigai-web` stamps a
    // `principal` argument on this write, which the script would refuse as no parameter of
    // its own — the door removes it, and the record names the passkey from the capability.
    let (status, body) = gonk.http(
        "POST",
        "/script/open-by-label/runs?label=triage",
        Some(&session),
        "",
    );
    assert_eq!(status, 200, "{body}");
    let person_run = run_iri(&body);
    let record = gonk.run_record(&person_run);
    let principal = record["principal"].as_str().unwrap_or_default().to_string();
    assert!(
        principal.starts_with(identity::PASSKEY_IRI_PREFIX),
        "a person's run names the passkey: {record}"
    );

    // ── QUIC, a machine WITHOUT a script grant: refused, though the script is public — a
    // certificate holds what its grant says, and `alpha` holds the default ledger only.
    let runs = |label: &str| {
        request(
            Verb::Sink,
            "urn:script:open-by-label:runs",
            &[("label", label)],
        )
    };
    match gonk.over_quic(runs("triage")) {
        Err(Error::Denied(why)) => assert!(why.contains("urn:cap:script:run"), "{why}"),
        other => panic!("a runner without the grant: {other:?}"),
    }

    // ── QUIC, the same machine granted the run: recorded as the CLIENT.
    let mut machine = grants_for("default", Authority::Read).unwrap();
    machine.extend(script_grants("open-by-label", ScriptAuthority::Run).unwrap());
    machine.extend(script_grants("drafted", ScriptAuthority::Publish).unwrap());
    gonk.regrant_alpha(&machine);
    let answer = gonk.over_quic(runs("triage")).expect("a machine runs it");
    let machine_run = run_iri(&text(&answer));
    let record = gonk.run_record(&machine_run);
    assert_eq!(
        record["principal"].as_str(),
        Some(quic::client_iri(&gonk.client.fingerprint).as_str()),
        "a machine's run names its certificate: {record}"
    );
    assert_ne!(machine_run, person_run);

    // ── QUIC: a machine DRAFTS and does not publish.
    let draft = |state: Option<&str>| {
        let mut args = vec![("language", "sparql"), ("content", OPEN_BY_LABEL)];
        if let Some(state) = state {
            args.push(("state", state));
        }
        request(Verb::Sink, "urn:script:drafted", &args)
    };
    match gonk.over_quic(draft(None)) {
        Err(Error::Denied(why)) => assert!(why.contains("a machine drafts"), "{why}"),
        other => panic!("a machine publishing: {other:?}"),
    }
    gonk.over_quic(draft(Some("draft")))
        .expect("a machine saves a draft");
    // Its author sees it; the person, who may read the name, does not; root does.
    gonk.over_quic(request(Verb::Source, "urn:script:drafted", &[]))
        .expect("the author reads the draft");
    let (status, body) = gonk.http("GET", "/script/drafted", Some(&session), "");
    assert_eq!(status, 404, "a draft is its author's: {body}");
    gonk.over_socket(request(Verb::Source, "urn:script:drafted", &[]))
        .expect("root sees every draft");
    match gonk.over_quic(request(Verb::Delete, "urn:script:drafted", &[])) {
        Err(Error::Denied(why)) => assert!(why.contains("may not retire"), "{why}"),
        other => panic!("a machine retiring: {other:?}"),
    }

    // ── The socket: root runs anything, and is recorded as nobody in particular.
    let answer = gonk
        .over_socket(runs("triage"))
        .expect("root runs it over the socket");
    let record = gonk.run_record(&run_iri(&text(&answer)));
    assert_eq!(
        record["principal"].as_str(),
        Some(ikigai_script::authority::UNSTAMPED),
        "{record}"
    );

    // ── And no door serves the validator.
    match gonk.over_socket(request(
        Verb::Source,
        "urn:shacl:validate",
        &[("data", ""), ("shapes", "")],
    )) {
        Err(Error::Unresolved(_)) => {}
        other => panic!("the validator at the socket: {other:?}"),
    }
    let (status, _) = gonk.http("GET", "/shacl/validate?data=&shapes=", None, "");
    assert_eq!(status, 404);
}
