//! Ledger [#3816](http://localhost:1062/l/default/item/3816): once a passkey session's write
//! has lapsed (#3815), a fresh assertion by the SAME passkey reopens it, without signing out.
//!
//! - [`a_challenge_of_another_ceremony_is_refused_both_ways`]
//! - [`an_assertion_by_another_passkey_is_refused`]
//! - [`a_reassertion_replaces_only_the_cookies_session_and_reopens_write`]
//! - [`the_session_is_named_by_its_id_when_the_passkey_has_two`]
//! - [`the_audit_line_names_the_reassertion`]

use std::sync::{Arc, Mutex};

mod common;

use common::Authenticator;
use ikigai_gonk::access::{Record, Sink};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, SignedIn, SESSION_SECONDS};
use ikigai_gonk::quic;
use serde_json::Value;

const NOW: u64 = 1_800_000_000;
/// Past the write bound, inside the read bound.
const LAPSED: u64 = NOW + SESSION_SECONDS + 3_600;

struct Rp {
    _home: tempfile::TempDir,
    layout: quic::Layout,
    rp: Passkeys,
    lines: Arc<Mutex<Vec<String>>>,
}

/// A relying party whose sessions read on for 240 h and whose audit lines are collected.
fn relying_party() -> Rp {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let keep = Arc::clone(&lines);
    let sink: Sink = Arc::new(move |l: &str| keep.lock().unwrap().push(l.to_string()));
    let hours = 240 * 3_600;
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW)
        .with_idle_seconds(hours)
        .with_max_seconds(hours)
        .auditing(Record::to(sink));
    Rp {
        _home: home,
        layout,
        rp,
        lines,
    }
}

fn enroll(rp: &Rp, auth: &Authenticator, label: &str) {
    let scopes = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&rp.layout, "writer", &scopes, false, 30, NOW).unwrap();
    let challenge = rp.rp.challenge(Purpose::Register, NOW).unwrap();
    rp.rp
        .register(
            auth.register_body_labeled(&challenge, &code, rp.rp.origin(), label)
                .as_bytes(),
            NOW,
        )
        .expect("enrolls");
}

fn sign_in(rp: &Rp, auth: &Authenticator, count: u32) -> String {
    let challenge = rp.rp.challenge(Purpose::Login, NOW).unwrap();
    rp.rp
        .login(
            auth.login_body(&challenge, rp.rp.origin(), count)
                .as_bytes(),
            NOW,
        )
        .expect("signs in")
        .0
}

/// `auth`'s answer to `challenge`, naming the session `field` = `value`.
fn body(
    rp: &Rp,
    auth: &Authenticator,
    challenge: &str,
    count: u32,
    named: &[(&str, &str)],
) -> String {
    let mut body: Value =
        serde_json::from_str(&auth.login_body(challenge, rp.rp.origin(), count)).unwrap();
    for (field, value) in named {
        body[*field] = (*value).into();
    }
    body.to_string()
}

fn writes(rp: &Rp, token: &str, now: u64) -> bool {
    rp.rp
        .identity(token, now)
        .is_some_and(|who| who.write_seconds(now) > 0)
}

#[test]
fn a_challenge_of_another_ceremony_is_refused_both_ways() {
    let rp = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, "laptop");
    let token = sign_in(&rp, &laptop, 1);

    for purpose in [Purpose::Login, Purpose::AddGrant, Purpose::Register] {
        let challenge = rp.rp.challenge(purpose, LAPSED).unwrap();
        let refused = rp
            .rp
            .reassert(
                body(&rp, &laptop, &challenge, 2, &[("session", &token)]).as_bytes(),
                SignedIn::Principal(None),
                LAPSED,
            )
            .expect_err("another ceremony's challenge");
        assert!(
            refused.contains("another ceremony"),
            "{purpose:?}: {refused}"
        );
    }
    assert!(rp.rp.identity(&token, LAPSED).is_some(), "nothing ended");

    // And the reverse: a re-assertion's challenge signs nobody in and widens nothing.
    let challenge = rp.rp.challenge(Purpose::Reassert, LAPSED).unwrap();
    let login = rp
        .rp
        .login(
            laptop.login_body(&challenge, rp.rp.origin(), 2).as_bytes(),
            LAPSED,
        )
        .expect_err("a re-assertion challenge is no sign-in");
    assert!(login.contains("another ceremony"), "{login}");
    let scopes = grants_for("default", Authority::Read).unwrap();
    let code = identity::invite(&rp.layout, "reader", &scopes, false, 30, NOW).unwrap();
    let widen = rp
        .rp
        .add_grant(
            body(
                &rp,
                &laptop,
                &challenge,
                2,
                &[("session", &token), ("invite", &code)],
            )
            .as_bytes(),
            LAPSED,
        )
        .expect_err("a re-assertion challenge adds no grant");
    assert!(widen.contains("another ceremony"), "{widen}");
}

