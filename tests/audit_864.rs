//! Audit round 4's reproductions (ledger [#864](http://localhost:1060/l/default/item/864)),
//! ported from `ikigai-devtools/claude/research/audit-ikigai-gonk-2026-10-07/claude/repro/`.
//!
//! Each test asserts the behavior the code's own documentation promises, and each FAILED on
//! `c3443a6` because of the defect it names — run there before its fix, which is what makes
//! it a regression test rather than a description. R1–R6 are the auditor's tests with their
//! bodies unchanged; R7 was a shell probe in `RUN.sh` and is the same probe in Rust here,
//! against the built binary with a scratch `HOME` and `XDG_CONFIG_HOME`.
//!
//! Nothing here touches a live gonk: every server binds `127.0.0.1:0` over a tempdir config
//! home, and every CLI run has its own homes.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, compose_with, doors, quic, web};
use ikigai_store::DurableStore;

fn request(verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    )
}

// ------------------------------------------------------------------------------------------
// R1. The backup family's tokens are refused at startup but ADMITTED per use.
//
// `grants::CAP_GONK_ADMIN`'s doc: "Neither can be attached to a certificate or a passkey, so
// neither is reachable from the QUIC door or the HTTP door at all". `trigger.rs`'s module doc:
// gonk refuses "the backup family's tokens on every certificate and every passkey it admits
// (check_grants), at startup and again per use". The per-use check is
// `quic::scopes_for_grant` (QUIC minter per connection, `identity::scopes_of` per passkey
// request), and it checks broad + wildcard tokens but NOT the backup family. Trigger: an
// operator edits grants.json while gonk runs (the files are re-read per connection precisely
// so an edit takes effect), adding `urn:cap:gonk:restore` to a grant.
// ------------------------------------------------------------------------------------------
#[test]
fn r1_a_grant_naming_the_restore_token_is_refused_per_use_as_it_is_at_startup() {
    const FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    std::fs::create_dir_all(config.path().join("gonk")).unwrap();
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push(backup::CAP_RESTORE.to_string());
    std::fs::write(
        layout.grants_json(),
        serde_json::json!({ "ops": scopes }).to_string(),
    )
    .unwrap();
    std::fs::write(
        layout.clients_json(),
        format!(r#"{{"clients": {{"{FP}": {{"grant": "ops"}}}}}}"#),
    )
    .unwrap();

    let grants: BTreeMap<String, Vec<String>> = quic::read_grants(&layout.grants_json()).unwrap();
    let at_startup = quic::check_grants(&grants);
    assert!(at_startup.is_err(), "startup refuses the restore token");

    // Per use: the QUIC door's per-connection decision, and the HTTP door's per-request one.
    let enrolment = quic::read_enrolment(&layout.clients_json())
        .unwrap()
        .unwrap();
    let per_connection = quic::authority(&enrolment, &grants, FP);
    let per_passkey = identity::scopes_of(&layout, "ops");

    // And what that capability reaches through the QUIC door's kernel: a restore that
    // writes a RocksDB store at a path the REMOTE caller chose.
    let backups_dir = tempfile::tempdir().unwrap();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().unwrap(),
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: backups_dir.path().to_path_buf(),
                keep: 5,
                every: None,
                store_path: PathBuf::from("/nonexistent/live/store"),
            }),
            jobs: None,
        }),
    ));
    let door = doors::door_kernel(Arc::clone(&hub));
    let into = backups_dir.path().join("chosen-by-the-remote-caller");
    let reached = per_connection.as_ref().ok().map(|(_, cap)| {
        block_on(Kernel::issue(
            &door,
            request(
                Verb::Sink,
                backup::RESTORE,
                &[
                    ("into", into.to_str().unwrap().as_bytes()),
                    ("content", b"<urn:a> <urn:b> <urn:c> <urn:g> .\n"),
                ],
            ),
            cap,
        ))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
    });

    assert!(
        per_connection.is_err() && per_passkey.is_err(),
        "startup refuses grant `ops` ({}) but per use admits it — quic per-connection: {:?}; \
         passkey per-request: {:?}; Sink {} through the QUIC door kernel under that capability: \
         {:?}; directory written: {}",
        at_startup.unwrap_err().lines().next().unwrap_or(""),
        per_connection.as_ref().map(|(g, _)| g),
        per_passkey,
        backup::RESTORE,
        reached,
        into.is_dir()
    );
}

