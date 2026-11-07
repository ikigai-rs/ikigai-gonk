//! Ledger #3225: core 0.3 scopes held content to whoever delivered it (ledger #2965), and
//! `ContentStore::put`/`put_replay` became root-only. gonk holds no content of its own: its
//! HTTP door is `ikigai_web::serve_with_listener`, not `serve_with_uploads`, so a request body
//! reaches its Sink inline and nothing is ever spilled or `put`; scripts live in
//! `ikigai-script`'s `DirBackend`, files and not content ids. So no gonk kernel installs a
//! content store, and a content id names nothing on any of them.
//!
//! This test pins that. Should gonk ever install a content store (taking uploads, say), it
//! fails, and whoever wires that store must hold each body for its deliverer
//! (`put_for`/`put_replay_for` with the deliverer's capability) and replace this test with one
//! that a signed-in uploader's own Sink reads its upload and another caller's does not.

use std::sync::Arc;

use ikigai_core::{ContentId, Error, Issuer};
use ikigai_gonk::{compose, doors};
use ikigai_store::DurableStore;

fn assert_holds_no_content(kernel: &dyn Issuer, door: &str) {
    let id = ContentId::of(b"an upload gonk never took");
    match kernel.content(&id) {
        Err(Error::Unsupported { detail, .. }) => assert!(
            detail.contains("no content store"),
            "the {door} kernel refused a content id for another reason: {detail}"
        ),
        other => panic!(
            "the {door} kernel holds content ({other:?}): hold it for its deliverer \
             (put_for/put_replay_for) and test that only the deliverer reads it"
        ),
    }
}

#[test]
fn no_gonk_kernel_holds_content() {
    let hub = Arc::new(compose(DurableStore::in_memory().unwrap()));
    assert_holds_no_content(hub.as_ref(), "hub");
    let socket = doors::door_kernel(Arc::clone(&hub));
    assert_holds_no_content(&socket, "socket door");
}
