//! The page a browser gets when it NAVIGATES to something the HTTP door answers with an error:
//! gonk's own page shell (header, navigation, styles, script) around the refusal, with the
//! passkey sign-in ON the page when signing in would cure it (ledger
//! [#2021](http://localhost:1062/l/default/item/2021), UI conventions D14: one refusal shell).
//!
//! Brian, 2026-10-20, on the library's bare 403 after a restart signed him out: *"It's not
//! styled, it should offer the passkey, etc."* `ikigai-web` 0.1.67 gives a door the seam for it,
//! [`EdgeConfig::error_page`] (ledger [#2025](http://localhost:1062/l/default/item/2025)), and
//! [`install`] is gonk's.
//!
//! ```text
//! a browser navigation (Sec-Fetch-Mode: navigate) that wants HTML, not htmx
//!   ├─ a kernel Denied, nobody signed in, passkeys on  → SIGN IN: the sentence + "Sign in with passkey"
//!   ├─ a kernel Denied, signed in                      → GRANT: the sentence + the grant it needs, no sign-in
//!   └─ anything else (404, 429, 5xx, an edge refusal)  → ERROR: "404 Not Found" + the message
//! everything else (curl, a script's fetch, htmx)      → the library's answer, unchanged
//! ```
//!
//! **The status and every header are the library's**: the hook writes only the body. The two
//! sentences are `web::Lacking`'s, read back from the refusal's text, so the page and the
//! plain-text refusal an agent reads say the same words. Signing in reloads the page
//! (`web/gonk.js` `signIn`), so the browser lands on the address it asked for.
//!
//! ★ **Which of the first two a refusal is comes from the REQUEST, not the text**: the same
//! [`doors::http_cap`] that decided the refusal decides the header here, and its principal is
//! the session cookie through [`crate::identity::Passkeys::identity`]. A refusal the edge wrote
//! before any capability existed (a cross-site write: [`ErrorView::refusal`]) is no sign-in's
//! to cure, and its page draws no navigation at all; a foreign `Host` keeps the library's page,
//! because the shell's stylesheet and script would be fetched from that host.
//!
//! ⚠ A browser sends `Sec-Fetch-Mode` only to a potentially trustworthy origin, so a gonk
//! reached over plain `http://` on a LAN address keeps the library's minimal page. gonk binds
//! loopback by default, where `localhost` is trustworthy.

use std::sync::Arc;

use ikigai_core::{Capability, Error};
use ikigai_web::{EdgeConfig, ErrorPageFn, ErrorView, HttpRequest};

use crate::doors::{self, HttpDoor};
use crate::render::{self, element, envelope};
use crate::web::{self, Web};

/// Which page a refusal gets: see the module docs' table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A kernel refusal of a caller nobody signed in as, on a server with passkeys: signing in
    /// may cure it, so the page offers it.
    SignIn,
    /// A kernel refusal of a signed-in caller: the grant lacks what the page needs, and signing
    /// in again cures nothing.
    Grant,
    /// Any other error: the status, and the message.
    Error,
}

impl Kind {
    /// The word the stylesheet dispatches on (`view:refusal/@kind`).
    fn word(self) -> &'static str {
        match self {
            Kind::SignIn => "signin",
            Kind::Grant => "grant",
            Kind::Error => "error",
        }
    }
}

/// The page's heading when signing in may cure the refusal.
pub const SIGN_IN_HEADING: &str = "Sign in to see this page";

/// The page's heading when the caller is signed in and its grant does not reach the page.
pub const GRANT_HEADING: &str = "Your grant doesn't include this page";

/// `config` with gonk's error page installed: [`EdgeConfig::error_page`], over the same `door`
/// whose capability decided the refusal (so the page's header is the one the caller's grant
/// draws everywhere else) and the page face's `web`.
pub fn install(mut config: EdgeConfig, door: HttpDoor, web: Arc<Web>) -> EdgeConfig {
    config.error_page = Some(error_page(door, web));
    config
}