// ------------------------------------------------------------------------------------------
// R1, every path. One function decides (`quic::grant_refusal`), and each path that turns a
// grant into scopes is asked here on its own, so a fifth path added later without it is the
// test that goes red rather than the audit that finds it.
// ------------------------------------------------------------------------------------------
#[test]
fn r1_every_grant_to_scopes_path_refuses_the_backup_family() {
    for token in ikigai_gonk::grants::CAP_GONK_ADMIN {
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let mut scopes = grants_for("default", Authority::Read).unwrap();
        scopes.push(token.to_string());
        let grants: BTreeMap<String, Vec<String>> =
            [("ops".to_string(), scopes.clone())].into_iter().collect();

        assert!(quic::check_grants(&grants).is_err(), "startup: {token}");
        assert!(
            quic::scopes_for_grant(&grants, "ops").is_err(),
            "per use: {token}"
        );
        assert!(
            ikigai_gonk::trigger::reviewer_scopes(&grants, "ops").is_err(),
            "the reviewer grant: {token}"
        );
        // The QUIC door's per-connection decision, and the passkey door's per-request one,
        // read the files as an edit after startup left them.
        std::fs::create_dir_all(config.path().join("gonk")).unwrap();
        std::fs::write(
            layout.grants_json(),
            serde_json::json!({ "ops": scopes }).to_string(),
        )
        .unwrap();
        let enrolment = quic::parse_enrolment(r#"{"clients": {"ab": "ops"}}"#).unwrap();
        assert!(
            quic::authority(&enrolment, &grants, "ab").is_err(),
            "per connection: {token}"
        );
        assert!(
            identity::scopes_of(&layout, "ops").is_err(),
            "per passkey request: {token}"
        );

        // Both writers of grants.json refuse it, and write nothing.
        let fresh = tempfile::tempdir().unwrap();
        let fresh = quic::Layout::in_config_home(fresh.path());
        let refused = quic::put_grant(&fresh, "ops", &scopes, true).unwrap_err();
        assert!(refused.contains(token), "{refused}");
        let refused = quic::enrol(&fresh, "ops", "ab", &scopes, true).unwrap_err();
        assert!(refused.contains(token), "{refused}");
        assert!(!fresh.grants_json().exists() && !fresh.clients_json().exists());
    }
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 2: `enrol` applied only the broad-token refusal, while `put_grant` also
// refused the offering wildcards — so a certificate could be enrolled under a grant a
// passkey could not. Both are `grant_refusal` now.
// ------------------------------------------------------------------------------------------
#[test]
fn enrol_refuses_the_offering_wildcards_as_put_grant_does() {
    for wildcard in [
        ikigai_gonk::grants::CAP_NET_ANY,
        ikigai_gonk::grants::CAP_EXEC_ANY,
    ] {
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let mut scopes = grants_for("default", Authority::Read).unwrap();
        scopes.push(wildcard.to_string());
        let refused = quic::enrol(&layout, "laptop", "ab", &scopes, true).unwrap_err();
        assert!(refused.contains(wildcard), "{refused}");
        assert!(!layout.grants_json().exists() && !layout.clients_json().exists());
        assert_eq!(
            refused,
            quic::put_grant(&layout, "laptop", &scopes, true).unwrap_err(),
            "the two writers refuse with one sentence"
        );
    }
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 3: `client add` minted (or wrote) the bundle and THEN enrolled it, so a
// refusal from the enrolment left a bundle behind. Importing another client's certificate
// under a new name is refused by the enrolment ("already enrolled under grant `a`"), and on
// `c3443a6` it left `clients/b/` with that certificate in it.
// ------------------------------------------------------------------------------------------
#[test]
fn client_add_refuses_before_it_writes_a_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for dir in ["home", "xdg/ikigai"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .output()
            .expect("run ikigai-gonk")
    };
    let clients = root.join("xdg/ikigai/gonk/quic/clients");
    let first = run(&["client", "add", "a", "--ledger", "default=read"]);
    assert!(first.status.success(), "{first:?}");
    let a_cert = clients.join("a/client.crt");
    let refused = run(&[
        "client",
        "add",
        "b",
        "--cert",
        a_cert.to_str().unwrap(),
        "--ledger",
        "default=read",
    ]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("already enrolled under grant `a`"),
        "{refused:?}"
    );
    assert!(
        !clients.join("b").exists(),
        "a refused enrolment left a bundle behind"
    );
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 1: the net host in a grant is enforced against the mount it reaches.
// `ikigai-browse` declares the offering wildcard `urn:cap:net:*`, which the kernel satisfies
// with ANY net grant — so a grant naming `127.0.0.1` reached a socket mount (whose host is
// `localhost`). The mount refuses it by name, before it dials: the socket below does not
// exist, so a call that got past the check would come back `Unavailable`, not `Denied`.
// ------------------------------------------------------------------------------------------
#[test]
fn a_net_grant_for_another_host_is_refused_at_the_mount() {
    use ikigai_resolve::Resolver;
    let home = std::path::Path::new("/nonexistent-home");
    let mount = ikigai_gonk::mount::parse("prefer urn:llm:=/nonexistent/llm.sock", home).unwrap();
    let lazy = ikigai_gonk::mount::LazyMount::new(mount.target.clone());
    let ask = || request(Verb::Source, "urn:llm:coder:ask", &[("prompt", b"hi")]);
    let as_host = |host: &str| Capability::scoped([format!("urn:cap:net:{host}")]);

    match lazy.issue_as(ask(), &as_host("127.0.0.1")) {
        Err(ikigai_core::Error::Denied(why)) => {
            assert!(why.contains("urn:cap:net:localhost"), "{why}")
        }
        other => panic!("a grant for another host reached the mount: {other:?}"),
    }
    match lazy.issue_as(ask(), &as_host("localhost")) {
        Err(ikigai_core::Error::Unavailable(_)) => {}
        other => panic!("the mount's own host is admitted, then the dial fails: {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------
// A real HTTP door, as tests/web.rs starts one.
// ------------------------------------------------------------------------------------------
struct Server {
    addr: SocketAddr,
    layout: quic::Layout,
    _config: tempfile::TempDir,
}

impl Server {
    fn start() -> Server {
        Server::start_logged(None)
    }

    fn start_logged(access: Option<ikigai_gonk::access::AccessLog>) -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
        let layout_kept = layout.clone();
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
        });
        let http = Arc::new(doors::http_kernel_with(
            Arc::clone(&hub),
            web::space(face),
            access,
        ));
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).unwrap(),
            port: addr.port(),
            passkeys: Some(passkeys),
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        Server {
            addr,
            layout: layout_kept,
            _config: config,
        }
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        self.try_raw(method, path, headers, body)
            .unwrap_or_else(|| panic!("no status line for {method} {path}"))
    }

    /// `None` when the server answered nothing (the connection closed with no status line).
    fn try_raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> Option<(u16, String)> {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        // The default `Host` only when the caller names none: two `Host` lines would test the
        // parser's choice between them, not the door.
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("Host: localhost:{}\r\n", self.addr.port()));
        }
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
        let _ = stream.read_to_end(&mut raw);
        let response = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())?;
        Some((status, body.to_string()))
    }
}

