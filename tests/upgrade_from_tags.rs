//! Upgrading from every tagged gonk that wrote `clients.json` without people (ledger #4368).
//!
//! The fixtures are the shapes ledger-v0.18.0, v0.20.0, v0.24.1, v0.24.2 and v0.25.0 wrote:
//! a passkey with one `grant` (through v0.24.2) or a `grants` list (v0.25.0), sessions with
//! `grant` or `grants`, grants spelled `urn:cap:exec:git`, and an `invites.json`. Each is
//! started through this server's migrations, and the migrated file must still start the OLD
//! server: the rules [`old_reader_single`] and [`old_reader_list`] are those tags' own.

use std::collections::BTreeMap;

use ikigai_gonk::identity::{self, Which};
use ikigai_gonk::quic::Layout;
use serde_json::{json, Value};
use sha2::Digest;

const NOW: u64 = 1_790_000_000;

/// `clients.json` as one tag wrote it: two passkeys and a QUIC client.
fn clients(tag: &str) -> Value {
    let quic = json!({ "deadbeef": { "grant": "agent", "label": "worker" } });
    match tag {
        // Through v0.24.2 a passkey held ONE grant, as `grant`.
        "v0.18.0" | "v0.20.0" | "v0.24.1" | "v0.24.2" => json!({
            "clients": quic,
            "passkeys": {
                "bGFwdG9w": { "grant": "brian", "label": "laptop", "public_key": "AAAA", "sign_count": 7 },
                "cGhvbmU": { "grant": "reviewer", "public_key": "BBBB" }
            }
        }),
        // v0.25.0 wrote a `grants` list.
        "v0.25.0" => json!({
            "clients": quic,
            "passkeys": {
                "bGFwdG9w": { "grants": ["brian", "reviewer"], "label": "laptop", "public_key": "AAAA", "sign_count": 7 },
                "cGhvbmU": { "grants": ["reviewer"], "label": "phone", "public_key": "BBBB", "sign_count": 2 }
            }
        }),
        _ => unreachable!("{tag}"),
    }
}

fn grants_json() -> Value {
    json!({
        "brian": ["urn:cap:ledger:default", "urn:cap:exec:git"],
        "reviewer": ["urn:cap:ledger:default:read"],
        "agent": ["urn:cap:ledger:default"]
    })
}

/// `sessions.json`: before #3388 a session held `grant`; after it, `grants`.
fn sessions(tag: &str) -> Value {
    let hash = "a".repeat(64);
    let entry = if tag == "v0.25.0" {
        json!({ "principal": "urn:iki:gonk:passkey:bGFwdG9w", "grants": ["brian", "reviewer"],
                "issued": NOW - 60, "last_seen": NOW - 30, "expires": NOW + 3600 })
    } else {
        json!({ "principal": "urn:iki:gonk:passkey:bGFwdG9w", "grant": "brian",
                "issued": NOW - 60, "last_seen": NOW - 30, "expires": NOW + 3600 })
    };
    json!({ "format": 1, "log_seen": 0, "sessions": { hash: entry } })
}

fn invites() -> Value {
    let code = sha2::Sha256::digest(b"an-invite-code");
    let hex: String = code.iter().map(|b| format!("{b:02x}")).collect();
    json!({ hex: { "grant": "reviewer", "expires": NOW + 600 } })
}

const TAGS: [&str; 5] = ["v0.18.0", "v0.20.0", "v0.24.1", "v0.24.2", "v0.25.0"];

fn write(path: &std::path::Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn seeded(tag: &str) -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::in_config_home(dir.path());
    write(&layout.clients_json(), &clients(tag));
    write(&layout.grants_json(), &grants_json());
    write(&layout.sessions_json(), &sessions(tag));
    write(&layout.invites_json(), &invites());
    (dir, layout)
}

/// What a start runs on the files, in main's order: the exec respelling, then the passkeys'
/// people (inside [`identity::Passkeys::new_at`]), which also restores the sessions.
fn start(layout: &Layout) -> identity::Passkeys {
    ikigai_gonk::refresh::respell_exec(layout, NOW * 1000).unwrap();
    identity::Passkeys::new_at(layout.clone(), 1062, NOW)
}

/// ledger-v0.18.0 through v0.24.2's reader: every passkey entry needs a `grant` name.
fn old_reader_single(doc: &Value) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (id, entry) in doc["passkeys"].as_object().into_iter().flatten() {
        let grant = entry
            .get("grant")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("passkey `{id}` has no `grant`"))?;
        entry
            .get("public_key")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("passkey `{id}` has no `public_key`"))?;
        out.insert(id.clone(), grant.to_string());
    }
    Ok(out)
}

