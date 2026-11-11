//! Ledger [#3388](http://localhost:1062/l/default/item/3388): a passkey was enrolled under ONE
//! grant name, and a grant is shared by every passkey enrolled under it, so the only way to give
//! one person more access was to widen a grant, which silently widened everyone under it. A
//! passkey now holds a LIST of grants (its scopes the union of theirs), and redeeming an invite
//! while signed in ADDS the invite's grant to the signed-in passkey, against a fresh assertion
//! by that passkey, and logs it.
//!
//! - [`a_signed_in_redemption_adds_the_grant_to_that_passkey_only`]
//! - [`a_redemption_without_a_fresh_assertion_is_refused`]
//! - [`the_single_name_form_still_loads_and_the_list_form_is_written`]
//! - [`removing_one_grant_from_a_passkey_touches_nothing_else`]
//! - [`passkey_list_shows_every_grant_and_each_widening`]
//! - [`redeeming_while_signed_in_works_over_http`]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Which};
use ikigai_gonk::quic;
use scratch_gonk::Gonk;
use serde_json::{json, Value};

const NOW: u64 = 1_800_000_000;

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
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

/// What the page posts to `add-grant`: the session, the invite, and `auth`'s answer to
/// `challenge`.
fn add_grant_body(
    auth: &Authenticator,
    challenge: &str,
    origin: &str,
    count: u32,
    session: &str,
    code: &str,
) -> String {
    let mut body: Value = serde_json::from_str(&auth.login_body(challenge, origin, count)).unwrap();
    body["session"] = session.into();
    body["invite"] = code.into();
    body.to_string()
}

fn grants_of(layout: &quic::Layout, auth: &Authenticator) -> Vec<String> {
    let id = base64_id(auth);
    identity::read_passkeys(layout).unwrap()[&id].grants.clone()
}

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

fn log(layout: &quic::Layout) -> String {
    std::fs::read_to_string(identity::audit_log(layout)).unwrap_or_default()
}

/// Two passkeys under one grant, `viewer`; one of them redeems a `writer` invite while signed
/// in. That one holds both now, and the other, and the shared grant, are exactly as they were.
#[test]
fn a_signed_in_redemption_adds_the_grant_to_that_passkey_only() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    enroll(&rp, &phone, &invite(&layout, "viewer", Authority::Read));
    let viewer_before = quic::read_grants(&layout.grants_json()).unwrap()["viewer"].clone();

    let (old_session, before) = sign_in(&rp, &laptop, 1);
    let (phone_session, _) = sign_in(&rp, &phone, 1);
    let code = invite(&layout, "writer", Authority::Write);

    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let (session, who, added) = rp
        .add_grant(
            add_grant_body(&laptop, &challenge, rp.origin(), 2, &old_session, &code).as_bytes(),
            NOW,
        )
        .expect("a fresh assertion by the signed-in passkey adds the grant");
    assert_eq!(added, "writer");
    assert_eq!(who.enrolled.grants, ["viewer", "writer"]);

    // That passkey only, in the file; the other passkey and the shared grant are unchanged.
    assert_eq!(grants_of(&layout, &laptop), ["viewer", "writer"]);
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
    let grants = quic::read_grants(&layout.grants_json()).unwrap();
    assert_eq!(
        grants["viewer"], viewer_before,
        "the shared grant was not edited"
    );

    // Its scopes are the union of both grants'.
    let writer = &grants["writer"];
    assert!(
        writer.iter().all(|s| who.scopes.contains(s)),
        "{:?}",
        who.scopes
    );
    assert!(before.scopes.iter().all(|s| who.scopes.contains(s)));
    assert!(
        writer.iter().any(|s| !before.scopes.contains(s)),
        "it widened"
    );

    // Ended and re-established: the old session is gone, the new one carries the union, and
    // the other passkey's session reads as it did.
    assert!(
        rp.identity(&old_session, NOW).is_none(),
        "the old session ended"
    );
    let now = rp.identity(&session, NOW).expect("the new session is live");
    assert_eq!(now.scopes, who.scopes);
    let other = rp
        .identity(&phone_session, NOW)
        .expect("the other passkey's session stands");
    assert_eq!(other.enrolled.grants, ["viewer"]);
    assert_eq!(other.scopes, before.scopes);

    // One log line names the passkey, the grant added and the invite — by its hash, never the code.
    let log = log(&layout);
    let line = log
        .lines()
        .find(|l| l.contains(identity::GRANT_ADDED))
        .unwrap_or_else(|| panic!("no widening line: {log}"));
    assert!(
        line.contains(&identity::passkey_iri(&base64_id(&laptop))),
        "{line}"
    );
    assert!(line.contains(" added=writer"), "{line}");
    assert!(line.contains(" grant=viewer,writer"), "{line}");
    assert!(line.contains(" invite=sha256:"), "{line}");
    assert!(
        !log.contains(&code),
        "the code itself is never written: {log}"
    );

    // Spent: the same invite cannot widen again.
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    assert!(rp
        .add_grant(
            add_grant_body(&phone, &challenge, rp.origin(), 2, &phone_session, &code).as_bytes(),
            NOW
        )
        .is_err());
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
}