// ------------------------------------------------------------------------------------------
// R2. A cross-site page can exhaust the passkey challenge table and lock sign-in out.
//
// On `c3443a6`: `doors::http_scopes` closes cross-site writes by computing an EMPTY capability for a POST
// whose Origin / Sec-Fetch-Site names another site. The passkey door declares no capability
// ("Public by design"), so that check does not reach it: a form POST from any page open in a
// browser on this machine (a `<form method=post action=http://localhost:1060/auth/login-options>`
// needs no preflight) mints a challenge. `identity::MAX_PENDING` (256) refuses past the bound
// for CHALLENGE_SECONDS (5 min), so 256 such posts make the real page's own same-origin sign-in
// fail, renewable every five minutes.
// ------------------------------------------------------------------------------------------
#[test]
fn r2_a_cross_site_page_cannot_lock_out_passkey_sign_in() {
    let server = Server::start();
    let cross_site = [
        ("Origin", "http://evil.example".to_string()),
        ("Sec-Fetch-Site", "cross-site".to_string()),
        (
            "Content-Type",
            "application/x-www-form-urlencoded".to_string(),
        ),
    ];
    let mut minted = 0;
    for _ in 0..identity::MAX_PENDING {
        let (status, _) = server.raw("POST", "/auth/login-options", &cross_site, "");
        if status == 200 {
            minted += 1;
        }
    }
    let origin = format!("http://localhost:{}", server.addr.port());
    let (status, body) = server.raw(
        "POST",
        "/auth/login-options",
        &[
            ("Accept", "application/json".to_string()),
            ("Content-Type", "application/json".to_string()),
            ("Origin", origin),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ],
        "{}",
    );
    assert!(
        minted == 0 && status == 200,
        "{minted} cross-site POSTs (Origin http://evil.example, Sec-Fetch-Site cross-site) each \
         minted a challenge; the page's own same-origin login-options then answered {status}: {}",
        body.trim()
    );
}

fn chrome_page(server: &Server, path: &str) -> Option<(u16, String)> {
    server.try_raw(
        "GET",
        path,
        &[
            (
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
            ),
            ("Sec-Fetch-Site", "none".to_string()),
        ],
        "",
    )
}

