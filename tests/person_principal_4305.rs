//! Ledger [#4305](http://localhost:1062/l/default/item/4305), points 5 and 6: the HTTP door
//! stamps a signed-in request with its passkey's PERSON, and what was held under a passkey IRI
//! before the upgrade stays its person's.
//!
//! - [`two_passkeys_of_one_person_are_stamped_as_that_person`] — a write signed by either
//!   passkey is authored by one person IRI, and both read as the person's label.
//! - [`a_claim_taken_under_a_passkey_iri_is_released_by_its_person_after_the_upgrade`] — the
//!   claim moves to the person ([`ikigai_gonk::person_claims`]); the history keeps the passkey.
//! - [`a_dispatcher_cursor_kept_under_a_passkey_iri_is_its_persons`]

mod common;

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::{doors, quic};
use ikigai_store::DurableStore;
use serde_json::Value;

const NOW: u64 = 1_800_000_000;

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

fn enroll(rp: &Passkeys, auth: &Authenticator, code: &str) -> identity::Enrolled {
    let challenge = rp.challenge(Purpose::Register, NOW).unwrap();
    rp.register(
        auth.register_body_labeled(&challenge, code, rp.origin(), &auth.label())
            .as_bytes(),
        NOW,
    )
    .expect("enrolls")
}

fn sign_in(rp: &Passkeys, auth: &Authenticator) -> String {
    let challenge = rp.challenge(Purpose::Login, NOW).unwrap();
    rp.login(auth.login_body(&challenge, rp.origin(), 1).as_bytes(), NOW)
        .expect("signs in")
        .0
}

fn with_cookie(token: &str) -> ikigai_web::HttpRequest {
    ikigai_web::HttpRequest {
        method: "POST".into(),
        path: "/iki/ledger/append".into(),
        raw_target: "/iki/ledger/append".into(),
        query: Vec::new(),
        headers: vec![(
            "cookie".to_string(),
            format!("{}={token}", identity::SESSION_COOKIE),
        )],
        body: Vec::new(),
        peer: Some("127.0.0.1".parse::<IpAddr>().unwrap()),
    }
}

#[test]
fn two_passkeys_of_one_person_are_stamped_as_that_person() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let rp = Arc::new(Passkeys::new_at(layout.clone(), 1062, NOW));
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    let scopes = grants_for("default", Authority::Write).unwrap();
    let code = identity::invite(&layout, "brian", &scopes, false, 30, NOW).unwrap();
    let first = enroll(&rp, &laptop, &code);
    let (code, _, _) = identity::invite_person(&layout, &first.person, 30, NOW).unwrap();
    enroll(&rp, &phone, &code);
    let door = doors::HttpDoor {
        anonymous: Vec::new(),
        public: Vec::new(),
        bind: "127.0.0.1:1062".parse().unwrap(),
        passkeys: Some(Arc::clone(&rp)),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
    };
    let by_laptop = doors::http_principal_of(&door, &with_cookie(&sign_in(&rp, &laptop)), NOW);
    let by_phone = doors::http_principal_of(&door, &with_cookie(&sign_in(&rp, &phone)), NOW);
    assert_eq!(by_laptop.as_deref(), Some(first.person.as_str()));
    assert_eq!(by_phone, by_laptop, "one person, whichever passkey signed");
    let labels = rp.labels();
    assert_eq!(labels[&first.person], laptop.label());
    assert_eq!(
        labels[&identity::passkey_iri(&base64_id(&phone))],
        laptop.label()
    );
    // The person is what each passkey's session answers for: either signs the other out.
    assert_eq!(
        rp.credentials_of(&first.person).len(),
        2,
        "both passkeys stand for the person"
    );
}

fn issue(
    hub: &Kernel,
    cap: &Capability,
    verb: Verb,
    target: &str,
    args: &[(&str, &str)],
) -> Result<Value, Error> {
    let req = args.iter().fold(
        Request::new(verb, Iri::parse(target).unwrap()),
        |req, (k, v)| req.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    );
    block_on(hub.issue(req, cap)).map(|r| {
        serde_json::from_slice(&r.bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&r.bytes).into_owned()))
    })
}

fn acting_as(principal: &str, authority: Authority) -> Capability {
    Capability::scoped(grants_for("default", authority).unwrap())
        .with_principal(principal)
        .unwrap()
}

fn root_issue(hub: &Kernel) -> impl Fn(Request) -> Result<Vec<u8>, String> + '_ {
    move |request| {
        block_on(hub.issue(request, &Capability::root()))
            .map(|r| r.bytes.to_vec())
            .map_err(|e| e.to_string())
    }
}

