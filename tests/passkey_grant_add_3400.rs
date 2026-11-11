//! Ledger [#3400](http://localhost:1062/l/default/item/3400): after #3388 a passkey holds a list
//! of grants, but the only way to add one was an invite redeemed while signed in, or editing
//! `clients.json` by hand. `ikigai-gonk passkey grant add` adds one grant to one passkey from
//! the command line, recorded as the same widening an invite writes, `by=owner`; `passkey
//! grant remove` is its other half, and `passkey remove-grant` still works as its alias.
//!
//! - [`adding_a_grant_widens_that_passkey_only_and_records_it`]
//! - [`an_undefined_or_held_grant_is_refused_with_nothing_written`]
//! - [`the_command_line_adds_lists_and_removes`]
//! - [`a_live_session_holds_the_added_grant_at_its_next_request`]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Which};
use ikigai_gonk::quic;
use scratch_gonk::Gonk;
use serde_json::Value;

const NOW: u64 = 1_800_000_000;

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
}

/// Write `grant` into `grants.json` with `authority` over `default`, as `passkey invite` would.
fn define(layout: &quic::Layout, grant: &str, authority: Authority) {
    invite(layout, grant, authority);
}

fn enroll(rp: &Passkeys, auth: &Authenticator, code: &str) {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body(&challenge, code, rp.origin()).as_bytes(),
        NOW,
    )
    .expect("enrolls");
}

fn grants_of(layout: &quic::Layout, auth: &Authenticator) -> Vec<String> {
    identity::read_passkeys(layout).unwrap()[&base64_id(auth)]
        .grants
        .clone()
}

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

