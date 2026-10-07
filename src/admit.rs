//! What a network door refuses BEFORE a request reaches anything it names — and who a request
//! is from, as far as the door itself can say.
//!
//! Two decisions live here, both made once per host-issued request at the outermost endpoint
//! of a door's kernel ([`Admitting`]), and neither is a capability question:
//!
//! 1. **Is this request one the door answers at all?** The HTTP door used to answer a foreign
//!    `Host` (DNS rebinding) and a cross-site write (a form on another site posting here) by
//!    computing an EMPTY capability. That is not "sees nothing": the manifold offers any
//!    action with no `requires` to an empty capability (ikigai-cli PR #335), so every
//!    capability-free resource was still served — gonk's pages, and the passkey ceremonies.
//!    Audit round 4 (ledger [#864](http://localhost:1060/l/default/item/864), R2) turned that
//!    into a lockout: 256 cross-site form posts to `/auth/login-options` filled the challenge
//!    table and every real sign-in answered 503 for five minutes, renewable. So the door now
//!    computes a REFUSAL instead ([`REFUSED_FOREIGN_HOST`], [`REFUSED_CROSS_SITE`] — marker
//!    scopes no resource requires and no grant may name), and [`Admitting`] answers it with
//!    `Denied` (a `403`) before any endpoint runs. PENDING item 2 asked for exactly this.
//!
//! 2. **May this request name that author?** Brian's decision (a) on R4: a door refuses an
//!    `author` that is shaped like a principal this server names ([`is_principal_iri`]) unless
//!    it is the request's OWN principal ([`principal`]). A free-text author (`chris`,
//!    `roborev`) renders as text and stays allowed; an author the item page would render AS A
//!    PERSON can only be the person the door authenticated.
//!
//! ⚠ **What this cannot reach, stated so nobody believes otherwise.** `ikigai-web` answers two
//! things without dispatching to the kernel at all, so no overlay sees them: `OPTIONS` (a
//! `204` with the declared verbs) and the `?description` face (the contract of every action
//! the request's capability is offered — which, for a refusal marker, is the capability-free
//! ones). Both disclose a CONTRACT and change nothing. Closing them needs a pre-dispatch
//! admission hook in the library (reported up; the shape is a function of the request that
//! answers a status before routing).
//!
//! And the author rule binds the AUTHOR ARGUMENT. A ledger writer also holds that ledger's
//! per-graph store write token, so `urn:iki:store:graph-update` can still insert a
//! `ledger:author` triple directly; the face that trusts only door-stamped authors (R4's
//! option (b)) is what would close that, and it was not the decision.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, Error, Invocation, Iri, Representation, Request,
    Resolution, Result, Scope, Space, SpaceEntry, Topology,
};

use crate::access::Door;

/// Every refusal marker starts here. A grant naming a scope under it is refused
/// (`quic::grant_refusal`), so only a door can compute one.
pub const REFUSED_PREFIX: &str = "urn:iki:gonk:door:refused:";

/// The HTTP door's answer to a `Host` that is not this server's loopback name.
pub const REFUSED_FOREIGN_HOST: &str = "urn:iki:gonk:door:refused:foreign-host";

/// The HTTP door's answer to a write whose `Origin` or `Sec-Fetch-Site` names another site.
pub const REFUSED_CROSS_SITE: &str = "urn:iki:gonk:door:refused:cross-site";

/// The sentence a refusal is answered with, when `capability` carries a refusal marker.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_gonk::admit;
/// let refused = Capability::scoped([admit::REFUSED_CROSS_SITE.to_string()]);
/// assert!(admit::refusal(&refused).unwrap().contains("another site"));
/// assert_eq!(admit::refusal(&Capability::scoped(Vec::<String>::new())), None);
/// assert_eq!(admit::refusal(&Capability::root()), None);
/// ```
pub fn refusal(capability: &Capability) -> Option<&'static str> {
    let held = capability.scopes()?;
    if held.contains(REFUSED_FOREIGN_HOST) {
        Some(
            "this server answers only to its own loopback name (localhost, 127.0.0.1 or [::1], \
             with its port); a request under another Host is refused — it is how a DNS \
             rebinding page reaches a loopback server",
        )
    } else if held.contains(REFUSED_CROSS_SITE) {
        Some(
            "a write from another site is refused: its Origin or Sec-Fetch-Site names a page \
             that is not this server's own",
        )
    } else {
        held.iter()
            .any(|scope| scope.starts_with(REFUSED_PREFIX))
            .then_some("this door refused the request")
    }
}

/// The IRI prefixes this server names a PRINCIPAL by: a passkey (which the item page renders
/// as that person's label) and a QUIC client certificate ([`crate::quic::client_iri`]).
pub const PRINCIPAL_PREFIXES: [&str; 2] = [
    crate::identity::PASSKEY_IRI_PREFIX,
    crate::quic::CLIENT_IRI_PREFIX,
];

