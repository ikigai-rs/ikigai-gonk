//! Ledger [#3816](http://localhost:1062/l/default/item/3816): once a passkey session's write has
//! lapsed (ledger #3815), a fresh assertion by the SAME passkey re-opens it, without signing out
//! or choosing another passkey: `reassert-options` and `reassert`, a ceremony of their own.
//!
//! - [`no_other_ceremony_s_challenge_is_spent_on_a_reassertion_nor_the_reverse`]
//! - [`an_assertion_by_another_passkey_is_refused`]
//! - [`a_stale_signature_counter_is_refused`]
//! - [`a_reassertion_replaces_that_session_only_and_writes_again`]
//! - [`the_audit_line_names_the_reassertion`]
//! - [`the_cookie_names_the_session_over_http`]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::access::{Record, Sink};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::quic;
use scratch_gonk::Gonk;
use serde_json::{json, Value};

const NOW: u64 = 1_800_000_000;

/// 240 hours: long enough that a session outlives its 12 h of write and only reads.
const HOURS_240: u64 = 240 * 3_600;

fn relying_party() -> (tempfile::TempDir, quic::Layout, Passkeys) {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW)
        .with_idle_seconds(HOURS_240)
        .with_max_seconds(HOURS_240);
    (home, layout, rp)
}

fn enroll(rp: &Passkeys, layout: &quic::Layout, auth: &Authenticator, grant: &str) {
    let scopes = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap();
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body(&challenge, &code, rp.origin())
            .as_bytes(),
        NOW,
    )
    .expect("enrolls");
}

fn sign_in(rp: &Passkeys, auth: &Authenticator, count: u32) -> String {
    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    rp.login(
        auth.login_body(&challenge, rp.origin(), count).as_bytes(),
        NOW,
    )
    .expect("signs in")
    .0
}

/// What a script posts to `reassert`: `auth`'s answer to `challenge`, naming `session`.
fn reassert_body(auth: &Authenticator, challenge: &str, count: u32, session: &str) -> String {
    let mut body: Value =
        serde_json::from_str(&auth.login_body(challenge, "http://localhost:1062", count)).unwrap();
    body["session"] = session.into();
    body.to_string()
}

fn writes(scopes: &[String]) -> bool {
    !identity::read_only(scopes)
}

#[test]
fn no_other_ceremony_s_challenge_is_spent_on_a_reassertion_nor_the_reverse() {
    let (_home, layout, rp) = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &layout, &laptop, "person");
    let session = sign_in(&rp, &laptop, 1);

    // A sign-in's and an add-grant's challenge, answered by the right passkey, re-assert nothing.
    for purpose in [Purpose::Login, Purpose::AddGrant, Purpose::Register] {
        let challenge = rp.challenge(purpose, NOW).unwrap();
        let refused = rp
            .reassert(
                reassert_body(&laptop, &challenge, 2, &session).as_bytes(),
                NOW,
            )
            .expect_err("another ceremony's challenge");
        assert!(
            refused.contains("another ceremony"),
            "{purpose:?}: {refused}"
        );
    }
    assert!(rp.identity(&session, NOW).is_some(), "the session stands");

    // And a re-assertion's challenge signs nobody in and widens nothing.
    let challenge = rp.challenge(Purpose::Reassert, NOW).unwrap();
    let refused = rp
        .login(
            laptop.login_body(&challenge, rp.origin(), 2).as_bytes(),
            NOW,
        )
        .expect_err("a re-assertion's challenge is no sign-in");
    assert!(refused.contains("another ceremony"), "{refused}");
    let scopes = grants_for("default", Authority::Read).unwrap();
    let code = identity::invite(&layout, "more", &scopes, false, 30, NOW).unwrap();
    let mut widen: Value =
        serde_json::from_str(&laptop.login_body(&challenge, rp.origin(), 2)).unwrap();
    widen["session"] = session.as_str().into();
    widen["invite"] = code.into();
    let refused = rp
        .add_grant(widen.to_string().as_bytes(), NOW)
        .expect_err("a re-assertion's challenge adds no grant");
    assert!(refused.contains("another ceremony"), "{refused}");

    // Its own ceremony's challenge does it.
    let (fresh, _) = rp
        .reassert(
            reassert_body(&laptop, &challenge, 2, &session).as_bytes(),
            NOW,
        )
        .expect("its own challenge re-asserts");
    assert!(rp.identity(&fresh, NOW).is_some());
}

#[test]
fn an_assertion_by_another_passkey_is_refused() {
    let (_home, layout, rp) = relying_party();
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &layout, &laptop, "person");
    enroll(&rp, &layout, &phone, "person");
    let session = sign_in(&rp, &laptop, 1);
    let later = NOW + identity::SESSION_SECONDS;

    let (challenge, credential) = rp
        .reassert_options(&identity::session_key(&session), later)
        .expect("a live session may ask");
    assert_eq!(
        rp.identity(&session, later).unwrap().enrolled.credential_id,
        credential,
        "the options name the session's own passkey"
    );
    // Another passkey under the very same grant: refused, so whom later writes are attributed
    // to cannot change.
    let refused = rp
        .reassert(
            reassert_body(&phone, &challenge, 1, &session).as_bytes(),
            later,
        )
        .expect_err("another passkey's assertion");
    assert!(refused.contains("not the one"), "{refused}");
    let who = rp.identity(&session, later).expect("the session stands");
    assert!(!writes(&who.scopes), "and its write is still lapsed");

    // No session, or one that is not live: refused before any ceremony.
    assert!(rp.reassert_options("", later).is_err());
    assert!(rp
        .reassert_options(&identity::session_key("no-such-token"), later)
        .is_err());
}