/// The session cookie alone, a sign-in challenge, another passkey's assertion, or an assertion
/// with no session: each is refused, nothing is written, and the invite is still good.
#[test]
fn a_redemption_without_a_fresh_assertion_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    enroll(&rp, &phone, &invite(&layout, "other", Authority::Read));
    let (session, _) = sign_in(&rp, &laptop, 1);
    let code = invite(&layout, "writer", Authority::Write);
    let clients = std::fs::read_to_string(layout.clients_json()).unwrap();

    // A stolen session plus the invite link: no assertion at all.
    let bare = json!({ "session": session, "invite": code }).to_string();
    let refused = rp
        .add_grant(bare.as_bytes(), NOW)
        .expect_err("no assertion");
    assert!(refused.contains("is missing"), "{refused}");

    // A sign-in's challenge cannot be spent on a widening.
    let login = rp.challenge(Purpose::Login, NOW).unwrap();
    let body = add_grant_body(&laptop, &login, rp.origin(), 2, &session, &code);
    let refused = rp
        .add_grant(body.as_bytes(), NOW)
        .expect_err("a login challenge");
    assert!(refused.contains("another ceremony"), "{refused}");

    // Another passkey's assertion, with this passkey's session.
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let body = add_grant_body(&phone, &challenge, rp.origin(), 2, &session, &code);
    let refused = rp
        .add_grant(body.as_bytes(), NOW)
        .expect_err("another passkey");
    assert!(
        refused.contains("not the one you are signed in with"),
        "{refused}"
    );

    // An assertion with no live session.
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let body = add_grant_body(&laptop, &challenge, rp.origin(), 2, "not-a-session", &code);
    let refused = rp.add_grant(body.as_bytes(), NOW).expect_err("no session");
    assert!(refused.contains("not signed in"), "{refused}");

    assert_eq!(
        std::fs::read_to_string(layout.clients_json()).unwrap(),
        clients,
        "nothing was written"
    );
    assert!(!log(&layout).contains(identity::GRANT_ADDED));

    // The invite is still good: the real ceremony succeeds with it.
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let body = add_grant_body(&laptop, &challenge, rp.origin(), 2, &session, &code);
    rp.add_grant(body.as_bytes(), NOW)
        .expect("the invite was not spent by a refusal");
    assert_eq!(grants_of(&layout, &laptop), ["viewer", "writer"]);
    assert_eq!(grants_of(&layout, &phone), ["other"]);

    // A grant the passkey already holds is refused before the invite is spent.
    let again = invite(&layout, "writer", Authority::Write);
    let (session, _) = sign_in(&rp, &laptop, 3);
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let body = add_grant_body(&laptop, &challenge, rp.origin(), 4, &session, &again);
    let refused = rp
        .add_grant(body.as_bytes(), NOW)
        .expect_err("already held");
    assert!(
        refused.contains("already holds grant `writer`"),
        "{refused}"
    );
    assert!(rp.invite_grant(&again).is_ok(), "still good");
}

/// A `clients.json` every earlier server wrote — `"grant": "<name>"` — still loads and signs
/// in; the next write of that entry is in the list form.
#[test]
fn the_single_name_form_still_loads_and_the_list_form_is_written() {
    use base64::Engine;
    use p256::pkcs8::EncodePublicKey;
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
    quic::put_grant(
        &layout,
        "alice",
        &grants_for("default", Authority::Read).unwrap(),
        false,
    )
    .unwrap();
    let auth = Authenticator::named("old");
    let spki = p256::ecdsa::VerifyingKey::from(&auth.key)
        .to_public_key_der()
        .unwrap();
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(spki.as_bytes());
    let doc = json!({
        "passkeys": { base64_id(&auth): {
            "grant": "alice", "label": "old", "public_key": key, "sign_count": 0
        }}
    });
    std::fs::write(layout.clients_json(), doc.to_string()).unwrap();

    assert_eq!(grants_of(&layout, &auth), ["alice"]);
    assert_eq!(identity::checked_passkeys(&layout).unwrap().len(), 1);
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (_, who) = sign_in(&rp, &auth, 1);
    assert_eq!(who.enrolled.grants, ["alice"]);

    // The sign-in moved the counter, so the entry was written: in the list form.
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(layout.clients_json()).unwrap()).unwrap();
    let entry = &written["passkeys"][base64_id(&auth)];
    assert_eq!(entry["grants"], json!(["alice"]), "{written}");
    assert!(entry.get("grant").is_none(), "{written}");
}

