//! Ledger [#313](http://localhost:1062/l/default/item/313): `passkey list` and
//! `passkey revoke`, mirroring `client list` and `client remove`.
//!
//! Before this the only way to see who was enrolled, or to remove someone, was to edit
//! `clients.json` by hand: no record of who was removed or when, and nothing validated the
//! edit. The real binary against a scratch config home; no server is started.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

struct Home {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Home {
    fn new() -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("cfg/gonk")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        Home { _tmp: tmp, root }
    }

    fn gonk_dir(&self) -> PathBuf {
        self.root.join("cfg/gonk")
    }

    fn clients(&self) -> PathBuf {
        self.gonk_dir().join("clients.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--config-home")
            .arg(self.root.join("cfg"))
            .env("HOME", self.root.join("home"))
            .output()
            .expect("run ikigai-gonk")
    }
}

fn passkey(grant: &str, label: &str) -> Value {
    json!({ "grant": grant, "label": label, "public_key": "AAAA", "sign_count": 3 })
}

fn seed(path: &Path) {
    let doc = json!({
        "clients": { "ab12": { "grant": "laptop" } },
        "passkeys": {
            "Y3JlZC1hbGljZQ": passkey("alice", "alice"),
            "Y3JlZC1ib2I": passkey("bob", "Touch ID"),
            "Y3JlZC1ib2IyIA": passkey("bob", "Touch ID"),
        }
    });
    std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn list_names_every_passkey_with_its_grant_and_iri() {
    let home = Home::new();
    let out = home.run(&["passkey", "list"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("no passkeys"),
        "{}",
        text(&out.stdout)
    );

    seed(&home.clients());
    let out = home.run(&["passkey", "list"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let listed = text(&out.stdout);
    for expected in [
        "alice",
        "Touch ID",
        "Y3JlZC1hbGljZQ",
        "Y3JlZC1ib2IyIA",
        "urn:iki:gonk:passkey:Y3JlZC1hbGljZQ",
    ] {
        assert!(listed.contains(expected), "{expected}: {listed}");
    }
}

#[test]
fn revoke_removes_one_passkey_and_writes_an_audit_line() {
    let home = Home::new();
    seed(&home.clients());
    let out = home.run(&["passkey", "revoke", "alice"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("revoked"),
        "{}",
        text(&out.stdout)
    );

    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(home.clients()).unwrap()).unwrap();
    let passkeys = doc["passkeys"].as_object().unwrap();
    assert!(!passkeys.contains_key("Y3JlZC1hbGljZQ"), "{doc}");
    assert_eq!(passkeys.len(), 2, "the others are untouched: {doc}");
    assert_eq!(
        doc["clients"]["ab12"]["grant"], "laptop",
        "QUIC clients untouched: {doc}"
    );

    let audit = std::fs::read_to_string(home.gonk_dir().join("passkeys.log")).unwrap();
    let lines: Vec<&str> = audit.lines().collect();
    assert_eq!(lines.len(), 1, "{audit}");
    assert!(
        lines[0].contains(" gonk:PasskeyRevoked urn:iki:gonk:passkey:Y3JlZC1hbGljZQ ")
            && lines[0].contains(" grant=alice")
            && lines[0].contains(" label=alice"),
        "{audit}"
    );
    assert!(
        lines[0].len() > 24 && lines[0].as_bytes()[23] == b'Z',
        "RFC 3339 with milliseconds first: {audit}"
    );

    // Gone is gone: a second revoke is refused and writes nothing.
    let again = home.run(&["passkey", "revoke", "alice"]);
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("passkey list"),
        "{}",
        text(&again.stderr)
    );
    let audit_after = std::fs::read_to_string(home.gonk_dir().join("passkeys.log")).unwrap();
    assert_eq!(audit_after, audit);
}

#[test]
fn an_ambiguous_label_is_refused_and_a_credential_id_is_exact() {
    let home = Home::new();
    seed(&home.clients());
    let before = std::fs::read(home.clients()).unwrap();
    let out = home.run(&["passkey", "revoke", "Touch ID"]);
    assert!(!out.status.success());
    let why = text(&out.stderr);
    assert!(
        why.contains("Y3JlZC1ib2I")
            && why.contains("Y3JlZC1ib2IyIA")
            && why.contains("--credential"),
        "{why}"
    );
    assert_eq!(
        std::fs::read(home.clients()).unwrap(),
        before,
        "nothing written"
    );

    let out = home.run(&["passkey", "revoke", "--credential", "Y3JlZC1ib2IyIA"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(home.clients()).unwrap()).unwrap();
    let passkeys = doc["passkeys"].as_object().unwrap();
    assert!(passkeys.contains_key("Y3JlZC1ib2I") && !passkeys.contains_key("Y3JlZC1ib2IyIA"));
}

/// ★ A malformed `clients.json` refuses the change: nothing is written, no audit line claims a
/// revocation that did not happen, and the file the doors read is left exactly as it was.
#[test]
fn a_malformed_clients_file_refuses_the_change() {
    for broken in [
        "{ not json".to_string(),
        r#"{"passkeys": {"Y3JlZC1hbGljZQ": {"label": "alice"}}}"#.to_string(),
        r#"{"passkeys": [], "clients": {}}"#.to_string(),
        r#"{"clients": {"ab12": 7}, "passkeys": {"Y3JlZC1hbGljZQ": {"grant": "alice", "label": "alice", "public_key": "AAAA"}}}"#.to_string(),
    ] {
        let home = Home::new();
        std::fs::write(home.clients(), &broken).unwrap();
        let out = home.run(&["passkey", "revoke", "alice"]);
        assert!(!out.status.success(), "{broken}: {}", text(&out.stdout));
        assert_eq!(std::fs::read_to_string(home.clients()).unwrap(), broken);
        assert!(
            !home.gonk_dir().join("passkeys.log").exists(),
            "{broken}: no audit line"
        );
    }
}