/// ledger-v0.25.0's reader (`entry_grants`): `grants`, or `grant`, agreeing when both.
fn old_reader_list(doc: &Value) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut out = BTreeMap::new();
    for (id, entry) in doc["passkeys"].as_object().into_iter().flatten() {
        let single = entry
            .get("grant")
            .and_then(Value::as_str)
            .map(str::to_string);
        let listed = entry.get("grants").and_then(Value::as_array).map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });
        let grants = match (listed, single) {
            (Some(l), Some(s)) if l != [s.clone()] => {
                return Err(format!(
                    "passkey `{id}` has both `grant` and `grants`, and they disagree"
                ))
            }
            (Some(l), _) => l,
            (None, Some(s)) => vec![s],
            (None, None) => return Err(format!("passkey `{id}` has no `grants`")),
        };
        if grants.is_empty() {
            return Err(format!("passkey `{id}` names no grant"));
        }
        out.insert(id.clone(), grants);
    }
    Ok(out)
}

fn expected(tag: &str) -> (Vec<&'static str>, Vec<&'static str>, &'static str, u32, u32) {
    // laptop's grants, phone's grants, phone's label, laptop's and phone's counters
    if tag == "v0.25.0" {
        (vec!["brian", "reviewer"], vec!["reviewer"], "phone", 7, 2)
    } else {
        (vec!["brian"], vec!["reviewer"], "reviewer", 7, 0)
    }
}

/// ★ Every tag's files: grants, labels, keys and counters carried; a second start changes
/// nothing; and the migrated `clients.json` still starts that tag (claim 1).
#[test]
fn every_tag_upgrades_and_its_old_reader_still_starts() {
    for tag in TAGS {
        let (_dir, layout) = seeded(tag);
        let before = std::fs::read_to_string(layout.clients_json()).unwrap();
        drop(start(&layout));
        let (laptop, phone, phone_label, laptop_count, phone_count) = expected(tag);
        let got = identity::read_passkeys(&layout).unwrap();
        assert_eq!(got["bGFwdG9w"].grants, laptop, "{tag}");
        assert_eq!(got["cGhvbmU"].grants, phone, "{tag}");
        assert_eq!(got["bGFwdG9w"].label, "laptop", "{tag}");
        assert_eq!(got["cGhvbmU"].label, phone_label, "{tag}");
        assert_eq!(got["bGFwdG9w"].public_key, "AAAA", "{tag}");
        assert_eq!(got["cGhvbmU"].public_key, "BBBB", "{tag}");
        assert_eq!(got["bGFwdG9w"].sign_count, laptop_count, "{tag}");
        assert_eq!(got["cGhvbmU"].sign_count, phone_count, "{tag}");
        assert_ne!(
            got["bGFwdG9w"].person, got["cGhvbmU"].person,
            "{tag}: nothing merged"
        );
        assert_eq!(
            std::fs::read_to_string(identity::people_backup(&layout)).unwrap(),
            before,
            "{tag}: the backup is the file before"
        );
        let doc = read(&layout.clients_json());
        assert_eq!(
            doc["clients"]["deadbeef"]["grant"], "agent",
            "{tag}: QUIC kept"
        );

        // The migrated file still starts the tag that wrote it.
        let old = old_reader_list(&doc).unwrap_or_else(|e| panic!("{tag}: {e}"));
        assert_eq!(old["bGFwdG9w"], laptop, "{tag}");
        assert_eq!(old["cGhvbmU"], phone, "{tag}");
        if tag != "v0.25.0" {
            let old = old_reader_single(&doc).unwrap_or_else(|e| panic!("{tag}: {e}"));
            assert_eq!(old["bGFwdG9w"], "brian", "{tag}");
        }

        // The grants respelled, the session restored, the invite kept.
        let grants = read(&layout.grants_json());
        assert!(
            !grants["brian"].to_string().contains("urn:cap:exec:git\""),
            "{tag}: {grants}"
        );
        let stored = read(&layout.sessions_json());
        let session = &stored["sessions"]["a".repeat(64)];
        let held = session.get("grants").map_or(&session["grant"], |g| &g[0]);
        assert_eq!(held, "brian", "{tag}: the session is kept: {stored}");
        assert_eq!(read(&layout.invites_json()), invites(), "{tag}");

        // Idempotent: a second start writes nothing.
        let migrated = std::fs::read_to_string(layout.clients_json()).unwrap();
        drop(start(&layout));
        assert_eq!(
            std::fs::read_to_string(layout.clients_json()).unwrap(),
            migrated,
            "{tag}"
        );
        assert_eq!(identity::migrate_people(&layout).unwrap(), 0, "{tag}");
    }
}

