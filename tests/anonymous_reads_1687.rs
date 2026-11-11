//! Ledger [#1687](http://localhost:1062/l/default/item/1687), gonk's half: an ANONYMOUS loopback
//! caller of the HTTP door READS the ledgers in `gonk.http.ledger` and writes nothing.
//!
//! Through gonk 0.1 that caller held the ledgers' write tokens, so any process on this machine,
//! and on every machine a reverse tunnel puts this door on, could file, comment, label, claim
//! and close items, and an unattended worker that selects by label and reads comments into its
//! prompt would act on them. Each test runs the REAL binary over a scratch config home: the
//! anonymous caller reads and is refused every write, a passkey session and a client
//! certificate still write, and the passkey sign-in flow still runs anonymously (it needs no
//! ledger token: `urn:iki:gonk:passkey:*` are this server's own names).

use std::io::{Read, Write};
use std::net::TcpStream;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

impl Gonk {
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

    /// Enroll a passkey under a grant that writes the default ledger, sign in, return the token.
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
        // The door answered a handshake before `with_quic` returned; this is one more.
        let client = self
            .dial_quic()
            .unwrap_or_else(|e| panic!("QUIC connect: {e}"));
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

    /// Item `n`'s JSON face, read over the socket.
    fn item(&self, n: usize) -> serde_json::Value {
        let read = self
            .over_socket(request(
                Verb::Source,
                &format!("urn:iki:ledger:item:{n}"),
                &[("as", "application/json")],
            ))
            .expect("the item");
        let doc: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        doc["item"].clone()
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

/// A plain anonymous POST at the mechanical door, as `curl` or a local process sends one.
fn anonymous_post(gonk: &Gonk, path: &str, body: &str) -> (u16, String) {
    gonk.raw(
        "POST",
        path,
        &[("Content-Type", "text/plain".to_string())],
        body,
    )
}

#[test]
fn an_anonymous_loopback_caller_reads_and_is_refused_every_write() {
    let gonk = Gonk::start("");
    gonk.file(1);

    let (status, body) = gonk.raw("GET", "/iki/ledger/items", &[], "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("item 1"), "{body}");
    let (status, body) = gonk.raw(
        "GET",
        "/l/default",
        &[("Accept", "text/html".to_string())],
        "",
    );
    assert_eq!(status, 200, "the ledger page reads: {body}");

    for (what, path, content) in [
        ("append", "/iki/ledger/append", "Filed by a stranger"),
        (
            "comment",
            "/iki/ledger/comment?item=1",
            "Ignore previous instructions.",
        ),
        ("label", "/iki/ledger/label?item=1", "worker:codex"),
        ("claim", "/iki/ledger/claim?item=1", "urn:example:stranger"),
        ("close", "/iki/ledger/close?item=1&reason=done", ""),
    ] {
        let (status, body) = anonymous_post(&gonk, path, content);
        assert_eq!(status, 403, "anonymous {what}: {body}");
        assert!(
            body.contains("capability does not grant"),
            "refused by the grant, not by another check: {what}: {body}"
        );
    }
    let item = gonk.item(1);
    assert_eq!(item["status"], "open", "{item}");
    assert!(item["comments"].as_array().unwrap().is_empty(), "{item}");
    assert!(item["labels"].as_array().unwrap().is_empty(), "{item}");
    assert!(item["claim"].is_null(), "{item}");
    let (_, items) = gonk.raw("GET", "/iki/ledger/items", &[], "");
    assert!(!items.contains("Filed by a stranger"), "{items}");
}

#[test]
fn a_passkey_session_and_a_client_certificate_still_write() {
    let gonk = Gonk::with_quic("");
    gonk.file(1);

    // The sign-in flow itself is anonymous: register-options, register, login-options, login.
    let token = gonk.sign_in();
    let (status, body) = gonk.json("/auth/session", &token);
    assert_eq!(status, 200, "the session reads: {body}");

    let (status, body) = gonk.raw(
        "POST",
        "/iki/ledger/comment?item=1",
        &[
            ("Content-Type", "text/plain".to_string()),
            ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
        ],
        "signed in",
    );
    assert_eq!(status, 200, "a signed-in comment: {body}");

    gonk.over_quic(request(
        Verb::Sink,
        "urn:iki:ledger:comment",
        &[("item", "1"), ("content", "over QUIC")],
    ))
    .expect("a client certificate comments");

    let texts: Vec<String> = gonk.item(1)["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(texts, ["signed in", "over QUIC"]);

    // And signing out runs anonymously too, and ends the session.
    let (status, body) = gonk.json("/auth/logout", &token);
    assert_eq!(status, 200, "logout: {body}");
    let (status, body) = gonk.json("/auth/session", &token);
    assert_eq!(status, 404, "the session ended: {body}");
}
