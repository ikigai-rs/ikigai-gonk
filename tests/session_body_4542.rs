//! Ledger [#4542](http://localhost:1062/l/default/item/4542): the sign-in answers (`login`,
//! `add-grant`, `reassert`) carried the session token itself in the JSON body, beside the
//! `HttpOnly` cookie (ledger #1591). The script that made the request reads that body, so any
//! script on the page could take the bearer token. The token now travels only in the
//! `Set-Cookie`; the body keeps `sessionId`, the token's digest, which names a session and
//! authorizes nothing.
//!
//! - [`no_sign_in_answer_carries_the_session_token`]

use std::io::{Read, Write};
use std::net::TcpStream;

mod common;
mod scratch_gonk;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use scratch_gonk::Gonk;
use serde_json::Value;

struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    /// The token the `Set-Cookie` opens: the only place it travels.
    fn cookie(&self) -> String {
        self.head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .find_map(|(_, value)| value.trim().strip_prefix("gonk_session="))
            .and_then(|v| v.split(';').next())
            .unwrap_or_else(|| panic!("no session cookie: {}", self.head))
            .to_string()
    }

    /// The answer must not hold the token anywhere in its body: not as `session`, not under
    /// any other name.
    fn holds_no_token(&self, op: &str) {
        let token = self.cookie();
        assert!(!token.is_empty(), "{op}: {}", self.head);
        let v = self.json();
        assert!(
            v.get("session").is_none(),
            "{op} answers a `session`: {}",
            self.body
        );
        assert!(
            !self.body.contains(&token),
            "{op} answers the token in its body: {}",
            self.body
        );
        assert_eq!(
            v["sessionId"].as_str(),
            Some(identity::session_id(&token).as_str()),
            "{op} still names its session by digest: {}",
            self.body
        );
    }
}

fn post(gonk: &Gonk, path: &str, body: &str, cookie: Option<&str>) -> Answer {
    let mut stream = TcpStream::connect(gonk.http).expect("connect");
    let cookie = cookie
        .map(|t| format!("Cookie: gonk_session={t}\r\n"))
        .unwrap_or_default();
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
    Answer {
        status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        head: head.to_string(),
        body: body.to_string(),
    }
}

fn challenge(gonk: &Gonk, op: &str, body: &str, cookie: Option<&str>) -> Value {
    let got = post(gonk, &format!("/auth/{op}"), body, cookie);
    assert_eq!(got.status, 200, "{op}: {}", got.body);
    got.json()
}

/// Sign in through the door, widen through the door, re-assert through the door: none of the
/// three answers puts the token where the page's script can read it, and each token still works
/// as the cookie.
#[test]
fn no_sign_in_answer_carries_the_session_token() {
    let gonk = Gonk::start("");
    let origin = format!("http://localhost:{}", gonk.http.port());
    let now = identity::now_seconds();
    let read = grants_for("default", Authority::Read).unwrap();
    let first = identity::invite(&gonk.layout, "viewer", &read, false, 5, now).unwrap();
    let laptop = Authenticator::named("laptop");

    let c = challenge(&gonk, "register-options", "{}", None);
    let got = post(
        &gonk,
        "/auth/register",
        &laptop.register_body(c["challenge"].as_str().unwrap(), &first, &origin),
        None,
    );
    assert_eq!(got.status, 200, "{}", got.body);

    // login
    let c = challenge(&gonk, "login-options", "{}", None);
    let login = post(
        &gonk,
        "/auth/login",
        &laptop.login_body(c["challenge"].as_str().unwrap(), &origin, 1),
        None,
    );
    assert_eq!(login.status, 200, "{}", login.body);
    login.holds_no_token("login");
    let session = login.cookie();

    // add-grant: the page names no token; the options and the widening go by cookie alone.
    let write = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&gonk.layout, "writer", &write, false, 5, now).unwrap();
    let asked = serde_json::json!({ "invite": code }).to_string();
    let options = challenge(&gonk, "add-grant-options", &asked, Some(&session));
    assert_eq!(options["grant"], "writer", "{options}");
    let mut widen: Value = serde_json::from_str(&laptop.login_body(
        options["challenge"].as_str().unwrap(),
        &origin,
        2,
    ))
    .unwrap();
    widen["invite"] = code.as_str().into();
    let added = post(&gonk, "/auth/add-grant", &widen.to_string(), Some(&session));
    assert_eq!(added.status, 200, "{}", added.body);
    added.holds_no_token("add-grant");
    assert_eq!(added.json()["added"], "writer", "{}", added.body);
    let widened = added.cookie();
    assert_ne!(widened, session);

    // reassert, naming the session by its digest as an answer's `sessionId` does.
    let id = added.json()["sessionId"].as_str().unwrap().to_string();
    let named = serde_json::json!({ "sessionId": id }).to_string();
    let options = challenge(&gonk, "reassert-options", &named, Some(&widened));
    let mut again: Value = serde_json::from_str(&laptop.login_body(
        options["challenge"].as_str().unwrap(),
        &origin,
        3,
    ))
    .unwrap();
    again["sessionId"] = id.as_str().into();
    let reasserted = post(&gonk, "/auth/reassert", &again.to_string(), Some(&widened));
    assert_eq!(reasserted.status, 200, "{}", reasserted.body);
    reasserted.holds_no_token("reassert");
    let fresh = reasserted.cookie();
    assert_ne!(fresh, widened);

    // The cookie is the session: who am I, by cookie alone.
    let who = post(&gonk, "/auth/session", "", Some(&fresh));
    assert_eq!(who.status, 200, "{}", who.body);
    assert_eq!(
        who.json()["grants"],
        serde_json::json!(["viewer", "writer"]),
        "{}",
        who.body
    );
    assert!(
        !who.body.contains(&fresh),
        "nor does `session` echo the token: {}",
        who.body
    );
}