/// ★ Claim 1, its second half: a CLI write by the new binary while the OLD server still runs
/// must leave a file the old server reads, holding the changed grants.
#[test]
fn a_cli_grant_change_is_mirrored_for_an_old_reader() {
    let (_dir, layout) = seeded("v0.25.0");
    drop(start(&layout));
    identity::add_grant(&layout, Which::Label("phone"), "brian", NOW * 1000).unwrap();
    let doc = read(&layout.clients_json());
    let old = old_reader_list(&doc).unwrap();
    assert_eq!(old["cGhvbmU"], ["reviewer", "brian"]);
    assert_eq!(old["bGFwdG9w"], ["brian", "reviewer"]);
    // Attached: the phone holds the laptop's person's grants, in both readers.
    identity::attach(&layout, Which::Label("phone"), "laptop", NOW * 1000).unwrap();
    identity::remove_grant(&layout, Which::Label("laptop"), "reviewer", NOW * 1000).unwrap();
    let doc = read(&layout.clients_json());
    let old = old_reader_list(&doc).unwrap();
    assert_eq!(old["cGhvbmU"], ["brian"]);
    assert_eq!(old["bGFwdG9w"], ["brian"]);
    assert_eq!(old_reader_single(&doc).unwrap()["cGhvbmU"], "brian");
    let new = identity::read_passkeys(&layout).unwrap();
    assert_eq!(new["cGhvbmU"].grants, ["brian"]);
}

/// ★ Claim 2: a grant change made by the new binary before its first start leaves the
/// backup holding the file from BEFORE people, so the documented rollback works.
#[test]
fn a_cli_write_before_the_first_start_keeps_a_pre_people_backup() {
    for tag in TAGS {
        let (_dir, layout) = seeded(tag);
        let before = std::fs::read_to_string(layout.clients_json()).unwrap();
        identity::add_grant(&layout, Which::Label("laptop"), "agent", NOW * 1000).unwrap();
        drop(start(&layout));
        let backup = std::fs::read_to_string(identity::people_backup(&layout)).unwrap();
        assert_eq!(
            backup, before,
            "{tag}: the backup is the file before people"
        );
        assert!(
            old_reader_list(&serde_json::from_str(&backup).unwrap()).is_ok(),
            "{tag}"
        );
    }
}

/// ★ Claim 3: `passkey attach phone --person laptop` before the first start. Both passkeys
/// must hold ONE list, a later change to it must not be undone by the other passkey, the
/// person keeps the laptop's label, and the next start has nothing left to warn about.
#[test]
fn an_attach_before_the_first_start_gives_one_person_one_grant_list() {
    let (_dir, layout) = seeded("v0.25.0");
    identity::attach(&layout, Which::Label("phone"), "laptop", NOW * 1000).unwrap();
    drop(start(&layout));
    let got = identity::read_passkeys(&layout).unwrap();
    assert_eq!(got["cGhvbmU"].person, got["bGFwdG9w"].person);
    assert_eq!(got["cGhvbmU"].grants, got["bGFwdG9w"].grants);
    identity::remove_grant(&layout, Which::Label("phone"), "reviewer", NOW * 1000).unwrap();
    let got = identity::read_passkeys(&layout).unwrap();
    assert_eq!(
        got["bGFwdG9w"].grants,
        ["brian"],
        "the laptop reads the person's list"
    );
    let doc = read(&layout.clients_json());
    for (id, entry) in doc["passkeys"].as_object().unwrap() {
        assert!(entry.get("person").is_some(), "{id} was migrated: {doc}");
    }
    let labels = identity::author_labels(&layout);
    assert_eq!(
        labels.get(&got["bGFwdG9w"].person).map(String::as_str),
        Some("laptop")
    );
}

/// ★ Claim 3, by hand: an old-form entry put back for a credential whose person exists. It is
/// pointed at that person and its own grants are dropped; it never widens or narrows anyone.
#[test]
fn a_restored_old_entry_joins_its_existing_person() {
    let (_dir, layout) = seeded("v0.25.0");
    identity::migrate_people(&layout).unwrap();
    let mut doc = read(&layout.clients_json());
    doc["passkeys"]["bGFwdG9w"] =
        json!({ "grants": ["agent"], "label": "laptop", "public_key": "AAAA", "sign_count": 9 });
    write(&layout.clients_json(), &doc);
    let got = identity::read_passkeys(&layout).unwrap();
    assert_eq!(
        got["bGFwdG9w"].grants,
        ["brian", "reviewer"],
        "the person's, not the entry's"
    );
    identity::migrate_people(&layout).unwrap();
    let doc = read(&layout.clients_json());
    assert!(doc["passkeys"]["bGFwdG9w"]["person"].is_string(), "{doc}");
    assert_eq!(doc["passkeys"]["bGFwdG9w"]["sign_count"], 9);
    let got = identity::read_passkeys(&layout).unwrap();
    assert_eq!(got["bGFwdG9w"].grants, ["brian", "reviewer"]);
    assert_eq!(
        identity::migrate_people(&layout).unwrap(),
        0,
        "nothing left to do"
    );
}