/// Taking one grant off one passkey leaves its other grants, every other passkey and
/// `grants.json` alone; its last grant is refused; and `passkey revoke` still removes one.
#[test]
fn removing_one_grant_from_a_passkey_touches_nothing_else() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    enroll(&rp, &laptop, &invite(&layout, "viewer", Authority::Read));
    enroll(&rp, &phone, &invite(&layout, "viewer", Authority::Read));
    let (session, _) = sign_in(&rp, &laptop, 1);
    let code = invite(&layout, "writer", Authority::Write);
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let (session, _, _) = rp
        .add_grant(
            add_grant_body(&laptop, &challenge, rp.origin(), 2, &session, &code).as_bytes(),
            NOW,
        )
        .unwrap();
    let grants = std::fs::read_to_string(layout.grants_json()).unwrap();

    let laptop_id = base64_id(&laptop);
    let now = identity::remove_grant(&layout, Which::Credential(&laptop_id), "viewer", 0).unwrap();
    assert_eq!(now.grants, ["writer"]);
    assert_eq!(grants_of(&layout, &laptop), ["writer"]);
    assert_eq!(grants_of(&layout, &phone), ["viewer"], "the other passkey");
    assert_eq!(
        std::fs::read_to_string(layout.grants_json()).unwrap(),
        grants,
        "no grant was edited"
    );
    // Narrowed at the next request: the live session reads the writer grant alone now.
    let who = rp.identity(&session, NOW).expect("still signed in");
    assert_eq!(who.enrolled.grants, ["writer"]);
    assert!(log(&layout).contains(&format!(
        "{} {} label=software-laptop grant=writer removed=viewer",
        identity::GRANT_REMOVED,
        identity::passkey_iri(&laptop_id)
    )));

    let refused =
        identity::remove_grant(&layout, Which::Credential(&laptop_id), "writer", 0).unwrap_err();
    assert!(refused.contains("passkey revoke"), "{refused}");
    let refused =
        identity::remove_grant(&layout, Which::Label("software-phone"), "writer", 0).unwrap_err();
    assert!(
        refused.contains("does not hold grant `writer`"),
        "{refused}"
    );

    identity::revoke(&layout, Which::Credential(&laptop_id), 0).unwrap();
    assert_eq!(identity::read_passkeys(&layout).unwrap().len(), 1);
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
}

