//! Current-state facts the roles-and-assignments design relies on (ledger #3749,
//! docs/grants/roles-and-assignments.md), not a test of a role or assignment API.
use ikigai_core::Capability;
use ikigai_gonk::grants::{self, Authority};
use ikigai_gonk::quic::GrantChange;
use ikigai_script::authority::{self, Act};

/// "Narrowed by its parent" cannot be the kernel's attenuation of the child's token list: that
/// is a literal set intersection, and a worker's `claim`, `comment` and `session` tokens are
/// not spelled in a writer's grant, though the writer's `urn:cap:ledger:write:{L}` covers them.
#[test]
fn literal_attenuation_drops_what_a_parents_write_token_implies() {
    let write = grants::grants_for("default", Authority::Write).unwrap();
    let worker = grants::grants_for("default", Authority::Worker).unwrap();
    let literal = Capability::scoped(write.clone()).attenuate(worker.clone());
    for verb in ["claim", "comment", "session"] {
        let token = format!("urn:cap:ledger:{verb}:default");
        assert!(worker.contains(&token));
        assert!(!literal.allows(&token), "attenuation dropped {token}");
        assert!(grants::implied_by(&token, &write), "write covers {token}");
    }
    assert!(worker.iter().all(|token| grants::implied_by(token, &write)));
}

/// The same for a script namespace: `team-*` covers `team-report` by ikigai-script's own rule,
/// and attenuation, which matches exactly, keeps neither the name nor the namespace's spelling.
#[test]
fn literal_attenuation_drops_a_script_name_its_namespace_covers() {
    let parent =
        Capability::scoped(grants::script_grants("team-*", grants::ScriptAuthority::Run).unwrap());
    let child = grants::script_grants("team-report", grants::ScriptAuthority::Run).unwrap();
    assert!(authority::holds(&parent, Act::Run, "team-report"));
    let literal = parent.attenuate(child);
    assert!(!literal.allows("urn:cap:script:run:team-report"));
    assert!(!authority::holds(&literal, Act::Run, "team-report"));
}

/// The zero-diff proof compares grants as SETS, the way the door and `--force`'s refusal do: a
/// role expansion that yields the same tokens in another order is no change.
#[test]
fn a_grant_reordered_is_no_change() {
    let mut expanded = grants::grants_for("default", Authority::Worker).unwrap();
    let stored = serde_json::json!(expanded.clone());
    expanded.reverse();
    let change = GrantChange::against(Some(&stored), &expanded);
    assert!(change.existed);
    assert!(!change.changes());
    assert!(!change.narrows());
}
