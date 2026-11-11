//! Ledger [#3401](http://localhost:1060/l/default/item/3401): `passkey invite <grant> … --force`
//! writes the grant WHOLE from its flags, by design. A scope no flag names (`urn:cap:exec:git`,
//! for ikigai-repo's code links) was dropped, and restoring it with a second `grants.json`
//! write left a window where a live session under the grant was missing it. A repeatable
//! `--scope <urn>` goes into the same grant in the same write.
//!
//! Reproduced before the fix with the real binary: `--scope` was `unknown argument`.
//!
//! The real binary against a scratch config home: no server is started.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Homes {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Homes {
    fn new() -> Homes {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for dir in ["home", "cfg", "data"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Homes { _tmp: tmp, root }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--config-home")
            .arg(self.root.join("cfg"))
            .arg("--data-home")
            .arg(self.root.join("data"))
            .env("HOME", self.root.join("home"))
            .output()
            .expect("run ikigai-gonk")
    }

    fn grants_json(&self) -> PathBuf {
        self.root.join("cfg").join("gonk").join("grants.json")
    }

    fn grant(&self, name: &str) -> Vec<String> {
        grant_in(&self.grants_json(), name)
    }
}

fn grant_in(path: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc[name]
        .as_array()
        .unwrap_or_else(|| panic!("no grant `{name}` in {text}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn ok(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn force_with_scope_keeps_the_scope_and_still_drops_a_ledger_that_left_the_flags() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--ledger",
        "notes=write",
        "--scope",
        "urn:cap:exec:git",
    ]));
    let first = homes.grant("chris");
    assert!(first.contains(&"urn:cap:exec:git".to_string()), "{first:?}");
    assert!(first.iter().any(|s| s.contains("notes")), "{first:?}");

    // `notes` leaves the flags; the scope no flag names stays, in the same write.
    let stdout = ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        "urn:cap:exec:git",
        "--force",
    ]));
    let second = homes.grant("chris");
    assert!(
        second.contains(&"urn:cap:exec:git".to_string()),
        "{second:?}"
    );
    assert!(
        !second.iter().any(|s| s.contains("notes")),
        "a ledger that left the flags leaves the grant: {second:?}"
    );
    assert!(
        !stdout.contains("- urn:cap:exec:git"),
        "the kept scope is not reported removed: {stdout}"
    );

    // Without `--scope`, `--force` drops it: the grant is exactly the flags.
    ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--force",
    ]));
    assert!(!homes
        .grant("chris")
        .contains(&"urn:cap:exec:git".to_string()));
}

#[test]
fn a_malformed_or_refused_scope_is_refused_with_nothing_written() {
    let homes = Homes::new();
    ok(&homes.run(&["passkey", "invite", "chris", "--ledger", "default=write"]));
    let before = std::fs::read_to_string(homes.grants_json()).unwrap();
    for bad in [
        "exec:git",
        "urn:cap:",
        "urn:cap:exec git",
        "urn:cap::git",
        "urn:cap:exec:*",
        "urn:iki:gonk:door:refused",
    ] {
        let out = homes.run(&[
            "passkey",
            "invite",
            "chris",
            "--ledger",
            "default=write",
            "--scope",
            bad,
            "--force",
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "`{bad}` must be refused");
        assert!(
            stderr.contains(&format!("`{bad}`")),
            "the refusal names `{bad}`: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(homes.grants_json()).unwrap(),
            before,
            "nothing written for `{bad}`"
        );
    }
}

#[test]
fn a_repeated_scope_and_one_a_flag_already_grants_are_stored_once() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        "urn:cap:exec:git",
        "--scope",
        "urn:cap:exec:git",
    ]));
    let grant = homes.grant("chris");
    assert_eq!(
        grant.iter().filter(|s| *s == "urn:cap:exec:git").count(),
        1,
        "{grant:?}"
    );
    // One the `--ledger` flag already grants is de-duplicated silently, too.
    let from_flag = grant
        .iter()
        .find(|s| s.starts_with("urn:cap:"))
        .unwrap()
        .clone();
    ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        "urn:cap:exec:git",
        "--scope",
        &from_flag,
    ]));
    let again = homes.grant("chris");
    assert_eq!(again, grant, "the same grant: no change, no --force needed");
}

#[test]
fn passkey_list_shows_the_grant_scopes_as_stored() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "passkey",
        "invite",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        "urn:cap:exec:git",
    ]));
    // A passkey enrolled under it, as `clients.json` holds one (enrolling needs a browser).
    std::fs::write(
        homes.root.join("cfg").join("gonk").join("clients.json"),
        r#"{"passkeys": {"Q0hSSVM": {"grant": "chris", "label": "chris-laptop",
            "public_key": "AAAA", "sign_count": 0}}}"#,
    )
    .unwrap();
    let stdout = ok(&homes.run(&["passkey", "list"]));
    for scope in homes.grant("chris") {
        assert!(stdout.contains(&scope), "`{scope}` listed: {stdout}");
    }
    assert!(stdout.contains("chris-laptop"), "{stdout}");
}