/// The hook itself: a page for a NAVIGATION, `None` for anything else (a script's `fetch`
/// keeps the library's answer), and `None` too when the page cannot be drawn, so a failed
/// render is the library's minimal page, never a second error.
pub fn error_page(door: HttpDoor, web: Arc<Web>) -> ErrorPageFn {
    let passkeys = door.passkeys.is_some();
    let cap = doors::http_cap(door.clone());
    Arc::new(move |request: &HttpRequest, view: &ErrorView<'_>| {
        if !view.navigation {
            return None;
        }
        // A foreign `Host` is not this server's page to draw: the browser would fetch its
        // stylesheet and script from that host, which this door refuses too. The library's
        // minimal page needs nothing fetched.
        if doors::http_refusal(&door, request) == Some(crate::admit::REFUSED_FOREIGN_HOST) {
            return None;
        }
        // An edge refusal was written before any authority was computed: no header to draw.
        let capability = view.refusal.is_none().then(|| cap(request));
        page(&web, capability.as_ref(), passkeys, view).ok()
    })
}

/// Which [`Kind`] of page `view` gets, for a caller holding `capability` (`None` for an edge
/// refusal) on a server with passkeys on or off.
pub fn kind(capability: Option<&Capability>, passkeys: bool, view: &ErrorView<'_>) -> Kind {
    let Some(capability) = capability else {
        return Kind::Error;
    };
    if !matches!(view.error, Some(Error::Denied(_))) {
        return Kind::Error;
    }
    match capability.principal() {
        Some(_) => Kind::Grant,
        None if passkeys => Kind::SignIn,
        None => Kind::Error,
    }
}

/// The whole page, through the one stylesheet every gonk page is drawn with.
pub fn page(
    web: &Web,
    capability: Option<&Capability>,
    passkeys: bool,
    view: &ErrorView<'_>,
) -> Result<String, String> {
    let kind = kind(capability, passkeys, view);
    let status = format!("{} {}", view.status, view.reason);
    let status = status.trim();
    let (heading, sentence, grant) = match (kind, view.error) {
        (Kind::SignIn, Some(Error::Denied(text))) => {
            let (sentence, _) = sentences(text);
            (SIGN_IN_HEADING.to_string(), sentence, None)
        }
        (Kind::Grant, Some(Error::Denied(text))) => {
            let (sentence, grant) = sentences(text);
            (GRANT_HEADING.to_string(), sentence, grant)
        }
        _ => (status.to_string(), view.message.trim().to_string(), None),
    };
    let mut attributes = vec![("kind", kind.word()), ("heading", heading.as_str())];
    if !sentence.is_empty() {
        attributes.push(("sentence", sentence.as_str()));
    }
    if let Some(grant) = grant.as_deref() {
        attributes.push(("grant", grant));
    }
    let mut children = capability
        .map(|capability| web::nav_for(web, capability))
        .unwrap_or_default();
    children.push_str(&element("refusal", &attributes, ""));
    let document = envelope(
        "page",
        &[("view", "refusal"), ("full", "true"), ("title", &heading)],
        &children,
    );
    render::render(&document, true)
}

/// A kernel refusal's text as the page shows it: the sentence, and the grant line's value when
/// it has one ([`web::GRANT_LINE`]). A signed-out sentence loses [`web::SIGN_IN_WHERE`], which
/// tells a reader of the PLAIN refusal where the sign-in is: on this page it is right there.
///
/// ```
/// use ikigai_gonk::refusal::sentences;
/// let text = "Sign in to read the ledger \"ventures\". Sign in with passkey is in the header \
///             of every gonk page (the front page is /); then come back to this address.\n\
///             The grant it needs: urn:cap:ledger:read:ventures (to read the ledger \"ventures\")";
/// assert_eq!(
///     sentences(text),
///     (
///         "Sign in to read the ledger \"ventures\".".to_string(),
///         Some("urn:cap:ledger:read:ventures (to read the ledger \"ventures\")".to_string()),
///     )
/// );
/// // A refusal gonk did not word is shown as it is.
/// assert_eq!(sentences("no grant names this"), ("no grant names this".to_string(), None));
/// ```
pub fn sentences(text: &str) -> (String, Option<String>) {
    let mut words = Vec::new();
    let mut grant = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line.strip_prefix(web::GRANT_LINE) {
            Some(rest) => grant = Some(rest.trim().to_string()),
            None => words.push(line),
        }
    }
    let sentence = words.join(" ");
    let sentence = sentence
        .strip_suffix(web::SIGN_IN_WHERE)
        .map(str::trim_end)
        .unwrap_or(&sentence)
        .to_string();
    (sentence, grant)
}
