//! Ledger [#1932](http://localhost:1062/l/default/item/1932): a review click whose model call is
//! REFUSED or RATE-LIMITED comes back to the browser as ONE answer with its class, and spends
//! no more regions.
//!
//! `ikigai-llm` 0.16 types a backend's answers by `ikigai-http`'s status table: an origin 401/403
//! is `Error::Refused`, a 429 (or a 503 with `Retry-After`) is `Error::RetryAfter` with the delay.
//! gonk's own model-call window (`urn:iki:gonk:mount:calls`, `ikigai-throttle` 0.2.0) refuses with
//! `RetryAfter` too. Through `ikigai-browse` 0.18.x the review pass stopped only at a `Denied`:
//! each of these was collapsed region by region, so a 429 was spent on every remaining region and
//! the delay was lost in a pass whose coverage line said only "incomplete". From 0.19.0 the pass
//! stops at the first `Denied`, `Refused` or `RetryAfter` and returns the error with its type, so
//! this door (`ikigai-web` 0.1.66) answers `429 Too Many Requests` with `Retry-After` for a rate
//! limit and `502 Bad Gateway` for a refused credential, and the access log names the class.
//!
//! These are the FLOOR TESTS for the `"0.19.0"` pin: on browse 0.18.9 a backend 429 or 401 was
//! asked once per region (three here), and gonk's own window refusing mid-pass answered `200`.
//!
//! The model is a STUB `urn:llm` peer, a kernel served over `ikigai_ipc` on a short socket under
//! `/tmp`, mounted exactly as `gonk.mount` mounts the real one, so the failure crosses the wire
//! (wire v9 carries `RetryAfter`'s delay and `Refused`) as it does in production. The click is
//! browse's own Review button: `GET /k?c=source urn:repo:{root}:review:{path} as=text/html`,
//! same-origin, under a signed-in `--browse derive` session. Nothing here touches a live gonk:
//! every server binds `127.0.0.1:0` over a tempdir config home.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;
mod ready;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Kernel, ReprType,
    Representation, Verb,
};
use ikigai_gonk::access::{AccessLog, Door};
use ikigai_gonk::budget::{self, Window};
use ikigai_gonk::config::{ExplainTiers, QueuePolicy};
use ikigai_gonk::grants::{browse_role_grants, grants_for, Authority, BrowseRole};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{browse, compose_with, doors, mount, quic, web};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

const ROOT: &str = "demo";
/// The file the click reviews: big enough for THREE regions at browse's default 16 KiB a model
/// call, so a pass that grinds on after a failure asks three times and one that stops asks once.
const FILE: &str = "big.rs";
/// The delay the stub's 429 carries, and so the `Retry-After` the door must answer with.
const DELAY: Duration = Duration::from_secs(30);

fn big_file() -> String {
    (0..2_400)
        .map(|n| format!("fn line_{n:05}() {{}}\n"))
        .collect()
}

// ------------------------------------------------------------------------------------------
// The stub `urn:llm` peer: every ask fails the same way, and is counted.
// ------------------------------------------------------------------------------------------

/// What the stub's ask does instead of answering.
#[derive(Clone, Copy, Debug)]
enum Fails {
    /// The backend answered 429 with `Retry-After: 30` (ikigai-llm 0.16's `RetryAfter`).
    RateLimited,
    /// The backend answered 401 to the module's credential (ikigai-llm 0.16's `Refused`).
    Refused,
    /// The backend answers: the failure, if any, is gonk's own window.
    Never,
}

struct StubAsk {
    fails: Fails,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for StubAsk {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.fails {
            Fails::RateLimited => Err(Error::RetryAfter {
                delay: Some(DELAY),
                detail: "rate-limited: llm backend returned 429".to_string(),
            }),
            Fails::Refused => Err(Error::Refused("llm backend returned 401".to_string())),
            Fails::Never => Ok(Representation::new(
                ReprType::new("text/plain"),
                b"NOTHING ABOVE THRESHOLD\n".to_vec(),
            )),
        }
    }
    fn name(&self) -> &str {
        "stub-ask"
    }
    fn describe(&self) -> Description {
        let optional = |name: &str| ArgSpec::new(name).optional();
        Description::new("stub-ask")
            .verb(Verb::Source)
            .input(optional("prompt"))
            .input(optional("in"))
            .input(optional("system"))
            .input(optional("temperature"))
            .input(optional("max_tokens"))
    }
}

struct StubModel;

#[async_trait]
impl Endpoint for StubModel {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"stub-model".to_vec(),
        ))
    }
    fn name(&self) -> &str {
        "stub-model"
    }
    fn describe(&self) -> Description {
        Description::new("stub-model").verb(Verb::Source)
    }
}

struct Peer {
    calls: Arc<AtomicUsize>,
    mount: mount::Mount,
    _dir: TempDir,
}

