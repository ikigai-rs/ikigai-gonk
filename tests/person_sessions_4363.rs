//! Ledger [#4363](http://localhost:1062/l/default/item/4363): PR 431 round 4. Since #4305 the
//! HTTP door stamps the PERSON as the principal, which no longer says which session a request
//! is. The door now also hands the passkey resource the key of the request's own session.
//!
//! With a laptop and a phone signed in as one person:
//! - [`each_device_is_answered_for_its_own_session`]: `session`, `add-grant-options` and
//!   `reassert-options` answer for the laptop's session when the laptop asks.
//! - [`one_devices_reads_do_not_keep_anothers_session_alive`]: the idle clock.
//! - [`a_page_sign_out_ends_the_other_passkeys_sessions`]
//! - [`a_sign_out_that_cannot_read_the_persons_passkeys_says_it_did_not_happen`]
//! - [`add_grant_by_another_passkey_of_the_person_widens_and_ends_every_session`], and one by
//!   another person's passkey is refused.
//! - [`the_claim_migration_leaves_a_passkey_iri_no_enrolled_passkey_names`]
//! - [`cursor_adoption_keeps_a_cursor_the_person_already_has`]

mod common;

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use common::Authenticator;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::{self, Passkeys, Purpose};
use ikigai_gonk::{doors, quic, web};
use ikigai_store::DurableStore;
use serde_json::Value;

fn base64_id(auth: &Authenticator) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&auth.id)
}

/// A relying party, its HTTP door and the page space behind it.
struct Gonk {
    _home: tempfile::TempDir,
    layout: quic::Layout,
    rp: Arc<Passkeys>,
    door: doors::HttpDoor,
    kernel: Kernel,
}

fn gonk(idle_seconds: Option<u64>) -> Gonk {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let mut rp = Passkeys::new(layout.clone(), 1062);
    if let Some(seconds) = idle_seconds {
        rp = rp.with_idle_seconds(seconds);
    }
    let rp = Arc::new(rp);
    let hub = Arc::new(ikigai_gonk::compose(DurableStore::in_memory().unwrap()));
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: Vec::new().into(),
        passkeys: Arc::clone(&rp),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        holder_aliases: Default::default(),
        display: Default::default(),
        page_assets: Default::default(),
        projects: Default::default(),
        toolbox: false,
        faces: Vec::new(),
        inquiry: None,
    });
    let door = doors::HttpDoor {
        anonymous: Vec::new(),
        public: Vec::new(),
        bind: "127.0.0.1:1062".parse().unwrap(),
        passkeys: Some(Arc::clone(&rp)),
        anonymous_sparql_budget_ms: ikigai_gonk::budget::DEFAULT_ANONYMOUS_SPARQL_BUDGET_MS,
        public_origin: None,
    };
    Gonk {
        kernel: doors::http_kernel(hub, web::space(face)),
        _home: home,
        layout,
        rp,
        door,
    }
}

fn invite(layout: &quic::Layout, grant: &str, authority: Authority) -> String {
    let scopes = grants_for("default", authority).unwrap();
    identity::invite(layout, grant, &scopes, false, 30, identity::now_seconds()).unwrap()
}

fn enroll(g: &Gonk, auth: &Authenticator, code: &str) -> identity::Enrolled {
    let now = identity::now_seconds();
    let challenge = g.rp.challenge(Purpose::Register, now).unwrap();
    g.rp.register(
        auth.register_body_labeled(&challenge, code, g.rp.origin(), &auth.label())
            .as_bytes(),
        now,
    )
    .expect("enrolls")
}

/// A laptop passkey, and a phone passkey of the same person (`invite --person`).
fn one_person(g: &Gonk) -> (Authenticator, Authenticator, identity::Enrolled) {
    let (laptop, phone) = (
        Authenticator::named("laptop"),
        Authenticator::named("phone"),
    );
    let first = enroll(g, &laptop, &invite(&g.layout, "writer", Authority::Write));
    let (code, _, _) =
        identity::invite_person(&g.layout, &first.person, 30, identity::now_seconds()).unwrap();
    let second = enroll(g, &phone, &code);
    assert_eq!(second.person, first.person, "one person");
    (laptop, phone, first)
}

fn sign_in(g: &Gonk, auth: &Authenticator, count: u32, at: u64) -> String {
    let challenge = g.rp.challenge(Purpose::Login, at).unwrap();
    g.rp.login(
        auth.login_body(&challenge, g.rp.origin(), count).as_bytes(),
        at,
    )
    .expect("signs in")
    .0
}