fn run(config: &std::path::Path, args: &[&str]) -> (bool, String, String) {
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

/// `passkey list` prints each passkey's grants and, under it, each grant an invite added.
#[test]
fn passkey_list_shows_every_grant_and_each_widening() {
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
    let (session, _) = sign_in(&rp, &laptop, 1);
    let code = invite(&layout, "writer", Authority::Write);
    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    rp.add_grant(
        add_grant_body(&laptop, &challenge, rp.origin(), 2, &session, &code).as_bytes(),
        NOW,
    )
    .unwrap();

    let (ok, listed, err) = run(&config, &["passkey", "list"]);
    assert!(ok, "{err}");
    let laptop_line = listed
        .lines()
        .find(|l| l.starts_with("software-laptop"))
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(laptop_line.contains("grants viewer, writer"), "{listed}");
    let phone_line = listed
        .lines()
        .find(|l| l.starts_with("software-phone"))
        .unwrap();
    assert!(phone_line.contains("grant  viewer"), "{listed}");
    let widened = listed
        .lines()
        .find(|l| l.contains("added grant `writer` by invite sha256:"))
        .unwrap_or_else(|| panic!("the widening is listed: {listed}"));
    let laptop_at = listed.find(laptop_line).unwrap();
    let phone_at = listed.find(phone_line).unwrap();
    let widened_at = listed.find(widened).unwrap();
    assert!(
        widened_at > laptop_at && (phone_at < laptop_at || widened_at < phone_at),
        "listed under the laptop passkey: {listed}"
    );

    let (ok, out, err) = run(
        &config,
        &[
            "passkey",
            "remove-grant",
            "software-laptop",
            "--grant",
            "viewer",
        ],
    );
    assert!(ok, "{err}");
    assert!(out.contains("removed grant `viewer`"), "{out}");
    let (_, listed, _) = run(&config, &["passkey", "list"]);
    assert!(listed.contains("removed grant `viewer`"), "{listed}");
    assert_eq!(grants_of(&layout, &phone), ["viewer"]);
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

/// The page's two steps over real HTTP: `add-grant-options` names the grant and the passkey
/// and spends nothing; `add-grant` with the fresh assertion widens and answers a new session.
#[test]
fn redeeming_while_signed_in_works_over_http() {
    let gonk = Gonk::start("");
    let origin = format!("http://localhost:{}", gonk.http.port());
    let now = identity::now_seconds();
    let scopes = grants_for("default", Authority::Read).unwrap();
    let first = identity::invite(&gonk.layout, "viewer", &scopes, false, 5, now).unwrap();
    let laptop = Authenticator::named("laptop");
    let (_, options) = post(&gonk, "/auth/register-options", "{}");
    let challenge: Value = serde_json::from_str(&options).unwrap();
    let (status, body) = post(
        &gonk,
        "/auth/register",
        &laptop.register_body(challenge["challenge"].as_str().unwrap(), &first, &origin),
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

    let write = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&gonk.layout, "writer", &write, false, 5, now).unwrap();
    let asked = json!({ "session": session, "invite": code }).to_string();
    let (status, body) = post(&gonk, "/auth/add-grant-options", &asked);
    assert_eq!(status, 200, "{body}");
    let options: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(options["grant"], "writer", "{body}");
    assert_eq!(options["credential"], base64_id(&laptop), "{body}");

    // The session alone, posted straight to add-grant, is refused.
    let (status, body) = post(&gonk, "/auth/add-grant", &asked);
    assert!((400..500).contains(&status), "{status} {body}");

    let widen = add_grant_body(
        &laptop,
        options["challenge"].as_str().unwrap(),
        &origin,
        2,
        &session,
        &code,
    );
    let (status, body) = post(&gonk, "/auth/add-grant", &widen);
    assert_eq!(status, 200, "{body}");
    let done: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(done["added"], "writer");
    assert_eq!(done["grants"], json!(["viewer", "writer"]));
    assert_eq!(done["grant"], "viewer, writer");
    let fresh = done["session"].as_str().unwrap();
    assert_ne!(fresh, session);

    let (status, _) = post(&gonk, "/auth/session", &session);
    assert_eq!(status, 404, "the old session ended");
    let (status, body) = post(&gonk, "/auth/session", fresh);
    assert_eq!(status, 200, "{body}");
    let who: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(who["grants"], json!(["viewer", "writer"]), "{body}");
    assert!(
        log(&gonk.layout).contains(identity::GRANT_ADDED),
        "{}",
        log(&gonk.layout)
    );
}

const JS: &str = include_str!("../web/gonk.js");
const XSL: &str = include_str!("../web/gonk.xsl");

/// The page's half, as wiring: what `tests/browser/add_grant_3388.mjs` runs.
#[test]
fn the_page_offers_to_add_the_grant_when_signed_in() {
    for wired in [
        // Adding is the default choice, in a labeled group.
        r#"<legend>What should this invite do?</legend>"#,
        r#"id="enrol-add" value="add" checked="checked"/> Add its grant to the passkey you are signed in with"#,
        r#"id="enrol-create" value="create"/> Create a new passkey"#,
        r#"<button type="submit" id="enrol-submit">Create passkey</button>"#,
    ] {
        assert!(XSL.contains(wired), "web/gonk.xsl: {wired}");
    }
    for wired in [
        // Shown only signed in; the submit follows the choice.
        "const adding = signedIn ? wireEnrolChoice() : () => false;",
        "if (adding()) addGrant(m[1]);",
        // A fresh assertion by the signed-in passkey only, then the new session replaces the old.
        r#"post("/auth/add-grant-options", { session: t, invite })"#,
        r#"allowCredentials: [{ type: "public-key", id: fromB64(credential) }],"#,
        r#"userVerification: "required","#,
        "setToken(done.session, done.seconds);",
    ] {
        assert!(JS.contains(wired), "web/gonk.js: {wired}");
    }
}