fn log(layout: &quic::Layout) -> String {
    std::fs::read_to_string(identity::audit_log(layout)).unwrap_or_default()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn run(config: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .arg("--config-home")
        .arg(config)
        .env("HOME", config)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Two passkeys under `viewer`; `writer` is added to one. That one holds both, the other and
/// `grants.json` are exactly as they were, and the widening is the #3388 line, `by=owner`.
#[test]
fn adding_a_grant_widens_that_passkey_only_and_records_it() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    enroll(&rp, &phone, &invite(&layout, "viewer", Authority::Read));
    define(&layout, "writer", Authority::Write);
    let grants = read(&layout.grants_json());

    let now = identity::add_grant(&layout, Which::Label("software-laptop"), "writer", 0).unwrap();
    assert_eq!(now.grants, ["viewer", "writer"]);
    assert_eq!(grants_of(&layout, &laptop), ["viewer", "writer"]);
    assert_eq!(grants_of(&layout, &phone), ["viewer"], "the other passkey");
    assert_eq!(read(&layout.grants_json()), grants, "no grant was edited");

    let laptop_id = base64_id(&laptop);
    assert!(
        log(&layout).contains(&format!(
            "{} {} label=software-laptop grant=viewer,writer added=writer by=owner\n",
            identity::GRANT_ADDED,
            identity::passkey_iri(&laptop_id)
        )),
        "{}",
        log(&layout)
    );
    let changes = identity::grant_changes(&layout).unwrap();
    let [change] = changes.as_slice() else {
        panic!("one change: {changes:?}")
    };
    assert_eq!(change.credential_id, laptop_id);
    assert!(change.added && change.by_owner && change.invite.is_none());
    assert_eq!(change.grant, "writer");

    // By credential id too, as `passkey revoke` names one.
    define(&layout, "reviewer", Authority::Read);
    let phone_id = base64_id(&phone);
    identity::add_grant(&layout, Which::Credential(&phone_id), "reviewer", 0).unwrap();
    assert_eq!(grants_of(&layout, &phone), ["viewer", "reviewer"]);
    assert_eq!(grants_of(&layout, &laptop), ["viewer", "writer"]);
}

/// A grant `grants.json` does not define, one it gives no scopes, and one the passkey already
/// holds are each refused, and neither `clients.json` nor the log changes.
#[test]
fn an_undefined_or_held_grant_is_refused_with_nothing_written() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    // `empty` is in grants.json with no scopes: defined, but it grants nothing.
    let mut doc: serde_json::Map<String, Value> =
        serde_json::from_str(&read(&layout.grants_json())).unwrap();
    doc.insert("empty".to_string(), Value::Array(Vec::new()));
    std::fs::write(
        layout.grants_json(),
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let (clients, logged) = (read(&layout.clients_json()), log(&layout));

    let which = Which::Label("software-laptop");
    let refused = identity::add_grant(&layout, which, "nosuch", 0).unwrap_err();
    assert!(refused.contains("unknown or grants no scopes"), "{refused}");
    let refused = identity::add_grant(&layout, which, "empty", 0).unwrap_err();
    assert!(refused.contains("nothing was written"), "{refused}");
    let refused = identity::add_grant(&layout, which, "viewer", 0).unwrap_err();
    assert!(
        refused.contains("already holds grant `viewer`"),
        "{refused}"
    );
    let refused = identity::add_grant(&layout, Which::Label("nobody"), "viewer", 0).unwrap_err();
    assert!(refused.contains("no passkey is labeled"), "{refused}");

    assert_eq!(
        read(&layout.clients_json()),
        clients,
        "clients.json unchanged"
    );
    assert_eq!(log(&layout), logged, "nothing recorded");
}

/// `passkey grant add`, `passkey list`, `passkey grant remove` and the `remove-grant` alias,
/// as the operator types them.
#[test]
fn the_command_line_adds_lists_and_removes() {
    let home = tempfile::tempdir().unwrap();
    let config: PathBuf = home.path().canonicalize().unwrap();
    let layout = quic::Layout::in_config_home(&config);
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    enroll(&rp, &phone, &invite(&layout, "viewer", Authority::Read));
    define(&layout, "writer", Authority::Write);
    define(&layout, "reviewer", Authority::Read);

    let (ok, out, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "writer"],
    );
    assert!(ok, "{err}");
    assert!(out.contains("added grant `writer`"), "{out}");
    let phone_id = base64_id(&phone);
    let (ok, _, err) = run(
        &config,
        &[
            "passkey",
            "grant",
            "add",
            "--credential",
            &phone_id,
            "reviewer",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(grants_of(&layout, &laptop), ["viewer", "writer"]);
    assert_eq!(grants_of(&layout, &phone), ["viewer", "reviewer"]);

    let (ok, _, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "writer"],
    );
    assert!(!ok, "a held grant is refused");
    assert!(err.contains("already holds grant `writer`"), "{err}");
    let (ok, _, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "nosuch"],
    );
    assert!(!ok, "an undefined grant is refused");
    assert!(err.contains("nosuch"), "{err}");

    let (ok, listed, err) = run(&config, &["passkey", "list"]);
    assert!(ok, "{err}");
    let laptop_at = listed.find("software-laptop").unwrap();
    let phone_at = listed.find("software-phone").unwrap();
    let added = listed
        .find("added grant `writer` by the owner (command line)")
        .unwrap_or_else(|| panic!("the owner's widening is listed: {listed}"));
    assert!(
        added > laptop_at && (phone_at < laptop_at || added < phone_at),
        "listed under the laptop passkey: {listed}"
    );
    assert!(listed.contains("grants viewer, writer"), "{listed}");

    // The remove half: `grant remove`, and `remove-grant` as its alias.
    let (ok, out, err) = run(
        &config,
        &["passkey", "grant", "remove", "software-laptop", "viewer"],
    );
    assert!(ok, "{err}");
    assert!(out.contains("removed grant `viewer`"), "{out}");
    assert_eq!(grants_of(&layout, &laptop), ["writer"]);
    let (ok, _, err) = run(
        &config,
        &[
            "passkey",
            "remove-grant",
            "software-phone",
            "--grant",
            "reviewer",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
    let (_, listed, _) = run(&config, &["passkey", "list"]);
    assert!(listed.contains("removed grant `viewer`"), "{listed}");
    assert!(listed.contains("removed grant `reviewer`"), "{listed}");

    let (ok, _, err) = run(&config, &["passkey", "grant", "add", "software-laptop"]);
    assert!(!ok);
    assert!(err.contains("expected <label> <grant>"), "{err}");
}

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

/// A running server, a signed-in session, and `passkey grant add` from ANOTHER process: the
/// same session (not ended, not replaced) holds the new grant at its very next request.
#[test]
fn a_live_session_holds_the_added_grant_at_its_next_request() {
    let gonk = Gonk::start("");
    let config = gonk.layout.dir().parent().unwrap().to_path_buf();
    let origin = format!("http://localhost:{}", gonk.http.port());
    let now = identity::now_seconds();
    let read_scopes = grants_for("default", Authority::Read).unwrap();
    let code = identity::invite(&gonk.layout, "viewer", &read_scopes, false, 5, now).unwrap();
    let laptop = Authenticator::named("laptop");
    let (_, options) = post(&gonk, "/auth/register-options", "{}");
    let challenge: Value = serde_json::from_str(&options).unwrap();
    let (status, body) = post(
        &gonk,
        "/auth/register",
        &laptop.register_body(challenge["challenge"].as_str().unwrap(), &code, &origin),
    );
    assert_eq!(status, 200, "{body}");
    let (_, options) = post(&gonk, "/auth/login-options", "{}");
    let challenge: Value = serde_json::from_str(&options).unwrap();
    let (status, body) = post(
        &gonk,
        "/auth/login",
        &laptop.login_body(challenge["challenge"].as_str().unwrap(), &origin, 1),
    );
    assert_eq!(status, 200, "{body}");
    let session = serde_json::from_str::<Value>(&body).unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = post(&gonk, "/auth/session", &session);
    assert_eq!(status, 200, "{body}");
    let who: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(who["grants"], serde_json::json!(["viewer"]), "{body}");

    let write = grants_for("default", Authority::Write).unwrap();
    identity::invite(&gonk.layout, "writer", &write, false, 5, now).unwrap();
    let (ok, out, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "writer"],
    );
    assert!(ok, "{out}{err}");

    let (status, body) = post(&gonk, "/auth/session", &session);
    assert_eq!(status, 200, "the session was not ended: {body}");
    let who: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        who["grants"],
        serde_json::json!(["viewer", "writer"]),
        "{body}"
    );
    assert!(log(&gonk.layout).contains("added=writer by=owner"));
}