#[test]
fn an_assertion_by_another_passkey_is_refused() {
    let rp = relying_party();
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, "laptop");
    enroll(&rp, &phone, "phone");
    let token = sign_in(&rp, &laptop, 1);

    let challenge = rp.rp.challenge(Purpose::Reassert, LAPSED).unwrap();
    let refused = rp
        .rp
        .reassert(
            body(&rp, &phone, &challenge, 1, &[("session", &token)]).as_bytes(),
            SignedIn::Principal(None),
            LAPSED,
        )
        .expect_err("another passkey cannot reopen this session");
    assert!(
        refused.contains("not the one you are signed in with"),
        "{refused}"
    );
    assert!(
        rp.rp.identity(&token, LAPSED).is_some(),
        "the session stands"
    );
    assert!(!writes(&rp, &token, LAPSED), "and still only reads");

    // A replayed counter is refused as a sign-in's is.
    let challenge = rp.rp.challenge(Purpose::Reassert, LAPSED).unwrap();
    rp.rp
        .reassert(
            body(&rp, &laptop, &challenge, 1, &[("session", &token)]).as_bytes(),
            SignedIn::Principal(None),
            LAPSED,
        )
        .expect_err("the signature counter did not advance");
}

#[test]
fn a_reassertion_replaces_only_the_cookies_session_and_reopens_write() {
    let rp = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, "laptop");
    let here = sign_in(&rp, &laptop, 1);
    let there = sign_in(&rp, &laptop, 2);
    assert!(!writes(&rp, &here, LAPSED), "write has lapsed");

    // The options pin the session's own passkey.
    let principal = identity::passkey_iri(
        &rp.rp
            .identity(&here, LAPSED)
            .unwrap()
            .enrolled
            .credential_id,
    );
    let (credential, challenge) = rp
        .rp
        .reassert_options(Some(&here), None, SignedIn::Principal(None), LAPSED)
        .unwrap();
    assert_eq!(identity::passkey_iri(&credential), principal);

    let (token, who) = rp
        .rp
        .reassert(
            body(&rp, &laptop, &challenge, 3, &[("session", &here)]).as_bytes(),
            SignedIn::Principal(None),
            LAPSED,
        )
        .expect("the same passkey reopens write");
    assert!(who.write_seconds(LAPSED) > 0, "fresh bounds");
    assert!(
        rp.rp.identity(&here, LAPSED).is_none(),
        "the old cookie no longer works"
    );
    assert!(writes(&rp, &token, LAPSED), "the new one writes");
    assert!(
        rp.rp.identity(&there, LAPSED).is_some(),
        "another session of the same passkey is untouched"
    );
    assert!(!writes(&rp, &there, LAPSED), "and is still as it was");

    // The challenge is single-use.
    rp.rp
        .reassert(
            body(&rp, &laptop, &challenge, 4, &[("session", &token)]).as_bytes(),
            SignedIn::Principal(None),
            LAPSED,
        )
        .expect_err("answered already");
}

/// The HTTP door's page cannot read its HttpOnly cookie, so it names its session by the
/// `sessionId` the sign-in answered; with two sessions and no name, nothing is guessed.
#[test]
fn the_session_is_named_by_its_id_when_the_passkey_has_two() {
    let rp = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, "laptop");
    let here = sign_in(&rp, &laptop, 1);
    let there = sign_in(&rp, &laptop, 2);
    let who = rp.rp.identity(&here, LAPSED).unwrap();
    let principal = identity::passkey_iri(&who.enrolled.credential_id);

    let refused = rp
        .rp
        .reassert_options(None, None, SignedIn::Principal(Some(&principal)), LAPSED)
        .expect_err("two sessions, none named");
    assert!(refused.contains("sessionId"), "{refused}");
    // A session id of another passkey's session (here: no session) is refused.
    rp.rp
        .reassert_options(
            None,
            Some(&"0".repeat(64)),
            SignedIn::Principal(Some(&principal)),
            LAPSED,
        )
        .expect_err("not this passkey's session");

    let id = identity::session_id(&here);
    let (_, challenge) = rp
        .rp
        .reassert_options(
            None,
            Some(&id),
            SignedIn::Principal(Some(&principal)),
            LAPSED,
        )
        .unwrap();
    let (token, _) = rp
        .rp
        .reassert(
            body(&rp, &laptop, &challenge, 3, &[("sessionId", &id)]).as_bytes(),
            SignedIn::Principal(Some(&principal)),
            LAPSED,
        )
        .expect("the named session is replaced");
    assert!(rp.rp.identity(&here, LAPSED).is_none());
    assert!(writes(&rp, &token, LAPSED));
    assert!(rp.rp.identity(&there, LAPSED).is_some());
}

#[test]
fn the_audit_line_names_the_reassertion() {
    let rp = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &laptop, "laptop");
    let here = sign_in(&rp, &laptop, 1);
    let challenge = rp.rp.challenge(Purpose::Reassert, LAPSED).unwrap();
    rp.rp
        .reassert(
            body(&rp, &laptop, &challenge, 2, &[("session", &here)]).as_bytes(),
            SignedIn::Principal(None),
            LAPSED,
        )
        .unwrap();
    let lines = rp.lines.lock().unwrap();
    let login = format!("<{}>", identity::LOGIN_CLASS);
    let line = lines
        .iter()
        .find(|l| l.contains(&login) && l.contains("urn:iki:gonk:passkey:reassert"))
        .unwrap_or_else(|| panic!("no reassert line: {lines:#?}"));
    assert!(line.contains("outcome=ok"), "{line}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("why=reasserted") && l.contains("sessions=1")),
        "the replaced session's end is logged: {lines:#?}"
    );
}
