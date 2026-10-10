//! Ledger [#964](http://localhost:1060/l/default/item/964) (gonk's half, ledger
//! [#979](http://localhost:1060/l/default/item/979)): an ANONYMOUS caller's SPARQL runs under
//! the HTTP door's small time budget, on every route it can send SPARQL by; a signed-in caller
//! gets the store's base; and gonk's own queries are never capped by the door.
//!
//! Driven over real HTTP, the server built as `main` builds it. The slow query carries its own
//! data — a three-way cross product of 100-row `VALUES` tables, a million solutions — so it
//! needs no graph to be slow, and costs the store nothing it could refuse by its algebra bounds
//! (`VALUES` rows are free there), so only the clock can stop it. Written with no whitespace
//! outside its literals, so it also fits `/k`'s command grammar.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Request, Verb};
use ikigai_gonk::budget;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::budget::TimeBudget;
use ikigai_store::DurableStore;

const GRAPH: &str = "urn:iki:ledger:graph:default";

/// The store's base in these servers: smaller than the real 5 s so the signed-in case is quick,
/// and well above every door budget used here.
const BASE: Duration = Duration::from_millis(2_500);

/// A million solutions from 1.8 KB of query, and no graph read at all.
fn cross_product() -> String {
    let table = |var: &str| {
        let values: String = (0..100).map(|n| format!("\"{n}\"")).collect();
        format!("VALUES?{var}{{{values}}}")
    };
    format!("SELECT*{{{}{}{}}}", table("a"), table("b"), table("c"))
}

struct Server {
    addr: SocketAddr,
    layout: quic::Layout,
    hub: Arc<ikigai_core::Kernel>,
    http: Arc<ikigai_core::Kernel>,
    door: doors::HttpDoor,
    _config: tempfile::TempDir,
}

