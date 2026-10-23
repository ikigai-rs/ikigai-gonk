//! Ledger [#1824](http://localhost:1062/l/default/item/1824): passkey sessions on the HTTP
//! door, driven over real HTTP with a software authenticator against a scratch gonk (port 0,
//! a tempdir config home).
//!
//! - [`a_revoked_passkeys_session_stays_dead_when_it_is_enrolled_again`]
//! - [`a_revoked_passkeys_session_stays_dead_when_the_file_is_restored`]
//! - [`a_session_for_one_ledger_cannot_act_on_another`]
//! - [`logout_all_ends_every_session_of_one_passkey_and_no_other`]
//! - [`the_operators_logout_all_reaches_the_running_server_through_the_log`]
//! - [`sign_ins_and_sign_outs_are_logged_and_a_token_never_is`]
//! - [`the_logout_command_needs_all_and_leaves_the_passkey_enrolled`]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

struct Server {
    addr: SocketAddr,
    layout: quic::Layout,
    /// Every sign-in and sign-out line the relying party wrote.
    audit: Arc<Mutex<Vec<String>>>,
    _config: tempfile::TempDir,
}

impl Server {
    /// Two ledgers, `default` and `acme`, and an anonymous caller who holds nothing: every
    /// authority a request has here comes from its session.
    fn start() -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let audit = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&audit);
        let passkeys = Arc::new(
            Passkeys::new(layout.clone(), addr.port()).auditing(Arc::new(move |line: &str| {
                into.lock().unwrap().push(line.to_string())
            })),
        );
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string(), "acme".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            toolbox: false,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let door = doors::HttpDoor {
            anonymous: Vec::new(),
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
        Server {
            addr,
            layout,
            audit,
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
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        Response::parse(&response)
    }

    /// A same-origin JSON post, as gonk.js makes one.
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

    /// An htmx form post from this origin, carrying `token` as the session cookie.
    fn form(&self, fields: &[(&str, &str)], token: &str) -> Response {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.raw(
            "POST",
            "/act",
            &[
                ("Accept", "*/*".to_string()),
                ("Origin", self.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
                ("HX-Request", "true".to_string()),
                (
                    "Content-Type",
                    "application/x-www-form-urlencoded".to_string(),
                ),
                ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
            ],
            &body,
        )
    }

    /// File an item in `ledger` as the session `token`.
    fn file(&self, ledger: &str, content: &str, token: &str) -> Response {
        self.form(
            &[
                ("_ledger", ledger),
                ("_action", "append"),
                ("_then", "items"),
                ("content", content),
            ],
            token,
        )
    }

    fn challenge(&self, op: &str) -> String {
        let got = self.json(&format!("/auth/{op}"), "{}");
        assert_eq!(got.status, 200, "{got:?}");
        let v: serde_json::Value = serde_json::from_str(&got.body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    }

    /// Enroll `authenticator` under grant `grant` holding `scopes`, through a fresh invite.
    fn enroll(&self, authenticator: &Authenticator, grant: &str, scopes: &[String]) {
        let invite = identity::invite(
            &self.layout,
            grant,
            scopes,
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
    }

    /// Sign `authenticator` in (its signature counter at `count`) and return the session token.
    fn login(&self, authenticator: &Authenticator, count: u32) -> String {
        let c = self.challenge("login-options");
        let login = self.json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), count),
        );
        assert_eq!(login.status, 200, "{login:?}");
        let v: serde_json::Value = serde_json::from_str(&login.body).unwrap();
        v["session"].as_str().unwrap().to_string()
    }

    /// The `/auth/session` status for `token`: 200 for a live session, 404 for none.
    fn session_status(&self, token: &str) -> u16 {
        self.json("/auth/session", token).status
    }
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl Response {
    fn parse(response: &str) -> Response {
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        Response {
            status,
            head: head.to_string(),
            body: body.to_string(),
        }
    }
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn credential(authenticator: &Authenticator) -> String {
    URL_SAFE_NO_PAD.encode(&authenticator.id)
}

/// ★ **A revoked passkey's session never comes back** (G-06's revoked-session neighbor). On
/// main before this change the session record outlived the revoke in memory, and the door
/// re-read `clients.json` per request: enrolling the same credential again through a new
/// invite made the OLD token admit again, without a new login.
#[test]
fn a_revoked_passkeys_session_stays_dead_when_it_is_enrolled_again() {
    let server = Server::start();
    let scopes = grants_for("default", Authority::Write).unwrap();
    let authenticator = Authenticator::new();
    server.enroll(&authenticator, "brian", &scopes);
    let token = server.login(&authenticator, 1);
    assert_eq!(server.session_status(&token), 200);
    assert_eq!(
        server.file("default", "Before the revoke", &token).status,
        200
    );

    // What `ikigai-gonk passkey revoke --credential <id>` runs, in another process.
    identity::revoke(
        &server.layout,
        identity::Which::Credential(&credential(&authenticator)),
        now_ms(),
    )
    .unwrap();
    assert_eq!(server.session_status(&token), 404, "refused at once");
    let refused = server.file("default", "After the revoke", &token);
    assert_eq!(refused.status, 403, "{refused:?}");

    // The same credential, enrolled again through a fresh invite.
    server.enroll(&authenticator, "brian", &scopes);
    assert_eq!(
        server.session_status(&token),
        404,
        "the session the revoke ended stays ended: enrolling again is not a sign-in"
    );
    let revived = server.file("default", "Revived?", &token);
    assert_eq!(revived.status, 403, "{revived:?}");

    // A NEW sign-in is a new session, and it works.
    let fresh = server.login(&authenticator, 2);
    assert_eq!(server.session_status(&fresh), 200);
    assert_eq!(
        server.file("default", "Signed in again", &fresh).status,
        200
    );
    assert_eq!(server.session_status(&token), 404);
}

/// ★ The other way a revocation could be undone: `clients.json` put back as it was (a restore
/// from a backup, an editor's undo). The passkey may sign in again, which is the operator's
/// decision; the session the revoke ended may not.
#[test]
fn a_revoked_passkeys_session_stays_dead_when_the_file_is_restored() {
    let server = Server::start();
    let scopes = grants_for("default", Authority::Write).unwrap();
    let authenticator = Authenticator::new();
    server.enroll(&authenticator, "brian", &scopes);
    let token = server.login(&authenticator, 1);
    let before = std::fs::read_to_string(server.layout.clients_json()).unwrap();

    identity::revoke(
        &server.layout,
        identity::Which::Credential(&credential(&authenticator)),
        now_ms(),
    )
    .unwrap();
    // Restored BEFORE the door is asked anything: no request saw the passkey missing.
    std::fs::write(server.layout.clients_json(), &before).unwrap();
    assert_eq!(server.session_status(&token), 404);
    let revived = server.file("default", "Revived by a restore?", &token);
    assert_eq!(revived.status, 403, "{revived:?}");
}

/// ★ **A session for one ledger cannot act on another** (G-06's other-ledger neighbor). The
/// session's authority is its grant and only its grant: write on `default` files there and
/// nowhere else, on a server that serves both ledgers.
#[test]
fn a_session_for_one_ledger_cannot_act_on_another() {
    let server = Server::start();
    let authenticator = Authenticator::new();
    server.enroll(
        &authenticator,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    let token = server.login(&authenticator, 1);
    let own = server.file("default", "Mine to file", &token);
    assert_eq!(own.status, 200, "{own:?}");
    let other = server.file("acme", "Not mine to file", &token);
    assert_eq!(other.status, 403, "{other:?}");
    assert!(
        other.body.contains("acme"),
        "the refusal names the ledger: {other:?}"
    );
}

/// ★ **`/auth/logout-all` signs a person out everywhere**: every session of the passkey the
/// token signs in as, and no other passkey's.
#[test]
fn logout_all_ends_every_session_of_one_passkey_and_no_other() {
    let server = Server::start();
    let scopes = grants_for("default", Authority::Write).unwrap();
    let laptop = Authenticator::new();
    let other = Authenticator::named("other");
    server.enroll(&laptop, "brian", &scopes);
    server.enroll(&other, "brian", &scopes);
    let first = server.login(&laptop, 1);
    let second = server.login(&laptop, 2);
    let theirs = server.login(&other, 1);

    let stranger = server.json("/auth/logout-all", "not-a-session");
    assert_eq!(
        stranger.status, 404,
        "a token that is no session ends nothing: {stranger:?}"
    );
    assert_eq!(server.session_status(&first), 200);

    let ended = server.json("/auth/logout-all", &second);
    assert_eq!(ended.status, 200, "{ended:?}");
    assert!(ended.body.contains("\"sessions\":2"), "{ended:?}");
    assert_eq!(server.session_status(&first), 404);
    assert_eq!(server.session_status(&second), 404);
    assert_eq!(
        server.session_status(&theirs),
        200,
        "another passkey's session is untouched"
    );
    assert_eq!(server.file("default", "Still mine", &theirs).status, 200);
}

/// ★ **`ikigai-gonk passkey logout --all` ends a running server's sessions** without a restart
/// and without revoking: it runs in another process, so its only way in is the line it
/// appends to `gonk/passkeys.log`, which the server reads at its next signed-in request.
#[test]
fn the_operators_logout_all_reaches_the_running_server_through_the_log() {
    let server = Server::start();
    let laptop = Authenticator::new();
    server.enroll(
        &laptop,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    let token = server.login(&laptop, 1);
    let ended =
        identity::end_sessions(&server.layout, identity::Which::Label("software"), now_ms())
            .unwrap();
    assert_eq!(ended.credential_id, credential(&laptop));
    assert_eq!(server.session_status(&token), 404);
    let refused = server.file("default", "After logout --all", &token);
    assert_eq!(refused.status, 403, "{refused:?}");
    // Still enrolled: a new sign-in works, and the old line does not end it.
    let fresh = server.login(&laptop, 2);
    assert_eq!(server.session_status(&fresh), 200);
    assert_eq!(
        server.file("default", "Signed in again", &fresh).status,
        200
    );
}

/// ★ **Every sign-in and sign-out is one line**, in the access log's grammar: who (the
/// principal), with what (the credential), and how it came out. A token never appears.
#[test]
fn sign_ins_and_sign_outs_are_logged_and_a_token_never_is() {
    let server = Server::start();
    let laptop = Authenticator::new();
    server.enroll(
        &laptop,
        "brian",
        &grants_for("default", Authority::Write).unwrap(),
    );
    let id = credential(&laptop);
    let iri = identity::passkey_iri(&id);

    // A refused sign-in: an assertion signed by the wrong key, under the enrolled id.
    let impostor = Authenticator {
        key: Authenticator::named("impostor").key,
        id: laptop.id.clone(),
    };
    let c = server.challenge("login-options");
    let refused = server.json("/auth/login", &impostor.login_body(&c, &server.origin(), 1));
    assert_eq!(refused.status, 403, "{refused:?}");

    let token = server.login(&laptop, 1);
    assert_eq!(server.json("/auth/logout", &token).status, 200);
    let again = server.login(&laptop, 2);
    let revoked = identity::revoke(&server.layout, identity::Which::Credential(&id), now_ms());
    assert!(revoked.is_ok());
    assert_eq!(server.session_status(&again), 404);

    let lines = server.audit.lock().unwrap().clone();
    let tail = |line: &str| line.split_once(' ').map(|(_, t)| t.to_string()).unwrap();
    let tails: Vec<String> = lines.iter().map(|l| tail(l)).collect();
    assert_eq!(
        tails,
        vec![
            format!(
                "gonk:Login urn:iki:gonk:passkey:login door=http outcome=denied principal=- \
                 credential={id} sessions=0"
            ),
            format!(
                "gonk:Login urn:iki:gonk:passkey:login door=http outcome=ok principal={iri} \
                 credential={id} sessions=1"
            ),
            format!(
                "gonk:Logout urn:iki:gonk:passkey:logout door=http outcome=ok principal={iri} \
                 credential={id} sessions=1 why=logout"
            ),
            format!(
                "gonk:Login urn:iki:gonk:passkey:login door=http outcome=ok principal={iri} \
                 credential={id} sessions=1"
            ),
            format!(
                "gonk:Logout urn:iki:gonk:passkey:session door=cli outcome=ok principal={iri} \
                 credential={id} sessions=1 why=revoked"
            ),
        ],
        "{lines:#?}"
    );
    for line in &lines {
        // RFC 3339 with milliseconds, UTC: the access log's first column.
        let at = line.split(' ').next().unwrap();
        assert!(at.len() == 24 && at.ends_with('Z'), "{line}");
        assert!(
            !line.contains(&token) && !line.contains(&again),
            "a token leaked: {line}"
        );
    }
}

/// The command itself, against a scratch config home (no server): `--all` is required, the
/// passkey is named as `passkey revoke` names one, and `clients.json` is not touched.
#[test]
fn the_logout_command_needs_all_and_leaves_the_passkey_enrolled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("cfg/gonk")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let clients = root.join("cfg/gonk/clients.json");
    let doc = serde_json::json!({
        "passkeys": {
            "Y3JlZC1hbGljZQ": { "grant": "alice", "label": "alice", "public_key": "AAAA", "sign_count": 3 }
        }
    });
    std::fs::write(&clients, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let before = std::fs::read_to_string(&clients).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--config-home")
            .arg(root.join("cfg"))
            .env("HOME", root.join("home"))
            .output()
            .unwrap()
    };
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();

    let bare = run(&["passkey", "logout", "alice"]);
    assert!(!bare.status.success());
    assert!(
        text(&bare.stderr).contains("--all"),
        "{}",
        text(&bare.stderr)
    );
    assert!(
        !root.join("cfg/gonk/passkeys.log").exists(),
        "nothing was written"
    );

    let unknown = run(&["passkey", "logout", "--all", "bob"]);
    assert!(!unknown.status.success());

    let out = run(&["passkey", "logout", "--all", "alice"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("stays enrolled"),
        "{}",
        text(&out.stdout)
    );
    let log = std::fs::read_to_string(root.join("cfg/gonk/passkeys.log")).unwrap();
    assert!(
        log.ends_with(" gonk:PasskeySessionsEnded urn:iki:gonk:passkey:Y3JlZC1hbGljZQ label=alice grant=alice\n"),
        "{log}"
    );
    assert_eq!(std::fs::read_to_string(&clients).unwrap(), before);
}
