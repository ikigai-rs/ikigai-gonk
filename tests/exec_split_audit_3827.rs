//! Ledger [#3827](http://localhost:1062/l/default/item/3827): the exec split's switch (ledger
//! #3820) is visible to the operator. Each scope it rewrites in `grants.json` becomes one entry of
//! gonk's own log segment (where the passkey sign-ins land) naming the grant, the old scope and
//! the new one; the start that rewrote says so in one summary line on stderr; and a start with
//! nothing to rewrite writes neither.

mod scratch_gonk;

use std::time::{Duration, Instant};

use ikigai_gonk::grants::{grants_for, Authority, CAP_EXEC_READ_GH, CAP_EXEC_READ_GIT};
use ikigai_gonk::quic;
use ikigai_gonk::refresh::EXEC_RESPELLED_CLASS;
use scratch_gonk::Gonk;

/// The segment lines of class [`EXEC_RESPELLED_CLASS`].
fn respelled(gonk: &Gonk) -> Vec<String> {
    let class = format!("<{EXEC_RESPELLED_CLASS}>");
    gonk.segments()
        .lines()
        .filter(|l| l.contains(&class))
        .map(str::to_string)
        .collect()
}

/// ★ A grants file holding both old spellings, in two grants: after the start the file holds the
/// new ones, the segment holds one line per rewritten scope naming the grant, the old scope and
/// the new, stderr holds one summary, and a second start writes no line and no summary.
#[test]
fn each_rewritten_scope_is_one_segment_line_and_a_second_start_writes_none() {
    let write = grants_for("default", Authority::Write).unwrap();
    let mut gonk = Gonk::with_quic_prepared("", &|config, _home| {
        let layout = quic::Layout::in_config_home(config);
        let mut alpha = write.clone();
        alpha.push("urn:cap:exec:git".to_string());
        alpha.push("urn:cap:exec:gh".to_string());
        let doc = serde_json::json!({
            "alpha": alpha,
            "beta": ["urn:cap:ledger:read:default", "urn:cap:exec:git"],
            "gamma": [CAP_EXEC_READ_GIT],
        });
        std::fs::write(layout.grants_json(), doc.to_string()).unwrap();
    });

    let stored = quic::read_grants(&gonk.layout.grants_json()).unwrap();
    let mut alpha = write.clone();
    alpha.push(CAP_EXEC_READ_GIT.to_string());
    alpha.push(CAP_EXEC_READ_GH.to_string());
    assert_eq!(stored["alpha"], alpha);
    assert_eq!(
        stored["beta"],
        ["urn:cap:ledger:read:default", CAP_EXEC_READ_GIT]
    );
    assert_eq!(stored["gamma"], [CAP_EXEC_READ_GIT], "nothing to rewrite");

    let deadline = Instant::now() + Duration::from_secs(10);
    let lines = loop {
        let lines = respelled(&gonk);
        if lines.len() >= 3 || Instant::now() > deadline {
            break lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(lines.len(), 3, "one line per rewritten scope: {lines:#?}");
    for (grant, from, to) in [
        ("alpha", "urn:cap:exec:git", CAP_EXEC_READ_GIT),
        ("alpha", "urn:cap:exec:gh", CAP_EXEC_READ_GH),
        ("beta", "urn:cap:exec:git", CAP_EXEC_READ_GIT),
    ] {
        let fields = [
            format!("grant={grant}"),
            format!("from={from}"),
            format!("to={to}"),
        ];
        assert!(
            lines
                .iter()
                .any(|l| fields.iter().all(|f| l.split(' ').any(|c| c == f))),
            "no line for {grant} {from} -> {to}: {lines:#?}"
        );
    }
    assert!(
        !lines.iter().any(|l| l.contains("grant=gamma")),
        "{lines:#?}"
    );
    let log = gonk.log();
    assert_eq!(
        log.matches("ikigai-gonk: migrated 3 exec scopes in 2 grants to urn:cap:exec:read:*")
            .count(),
        1,
        "{log}"
    );

    gonk.child.kill().unwrap();
    gonk.child.wait().unwrap();
    gonk.restart();
    // The restarted server is up; its start wrote its own segment lines by now.
    assert_eq!(respelled(&gonk), lines, "a second start writes no line");
    let log = gonk.log();
    assert!(!log.contains("migrated"), "{log}");
    assert!(!log.contains("rewrote"), "{log}");
}

/// ★ Every load that turns a grant into scopes refuses a retired spelling (one written after the
/// switch, by hand) with a sentence naming the spelling that replaced it: the startup check of
/// `grants.json`, `gonk.review.grant`'s resolve, the QUIC door's per-connection resolve, and the
/// passkey door's.
#[test]
fn every_load_refuses_the_old_spelling_naming_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(dir.path());
    std::fs::create_dir_all(layout.dir()).unwrap();
    for (old, new) in ikigai_gonk::grants::RETIRED_EXEC {
        let doc = serde_json::json!({ "builder": ["urn:cap:ledger:read:default", old] });
        std::fs::write(layout.grants_json(), doc.to_string()).unwrap();
        let grants = quic::read_grants(&layout.grants_json()).unwrap();
        let instead = format!("Name `{new}` instead");
        for (site, refused) in [
            ("startup", quic::check_grants(&grants).unwrap_err()),
            (
                "gonk.review.grant",
                ikigai_gonk::trigger::reviewer_scopes(&grants, "builder").unwrap_err(),
            ),
            (
                "QUIC",
                quic::scopes_for_grant(&grants, "builder").unwrap_err(),
            ),
            (
                "passkey",
                ikigai_gonk::identity::scopes_of(&layout, "builder").unwrap_err(),
            ),
        ] {
            assert!(refused.contains(old), "{site}: {refused}");
            assert!(refused.contains(&instead), "{site}: {refused}");
        }
    }
}

/// ★ A start that fails AFTER the switch rewrote `grants.json` has already recorded the
/// migration: the next start finds nothing to rewrite, so an entry not written by the failed one
/// would never be written (ledger #3827, PR 441's review). The failure here is the startup check
/// refusing another grant's broad store token, which runs after the rewrite; then that grant is
/// removed, and a clean start writes no second set of lines.
#[test]
fn a_start_that_fails_after_the_rewrite_has_already_written_the_lines() {
    let mut gonk = Gonk::with_quic("");
    gonk.child.kill().unwrap();
    gonk.child.wait().unwrap();

    let write = grants_for("default", Authority::Write).unwrap();
    let mut alpha = write.clone();
    alpha.push("urn:cap:exec:git".to_string());
    alpha.push("urn:cap:exec:gh".to_string());
    let doc = serde_json::json!({
        "alpha": alpha,
        "beta": ["urn:cap:ledger:read:default", "urn:cap:exec:git"],
        "broad": [ikigai_store::CAP_READ],
    });
    std::fs::write(gonk.layout.grants_json(), doc.to_string()).unwrap();

    let refused = gonk
        .try_restart()
        .expect_err("the broad grant stops the start");
    assert!(refused.contains("broad"), "{refused}");
    let stored = quic::read_grants(&gonk.layout.grants_json()).unwrap();
    assert_eq!(
        stored["beta"],
        ["urn:cap:ledger:read:default", CAP_EXEC_READ_GIT],
        "the switch ran before the refusal"
    );
    let lines = respelled(&gonk);
    assert_eq!(
        lines.len(),
        3,
        "written before the start failed: {lines:#?}"
    );
    for (grant, from) in [
        ("alpha", "urn:cap:exec:git"),
        ("alpha", "urn:cap:exec:gh"),
        ("beta", "urn:cap:exec:git"),
    ] {
        assert!(
            lines.iter().any(|l| {
                let columns: Vec<&str> = l.split(' ').collect();
                columns.contains(&format!("grant={grant}").as_str())
                    && columns.contains(&format!("from={from}").as_str())
            }),
            "no line for {grant} {from}: {lines:#?}"
        );
    }

    let mut grants = stored;
    grants.remove("broad");
    std::fs::write(
        gonk.layout.grants_json(),
        serde_json::to_string(&grants).unwrap(),
    )
    .unwrap();
    gonk.restart();
    assert_eq!(respelled(&gonk), lines, "the clean start writes no line");
    assert!(!gonk.log().contains("migrated"), "{}", gonk.log());
}
