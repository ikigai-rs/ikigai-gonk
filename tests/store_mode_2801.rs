//! The live dataset is owner-only (ledger #2801): the store's directory is `0700` once gonk
//! has opened it, whatever mode it had before, as the backup's rotation directory already is.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use ikigai_store::DurableStore;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_store_in_a_world_readable_directory_is_owner_only_after_it_opens() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("store");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    ikigai_gonk::private_store_dir(&dir).unwrap();
    let store = DurableStore::open(&dir).expect("the store opens");
    drop(store);

    assert_eq!(mode(&dir), 0o700, "the store directory is owner-only");
}

#[test]
fn a_store_directory_gonk_creates_is_owner_only_from_the_start() {
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join(".ikigai");
    let dir = parent.join("store");

    ikigai_gonk::private_store_dir(&dir).unwrap();
    assert_eq!(mode(&parent), 0o700, "a parent it creates is owner-only");
    assert_eq!(mode(&dir), 0o700, "the store directory is owner-only");
    drop(DurableStore::open(&dir).expect("the store opens"));
    assert_eq!(mode(&dir), 0o700, "opening the store keeps it so");
}
