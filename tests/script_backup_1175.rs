//! Ledger [#1175](http://localhost:1060/l/default/item/1175), its first half: the scripts are in
//! the backup, and in its verify.
//!
//! Script atoms live in `ikigai-script`'s `DirBackend` (`gonk.script.dir`), not in the dataset,
//! so before this a backup — the dataset's N-Quads — held none of them, and a restore could not
//! bring one back. Now each backup writes `gonk-scripts-<stamp>.json.gz` beside the store's
//! archive, the sidecar records both, they are pruned together, and
//! `urn:iki:gonk:restore:scripts` writes one into a new directory and reads every file back
//! through the library.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::compose_with;
use ikigai_gonk::grants::{grants_for, script_grants, Authority, ScriptAuthority};
use ikigai_gonk::script::Scripts;
use ikigai_script::Backend as _;

const PERSON: &str = "urn:iki:gonk:passkey:QlJJQU4";

const OPEN_ITEMS: &str = "\
# Open items, lowest number first.
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
SELECT ?number
FROM <urn:iki:ledger:graph:default>
WHERE { ?item ledger:status ledger:open ; ledger:number ?number }
ORDER BY ?number
";

const ALL_ITEMS: &str = "\
# Every item.
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
SELECT ?number
FROM <urn:iki:ledger:graph:default>
WHERE { ?item ledger:number ?number }
";

fn request(verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    )
}

fn issue(hub: &Kernel, who: &Capability, verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Vec<u8> {
    block_on(hub.issue(request(verb, iri, args), who))
        .unwrap_or_else(|e| panic!("{verb:?} {iri}: {e}"))
        .bytes
}

fn person() -> Capability {
    let mut scopes = grants_for("default", Authority::Write).unwrap();
    for name in ["open-items", "all-items"] {
        scopes.extend(script_grants(name, ScriptAuthority::Publish).unwrap());
    }
    Capability::scoped(scopes).with_principal(PERSON).unwrap()
}

/// A hub as `main` composes it: the backup family rotating into `backups`, the scripts in a
/// `DirBackend` at `scripts`, which the backup snapshots.
fn server(backups: &Path, scripts: &Path, keep: usize) -> (Arc<Kernel>, Scripts) {
    let family = Scripts::open(scripts, Default::default()).expect("the scripts directory");
    let (store, handle) = ikigai_store::DurableStore::in_memory_shared_declaring(
        ikigai_store::SharerWrites::only_the_default_graph(),
    )
    .expect("an in-memory store");
    let dataset = ikigai_gonk::backup::Dataset::new(&store, handle);
    let hub = Arc::new(compose_with(
        store,
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: backups.to_path_buf(),
                keep,
                every: None,
                store_path: PathBuf::from("/nonexistent/live/store"),
                scripts: Some(scripts.to_path_buf()),
            }),
            jobs: None,
            next: Default::default(),
            dataset,
        }),
        Some(&family),
        None,
    ));
    family.attach(&hub);
    (hub, family)
}