fn item_id(text: &str) -> String {
    let at = text
        .find("urn:iki:ledger:default:item:")
        .unwrap_or_else(|| panic!("no item IRI in: {text}"));
    text[at + "urn:iki:ledger:default:item:".len()..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect()
}

// ------------------------------------------------------------------------------------------
// R4. Any ledger writer can attribute a write to someone else's passkey, and the page
// renders it as that person.
//
// `web::Act` refuses a form field named `author` ("The door names the author; a form may
// not ... without this line a browser could attribute a comment to anyone"), and forwards the
// door-computed `principal` instead. But the same HTTP door also serves the mechanical route
// (`routes_only: false`): `POST /iki/ledger/append?author=…` reaches `urn:iki:ledger:append`,
// whose `author` is an ordinary optional string, with nothing refusing it. `enrich_authors`
// then renders any author of the form `urn:iki:gonk:passkey:<id>` as that passkey's CURRENT
// label. A passkey's IRI is in every signed-in write's Turtle face, so it is discoverable.
// Trigger: the anonymous loopback grant (or any passkey with ledger write) posting to the
// mechanical route.
// ------------------------------------------------------------------------------------------
#[test]
fn r4_only_the_door_can_name_a_passkey_as_an_author() {
    let server = Server::start();
    // A colleague's enrolled passkey, as `clients.json` holds it.
    std::fs::create_dir_all(server.layout.clients_json().parent().unwrap()).unwrap();
    std::fs::write(
        server.layout.clients_json(),
        r#"{"passkeys": {"Q1JFRC1CUklBTg": {"grant": "brian", "label": "Brian Sletten",
            "public_key": "AA", "sign_count": 0}}}"#,
    )
    .unwrap();
    // Anonymous, from this machine, the way `curl` (or `roborev file`) posts.
    let (status, body) = server.raw(
        "POST",
        "/iki/ledger/append?author=urn%3Aiki%3Agonk%3Apasskey%3AQ1JFRC1CUklBTg",
        &[],
        "Ship it, no review needed",
    );
    // ★ Brian's decision (a): the door REFUSES a principal-shaped author that is not the
    // request's own, so the forgery never becomes an item. (The auditor's test asserted a
    // 200 here and then read the page, which fits a fix in the face; this is the fix that
    // was chosen, so the write itself is what is checked — and that nothing was filed.)
    assert_eq!(status, 403, "{body}");
    let (status, page) = chrome_page(&server, "/l/default?status=all").unwrap();
    assert_eq!(status, 200);
    assert!(
        !page.contains("Ship it") && !page.contains("by Brian Sletten"),
        "an anonymous POST to the mechanical route filed an item attributed to another \
         person's passkey: …{}…",
        page.find("by Brian Sletten")
            .map(|at| &page[at.saturating_sub(120)..(at + 40).min(page.len())])
            .unwrap_or("")
    );
}

// ------------------------------------------------------------------------------------------
// R6. The access log's `principal` column on an HTTP READ is whatever the caller typed.
//
// `access.rs`: "who a READ is from. `ikigai-web` hands the principal to writes only, so an
// HTTP read's `principal` is `-`", and `principal()`: "The door stamps it
// (`doors::http_principal`) and drops a submitted one." But `ikigai-web` 0.1.39 reserves the
// provenance names only on MUTATING verbs (`is_provenance` is checked under
// `verb.is_mutating()`), so a `?principal=…` on a GET reaches the request untouched, and the
// access line records it as who asked. The column is `ikigai-log`'s `log:onBehalfOf`.
// ------------------------------------------------------------------------------------------
#[test]
fn r6_a_read_cannot_name_its_own_principal_in_the_access_log() {
    let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&lines);
    let server = Server::start_logged(Some(ikigai_gonk::access::AccessLog::to(
        ikigai_gonk::access::Door::Http,
        Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
    )));
    let (status, _) = chrome_page(
        &server,
        "/l/default?principal=urn%3Aiki%3Agonk%3Apasskey%3AQ1JFRC1CUklBTg",
    )
    .unwrap();
    assert_eq!(status, 200);
    let logged = lines.lock().unwrap().clone();
    let page = logged
        .iter()
        .find(|l| l.contains("urn:iki:gonk:page:ledger:default"))
        .cloned()
        .unwrap_or_default();
    assert!(
        page.contains(" principal=- "),
        "an anonymous GET named its own principal in the access log: {page}"
    );
}

// ------------------------------------------------------------------------------------------
// R2 and PENDING item 2, every path: a foreign `Host` (any method) and a cross-site write are
// REFUSED before dispatch — the pages, the mechanical routes and the passkey ceremonies alike
// — and the two callers the door exists for are untouched: the page's own same-origin
// ceremony, and a local process that sends no `Origin` at all.
// ------------------------------------------------------------------------------------------
#[test]
fn a_foreign_host_and_a_cross_site_write_are_refused_on_every_path() {
    let server = Server::start();
    let port = server.addr.port();
    let rebound = ("Host", format!("evil.example:{port}"));
    for (method, path) in [
        ("GET", "/"),
        ("GET", "/l/default"),
        ("GET", "/iki/ledger/items"),
        ("GET", "/static/gonk.css"),
        ("POST", "/auth/login-options"),
        ("POST", "/auth/register-options"),
        ("POST", "/auth/session"),
    ] {
        let (status, body) = server.raw(method, path, std::slice::from_ref(&rebound), "");
        assert_eq!(status, 403, "{method} {path} under a foreign Host: {body}");
    }
    for path in [
        "/auth/login-options",
        "/auth/register-options",
        "/auth/logout",
        "/iki/ledger/append",
    ] {
        for header in [
            ("Origin", "http://evil.example".to_string()),
            ("Sec-Fetch-Site", "cross-site".to_string()),
            ("Sec-Fetch-Site", "same-site".to_string()),
        ] {
            let (status, body) = server.raw("POST", path, std::slice::from_ref(&header), "x");
            assert_eq!(status, 403, "POST {path} with {header:?}: {body}");
        }
    }
    // The page's own ceremony, and `curl`.
    let (status, body) = server.raw(
        "POST",
        "/auth/login-options",
        &[
            ("Origin", format!("http://localhost:{port}")),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ],
        "{}",
    );
    assert_eq!(status, 200, "same-origin sign-in: {body}");
    let (status, body) = server.raw("POST", "/auth/login-options", &[], "{}");
    assert_eq!(status, 200, "a local process: {body}");
    // A refusal marker is the door's alone: a grant naming one is refused like any other.
    for marker in [
        ikigai_gonk::admit::REFUSED_FOREIGN_HOST,
        ikigai_gonk::admit::REFUSED_CROSS_SITE,
    ] {
        assert!(quic::grant_refusal("x", &[marker.to_string()]).is_some());
    }
}