/// Whether `author` is shaped like a principal this server names — compared the way a
/// reader would: surrounding whitespace ignored, the prefix's case ignored.
///
/// ```
/// use ikigai_gonk::admit::is_principal_iri;
/// assert!(is_principal_iri("urn:iki:gonk:passkey:Q1JFRC1CUklBTg"));
/// assert!(is_principal_iri(" URN:IKI:GONK:PASSKEY:x"));
/// assert!(is_principal_iri("urn:iki:gonk:client:ab12"));
/// assert!(!is_principal_iri("chris"));
/// assert!(!is_principal_iri("roborev"));
/// ```
pub fn is_principal_iri(author: &str) -> bool {
    let author = author.trim();
    PRINCIPAL_PREFIXES.iter().any(|prefix| {
        author.len() >= prefix.len()
            && author.is_char_boundary(prefix.len())
            && author[..prefix.len()].eq_ignore_ascii_case(prefix)
    })
}

/// Who a request through `door` is from, as the DOOR knows it — never a value a caller
/// submitted.
///
/// - **HTTP**: the `principal` argument on a WRITE. `ikigai-web` drops a submitted
///   `?principal=` on a mutating verb and stamps its own (`doors::http_principal`), so there it
///   is the door's. On a READ the library forwards a submitted one untouched (ledger #864, R6:
///   its half is ikigai-cli's), so a read names nobody here.
/// - **QUIC**: the client's NAME the door put in the connection's capability
///   ([`crate::quic::authority`]) — never in a request, so a caller cannot type it. Absent
///   when a client carried a narrower capability of its own (the clamp drops it).
/// - **socket**: `None` — the owner, who is no principal this server names.
pub fn principal(door: Door, request: &Request, capability: &Capability) -> Option<String> {
    match door {
        Door::Quic => capability.scopes().and_then(|held| {
            held.iter()
                .find(|scope| scope.starts_with(crate::quic::CLIENT_IRI_PREFIX))
                .cloned()
        }),
        Door::Http if request.verb.is_mutating() => match request.args.get("principal") {
            Some(ArgRef::Inline(bytes)) => {
                let value = String::from_utf8_lossy(bytes);
                (!value.is_empty() && value.chars().all(|c| c.is_ascii_graphic()))
                    .then(|| value.into_owned())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Why `request` may not name the `author` it carries, or `None` when it may.
///
/// An author shaped like a principal ([`is_principal_iri`]) must BE the request's own
/// ([`principal`]); any other author is text and passes.
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::{access::Door, admit};
/// let append = |args: &[(&str, &str)]| {
///     args.iter().fold(
///         Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap()),
///         |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
///     )
/// };
/// let brian = "urn:iki:gonk:passkey:QlJJQU4";
/// let anyone = Capability::scoped(Vec::<String>::new());
/// assert!(admit::author_refusal(Door::Http, &append(&[("author", brian)]), &anyone).is_some());
/// assert!(admit::author_refusal(Door::Http, &append(&[("author", "chris")]), &anyone).is_none());
/// assert!(admit::author_refusal(
///     Door::Http,
///     &append(&[("author", brian), ("principal", brian)]),
///     &anyone
/// )
/// .is_none());
/// // A QUIC client may name itself, and nobody else.
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let connected = Capability::scoped([laptop.clone()]);
/// assert!(admit::author_refusal(Door::Quic, &append(&[("author", &laptop)]), &connected).is_none());
/// assert!(admit::author_refusal(Door::Quic, &append(&[("author", brian)]), &connected).is_some());
/// ```
pub fn author_refusal(door: Door, request: &Request, capability: &Capability) -> Option<String> {
    let Some(ArgRef::Inline(bytes)) = request.args.get("author") else {
        return None;
    };
    let author = String::from_utf8_lossy(bytes);
    if !is_principal_iri(&author) {
        return None;
    }
    let own = principal(door, request, capability);
    if own.as_deref() == Some(author.trim()) {
        return None;
    }
    Some(format!(
        "`author` names a principal ({}), and only the door names one: a write may name its \
         own principal or a plain-text author, never another identity. {}",
        author.trim(),
        match own {
            Some(own) => format!("This request is from {own}."),
            None => "This request is from nobody the door authenticated.".to_string(),
        }
    ))
}

/// A space that answers a door's refusals before anything it binds runs: an overlay with no
/// identity or structure of its own (it forwards [`Space::id`], [`Space::topology`] and
/// [`Space::entries`], so `urn:kernel:topology` reads the same with it or without it).
pub struct Admitting {
    inner: Arc<dyn Space>,
    door: Door,
}

impl Admitting {
    /// Guard `inner` for requests that arrive through `door`.
    pub fn new(inner: Arc<dyn Space>, door: Door) -> Admitting {
        Admitting { inner, door }
    }
}

impl Space for Admitting {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        self.inner.resolve(request, scope).map_endpoint(|endpoint| {
            Arc::new(Admitted {
                inner: endpoint,
                door: self.door,
            }) as Arc<dyn Endpoint>
        })
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

/// The endpoint [`Admitting`] resolves to: the real one, behind the two checks.
struct Admitted {
    inner: Arc<dyn Endpoint>,
    door: Door,
}

#[async_trait]
impl Endpoint for Admitted {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // A sub-request is the work of a request this already admitted, under the same
        // capability; the checks are about what arrived at the door.
        if inv.depth() == 0 {
            if let Some(why) = refusal(inv.capability) {
                return Err(Error::Denied(why.to_string()));
            }
            if inv.request.verb.is_mutating() {
                if let Some(why) = author_refusal(self.door, inv.request, inv.capability) {
                    return Err(Error::Denied(why));
                }
            }
        }
        self.inner.invoke(inv).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }

    fn confinement(&self) -> Option<Topology> {
        self.inner.confinement()
    }
}
