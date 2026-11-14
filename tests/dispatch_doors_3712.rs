//! The compact view through a real process's socket, QUIC and signed-in HTTP routes.
mod common;
mod scratch_gonk;

use std::io::{Read, Write};
use std::net::TcpStream;

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;
use serde_json::Value;

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter()
        .fold(Request::new(verb, Iri::parse(iri).unwrap()), |r, (k, v)| {
            r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()))
        })
}
fn http(
    gonk: &Gonk,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: &str,
    face: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).unwrap();
    let host = format!("localhost:{}", gonk.http.port());
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\nAccept: {face}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).unwrap();
    if let Some(session) = session {
        write!(stream, "Cookie: gonk_session={session}\r\n").unwrap();
    }
    write!(stream, "\r\n{body}").unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut rest = body;
        let mut body = String::new();
        while let Some((n, next)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(n, 16).unwrap();
            if n == 0 {
                break;
            }
            body.push_str(&next[..n]);
            rest = &next[n + 2..];
        }
        (status, body)
    } else {
        (status, body.to_string())
    }
}
fn sign_in(gonk: &Gonk) -> String {
    let authenticator = common::Authenticator::new();
    let invite = identity::invite(
        &gonk.layout,
        "dispatcher",
        &grants_for("default", Authority::Write).unwrap(),
        false,
        30,
        identity::now_seconds(),
    )
    .unwrap();
    let json = |op: &str, body: &str| {
        let (status, body) = http(
            gonk,
            "POST",
            &format!("/auth/{op}"),
            None,
            body,
            "application/json",
        );
        assert_eq!(status, 200, "{op}: {body}");
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let origin = format!("http://localhost:{}", gonk.http.port());
    let challenge = json("register-options", "{}");
    json(
        "register",
        &authenticator.register_body(challenge["challenge"].as_str().unwrap(), &invite, &origin),
    );
    let challenge = json("login-options", "{}");
    let session = json(
        "login",
        &authenticator.login_body(challenge["challenge"].as_str().unwrap(), &origin, 1),
    );
    session["session"].as_str().unwrap().to_string()
}

#[test]
fn all_three_doors_share_the_view_and_http_keeps_the_signed_in_reader() {
    let gonk = Gonk::with_quic("");
    let socket = ikigai_ipc::connect(&gonk.socket).unwrap();
    socket
        .issue(request(
            Verb::Sink,
            "urn:iki:ledger:default:append",
            &[("content", "A dispatchable item")],
        ))
        .unwrap();
    let view = "urn:iki:gonk:dispatch:default";
    let ack = "urn:iki:gonk:dispatch:default:ack";
    let (plain, _) = socket
        .issue(request(
            Verb::Source,
            view,
            &[("reader", "socket"), ("item", "1")],
        ))
        .unwrap();
    assert_eq!(plain.repr_type.media_type, "text/plain");
    assert!(String::from_utf8_lossy(&plain.bytes).contains("#1 created"));
    let quic = gonk.dial_quic().unwrap();
    let (first, _) = quic
        .issue(request(Verb::Source, view, &[("as", "application/json")]))
        .unwrap();
    let first: Value = serde_json::from_slice(&first.bytes).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    quic.issue(request(
        Verb::Sink,
        ack,
        &[("content", &first["acknowledge"].to_string())],
    ))
    .unwrap();
    let (empty, _) = quic
        .issue(request(Verb::Source, view, &[("as", "application/json")]))
        .unwrap();
    assert!(
        serde_json::from_slice::<Value>(&empty.bytes).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let (status, _) = http(
        &gonk,
        "GET",
        "/l/default/dispatch?reader=forged",
        None,
        "",
        "application/json",
    );
    assert_eq!(
        status, 400,
        "an anonymous caller cannot invent a cursor identity"
    );
    let session = sign_in(&gonk);
    let (status, body) = http(
        &gonk,
        "GET",
        "/l/default/dispatch?item=1",
        Some(&session),
        "",
        "application/json",
    );
    assert_eq!(status, 200, "{body}");
    let doc: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        doc["events"].as_array().unwrap().len(),
        1,
        "the HTTP principal has its own cursor"
    );
    assert!(doc["reader"]
        .as_str()
        .unwrap()
        .starts_with("urn:iki:gonk:passkey:"));
    let payload = doc["acknowledge"].to_string();
    let (status, body) = http(
        &gonk,
        "POST",
        "/l/default/dispatch/ack?item=1",
        Some(&session),
        &payload,
        "application/json",
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = http(
        &gonk,
        "GET",
        "/l/default/dispatch?item=1",
        Some(&session),
        "",
        "application/json",
    );
    assert_eq!(status, 200, "{body}");
    assert!(serde_json::from_str::<Value>(&body).unwrap()["events"]
        .as_array()
        .unwrap()
        .is_empty());
    let (_, body) = http(
        &gonk,
        "GET",
        "/l/default/dispatch",
        Some(&session),
        "",
        "text/plain",
    );
    assert!(
        body.contains("#1 created"),
        "ledger scope stays independent: {body}"
    );
}