// ------------------------------------------------------------------------------------------
// Ledger #879, item 1: a request this door refuses is refused at the EDGE, before `OPTIONS`
// and the `?description` face — which `ikigai-web` answers without dispatching, so a refusal
// made only in the kernel still told a rebinding page the declared verbs and the contract of
// every capability-free action. With `EdgeConfig::admit_fn` unset (gonk through ikigai-web
// 0.1.40), the foreign-`Host` `OPTIONS` below answered `204` and its `?description` `200`.
// ------------------------------------------------------------------------------------------
#[test]
fn a_refused_request_gets_no_options_and_no_description() {
    let server = Server::start();
    let port = server.addr.port();
    let rebound = [("Host", format!("evil.example:{port}"))];
    let cross_site = [
        ("Origin", "http://evil.example".to_string()),
        ("Sec-Fetch-Site", "cross-site".to_string()),
    ];
    for path in [
        "/l/default",
        "/iki/ledger/append",
        "/auth/login-options",
        "/",
    ] {
        for (method, query) in [
            ("OPTIONS", ""),
            ("GET", "?description"),
            ("HEAD", "?description"),
        ] {
            let (status, body) = server.raw(method, &format!("{path}{query}"), &rebound, "");
            assert_eq!(
                status, 403,
                "{method} {path}{query} under a foreign Host: {body}"
            );
            assert!(
                method == "HEAD" || body.contains("loopback name"),
                "{method} {path}{query}: {body}"
            );
            assert!(!body.contains("openapi"), "no contract leaks: {body}");
        }
        // A cross-site preflight is the first half of a cross-site write, refused with it.
        let (status, body) = server.raw("OPTIONS", path, &cross_site, "");
        assert_eq!(status, 403, "a cross-site OPTIONS {path}: {body}");
        assert!(body.contains("another site"), "{body}");
        // And a cross-site write that asks for a description is a write, refused at the edge.
        let (status, body) = server.raw("POST", &format!("{path}?description"), &cross_site, "x");
        assert_eq!(status, 403, "a cross-site POST {path}?description: {body}");
    }
    // The door's own callers are untouched: a same-origin sign-in, a local process's OPTIONS,
    // and the description of a resource read from this server's own name.
    let same_origin = [
        ("Origin", format!("http://localhost:{port}")),
        ("Sec-Fetch-Site", "same-origin".to_string()),
    ];
    let (status, body) = server.raw("POST", "/auth/login-options", &same_origin, "{}");
    assert_eq!(status, 200, "same-origin sign-in: {body}");
    let (status, body) = server.raw("OPTIONS", "/iki/ledger/append", &[], "");
    assert_eq!(status, 204, "a local process's OPTIONS: {body}");
    let (status, body) = server.raw("GET", "/l/default?description", &[], "");
    assert_eq!(
        status, 200,
        "a local description of the page refused above: {body}"
    );
}

