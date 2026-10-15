//! The store's host reserve is never a grant (ledger
//! [#1600](http://localhost:1062/l/default/item/1600)).
//!
//! `budget::STORE_RESERVE` (`urn:cap:store:reserve`, ledger #1598) is the token gonk gives its
//! own jobs so that, from the `ikigai-store` release carrying ledger #1002, their evaluations
//! use the slots the store keeps for the host. Written into `grants.json` for a certificate or a
//! passkey it would put a caller in that host class: out of the per-caller share and into the
//! slots kept for the backup. So every path that turns a grant into scopes refuses it, through
//! the one decision (`quic::grant_refusal`), and each path is asked here on its own.
//!
//! Nothing here touches a live gonk: every file is in a tempdir config home.

use std::collections::BTreeMap;

use ikigai_core::Capability;
use ikigai_gonk::budget::STORE_RESERVE;
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{identity, quic};

const CLEAN: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
const HOSTED: &str = "6f1c00000000000000000000000000000000000000000000000000000000ef01";

/// A read grant on `default`, plus the reserve.
fn with_reserve() -> Vec<String> {
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push(STORE_RESERVE.to_string());
    scopes
}

/// The refusal names the token and says it is the host's own, never a caller's.
fn assert_names_the_reserve(refusal: &str) {
    assert!(refusal.contains(STORE_RESERVE), "{refusal}");
    assert!(refusal.contains("host"), "{refusal}");
}

#[test]
fn a_grants_json_grant_of_the_reserve_is_refused_at_startup_and_per_use() {
    let grants: BTreeMap<String, Vec<String>> =
        [("ops".to_string(), with_reserve())].into_iter().collect();
    assert_names_the_reserve(&quic::check_grants(&grants).unwrap_err());
    assert_names_the_reserve(&quic::scopes_for_grant(&grants, "ops").unwrap_err());
    assert_names_the_reserve(&ikigai_gonk::trigger::reviewer_scopes(&grants, "ops").unwrap_err());
    // A future token in the reserve's own family is the same priority.
    let family = vec![format!("{STORE_RESERVE}:backup")];
    assert!(quic::grant_refusal("ops", &family).is_some());
}

/// ★ An edit to `grants.json` after startup refuses the identity it names and nothing else:
/// the QUIC door's per-connection decision and the passkey door's per-request one re-read the
/// file, so a grant holding the reserve refuses its own callers while every other grant in the
/// same file is still admitted. The door stays up.
#[test]
fn an_edit_granting_the_reserve_refuses_that_identity_and_keeps_the_door() {
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    std::fs::create_dir_all(config.path().join("gonk")).unwrap();
    let clean = grants_for("default", Authority::Read).unwrap();
    std::fs::write(
        layout.grants_json(),
        serde_json::json!({ "reader": clean, "ops": with_reserve() }).to_string(),
    )
    .unwrap();
    let grants = quic::read_grants(&layout.grants_json()).expect("the file parses");
    let enrolment = quic::parse_enrolment(&format!(
        r#"{{"clients": {{"{CLEAN}": "reader", "{HOSTED}": "ops"}}}}"#
    ))
    .unwrap();

    let (grant, capability) =
        quic::authority(&enrolment, &grants, CLEAN).expect("the clean grant is admitted");
    assert_eq!(grant, "reader");
    assert!(!capability.allows(STORE_RESERVE));
    assert_names_the_reserve(&quic::authority(&enrolment, &grants, HOSTED).unwrap_err());

    assert_eq!(identity::scopes_of(&layout, "reader").unwrap(), clean);
    assert_names_the_reserve(&identity::scopes_of(&layout, "ops").unwrap_err());
}

/// `client add` checks before it mints a key pair (`enrol_refusal`) and again as it writes
/// (`enrol`); a passkey invite writes the grant first (`put_grant`, through `identity::invite`).
/// Each refuses the reserve and writes nothing.
#[test]
fn client_add_and_a_passkey_invite_refuse_the_reserve_and_write_nothing() {
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    let scopes = with_reserve();

    assert_names_the_reserve(
        &quic::enrol_refusal(&layout, "laptop", None, &scopes, true).unwrap_err(),
    );
    assert_names_the_reserve(&quic::enrol(&layout, "laptop", CLEAN, &scopes, true).unwrap_err());
    assert_names_the_reserve(&quic::put_grant(&layout, "laptop", &scopes, true).unwrap_err());
    assert_names_the_reserve(
        &identity::invite(&layout, "laptop", &scopes, true, 60, 1_000).unwrap_err(),
    );
    assert!(
        !layout.grants_json().exists(),
        "a refused grant was written"
    );
    assert!(
        !layout.clients_json().exists(),
        "a refused enrollment was written"
    );
    assert!(
        !layout.invites_json().exists(),
        "a refused invite was written"
    );
}

/// ★ Why `urn:cap:store:*` is NOT refused for this item: no wildcard reaches the reserve. The
/// store decides the host class with `Capability::allows(CAP_RESERVE)` (ikigai-store PR 63,
/// `Caller::of`), and `allows` is an EXACT match, so a grant of the store wildcard does not
/// put its holder in the host class. If core ever makes `allows` match a held wildcard, this
/// fails, and `grant_refusal` must then refuse the wildcards that would cover the reserve.
#[test]
fn no_store_wildcard_reaches_the_reserve() {
    for wildcard in ["urn:cap:store:*", "urn:cap:*", "*"] {
        let held = Capability::scoped([wildcard]);
        assert!(
            !held.allows(STORE_RESERVE),
            "{wildcard} reaches the reserve"
        );
    }
}
