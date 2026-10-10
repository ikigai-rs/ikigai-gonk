//! Readiness for a socket door a test serves IN-PROCESS: wait until it ANSWERS, never until
//! its file exists (ledger [#1028](http://localhost:1060/l/default/item/1028)).
//!
//! ⚠ The socket's file appearing is not the socket listening. `ikigai_ipc::serve` binds, which
//! creates the file, and only then listens; a connect in between is refused (`ECONNREFUSED`).
//! Measured on `a0bf096` with the old `while !socket.exists()` wait: 15 of 120 runs of
//! `tests/raw_store_write_878.rs` at 24 concurrent copies failed with `connect: … Connection
//! refused`, and `tests/access.rs` 1 of 480 at 48. So the wait is for a connection that
//! completes the version hello, and that connection is the one handed back.
//!
//! The child-process twin is `tests/scratch_gonk/mod.rs`, which asks every door the same way.

use std::path::Path;
use std::time::{Duration, Instant};

/// Wait (bounded) until the `ikigai_ipc` socket at `path` accepts a connection and completes
/// the hello, and return that connection. A caller that dials its own later can drop it.
pub fn socket(path: &Path) -> ikigai_ipc::IpcResolver {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match ikigai_ipc::connect(path) {
            Ok(client) => return client,
            Err(e) if Instant::now() > deadline => {
                panic!("the socket at {} never answered: {e}", path.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
