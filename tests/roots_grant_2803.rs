use std::collections::BTreeMap;

use ikigai_gonk::{identity, quic, roots};

#[test]
fn browse_roots_authority_is_refused_at_startup_and_for_quic_and_passkeys() {
    const FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    std::fs::create_dir_all(config.path().join("gonk")).unwrap();
    let scopes = vec![
        roots::CAP_ROOTS.to_string(),
        "urn:cap:browse:read:*".to_string(),
    ];
    let grants = BTreeMap::from([("reader".to_string(), scopes.clone())]);
    std::fs::write(layout.grants_json(), serde_json::to_vec(&grants).unwrap()).unwrap();
    let enrolment = quic::parse_enrolment(&format!(
        r#"{{"clients": {{"{FP}": {{"grant": "reader"}}}}}}"#
    ))
    .unwrap();

    // The same file is checked at startup, per QUIC connection, and per passkey request.
    let startup = quic::check_grants(&grants);
    let connection = quic::authority(&enrolment, &grants, FP);
    let passkey = identity::scopes_of(&layout, "reader");
    assert!(
        startup.is_err() && connection.is_err() && passkey.is_err(),
        "roots grant must be refused: startup={startup:?}, QUIC={connection:?}, passkey={passkey:?}"
    );
    for refusal in [
        startup.unwrap_err(),
        connection.unwrap_err(),
        passkey.unwrap_err(),
    ] {
        assert!(refusal.contains(roots::CAP_ROOTS), "{refusal}");
    }

    // Browse read remains a legitimate grant without the owner's roots authority.
    let read_only = vec!["urn:cap:browse:read:*".to_string()];
    assert_eq!(quic::grant_refusal("reader", &read_only), None);
}
