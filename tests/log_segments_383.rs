//! Ledger [#383](http://localhost:1062/l/default/item/383): gonk keeps ONE record of who did
//! what, an `ikigai-log` segment in its data home, and every door writes its request into it
//! attributed to the caller THAT door authenticated. A real gonk (the binary, in a child
//! process, over a scratch config home), driven through each door.
//!
//! - every request is one `log:Access` entry in `gonk/log/`, and none is on stderr any more;
//! - the socket's is the owner, the QUIC door's the client its certificate named, a signed-in
//!   HTTP request's the passkey, and an anonymous one's `anon` — explicitly, never absent;
//! - a principal the caller sends is never the one written;
//! - `urn:log:*` answers the owner's socket and is refused (`403`) on the HTTP door, signed in
//!   or not, and refused on the QUIC door: a segment holds every principal and item IRI;
//! - the segment verifies: its hash chain is whole.

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

/// One raw HTTP request: the status and the body.
fn raw(
    gonk: &Gonk,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\n",
        gonk.http.port()
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
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}

fn origin(gonk: &Gonk) -> String {
    format!("http://localhost:{}", gonk.http.port())
}

fn json(gonk: &Gonk, path: &str, body: &str) -> (u16, String) {
    raw(
        gonk,
        "POST",
        path,
        &[
            ("Accept", "application/json".to_string()),
            ("Content-Type", "application/json".to_string()),
            ("Origin", origin(gonk)),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ],
        body,
    )
}

/// A GET as a browser navigation, with the session cookie when `token` is given.
fn get(gonk: &Gonk, path: &str, token: Option<&str>) -> (u16, String) {
    let mut headers = vec![
        ("Accept", "text/plain, text/html".to_string()),
        ("Sec-Fetch-Site", "none".to_string()),
    ];
    if let Some(token) = token {
        headers.push(("Cookie", format!("{}={token}", identity::SESSION_COOKIE)));
    }
    raw(gonk, "GET", path, &headers, "")
}

/// Enroll a passkey under a grant that writes the default ledger, sign in, return the token.
fn sign_in(gonk: &Gonk) -> String {
    let authenticator = Authenticator::new();
    let invite = identity::invite(
        &gonk.layout,
        "person",
        &grants_for("default", Authority::Write).unwrap(),
        false,
        30,
        identity::now_seconds(),
    )
    .unwrap();
    let challenge = |op: &str| {
        let (status, body) = json(gonk, &format!("/auth/{op}"), "{}");
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["challenge"].as_str().unwrap().to_string()
    };
    let c = challenge("register-options");
    let (status, body) = json(
        gonk,
        "/auth/register",
        &authenticator.register_body(&c, &invite, &origin(gonk)),
    );
    assert_eq!(status, 200, "{body}");
    let c = challenge("login-options");
    let (status, body) = json(
        gonk,
        "/auth/login",
        &authenticator.login_body(&c, &origin(gonk), 1),
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["session"].as_str().unwrap().to_string()
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).unwrap());
    for (k, v) in args {
        request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
    }
    request
}

fn over_socket(gonk: &Gonk, request: Request) -> Result<Representation, Error> {
    let client =
        ikigai_ipc::connect(&gonk.socket).map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
    client.issue(request).map(|(repr, _)| repr)
}

/// The `log:Access` entries about `subject`, in the order they were written.
fn door_lines(gonk: &Gonk, subject: &str) -> Vec<String> {
    gonk.segments()
        .lines()
        .filter(|l| l.contains(&format!(" log:Access {subject} ")))
        .map(str::to_string)
        .collect()
}

/// The value of `key` in `line`.
fn column<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|c| c.strip_prefix(&format!("{key}=")))
}

