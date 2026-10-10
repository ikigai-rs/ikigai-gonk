//! Ledger [#880](http://localhost:1060/l/default/item/880): a GET with side effects, sent by a
//! page that is not gonk's own.
//!
//! `SameSite=Strict` keys on the SITE, and a site ignores the port: a page on
//! `http://localhost:8090` is same-site with gonk on `http://localhost:1060`, so every request
//! it makes carries gonk's session cookie. The door refused that page's WRITES (a non-GET whose
//! `Origin` or `Sec-Fetch-Site` names another page), but a GET passed under the signed-in
//! identity's whole grant — and a GET can spend inference and write: `urn:repo:{root}:explain`
//! and `:review` derive through the mounted `urn:llm:` peer and archive what it said.
//!
//! The reproductions below each FAILED before this change, run on `ed43af3` plus PR 98 (which
//! touched only the socket): the stub peer was called, and explain and review archived its
//! answer. Everything here runs against a STUB `urn:llm` peer — a kernel served over
//! `ikigai_ipc` on a short socket under `/tmp`, mounted exactly as `gonk.mount` mounts the real
//! one — so no test spends real inference, and the mount's own net-host check is in the path.
//!
//! Nothing here touches a live gonk: every server binds `127.0.0.1:0` over a tempdir config
//! home.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

mod common;
mod ready;