/// A crash between the backup and the rename: the next start finishes the migration, and the
/// backup is still the file before (no second, stamped one).
#[test]
fn a_crash_between_backup_and_rename_finishes_at_the_next_start() {
    let (_dir, layout) = seeded("v0.24.2");
    let before = std::fs::read_to_string(layout.clients_json()).unwrap();
    std::fs::write(identity::people_backup(&layout), &before).unwrap();
    assert_eq!(identity::migrate_people(&layout).unwrap(), 2);
    assert_eq!(
        std::fs::read_to_string(identity::people_backup(&layout)).unwrap(),
        before
    );
    assert!(stamped(&layout).is_empty(), "{:?}", stamped(&layout));
}

fn stamped(layout: &Layout) -> Vec<std::path::PathBuf> {
    let dir = layout.clients_json().parent().unwrap().to_path_buf();
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("clients.json.before-people-"))
        })
        .collect();
    out.sort();
    out
}

/// ★ Claim 5: a rollback (the backup restored, then changed by the old server) and a
/// re-upgrade keeps the first backup, writes a stamped one holding the file this migration
/// started from, and names THAT one.
#[test]
fn a_re_upgrade_names_a_fresh_backup() {
    let (_dir, layout) = seeded("v0.25.0");
    let first = std::fs::read_to_string(layout.clients_json()).unwrap();
    identity::migrate_people(&layout).unwrap();
    // Rolled back: the backup restored, and the old server enrolls a third passkey.
    let mut doc: Value = serde_json::from_str(&first).unwrap();
    doc["passkeys"]["dGFibGV0"] =
        json!({ "grants": ["reviewer"], "label": "tablet", "public_key": "CCCC" });
    write(&layout.clients_json(), &doc);
    let second = std::fs::read_to_string(layout.clients_json()).unwrap();
    let done = identity::migrate(&layout).unwrap();
    assert_eq!(done.migrated, 3);
    assert_eq!(
        std::fs::read_to_string(identity::people_backup(&layout)).unwrap(),
        first,
        "the first backup is kept"
    );
    let fresh = stamped(&layout);
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    assert_eq!(std::fs::read_to_string(&fresh[0]).unwrap(), second);
    assert_eq!(
        done.backup.as_deref(),
        Some(fresh[0].as_path()),
        "named in the line"
    );
    assert!(identity::read_passkeys(&layout)
        .unwrap()
        .contains_key("dGFibGV0"));
}

/// Every `clients.json.before-people*` file beside `clients.json`.
fn backups(layout: &Layout) -> Vec<std::path::PathBuf> {
    let dir = layout.clients_json().parent().unwrap().to_path_buf();
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("clients.json.before-people"))
        })
        .collect();
    out.sort();
    out
}

