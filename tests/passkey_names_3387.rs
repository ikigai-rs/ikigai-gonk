//! Ledger [#3387](http://localhost:1062/l/default/item/3387): the system passkey picker showed
//! each passkey by its bare label, so it could not tell two gonks' passkeys (every gonk is the
//! `localhost` relying party) or two grants apart. A passkey is now created with the label and
//! host as its WebAuthn `user.name` and the label and grant as its `user.displayName`, and each
//! sign-in signals the current label and grant where the browser can.
//!
//! The script's half (the names in `navigator.credentials.create()`, their 64-byte bound, the
//! signal on sign-in and silence without it) is run by `tests/browser/passkey_names_3387.mjs`;
//! this holds the server's half over HTTP and pins the script's as WIRING.
//!
//! - [`register_options_names_the_invites_grant_and_spends_nothing`]
//! - [`the_script_names_the_passkey_and_signals_at_sign_in`]

use std::io::{Read, Write};
use std::net::TcpStream;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use scratch_gonk::Gonk;

const JS: &str = include_str!("../web/gonk.js");

fn post(gonk: &Gonk, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nOrigin: http://localhost:{port}\r\n\
         Sec-Fetch-Site: same-origin\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        port = gonk.http.port()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

/// What the display name needs before the authenticator is asked: the grant the invite would
/// enroll under. Asking spends nothing, and an invite that could never enroll is refused first.
#[test]
fn register_options_names_the_invites_grant_and_spends_nothing() {
    let gonk = Gonk::start("");
    let origin = format!("http://localhost:{}", gonk.http.port());
    let scopes = grants_for("default", Authority::Read).unwrap();
    let code = identity::invite(
        &gonk.layout,
        "owner-viewer",
        &scopes,
        false,
        5,
        identity::now_seconds(),
    )
    .unwrap();

    let asked = format!(r#"{{"label":"laptop","invite":"{code}"}}"#);
    let (status, body) = post(&gonk, "/auth/register-options", &asked);
    assert_eq!(status, 200, "{body}");
    let options: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(options["grant"], "owner-viewer", "{body}");
    assert!(
        !body.contains(&code),
        "the answer does not echo the invite: {body}"
    );

    // Asked twice, still good: the invite was read, not redeemed.
    let (status, body) = post(&gonk, "/auth/register-options", &asked);
    assert_eq!(status, 200, "{body}");
    let options: serde_json::Value = serde_json::from_str(&body).unwrap();
    let registration = Authenticator::new().register_body_labeled(
        options["challenge"].as_str().unwrap(),
        &code,
        &origin,
        "laptop",
    );
    let (status, body) = post(&gonk, "/auth/register", &registration);
    assert_eq!(status, 200, "{body}");

    // Spent now, so a second passkey on it is refused before any credential is minted.
    let (status, body) = post(
        &gonk,
        "/auth/register-options",
        &format!(r#"{{"label":"phone","invite":"{code}"}}"#),
    );
    assert!(
        (400..500).contains(&status) && body.contains("this invite is not valid"),
        "{status} {body}"
    );

    // No invite named, no grant in the answer (and no refusal): the 3384 shape still holds.
    let (status, body) = post(&gonk, "/auth/register-options", r#"{"label":"phone"}"#);
    assert_eq!(status, 200, "{body}");
    let options: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(options.get("grant").is_none(), "{body}");
}

/// The script's half, as wiring: what `tests/browser/passkey_names_3387.mjs` runs.
#[test]
fn the_script_names_the_passkey_and_signals_at_sign_in() {
    for wired in [
        // The invite goes to register-options and its grant comes back.
        r#"post("/auth/register-options", { label: name, invite })"#,
        // Creation: label and host, label and grant, each within 64 bytes.
        "user: { id: user, ...passkeyNames(name, grant) }",
        r#"name: named(" @ " + location.host), displayName: named(" · " + grant)"#,
        "const NAME_BYTES = 64;",
        // Sign-in: the signal, feature-detected, its failure swallowed, before the reload.
        "PublicKeyCredential.signalCurrentUserDetails;",
        r#"if (typeof signal !== "function""#,
        "await signalNames(cred, done);\n      location.reload();",
        "userId: toB64(handle),",
        "new Promise((resolve) => setTimeout(resolve, 2000)),",
    ] {
        assert!(JS.contains(wired), "web/gonk.js: {wired}");
    }
}
