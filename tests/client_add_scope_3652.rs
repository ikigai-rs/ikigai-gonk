//! Ledger [#3652](http://localhost:1060/l/default/item/3652): `client add` had no `--scope`, so
//! a scope no other flag names (an alias graph's read, a purge without the delete
//! `--ledger L=purge` brings) meant rewriting `grants.json` by hand. It takes the invite's
//! contract (ledger #3401): a repeatable `--scope <urn>`, checked by `scope_spec`, written in
//! the same write, a repeat stored once. And `--print` says what the flags resolve to, and
//! what they would remove and add on an existing grant, minting and writing nothing.
//!
//! Reproduced before the fix with the real binary: `--scope` and `--print` were each
//! `client add: unknown argument`.
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

const GIT: &str = "urn:cap:exec:git";

#[test]
fn scope_round_trips_and_force_keeps_it_while_dropping_a_ledger() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--ledger",
        "notes=write",
        "--scope",
        GIT,
    ]));
    let first = homes.grant("chris");
    assert!(first.contains(&GIT.to_string()), "{first:?}");
    assert!(first.iter().any(|s| s.contains("notes")), "{first:?}");

    // The same flags again: the same grant, no --force needed.
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--ledger",
        "notes=write",
        "--scope",
        GIT,
    ]));
    assert_eq!(homes.grant("chris"), first);

    // `notes` leaves the flags; the scope no flag names stays, in the same write.
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        GIT,
        "--force",
    ]));
    let second = homes.grant("chris");
    assert!(second.contains(&GIT.to_string()), "{second:?}");
    assert!(!second.iter().any(|s| s.contains("notes")), "{second:?}");
}

#[test]
fn scope_alone_enrolls_the_client() {
    let homes = Homes::new();
    let stdout = ok(&homes.run(&["client", "add", "chris", "--scope", GIT]));
    assert_eq!(homes.grant("chris"), vec![GIT.to_string()]);
    assert!(stdout.contains("enrolled     grant `chris`"), "{stdout}");
}

#[test]
fn a_malformed_or_refused_scope_is_refused_with_nothing_written() {
    let homes = Homes::new();
    ok(&homes.run(&["client", "add", "chris", "--ledger", "default=write"]));
    let before = homes.snapshot();
    for bad in [
        "exec:git",
        "urn:cap:",
        "urn:cap:exec git",
        "urn:cap::git",
        "urn:cap:exec:*",
        "urn:iki:gonk:door:refused",
    ] {
        let out = homes.run(&[
            "client",
            "add",
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
        assert!(homes.snapshot() == before, "nothing written for `{bad}`");
    }
}

#[test]
fn a_repeated_scope_and_one_a_flag_already_grants_are_stored_once() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        GIT,
        "--scope",
        GIT,
    ]));
    let grant = homes.grant("chris");
    assert_eq!(grant.iter().filter(|s| *s == GIT).count(), 1, "{grant:?}");
    let from_flag = grant
        .iter()
        .find(|s| s.starts_with("urn:cap:") && *s != GIT)
        .unwrap()
        .clone();
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        GIT,
        "--scope",
        &from_flag,
    ]));
    assert_eq!(homes.grant("chris"), grant, "no change, no --force needed");
}

#[test]
fn print_on_an_empty_home_lists_the_scopes_and_writes_nothing() {
    let homes = Homes::new();
    let before = homes.snapshot();
    let stdout = ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--scope",
        GIT,
        "--print",
    ]));
    assert!(homes.snapshot() == before, "--print wrote something");
    assert!(stdout.contains(GIT), "{stdout}");
    assert!(stdout.contains("nothing was written"), "{stdout}");
    assert!(
        !stdout.contains("fingerprint"),
        "no bundle minted: {stdout}"
    );
}

#[test]
fn print_on_an_existing_grant_names_the_removes_and_adds_and_writes_nothing() {
    let homes = Homes::new();
    ok(&homes.run(&[
        "client",
        "add",
        "chris",
        "--ledger",
        "default=write",
        "--ledger",
        "notes=write",
    ]));
    let notes: Vec<String> = homes
        .grant("chris")
        .into_iter()
        .filter(|s| s.contains("notes"))
        .collect();
    assert!(!notes.is_empty());
    let before = homes.snapshot();
    // With and without --force, and with --rotate: still nothing written or minted.
    for extra in [&[][..], &["--force"][..], &["--rotate"][..]] {
        let mut args = vec![
            "client",
            "add",
            "chris",
            "--ledger",
            "default=write",
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
        for scope in &notes {
            assert!(
                stdout.contains(&format!("    removes  {scope}")),
                "{extra:?}: {stdout}"
            );
        }
        assert!(
            stdout.contains(&format!("    adds     {GIT}")),
            "{extra:?}: {stdout}"
        );
    }
    // A refused scope is refused under --print too: it would never be written.
    let out = homes.run(&[
        "client",
        "add",
        "chris",
        "--scope",
        "urn:cap:exec:*",
        "--print",
    ]);
    assert!(!out.status.success());
    assert!(homes.snapshot() == before);
}