/// `op` of the passkey resource, as the HTTP door serves `/auth/<op>` to a browser holding
/// the cookie `token`: the capability is the door's own ([`doors::http_cap`]).
fn auth(g: &Gonk, op: &str, token: &str, content: &str) -> Result<Value, Error> {
    let post = op != "session" || !content.is_empty();
    let request = ikigai_web::HttpRequest {
        method: if post { "POST" } else { "GET" }.into(),
        path: format!("/auth/{op}"),
        raw_target: format!("/auth/{op}"),
        query: Vec::new(),
        headers: vec![
            ("host".to_string(), "localhost:1062".to_string()),
            (
                "cookie".to_string(),
                format!("{}={token}", identity::SESSION_COOKIE),
            ),
        ],
        body: Vec::new(),
        peer: Some("127.0.0.1".parse::<IpAddr>().unwrap()),
    };
    let capability = doors::http_cap(g.door.clone())(&request);
    issue(
        &g.kernel,
        &capability,
        if post { Verb::Sink } else { Verb::Source },
        &format!("urn:iki:gonk:passkey:{op}"),
        content,
    )
}

fn issue(
    kernel: &Kernel,
    capability: &Capability,
    verb: Verb,
    target: &str,
    content: &str,
) -> Result<Value, Error> {
    let request = Request::new(verb, Iri::parse(target).unwrap())
        .with_arg("content", ArgRef::Inline(content.as_bytes().to_vec()));
    // An answer that sets a cookie is wrapped with its headers in-process: kept as text.
    block_on(kernel.issue(request, capability)).map(|r| {
        serde_json::from_slice(&r.bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&r.bytes).into_owned()))
    })
}

/// ★ The defect: the laptop's own requests were answered from the phone's session, the newest
/// of the person's, so `session` named the phone, add-grant asked the authenticator for the
/// phone's credential, and a re-assertion with no `sessionId` was refused as ambiguous.
#[test]
fn each_device_is_answered_for_its_own_session() {
    let g = gonk(None);
    let (laptop, phone, _) = one_person(&g);
    let now = identity::now_seconds();
    // The phone signs in last, so it is the person's newest session.
    let at_laptop = sign_in(&g, &laptop, 1, now - 20);
    let at_phone = sign_in(&g, &phone, 1, now - 10);

    let mine = auth(&g, "session", &at_laptop, "").expect("the laptop's session");
    assert_eq!(mine["label"], laptop.label(), "{mine}");
    let theirs = auth(&g, "session", &at_phone, "").expect("the phone's session");
    assert_eq!(theirs["label"], phone.label(), "{theirs}");

    let code = invite(&g.layout, "reviewer", Authority::Read);
    let options = auth(
        &g,
        "add-grant-options",
        &at_laptop,
        &serde_json::json!({ "invite": code }).to_string(),
    )
    .expect("add-grant options");
    assert_eq!(
        options["credential"],
        base64_id(&laptop),
        "the authenticator is asked for the laptop's own passkey"
    );

    let options = auth(&g, "reassert-options", &at_laptop, "")
        .expect("one session of this browser: no `sessionId` is needed");
    assert_eq!(options["credential"], base64_id(&laptop), "{options}");
    let options = auth(&g, "reassert-options", &at_phone, "").expect("the phone's own");
    assert_eq!(options["credential"], base64_id(&phone), "{options}");
}

/// ★ The defect: every read by the laptop restarted the idle clock of the phone's session, so
/// a phone left alone never went idle (and the laptop's own did).
#[test]
fn one_devices_reads_do_not_keep_anothers_session_alive() {
    let idle = 4;
    let g = gonk(Some(idle));
    let (laptop, phone, _) = one_person(&g);
    let now = identity::now_seconds();
    let at_laptop = sign_in(&g, &laptop, 1, now - 1);
    let at_phone = sign_in(&g, &phone, 1, now);
    for _ in 0..=idle + 2 {
        let answer = auth(&g, "session", &at_laptop, "").expect("the laptop stays signed in");
        assert_eq!(answer["label"], laptop.label(), "{answer}");
        std::thread::sleep(Duration::from_secs(1));
    }
    let now = identity::now_seconds();
    assert!(
        g.rp.identity(&at_phone, now).is_none(),
        "the phone, left alone, went idle"
    );
    assert!(g.rp.identity(&at_laptop, now).is_some());
}

