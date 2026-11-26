//! Ledger [#4305](http://localhost:1062/l/default/item/4305): a person that several passkeys
//! name. A passkey's grants are its person's, so a second device enrolled for a person, or a
//! passkey attached to one, holds that person's grants and reads as that person's label.
//!
//! - [`an_invite_for_a_person_enrolls_a_passkey_of_that_person_with_its_grants`]
//! - [`a_person_invite_does_not_widen_a_signed_in_passkey`]
//! - [`attaching_makes_two_passkeys_one_person_and_keeps_the_one_before`]
//! - [`a_persons_last_passkey_revoked_leaves_the_person_named`]

mod common;

use common::Authenticator;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose, Which};
use ikigai_gonk::quic;

const NOW: u64 = 1_800_000_000;

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, NOW).unwrap()
}

fn enroll(rp: &Passkeys, auth: &Authenticator, code: &str) -> Result<identity::Enrolled, String> {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body_labeled(&challenge, code, rp.origin(), &auth.label())
            .as_bytes(),
        NOW,
    )
}

fn sign_in(rp: &Passkeys, auth: &Authenticator, count: u32) -> (String, identity::Identity) {
    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    rp.login(
        auth.login_body(&challenge, rp.origin(), count).as_bytes(),
        NOW,
    )
    .expect("signs in")
}

fn relying_party() -> (tempfile::TempDir, quic::Layout, Passkeys) {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Passkeys::new_at(layout.clone(), 1062, NOW);
    (home, layout, rp)
}

#[test]
fn an_invite_for_a_person_enrolls_a_passkey_of_that_person_with_its_grants() {
    let (_home, layout, rp) = relying_party();
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    let first = enroll(&rp, &laptop, &invite(&layout, "brian", Authority::Write)).unwrap();
    // Widened after enrollment, so the phone can only hold `reviewer` through the person.
    invite(&layout, "reviewer", Authority::Read); // defines the grant `reviewer`
    identity::add_grant(
        &layout,
        Which::Label(&laptop.label()),
        "reviewer",
        NOW * 1000,
    )
    .unwrap();

    // By label and by IRI alike.
    for (key, auth) in [
        (laptop.label(), &phone),
        (first.person.clone(), &Authenticator::named("tablet")),
    ] {
        let (code, iri, who) = identity::invite_person(&layout, &key, 30, NOW).unwrap();
        assert_eq!(iri, first.person);
        assert_eq!(who.grants, ["brian", "reviewer"]);
        let enrolled = enroll(&rp, auth, &code).expect("enrolls under the person");
        assert_eq!(enrolled.person, first.person, "binds to the person");
        assert_eq!(enrolled.grants, ["brian", "reviewer"]);
        // The person's grants apply at once: the first sign-in holds them.
        let (_, signed_in) = sign_in(&rp, auth, 1);
        assert_eq!(signed_in.enrolled.person, first.person);
        assert_eq!(signed_in.enrolled.grants, ["brian", "reviewer"]);
        assert_eq!(
            signed_in.scopes,
            identity::scopes_of_all(&layout, &signed_in.enrolled.grants).unwrap()
        );
    }
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(layout.clients_json()).unwrap()).unwrap();
    assert_eq!(
        doc["people"].as_object().unwrap().len(),
        1,
        "no person was made"
    );
    let labels = rp.labels();
    for auth in [&laptop, &phone] {
        assert_eq!(
            labels[&identity::passkey_iri(&base64_id(auth))],
            laptop.label(),
            "every passkey of the person reads as its label"
        );
    }
    assert_eq!(labels[&first.person], laptop.label());

    let refused = identity::invite_person(&layout, "nobody", 30, NOW).expect_err("no such person");
    assert!(
        refused.contains("no person is labeled `nobody`"),
        "{refused}"
    );
}

#[test]
fn a_person_invite_does_not_widen_a_signed_in_passkey() {
    let (_home, layout, rp) = relying_party();
    let (laptop, other) = (
        Authenticator::named("laptop"),
        Authenticator::named("other"),
    );
    enroll(&rp, &laptop, &invite(&layout, "brian", Authority::Write)).unwrap();
    enroll(&rp, &other, &invite(&layout, "chris", Authority::Read)).unwrap();
    let (code, _, _) = identity::invite_person(&layout, &laptop.label(), 30, NOW).unwrap();
    let (token, _) = sign_in(&rp, &other, 1);
    let refused = rp
        .add_grant_preview(&token, &code, NOW)
        .expect_err("a person's invite widens nobody");
    assert!(
        refused.contains("enrolls a new passkey of person"),
        "{refused}"
    );
    assert_eq!(
        identity::read_passkeys(&layout).unwrap()[&base64_id(&other)].grants,
        ["chris"]
    );
}

#[test]
fn attaching_makes_two_passkeys_one_person_and_keeps_the_one_before() {
    let (_home, layout, rp) = relying_party();
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    let a = enroll(&rp, &laptop, &invite(&layout, "brian", Authority::Write)).unwrap();
    let b = enroll(&rp, &phone, &invite(&layout, "phone", Authority::Read)).unwrap();
    assert_ne!(a.person, b.person, "two enrollments, two people");

    let (now, before) = identity::attach(
        &layout,
        Which::Credential(&base64_id(&phone)),
        &a.person,
        NOW * 1000,
    )
    .unwrap();
    assert_eq!(before, b.person);
    assert_eq!(now.person, a.person);
    assert_eq!(now.grants, ["brian"], "the person's grants, not its own");
    let (_, signed_in) = sign_in(&rp, &phone, 1);
    assert_eq!(signed_in.enrolled.person, a.person);

    // Both passkeys, and the person, read as the person's label; the person the phone named
    // before is kept, so what it was named by still reads.
    let labels = rp.labels();
    assert_eq!(
        labels[&identity::passkey_iri(&base64_id(&phone))],
        laptop.label()
    );
    assert_eq!(
        labels[&identity::passkey_iri(&base64_id(&laptop))],
        laptop.label()
    );
    assert_eq!(labels[&b.person], phone.label());
    let log = std::fs::read_to_string(identity::audit_log(&layout)).unwrap();
    assert!(log.contains(identity::ATTACHED), "{log}");

    let again = identity::attach(&layout, Which::Label(&phone.label()), &a.person, NOW * 1000)
        .expect_err("already");
    assert!(again.contains("already names person"), "{again}");
}

#[test]
fn a_persons_last_passkey_revoked_leaves_the_person_named() {
    let (_home, layout, rp) = relying_party();
    let laptop = Authenticator::named("laptop");
    let a = enroll(&rp, &laptop, &invite(&layout, "brian", Authority::Write)).unwrap();
    identity::revoke(&layout, Which::Label(&laptop.label()), NOW * 1000).unwrap();
    assert!(identity::read_passkeys(&layout).unwrap().is_empty());
    assert_eq!(
        rp.labels()[&a.person],
        laptop.label(),
        "history still names it"
    );
    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    assert!(rp
        .login(
            laptop.login_body(&challenge, rp.origin(), 1).as_bytes(),
            NOW
        )
        .is_err());
}
