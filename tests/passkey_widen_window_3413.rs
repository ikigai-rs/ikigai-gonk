//! Ledger [#3413](http://localhost:1062/l/default/item/3413), the audit of passkey grants: a
//! redemption while signed in promises that "the only session carrying the wider grant is the
//! one whose holder just proved the passkey". It wrote the wider grant into `clients.json`
//! first and ended the passkey's old sessions only after its audit line was written, so for as
//! long as that write took, every old session of the passkey (a stolen cookie included) read
//! the wider grant, and kept it if the line failed and the change was undone. The old sessions
//! are now ended before the grant is written.
//!
//! - [`no_old_session_reads_the_wider_grant_while_the_widening_is_recorded`]
//! - [`the_last_grant_refusal_reads_as_one_sentence`]

#![cfg(unix)]

use std::io::Read;

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::quic;
use serde_json::Value;

const NOW: u64 = 1_800_000_000;

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
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

fn grants_of(layout: &quic::Layout, auth: &Authenticator) -> Vec<String> {
    use base64::Engine;
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id);
    identity::read_passkeys(layout).unwrap()[&id].grants.clone()
}

/// `gonk/passkeys.log` is made a FIFO, so the widening's audit line blocks in `open` until this
/// test reads it: the moment between the grant being written and the line being recorded,
/// held open. A session the passkey opened before the widening, asked about in that moment,
/// must not read the wider grant; it must be ended.
#[test]
fn no_old_session_reads_the_wider_grant_while_the_widening_is_recorded() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let laptop = Authenticator::named("laptop");
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        laptop
            .register_body(
                &challenge,
                &invite(&layout, "viewer", Authority::Read),
                rp.origin(),
            )
            .as_bytes(),
        NOW,
    )
    .expect("enrolls");
    let session = sign_in(&rp, &laptop, 1);
    // The same passkey signed in elsewhere: another browser, or a cookie someone took.
    let elsewhere = sign_in(&rp, &laptop, 2);
    let read_only = rp.identity(&elsewhere, NOW).unwrap().scopes;
    let code = invite(&layout, "writer", Authority::Write);

    let log = identity::audit_log(&layout);
    let fifo = std::ffi::CString::new(log.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let challenge = rp.challenge(Purpose::AddGrant, NOW).unwrap();
    let mut body: Value =
        serde_json::from_str(&laptop.login_body(&challenge, rp.origin(), 3)).unwrap();
    body["session"] = session.as_str().into();
    body["invite"] = code.as_str().into();
    let body = body.to_string();

    let (during, line, widened) = std::thread::scope(|s| {
        let widening = s.spawn(|| rp.add_grant(body.as_bytes(), NOW));
        // Wait for the grant to land in clients.json; the widening is then blocked on the log.
        let started = std::time::Instant::now();
        while grants_of(&layout, &laptop) != ["viewer", "writer"] {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "the widening never wrote clients.json"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let during = rp.identity(&elsewhere, NOW).map(|who| who.scopes);
        // Let the line through, and the widening finish.
        let mut line = String::new();
        std::fs::File::open(&log)
            .unwrap()
            .read_to_string(&mut line)
            .unwrap();
        (during, line, widening.join().unwrap())
    });

    let (fresh, who, added) = widened.expect("the widening succeeds");
    assert_eq!(added, "writer");
    assert!(line.contains(identity::GRANT_ADDED), "{line}");
    assert!(
        who.scopes.iter().any(|s| !read_only.contains(s)),
        "the fresh session carries the wider grant"
    );
    assert!(
        during.is_none(),
        "a session opened before the widening read {:?} while it was being recorded \
         (it held {read_only:?}); only the fresh session may carry the wider grant",
        during.unwrap()
    );
    assert!(rp.identity(&elsewhere, NOW).is_none(), "and stays ended");
    assert!(rp.identity(&session, NOW).is_none());
    assert!(rp.identity(&fresh, NOW).is_some());
}

/// The refusal to take a passkey's last grant reads as one sentence: its source line lost
/// its `\` continuation, so the message carried a run of spaces in the middle of a word pair.
#[test]
fn the_last_grant_refusal_reads_as_one_sentence() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    let laptop = Authenticator::named("laptop");
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    let enrolled = rp
        .register(
            laptop
                .register_body(
                    &challenge,
                    &invite(&layout, "viewer", Authority::Read),
                    rp.origin(),
                )
                .as_bytes(),
            NOW,
        )
        .expect("enrolls");
    let refused = identity::remove_grant(
        &layout,
        identity::Which::Credential(&enrolled.credential_id),
        "viewer",
        0,
    )
    .unwrap_err();
    assert!(
        refused.contains("cannot sign in: `ikigai-gonk passkey revoke`"),
        "{refused}"
    );
}