#[test]
fn a_page_sign_out_ends_the_other_passkeys_sessions() {
    let g = gonk(None);
    let (laptop, phone, first) = one_person(&g);
    let now = identity::now_seconds();
    let at_laptop = sign_in(&g, &laptop, 1, now);
    let at_phone = sign_in(&g, &phone, 1, now);
    let again_phone = sign_in(&g, &phone, 2, now);
    let stranger = Authenticator::named("stranger");
    enroll(&g, &stranger, &invite(&g.layout, "other", Authority::Read));
    let at_stranger = sign_in(&g, &stranger, 1, now);

    let answer = auth(&g, "logout", &at_laptop, "").expect("signed out");
    assert!(answer.to_string().contains("signedOut"), "{answer}");
    let now = identity::now_seconds();
    for (token, device) in [
        (&at_laptop, "the laptop"),
        (&at_phone, "the phone"),
        (&again_phone, "the phone's second session"),
    ] {
        assert!(
            g.rp.identity(token, now).is_none(),
            "{device}: every device's session of {} ended",
            first.person
        );
    }
    assert!(
        g.rp.identity(&at_stranger, now).is_some(),
        "another person's session stands"
    );
}

/// ★ The defect: with `clients.json` unreadable no passkey of the person resolves, nothing is
/// ended, and the answer still said `signedOut: true`.
#[test]
fn a_sign_out_that_cannot_read_the_persons_passkeys_says_it_did_not_happen() {
    let g = gonk(None);
    let (laptop, _, first) = one_person(&g);
    let at_laptop = sign_in(&g, &laptop, 1, identity::now_seconds());
    let good = std::fs::read_to_string(g.layout.clients_json()).unwrap();
    std::fs::write(g.layout.clients_json(), "{ not json").unwrap();
    // The capability the door stamped the person into, as it reaches the resource.
    let capability = Capability::scoped(Vec::<String>::new())
        .with_principal(&first.person)
        .unwrap();
    let refused = issue(
        &g.kernel,
        &capability,
        Verb::Sink,
        "urn:iki:gonk:passkey:logout",
        "",
    )
    .expect_err("nothing was ended, so it is not a sign-out");
    assert!(
        refused.to_string().contains("the sign-out did not happen"),
        "{refused}"
    );
    std::fs::write(g.layout.clients_json(), good).unwrap();
    assert!(
        g.rp.identity(&at_laptop, identity::now_seconds()).is_some(),
        "the session it could not end is still live"
    );
}

fn add_grant_body(auth: &Authenticator, challenge: &str, origin: &str, count: u32) -> Value {
    serde_json::from_str(&auth.login_body(challenge, origin, count)).unwrap()
}

#[test]
fn add_grant_by_another_passkey_of_the_person_widens_and_ends_every_session() {
    let g = gonk(None);
    let (laptop, phone, first) = one_person(&g);
    let now = identity::now_seconds();
    let at_laptop = sign_in(&g, &laptop, 1, now);
    let again_laptop = sign_in(&g, &laptop, 2, now);
    let at_phone = sign_in(&g, &phone, 1, now);
    let stranger = Authenticator::named("stranger");
    enroll(&g, &stranger, &invite(&g.layout, "other", Authority::Read));
    let at_stranger = sign_in(&g, &stranger, 1, now);
    let code = invite(&g.layout, "reviewer", Authority::Read);

    // Another person's passkey answering for the laptop's session is refused.
    let challenge = g.rp.challenge(Purpose::AddGrant, now).unwrap();
    let mut body = add_grant_body(&stranger, &challenge, g.rp.origin(), 2);
    body["session"] = at_laptop.clone().into();
    body["invite"] = code.clone().into();
    let refused =
        g.rp.add_grant(body.to_string().as_bytes(), now)
            .expect_err("another person's passkey");
    assert!(
        refused.contains("not a passkey of the person you are signed in as"),
        "{refused}"
    );
    assert!(g.rp.identity(&at_laptop, now).is_some(), "nothing ended");

    // The phone, a passkey of the same person, answers for the laptop's session.
    let challenge = g.rp.challenge(Purpose::AddGrant, now).unwrap();
    let mut body = add_grant_body(&phone, &challenge, g.rp.origin(), 2);
    body["session"] = at_laptop.clone().into();
    body["invite"] = code.into();
    let (fresh, who, added) =
        g.rp.add_grant(body.to_string().as_bytes(), now)
            .expect("any passkey of the person may answer");
    assert_eq!(added, "reviewer");
    assert_eq!(who.enrolled.credential_id, base64_id(&phone));
    assert_eq!(who.enrolled.person, first.person);
    let passkeys = identity::read_passkeys(&g.layout).unwrap();
    for auth in [&laptop, &phone] {
        assert_eq!(
            passkeys[&base64_id(auth)].grants,
            ["writer", "reviewer"],
            "every passkey of the person holds the grant"
        );
    }
    for (token, which) in [
        (&at_laptop, "the laptop's session it answered for"),
        (&again_laptop, "the laptop's other session"),
        (&at_phone, "the phone's earlier session"),
    ] {
        assert!(g.rp.identity(token, now).is_none(), "{which} ended");
    }
    assert!(g.rp.identity(&fresh, now).is_some(), "the one new session");
    assert!(
        g.rp.identity(&at_stranger, now).is_some(),
        "another person's session stands"
    );
}

