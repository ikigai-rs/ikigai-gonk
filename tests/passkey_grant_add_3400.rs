//! Ledger [#3400](http://localhost:1062/l/default/item/3400): an operator could put an enrolled
//! passkey on another grant only by sending an invite or by editing `clients.json` by hand.
//! `ikigai-gonk passkey grant add` does it from the command line, refuses what it must, and
//! records the widening where an invite's widening is recorded.
//!
//! - [`adding_a_grant_widens_that_passkey_only_and_is_recorded`]
//! - [`an_undefined_or_held_grant_is_refused_with_nothing_written`]
//! - [`the_command_widens_a_live_session_and_the_remove_half_still_works`]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Which};
use ikigai_gonk::quic;

const NOW: u64 = 1_800_000_000;

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
}

fn define(layout: &quic::Layout, grant: &str, authority: Authority) {
    let scopes = grants_for("default", authority).unwrap();
    quic::put_grant(layout, grant, &scopes, false).unwrap();
}

fn enroll(rp: &Passkeys, auth: &Authenticator, code: &str) {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body(&challenge, code, rp.origin()).as_bytes(),
        NOW,
    )
    .expect("enrolls");
}

fn sign_in(rp: &Passkeys, auth: &Authenticator, count: u32) -> (String, identity::Identity) {
    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    rp.login(
        auth.login_body(&challenge, rp.origin(), count).as_bytes(),
        NOW,
    )
    .expect("signs in")
}

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

fn grants_of(layout: &quic::Layout, auth: &Authenticator) -> Vec<String> {
    identity::read_passkeys(layout).unwrap()[&base64_id(auth)]
        .grants
        .clone()
}

fn log(layout: &quic::Layout) -> String {
    std::fs::read_to_string(identity::audit_log(layout)).unwrap_or_default()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Two passkeys under `viewer`, and a `writer` grant defined; the owner adds `writer` to the
/// laptop. The laptop holds both, the phone and `grants.json` are as they were, and one
/// `gonk:PasskeyGrantAdded` line names the passkey, the grant and the owner (no invite).
#[test]
fn adding_a_grant_widens_that_passkey_only_and_is_recorded() {
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

    let log = log(&layout);
    let lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains(identity::GRANT_ADDED))
        .collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(
        lines[0].contains(&format!(
            "{} {} label=software-laptop grant=viewer,writer added=writer by=owner",
            identity::GRANT_ADDED,
            identity::passkey_iri(&base64_id(&laptop))
        )),
        "{log}"
    );
    assert!(!lines[0].contains("invite="), "{log}");

    // The same reader `passkey list` uses sees it as the owner's.
    let changes = identity::grant_changes(&layout).unwrap();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].added);
    assert_eq!(changes[0].grant, "writer");
    assert_eq!(changes[0].invite, None);
    assert_eq!(changes[0].by.as_deref(), Some(identity::BY_OWNER));
}

/// A grant `grants.json` does not define, one it defines with no scopes, one the passkey
/// already holds, and a passkey that does not exist: each is refused, and neither
/// `clients.json` nor `grants.json` nor the log changes.
#[test]
fn an_undefined_or_held_grant_is_refused_with_nothing_written() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    // An empty scope list, written as a hand edit would leave it.
    let mut doc: serde_json::Value = serde_json::from_str(&read(&layout.grants_json())).unwrap();
    doc["hollow"] = serde_json::json!([]);
    std::fs::write(layout.grants_json(), doc.to_string()).unwrap();
    let (clients, grants, before) = (
        read(&layout.clients_json()),
        read(&layout.grants_json()),
        log(&layout),
    );

    let laptop_id = base64_id(&laptop);
    for (which, grant, says) in [
        (Which::Credential(&laptop_id), "nobody", "unknown"),
        (Which::Label("software-laptop"), "hollow", "no scopes"),
        (Which::Label("software-laptop"), "viewer", "already holds"),
        (Which::Label("no-such-passkey"), "viewer", "no passkey"),
    ] {
        let refused = identity::add_grant(&layout, which, grant, 0).unwrap_err();
        assert!(refused.contains(says), "{grant}: {refused}");
        assert_eq!(read(&layout.clients_json()), clients, "{grant}");
        assert_eq!(read(&layout.grants_json()), grants, "{grant}");
        assert_eq!(log(&layout), before, "{grant}");
    }
    assert_eq!(grants_of(&layout, &laptop), ["viewer"]);
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

/// The command, run in another process: a session the laptop opened before it sees the new
/// grant at its next request and is NOT ended; the phone's session is unchanged; `passkey
/// list` shows the owner added it; and `grant remove` (and its alias `remove-grant`) take
/// grants off again.
#[test]
fn the_command_widens_a_live_session_and_the_remove_half_still_works() {
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
    define(&layout, "auditor", Authority::Audit);
    let (session, before) = sign_in(&rp, &laptop, 1);
    let (phone_session, phone_before) = sign_in(&rp, &phone, 1);

    let (ok, out, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "writer"],
    );
    assert!(ok, "{err}");
    assert!(
        out.contains("added grant `writer` to passkey `software-laptop`"),
        "{out}"
    );

    let who = rp
        .identity(&session, NOW)
        .expect("the session was not ended");
    assert_eq!(who.enrolled.grants, ["viewer", "writer"]);
    let writer = &quic::read_grants(&layout.grants_json()).unwrap()["writer"];
    assert!(
        writer.iter().all(|s| who.scopes.contains(s)),
        "{:?}",
        who.scopes
    );
    assert!(
        writer.iter().any(|s| !before.scopes.contains(s)),
        "it widened"
    );
    let other = rp
        .identity(&phone_session, NOW)
        .expect("the phone's session");
    assert_eq!(other.enrolled.grants, ["viewer"]);
    assert_eq!(other.scopes, phone_before.scopes);

    // By credential id, too; and a second add of the same grant is refused.
    let laptop_id = base64_id(&laptop);
    let (ok, _, err) = run(
        &config,
        &[
            "passkey",
            "grant",
            "add",
            "--credential",
            &laptop_id,
            "auditor",
        ],
    );
    assert!(ok, "{err}");
    let (ok, _, err) = run(
        &config,
        &["passkey", "grant", "add", "software-laptop", "writer"],
    );
    assert!(!ok);
    assert!(err.contains("already holds grant `writer`"), "{err}");

    let (ok, listed, err) = run(&config, &["passkey", "list"]);
    assert!(ok, "{err}");
    assert!(
        listed.contains("grants viewer, writer, auditor"),
        "{listed}"
    );
    assert!(
        listed.contains("added grant `writer` by the owner (command line)"),
        "{listed}"
    );

    // The remove half: `grant remove`, and `remove-grant --grant` as its alias.
    let (ok, out, err) = run(
        &config,
        &["passkey", "grant", "remove", "software-laptop", "writer"],
    );
    assert!(ok, "{err}");
    assert!(out.contains("removed grant `writer`"), "{out}");
    let (ok, _, err) = run(
        &config,
        &[
            "passkey",
            "remove-grant",
            "software-laptop",
            "--grant",
            "auditor",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(grants_of(&layout, &laptop), ["viewer"]);
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
    let who = rp.identity(&session, NOW).expect("still signed in");
    assert_eq!(who.enrolled.grants, ["viewer"]);
    let (_, listed, _) = run(&config, &["passkey", "list"]);
    assert!(listed.contains("removed grant `writer`"), "{listed}");
    assert!(listed.contains("removed grant `auditor`"), "{listed}");
}