use async_trait::async_trait;
use futures::executor::block_on;
use ikigai_core::{
    ArgSpec, Capability, Description, Endpoint, EndpointSpace, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use ikigai_gonk::config::{ExplainTiers, QueuePolicy};
use ikigai_gonk::grants::{browse_role_grants, grants_for, Authority, BrowseRole};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{browse, compose_with, doors, mount, quic, web};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

const ROOT: &str = "demo";
/// The port of the attacking page: another program on this machine, same site.
const OTHER: &str = "http://localhost:8090";

// ------------------------------------------------------------------------------------------
// The stub `urn:llm` peer.
// ------------------------------------------------------------------------------------------

/// Answers every ask with the same short text and counts the calls.
struct StubAsk {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Endpoint for StubAsk {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Representation::new(
            ReprType::new("text/plain"),
            // Readable as an explanation, and as a review that found nothing (browse refuses an
            // answer it cannot parse, and archives nothing then).
            b"A stub explanation: nothing here is real.\nNOTHING ABOVE THRESHOLD\n".to_vec(),
        ))
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

/// The stub peer, served on a socket under a short `/tmp` directory (a scratchpad path is too
/// long for `sun_path`), and the mount line that reaches it.
struct Peer {
    calls: Arc<AtomicUsize>,
    mount: mount::Mount,
    _dir: TempDir,
}

fn stub_peer() -> Peer {
    let dir = tempfile::Builder::new()
        .prefix("g8")
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
// The served gonk: a browse root, the mount, and the real HTTP door.
// ------------------------------------------------------------------------------------------

struct Gonk {
    addr: SocketAddr,
    layout: quic::Layout,
    hub: Arc<Kernel>,
    peer: Peer,
    _root: TempDir,
    _config: TempDir,
}

fn gonk() -> Gonk {
    let root = tempfile::tempdir().expect("a root");
    for name in ["README.md", "own.md", "typed.md", "script.md"] {
        std::fs::write(
            root.path().join(name),
            format!("# {name}\n\nA file to explain.\n"),
        )
        .unwrap();
    }
    let peer = stub_peer();
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let roots: Vec<(String, PathBuf)> = vec![(ROOT.to_string(), root.path().to_path_buf())];
    let tiers = ExplainTiers::default();
    let wired = browse::wire(roots, handle, None, Some(&tiers), &graph, None);
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        vec![mount::space(&peer.mount)],
        Vec::new(),
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
    });
    let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
    let door = doors::HttpDoor {
        anonymous: grants_for("default", Authority::Write).expect("the ledger's tokens"),
        port: addr.port(),
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
    Gonk {
        addr,
        layout,
        hub,
        peer,
        _root: root,
        _config: config,
    }
}

/// Who sent a request, as the browser labels it.
#[derive(Clone, Copy, Debug)]
enum From {
    /// gonk's own page (htmx): `Sec-Fetch-Site: same-origin`, a `Referer` on this origin.
    OwnPage,
    /// A person typing the URL or opening a bookmark: `Sec-Fetch-Site: none`.
    Typed,
    /// A process with no browser (curl, a script): no fetch metadata at all.
    NoBrowser,
    /// A page on `http://localhost:8090` loading the URL as an image: `no-cors`, a `Referer`,
    /// no `Origin`.
    OtherPortImage,
    /// The same page calling `fetch(url, {credentials: "include"})`: `cors`, with `Origin`.
    OtherPortFetch,
    /// The same page navigating the window there (a link, `location = …`).
    OtherPortNavigation,
    /// An old browser on that page that sends no `Sec-Fetch-*`: only its `Referer` says so.
    OtherPortRefererOnly,
}

impl Gonk {
    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
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
        (status, body.to_string())
    }

    /// A GET of `path`, labeled as `from` labels it, carrying `session` as gonk's cookie.
    fn get(&self, path: &str, from: From, session: Option<&str>) -> (u16, String) {
        let own = self.origin();
        let mut headers: Vec<(&str, String)> = vec![("Accept", "text/html".to_string())];
        let mut add = |k: &'static str, v: &str| headers.push((k, v.to_string()));
        match from {
            From::OwnPage => {
                add("Sec-Fetch-Site", "same-origin");
                add("Sec-Fetch-Mode", "cors");
                add(
                    "Referer",
                    &format!("{own}/browse/urn:repo:demo:file:README.md"),
                );
                add("HX-Request", "true");
            }
            From::Typed => {
                add("Sec-Fetch-Site", "none");
                add("Sec-Fetch-Mode", "navigate");
            }
            From::NoBrowser => {}
            From::OtherPortImage => {
                add("Sec-Fetch-Site", "same-site");
                add("Sec-Fetch-Mode", "no-cors");
                add("Sec-Fetch-Dest", "image");
                add("Referer", &format!("{OTHER}/innocent.html"));
            }
            From::OtherPortFetch => {
                add("Sec-Fetch-Site", "same-site");
                add("Sec-Fetch-Mode", "cors");
                add("Origin", OTHER);
                add("Referer", &format!("{OTHER}/innocent.html"));
            }
            From::OtherPortNavigation => {
                add("Sec-Fetch-Site", "same-site");
                add("Sec-Fetch-Mode", "navigate");
                add("Sec-Fetch-Dest", "document");
                add("Referer", &format!("{OTHER}/innocent.html"));
            }
            From::OtherPortRefererOnly => {
                add("Referer", &format!("{OTHER}/innocent.html"));
            }
        }
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    fn post_json(&self, path: &str, body: &str) -> (u16, String) {
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
        let (status, body) = self.post_json(&format!("/auth/{op}"), "{}");
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("a challenge");
        v["challenge"].as_str().expect("challenge").to_string()
    }

    /// Enrol a passkey under the `--browse derive` role over every root (plus the default
    /// ledger's write grant) and sign in: the identity whose session a same-site page rides.
    fn sign_in_deriving(&self) -> String {
        let mut scopes = grants_for("default", Authority::Write).expect("ledger tokens");
        scopes.extend(
            browse_role_grants(BrowseRole::Derive, &[], Some("localhost")).expect("derive"),
        );
        let authenticator = common::Authenticator::named("deriver");
        let invite = identity::invite(
            &self.layout,
            "deriver",
            &scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .expect("an invite");
        let c = self.challenge("register-options");
        let (status, body) = self.post_json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(status, 200, "{body}");
        let c = self.challenge("login-options");
        let (status, body) = self.post_json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("a session");
        v["session"].as_str().expect("session").to_string()
    }

    fn calls(&self) -> usize {
        self.peer.calls.load(Ordering::SeqCst)
    }

    /// How many explanation versions the archive holds for `path`, read as root.
    fn archived(&self, path: &str) -> String {
        let read = block_on(Kernel::issue(
            &self.hub,
            Request::new(
                Verb::Source,
                Iri::parse(format!("urn:repo:{ROOT}:explain-versions:{path}")).unwrap(),
            ),
            &Capability::root(),
        ))
        .expect("explain-versions");
        String::from_utf8_lossy(&read.bytes).into_owned()
    }
}

/// `/k?c=<command>`, the command percent-encoded whole.
fn k(command: &str) -> String {
    let encoded: String = command
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    format!("/k?c={encoded}")
}

const FOREIGN: [From; 4] = [
    From::OtherPortImage,
    From::OtherPortFetch,
    From::OtherPortNavigation,
    From::OtherPortRefererOnly,
];

// ------------------------------------------------------------------------------------------
// S1, the reproduction: explain and review, from a page on another localhost port, under the
// signed-in derive grant. On `ed43af3` each answered 200 and the stub peer was called.
// ------------------------------------------------------------------------------------------

#[test]
fn a_page_on_another_port_cannot_spend_inference_through_explain() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    for from in FOREIGN {
        for path in [
            k("source urn:repo:demo:explain:README.md as=text/html"),
            // The mechanical mapping reaches the same resource without `/k` at all.
            "/repo/demo/explain/README.md".to_string(),
        ] {
            let (status, body) = gonk.get(&path, from, Some(&session));
            assert!(
                status == 403 && gonk.calls() == 0,
                "{from:?} GET {path} answered {status} and the stub peer was called {} time(s): \
                 {}",
                gonk.calls(),
                body.chars().take(300).collect::<String>()
            );
        }
    }
    let archived = gonk.archived("README.md");
    assert!(
        !archived.contains("stub"),
        "an explanation was archived: {archived}"
    );
}

#[test]
fn a_page_on_another_port_cannot_spend_inference_through_review() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    for from in FOREIGN {
        let path = k("source urn:repo:demo:review:README.md");
        let (status, body) = gonk.get(&path, from, Some(&session));
        assert!(
            status == 403 && gonk.calls() == 0,
            "{from:?} GET {path} answered {status} and the stub peer was called {} time(s): {}",
            gonk.calls(),
            body.chars().take(300).collect::<String>()
        );
    }
}

/// The peer itself, asked directly by its own IRI through the mechanical mapping — the shortest
/// path from a page to the model.
#[test]
fn a_page_on_another_port_cannot_ask_the_mounted_model_directly() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    for from in FOREIGN {
        let (status, body) = gonk.get("/llm/coder/ask?prompt=hello", from, Some(&session));
        assert!(
            status == 403 && gonk.calls() == 0,
            "{from:?} answered {status}, stub called {} time(s): {body}",
            gonk.calls()
        );
    }
}