fn stub_peer(fails: Fails) -> Peer {
    // A scratchpad path is too long for `sun_path`: a short directory under `/tmp`.
    let dir = tempfile::Builder::new()
        .prefix("r9")
        .tempdir_in("/tmp")
        .expect("a short socket directory");
    let socket = dir.path().join("l.sock");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut space = EndpointSpace::new();
    for provider in ["urn:llm:coder", "urn:llm"] {
        space = space
            .bind(
                Exact::new(format!("{provider}:ask")),
                StubAsk {
                    fails,
                    calls: Arc::clone(&calls),
                },
            )
            .bind(Exact::new(format!("{provider}:model")), StubModel);
    }
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(kernel, &path));
    ready::socket(&socket);
    let mount = mount::parse(
        &format!("prefer urn:llm:={}", socket.display()),
        std::path::Path::new("/home/nobody"),
    )
    .expect("a mount line");
    Peer {
        calls,
        mount,
        _dir: dir,
    }
}

// ------------------------------------------------------------------------------------------
// The served gonk: one browse root, the mount behind a model-call window, the real HTTP door
// with its access log collected.
// ------------------------------------------------------------------------------------------

struct Gonk {
    addr: SocketAddr,
    layout: quic::Layout,
    peer: Peer,
    access: Arc<Mutex<Vec<String>>>,
    _root: TempDir,
    _config: TempDir,
}

fn gonk(fails: Fails, calls: Window) -> Gonk {
    let root = tempfile::tempdir().expect("a root");
    std::fs::write(root.path().join(FILE), big_file()).unwrap();
    let peer = stub_peer(fails);
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let roots: Vec<(String, PathBuf)> = vec![(ROOT.to_string(), root.path().to_path_buf())];
    let tiers = ExplainTiers::default();
    let wired = browse::wire(
        roots,
        store
            .shared_writer()
            .expect("a shared store hands out its writer"),
        None,
        Some(&tiers),
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        vec![mount::space(&peer.mount, &calls)],
        Vec::new(),
        None,
        None,
        None,
    ));

    let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("a loopback listener");
    let addr = listener.local_addr().expect("a local address");
    let config = tempfile::tempdir().expect("a config home");
    let layout = quic::Layout::in_config_home(config.path());
    let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: vec![ROOT.to_string()],
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let access = Arc::new(Mutex::new(Vec::<String>::new()));
    let into = Arc::clone(&access);
    let log = AccessLog::to(
        Door::Http,
        Arc::new(move |line: &str| into.lock().unwrap().push(line.to_string())),
    );
    let http = Arc::new(doors::http_kernel_with(
        Arc::clone(&hub),
        web::space(face),
        Some(log),
    ));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Write).expect("the ledger's tokens"),
        bind: addr,
        passkeys: Some(passkeys),
        anonymous_sparql_budget_ms: budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(
            http,
            doors::http_cap(door.clone()),
            listener,
            doors::edge_config(door),
        ))
    });
    Gonk {
        addr,
        layout,
        peer,
        access,
        _root: root,
        _config: config,
    }
}

/// One HTTP answer: the status, the head's lines and the body.
struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

