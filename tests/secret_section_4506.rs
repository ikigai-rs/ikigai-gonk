//! Ledger #4506: gonk reads the `[secret]` section of its config home's `config.toml` and keeps
//! every secret (the controller key, the open invites' codes) in the stores it names.
//!
//! - No section: the keystore is exactly ikigai-secret's no-section configuration (what
//!   `default_backend()` builds), and its space reserves no new name.
//! - A section routing `ikigai-gonk.*` to a second file store: the controller key minted through
//!   gonk's keystore, and an invite's code, are kept there and read from there.
//! - A malformed section: `serve` refuses to start with ikigai-secret's sentence, before anything
//!   is opened.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use ikigai_gonk::onboard::{generate_key_in, DEFAULT_KEY, INVITE_SECRET_PREFIX};
use ikigai_gonk::secret::Keystore;
use ikigai_secret::{Backend, FileBackend, Found, PresenceGate, SecretConfig};

/// A gate that finds someone at the machine, so the mint runs headless (Touch ID on a Mac).
fn present() -> Option<Arc<dyn PresenceGate>> {
    Some(Arc::new(|_reason: &str| Ok(())))
}

fn routed_text(gonk: &Path, other: &Path) -> String {
    format!(
        "gonk.bind = \"127.0.0.1:1062\"\n\
         gonk.browse.root = core=~/git/core\n\
         gonk.browse.root = edge=~/git/edge\n\
         \n\
         [secret]\n\
         default = \"other\"\n\
         \n\
         [secret.store.other]\n\
         kind = \"file\"\n\
         path = \"{}\"\n\
         \n\
         [secret.store.gonk]\n\
         kind = \"file\"\n\
         path = \"{}\"\n\
         \n\
         [[secret.route]]\n\
         match = \"ikigai-gonk.*\"\n\
         store = \"gonk\"\n",
        other.display(),
        gonk.display()
    )
}

#[test]
fn no_section_is_the_store_gonk_always_used() {
    let home = tempfile::tempdir().unwrap();
    let text = "gonk.bind = \"127.0.0.1:1062\"\ngonk.browse.root = core=~/git/core\n";
    assert_eq!(ikigai_gonk::secret::section(text).unwrap(), None);
    // An install from before (what ikigai-secret's `found_install` answers for one).
    let keystore = Keystore::for_host(None, Some(home.path()), Found::Existing).unwrap();
    assert!(!keystore.is_configured());
    // The same configuration as no section at all, which ikigai-secret builds exactly as
    // `default_backend()` does: one store, the platform's, as the default, with no routes.
    let none = SecretConfig::from_section(None, Some(home.path())).unwrap();
    assert_eq!(keystore.routing().config(), &none);
    assert!(keystore.routing().config().routes().is_empty());
    let store = &keystore.routing().config().stores()[0];
    if cfg!(target_os = "macos") {
        assert!(matches!(
            store.kind(),
            ikigai_secret::StoreKind::Keychain { service } if service == ikigai_secret::DEFAULT_SERVICE
        ));
    } else {
        assert!(matches!(
            store.kind(),
            ikigai_secret::StoreKind::File { dir: None }
        ));
    }
    let said = keystore.describe();
    assert!(said.contains("as before"), "{said}");
    assert!(!said.contains("reserved"), "{said}");
    // And ikigai-secret's notice, word for word, as the one extra start-up line.
    assert_eq!(keystore.notice(), none.notice().as_deref());
    assert!(keystore.notice().unwrap().contains("age-file"));
}

#[test]
fn a_new_install_keeps_todays_store_until_gonk_resolves_age_identities() {
    let home = tempfile::tempdir().unwrap();
    let keystore = Keystore::for_host(None, Some(home.path()), Found::Nothing).unwrap();
    // ikigai-secret 0.5.4 would make an age-file store whose identity the host resolves; gonk
    // resolves none yet (ledger #4495), so the new install keeps `default_backend()`'s store.
    let today = SecretConfig::from_section(None, Some(home.path())).unwrap();
    assert_eq!(keystore.routing().config(), &today);
    let wanted = SecretConfig::for_host(None, Some(home.path()), Found::Nothing).unwrap();
    assert_eq!(wanted.default_store(), "age-file");
    assert!(wanted.build_with_keys(Default::default()).is_err());
    // No notice: it would tell a new install that it got age-file.
    assert_eq!(keystore.notice(), None);
    let said = keystore.describe();
    assert!(said.contains("ledger #4495"), "{said}");
}

