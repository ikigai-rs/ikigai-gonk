//! This process's instance id (ledger [#375](http://localhost:1062/l/default/item/375)): how a
//! page tells that the server behind it has MOVED.
//!
//! ★ Not the version. gonk is an application and `CARGO_PKG_VERSION` moves when someone
//! decides to move it, not when the code changes: it stayed put across three merged arcs on
//! 2026-09-16, while a browser kept showing the old UI and two people concluded a feature had
//! not shipped. A restart is a new process by construction, so an id drawn at startup changes
//! exactly when the server has, with no discipline from anyone about what to bump.
//!
//! Every full page carries it as `<meta name="gonk-instance">` ([`crate::render::render`]),
//! `GET /instance` ([`INSTANCE_PATH`]) answers it as plain text, and `web/gonk.js` compares the
//! two and says, in words, that the page is out of date. The banner names it too.
//!
//! It is a random token, not a secret: it says only that this process is not the one before.

use async_trait::async_trait;
use ikigai_core::{
    Description, Endpoint, Error, Invocation, ReprType, Representation, Result, Verb,
};
use std::sync::OnceLock;

/// The route `web/gonk.js` polls.
pub const INSTANCE_PATH: &str = "/instance";
/// The resource behind it.
pub const INSTANCE_IRI: &str = "urn:iki:gonk:instance";

static ID: OnceLock<String> = OnceLock::new();

/// This process's id: 128 random bits as 32 lowercase hex digits, drawn on the first call and
/// the same for every call after. `main` calls it before it serves, so it is drawn at startup.
///
/// ```
/// let id = ikigai_gonk::instance::id();
/// assert_eq!(id.len(), 32);
/// assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
/// assert_eq!(id, ikigai_gonk::instance::id());
/// ```
pub fn id() -> &'static str {
    ID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the operating system's random source");
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// `GET /instance`: [`id`], as `text/plain`.
pub struct Instance;

#[async_trait]
impl Endpoint for Instance {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "the instance id answers Source, not {:?}",
                inv.request.verb
            )));
        }
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            id().as_bytes().to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-instance"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-instance")
            .title("This server process's instance id")
            .summary(
                "A random id drawn when this process started, the same one every full page \
                 carries in <meta name=\"gonk-instance\">. A page whose id differs from this \
                 answer was served by an earlier process: the server has restarted, and may \
                 have been upgraded, since it loaded.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .output("text/plain")
    }
}
