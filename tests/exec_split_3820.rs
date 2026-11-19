//! Ledger [#3820](http://localhost:1062/l/default/item/3820), the exec split: `ikigai-repo` 0.6.0
//! retired `urn:cap:exec:git` and `urn:cap:exec:gh` for `urn:cap:exec:read:git` and
//! `urn:cap:exec:read:gh`. At the switch gonk rewrites stored grants in place, and refuses the old
//! spelling afterwards with a sentence naming the new one.

use std::process::{Command, Output};

mod scratch_gonk;

use ikigai_core::{Iri, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority, CAP_EXEC_READ_GH, CAP_EXEC_READ_GIT};
use ikigai_gonk::quic;
use ikigai_gonk::refresh;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

fn items() -> Request {
    Request::new(Verb::Source, Iri::parse("urn:iki:ledger:items").unwrap())
}

fn backups(layout: &quic::Layout, why: &str) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(layout.dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(&format!("-{why}"))
        })
        .collect();
    found.sort();
    found
}

/// ★ A gonk started over a `grants.json` holding the old spellings rewrites them in place, at
/// their positions, keeps every other token and field, backs the file up, and admits the client;
/// a later hand edit back to the old spelling refuses the client's next connection, naming the
/// new one.
#[test]
fn the_switch_rewrites_stored_grants_and_the_old_spelling_is_refused_after() {
    let write = grants_for("default", Authority::Write).unwrap();
    let before = std::sync::Mutex::new(String::new());
    let gonk = Gonk::with_quic_prepared("", &|config, _home| {
        let layout = quic::Layout::in_config_home(config);
        let mut alpha = vec!["urn:cap:exec:git".to_string()];
        alpha.extend(write.clone());
        alpha.push("urn:cap:exec:gh".to_string());
        let doc = serde_json::json!({
            "alpha": alpha,
            // Already holding the new spelling: the old one goes, the new one is held once.
            "both": ["urn:cap:exec:read:git", "urn:cap:exec:git"],
            // The object form keeps its other fields.
            "obj": { "scopes": ["urn:cap:exec:gh"], "note": "by hand" },
        });
        let text = serde_json::to_string_pretty(&doc).unwrap() + "\n";
        std::fs::write(layout.grants_json(), &text).unwrap();
        *before.lock().unwrap() = text;
    });

    let stored = quic::read_grants(&gonk.layout.grants_json()).unwrap();
    let mut alpha = vec![CAP_EXEC_READ_GIT.to_string()];
    alpha.extend(write.clone());
    alpha.push(CAP_EXEC_READ_GH.to_string());
    assert_eq!(stored["alpha"], alpha, "rewritten in place");
    assert_eq!(stored["both"], [CAP_EXEC_READ_GIT]);
    assert_eq!(stored["obj"], [CAP_EXEC_READ_GH]);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(gonk.layout.grants_json()).unwrap()).unwrap();
    assert_eq!(doc["obj"]["note"], "by hand");
    let backup = backups(&gonk.layout, "exec-split");
    assert_eq!(backup.len(), 1, "one backup");
    assert_eq!(
        std::fs::read_to_string(&backup[0]).unwrap(),
        *before.lock().unwrap(),
        "the backup is the file as it was"
    );
    let log = gonk.log();
    assert!(
        log.contains("grant `alpha`: rewrote urn:cap:exec:git to urn:cap:exec:read:git"),
        "{log}"
    );

    let client = gonk.dial_quic().expect("the rewritten grant admits alpha");
    client.issue(items()).expect("alpha reads");
    drop(client);

    // What the next start runs finds nothing to rewrite and makes no second backup.
    assert_eq!(refresh::respell_exec(&gonk.layout, 1).unwrap(), None);
    assert_eq!(backups(&gonk.layout, "exec-split").len(), 1);

    // A hand edit back to the old spelling, after the switch: refused at the next connection.
    let mut edited: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(gonk.layout.grants_json()).unwrap()).unwrap();
    edited["alpha"][0] = "urn:cap:exec:git".into();
    std::fs::write(gonk.layout.grants_json(), edited.to_string()).unwrap();
    let refused = gonk
        .dial_quic()
        .map_err(|e| e.to_string())
        .and_then(|client| client.issue(items()).map_err(|e| format!("{e:?}")));
    assert!(
        refused.is_err(),
        "a retired spelling admitted a QUIC client"
    );
    // The client is told only that it is not admitted; the operator's log says why.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let log = gonk.log() + &gonk.segments();
        if log.contains("urn:cap:exec:git") && log.contains("`urn:cap:exec:read:git` instead") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the refusal names the new spelling: {log}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn gonk_cli(config: &std::path::Path, home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .arg("--config-home")
        .arg(config)
        .env("HOME", home)
        .output()
        .expect("run ikigai-gonk")
}

/// `client refresh` lists the same rewrite on a dry run, writing nothing, and `--write` makes it
/// with a `-refresh` backup; a second run has nothing to do.
#[test]
fn client_refresh_lists_and_writes_the_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("cfg");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(config.join("gonk")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let layout = quic::Layout::in_config_home(&config);
    let mut held = grants_for("default", Authority::Read).unwrap();
    held.push("urn:cap:exec:git".to_string());
    let text = serde_json::to_string_pretty(&serde_json::json!({ "reader": held })).unwrap();
    std::fs::write(layout.grants_json(), &text).unwrap();

    let dry = gonk_cli(&config, &home, &["client", "refresh"]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let out = String::from_utf8_lossy(&dry.stdout);
    assert!(
        out.contains("  rewrites urn:cap:exec:git -> urn:cap:exec:read:git"),
        "{out}"
    );
    assert!(out.contains("Dry run: nothing written"), "{out}");
    assert_eq!(std::fs::read_to_string(layout.grants_json()).unwrap(), text);

    let wrote = gonk_cli(&config, &home, &["client", "refresh", "--write"]);
    assert!(
        wrote.status.success(),
        "{}",
        String::from_utf8_lossy(&wrote.stderr)
    );
    let mut want = grants_for("default", Authority::Read).unwrap();
    want.push(CAP_EXEC_READ_GIT.to_string());
    assert_eq!(
        quic::read_grants(&layout.grants_json()).unwrap()["reader"],
        want
    );
    assert_eq!(backups(&layout, "refresh").len(), 1);

    let again = refresh::refresh(&layout, true, 0).unwrap();
    assert_eq!(again.respelled(), 0);
    assert!(!again.wrote);
}

/// `respell_exec` on a file with nothing to rewrite, or no file, touches nothing.
#[test]
fn nothing_to_rewrite_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(tmp.path());
    assert_eq!(refresh::respell_exec(&layout, 0).unwrap(), None);
    std::fs::create_dir_all(layout.dir()).unwrap();
    let text = serde_json::json!({ "r": [CAP_EXEC_READ_GIT] }).to_string();
    std::fs::write(layout.grants_json(), &text).unwrap();
    assert_eq!(refresh::respell_exec(&layout, 0).unwrap(), None);
    assert_eq!(std::fs::read_to_string(layout.grants_json()).unwrap(), text);
    assert!(backups(&layout, "exec-split").is_empty());
}
