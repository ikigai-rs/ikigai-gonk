//! The state key a scripted layer host's tests open it with (ikigai-layers 0.3.0 seals a host's
//! state at rest, so `Host::open` takes a `StateKey`; ledger #2021).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use age::secrecy::ExposeSecret;
use ikigai_layers::StateKey;

/// Mint a fresh age X25519 identity, write it as an identity file under `dir`, and load it, the
/// way the layers crate's own tests do. A fresh key per host: nothing reads the state twice.
pub fn state_key(dir: &Path) -> StateKey {
    // A writable parent would let another account replace even an owner-only key.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .expect("secure the test state key directory");
    let identity = age::x25519::Identity::generate();
    let path = dir.join("state.key");
    // Layers 0.10 refuses an identity another account can read or replace. Create it
    // owner-only from the start, regardless of the worker's umask.
    let mut key = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .expect("create the test state key");
    writeln!(
        key,
        "# created by a test\n{}",
        identity.to_string().expose_secret()
    )
    .expect("write the test state key");
    StateKey::load(&path).expect("load the test state key")
}
