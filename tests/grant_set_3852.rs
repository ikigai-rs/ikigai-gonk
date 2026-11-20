//! Ledger [#3852](http://localhost:1060/l/default/item/3852): a grant a passkey or certificate
//! already holds had no verb to gain a scope without a side effect: `client add --force` mints
//! and enrolls a certificate, `passkey invite --force` mints an enrollment link. `grant set`
//! writes one grant's scopes into `grants.json` and nothing else, with the exact-scope rules
//! of the other writers and `client refresh --write`'s backup.
//!
//! Reproduced before the fix with the real binary: `ikigai-gonk grant set …` was
//! `unknown argument `grant``.
//!
//! The real binary against a scratch config home: no server is started.

use std::collections::BTreeMap;
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
        let text = std::fs::read_to_string(self.grants_json()).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        doc[name]
            .as_array()
            .unwrap_or_else(|| panic!("no grant `{name}` in {text}"))
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    }

    /// Every file under the scratch root, path to bytes: what "writes nothing" is checked
    /// against.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    out.insert(path.clone(), Vec::new());
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &mut out);
        out
    }
}

fn ok(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const GIT: &str = "urn:cap:exec:read:git";

/// Every path under the config home's `gonk/` directory that is not `grants.json`, one of its
/// backups or its lock: what "mints nothing else" is checked against.
fn others(homes: &Homes) -> Vec<PathBuf> {
    homes
        .snapshot()
        .into_keys()
        .filter(|path| path.starts_with(homes.root.join("cfg")))
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            !(name == "gonk"
                || name == "cfg"
                || name == "grants.json"
                || name == ".grants.json.lock"
                || name.starts_with("grants.json.bak-"))
        })
        .collect()
}

fn backups(homes: &Homes) -> Vec<PathBuf> {
    homes
        .snapshot()
        .into_keys()
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("grants.json.bak-")
        })
        .collect()
}

#[test]
fn a_new_grant_is_written_without_force_and_nothing_else_is_minted() {
    let homes = Homes::new();
    let stdout = ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--scope",
        GIT,
    ]));
    let grant = homes.grant("viewer");
    assert!(grant.contains(&GIT.to_string()), "{grant:?}");
    assert!(grant.iter().any(|s| s.contains("default")), "{grant:?}");
    assert!(stdout.contains("added"), "{stdout}");
    assert_eq!(
        others(&homes),
        Vec::<PathBuf>::new(),
        "minted something else"
    );
    assert!(backups(&homes).is_empty(), "no file existed, so no backup");
}

#[test]
fn the_flags_resolve_to_what_client_add_resolves_them_to() {
    let homes = Homes::new();
    let flags = [
        "--ledger",
        "default=write",
        "--worker",
        "notes",
        "--browse-graph",
        "read",
        "--ledger-graph",
        "default",
        "--link-in",
        "notes",
        "--scope",
        GIT,
    ];
    let mut args = vec!["grant", "set", "a"];
    args.extend_from_slice(&flags);
    ok(&homes.run(&args));
    let mut args = vec!["client", "add", "b"];
    args.extend_from_slice(&flags);
    ok(&homes.run(&args));
    assert_eq!(homes.grant("a"), homes.grant("b"));
}

#[test]
fn an_existing_grant_is_refused_without_force_naming_removes_and_adds() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--ledger",
        "notes=read",
    ]));
    let first = homes.grant("viewer");
    let notes: Vec<String> = first
        .iter()
        .filter(|s| s.contains("notes"))
        .cloned()
        .collect();
    assert!(!notes.is_empty(), "{first:?}");
    let before = homes.snapshot();
    let out = homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--scope",
        GIT,
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    for scope in &notes {
        assert!(
            stderr.contains(&format!("    removes  {scope}")),
            "{stderr}"
        );
    }
    assert!(stderr.contains(&format!("    adds     {GIT}")), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");
    assert!(
        homes.snapshot() == before,
        "a refused rewrite wrote something"
    );

    // The same flags as are stored: nothing to change, no --force, no write, no backup.
    ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "notes=read",
        "--ledger",
        "default=read",
    ]));
    assert!(
        homes.snapshot() == before,
        "an unchanged grant was rewritten"
    );
}

#[test]
fn force_replaces_the_grant_whole_with_a_backup_and_leaves_clients_alone() {
    let homes = Homes::new();
    // A certificate holds the grant, so there is a bundle and an enrollment to leave alone.
    ok(&homes.run(&[
        "client",
        "add",
        "viewer",
        "--ledger",
        "default=read",
        "--ledger",
        "notes=read",
    ]));
    let others_before: BTreeMap<PathBuf, Vec<u8>> = homes
        .snapshot()
        .into_iter()
        .filter(|(path, _)| others(&homes).contains(path))
        .collect();
    let grants_before = std::fs::read(homes.grants_json()).unwrap();
    let stdout = ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--scope",
        GIT,
        "--force",
    ]));
    let after = homes.grant("viewer");
    assert!(after.contains(&GIT.to_string()), "{after:?}");
    assert!(
        !after.iter().any(|s| s.contains("notes")),
        "whole: {after:?}"
    );
    assert!(stdout.contains("REPLACED"), "{stdout}");
    assert!(stdout.contains("NARROWING"), "{stdout}");
    let others_after: BTreeMap<PathBuf, Vec<u8>> = homes
        .snapshot()
        .into_iter()
        .filter(|(path, _)| others(&homes).contains(path))
        .collect();
    assert!(
        others_after == others_before,
        "clients.json or a bundle changed"
    );
    let backups = backups(&homes);
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(
        backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-grant-set"),
        "{backups:?}"
    );
    assert_eq!(std::fs::read(&backups[0]).unwrap(), grants_before);
}

#[test]
fn print_writes_nothing_new_or_existing() {
    let homes = Homes::new();
    let before = homes.snapshot();
    let stdout = ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--scope",
        GIT,
        "--print",
    ]));
    assert!(homes.snapshot() == before, "--print wrote something");
    assert!(stdout.contains(GIT), "{stdout}");
    assert!(stdout.contains("nothing was written"), "{stdout}");
    assert!(stdout.contains("is not in"), "{stdout}");

    ok(&homes.run(&[
        "grant",
        "set",
        "viewer",
        "--ledger",
        "default=read",
        "--ledger",
        "notes=read",
    ]));
    let before = homes.snapshot();
    for extra in [&[][..], &["--force"][..]] {
        let mut args = vec![
            "grant",
            "set",
            "viewer",
            "--ledger",
            "default=read",
            "--scope",
            GIT,
            "--print",
        ];
        args.extend_from_slice(extra);
        let stdout = ok(&homes.run(&args));
        assert!(
            homes.snapshot() == before,
            "{extra:?} --print wrote something"
        );
        assert!(stdout.contains("    removes  "), "{extra:?}: {stdout}");
        assert!(
            stdout.contains(&format!("    adds     {GIT}")),
            "{extra:?}: {stdout}"
        );
    }
}

#[test]
fn no_scope_and_a_refused_scope_write_nothing() {
    let homes = Homes::new();
    let before = homes.snapshot();
    assert!(!homes.run(&["grant", "set", "viewer"]).status.success());
    assert!(!homes
        .run(&["grant", "set", "viewer", "--scope", "urn:cap:exec:*"])
        .status
        .success());
    assert!(!homes
        .run(&[
            "grant",
            "set",
            "viewer",
            "--minutes",
            "5",
            "--ledger",
            "default=read"
        ])
        .status
        .success());
    assert!(homes.snapshot() == before, "a refusal wrote something");
}