// ------------------------------------------------------------------------------------------
// R4, both directions and every route. A free-text author is text and stays allowed; an
// author shaped like a principal is refused unless it is the request's own — on the
// mechanical route, on the form adapter's own query string, and through the QUIC door.
// ------------------------------------------------------------------------------------------
#[test]
fn a_plain_text_author_is_kept_and_another_principal_is_refused_on_every_route() {
    let server = Server::start();
    let (status, body) = server.raw(
        "POST",
        "/iki/ledger/append?author=chris",
        &[],
        "Filed by a bridge",
    );
    assert_eq!(status, 200, "{body}");
    let (status, page) =
        chrome_page(&server, &format!("/l/default/item/{}", item_id(&body))).expect("an item page");
    assert_eq!(status, 200);
    assert!(page.contains("by chris"), "a plain author renders as text");

    let someone = "urn%3Aiki%3Agonk%3Apasskey%3AQ1JFRC1CUklBTg";
    for path in [
        format!("/iki/ledger/append?author={someone}"),
        "/iki/ledger/append?author=%20URN%3AIKI%3AGONK%3APASSKEY%3Ax".to_string(),
        format!("/act?author={someone}"),
    ] {
        let (status, body) = server.raw(
            "POST",
            &path,
            &[(
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            )],
            "_ledger=default&_action=append&content=forged",
        );
        assert_eq!(status, 403, "POST {path}: {body}");
    }

    // The QUIC door: a client holding ledger write cannot name a passkey either.
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let door = doors::quic_kernel_with(Arc::clone(&hub), None);
    let writer = Capability::scoped(grants_for("default", Authority::Write).unwrap());
    let append = |author: &str| {
        block_on(Kernel::issue(
            &door,
            request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("author", author.as_bytes()), ("content", b"over quic")],
            ),
            &writer,
        ))
    };
    assert!(matches!(
        append("urn:iki:gonk:passkey:Q1JFRC1CUklBTg"),
        Err(ikigai_core::Error::Denied(_))
    ));
    assert!(append("laptop").is_ok());
}

// ------------------------------------------------------------------------------------------
// R7 and ledger #816: the QUIC client's lifecycle from the command line. `--rotate` is the
// explicit replacement (a new identity, the old fingerprint unenrolled, and it SAYS so);
// `client list` and `client remove` exist; and the printed `--connect` line names the port
// this config's server listens on, which follows the HTTP port.
// ------------------------------------------------------------------------------------------
struct Cli {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Cli {
    fn new(config: &str) -> Cli {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in ["home", "xdg/ikigai"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("xdg/ikigai/config.toml"), config).unwrap();
        Cli { _tmp: tmp, root }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .output()
            .expect("run ikigai-gonk");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn enrolled(&self) -> Vec<String> {
        let text = std::fs::read_to_string(self.root.join("xdg/ikigai/gonk/clients.json")).unwrap();
        let clients: serde_json::Value = serde_json::from_str(&text).unwrap();
        clients["clients"]
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }
}

fn fingerprint_in(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("fingerprint"))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| panic!("no fingerprint line in: {stdout}"))
}

#[test]
fn rotate_replaces_the_identity_and_unenrols_the_old_one() {
    let cli = Cli::new("");
    let (ok, first, err) = cli.run(&["client", "add", "laptop", "--ledger", "default=read"]);
    assert!(ok, "{err}");
    let (ok, kept, err) = cli.run(&[
        "client",
        "add",
        "laptop",
        "--ledger",
        "default=write",
        "--force",
    ]);
    assert!(ok, "{err}");
    assert_eq!(fingerprint_in(&first), fingerprint_in(&kept));
    assert!(kept.contains("identity is unchanged"), "{kept}");
    let (ok, rotated, err) = cli.run(&[
        "client",
        "add",
        "laptop",
        "--ledger",
        "default=write",
        "--rotate",
    ]);
    assert!(ok, "{err}");
    let (old, new) = (fingerprint_in(&first), fingerprint_in(&rotated));
    assert_ne!(old, new, "--rotate mints a new identity");
    assert!(
        rotated.contains("ROTATED") && rotated.contains(&old),
        "{rotated}"
    );
    assert_eq!(
        cli.enrolled(),
        [new],
        "only the new certificate is enrolled"
    );
    // Rotating a client that does not exist is refused, before anything is written.
    let (ok, _, err) = cli.run(&["client", "add", "ghost", "--rotate"]);
    assert!(!ok && err.contains("no identity to rotate"), "{err}");
    assert!(!cli.root.join("xdg/ikigai/gonk/quic/clients/ghost").exists());
}