#[test]
fn every_door_writes_its_caller_into_the_segment_and_nothing_onto_stderr() {
    let gonk = Gonk::with_quic("");
    const FORGED: &str = "urn:iki:gonk:passkey:Rk9SR0VE";
    let page = "urn:iki:gonk:page:ledger:default";

    // HTTP, anonymous, typing a principal it is not.
    let (status, _) = get(&gonk, &format!("/l/default?principal={FORGED}"), None);
    assert_eq!(status, 200);
    // HTTP, signed in.
    let token = sign_in(&gonk);
    let (status, _) = get(&gonk, "/l/default?status=all", Some(&token));
    assert_eq!(status, 200);
    // The socket, naming a principal of its own.
    over_socket(
        &gonk,
        request(
            Verb::Source,
            "urn:iki:ledger:items",
            &[("principal", FORGED)],
        ),
    )
    .expect("the owner reads the items");
    // QUIC, as the enrolled client.
    let quic = gonk.dial_quic().expect("a QUIC connection");
    quic.issue(request(
        Verb::Source,
        "urn:iki:ledger:items",
        &[("principal", FORGED)],
    ))
    .expect("the client reads the items");

    let http = door_lines(&gonk, page);
    assert_eq!(http.len(), 2, "{http:#?}");
    assert_eq!(column(&http[0], "door"), Some("http"));
    assert_eq!(column(&http[0], "principal"), Some("anon"), "{}", http[0]);
    assert_eq!(
        column(&http[0], "credential"),
        Some("https://ikigai-rs.dev/ns/log#anonymous"),
        "{}",
        http[0]
    );
    let passkey = column(&http[1], "principal").unwrap();
    assert!(
        passkey.starts_with(identity::PASSKEY_IRI_PREFIX),
        "{}",
        http[1]
    );
    assert_eq!(
        column(&http[1], "credential"),
        Some("https://ikigai-rs.dev/ns/log#passkey"),
        "{}",
        http[1]
    );

    let items = door_lines(&gonk, "urn:iki:ledger:items");
    let socket = items
        .iter()
        .find(|l| column(l, "door") == Some("socket"))
        .unwrap_or_else(|| panic!("no socket line: {items:#?}"));
    assert_eq!(
        column(socket, "principal"),
        Some(ikigai_gonk::access::OWNER_PRINCIPAL)
    );
    assert_eq!(
        column(socket, "credential"),
        Some("https://ikigai-rs.dev/ns/log#socketOwner")
    );
    let client = items
        .iter()
        .find(|l| column(l, "door") == Some("quic"))
        .unwrap_or_else(|| panic!("no QUIC line: {items:#?}"));
    assert!(
        column(client, "principal")
            .unwrap()
            .starts_with("urn:iki:host:gonk:client:"),
        "{client}"
    );
    assert_eq!(
        column(client, "credential"),
        Some("https://ikigai-rs.dev/ns/log#clientCertificate")
    );

    let segments = gonk.segments();
    assert!(
        !segments.contains(FORGED) && !segments.contains("Rk9SR0VE"),
        "a forged principal was written"
    );
    assert!(!segments.contains(&token), "a session token was written");
    // The sign-in is in the same segment, under the passkey it authenticated.
    assert!(
        segments
            .lines()
            .any(|l| l.contains("urn:iki:gonk:passkey:login") && l.contains(passkey)),
        "{segments}"
    );
    // ONE record: stderr carries the startup banner, and no request at all.
    let stderr = gonk.log();
    assert!(
        !stderr.contains("Access ") && !stderr.contains(page),
        "a request reached stderr: {stderr}"
    );
}

#[test]
fn the_log_is_the_owners_and_refused_at_every_network_door() {
    let gonk = Gonk::with_quic("");
    // Something to have logged first.
    let (status, _) = get(&gonk, "/l/default", None);
    assert_eq!(status, 200);

    // The owner reads the segment list and verifies the chain over the socket.
    let segments = over_socket(&gonk, request(Verb::Source, "urn:log:segments", &[]))
        .expect("the owner lists the segments");
    assert!(!segments.bytes.is_empty());
    let verify = over_socket(&gonk, request(Verb::Source, "urn:log:verify", &[]))
        .expect("the owner verifies the chain");
    let report = String::from_utf8_lossy(&verify.bytes).into_owned();
    assert!(
        report.contains(" OK entries=") && report.contains(", 0 broken"),
        "the chain verifies: {report}"
    );

    // HTTP, anonymous and signed in: 403, never the segment.
    let token = sign_in(&gonk);
    for path in ["/log/segments", "/log/verify", "/log/config"] {
        for who in [None, Some(token.as_str())] {
            let (status, body) = get(&gonk, path, who);
            assert_eq!(status, 403, "{path} as {who:?}: {body}");
            assert!(!body.contains("log:Access"), "{body}");
        }
    }
    // A write to the record over HTTP is refused as well.
    let (status, _) = raw(
        &gonk,
        "POST",
        "/log/write",
        &[
            ("Origin", origin(&gonk)),
            ("Sec-Fetch-Site", "same-origin".to_string()),
            ("Cookie", format!("{}={token}", identity::SESSION_COOKIE)),
        ],
        "",
    );
    assert!(status == 403 || status == 400, "{status}");

    // QUIC, as an enrolled client: refused.
    let quic = gonk.dial_quic().expect("a QUIC connection");
    let refused = quic
        .issue(request(Verb::Source, "urn:log:segments", &[]))
        .map(|(repr, _)| repr);
    assert!(
        matches!(refused, Err(Error::Denied(_))),
        "the QUIC door reads no segment: {refused:?}"
    );
}
