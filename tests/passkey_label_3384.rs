//! Ledger [#3384](http://localhost:1062/l/default/item/3384): enrollment accepted a label
//! another passkey already had. `passkey revoke <label>` refuses a label two passkeys share,
//! and the system's passkey picker cannot tell them apart, so enrollment created exactly the
//! ambiguity revocation refuses. Now enrollment refuses it, naming the label, and the invite
//! stays good so the person can choose another.
//!
//! - [`a_second_passkey_with_a_used_label_is_refused_and_the_invite_stays_good`]
//! - [`an_unlabeled_enrollment_is_checked_under_the_label_it_would_get`]
//! - [`register_options_refuses_a_used_label_before_the_authenticator_is_asked`]

use std::io::{Read, Write};
use std::net::TcpStream;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::quic;
use scratch_gonk::Gonk;

const NOW: u64 = 1_800_000_000;

fn invite(layout: &quic::Layout, grant: &str) -> String {
    let scopes = grants_for("default", Authority::Read).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
}

fn register(
    rp: &Passkeys,
    auth: &Authenticator,
    code: &str,
    label: &str,
) -> Result<identity::Enrolled, String> {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body_labeled(&challenge, code, rp.origin(), label)
            .as_bytes(),
        NOW,
    )
}

#[test]
fn a_second_passkey_with_a_used_label_is_refused_and_the_invite_stays_good() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new(layout.clone(), 1062);

    let first = invite(&layout, "ada");
    register(&rp, &Authenticator::named("one"), &first, "laptop").expect("the first enrolls");

    let second = invite(&layout, "ada");
    let phone = Authenticator::named("two");
    let refused = register(&rp, &phone, &second, " laptop ").expect_err("a used label");
    assert!(
        refused.contains("`laptop`") && refused.contains("choose a different label"),
        "the refusal names the label and the way on: {refused}"
    );
    assert_eq!(identity::read_passkeys(&layout).unwrap().len(), 1);

    // The same invite, a different label: it was not spent by the refusal.
    let enrolled = register(&rp, &phone, &second, "phone").expect("the invite is still good");
    assert_eq!(enrolled.label, "phone");
    let labels: Vec<String> = identity::read_passkeys(&layout)
        .unwrap()
        .into_values()
        .map(|e| e.label)
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.contains(&"laptop".into()) && labels.contains(&"phone".into()));
}

#[test]
fn an_unlabeled_enrollment_is_checked_under_the_label_it_would_get() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new(layout.clone(), 1062);

    // No label: the passkey is labeled with its grant.
    let first = invite(&layout, "ada");
    let enrolled = register(&rp, &Authenticator::named("one"), &first, "").unwrap();
    assert_eq!(enrolled.label, "ada");

    let second = invite(&layout, "ada");
    let other = Authenticator::named("two");
    let refused = register(&rp, &other, &second, "").expect_err("the grant's name is taken");
    assert!(refused.contains("`ada`"), "{refused}");
    register(&rp, &other, &second, "desk").expect("the invite is still good");
}

/// The early answer: the page names its label in `register-options`, so a taken one is refused
/// before `navigator.credentials.create()` mints a credential that could never be enrolled.
#[test]
fn register_options_refuses_a_used_label_before_the_authenticator_is_asked() {
    let gonk = Gonk::start("");
    let post = |path: &str, body: &str| -> (u16, String) {
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
    };
    let origin = format!("http://localhost:{}", gonk.http.port());
    let scopes = grants_for("default", Authority::Read).unwrap();
    let code = identity::invite(
        &gonk.layout,
        "ada",
        &scopes,
        false,
        5,
        identity::now_seconds(),
    )
    .unwrap();

    let (status, body) = post("/auth/register-options", r#"{"label":"laptop"}"#);
    assert_eq!(status, 200, "a free label: {body}");
    let options: serde_json::Value = serde_json::from_str(&body).unwrap();
    let registration = Authenticator::new().register_body_labeled(
        options["challenge"].as_str().unwrap(),
        &code,
        &origin,
        "laptop",
    );
    let (status, body) = post("/auth/register", &registration);
    assert_eq!(status, 200, "{body}");

    let (status, body) = post("/auth/register-options", r#"{"label":"laptop"}"#);
    assert!(
        (400..500).contains(&status) && body.contains("`laptop`"),
        "a used label is refused before any challenge: {status} {body}"
    );
    let (status, body) = post("/auth/register-options", r#"{"label":"phone"}"#);
    assert_eq!(status, 200, "{body}");
    let (status, body) = post("/auth/register-options", "{}");
    assert_eq!(status, 200, "no label, no check: {body}");
}
