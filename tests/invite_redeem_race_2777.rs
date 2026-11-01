//! Ledger [#2777](http://localhost:1062/l/default/item/2777): a redeemed single-use invite is
//! never revived by a `passkey invite` running at the same time.
//!
//! `redeem` and `invite` each read `invites.json`, changed it in memory and renamed it back,
//! with no lock and one shared temporary name. An invite landing between a redeem's read and
//! its rename wrote back the code the redeem had just removed, so a second authenticator could
//! enroll with it; or the two renames raced over the shared temporary and the invite reported
//! a failure for a code it never wrote. Found by the check-then-act audit (ledger #2772),
//! which reproduced it 20/30.
//!
//! - [`a_redeemed_invite_is_not_revived_by_a_concurrent_invite`]

use std::sync::{Arc, Barrier};

mod common;

use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::quic;

const NOW: u64 = 1_800_000_000;
const ROUNDS: u32 = 30;

/// `RACE_ROUNDS` overrides [`ROUNDS`], for a longer soak.
fn rounds() -> u32 {
    std::env::var("RACE_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ROUNDS)
}

/// Register an authenticator called `name` with `code`. Each needs its own name: a credential
/// id already enrolled is refused before the invite is looked at.
fn register(rp: &Passkeys, name: &str, code: &str) -> Result<identity::Enrolled, String> {
    let auth = common::Authenticator::named(name);
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body(&challenge, code, rp.origin()).as_bytes(),
        NOW,
    )
}

/// Each round issues one invite, then releases a registration that redeems it and a second
/// `identity::invite` for the same grant together. Whatever order they land in, both answered
/// Ok, so afterwards the redeemed code must enroll nobody else and the new code must work.
#[test]
fn a_redeemed_invite_is_not_revived_by_a_concurrent_invite() {
    let rounds = rounds();
    let scopes = grants_for("default", Authority::Read).unwrap();
    let mut violations = Vec::new();
    for round in 0..rounds {
        let home = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(home.path());
        let first = identity::invite(&layout, "person", &scopes, false, 30, NOW).unwrap();
        let rp = Arc::new(Passkeys::new(layout.clone(), 1062));
        let auth = common::Authenticator::new();
        let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
        let body = auth.register_body(&challenge, &first, rp.origin());
        let barrier = Arc::new(Barrier::new(2));

        let redeeming = {
            let (rp, barrier) = (Arc::clone(&rp), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                rp.register(body.as_bytes(), NOW)
            })
        };
        let inviting = {
            let (layout, barrier, scopes) = (layout.clone(), Arc::clone(&barrier), scopes.clone());
            std::thread::spawn(move || {
                barrier.wait();
                identity::invite(&layout, "person", &scopes, false, 30, NOW)
            })
        };
        let registered = redeeming.join().unwrap();
        let second = inviting.join().unwrap();

        let mut broken = Vec::new();
        if let Err(e) = &registered {
            broken.push(format!("the registration failed: {e}"));
        }
        match &second {
            Err(e) => broken.push(format!("the invite failed: {e}")),
            Ok(code) => {
                if let Err(e) = register(&rp, "second", code) {
                    broken.push(format!("the new invite does not work: {e}"));
                }
            }
        }
        if register(&rp, "intruder", &first).is_ok() {
            broken.push("a second authenticator enrolled with the redeemed invite".to_string());
        }
        if !broken.is_empty() {
            violations.push(format!("round {round}: {}", broken.join("; ")));
        }
    }
    assert!(
        violations.is_empty(),
        "{}/{rounds} rounds broke the single-use invite:\n{}",
        violations.len(),
        violations.join("\n")
    );
}