impl Server {
    /// A server whose anonymous SPARQL budget is `door_ms`, over a store whose base is [`BASE`].
    fn start(door_ms: u64) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        // The overdue cap lifted: the cross products below go on running a little after their
        // callers are answered, and on a runner with four cores the store's default cap (one)
        // would refuse the next request for the last one's sake. That cap is the store's
        // behavior, not the door's, and is not what this file tests.
        let store = DurableStore::in_memory()
            .unwrap()
            .with_time_budget(TimeBudget::new(BASE).with_max_overdue(64));
        let hub = Arc::new(compose(store));
        // One item, so the default ledger's graph exists: `urn:sparql:*` refuses an EMPTY
        // default dataset before evaluating anything.
        block_on(
            hub.issue(
                Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
                    .with_arg("content", ArgRef::Inline(b"seed".to_vec())),
                &Capability::root(),
            ),
        )
        .expect("a seed item");
        let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).unwrap(),
            bind: addr,
            passkeys: Some(passkeys),
            anonymous_sparql_budget_ms: door_ms,
        };
        let served = Arc::clone(&http);
        let served_door = door.clone();
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                served,
                doors::http_cap(served_door.clone()),
                listener,
                doors::edge_config(served_door),
            ))
        });
        Server {
            addr,
            layout,
            hub,
            http,
            door,
            _config: config,
        }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Response {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
            self.addr.port()
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
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let response = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        Response {
            status,
            body: body.chars().take(2_000).collect(),
        }
    }

    /// A GET for JSON results, signed in when `cookie` is given; how long it took.
    fn get(&self, path: &str, accept: &str, cookie: Option<&str>) -> (Response, Duration) {
        let mut headers = vec![
            ("Accept", accept.to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
        ];
        if let Some(cookie) = cookie {
            headers.push(("Cookie", format!("{}={cookie}", identity::SESSION_COOKIE)));
        }
        let started = Instant::now();
        let response = self.raw("GET", path, &headers, "");
        (response, started.elapsed())
    }

    fn json(&self, path: &str, body: &str) -> Response {
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

    /// Enrol a passkey under a grant reading the default ledger, sign in, and return the
    /// session token.
    fn sign_in(&self) -> String {
        let authenticator = Authenticator::new();
        let invite = identity::invite(
            &self.layout,
            "reader",
            &grants_for("default", Authority::Delete).unwrap(),
            false,
            30,
            identity::now_seconds(),
        )
        .unwrap();
        let c = self.challenge("register-options");
        let enrolled = self.json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(enrolled.status, 200, "{enrolled:?}");
        let c = self.challenge("login-options");
        let login = self.json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(login.status, 200, "{login:?}");
        let v: serde_json::Value = serde_json::from_str(&login.body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    fn challenge(&self, op: &str) -> String {
        let got = self.json(&format!("/auth/{op}"), "{}");
        assert_eq!(got.status, 200, "{got:?}");
        let v: serde_json::Value = serde_json::from_str(&got.body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }
}

struct Response {
    status: u16,
    body: String,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.body)
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

const JSON: &str = "application/sparql-results+json";

/// Every route an anonymous caller can send its own SPARQL by, as `(name, path, Accept)`.
fn routes(query: &str) -> Vec<(&'static str, String, &'static str)> {
    let q = encode(query);
    vec![
        // The mechanical mapping onto gonk's `urn:sparql:*` face.
        ("/sparql/select", format!("/sparql/select?query={q}"), JSON),
        // The mechanical mapping onto the store's graph-scoped door.
        (
            "/iki/store/graph-select",
            format!("/iki/store/graph-select?graph={}&query={q}", encode(GRAPH)),
            JSON,
        ),
        // The SPARQL page's protocol face (no ledger: `urn:sparql:select`)…
        ("/sparql (protocol)", format!("/sparql?query={q}"), JSON),
        // …naming a ledger (the store's door, one graph)…
        (
            "/sparql?ledger (protocol)",
            format!("/sparql?ledger=default&query={q}"),
            JSON,
        ),
        // …and the editor's results fragment, which runs it on the page's behalf.
        (
            "/sparql/results",
            format!("/sparql/results?query={q}"),
            "text/html",
        ),
        // The browse adapter, which issues any read the caller names.
        (
            "/k",
            format!(
                "/k?c={}",
                encode(&format!("source urn:sparql:select query={query}"))
            ),
            "*/*",
        ),
    ]
}

/// ★ The reproduction. On the tree before this change every route below ran the cross
/// product to the store's BASE (here 2.5 s; 5 s live) and was refused there — or, before
/// store 0.2.8, ran it to completion (the reading room's twin answered 272 MB after 11.2 s).
/// Now each is refused at the door's budget.
#[test]
fn an_anonymous_cross_product_is_stopped_at_the_door_budget_on_every_route() {
    let server = Server::start(400);
    let query = cross_product();
    for (name, path, accept) in routes(&query) {
        let (response, took) = server.get(&path, accept, None);
        assert!(
            response.body.contains("time budget of 400 ms"),
            "{name}: refused at the DOOR's budget, not the store's: {response:?} after {took:?}"
        );
        // The data routes answer within moments of the budget. The editor's fragment renders a
        // page around the refusal, and the FIRST page a server renders pays the stylesheet's
        // cold start (1.34 s of a debug build with a one-row query, 33 ms warm; this refusal
        // answers in 0.43 s warm), so its time says nothing about the store's; the budget in
        // its message does.
        if accept != "text/html" {
            assert!(
                took < BASE,
                "{name}: answered at the door's budget, before the store's {BASE:?}: {took:?}"
            );
        }
    }
}

/// A caller may LOWER its budget and nothing else: `budget=` above the door's is overwritten,
/// one below it is kept, and garbage is overwritten rather than refused or obeyed.
#[test]
fn an_anonymous_caller_may_lower_the_door_budget_and_never_raise_it() {
    let server = Server::start(600);
    let q = encode(&cross_product());
    for (asked, applied) in [("600000", 600), ("0", 600), ("1s", 600), ("150", 150)] {
        let (response, _) = server.get(
            &format!("/sparql/select?budget={asked}&query={q}"),
            JSON,
            None,
        );
        assert!(
            response
                .body
                .contains(&format!("time budget of {applied} ms")),
            "budget={asked} → {applied} ms: {response:?}"
        );
    }
}

/// ★ A by-REFERENCE `budget=` cannot carry an anonymous caller past the door. The HTTP door
/// only ever sends inline values, so this is driven through the door's own kernel, under the
/// capability the door computes for an anonymous request: the stamp overwrites it.
///
/// And the store's own answer to one, which ledger #964 asked about: `ikigai-store` 0.2.8
/// REFUSES a by-reference `budget=` (it is read by value only) — it does not read it as
/// absent and fall to its ceiling the way `ikigai-sparql` does.
#[test]
fn a_by_reference_budget_cannot_bypass_the_door_and_the_store_refuses_one() {
    let server = Server::start(300);
    let anonymous = Capability::scoped(
        grants_for("default", Authority::Write)
            .unwrap()
            .into_iter()
            .chain([budget::anonymous_marker(300)]),
    );
    let by_reference = ArgRef::Reference(Iri::parse("urn:example:an-hour").unwrap());
    let select = |target: &str| {
        Request::new(Verb::Source, Iri::parse(target).unwrap())
            .with_arg("query", ArgRef::Inline(cross_product().into_bytes()))
            .with_arg("graph", ArgRef::Inline(GRAPH.as_bytes().to_vec()))
            .with_arg("budget", by_reference.clone())
    };
    for target in ["urn:iki:store:graph-select", "urn:sparql:select"] {
        match block_on(server.http.issue(select(target), &anonymous)) {
            Err(Error::Timeout(message)) => {
                assert!(
                    message.contains("time budget of 300 ms"),
                    "{target}: {message}"
                )
            }
            other => panic!("{target}: stamped to the door's budget, got {other:?}"),
        }
    }
    match block_on(
        server
            .hub
            .issue(select("urn:iki:store:graph-select"), &Capability::root()),
    ) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "budget");
            assert!(detail.contains("not an inline value"), "{detail}");
        }
        other => panic!("the store refuses a by-reference budget, got {other:?}"),
    }
}

/// A signed-in caller carries no door marker, so its query runs to the store's base — the
/// door's budget is for anonymous callers only.
#[test]
fn a_signed_in_caller_gets_the_stores_base_not_the_door_budget() {
    let server = Server::start(300);
    let token = server.sign_in();
    let (response, took) = server.get(
        &format!("/sparql/select?query={}", encode(&cross_product())),
        JSON,
        Some(&token),
    );
    assert!(
        response
            .body
            .contains(&format!("time budget of {} ms", BASE.as_millis())),
        "{response:?} after {took:?}"
    );
    assert!(took >= BASE, "{took:?}");
}

/// ★ gonk's OWN queries are not capped by the door: with the door's budget at 1 ms — less
/// than any real query takes — an anonymous caller still gets every page and ledger read,
/// because the ledger's queries are sub-requests inside the hub and the stamp applies only to
/// SPARQL a caller sends. The same caller's own trivial query IS refused.
#[test]
fn the_door_budget_never_caps_gonks_own_queries() {
    let server = Server::start(1);
    for n in 0..30 {
        let filed = server.raw(
            "POST",
            "/act",
            &[
                ("Accept", "*/*".to_string()),
                ("Origin", server.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("HX-Request", "true".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
            ],
            &format!("_ledger=default&_action=append&_then=items&content=item+{n}"),
        );
        assert_eq!(filed.status, 200, "{filed:?}");
    }
    for path in ["/", "/l/default", "/l/default/items", "/iki/ledger/next"] {
        let (response, _) = server.get(path, "text/html,*/*;q=0.8", None);
        assert_eq!(response.status, 200, "{path}: {response:?}");
    }
    let (response, _) = server.get(
        &format!("/sparql/select?query={}", encode(&cross_product())),
        JSON,
        None,
    );
    assert!(
        response.body.contains("time budget of 1 ms"),
        "{response:?}"
    );
}

/// The marker is the door's alone: an anonymous request's capability carries it, a signed-in
/// one does not, and a foreign NAVIGATION — whose capability is filtered to an allowlist that
/// would drop it — still carries it.
#[test]
fn the_door_marks_exactly_the_anonymous_capability() {
    let server = Server::start(700);
    let token = server.sign_in();
    let request = |cookie: Option<&str>, site: &str| {
        let mut headers = vec![
            (
                "host".to_string(),
                format!("localhost:{}", server.addr.port()),
            ),
            ("sec-fetch-site".to_string(), site.to_string()),
            ("sec-fetch-mode".to_string(), "navigate".to_string()),
            ("sec-fetch-dest".to_string(), "document".to_string()),
        ];
        if let Some(cookie) = cookie {
            headers.push((
                "cookie".to_string(),
                format!("{}={cookie}", identity::SESSION_COOKIE),
            ));
        }
        ikigai_web::HttpRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            query: Vec::new(),
            headers,
            body: Vec::new(),
            peer: Some("127.0.0.1".parse::<std::net::IpAddr>().unwrap()),
        }
    };
    let now = identity::now_seconds();
    let marker = budget::anonymous_marker(700);
    let anonymous = doors::http_scopes(&server.door, &request(None, "none"), now);
    assert!(anonymous.contains(&marker), "{anonymous:?}");
    let foreign = doors::http_scopes(&server.door, &request(None, "cross-site"), now);
    assert!(foreign.contains(&marker), "{foreign:?}");
    let signed_in = doors::http_scopes(&server.door, &request(Some(&token), "none"), now);
    assert!(
        !signed_in
            .iter()
            .any(|s| s.starts_with(budget::ANONYMOUS_BUDGET_PREFIX)),
        "{signed_in:?}"
    );
    // And no grant may name it: a grants.json carrying the marker stops the server.
    assert!(quic::grant_refusal("forged", &[marker]).is_some());
}
