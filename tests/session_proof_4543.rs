//! Ledger [#4543](http://localhost:1062/l/default/item/4543): the session cookie is bound to the
//! browser that signed in. `gonk_session` used to be a bearer token: a write carrying the cookie
//! value alone ran under the passkey's whole grant. Now a state-changing request keeps the
//! session's write scopes only with a `Gonk-Proof` signed by the P-256 key the page bound at its
//! sign-in, over the method, path, a fresh time and a door-issued nonce spent by that write.

mod common;

use std::net::IpAddr;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::{doors, quic};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};

struct Gonk {
    _home: tempfile::TempDir,
    layout: quic::Layout,
    rp: Arc<Passkeys>,
    door: doors::HttpDoor,
}

fn gonk() -> Gonk {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Arc::new(Passkeys::new(layout.clone(), 1062));
    let door = doors::HttpDoor {
        anonymous: Vec::new(),
        public: Vec::new(),
        bind: "127.0.0.1:1062".parse().unwrap(),
        passkeys: Some(Arc::clone(&rp)),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        public_origin: None,
    };
    Gonk {
        _home: home,
        layout,
        rp,
        door,
    }
}

/// The browser's session key: what WebCrypto keeps non-extractable in the page.
fn browser_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32].into()).unwrap()
}

fn public_b64(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

/// Enroll a writer and sign in, binding `key` when given; the cookie's token.
fn signed_in(g: &Gonk, key: Option<&SigningKey>) -> String {
    let auth = Authenticator::new();
    let now = identity::now_seconds();
    let scopes = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&g.layout, "writer", &scopes, false, 30, now).unwrap();
    let challenge = g.rp.challenge(Purpose::Register, now).unwrap();
    g.rp.register(
        auth.register_body(&challenge, &code, g.rp.origin())
            .as_bytes(),
        now,
    )
    .expect("enrolls");
    let challenge = g.rp.challenge(Purpose::Login, now).unwrap();
    let mut body: serde_json::Value =
        serde_json::from_str(&auth.login_body(&challenge, g.rp.origin(), 1)).unwrap();
    if let Some(key) = key {
        body["proofKey"] = public_b64(key).into();
    }
    g.rp.login(body.to_string().as_bytes(), now)
        .expect("signs in")
        .0
}