#[test]
fn a_stale_signature_counter_is_refused() {
    let (_home, layout, rp) = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &layout, &laptop, "person");
    let session = sign_in(&rp, &laptop, 7);
    let challenge = rp.challenge(Purpose::Reassert, NOW).unwrap();
    assert!(
        rp.reassert(
            reassert_body(&laptop, &challenge, 7, &session).as_bytes(),
            NOW
        )
        .is_err(),
        "a counter that did not move, as a sign-in refuses it"
    );
    let challenge = rp.challenge(Purpose::Reassert, NOW).unwrap();
    rp.reassert(
        reassert_body(&laptop, &challenge, 8, &session).as_bytes(),
        NOW,
    )
    .expect("a counter that moved");
    let stored = identity::read_passkeys(&layout).unwrap();
    assert_eq!(
        stored.values().next().unwrap().sign_count,
        8,
        "and it is stored"
    );
}

/// Two sessions of one passkey (two devices), both past the write bound. Re-asserting one
/// ends that one, opens a new one that writes, and leaves the other exactly as it was.
#[test]
fn a_reassertion_replaces_that_session_only_and_writes_again() {
    let (_home, layout, rp) = relying_party();
    let laptop = Authenticator::named("laptop");
    enroll(&rp, &layout, &laptop, "person");
    let here = sign_in(&rp, &laptop, 1);
    let there = sign_in(&rp, &laptop, 2);
    let later = NOW + identity::SESSION_SECONDS + 60;

    let lapsed = rp.identity(&here, later).expect("still signed in");
    assert!(
        !writes(&lapsed.scopes),
        "write has lapsed: {:?}",
        lapsed.scopes
    );
    assert!(!lapsed.restore.is_empty());

    let (challenge, _) = rp
        .reassert_options(&identity::session_key(&here), later)
        .unwrap();
    let (fresh, who) = rp
        .reassert(
            reassert_body(&laptop, &challenge, 3, &here).as_bytes(),
            later,
        )
        .expect("the same passkey re-asserts");
    assert_ne!(fresh, here);
    assert!(writes(&who.scopes), "{:?}", who.scopes);
    assert_eq!(
        who.write_seconds(later),
        identity::SESSION_SECONDS,
        "fresh bounds"
    );
    assert_eq!(who.expires, later + HOURS_240);

    assert!(rp.identity(&here, later).is_none(), "the old session ended");
    let now = rp.identity(&fresh, later).expect("the new one is live");
    assert!(writes(&now.scopes) && now.restore.is_empty());

    let other = rp
        .identity(&there, later)
        .expect("the passkey's other session is untouched");
    assert!(!writes(&other.scopes), "and still only reads");
    assert_eq!(other.expires, NOW + HOURS_240);

    // The old token cannot be re-asserted again: it is no session.
    let challenge = rp.challenge(Purpose::Reassert, later).unwrap();
    assert!(rp
        .reassert(
            reassert_body(&laptop, &challenge, 4, &here).as_bytes(),
            later
        )
        .is_err());
}

/// The value of `key` in a segment line.
fn column<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|c| c.strip_prefix(&format!("{key}=")))
}

#[test]
fn the_audit_line_names_the_reassertion() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let keep = Arc::clone(&lines);
    let sink: Sink = Arc::new(move |l: &str| keep.lock().unwrap().push(l.to_string()));
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW).auditing(Record::to(sink));
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &layout, &laptop, "person");
    enroll(&rp, &layout, &phone, "person");
    let session = sign_in(&rp, &laptop, 1);

    let challenge = rp.challenge(Purpose::Reassert, NOW).unwrap();
    rp.reassert(
        reassert_body(&phone, &challenge, 1, &session).as_bytes(),
        NOW,
    )
    .unwrap_err();
    let challenge = rp.challenge(Purpose::Reassert, NOW).unwrap();
    rp.reassert(
        reassert_body(&laptop, &challenge, 2, &session).as_bytes(),
        NOW,
    )
    .unwrap();

    let lines = lines.lock().unwrap();
    let reasserts: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains(" urn:iki:gonk:passkey:reassert "))
        .collect();
    assert_eq!(reasserts.len(), 2, "{lines:#?}");
    let (denied, ok) = (reasserts[0], reasserts[1]);
    assert!(
        denied.contains(&format!("<{}>", identity::LOGIN_CLASS)),
        "{denied}"
    );
    assert_eq!(column(denied, "outcome"), Some("denied"), "{denied}");
    assert_eq!(column(ok, "outcome"), Some("ok"), "{ok}");
    let laptop_iri = identity::passkey_iri(&laptop_id(&laptop));
    assert_eq!(column(ok, "principal"), Some(laptop_iri.as_str()), "{ok}");
    assert_eq!(column(ok, "sessions"), Some("1"), "{ok}");
    // The session it replaced ends under its own line, naming why.
    let ended = lines
        .iter()
        .find(|l| column(l, "why") == Some("reasserted"))
        .unwrap_or_else(|| panic!("no ending line: {lines:#?}"));
    assert!(
        ended.contains(&format!("<{}>", identity::LOGOUT_CLASS)),
        "{ended}"
    );
    assert_eq!(column(ended, "sessions"), Some("1"), "{ended}");
}