#[test]
fn client_list_and_remove_show_and_revoke_what_is_enrolled() {
    let cli = Cli::new("");
    let (ok, a, err) = cli.run(&["client", "add", "a", "--ledger", "default=read"]);
    assert!(ok, "{err}");
    let (ok, b, err) = cli.run(&["client", "add", "b"]);
    assert!(ok, "{err}");
    let (ok, listed, err) = cli.run(&["client", "list"]);
    assert!(ok, "{err}");
    let row = |fp: &str| {
        listed
            .lines()
            .find(|line| line.contains(fp))
            .unwrap_or_else(|| panic!("no row for {fp}: {listed}"))
            .to_string()
    };
    assert!(
        row(&fingerprint_in(&a)).contains("trusted and enrolled"),
        "{listed}"
    );
    assert!(
        row(&fingerprint_in(&b)).contains("NOT enrolled"),
        "{listed}"
    );

    let (ok, removed, err) = cli.run(&["client", "remove", "a"]);
    assert!(ok, "{err}");
    assert!(removed.contains("unenrolled"), "{removed}");
    assert!(cli.enrolled().is_empty());
    assert!(!cli.root.join("xdg/ikigai/gonk/quic/clients/a").exists());
    let grants = std::fs::read_to_string(cli.root.join("xdg/ikigai/gonk/grants.json")).unwrap();
    assert!(
        grants.contains("\"a\""),
        "the grant is left alone: {grants}"
    );

    // An enrolment with no bundle (what an old `--force` left behind) is removable by its
    // fingerprint, and listed as what it is until then.
    let orphan = "ab".repeat(32);
    std::fs::write(
        cli.root.join("xdg/ikigai/gonk/clients.json"),
        format!(r#"{{"clients": {{"{orphan}": {{"grant": "a"}}}}}}"#),
    )
    .unwrap();
    let (_, listed, _) = cli.run(&["client", "list"]);
    assert!(listed.contains("NO bundle"), "{listed}");
    let (ok, _, err) = cli.run(&["client", "remove", "--fingerprint", &orphan]);
    assert!(ok, "{err}");
    assert!(cli.enrolled().is_empty());
    let (ok, _, err) = cli.run(&["client", "remove", "nobody"]);
    assert!(!ok && err.contains("client list"), "{err}");
}

#[test]
fn the_connect_line_names_the_port_the_server_listens_on() {
    let cli = Cli::new("gonk.port = 1070\n");
    let (ok, out, err) = cli.run(&["client", "add", "a", "--ledger", "default=read"]);
    assert!(ok, "{err}");
    assert!(out.contains("quic://<gonk host>:1070 "), "{out}");
    let (ok, out, err) = cli.run(&[
        "client",
        "add",
        "b",
        "--ledger",
        "default=read",
        "--quic-bind",
        "0.0.0.0:2000",
    ]);
    assert!(ok, "{err}");
    assert!(out.contains("quic://<gonk host>:2000 "), "{out}");
}

// ------------------------------------------------------------------------------------------
// Ledger #816 (4), #879 and R4 option (b): a QUIC write is attributed. The session names the
// client (`quic::minter`, never the capability), `ikigai-quic` stamps that name on every
// request as `principal`, the access log writes it, and the author rule lets that client name
// itself and nobody else — and a write that names NO author is attributed to it by the door.
// The stamp is written here by hand, as the transport writes it; `tests/doors.rs` drives the
// real transport end to end.
// ------------------------------------------------------------------------------------------
#[test]
fn a_quic_write_is_logged_attributed_and_may_name_only_its_own_client() {
    const FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    let grants: BTreeMap<String, Vec<String>> =
        [("rw".to_string(), scopes.clone())].into_iter().collect();
    let enrolment = quic::parse_enrolment(&format!(r#"{{"clients": {{"{FP}": "rw"}}}}"#)).unwrap();
    let (_, capability) = quic::authority(&enrolment, &grants, FP).unwrap();
    let me = quic::client_iri(FP);
    assert!(
        !capability.allows(&me),
        "the name is not authority: it rides beside the capability, never inside it (ledger #879)"
    );
    // A grant cannot hand out a client's name.
    scopes.push(me.clone());
    assert!(quic::grant_refusal("rw", &scopes).is_some());

    let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&lines);
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    let door = doors::quic_kernel_with(
        Arc::clone(&hub),
        Some(ikigai_gonk::access::AccessLog::to(
            ikigai_gonk::access::Door::Quic,
            Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
        )),
    );
    let append = |author: Option<&str>| {
        let mut args: Vec<(&str, &[u8])> =
            vec![("principal", me.as_bytes()), ("content", b"over quic")];
        if let Some(author) = author {
            args.push(("author", author.as_bytes()));
        }
        block_on(Kernel::issue(
            &door,
            request(Verb::Sink, "urn:iki:ledger:append", &args),
            &capability,
        ))
    };
    let author_of = |answer: Result<ikigai_core::Representation, ikigai_core::Error>| {
        let filed = String::from_utf8_lossy(&answer.expect("filed").bytes).into_owned();
        let number = filed
            .trim_start_matches('#')
            .split(' ')
            .next()
            .unwrap()
            .to_string();
        let read = block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                &format!("urn:iki:ledger:item:{number}"),
                &[("as", b"application/json")],
            ),
            &Capability::root(),
        ))
        .expect("the item");
        let item: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        item["item"]["author"].as_str().map(str::to_string)
    };
    // Absent: filled from the door's principal.
    assert_eq!(
        author_of(append(None)),
        Some(me.clone()),
        "absent -> filled"
    );
    // Its own principal: accepted.
    assert_eq!(
        author_of(append(Some(&me))),
        Some(me.clone()),
        "own -> accepted"
    );
    // Free text: kept as written.
    assert_eq!(
        author_of(append(Some("hermes"))),
        Some("hermes".to_string()),
        "free text -> kept"
    );
    // Another principal: refused, a client or a passkey alike.
    for other in [
        quic::client_iri("00"),
        "urn:iki:gonk:passkey:Q1JFRC1CUklBTg".to_string(),
    ] {
        assert!(
            matches!(append(Some(&other)), Err(ikigai_core::Error::Denied(_))),
            "another principal -> refused: {other}"
        );
    }
    let logged = lines.lock().unwrap().clone();
    assert_eq!(
        logged.len(),
        5,
        "one line per request, the filled ones included: {logged:#?}"
    );
    assert!(
        logged
            .iter()
            .all(|line| line.contains(&format!(" principal={me} "))),
        "{logged:#?}"
    );
}