impl Gonk {
    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(&self, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> Answer {
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
        stream.write_all(head.as_bytes()).expect("write head");
        stream.write_all(body.as_bytes()).expect("write body");
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line for {method} {path}: {response}"));
        Answer {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    fn post_json(&self, path: &str, body: &str) -> Answer {
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

    fn challenge(&self, op: &str) -> String {
        let answer = self.post_json(&format!("/auth/{op}"), "{}");
        assert_eq!(answer.status, 200, "{}", answer.body);
        let v: serde_json::Value = serde_json::from_str(&answer.body).expect("a challenge");
        v["challenge"].as_str().expect("challenge").to_string()
    }

    /// Enroll a passkey under the `--browse derive` role over every root and sign in: the
    /// identity whose session the Review button carries.
    fn sign_in_deriving(&self) -> String {
        let mut scopes = grants_for("default", Authority::Write).expect("ledger tokens");
        scopes.extend(
            browse_role_grants(BrowseRole::Derive, &[], Some("localhost")).expect("derive"),
        );
        let authenticator = common::Authenticator::named("reviewer");
        let invite = identity::invite(
            &self.layout,
            "reviewer",
            &scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .expect("an invite");
        let c = self.challenge("register-options");
        let answer = self.post_json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(answer.status, 200, "{}", answer.body);
        let c = self.challenge("login-options");
        let answer = self.post_json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(answer.status, 200, "{}", answer.body);
        let v: serde_json::Value = serde_json::from_str(&answer.body).expect("a session");
        v["session"].as_str().expect("session").to_string()
    }

    /// The Review button's click: browse's own `hx-get`, folded into `/k?c=` as `web/gonk.js`
    /// folds it, from gonk's own page, carrying the signed-in session.
    fn click_review(&self, session: &str) -> Answer {
        let command = format!("source urn:repo:{ROOT}:review:{FILE} as=text/html");
        let encoded: String = command
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect();
        let own = self.origin();
        self.raw(
            "GET",
            &format!("/k?c={encoded}"),
            &[
                ("Accept", "text/html".to_string()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("Sec-Fetch-Mode", "cors".to_string()),
                (
                    "Referer",
                    format!("{own}/browse/urn:repo:{ROOT}:file:{FILE}"),
                ),
                ("HX-Request", "true".to_string()),
                ("Cookie", format!("{}={session}", identity::SESSION_COOKIE)),
            ],
            "",
        )
    }

    fn model_calls(&self) -> usize {
        self.peer.calls.load(Ordering::SeqCst)
    }

    /// The access line the click wrote: the one for `urn:iki:gonk:k`, the door's own target.
    fn click_line(&self) -> String {
        let lines = self.access.lock().unwrap();
        let clicks: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains(" urn:iki:gonk:k "))
            .collect();
        assert_eq!(clicks.len(), 1, "one line for the one click: {lines:#?}");
        clicks[0].clone()
    }
}

/// The browse default window: large enough that only the stub refuses.
fn roomy() -> Window {
    budget::model_calls(budget::DEFAULT_MODEL_CALLS_PER_HOUR)
}

// ------------------------------------------------------------------------------------------
// The tests.
// ------------------------------------------------------------------------------------------

/// ★ A backend 429: one model call, and the browser gets a `429` with `Retry-After: 30`.
#[test]
fn a_rate_limited_review_click_is_one_429_with_retry_after() {
    let gonk = gonk(Fails::RateLimited, roomy());
    let session = gonk.sign_in_deriving();
    let answer = gonk.click_review(&session);
    assert_eq!(
        gonk.model_calls(),
        1,
        "the pass stops at the first rate limit; it does not spend the other two regions"
    );
    assert_eq!(answer.status, 429, "{}\n\n{}", answer.head, answer.body);
    assert_eq!(
        answer.header("Retry-After").as_deref(),
        Some("30"),
        "the delay the backend asked for crosses the wire and reaches the browser: {}",
        answer.head
    );
    assert!(
        answer.body.contains("llm backend returned 429"),
        "the body names the refusal: {}",
        answer.body
    );
    let line = gonk.click_line();
    assert!(line.contains(" outcome=retry-after "), "{line}");
}

/// ★ A backend 401 to the module's credential: one model call, and the browser gets the
/// refusal, `502 Bad Gateway` (a remote authority refused the credential this side presented),
/// never a pass with every region collapsed.
#[test]
fn a_refused_review_click_is_one_502_naming_the_refusal() {
    let gonk = gonk(Fails::Refused, roomy());
    let session = gonk.sign_in_deriving();
    let answer = gonk.click_review(&session);
    assert_eq!(
        gonk.model_calls(),
        1,
        "the pass stops at the first refusal; it does not spend the other two regions"
    );
    assert_eq!(answer.status, 502, "{}\n\n{}", answer.head, answer.body);
    assert_eq!(answer.header("Retry-After"), None, "{}", answer.head);
    assert!(
        answer.body.contains("llm backend returned 401"),
        "the body names the refusal: {}",
        answer.body
    );
    let line = gonk.click_line();
    assert!(line.contains(" outcome=refused "), "{line}");
}

/// ★ gonk's OWN model-call window (`urn:iki:gonk:mount:calls`) refusing mid-pass is the same
/// story: at a window of one, region one is asked, region two is refused here before the wire
/// with the window's `RetryAfter`, and the click is a `429` with `Retry-After`, naming the
/// window. On browse 0.18.9 regions two and three were each collapsed and the pass answered
/// `200` as "incomplete".
#[test]
fn gonks_own_model_call_window_stops_a_review_click_as_a_429() {
    let gonk = gonk(Fails::Never, budget::model_calls(1));
    let session = gonk.sign_in_deriving();
    let answer = gonk.click_review(&session);
    assert_eq!(
        gonk.model_calls(),
        1,
        "only the first region reached the model"
    );
    assert_eq!(answer.status, 429, "{}\n\n{}", answer.head, answer.body);
    let retry_after: u64 = answer
        .header("Retry-After")
        .unwrap_or_else(|| panic!("a 429 carries Retry-After: {}", answer.head))
        .parse()
        .expect("Retry-After is delay-seconds, a whole number");
    // The window is an hour and was spent moments ago: the wait is most of it, never 0.
    assert!(
        (1..=3_600).contains(&retry_after),
        "Retry-After: {retry_after}"
    );
    assert!(
        answer.body.contains(budget::MODEL_CALLS_IRI),
        "the body names the window: {}",
        answer.body
    );
    let line = gonk.click_line();
    assert!(line.contains(" outcome=retry-after "), "{line}");
}
