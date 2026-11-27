//! Ledger [#4367](http://localhost:1062/l/default/item/4367): a configured public origin for
//! the HTTP door, on its own loopback port, so a person's passkey works through a TLS proxy
//! (`tailscale serve`).
//!
//! Two listeners over one resolver and one relying party, as `serve` runs them: the local
//! door (`localhost`) and the public door (`https://box.tail1.ts.net`), told apart by the port
//! a request arrives on, never by its `Host`.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Site};
use ikigai_gonk::{compose, doors, quic, web};
use ikigai_store::DurableStore;

const ORIGIN: &str = "https://box.tail1.ts.net";
const HOST: &str = "box.tail1.ts.net";

struct Server {
    local: SocketAddr,
    public: SocketAddr,
    layout: quic::Layout,
    passkeys: Arc<Passkeys>,
    _config: tempfile::TempDir,
}

struct Response {
    status: u16,
    head: String,
    body: String,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\n\n{}", self.head, self.body)
    }
}

fn door(bind: SocketAddr, passkeys: &Arc<Passkeys>, public: Option<Site>) -> doors::HttpDoor {
    doors::HttpDoor {
        anonymous: if public.is_some() {
            Vec::new()
        } else {
            grants_for("default", Authority::Read).unwrap()
        },
        bind,
        passkeys: Some(Arc::clone(passkeys)),
        public: Vec::new(),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        public_origin: public,
    }
}

