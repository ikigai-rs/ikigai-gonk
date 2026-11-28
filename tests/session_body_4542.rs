//! Ledger [#4542](http://localhost:1062/l/default/item/4542): the session cookie is HttpOnly
//! (#1591) so that no script on the page can read the token, but `login`, `add-grant` and
//! `reassert` also answered the token itself in the JSON body (`"session": token`), which the
//! script that made the request reads. The answers now carry `sessionId` (the token's digest)
//! and never the token: it travels in the HttpOnly `Set-Cookie` only.
//!
//! - [`no_sign_in_answer_carries_the_session_token`]: every door that opens a session, over
//!   real HTTP, answers a cookie and a `sessionId` that names it, and no token in the body.
//! - [`a_caller_holding_the_token_still_names_it_in_add_grant_options`]: the request-side
//!   `session` field keeps working for a caller that has the token.

mod common;
mod scratch_gonk;

use common::{post_page, post_page_signed_in, Answer, Authenticator};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use scratch_gonk::Gonk;
use serde_json::{json, Value};

fn challenge(answer: &Answer) -> String {
    assert_eq!(answer.status, 200, "{answer:?}");
    answer.json()["challenge"].as_str().unwrap().to_string()
}

/// `answer` opened a session: a cookie with the token, a body naming it by `sessionId`, and
/// the token nowhere in the body, under any field.
fn opened_without_the_token(door: &str, answer: &Answer) -> String {
    assert_eq!(answer.status, 200, "{door}: {answer:?}");
    let token = answer.session();
    let body = answer.json();
    assert!(
        body.get("session").is_none(),
        "{door}: the answer body carries `session`: {}",
        answer.body
    );
    assert!(
        !answer.body.contains(&token),
        "{door}: the answer body carries the session token: {}",
        answer.body
    );
    assert_eq!(
        body["sessionId"],
        json!(identity::session_id(&token)),
        "{door}: `sessionId` names the cookie's session: {}",
        answer.body
    );
    let cookie = answer
        .head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    assert!(cookie.contains("HttpOnly"), "{door}: {cookie}");
    token
}

fn enroll_and_sign_in(gonk: &Gonk, laptop: &Authenticator) -> String {
    let origin = format!("http://localhost:{}", gonk.http.port());
    let scopes = grants_for("default", Authority::Read).unwrap();
    let now = identity::now_seconds();
    let code = identity::invite(&gonk.layout, "viewer", &scopes, false, 5, now).unwrap();
    let c = challenge(&post_page(gonk.http, "/auth/register-options", "{}"));
    let registered = post_page(
        gonk.http,
        "/auth/register",
        &laptop.register_body(&c, &code, &origin),
    );
    assert_eq!(registered.status, 200, "{registered:?}");
    let c = challenge(&post_page(gonk.http, "/auth/login-options", "{}"));
    let login = post_page(gonk.http, "/auth/login", &laptop.login_body(&c, &origin, 1));
    opened_without_the_token("login", &login)
}

#[test]
fn no_sign_in_answer_carries_the_session_token() {
    let gonk = Gonk::start("");
    let origin = format!("http://localhost:{}", gonk.http.port());
    let laptop = Authenticator::named("laptop");
    let token = enroll_and_sign_in(&gonk, &laptop);

    // The page asks about its session with the cookie alone.
    let who = post_page_signed_in(gonk.http, &token, "/auth/session", "");
    assert_eq!(who.status, 200, "{who:?}");
    assert_eq!(who.json()["label"], laptop.label(), "{who:?}");

    // add-grant, as the page posts it: the invite and the assertion, no token.
    let write = grants_for("default", Authority::Write).unwrap();
    let now = identity::now_seconds();
    let code = identity::invite(&gonk.layout, "writer", &write, false, 5, now).unwrap();
    let options = post_page_signed_in(
        gonk.http,
        &token,
        "/auth/add-grant-options",
        &json!({ "invite": code }).to_string(),
    );
    let c = challenge(&options);
    let mut widen: Value = serde_json::from_str(&laptop.login_body(&c, &origin, 2)).unwrap();
    widen["invite"] = code.into();
    let added = post_page_signed_in(gonk.http, &token, "/auth/add-grant", &widen.to_string());
    let token = opened_without_the_token("add-grant", &added);
    assert_eq!(added.json()["added"], "writer", "{added:?}");

    // reassert, as a page reopening write: the cookie names the session.
    let options = post_page_signed_in(gonk.http, &token, "/auth/reassert-options", "");
    let c = challenge(&options);
    let again = post_page_signed_in(
        gonk.http,
        &token,
        "/auth/reassert",
        &laptop.login_body(&c, &origin, 3),
    );
    let fresh = opened_without_the_token("reassert", &again);
    let who = post_page_signed_in(gonk.http, &fresh, "/auth/session", "");
    assert_eq!(who.status, 200, "{who:?}");
}

#[test]
fn a_caller_holding_the_token_still_names_it_in_add_grant_options() {
    let gonk = Gonk::start("");
    let laptop = Authenticator::named("laptop");
    let token = enroll_and_sign_in(&gonk, &laptop);
    let write = grants_for("default", Authority::Write).unwrap();
    let now = identity::now_seconds();
    let code = identity::invite(&gonk.layout, "writer", &write, false, 5, now).unwrap();
    // No cookie: the token in the request body is what says who is asking.
    let options = post_page(
        gonk.http,
        "/auth/add-grant-options",
        &json!({ "session": token, "invite": code }).to_string(),
    );
    assert_eq!(options.status, 200, "{options:?}");
    assert_eq!(options.json()["grant"], "writer", "{options:?}");
}
