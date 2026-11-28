//! Ledger #4506 on ikigai-secret 0.5.4: gonk started on a data and config home as an older
//! release left it, with no `[secret]` section.
//!
//! - An install from before (a file store with gonk's controller key and an open invite's code
//!   in it) keeps them readable and unchanged, and gets ikigai-secret's notice.
//! - An empty home is a new install. It keeps `default_backend()`'s store until gonk resolves
//!   age identities (ledger #4495), and gets no notice.
//! - An age-file marker stays age-file. Gonk has no keys for it, so start is refused with
//!   ikigai-secret's sentence, and nothing is written.
//!
//! No secret moves between stores. These set `HOME` and `IKIGAI_SECRETS` for the process (the
//! file store and `found_install` read them), so they are ONE test, run in order, alone in this
//! binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use ikigai_gonk::onboard::{DEFAULT_KEY, INVITE_SECRET_PREFIX};
use ikigai_gonk::secret::{Keystore, NO_AGE_IDENTITY};
use ikigai_secret::{Backend, FileBackend, Found, SecretConfig};

const NO_SECTION: &str = "gonk.bind = \"127.0.0.1:0\"\ngonk.browse.root = core=~/git/core\n";

/// `{home}/.ikigai/secrets`, the data home's file store, made private as ikigai-secret makes it.
fn secrets_dir(home: &Path) -> PathBuf {
    let dir = home.join(".ikigai").join("secrets");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in [home.join(".ikigai"), dir.clone()] {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    dir
}

/// Every file under `dir` with its bytes, to show nothing in a store changed.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    all.sort();
    all
}

fn set_env(home: &Path) {
    // One test in this binary, so nothing else reads the environment while it changes.
    std::env::set_var("HOME", home);
    std::env::remove_var("IKIGAI_SECRETS");
}

#[test]
fn an_older_release_home_keeps_its_store_and_no_secret_moves() {
    // 1. An install from before: a file store in the data home with gonk's secrets in it.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("existing");
    let dir = secrets_dir(&home);
    let invite = format!("{INVITE_SECRET_PREFIX}abc");
    let old = FileBackend::new(&dir);
    old.set(DEFAULT_KEY, b"-----BEGIN PRIVATE KEY-----\nold\n")
        .unwrap();
    old.set(&invite, b"123456").unwrap();
    let before = snapshot(&dir);
    set_env(&home);

    assert_eq!(
        ikigai_secret::found_install(Some(&home)).unwrap(),
        Found::Existing
    );
    let keystore = Keystore::from_config_text(NO_SECTION, Some(&home)).unwrap();
    assert_eq!(
        keystore.routing().config(),
        &SecretConfig::from_section(None, Some(&home)).unwrap(),
        "the store default_backend() built before, unchanged"
    );
    let notice = keystore
        .notice()
        .expect("an install from before gets the notice");
    assert!(
        notice.starts_with("ikigai-secret: secrets stay in the"),
        "{notice}"
    );
    assert!(!notice.contains('\n'), "one line: {notice}");
    assert!(keystore.describe().contains("as before"));
    if cfg!(not(target_os = "macos")) {
        // Off a Mac, today's store IS this file store: the key and the code read back as they
        // were. (On a Mac it is the Keychain, which a test does not touch.)
        let backend = keystore.backend();
        assert_eq!(
            backend.get(DEFAULT_KEY).unwrap().as_deref(),
            Some(&b"-----BEGIN PRIVATE KEY-----\nold\n"[..])
        );
        assert_eq!(
            backend.get(&invite).unwrap().as_deref(),
            Some(&b"123456"[..])
        );
    }
    assert_eq!(snapshot(&dir), before, "nothing in the old store changed");

    // 2. An empty home: a new install.
    let home = root.path().join("empty");
    std::fs::create_dir_all(&home).unwrap();
    set_env(&home);
    let found = ikigai_secret::found_install(Some(&home)).unwrap();
    if cfg!(not(target_os = "macos")) {
        // (On a Mac, any Keychain item under `ikigai-secret` on the machine makes it Existing.)
        assert_eq!(found, Found::Nothing);
    }
    if found == Found::Nothing {
        let keystore = Keystore::from_config_text(NO_SECTION, Some(&home)).unwrap();
        assert_eq!(
            keystore.routing().config(),
            &SecretConfig::from_section(None, Some(&home)).unwrap()
        );
        assert_eq!(keystore.notice(), None);
        assert!(keystore.describe().contains("ledger #4495"));
    }
    assert!(
        !home.join(".ikigai").exists(),
        "building the keystore made no store"
    );

    // 3. An age-file marker (left by another ikigai host) stays age-file: refused, untouched.
    let home = root.path().join("age");
    let dir = secrets_dir(&home);
    std::fs::write(dir.join(".ikigai-age-file"), "an age-file store\n").unwrap();
    std::fs::write(dir.join("other.host.key"), b"age-encryption.org/v1\nsealed").unwrap();
    let before = snapshot(&dir);
    set_env(&home);
    assert_eq!(
        ikigai_secret::found_install(Some(&home)).unwrap(),
        Found::AgeFile
    );
    let wanted = SecretConfig::for_host(None, Some(&home), Found::AgeFile).unwrap();
    assert_eq!(wanted.default_store(), "age-file");
    let refusal = wanted
        .build_with_keys(Default::default())
        .err()
        .unwrap()
        .to_string();
    let e = Keystore::from_config_text(NO_SECTION, Some(&home))
        .err()
        .expect("no keys for the age-file store: refused");
    assert_eq!(e, format!("{refusal}\n{NO_AGE_IDENTITY}"));

    // And `serve` on that home refuses to start with that sentence, before anything is opened.
    let config_home = root.path().join("age-cfg");
    let data_home = root.path().join("age-data");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::create_dir_all(&data_home).unwrap();
    std::fs::write(
        config_home.join("config.toml"),
        "gonk.bind = \"127.0.0.1:0\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["serve", "--port", "0", "--no-backup"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", &home)
        .env_remove("IKIGAI_SECRETS")
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&refusal), "{stderr}\n---\n{refusal}");
    assert!(stderr.contains(NO_AGE_IDENTITY), "{stderr}");
    assert!(std::fs::read_dir(&data_home).unwrap().next().is_none());
    assert_eq!(snapshot(&dir), before, "the age-file store is untouched");
}