/// Two items, two published scripts (one republished, so its head has history), and one run
/// recorded.
fn seed(hub: &Kernel) {
    let root = Capability::root();
    for content in ["first", "second"] {
        issue(
            hub,
            &root,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content.as_bytes())],
        );
    }
    for (name, text) in [
        ("open-items", ALL_ITEMS),
        ("open-items", OPEN_ITEMS),
        ("all-items", ALL_ITEMS),
    ] {
        issue(
            hub,
            &person(),
            Verb::Sink,
            &format!("urn:script:{name}"),
            &[("language", b"sparql"), ("content", text.as_bytes())],
        );
    }
    issue(
        hub,
        &person(),
        Verb::Sink,
        "urn:script:open-items:runs",
        &[],
    );
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_backup_holds_the_scripts_and_their_restore_verifies() {
    let work = tempfile::tempdir().unwrap();
    let (backups, scripts) = (work.path().join("backups"), work.path().join("scripts"));
    let (hub, _family) = server(&backups, &scripts, 5);
    seed(&hub);
    let root = Capability::root();

    let report = String::from_utf8(issue(&hub, &root, Verb::Source, backup::BACKUP, &[])).unwrap();
    assert!(report.contains("scripts gonk-scripts-"), "{report}");
    assert!(report.contains("2 script(s)"), "{report}");

    // Two archives under one stamp, and one sidecar describing both.
    let on_disk = files_in(&backups);
    let store = on_disk
        .iter()
        .find(|n| n.starts_with("gonk-store-") && n.ends_with(".nq.gz"))
        .expect("a store archive")
        .clone();
    let scripts_archive = backup::scripts_archive_for(&store).unwrap();
    assert!(on_disk.contains(&scripts_archive), "{on_disk:?}");
    let meta = std::fs::read_to_string(backups.join(format!("{store}.meta.json"))).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert_eq!(
        meta["scripts_archive"],
        scripts_archive.as_str(),
        "{meta:#}"
    );
    assert_eq!(meta["scripts_names"], 2, "{meta:#}");
    // open-items: head + 2 versions + 1 run; all-items: head + 1 version.
    assert_eq!(meta["scripts_files"], 6, "{meta:#}");

    // The status names it.
    let status = String::from_utf8(issue(&hub, &root, Verb::Source, backup::STATUS, &[])).unwrap();
    assert!(
        status.contains(&format!("scripts        {scripts_archive}")),
        "{status}"
    );

    // Read it back out by name, and restore it into a NEW directory: the verifier.
    let archive = issue(
        &hub,
        &root,
        Verb::Source,
        &format!("urn:iki:gonk:backup:archive:{scripts_archive}"),
        &[],
    );
    // Ledger #2730: its download is a body over the file whose content id the store sidecar
    // of the same stamp recorded (`scripts_archive_content_id`).
    let id = ikigai_core::ContentId::of(&archive).to_string();
    assert_eq!(meta["scripts_archive_content_id"], id.as_str(), "{meta:#}");
    let body = block_on(hub.issue_body(
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:iki:gonk:backup:archive:{scripts_archive}")).unwrap(),
        ),
        &root,
    ))
    .unwrap();
    let ikigai_core::Body::Replay(replay) = &body.bytes else {
        panic!("the scripts archive was buffered whole");
    };
    assert_eq!(replay.validator().map(|v| v.to_string()), Some(id));
    let restored = work.path().join("restored-scripts");
    let into = restored.to_string_lossy().into_owned();
    let report = String::from_utf8(issue(
        &hub,
        &root,
        Verb::Sink,
        backup::RESTORE_SCRIPTS,
        &[("content", &archive), ("into", into.as_bytes())],
    ))
    .unwrap();
    assert!(report.contains("restored 2 script(s)"), "{report}");
    assert!(report.contains("3 version(s), 1 run(s)"), "{report}");

    // The restored directory is a scripts directory the library reads as the live one.
    let live = ikigai_script::DirBackend::open(&scripts).unwrap();
    let copy = ikigai_script::DirBackend::open(&restored).unwrap();
    assert_eq!(copy.names().unwrap(), live.names().unwrap());
    for name in live.names().unwrap() {
        assert_eq!(
            copy.head(&name).unwrap(),
            live.head(&name).unwrap(),
            "{name}"
        );
    }
    assert!(copy.run("open-items", 1).unwrap().is_some());

    // And a server over it runs the script as the live one does.
    let (other, _family) = server(&work.path().join("other-backups"), &restored, 5);
    for content in ["first", "second"] {
        issue(
            &other,
            &root,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", content.as_bytes())],
        );
    }
    let run = |kernel: &Kernel| {
        issue(
            kernel,
            &person(),
            Verb::Source,
            "urn:script:open-items:result",
            &[("as", b"text/csv")],
        )
    };
    assert_eq!(run(&other), run(&hub));
}

#[test]
fn an_unchanged_scripts_directory_backs_up_to_the_same_bytes() {
    let work = tempfile::tempdir().unwrap();
    let scripts = work.path().join("scripts");
    let (hub, _family) = server(&work.path().join("a"), &scripts, 5);
    seed(&hub);
    issue(&hub, &Capability::root(), Verb::Source, backup::BACKUP, &[]);
    let (again, _family) = hub_over_same(&work.path().join("b"), &scripts);
    issue(
        &again,
        &Capability::root(),
        Verb::Source,
        backup::BACKUP,
        &[],
    );
    let digest = |dir: &Path| {
        let store = files_in(dir)
            .into_iter()
            .find(|n| n.ends_with(".nq.gz"))
            .unwrap();
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("{store}.meta.json"))).unwrap(),
        )
        .unwrap();
        meta["scripts_json_sha256"].as_str().unwrap().to_string()
    };
    assert_eq!(
        digest(&work.path().join("a")),
        digest(&work.path().join("b"))
    );
}

/// A second server over the SAME scripts directory, backing up elsewhere (the first one's
/// `DirBackend` is idle by then; one process per directory is the library's rule for writers).
fn hub_over_same(backups: &Path, scripts: &Path) -> (Arc<Kernel>, Scripts) {
    server(backups, scripts, 5)
}

