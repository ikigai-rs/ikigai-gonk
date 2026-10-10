//! Ledger [#775](http://localhost:1060/l/default/item/775), step 3: a claim's KIND is the
//! door's, never the caller's — and the doctor and the item page show what the ledger records.
//!
//! `ikigai-ledger` 0.5.0 stamps every claim `machine` or `person` with a function the HOST
//! supplies ([`ikigai_gonk::admit::claim_kind`]): a passkey principal is a person, anything else a
//! machine. That reads the request's `principal`, so it is sound only if every door OVERWRITES
//! that argument. Each test here sends a FORGED passkey principal and a `kind=person` through
//! one door of a REAL gonk (the binary, in a child process, over a scratch config home, data
//! home and socket) and reads back the kind the ledger stored:
//!
//! | door | what the forgery meets | stored |
//! | --- | --- | --- |
//! | socket | `doors::HubSpace::naming_nobody` removes it | machine |
//! | QUIC | `ikigai-quic` replaces it with the connection's client IRI | machine |
//! | HTTP, anonymous | `ikigai-web` drops `?principal=` and stamps none | machine |
//! | HTTP, signed in, mechanical | `ikigai-web` replaces it with the session's passkey | person |
//! | HTTP, signed in, the item page's form | the form adapter forwards the door's | person |

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::Authenticator;
use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{identity, quic};
use ikigai_resolve::Resolver;

const FORGED: &str = "urn:iki:gonk:passkey:Rk9SR0VE";