// ------------------------------------------------------------------------------------------
// What keeps working.
// ------------------------------------------------------------------------------------------

/// gonk's own Explain and Review buttons (htmx, same origin), a typed URL, and a process with
/// no browser (which carries a session only if it chose to) still derive.
#[test]
fn gonks_own_page_a_typed_url_and_a_non_browser_client_still_derive() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    let mut expected = 0;
    for (from, file) in [
        (From::OwnPage, "own.md"),
        (From::Typed, "typed.md"),
        (From::NoBrowser, "script.md"),
    ] {
        let (status, body) = gonk.get(
            &k(&format!("source urn:repo:demo:explain:{file} as=text/html")),
            from,
            Some(&session),
        );
        assert_eq!(status, 200, "{from:?}: {body}");
        expected += 1;
        assert_eq!(gonk.calls(), expected, "{from:?}: the stub was not called");
        let archived = gonk.archived(file);
        assert!(
            archived.contains("stub"),
            "{from:?}: nothing archived: {archived}"
        );
    }
    let (status, body) = gonk.get(
        &k("source urn:repo:demo:review:README.md"),
        From::OwnPage,
        Some(&session),
    );
    assert_eq!(status, 200, "review from gonk's own page: {body}");
}

/// Plain reads keep working when another page NAVIGATES here — a link from the CMS, a form —
/// under the identity's READ authority: a ledger item page, the browse shell, a file face. What
/// such a page may not do is LOAD them (an image, a fetch): nothing gonk serves is meant to be
/// embedded, and that is refused at the edge with the door's own sentence.
#[test]
fn plain_reads_from_another_ports_navigation_still_work_and_its_loads_are_refused() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    let (status, filed) = gonk.raw(
        "POST",
        "/iki/ledger/append",
        &[],
        "Linked from another page",
    );
    assert_eq!(status, 200, "{filed}");
    let reads = [
        "/l/default/item/1".to_string(),
        "/browse/urn:repo:demo:file:README.md".to_string(),
        k("source urn:repo:demo:file:README.md as=text/html"),
        k("source urn:repo:demo:tree"),
    ];
    for from in [From::OtherPortNavigation, From::OtherPortRefererOnly] {
        for path in &reads {
            let (status, body) = gonk.get(path, from, Some(&session));
            assert_eq!(status, 200, "{from:?} GET {path}: {body}");
        }
    }
    // ★ The item page still offers the ledger's own forms: a ledger WRITE token is kept, since
    // only a POST can use it and a POST from another page is refused on its own.
    let (_, page) = gonk.get(
        "/l/default/item/1",
        From::OtherPortNavigation,
        Some(&session),
    );
    assert!(page.contains("<form"), "{page}");
    for from in [From::OtherPortImage, From::OtherPortFetch] {
        for path in &reads {
            let (status, body) = gonk.get(path, from, Some(&session));
            assert_eq!(status, 403, "{from:?} GET {path}: {body}");
            assert!(body.contains("NAVIGATE"), "{body}");
        }
    }
    assert_eq!(gonk.calls(), 0, "a read spent inference");
}

/// A derivation refused because another page asked for it says WHY — the kernel alone would
/// say "does not grant `urn:cap:net:*`" to a person whose grant does.
#[test]
fn the_refusal_names_the_other_page() {
    let gonk = gonk();
    let session = gonk.sign_in_deriving();
    let (status, body) = gonk.get(
        &k("source urn:repo:demo:explain:README.md as=text/html"),
        From::OtherPortNavigation,
        Some(&session),
    );
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("not this server's own") && body.contains("READ"),
        "{body}"
    );
}

/// The door's marker for another page's navigation is the door's alone: a grant naming it is
/// refused, like the refusal markers.
#[test]
fn a_grant_cannot_name_the_foreign_page_marker() {
    for marker in [
        ikigai_gonk::admit::FOREIGN_PAGE,
        ikigai_gonk::admit::REFUSED_FOREIGN_LOAD,
    ] {
        assert!(quic::grant_refusal("x", &[marker.to_string()]).is_some());
    }
}