impl Server {
    fn start() -> Server {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bind = || {
            runtime
                .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
                .unwrap()
        };
        let (local_listener, public_listener) = (bind(), bind());
        let local = local_listener.local_addr().unwrap();
        let public = public_listener.local_addr().unwrap();
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
        let passkeys = Arc::new(
            Passkeys::new(layout.clone(), local.port()).with_public_site(Site::public(ORIGIN)),
        );
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            browse_roots: Vec::new().into(),
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
            epochs: None,
            holder_aliases: Default::default(),
            display: Default::default(),
            page_assets: Default::default(),
            projects: Default::default(),
            toolbox: false,
            faces: Vec::new(),
            inquiry: None,
        });
        let http = Arc::new(doors::http_kernel(Arc::clone(&hub), web::space(face)));
        let local_door = door(local, &passkeys, None);
        let public_door = door(public, &passkeys, Some(Site::public(ORIGIN)));
        std::thread::spawn(move || {
            runtime.block_on(async move {
                let a = ikigai_web::serve_with_listener(
                    Arc::clone(&http),
                    doors::http_cap(local_door.clone()),
                    local_listener,
                    doors::edge_config(local_door),
                );
                let b = ikigai_web::serve_with_listener(
                    http,
                    doors::http_cap(public_door.clone()),
                    public_listener,
                    doors::edge_config(public_door),
                );
                tokio::select! { _ = a => {}, _ = b => {} }
            })
        });
        Server {
            local,
            public,
            layout,
            passkeys,
            _config: config,
        }
    }

    fn local_origin(&self) -> String {
        format!("http://localhost:{}", self.local.port())
    }

    fn raw(
        &self,
        to: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> Response {
        let mut stream = TcpStream::connect(to).expect("connect");
        let mut head = format!("{method} {path} HTTP/1.1\r\n");
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
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        Response {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    /// A same-origin JSON post at the public listener, as the proxy forwards it.
    fn public_json(&self, path: &str, body: &str) -> Response {
        self.raw(
            self.public,
            "POST",
            path,
            &[
                ("Host", HOST.to_string()),
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", ORIGIN.to_string()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    fn local_json(&self, path: &str, body: &str) -> Response {
        self.raw(
            self.local,
            "POST",
            path,
            &[
                ("Host", format!("localhost:{}", self.local.port())),
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", self.local_origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    fn get(&self, to: SocketAddr, host: &str, path: &str, cookie: Option<&str>) -> Response {
        let mut headers = vec![
            ("Host", host.to_string()),
            ("Accept", "application/json".to_string()),
        ];
        if let Some(token) = cookie {
            headers.push(("Cookie", format!("{}={token}", identity::SESSION_COOKIE)));
        }
        self.raw(to, "GET", path, &headers, "")
    }
}

fn challenge_of(got: &Response) -> (String, String) {
    assert_eq!(got.status, 200, "{got:?}");
    let v: serde_json::Value = serde_json::from_str(&got.body).unwrap();
    (
        v["challenge"].as_str().unwrap().to_string(),
        v["rpId"].as_str().unwrap().to_string(),
    )
}

fn json(got: &Response) -> serde_json::Value {
    serde_json::from_str(&got.body).unwrap_or_else(|_| panic!("not JSON: {got:?}"))
}

/// The person a laptop's `localhost` passkey makes, with write on `default`.
fn enroll_laptop(server: &Server, laptop: &Authenticator) -> String {
    let scopes = grants_for("default", Authority::Write).unwrap();
    let now = identity::now_seconds();
    let invite = identity::invite(&server.layout, "brian", &scopes, false, 30, now).unwrap();
    let (c, rp) = challenge_of(&server.local_json("/auth/register-options", "{}"));
    assert_eq!(rp, "localhost");
    let got = server.local_json(
        "/auth/register",
        &laptop.register_body(&c, &invite, &server.local_origin()),
    );
    assert_eq!(got.status, 200, "{got:?}");
    let enrolled = identity::read_passkeys(&server.layout).unwrap();
    enrolled.values().next().unwrap().person.clone()
}

/// ★ Reproduced on main (39d56d6): the relying party is `localhost` alone, so a ceremony at
/// the proxy's origin is refused, and the door answers the proxy's `Host` with 421.
#[test]
fn without_a_public_origin_a_proxied_ceremony_fails_and_its_host_is_misdirected() {
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let rp = Passkeys::new(layout.clone(), 1062);
    let now = identity::now_seconds();
    let invite = identity::invite(&layout, "brian", &[], false, 30, now).unwrap();
    let challenge = rp.challenge(Purpose::Register, now).unwrap();
    let phone = Authenticator::named("phone");
    let refused = rp
        .register(
            phone.register_body(&challenge, &invite, ORIGIN).as_bytes(),
            now,
        )
        .unwrap_err();
    assert!(refused.contains(ORIGIN), "{refused}");
    let local = doors::HttpDoor {
        public_origin: None,
        ..door("127.0.0.1:1062".parse().unwrap(), &Arc::new(rp), None)
    };
    let request = ikigai_web::HttpRequest {
        method: "GET".into(),
        headers: vec![("host".into(), HOST.into())],
        path: "/".into(),
        raw_target: "/".into(),
        query: Vec::new(),
        body: Vec::new(),
        peer: Some("127.0.0.1".parse().unwrap()),
    };
    assert_eq!(
        doors::http_refusal(&local, &request),
        Some(ikigai_gonk::admit::REFUSED_FOREIGN_HOST)
    );
}

#[test]
fn the_public_listener_grants_nobody_anonymous_whatever_host_it_is_sent() {
    let server = Server::start();
    // The local listener: anonymous loopback read, exactly as before.
    let host = format!("localhost:{}", server.local.port());
    let got = server.get(
        server.local,
        &host,
        "/iki/ledger/items?ledger=default",
        None,
    );
    assert_eq!(got.status, 200, "{got:?}");
    // The public listener: its own Host reads nothing anonymously.
    let got = server.get(
        server.public,
        HOST,
        "/iki/ledger/items?ledger=default",
        None,
    );
    assert!(
        matches!(got.status, 401 | 403),
        "anonymous at the public origin: {got:?}"
    );
    // `Host: localhost` (with or without the public port) is not a way in: it is misdirected.
    for host in [
        "localhost".to_string(),
        format!("localhost:{}", server.public.port()),
        format!("127.0.0.1:{}", server.public.port()),
        "evil.example".to_string(),
    ] {
        let got = server.get(
            server.public,
            &host,
            "/iki/ledger/items?ledger=default",
            None,
        );
        assert_eq!(got.status, 421, "Host {host}: {got:?}");
    }
    // A write from any other origin, or with no Origin at all, is refused.
    for origin in [
        Some(server.local_origin()),
        Some("https://evil.example".into()),
        None,
    ] {
        let mut headers = vec![
            ("Host", HOST.to_string()),
            ("Content-Type", "application/json".to_string()),
        ];
        if let Some(origin) = &origin {
            headers.push(("Origin", origin.clone()));
        }
        let got = server.raw(server.public, "POST", "/auth/login-options", &headers, "{}");
        assert_eq!(got.status, 403, "Origin {origin:?}: {got:?}");
    }
}

#[test]
fn a_phone_passkey_joins_the_person_at_the_public_relying_party() {
    let server = Server::start();
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    let person = enroll_laptop(&server, &laptop);
    // `passkey invite --person`: an invite that enrolls a new passkey of that person.
    let now = identity::now_seconds();
    let (invite, _, _) = identity::invite_person(&server.layout, &person, 30, now).unwrap();
    let (c, rp) = challenge_of(&server.public_json("/auth/register-options", "{}"));
    assert_eq!(rp, HOST, "the relying party is the origin's whole host");
    let got = server.public_json(
        "/auth/register",
        &phone.register_body_labeled(&c, &invite, ORIGIN, "phone"),
    );
    assert_eq!(got.status, 200, "{got:?}");
    let enrolled = identity::read_passkeys(&server.layout).unwrap();
    let entry = enrolled
        .values()
        .find(|e| e.label == "phone")
        .expect("the phone's passkey");
    assert_eq!(entry.rp_id, HOST, "registration records the public rp_id");
    assert_eq!(entry.person, person, "one person, laptop and phone");

    // It signs in at the public listener, with a `Secure` cookie.
    let (c, rp) = challenge_of(&server.public_json("/auth/login-options", "{}"));
    assert_eq!(rp, HOST);
    let got = server.public_json("/auth/login", &phone.login_body_at(&c, ORIGIN, HOST, 1));
    assert_eq!(got.status, 200, "{got:?}");
    let cookie = got
        .head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .expect("a session cookie");
    assert!(cookie.contains("; Secure"), "{cookie}");
    let token = common::session_cookie(&got.head).expect("the token rides in the cookie");
    assert!(json(&got).get("session").is_none(), "{got:?}");
    // …and its session holds the person's grants there.
    let got = server.get(
        server.public,
        HOST,
        "/iki/ledger/items?ledger=default",
        Some(&token),
    );
    assert_eq!(got.status, 200, "{got:?}");
    let door = door(server.public, &server.passkeys, Some(Site::public(ORIGIN)));
    let request = ikigai_web::HttpRequest {
        method: "GET".into(),
        headers: vec![
            ("host".into(), HOST.into()),
            (
                "cookie".into(),
                format!("{}={token}", identity::SESSION_COOKIE),
            ),
        ],
        path: "/".into(),
        raw_target: "/".into(),
        query: Vec::new(),
        body: Vec::new(),
        peer: Some("127.0.0.1".parse().unwrap()),
    };
    assert_eq!(
        doors::http_principal_of(&door, &request, identity::now_seconds()).as_deref(),
        Some(person.as_str())
    );
    let scopes = doors::http_scopes(&door, &request, identity::now_seconds());
    for grant in grants_for("default", Authority::Write).unwrap() {
        assert!(scopes.contains(&grant), "{grant} in {scopes:?}");
    }

    // The local listener's cookie has no `Secure`, as before.
    let (c, _) = challenge_of(&server.local_json("/auth/login-options", "{}"));
    let got = server.local_json(
        "/auth/login",
        &laptop.login_body(&c, &server.local_origin(), 1),
    );
    assert_eq!(got.status, 200, "{got:?}");
    assert!(!got.head.contains("; Secure"), "{got:?}");

    // A passkey signs in only where it was registered.
    let (c, _) = challenge_of(&server.public_json("/auth/login-options", "{}"));
    let got = server.public_json("/auth/login", &laptop.login_body_at(&c, ORIGIN, HOST, 2));
    assert_eq!(
        got.status, 403,
        "a localhost passkey at the public listener: {got:?}"
    );
    let (c, _) = challenge_of(&server.local_json("/auth/login-options", "{}"));
    let got = server.local_json(
        "/auth/login",
        &phone.login_body_at(&c, &server.local_origin(), "localhost", 2),
    );
    assert_eq!(
        got.status, 403,
        "a public passkey at the local listener: {got:?}"
    );
}

#[test]
fn each_config_error_refuses_the_start() {
    use ikigai_gonk::config::public_http;
    let local: SocketAddr = "127.0.0.1:1062".parse().unwrap();
    let with = |origin: &str, bind: &str| {
        format!("gonk.http.public_origin = \"{origin}\"\ngonk.http.public_bind = \"{bind}\"\n")
    };
    assert_eq!(public_http("", local), Ok(None), "no keys: as before");
    let ok = public_http(&with(ORIGIN, "127.0.0.1:1063"), local)
        .unwrap()
        .unwrap();
    assert_eq!(ok.origin, ORIGIN);
    for (text, says) in [
        (
            format!("gonk.http.public_origin = \"{ORIGIN}\"\n"),
            "without gonk.http.public_bind",
        ),
        (
            "gonk.http.public_bind = \"127.0.0.1:1063\"\n".to_string(),
            "without gonk.http.public_origin",
        ),
        (
            with("http://box.tail1.ts.net", "127.0.0.1:1063"),
            "https://",
        ),
        (with("box.tail1.ts.net", "127.0.0.1:1063"), "https://"),
        (with("https://", "127.0.0.1:1063"), "not an origin"),
        (
            with("https://box.tail1.ts.net/l/default", "127.0.0.1:1063"),
            "not an origin",
        ),
        (
            with("https://user@box.tail1.ts.net", "127.0.0.1:1063"),
            "not an origin",
        ),
        (
            with("https://100.64.0.1", "127.0.0.1:1063"),
            "not an origin",
        ),
        (with(ORIGIN, "0.0.0.0:1063"), "not loopback"),
        (with(ORIGIN, "100.64.0.1:1063"), "not loopback"),
        (with(ORIGIN, "nonsense"), "not an address"),
        (with(ORIGIN, "127.0.0.1:1062"), "port of its own"),
    ] {
        let refused = public_http(&text, local).unwrap_err();
        assert!(refused.contains(says), "{text}: {refused}");
    }
}