struct Gonk {
    child: Child,
    http: SocketAddr,
    quic: SocketAddr,
    socket: PathBuf,
    stderr: PathBuf,
    layout: quic::Layout,
    client: quic::Bundle,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Gonk {
    fn start() -> Gonk {
        let home = tempfile::tempdir().unwrap();
        // A Unix socket path must fit `sun_path` (104 bytes on macOS); a scratch dir does not.
        let short = tempfile::Builder::new()
            .prefix("gk")
            .tempdir_in("/tmp")
            .unwrap();
        let config = home.path().join("config");
        let data = home.path().join("data");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let layout = quic::Layout::in_config_home(&config);
        quic::server_identity(&layout).unwrap();
        let client = quic::add_client(&layout, "alpha", None, false).unwrap();
        quic::enrol(
            &layout,
            "alpha",
            &client.fingerprint,
            &grants_for("default", Authority::Write).unwrap(),
            false,
        )
        .unwrap();
        let (port, quic_port) = (free_tcp_port(), free_udp_port());
        let socket = short.path().join("s");
        let stderr = home.path().join("stderr.log");
        let child = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .arg("--config-home")
            .arg(&config)
            .arg("--data-home")
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .args(["--port", &port.to_string()])
            .args(["--quic-bind", &format!("127.0.0.1:{quic_port}")])
            .arg("--no-backup")
            .env("HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .expect("spawn ikigai-gonk");
        let mut gonk = Gonk {
            child,
            http: format!("127.0.0.1:{port}").parse().unwrap(),
            quic: format!("127.0.0.1:{quic_port}").parse().unwrap(),
            socket,
            stderr,
            layout,
            client,
            _dirs: (home, short),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !(gonk.socket.exists() && TcpStream::connect(gonk.http).is_ok()) {
            if let Some(status) = gonk.child.try_wait().unwrap() {
                panic!("gonk DIED starting ({status}):\n{}", gonk.log());
            }
            assert!(
                Instant::now() < deadline,
                "gonk never came up: {}",
                gonk.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        gonk
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.http.port())
    }

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

    /// A page, as a browser navigation of this server's own.
    fn page(&self, path: &str, cookie: Option<&str>) -> (u16, String) {
        let mut headers = vec![
            ("Accept", "text/html".to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
        ];
        if let Some(cookie) = cookie {
            headers.push(("Cookie", format!("{}={cookie}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    /// Enrol a passkey under a grant that writes the default ledger, sign in, return the token.
    fn sign_in(&self) -> String {
        let authenticator = Authenticator::new();
        let invite = identity::invite(
            &self.layout,
            "person",
            &grants_for("default", Authority::Write).unwrap(),
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

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_quic(&self, request: Request) -> Result<Representation, Error> {
        let identity = ikigai_quic::Identity {
            cert_pem: std::fs::read_to_string(self.client.dir.join("client.crt")).unwrap(),
            key_pem: std::fs::read_to_string(self.client.dir.join("client.key")).unwrap(),
        };
        let (server, _) = quic::server_identity(&self.layout).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let client = loop {
            match ikigai_quic::connect(self.quic, &identity, &server.cert_pem) {
                Ok(client) => break client,
                Err(e) if Instant::now() > deadline => panic!("QUIC connect: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        client.issue(request).map(|(repr, _)| repr)
    }

    /// File `n` items over the socket; their numbers are 1..=n.
    fn file(&self, n: usize) {
        for i in 1..=n {
            self.over_socket(request(
                Verb::Sink,
                "urn:iki:ledger:append",
                &[("content", &format!("item {i}"))],
            ))
            .expect("append over the socket");
        }
    }

    /// The stored claim of item `n` (`claim` of the JSON face), read over the socket.
    fn claim(&self, n: usize) -> serde_json::Value {
        let read = self
            .over_socket(request(
                Verb::Source,
                &format!("urn:iki:ledger:item:{n}"),
                &[("as", "application/json")],
            ))
            .expect("the item");
        let doc: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        doc["item"]["claim"].clone()
    }
}

impl Drop for Gonk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
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

/// A forged claim: a passkey principal the caller made up, and a `kind` the ledger does not take.
fn forged_claim(item: usize, holder: &str) -> Request {
    request(
        Verb::Sink,
        "urn:iki:ledger:claim",
        &[
            ("item", &item.to_string()),
            ("content", holder),
            ("principal", FORGED),
            ("kind", "person"),
        ],
    )
}

#[test]
fn no_door_lets_a_caller_choose_its_claim_kind() {
    let gonk = Gonk::start();
    gonk.file(5);
    let token = gonk.sign_in();

    // The socket: the forged principal is removed before the hub sees it.
    gonk.over_socket(forged_claim(1, "via-socket"))
        .expect("claim over the socket");
    assert_eq!(
        gonk.claim(1)["kind"],
        "machine",
        "socket: {}",
        gonk.claim(1)
    );

    // QUIC: ikigai-quic replaces it with the client's own IRI.
    gonk.over_quic(forged_claim(2, "via-quic"))
        .expect("claim over QUIC");
    assert_eq!(gonk.claim(2)["kind"], "machine", "quic: {}", gonk.claim(2));

    // HTTP, anonymous, the mechanical door: `?principal=` and `?kind=` sent as a query.
    let (status, body) = gonk.raw(
        "POST",
        &format!(
            "/iki/ledger/claim?item=3&principal={}&kind=person",
            encode(FORGED)
        ),
        &[("Content-Type", "text/plain".to_string())],
        "via-http",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        gonk.claim(3)["kind"],
        "machine",
        "anonymous http: {}",
        gonk.claim(3)
    );

    // HTTP, signed in, the mechanical door: the forgery is replaced by the session's passkey,
    // so the kind is right for the right reason.
    let (status, body) = gonk.raw(
        "POST",
        &format!("/iki/ledger/claim?item=4&principal={}", encode(FORGED)),
        &[
            ("Content-Type", "text/plain".to_string()),
            ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
        ],
        "via-http-signed-in",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        gonk.claim(4)["kind"],
        "person",
        "signed-in http: {}",
        gonk.claim(4)
    );

    // HTTP, signed in, the item page's own form, with a lease.
    let item5 = gonk.claim_form_target(5);
    let form = [
        ("_ledger", "default"),
        ("_action", "claim"),
        ("_verb", "Sink"),
        ("_then", "card"),
        ("_id", "5"),
        ("item", item5.as_str()),
        ("content", "brian"),
        ("lease", "30m"),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    let (status, body) = gonk.raw(
        "POST",
        "/act",
        &[
            ("Accept", "*/*".to_string()),
            ("Origin", gonk.origin()),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            ("HX-Request", "true".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
        ],
        &form,
    );
    assert_eq!(status, 200, "{body}");
    let claim = gonk.claim(5);
    assert_eq!(claim["kind"], "person", "the form: {claim}");
    assert_eq!(claim["lease"], "PT30M", "{claim}");

    // ★ The item page shows the state and the claim: holder, kind, lease and expiry.
    let (status, page) = gonk.page("/l/default/item/5", None);
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("<dt>State</dt><dd>filed</dd>"), "{page}");
    assert!(
        page.contains("held by brian (person), leased PT30M until "),
        "{page}"
    );

    // ★ The doctor: a MACHINE claim on an item that is not in flight is orphaned; a person's
    // hold is left alone. Items 1–3 are machine claims in `filed`; 4 and 5 are people's.
    let (status, doctor) = gonk.page("/l/default/doctor", None);
    assert_eq!(status, 200, "{doctor}");
    assert!(doctor.contains("3 problem(s)"), "{doctor}");
    for n in 1..=3 {
        assert!(
            doctor.contains(&format!(">#{n}</a>")),
            "#{n} is orphaned: {doctor}"
        );
    }
    assert!(
        !doctor.contains(">#4</a>") && !doctor.contains(">#5</a>"),
        "{doctor}"
    );
    assert!(doctor.contains("orphaned"), "{doctor}");
    // The ledger page links to it.
    let (_, ledger) = gonk.page("/l/default", None);
    assert!(ledger.contains("href='/l/default/doctor'"), "{ledger}");
}

impl Gonk {
    /// Item `n`'s IRI, as the item page's forms carry it in `item`.
    fn claim_form_target(&self, n: usize) -> String {
        let read = self
            .over_socket(request(
                Verb::Source,
                &format!("urn:iki:ledger:item:{n}"),
                &[("as", "application/json")],
            ))
            .expect("the item");
        let doc: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        doc["item"]["iri"].as_str().unwrap().to_string()
    }
}
