//! The state key a scripted layer host's tests open it with (ikigai-layers 0.3.0 seals a host's
//! state at rest, so `Host::open` takes a `StateKey`; ledger #2021).

use std::path::Path;

use age::secrecy::ExposeSecret;
use ikigai_layers::StateKey;

/// Mint a fresh age X25519 identity, write it as an identity file under `dir`, and load it, the
/// way the layers crate's own tests do. A fresh key per host: nothing reads the state twice.
pub fn state_key(dir: &Path) -> StateKey {
    let identity = age::x25519::Identity::generate();
    let path = dir.join("state.key");
    std::fs::write(
        &path,
        format!(
            "# created by a test\n{}\n",
            identity.to_string().expose_secret()
        ),
    )
    .expect("write the test state key");
    StateKey::load(&path).expect("load the test state key")
}