const PASSKEY: &str = "urn:iki:gonk:passkey:bGFwdG9w";
const PERSON: &str = "urn:iki:gonk:person:id:QUFBQUFBQUFB";

#[test]
fn a_claim_taken_under_a_passkey_iri_is_released_by_its_person_after_the_upgrade() {
    let hub = ikigai_gonk::compose(DurableStore::in_memory().unwrap());
    let root = Capability::root();
    issue(
        &hub,
        &root,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "Work taken before people")],
    )
    .unwrap();
    // Before the upgrade: the door stamped the passkey IRI.
    let before = acting_as(PASSKEY, Authority::Write);
    issue(
        &hub,
        &before,
        Verb::Sink,
        "urn:iki:ledger:default:claim",
        &[("item", "1"), ("content", "laptop"), ("lease", "10m")],
    )
    .unwrap();
    // After it, the same person is stamped with its person IRI, which the ledger compares
    // exactly: without the move, its own claim is refused to it.
    let after = acting_as(PERSON, Authority::Write);
    let renew = [
        ("item", "1"),
        ("content", "laptop"),
        ("lease", "10m"),
        ("renew", "true"),
    ];
    assert!(
        issue(
            &hub,
            &after,
            Verb::Sink,
            "urn:iki:ledger:default:claim",
            &renew
        )
        .is_err(),
        "the defect: a person cannot renew what its passkey claimed"
    );

    let people: BTreeMap<String, String> = [
        (PASSKEY.to_string(), PERSON.to_string()),
        // A passkey that holds nothing moves nothing.
        (
            "urn:iki:gonk:passkey:cGhvbmU".to_string(),
            PERSON.to_string(),
        ),
    ]
    .into();
    assert_eq!(
        ikigai_gonk::person_claims::run(&root_issue(&hub), &people).unwrap(),
        1
    );
    assert_eq!(
        ikigai_gonk::person_claims::run(&root_issue(&hub), &people).unwrap(),
        0,
        "a second run moves nothing"
    );
    issue(
        &hub,
        &after,
        Verb::Sink,
        "urn:iki:ledger:default:claim",
        &renew,
    )
    .expect("the person renews its passkey's claim");
    issue(
        &hub,
        &after,
        Verb::Delete,
        "urn:iki:ledger:default:claim",
        &[("item", "1")],
    )
    .expect("the person releases it");
    let item = issue(
        &hub,
        &root,
        Verb::Source,
        "urn:iki:ledger:default:item:1",
        &[("as", "application/json")],
    )
    .unwrap();
    assert!(
        item["item"]["claimed_by"].is_null() || item["item"].get("claimed_by").is_none(),
        "released: {item}"
    );
}

#[test]
fn a_dispatcher_cursor_kept_under_a_passkey_iri_is_its_persons() {
    let former: ikigai_gonk::dispatch::Former = Arc::new(|principal: &str| {
        if principal == PERSON {
            vec![PASSKEY.to_string()]
        } else {
            Vec::new()
        }
    });
    let hub = ikigai_gonk::compose_with(
        DurableStore::in_memory().unwrap(),
        None,
        Vec::new(),
        vec![Arc::new(
            ikigai_gonk::dispatch::space_with_former(None, former).unwrap(),
        )],
        None,
        None,
        None,
    );
    const VIEW: &str = "urn:iki:gonk:dispatch:default:item:1";
    let root = Capability::root();
    issue(
        &hub,
        &root,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "A dispatcher task")],
    )
    .unwrap();
    issue(
        &hub,
        &root,
        Verb::Sink,
        "urn:iki:ledger:default:comment",
        &[
            ("item", "1"),
            ("content", "a hand-back"),
            ("handback", "true"),
        ],
    )
    .unwrap();
    let look = |cap: &Capability| {
        issue(&hub, cap, Verb::Source, VIEW, &[("as", "application/json")]).unwrap()
    };
    let passkey = acting_as(PASSKEY, Authority::Read);
    let first = look(&passkey);
    assert!(first.to_string().contains("a hand-back"));
    issue(
        &hub,
        &passkey,
        Verb::Sink,
        VIEW,
        &[("cursor", first["cursor"].as_str().unwrap())],
    )
    .unwrap();
    let person = acting_as(PERSON, Authority::Read);
    assert!(
        !look(&person)["items"].to_string().contains("a hand-back"),
        "the person reads on from where its passkey acknowledged"
    );
    let stranger = acting_as("urn:iki:gonk:person:id:QkJCQkJCQkJC", Authority::Read);
    assert!(
        look(&stranger)["items"].to_string().contains("a hand-back"),
        "another person has a cursor of its own"
    );
}