#[test]
fn a_scripts_restore_refuses_a_tampered_or_hostile_archive_and_the_live_directory() {
    let work = tempfile::tempdir().unwrap();
    let (backups, scripts) = (work.path().join("backups"), work.path().join("scripts"));
    let (hub, _family) = server(&backups, &scripts, 5);
    seed(&hub);
    let root = Capability::root();
    issue(&hub, &root, Verb::Source, backup::BACKUP, &[]);
    let store = files_in(&backups)
        .into_iter()
        .find(|n| n.ends_with(".nq.gz"))
        .unwrap();
    let gz = std::fs::read(backups.join(backup::scripts_archive_for(&store).unwrap())).unwrap();
    let json = issue(
        &hub,
        &root,
        Verb::Source,
        "urn:decompress:gzip",
        &[("content", &gz), ("as", b"application/json")],
    );
    let document: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let restore = |content: &[u8], into: &Path| {
        let into = into.to_string_lossy().into_owned();
        block_on(hub.issue(
            request(
                Verb::Sink,
                backup::RESTORE_SCRIPTS,
                &[("content", content), ("into", into.as_bytes())],
            ),
            &root,
        ))
    };

    // A version whose text was edited after it was taken does not verify: the library checks
    // every version against the digest it is named by.
    let mut tampered = document.clone();
    let files = tampered["files"].as_object_mut().unwrap();
    let version = files
        .keys()
        .find(|path| path.starts_with("all-items/versions/"))
        .unwrap()
        .clone();
    let text = files[&version]
        .as_str()
        .unwrap()
        .replace("Every item", "Every item, edited");
    files.insert(version.clone(), text.into());
    let into = work.path().join("tampered");
    match restore(&serde_json::to_vec(&tampered).unwrap(), &into) {
        Err(Error::Endpoint(why)) => assert!(why.contains("do not verify"), "{why}"),
        other => panic!("a tampered version must not verify: {other:?}"),
    }

    // A head whose version is missing from the archive does not verify either.
    let mut short = document.clone();
    short["files"].as_object_mut().unwrap().remove(&version);
    match restore(
        &serde_json::to_vec(&short).unwrap(),
        &work.path().join("short"),
    ) {
        Err(Error::Endpoint(why)) => assert!(why.contains("all-items"), "{why}"),
        other => panic!("a head naming a missing version must not verify: {other:?}"),
    }

    // A path outside the layout is refused before anything is written.
    for hostile in [
        "../escaped/head.json",
        "x/../../escaped/head.json",
        "/tmp/head.json",
    ] {
        let mut bad = document.clone();
        bad["files"]
            .as_object_mut()
            .unwrap()
            .insert(hostile.to_string(), "{}".into());
        let into = work.path().join("hostile");
        match restore(&serde_json::to_vec(&bad).unwrap(), &into) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "content"),
            other => panic!("`{hostile}` must be refused: {other:?}"),
        }
        assert!(!into.exists(), "nothing was written for `{hostile}`");
        assert!(!work.path().join("escaped").exists());
    }

    // A store archive is not a scripts archive.
    let store_gz = std::fs::read(backups.join(&store)).unwrap();
    assert!(restore(&store_gz, &work.path().join("wrong-kind")).is_err());

    // The live directory, and a directory that holds something, are refused by name.
    match restore(&json, &scripts) {
        Err(Error::InvalidArgument { detail, .. }) => assert!(detail.contains("LIVE"), "{detail}"),
        other => panic!("the live directory must be refused: {other:?}"),
    }
    std::fs::create_dir_all(work.path().join("full")).unwrap();
    std::fs::write(work.path().join("full/x"), "x").unwrap();
    assert!(restore(&json, &work.path().join("full")).is_err());
}

/// The rotation keeps a store archive and its scripts archive together: pruning one prunes
/// the other.
#[test]
fn a_pruned_backup_takes_its_scripts_archive_with_it() {
    let work = tempfile::tempdir().unwrap();
    let (backups, scripts) = (work.path().join("backups"), work.path().join("scripts"));
    std::fs::create_dir_all(&backups).unwrap();
    // An older backup, as an earlier run left it.
    let old = "gonk-store-2020-01-01T000000Z.nq.gz";
    for name in [
        old.to_string(),
        format!("{old}.meta.json"),
        backup::scripts_archive_for(old).unwrap(),
    ] {
        std::fs::write(backups.join(name), b"old").unwrap();
    }
    let (hub, _family) = server(&backups, &scripts, 1);
    seed(&hub);
    let report = String::from_utf8(issue(
        &hub,
        &Capability::root(),
        Verb::Source,
        backup::BACKUP,
        &[],
    ))
    .unwrap();
    assert!(report.contains(&format!("pruned {old}")), "{report}");
    let left = files_in(&backups);
    assert!(
        !left.iter().any(|n| n.contains("2020-01-01")),
        "the old backup's three files went together: {left:?}"
    );
    assert_eq!(
        left.len(),
        3,
        "one store archive, its sidecar, its scripts: {left:?}"
    );
}