fn claim_issue(hub: &Kernel, cap: &Capability, args: &[(&str, &str)]) -> Result<(), Error> {
    let request = args.iter().fold(
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:default:claim").unwrap(),
        ),
        |req, (k, v)| req.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
    );
    block_on(hub.issue(request, cap)).map(|_| ())
}

fn acting_as(principal: &str, authority: Authority) -> Capability {
    Capability::scoped(grants_for("default", authority).unwrap())
        .with_principal(principal)
        .unwrap()
}

#[test]
fn the_claim_migration_leaves_a_passkey_iri_no_enrolled_passkey_names() {
    const ENROLLED: &str = "urn:iki:gonk:passkey:bGFwdG9w";
    const UNKNOWN: &str = "urn:iki:gonk:passkey:Z29uZQ";
    const PERSON: &str = "urn:iki:gonk:person:id:QUFBQUFBQUFB";
    let hub = ikigai_gonk::compose(DurableStore::in_memory().unwrap());
    let root = Capability::root();
    let append = Request::new(
        Verb::Sink,
        Iri::parse("urn:iki:ledger:default:append").unwrap(),
    )
    .with_arg("content", ArgRef::Inline(b"Work".to_vec()));
    block_on(hub.issue(append, &root)).unwrap();
    let claim = [("item", "1"), ("content", "gone"), ("lease", "10m")];
    claim_issue(&hub, &acting_as(UNKNOWN, Authority::Write), &claim).unwrap();

    let people: BTreeMap<String, String> = [(ENROLLED.to_string(), PERSON.to_string())].into();
    let issue_root = |request: Request| {
        block_on(hub.issue(request, &root))
            .map(|r| r.bytes.to_vec())
            .map_err(|e| e.to_string())
    };
    assert_eq!(
        ikigai_gonk::person_claims::run(&issue_root, &people).unwrap(),
        0,
        "nothing moved"
    );
    let renew = [
        ("item", "1"),
        ("content", "gone"),
        ("lease", "10m"),
        ("renew", "true"),
    ];
    claim_issue(&hub, &acting_as(UNKNOWN, Authority::Write), &renew)
        .expect("the claim is still the unknown passkey's");
    assert!(
        claim_issue(&hub, &acting_as(PERSON, Authority::Write), &renew).is_err(),
        "and no person's"
    );
}

#[test]
fn cursor_adoption_keeps_a_cursor_the_person_already_has() {
    const PASSKEY: &str = "urn:iki:gonk:passkey:bGFwdG9w";
    const PERSON: &str = "urn:iki:gonk:person:id:QUFBQUFBQUFB";
    const VIEW: &str = "urn:iki:gonk:dispatch:default:item:1";
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
    let root = Capability::root();
    let call = |cap: &Capability, verb: Verb, target: &str, args: &[(&str, &str)]| {
        let request = args.iter().fold(
            Request::new(verb, Iri::parse(target).unwrap()),
            |req, (k, v)| req.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
        );
        block_on(hub.issue(request, cap))
            .map(|r| serde_json::from_slice::<Value>(&r.bytes).unwrap_or(Value::Null))
            .unwrap()
    };
    let hand_back = |text: &str| {
        call(
            &root,
            Verb::Sink,
            "urn:iki:ledger:default:comment",
            &[("item", "1"), ("content", text), ("handback", "true")],
        );
    };
    let look = |cap: &Capability| call(cap, Verb::Source, VIEW, &[("as", "application/json")]);
    let acknowledge = |cap: &Capability, seen: &Value| {
        call(
            cap,
            Verb::Sink,
            VIEW,
            &[("cursor", seen["cursor"].as_str().unwrap())],
        );
    };
    call(
        &root,
        Verb::Sink,
        "urn:iki:ledger:default:append",
        &[("content", "A dispatcher task")],
    );
    let person = acting_as(PERSON, Authority::Read);
    let passkey = acting_as(PASSKEY, Authority::Read);
    hand_back("the first hand-back");
    // The person acknowledges the first hand-back under its own principal.
    acknowledge(&person, &look(&person));
    hand_back("the second hand-back");
    // Its passkey IRI, later, acknowledges both.
    let seen = look(&passkey);
    assert!(seen.to_string().contains("the second hand-back"), "{seen}");
    acknowledge(&passkey, &seen);
    let mine = look(&person);
    assert!(
        mine["items"].to_string().contains("the second hand-back"),
        "the person reads on from its OWN cursor, not its passkey's: {mine}"
    );
}