#[test]
fn a_route_for_gonk_keeps_its_key_and_codes_in_the_second_store() {
    let gonk = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let keystore =
        Keystore::from_config_text(&routed_text(gonk.path(), other.path()), Some(home.path()))
            .unwrap();
    assert!(keystore.is_configured());

    // The controller key, minted the way `onboard init` mints it.
    let public = generate_key_in(keystore.space(present()), DEFAULT_KEY).unwrap();
    assert!(public.contains("PUBLIC KEY"), "{public}");
    // An invite's code, kept the way `Onboard::invite` keeps it.
    let code = format!("{INVITE_SECRET_PREFIX}abc");
    keystore.backend().set(&code, b"123456").unwrap();

    let there = FileBackend::new(gonk.path());
    let elsewhere = FileBackend::new(other.path());
    assert!(there.get(DEFAULT_KEY).unwrap().is_some());
    assert_eq!(there.get(&code).unwrap().as_deref(), Some(&b"123456"[..]));
    assert_eq!(elsewhere.get(DEFAULT_KEY).unwrap(), None);
    assert_eq!(elsewhere.get(&code).unwrap(), None);
    // And read back from there through gonk's keystore.
    assert_eq!(
        keystore.backend().get(DEFAULT_KEY).unwrap(),
        there.get(DEFAULT_KEY).unwrap()
    );
    // A name no route matches goes to the default store.
    keystore.backend().set("someone.else", b"x").unwrap();
    assert!(elsewhere.get("someone.else").unwrap().is_some());
    assert_eq!(there.get("someone.else").unwrap(), None);

    let said = keystore.describe();
    assert!(said.contains("controller key is kept in `gonk`"), "{said}");
    assert!(said.contains("invite codes in `gonk`"), "{said}");
    assert!(
        said.contains("urn:secret:store and urn:secret:route"),
        "{said}"
    );
}

#[test]
fn a_malformed_section_refuses_to_start_with_ikigai_secrets_sentence() {
    let root = tempfile::tempdir().unwrap();
    let config_home = root.path().join("cfg");
    let data_home = root.path().join("data");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::create_dir_all(&data_home).unwrap();
    std::fs::write(
        config_home.join("config.toml"),
        "gonk.bind = \"127.0.0.1:0\"\n\n[secret]\ndefault = \"nowhere\"\n\n\
         [[secret.route]]\nmatch = \"ikigai-gonk.*\"\nstore = \"missing\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["serve", "--port", "0", "--no-backup"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", root.path().join("home"))
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    // ikigai-secret's own sentence, every problem listed.
    let section = ikigai_gonk::secret::section(
        &std::fs::read_to_string(config_home.join("config.toml")).unwrap(),
    )
    .unwrap();
    let refusal = SecretConfig::from_section(section.as_ref(), Some(root.path()))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.starts_with("The [secret] section was refused and nothing in it was applied"),
        "{refusal}"
    );
    assert!(stderr.contains(&refusal), "{stderr}\n---\n{refusal}");
    assert!(stderr.contains("nowhere"), "{stderr}");
    assert!(stderr.contains("missing"), "{stderr}");
    assert!(
        std::fs::read_dir(&data_home).unwrap().next().is_none(),
        "refused before anything is opened: nothing is written under the data home"
    );
}

#[test]
fn an_unknown_key_refuses_to_start_naming_its_line() {
    let root = tempfile::tempdir().unwrap();
    let config_home = root.path().join("cfg");
    let data_home = root.path().join("data");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::create_dir_all(&data_home).unwrap();
    std::fs::write(
        config_home.join("config.toml"),
        "gonk.bind = \"127.0.0.1:0\"\n[secret]\ndefault = \"a\"\nfallback = \"b\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["serve", "--port", "0", "--no-backup"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", root.path().join("home"))
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("The [secret] section was refused and nothing in it was applied"),
        "{stderr}"
    );
    assert!(stderr.contains("line 4"), "{stderr}");
    assert!(stderr.contains("fallback"), "{stderr}");
}

#[test]
fn a_malformed_section_refuses_onboard_init_with_ikigai_secrets_sentence() {
    let root = tempfile::tempdir().unwrap();
    let config_home = root.path().join("cfg");
    let data_home = root.path().join("data");
    let home = root.path().join("home");
    for dir in [&config_home, &data_home, &home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let text = "gonk.bind = \"127.0.0.1:0\"\n\n[secret]\ndefault = \"nowhere\"\n";
    std::fs::write(config_home.join("config.toml"), text).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["onboard", "init"])
        .arg("--config-home")
        .arg(&config_home)
        .arg("--data-home")
        .arg(&data_home)
        .env("HOME", &home)
        .env_remove("IKIGAI_SECRETS")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run ikigai-gonk");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let section = ikigai_gonk::secret::section(text).unwrap();
    let refusal = SecretConfig::from_section(section.as_ref(), Some(&home))
        .unwrap_err()
        .to_string();
    assert!(stderr.contains(&refusal), "{stderr}\n---\n{refusal}");
    // Nothing minted anywhere.
    assert!(std::fs::read_dir(&home).unwrap().next().is_none());
    assert!(std::fs::read_dir(&data_home).unwrap().next().is_none());
}
