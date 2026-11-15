//! A current-state fact behind the rules-as-of design, not a historical API test.
use ikigai_gonk::{grants, quic};

#[test]
fn forced_grant_replacement_keeps_only_current_scopes_without_provenance() {
    let home = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(home.path());
    let before = grants::grants_for("default", grants::Authority::Write).unwrap();
    let after = grants::grants_for("default", grants::Authority::Read).unwrap();
    quic::put_grant(&layout, "worker", &before, false).unwrap();
    let original = std::fs::read(layout.grants_json()).unwrap();
    assert!(quic::put_grant(&layout, "worker", &after, false).is_err());
    assert_eq!(std::fs::read(layout.grants_json()).unwrap(), original);

    quic::put_grant(&layout, "worker", &after, true).unwrap();
    assert_eq!(
        quic::read_grants(&layout.grants_json()).unwrap()["worker"],
        after
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(layout.grants_json()).unwrap()).unwrap();
    assert_eq!(stored, serde_json::json!({"worker": after}));
    assert_ne!(before, after);
}