fn laptop_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

/// A same-origin POST, with the session cookie `cookie` if not empty: the status, the
/// `Set-Cookie` token if any, and the body.
fn post(gonk: &Gonk, path: &str, cookie: &str, body: &str) -> (u16, Option<String>, String) {
    let mut stream = std::net::TcpStream::connect(gonk.http).expect("connect");
    let cookie = if cookie.is_empty() {
        String::new()
    } else {
        format!("Cookie: {}={cookie}\r\n", identity::SESSION_COOKIE)
    };
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nOrigin: http://localhost:{port}\r\n\
         Sec-Fetch-Site: same-origin\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        port = gonk.http.port()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let set = head.lines().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.eq_ignore_ascii_case("set-cookie").then(|| {
            value
                .trim()
                .strip_prefix(&format!("{}=", identity::SESSION_COOKIE))
                .and_then(|v| v.split(';').next())
                .unwrap_or_default()
                .to_string()
        })
    });
    (status, set, body.to_string())
}

/// The page's two steps over real HTTP, naming no token: the cookie names the session, which
/// is replaced, and the passkey's other session (another browser) is untouched.
#[test]
fn the_cookie_names_the_session_over_http() {
    let gonk = Gonk::start("");
    let origin = format!("http://localhost:{}", gonk.http.port());
    let now = identity::now_seconds();
    let scopes = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&gonk.layout, "person", &scopes, false, 5, now).unwrap();
    let laptop = Authenticator::named("laptop");
    let (_, _, options) = post(&gonk, "/auth/register-options", "", "{}");
    let challenge: Value = serde_json::from_str(&options).unwrap();
    let (status, _, body) = post(
        &gonk,
        "/auth/register",
        "",
        &laptop.register_body(challenge["challenge"].as_str().unwrap(), &code, &origin),
    );
    assert_eq!(status, 200, "{body}");
    let mut sessions = Vec::new();
    for count in [1, 2] {
        let (_, _, options) = post(&gonk, "/auth/login-options", "", "{}");
        let challenge: Value = serde_json::from_str(&options).unwrap();
        let (status, set, body) = post(
            &gonk,
            "/auth/login",
            "",
            &laptop.login_body(challenge["challenge"].as_str().unwrap(), &origin, count),
        );
        assert_eq!(status, 200, "{body}");
        sessions.push(set.expect("a sign-in sets the cookie"));
    }
    let (here, there) = (&sessions[0], &sessions[1]);

    // No cookie: nothing to re-assert.
    let (status, _, body) = post(&gonk, "/auth/reassert-options", "", "");
    assert!((400..500).contains(&status), "{status} {body}");

    let (status, _, body) = post(&gonk, "/auth/reassert-options", here, "");
    assert_eq!(status, 200, "{body}");
    let options: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(options["credential"], laptop_id(&laptop), "{body}");
    assert_eq!(options["rpId"], "localhost", "{body}");

    // A sign-in challenge, posted to reassert with the cookie: refused.
    let (_, _, login) = post(&gonk, "/auth/login-options", "", "{}");
    let login: Value = serde_json::from_str(&login).unwrap();
    let (status, set, body) = post(
        &gonk,
        "/auth/reassert",
        here,
        &laptop.login_body(login["challenge"].as_str().unwrap(), &origin, 3),
    );
    assert!((400..500).contains(&status), "{status} {body}");
    assert!(set.is_none());

    let (status, set, body) = post(
        &gonk,
        "/auth/reassert",
        here,
        &laptop.login_body(options["challenge"].as_str().unwrap(), &origin, 3),
    );
    assert_eq!(status, 200, "{body}");
    let fresh = set.expect("the re-assertion sets the new cookie");
    assert!(!fresh.is_empty() && &fresh != here);
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(answer["session"], json!(fresh));

    // The old cookie no longer works; the new one does, and writes; the other is untouched.
    let (status, _, _) = post(&gonk, "/auth/session", here, "");
    assert_eq!(status, 404, "the old cookie names no session");
    let (status, _, body) = post(&gonk, "/auth/session", &fresh, "");
    assert_eq!(status, 200, "{body}");
    let who: Value = serde_json::from_str(&body).unwrap();
    assert!(who["writeSeconds"].as_u64().unwrap() > 0, "{body}");
    let (status, _, body) = post(&gonk, "/auth/session", "", there);
    assert_eq!(status, 200, "the other session stands: {body}");
}
