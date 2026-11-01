//! Ledger [#2776](http://localhost:1062/l/default/item/2776): a passkey revoke is never undone
//! by a sign-in of the same passkey running at the same time.
//!
//! A sign-in whose signature counter moved writes the passkey's entry back to `clients.json`.
//! It used to read the entry, verify, then re-read the file and re-insert the entry
//! unconditionally, so a `passkey revoke` landing between the two was reverted: the revoked
//! credential was enrolled again and could sign in once more. Found by the check-then-act audit
//! (ledger #2772), which reproduced it 30/30.
//!
//! - [`a_revoke_is_not_undone_by_a_concurrent_sign_in`]

use std::sync::{Arc, Barrier};

mod common;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Which};
use ikigai_gonk::quic;
use p256::pkcs8::EncodePublicKey;
use serde_json::json;

const NOW: u64 = 1_800_000_000;
const ROUNDS: u32 = 30;

/// `RACE_ROUNDS` overrides [`ROUNDS`], for a longer soak.
fn rounds() -> u32 {
    std::env::var("RACE_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ROUNDS)
}

/// A config home with ONE passkey enrolled under grant `person`, its counter at zero.
fn enrolled(auth: &common::Authenticator) -> (tempfile::TempDir, quic::Layout, String) {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
    let id = URL_SAFE_NO_PAD.encode(&auth.id);
    let key = URL_SAFE_NO_PAD.encode(
        p256::ecdsa::VerifyingKey::from(&auth.key)
            .to_public_key_der()
            .unwrap()
            .as_bytes(),
    );
    std::fs::write(
        layout.clients_json(),
        json!({ "passkeys": { &id: {"grant": "person", "label": "person", "public_key": key}} })
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        layout.grants_json(),
        json!({"person": ["urn:cap:ledger:read:default"]}).to_string(),
    )
    .unwrap();
    std::fs::write(identity::audit_log(&layout), b"").unwrap();
    (home, layout, id)
}

/// Each round releases a real sign-in (its counter moved, so it writes the entry back) and a
/// real `identity::revoke` of the same passkey together. Whatever order they land in, the
/// revoke answered Ok, so afterwards the passkey must not be enrolled and no session the
/// sign-in opened may still answer.
#[test]
fn a_revoke_is_not_undone_by_a_concurrent_sign_in() {
    let rounds = rounds();
    let mut violations = Vec::new();
    for round in 0..rounds {
        let auth = common::Authenticator::new();
        let (_home, layout, id) = enrolled(&auth);
        let rp = Arc::new(Passkeys::new(layout.clone(), 1062));
        let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
        let body = auth.login_body(&challenge, rp.origin(), round + 1);
        let barrier = Arc::new(Barrier::new(2));

        let signing = {
            let (rp, barrier) = (Arc::clone(&rp), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                rp.login(body.as_bytes(), NOW)
            })
        };
        let revoking = {
            let (layout, barrier, id) = (layout.clone(), Arc::clone(&barrier), id.clone());
            std::thread::spawn(move || {
                barrier.wait();
                identity::revoke(&layout, Which::Credential(&id), NOW * 1000)
            })
        };
        let login = signing.join().unwrap();
        revoking
            .join()
            .unwrap()
            .expect("the revoke itself succeeds");

        let still = identity::read_passkeys(&layout).unwrap().contains_key(&id);
        let live = login
            .as_ref()
            .ok()
            .is_some_and(|(token, _)| rp.identity(token, NOW).is_some());
        if still || live {
            violations.push(format!(
                "round {round}: enrolled after revoke = {still}, session live = {live}, login = {:?}",
                login.map(|_| "ok")
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{}/{rounds} rounds undid the revoke:\n{}",
        violations.len(),
        violations.join("\n")
    );
}