/// One passkey command run on a layout, named for the assertion messages.
type Refusal = (&'static str, fn(&Layout) -> Result<(), String>);
type Success = (&'static str, fn(&Layout));

/// ★ Round 2 (ledger #4432): a REFUSED passkey command writes nothing, even on a file the
/// people migration is due on: not the change, not the migration, not a backup.
#[test]
fn a_refused_command_on_an_old_form_file_writes_nothing() {
    let refusals: [Refusal; 9] = [
        ("grant add, already held", |l| {
            identity::add_grant(l, Which::Label("laptop"), "brian", NOW * 1000).map(drop)
        }),
        ("grant add, undefined grant", |l| {
            identity::add_grant(l, Which::Label("laptop"), "nosuch", NOW * 1000).map(drop)
        }),
        ("grant add, no such passkey", |l| {
            identity::add_grant(l, Which::Label("nobody"), "agent", NOW * 1000).map(drop)
        }),
        ("remove-grant, not held", |l| {
            identity::remove_grant(l, Which::Label("laptop"), "agent", NOW * 1000).map(drop)
        }),
        ("remove-grant, the last one", |l| {
            identity::remove_grant(l, Which::Credential("cGhvbmU"), "reviewer", NOW * 1000)
                .map(drop)
        }),
        ("attach, no such person", |l| {
            identity::attach(l, Which::Label("laptop"), "nobody", NOW * 1000).map(drop)
        }),
        ("attach, already that person", |l| {
            identity::attach(l, Which::Label("laptop"), "laptop", NOW * 1000).map(drop)
        }),
        ("revoke, no such label", |l| {
            identity::revoke(l, Which::Label("nobody"), NOW * 1000).map(drop)
        }),
        ("revoke, no such credential", |l| {
            identity::revoke(l, Which::Credential("bm9ib2R5"), NOW * 1000).map(drop)
        }),
    ];
    for tag in TAGS {
        for (what, run) in refusals {
            let (_dir, layout) = seeded(tag);
            let before = std::fs::read(layout.clients_json()).unwrap();
            assert!(run(&layout).is_err(), "{tag} {what}: refused");
            assert_eq!(
                std::fs::read(layout.clients_json()).unwrap(),
                before,
                "{tag} {what}: clients.json byte-identical"
            );
            assert_eq!(
                backups(&layout),
                Vec::<std::path::PathBuf>::new(),
                "{tag} {what}"
            );
            assert!(
                !identity::audit_log(&layout).exists(),
                "{tag} {what}: no audit line"
            );
        }
    }
}

/// ★ Round 2: an ambiguous label, as `passkey_313` refuses it, on an old-form file.
#[test]
fn a_refused_ambiguous_revoke_on_an_old_form_file_writes_nothing() {
    let (_dir, layout) = seeded("v0.24.2");
    let mut doc = read(&layout.clients_json());
    doc["passkeys"]["cGhvbmU"]["label"] = json!("laptop");
    write(&layout.clients_json(), &doc);
    let before = std::fs::read(layout.clients_json()).unwrap();
    let why = identity::revoke(&layout, Which::Label("laptop"), NOW * 1000).unwrap_err();
    assert!(why.contains("--credential"), "{why}");
    assert_eq!(std::fs::read(layout.clients_json()).unwrap(), before);
    assert!(backups(&layout).is_empty());
}

/// ★ Round 2: the same commands SUCCEEDING on an old-form file write the migrated file, the
/// change with it, and the pre-people backup.
#[test]
fn a_command_that_succeeds_on_an_old_form_file_writes_the_migration_and_the_backup() {
    let successes: [Success; 4] = [
        ("grant add", |l| {
            identity::add_grant(l, Which::Label("laptop"), "agent", NOW * 1000).unwrap();
            assert!(identity::read_passkeys(l).unwrap()["bGFwdG9w"]
                .grants
                .contains(&"agent".to_string()));
        }),
        ("remove-grant", |l| {
            identity::add_grant(l, Which::Credential("cGhvbmU"), "agent", NOW * 1000).unwrap();
            identity::remove_grant(l, Which::Credential("cGhvbmU"), "agent", NOW * 1000).unwrap();
            assert_eq!(
                identity::read_passkeys(l).unwrap()["cGhvbmU"].grants,
                ["reviewer"]
            );
        }),
        ("attach", |l| {
            identity::attach(l, Which::Credential("cGhvbmU"), "laptop", NOW * 1000).unwrap();
            let got = identity::read_passkeys(l).unwrap();
            assert_eq!(got["cGhvbmU"].person, got["bGFwdG9w"].person);
        }),
        ("revoke", |l| {
            identity::revoke(l, Which::Credential("cGhvbmU"), NOW * 1000).unwrap();
            assert!(!identity::read_passkeys(l).unwrap().contains_key("cGhvbmU"));
        }),
    ];
    for tag in TAGS {
        for (what, run) in successes {
            let (_dir, layout) = seeded(tag);
            let before = std::fs::read_to_string(layout.clients_json()).unwrap();
            run(&layout);
            assert_eq!(
                backups(&layout),
                [identity::people_backup(&layout)],
                "{tag} {what}: one backup"
            );
            assert_eq!(
                std::fs::read_to_string(identity::people_backup(&layout)).unwrap(),
                before,
                "{tag} {what}: the backup is the file before people"
            );
            let doc = read(&layout.clients_json());
            assert!(doc["people"].is_object(), "{tag} {what}: {doc}");
            for (id, entry) in doc["passkeys"].as_object().unwrap() {
                assert!(
                    entry["person"].is_string() && entry["rp_id"].is_string(),
                    "{tag} {what}: {id} migrated: {doc}"
                );
            }
            assert!(
                old_reader_list(&doc).is_ok(),
                "{tag} {what}: an old gonk starts"
            );
            // Nothing left for a start to migrate.
            assert_eq!(
                identity::migrate_people(&layout).unwrap(),
                0,
                "{tag} {what}"
            );
        }
    }
}