fn proof(g: &Gonk, key: &SigningKey, method: &str, path: &str) -> String {
    let now = identity::now_seconds();
    let nonce = g.rp.proof_nonce(now).unwrap();
    let message = identity::proof_message(method, path, now, &nonce);
    let signature: Signature = key.sign(message.as_bytes());
    format!(
        "{now}.{nonce}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn request(method: &str, path: &str, token: &str, proof: Option<&str>) -> ikigai_web::HttpRequest {
    let mut headers = vec![
        ("host".to_string(), "localhost:1062".to_string()),
        (
            "cookie".to_string(),
            format!("{}={token}", identity::SESSION_COOKIE),
        ),
    ];
    if let Some(proof) = proof {
        headers.push((identity::PROOF_HEADER.to_string(), proof.to_string()));
    }
    ikigai_web::HttpRequest {
        method: method.into(),
        path: path.into(),
        raw_target: path.into(),
        query: Vec::new(),
        headers,
        body: Vec::new(),
        peer: Some("127.0.0.1".parse::<IpAddr>().unwrap()),
    }
}

/// Whether the door's capability for `request` holds any scope that writes.
fn writes(g: &Gonk, request: &ikigai_web::HttpRequest) -> bool {
    doors::http_scopes(&g.door, request, identity::now_seconds())
        .iter()
        .any(|s| s.starts_with("urn:cap:") && !identity::read_only(std::slice::from_ref(s)))
}

fn reads(g: &Gonk, request: &ikigai_web::HttpRequest) -> bool {
    doors::http_scopes(&g.door, request, identity::now_seconds())
        .iter()
        .any(|s| s.starts_with("urn:cap:ledger:read:"))
}

const APPEND: &str = "/iki/ledger/append";

/// ★ The defect: the cookie value alone wrote.
#[test]
fn a_write_with_the_cookie_and_no_proof_is_refused() {
    let g = gonk();
    let key = browser_key(7);
    let token = signed_in(&g, Some(&key));
    let bare = request("POST", APPEND, &token, None);
    assert!(!writes(&g, &bare), "the cookie alone wrote");
    assert!(reads(&g, &bare), "the session still names its reads");
    let signed = proof(&g, &key, "POST", APPEND);
    assert!(
        writes(&g, &request("POST", APPEND, &token, Some(&signed))),
        "a write proven by the session's own key writes"
    );
}

#[test]
fn a_proof_from_a_different_key_is_refused() {
    let g = gonk();
    let token = signed_in(&g, Some(&browser_key(7)));
    let other = proof(&g, &browser_key(9), "POST", APPEND);
    assert!(!writes(&g, &request("POST", APPEND, &token, Some(&other))));
}

#[test]
fn a_replayed_nonce_is_refused() {
    let g = gonk();
    let key = browser_key(7);
    let token = signed_in(&g, Some(&key));
    let signed = proof(&g, &key, "POST", APPEND);
    assert!(writes(&g, &request("POST", APPEND, &token, Some(&signed))));
    assert!(
        !writes(&g, &request("POST", APPEND, &token, Some(&signed))),
        "the same proof wrote twice"
    );
}

#[test]
fn a_proof_for_another_method_path_or_time_is_refused() {
    let g = gonk();
    let key = browser_key(7);
    let token = signed_in(&g, Some(&key));
    let elsewhere = proof(&g, &key, "POST", "/iki/ledger/close");
    assert!(!writes(
        &g,
        &request("POST", APPEND, &token, Some(&elsewhere))
    ));
    let deleting = proof(&g, &key, "DELETE", APPEND);
    assert!(!writes(
        &g,
        &request("POST", APPEND, &token, Some(&deleting))
    ));
    let now = identity::now_seconds();
    let stale_at = now - identity::PROOF_SKEW_SECONDS - 5;
    let nonce = g.rp.proof_nonce(now).unwrap();
    let signature: Signature =
        key.sign(identity::proof_message("POST", APPEND, stale_at, &nonce).as_bytes());
    let stale = format!(
        "{stale_at}.{nonce}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    assert!(!writes(&g, &request("POST", APPEND, &token, Some(&stale))));
    // A nonce of a ceremony's (a login challenge) is not a proof nonce.
    let challenge = g.rp.challenge(Purpose::Login, now).unwrap();
    let signature: Signature =
        key.sign(identity::proof_message("POST", APPEND, now, &challenge).as_bytes());
    let foreign = format!(
        "{now}.{challenge}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    assert!(!writes(
        &g,
        &request("POST", APPEND, &token, Some(&foreign))
    ));
}

#[test]
fn a_read_with_the_cookie_alone_works() {
    let g = gonk();
    let token = signed_in(&g, Some(&browser_key(7)));
    let read = request("GET", "/l/default", &token, None);
    assert!(reads(&g, &read));
    assert!(
        writes(&g, &read),
        "a read keeps the whole grant's capability, as before"
    );
}

#[test]
fn a_session_with_no_key_reads_and_writes_nothing() {
    let g = gonk();
    let key = browser_key(7);
    let token = signed_in(&g, None);
    assert!(reads(&g, &request("GET", "/l/default", &token, None)));
    let signed = proof(&g, &key, "POST", APPEND);
    assert!(!writes(&g, &request("POST", APPEND, &token, Some(&signed))));
}

/// Upgrade from older versions: each `sessions.json` shape an older gonk wrote (format 1 with
/// `grants`, and before #3388 with one `grant`) loads keyless, read-only for writes, backed up
/// once and rewritten as format 2; a second start changes nothing.
#[test]
fn every_older_sessions_json_migrates_once_with_a_backup() {
    for fixture in ["format-1-grants.json", "format-1-grant.json"] {
        let g = gonk();
        let key = browser_key(7);
        let token = signed_in(&g, Some(&key));
        let hash = identity::session_id(&token);
        let path = g.layout.sessions_json();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let principal = doc["sessions"][&hash]["principal"]
            .as_str()
            .unwrap()
            .to_string();
        let now = identity::now_seconds();
        let older = std::fs::read_to_string(format!(
            "{}/tests/fixtures/sessions_4543/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        .replace("HASH", &hash)
        .replace("PRINCIPAL", &principal)
        .replace("\"ISSUED\"", &now.to_string())
        .replace("\"EXPIRES\"", &(now + 3600).to_string());
        std::fs::write(&path, &older).unwrap();

        let rp = Arc::new(Passkeys::new(g.layout.clone(), 1062));
        let door = doors::HttpDoor {
            passkeys: Some(Arc::clone(&rp)),
            ..g.door.clone()
        };
        let backup = identity::sessions_backup(&g.layout);
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            older,
            "{fixture}"
        );
        let migrated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(migrated["format"], 2, "{fixture}");
        assert!(
            migrated["sessions"][&hash]["proof_key"].is_null(),
            "{fixture}"
        );

        let g2 = Gonk { door, rp, ..g };
        assert!(
            reads(&g2, &request("GET", "/l/default", &token, None)),
            "{fixture}"
        );
        let signed = proof(&g2, &key, "POST", APPEND);
        assert!(
            !writes(&g2, &request("POST", APPEND, &token, Some(&signed))),
            "{fixture}"
        );

        // Idempotent: a second start keeps the file and the backup as they are.
        let before = std::fs::read_to_string(&path).unwrap();
        let _again = Passkeys::new(g2.layout.clone(), 1062);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{fixture}");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            older,
            "{fixture}"
        );
    }
}