// ------------------------------------------------------------------------------------------
// R3. `/sparql` (and the editor page) misreads the query form of valid SPARQL.
//
// On `c3443a6`: `web::query_form` skips `PREFIX` by consuming exactly two whitespace-separated words, and
// matches the form keyword as a whole whitespace token. SPARQL needs no whitespace between a
// PNAME_NS and its IRIREF (`PREFIX ex:<urn:x#>`) or after `SELECT` (`SELECT*{…}`), and the
// store parses both — but the protocol face answers 400 "not a SELECT, ASK, CONSTRUCT or
// DESCRIBE query" and the editor page "Not a query form this page runs".
// ------------------------------------------------------------------------------------------
#[test]
fn r3_valid_sparql_is_read_as_its_query_form() {
    let hub = compose(DurableStore::in_memory().unwrap());
    let queries = [
        "PREFIX ex:<urn:x#> SELECT ?s WHERE { ?s ?p ?o }",
        "SELECT*{ ?s ?p ?o }",
    ];
    let mut wrong = Vec::new();
    for query in queries {
        // The store's own parser accepts it (root, over a named graph).
        let store = block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                "urn:iki:store:graph-select",
                &[
                    ("query", query.as_bytes()),
                    ("graph", b"urn:iki:ledger:graph:default"),
                ],
            ),
            &Capability::root(),
        ));
        let form = web::query_form(query);
        if form != Some("select") {
            wrong.push(format!(
                "{query:?}: query_form = {form:?}; the store answered it: {}",
                store.is_ok()
            ));
        }
    }
    // And end to end through the protocol face. ⚠ One item first: `urn:sparql:*` refuses an
    // EMPTY default dataset by design, and a fresh store has no ledger graph yet — the
    // auditor's test met that refusal after `query_form` was fixed, and it is the fixture's,
    // not the defect's.
    let server = Server::start();
    let (status, body) = server.raw("POST", "/iki/ledger/append", &[], "One item");
    assert_eq!(status, 200, "{body}");
    let (status, body) = server.raw(
        "GET",
        "/sparql?query=SELECT*%7B%3Fs%20%3Fp%20%3Fo%7D&as=application%2Fsparql-results%2Bjson",
        &[("Accept", "application/sparql-results+json".to_string())],
        "",
    );
    assert!(
        wrong.is_empty() && status == 200,
        "{wrong:#?}\nGET /sparql?query=SELECT*{{?s ?p ?o}} → {status}: {}",
        body.trim()
    );
}

// ------------------------------------------------------------------------------------------
// Ledger #799: a scratch gonk is named by flags, not smuggled in through the environment.
// `HOME` and `XDG_CONFIG_HOME` point at an "elsewhere" that must stay empty; the server and the
// command that provisions it both take `--config-home` / `--data-home`, and the store follows
// the data home.
// ------------------------------------------------------------------------------------------
#[test]
fn a_scratch_gonk_is_named_by_its_home_flags() {
    use std::io::BufRead;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (config, data, elsewhere) = (root.join("cfg"), root.join("data"), root.join("elsewhere"));
    std::fs::create_dir_all(&elsewhere).unwrap();
    let gonk = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
        command
            .current_dir(&root)
            .env("HOME", &elsewhere)
            .env("XDG_CONFIG_HOME", elsewhere.join("xdg"))
            .stdin(std::process::Stdio::null());
        command
    };
    let homes = [
        "--config-home",
        config.to_str().unwrap(),
        "--data-home",
        data.to_str().unwrap(),
    ];

    let added = gonk()
        .args(["client", "add", "laptop", "--ledger", "default=read"])
        .args(homes)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    assert!(config.join("gonk/clients.json").is_file());

    let mut child = gonk()
        .args([
            "--port",
            "0",
            "--socket",
            "g.sock",
            "--no-quic",
            "--no-backup",
        ])
        .args(homes)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut banner = String::new();
    for line in std::io::BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map_while(std::result::Result::ok)
    {
        banner.push_str(&line);
        banner.push('\n');
        if line.trim_start().starts_with("mount") {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(banner.contains("http://localhost:"), "no banner:\n{banner}");
    assert!(
        data.join("store").is_dir(),
        "the store follows --data-home:\n{banner}"
    );
    let stray: Vec<_> = walk(&elsewhere);
    assert!(
        stray.is_empty(),
        "written outside the named homes: {stray:?}"
    );
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        found.push(entry.path());
        if entry.path().is_dir() {
            found.extend(walk(&entry.path()));
        }
    }
    found
}
