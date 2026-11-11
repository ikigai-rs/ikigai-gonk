//! Ledger [#3414](http://localhost:1062/l/default/item/3414), an audit of the request log
//! (ledger #383): a passkey's sign-in and sign-out entries name the same principal as every
//! door line of its requests, and a passkey no door can name is never enrolled.
//!
//! A credential id is the authenticator's choice, up to 1023 bytes by WebAuthn. Two lengths
//! went wrong:
//!
//! - `a_long_credential_id_signs_in_and_out_under_the_principal_its_requests_carry`: past
//!   256 base64url characters the `gonk:Login` / `gonk:Logout` entry's `principal=` was built
//!   from the CUT `passkey=` column, so it named an IRI that is no passkey's, while the door
//!   lines of the same session named the whole one.
//! - `a_credential_id_too_long_to_be_a_principal_is_refused_at_enrollment`: past core's
//!   principal bound (`MAX_PRINCIPAL_LEN`, 512 bytes, for the whole `urn:iki:gonk:passkey:…`)
//!   the HTTP door cannot mint the name into the capability, so every request of that signed-in
//!   passkey was logged `principal=anon`. Such a passkey is refused now, and its invite is not
//!   spent.

use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

mod common;

use common::Authenticator;
use ikigai_gonk::access::{Record, Sink};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::quic;

const NOW: u64 = 1_800_000_000;

/// A passkey relying party in a scratch config home whose sign-in entries are collected.
fn relying_party() -> (
    tempfile::TempDir,
    quic::Layout,
    Passkeys,
    Arc<Mutex<Vec<String>>>,
) {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let keep = Arc::clone(&lines);
    let sink: Sink = Arc::new(move |l: &str| keep.lock().unwrap().push(l.to_string()));
    let rp = Passkeys::new(layout.clone(), 1062).auditing(Record::to(sink));
    (home, layout, rp, lines)
}

/// An authenticator whose credential id is `bytes` long.
fn with_id_of(bytes: usize) -> Authenticator {
    let mut authenticator = Authenticator::named("long");
    authenticator.id = (0..bytes).map(|i| (i % 251) as u8).collect();
    authenticator
}

fn invite(layout: &quic::Layout) -> String {
    let scopes = grants_for("default", Authority::Write).unwrap();
    identity::invite(layout, "person", &scopes, false, 30, NOW).unwrap()
}

fn enroll(rp: &Passkeys, code: &str, auth: &Authenticator) -> Result<(), String> {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body_labeled(&challenge, code, rp.origin(), "long")
            .as_bytes(),
        NOW,
    )
    .map(|_| ())
}

/// The value of `key` in a segment line.
fn column<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|c| c.strip_prefix(&format!("{key}=")))
}

#[test]
fn a_long_credential_id_signs_in_and_out_under_the_principal_its_requests_carry() {
    let (_home, layout, rp, lines) = relying_party();
    // 300 bytes: 400 base64url characters, past the column cut (256) and inside core's
    // principal bound (512 for the whole IRI).
    let auth = with_id_of(300);
    let id = URL_SAFE_NO_PAD.encode(&auth.id);
    assert!(id.len() > ikigai_gonk::access::MAX_FIELD);
    enroll(&rp, &invite(&layout), &auth).expect("a 300-byte credential id enrolls");

    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    let (token, who) = rp
        .login(auth.login_body(&challenge, rp.origin(), 1).as_bytes(), NOW)
        .expect("it signs in");
    // What the HTTP door mints into each of this session's requests, and so what every door
    // line of them names (`doors::http_cap`).
    let door_principal = identity::passkey_iri(&who.enrolled.credential_id);
    assert_eq!(door_principal, identity::passkey_iri(&id));
    rp.logout(&token);

    let lines = lines.lock().unwrap();
    for class in [identity::LOGIN_CLASS, identity::LOGOUT_CLASS] {
        let line = lines
            .iter()
            .find(|l| l.split(' ').nth(1) == Some(format!("<{class}>").as_str()))
            .unwrap_or_else(|| panic!("no {class} entry: {lines:#?}"));
        assert_eq!(
            column(line, "principal"),
            Some(door_principal.as_str()),
            "{class} names the passkey its requests name: {line}"
        );
    }
}

#[test]
fn a_credential_id_too_long_to_be_a_principal_is_refused_at_enrollment() {
    let (_home, layout, rp, _lines) = relying_party();
    // 400 bytes: 534 base64url characters, so `urn:iki:gonk:passkey:<id>` is past core's bound
    // and cannot be the capability's principal.
    let auth = with_id_of(400);
    let iri = identity::passkey_iri(&URL_SAFE_NO_PAD.encode(&auth.id));
    assert!(
        ikigai_core::Capability::scoped(Vec::<String>::new())
            .with_principal(&iri)
            .is_err(),
        "the premise: no door can name this passkey"
    );
    let code = invite(&layout);
    let refused = enroll(&rp, &code, &auth).expect_err("a passkey no door can name");
    assert!(refused.contains("credential id"), "{refused}");
    assert!(identity::read_passkeys(&layout).unwrap().is_empty());

    // The refusal spent nothing: the same invite enrolls the longest id a door can name.
    let fits = (ikigai_core::MAX_PRINCIPAL_LEN - identity::PASSKEY_IRI_PREFIX.len()) / 4 * 3;
    enroll(&rp, &code, &with_id_of(fits)).expect("the longest nameable id enrolls");
    assert_eq!(identity::read_passkeys(&layout).unwrap().len(), 1);
}
