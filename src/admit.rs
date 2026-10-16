//! What a door refuses BEFORE a request reaches anything it names — and who a request is
//! from, as far as the door itself can say.
//!
//! Five decisions live here, made at the outermost endpoint of a door's kernel
//! ([`Admitting`]). The first three are made once per host-issued request and none of them is
//! a capability question; the fourth reads the capability for a token no endpoint requires; the
//! fifth re-reads the capability itself:
//!
//! 1. **Is this request one the door answers at all?** The HTTP door used to answer a foreign
//!    `Host` (DNS rebinding) and a cross-site write (a form on another site posting here) by
//!    computing an EMPTY capability. That is not "sees nothing": the manifold offers any
//!    action with no `requires` to an empty capability (ikigai-cli PR #335), so every
//!    capability-free resource was still served — gonk's pages, and the passkey ceremonies.
//!    Audit round 4 (ledger [#864](http://localhost:1060/l/default/item/864), R2) turned that
//!    into a lockout: 256 cross-site form posts to `/auth/login-options` filled the challenge
//!    table and every real sign-in answered 503 for five minutes, renewable.
//!
//!    ★ **The decision is made at the EDGE now, before anything answers** (ledger
//!    [#879](http://localhost:1060/l/default/item/879), item 1). `ikigai-web` answers three
//!    things without dispatching to the kernel — `OPTIONS` (the declared verbs), the
//!    `?description` face (the contract of every action offered to the capability) and the
//!    push stream — so a refusal made inside the kernel still disclosed those. Since
//!    `ikigai-web` 0.1.41 [`crate::doors::edge_config`] installs `EdgeConfig::admit_fn`
//!    ([`crate::doors::http_admit`]), which runs ahead of all of them and answers with
//!    [`refusal`]'s sentence — `421 Misdirected Request` for a foreign `Host` (ledger
//!    [#2](http://localhost:1060/l/default/item/2)), `403` for the rest
//!    ([`crate::doors::refusal_status`]). The capability function still computes the same refusal as a
//!    marker scope ([`REFUSED_FOREIGN_HOST`], [`REFUSED_CROSS_SITE`] — scopes no resource
//!    requires and no grant may name) and [`Admitting`] still answers a marker with `Denied`:
//!    one decision ([`crate::doors::http_refusal`]), two places that act on it, so a kernel
//!    served without the edge hook refuses exactly what the edge refuses.
//!
//!    ★ Since ledger [#880](http://localhost:1060/l/default/item/880) the same decision refuses
//!    a READ another page loads ([`REFUSED_FOREIGN_LOAD`]), and a read another page navigates
//!    to is answered under the READ half of the caller's authority ([`foreign_page_scopes`],
//!    marked [`FOREIGN_PAGE`]): a page on another localhost port is same-site, so it carried the
//!    signed-in session into `explain` and `review` through `ed43af3`.
//!
//! 2. **May this request name that author?** Brian's decision (a) on R4: a door refuses an
//!    `author` that is shaped like a principal this server names ([`is_principal_iri`]) unless
//!    it is the request's OWN principal ([`principal`]). A free-text author (`chris`,
//!    `roborev`) renders as text and stays allowed; an author the item page would render AS A
//!    PERSON can only be the person the door authenticated.
//!
//! 3. **Who wrote this, when the writer did not say?** (R4 option (b), Brian, 2026-10-08.) A
//!    QUIC write with no `author` is attributed to the client the door authenticated: its
//!    [`principal`] (minted into the capability) is filled in as `author` wherever the target declares one
//!    ([`fill_author`]). The HTTP door has done the same since ikigai-web 0.1.27, in the form
//!    adapter (`crate::web`'s `Act`), and the socket's caller is the owner, who is no
//!    principal this server names, so it fills nothing.
//!
//! 4. **May this request write the store RAW?** (ledger
//!    [#878](http://localhost:1060/l/default/item/878), option (b), Brian, 2026-10-09; the
//!    socket since ledger [#899](http://localhost:1060/l/default/item/899).) The
//!    author rule binds the AUTHOR ARGUMENT, and a raw store write passes none: a ledger
//!    writer holds that ledger's per-graph store write token, so through 1d02e53
//!    `urn:iki:store:graph-update` inserted a `ledger:author` triple naming anyone's passkey,
//!    and the item page rendered that person. A door now refuses a raw store write against a
//!    LEDGER graph unless the capability carries the explicit raw grant for that graph
//!    ([`raw_write_refusal`]). The ledger's own writes are untouched, structurally rather than
//!    by a depth test: `ikigai-ledger` issues its `graph-update` sub-requests inside the HUB
//!    kernel, under the caller's capability, and the hub has no admission layer — this one
//!    wraps only what arrives at a door.
//!
//! 5. **Does the client still hold this authority?** (ledger
//!    [#1710](http://localhost:1062/l/default/item/1710).) `ikigai-quic` mints a connection's
//!    authority ONCE, from `clients.json` and `grants.json` as they read at the handshake, and a
//!    client answering the door's keep-alive holds its connection until one side restarts: through
//!    9d4e722 a removed grant kept answering Hermes until gonk restarted. So the QUIC door re-reads
//!    both files on every call that arrives ([`standing_scopes`], the same read the minter makes):
//!    a client whose enrollment or grant is gone is refused, and one whose grant NARROWED is
//!    answered under the narrower authority, by issuing the call again one level down under the
//!    capability attenuated to the grant as it reads now. It cannot WIDEN a call (there is no
//!    such operation on a capability, by design), so a widened grant waits for the connection to
//!    age out ([`crate::quic::MAX_CONNECTION_AGE`]). The HTTP door has re-read `grants.json` per
//!    request since it had grants; this brings the QUIC door to the same rule.
//!
//! ★ **The socket runs rule 4 and nothing else** (ledger
//! [#899](http://localhost:1060/l/default/item/899)). Its caller is the owner (`0600`, peer UID
//! checked) and normally holds root, which passes; but the transport passes the caller's
//! capability through, so an owner process that NARROWED itself to a ledger grant (`cap seal`,
//! `ikigai mcp --grant` mounted on the socket) carried that grant's store token to the raw doors
//! and, through `ed43af3`, forged an author there. [`Admitting`] over [`Door::Socket`] holds it
//! to rule 4 exactly as a network door holds a client. Rules 1–3 stay off at the socket: no door computes a refusal marker there,
//! and the socket names no principal, so rule 2 would refuse the owner's every principal-shaped
//! author.
//!
//! ⚠ **What the doors cannot reach, stated so nobody believes otherwise.** With rule 2 off, an
//! owner process sealed to a ledger grant can still FILE an item whose `author` argument names
//! a principal — the ledger's own argument on a new item, never a triple on someone else's
//! (pinned in `tests/raw_store_write_878.rs`, `the_socket_keeps_no_author_rule`). An agent that
//! must not claim a person needs an identity of its own on the QUIC door. The owner at root
//! writes anything over the socket — that is where migrations and repairs run (`ikigai-gonk
//! ledger backfill-keys`), and the owner can read and write the dataset's files directly
//! anyway. And a holder of the raw grant ([`crate::grants::ledger_graph_grants`]) can still
//! write any triple into that graph — that is what the grant says, and only an operator mints
//! it.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, Error, InputSource, Invocation, Iri, Representation,
    Request, Resolution, Result, Scope, Space, SpaceEntry, Topology, Verb,
};

use crate::access::Door;

/// Every refusal marker starts here. A grant naming a scope under it is refused
/// (`quic::grant_refusal`), so only a door can compute one.
pub const REFUSED_PREFIX: &str = "urn:iki:gonk:door:refused:";

/// The HTTP door's answer to a `Host` that is not this server's loopback name.
pub const REFUSED_FOREIGN_HOST: &str = "urn:iki:gonk:door:refused:foreign-host";

/// The HTTP door's answer to a write whose `Origin` or `Sec-Fetch-Site` names another site.
pub const REFUSED_CROSS_SITE: &str = "urn:iki:gonk:door:refused:cross-site";

/// The HTTP door's answer to a read that a page which is not this server's own LOADS rather
/// than navigates to — an image, a script, a `fetch`, a frame (ledger
/// [#880](http://localhost:1060/l/default/item/880)). Nothing gonk serves is meant to be
/// embedded in or fetched by another page.
pub const REFUSED_FOREIGN_LOAD: &str = "urn:iki:gonk:door:refused:foreign-load";

/// Every door marker starts here: the refusals ([`REFUSED_PREFIX`]) and [`FOREIGN_PAGE`]. A
/// grant naming a scope under it is refused (`quic::grant_refusal`), so only a door computes one.
pub const DOOR_PREFIX: &str = "urn:iki:gonk:door:";

/// Carried by a request a page that is not this server's own NAVIGATED to (a link on another
/// site or another localhost port, a form with `method=get`, `location = …`), beside the READ
/// half of the caller's authority ([`foreign_page_scopes`]) — so the door can say why a
/// derivation was refused. Not a refusal: nothing requires it and nothing refuses on it alone.
pub const FOREIGN_PAGE: &str = "urn:iki:gonk:door:foreign-page";

/// What a request from a page that is not this server's own keeps of the caller's authority:
/// scopes in these families and nothing else (ledger
/// [#880](http://localhost:1060/l/default/item/880)).
///
/// ★ **An allowlist, so a family added later is dropped until someone decides otherwise.** Kept
/// is what a READ needs — the ledgers (whose writes need a mutating verb, which such a page
/// cannot send: [`REFUSED_CROSS_SITE`]), the store's per-graph READ tokens, the browse family's
/// read tokens, and `urn:cap:annotate` (exercised only by Sinks; kept so the queue renders its
/// controls). Dropped is everything whose USE is a spend or an act: `urn:cap:net:*` (every
/// derivation, and the `urn:llm:` mount itself, which checks it before it dials), `urn:cap:exec:*`
/// (programs), `urn:cap:fs:*`, the store's per-graph WRITE tokens, the raw-write grant, and
/// `urn:cap:kernel:inspect`.
pub const FOREIGN_PAGE_KEEPS: [&str; 4] = [
    "urn:cap:ledger:",
    "urn:cap:store:read:graph:",
    "urn:cap:browse:read:",
    "urn:cap:annotate",
];

/// `scopes`, as a page that is not this server's own may use them: the [`FOREIGN_PAGE_KEEPS`]
/// families, then the [`FOREIGN_PAGE`] marker.
///
/// ```
/// use ikigai_gonk::admit::{foreign_page_scopes, FOREIGN_PAGE};
/// let derive = [
///     "urn:cap:ledger:read:default",
///     "urn:cap:ledger:write:default",
///     "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
///     "urn:cap:store:write:graph:urn:iki:ledger:graph:default",
///     "urn:cap:browse:read:*",
///     "urn:cap:annotate",
///     "urn:cap:net:localhost",
///     "urn:cap:exec:git",
/// ]
/// .map(String::from);
/// assert_eq!(
///     foreign_page_scopes(&derive),
///     [
///         "urn:cap:ledger:read:default",
///         "urn:cap:ledger:write:default",
///         "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
///         "urn:cap:browse:read:*",
///         "urn:cap:annotate",
///         FOREIGN_PAGE,
///     ]
/// );
/// ```
pub fn foreign_page_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|scope| {
            FOREIGN_PAGE_KEEPS.iter().any(|kept| {
                if kept.ends_with(':') {
                    scope.starts_with(kept)
                } else {
                    scope.as_str() == *kept
                }
            })
        })
        .cloned()
        .chain(std::iter::once(FOREIGN_PAGE.to_string()))
        .collect()
}

/// The sentence a refusal under [`FOREIGN_PAGE`] ends with.
pub const FOREIGN_PAGE_SENTENCE: &str =
    "This request came from a page that is not this server's own (a link or form on another \
     site, or on another program's localhost port), and such a request carries only the READ \
     half of your authority: it may not spend inference, run a program or write. Open it from \
     gonk's own page, or type the address";

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
    } else if held.contains(REFUSED_FOREIGN_LOAD) {
        Some(
            "a page that is not this server's own may only NAVIGATE here (a link, or a typed \
             address); it may not load this server's resources as an image, script, frame or \
             fetch",
        )
    } else {
        held.iter()
            .any(|scope| scope.starts_with(REFUSED_PREFIX))
            .then_some("this door refused the request")
    }
}

/// The IRI prefixes this server names a PRINCIPAL by: a passkey (which the item page renders
/// as that person's label) and a QUIC client certificate ([`crate::quic::client_iri`]) — in
/// BOTH of its spellings.
///
/// ★ The client's pre-2026-10-10 spelling ([`crate::quic::LEGACY_CLIENT_IRI_PREFIX`]) is
/// never minted again and is still here, permanently (ledger
/// [#1183](http://localhost:1060/l/default/item/1183)): the record holds authors written in
/// it, so if it stopped being principal-shaped a client could write
/// `urn:iki:gonk:client:<another machine's fingerprint>` as plain text and be indistinguishable,
/// in the record, from that machine before the migration.
///
/// ★ And a layer binding's mirror ([`crate::layer::MIRROR_PRINCIPAL_PREFIX`], ledger
/// [#1190](http://localhost:1060/l/default/item/1190)): this process's own actor, minted by no
/// door, so no caller at any door can write as it.
pub const PRINCIPAL_PREFIXES: [&str; 4] = [
    crate::identity::PASSKEY_IRI_PREFIX,
    crate::quic::CLIENT_IRI_PREFIX,
    crate::quic::LEGACY_CLIENT_IRI_PREFIX,
    crate::layer::MIRROR_PRINCIPAL_PREFIX,
];

/// Whether `author` is shaped like a principal this server names — compared the way a
/// reader would: surrounding whitespace ignored, the prefix's case ignored.
///
/// ```
/// use ikigai_gonk::admit::is_principal_iri;
/// assert!(is_principal_iri("urn:iki:gonk:passkey:Q1JFRC1CUklBTg"));
/// assert!(is_principal_iri(" URN:IKI:GONK:PASSKEY:x"));
/// assert!(is_principal_iri("urn:iki:host:gonk:client:ab12"));
/// // The old spelling, recognized for ever: see `PRINCIPAL_PREFIXES`.
/// assert!(is_principal_iri("urn:iki:gonk:client:ab12"));
/// assert!(is_principal_iri(" URN:IKI:GONK:CLIENT:ab12"));
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

/// The argument both network transports stamp a request's principal under — the same name
/// `ikigai_quic::PRINCIPAL_ARG` and `ikigai-web`'s `PrincipalFn` use.
pub const PRINCIPAL_ARG: &str = "principal";

/// Who a request through `door` is from, as the DOOR knows it — never a value a caller
/// submitted: the principal the door minted into the request's capability
/// (`Capability::principal`, core 0.1.93; ledger [#1077](http://localhost:1060/l/default/item/1077)).
///
/// - **HTTP**: the signed-in passkey's IRI, minted by [`crate::doors::http_cap`] from the
///   session cookie, on every verb. An anonymous caller has none, and nothing a caller sends
///   (a `?principal=`, a header) reaches the capability.
/// - **QUIC**: the client's NAME ([`crate::quic::authority`], `urn:iki:host:gonk:client:<fingerprint>`).
///   A client carrying a narrower capability of its own is still named: core's `clamp` keeps
///   the session's principal and never takes the carried one's (ledger #879's failure, closed
///   in the capability itself).
/// - **socket**: `None` — the owner, who is no principal this server names. Its transport
///   passes a carried capability through under a root ceiling, name included, so a name there
///   would be the caller's own choice and is ignored.
///
/// Until 0.1.93 this read the `principal` ARGUMENT the transports stamp ([`PRINCIPAL_ARG`]),
/// which `ikigai-web` stamps on writes only. The argument is still stamped, and still what
/// [`claim_kind`] reads in the hub, where the door is not known.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_gonk::{access::Door, admit};
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let named = Capability::scoped(["urn:cap:ledger:read:default"]).with_principal(&laptop).unwrap();
/// assert_eq!(admit::principal(Door::Quic, &named).as_deref(), Some(laptop.as_str()));
/// assert_eq!(admit::principal(Door::Socket, &named), None);
/// assert_eq!(admit::principal(Door::Http, &Capability::scoped(["urn:cap:ledger:read:default"])), None);
/// ```
pub fn principal(door: Door, capability: &Capability) -> Option<String> {
    match door {
        Door::Quic | Door::Http => capability.principal().map(str::to_string),
        Door::Socket => None,
    }
}

/// The ledger's claim-kind stamper (ledger [#775](http://localhost:1060/l/default/item/775),
/// step 3): a claim whose request carries a PASSKEY principal is a `person`'s, every other a
/// `machine`'s. Brian, 2026-10-09: **passkey = person; a QUIC client, an agent grant, or none =
/// machine; the local socket = machine.**
///
/// ★ It reads the `principal` argument, and that is sound ONLY because every door OVERWRITES
/// that argument before the hub sees the request — so a caller can neither pose as a person
/// nor choose its kind (the ledger declares no `kind` input and ignores one):
///
/// | door | `principal` the hub sees |
/// | --- | --- |
/// | HTTP | stamped by `ikigai-web` on every WRITE from the session cookie; a `?principal=` a caller sends is dropped on every verb. The form adapter ([`crate::web`]'s `/act`) forwards the door's to the ledger action it issues |
/// | QUIC | stamped by `ikigai-quic` on every request, the connection's client IRI, after removing any the client sent |
/// | socket | REMOVED: nothing stamps one there, so a value would be the caller's own ([`crate::doors::HubSpace::naming_nobody`]) |
///
/// `tests/claim_kind_775.rs` proves each row against a real server: a caller-supplied
/// principal and `kind=` do not survive any door.
///
/// ★ **One more claimant: a layer binding's mirror** (ledger
/// [#1190](http://localhost:1060/l/default/item/1190)). It claims an item for the participant
/// who TOOK its offer, so the kind is that participant's, read from the collaboration's
/// participants graph ([`crate::layer::kind_for`]: `foaf:Person` is a person, anything else a
/// machine). It is recognized by the capability's principal, which only this process mints
/// ([`crate::layer::MIRROR_PRINCIPAL_PREFIX`]); no door can mint it.
///
/// ```
/// use ikigai_core::{ArgRef, Bindings, Capability, Invocation, Iri, Request, Verb};
/// use ikigai_ledger::claim::ClaimKind;
/// let stamp = ikigai_gonk::admit::claim_kind();
/// let kind = |principal: Option<&str>| {
///     let mut request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:claim").unwrap());
///     if let Some(p) = principal {
///         request = request.with_arg("principal", ArgRef::Inline(p.as_bytes().to_vec()));
///     }
///     let (bindings, cap) = (Bindings::new(), Capability::root());
///     stamp(&Invocation::detached(&request, &bindings, &cap))
/// };
/// assert_eq!(kind(Some("urn:iki:gonk:passkey:Q1JFRC1CUklBTg")), ClaimKind::Person);
/// assert_eq!(kind(Some("urn:iki:host:gonk:client:6f1c")), ClaimKind::Machine);
/// assert_eq!(kind(Some("urn:iki:gonk:client:6f1c")), ClaimKind::Machine);
/// assert_eq!(kind(None), ClaimKind::Machine);
/// ```
pub fn claim_kind() -> ikigai_ledger::claim::ClaimKindStamper {
    Arc::new(|inv: &Invocation<'_>| {
        if let Some(mirror) = inv
            .capability
            .principal()
            .filter(|p| p.starts_with(crate::layer::MIRROR_PRINCIPAL_PREFIX))
        {
            let holder = inv.inline_str("content").unwrap_or_default().trim();
            return crate::layer::kind_for(mirror, holder);
        }
        let person = match inv.request.args.get(PRINCIPAL_ARG) {
            Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes).is_ok_and(|p| {
                let prefix = crate::identity::PASSKEY_IRI_PREFIX;
                let p = p.trim();
                p.len() > prefix.len()
                    && p.is_char_boundary(prefix.len())
                    && p[..prefix.len()].eq_ignore_ascii_case(prefix)
            }),
            _ => false,
        };
        if person {
            ikigai_ledger::claim::ClaimKind::Person
        } else {
            ikigai_ledger::claim::ClaimKind::Machine
        }
    })
}

/// Why `request` may not name the `author` it carries, or `None` when it may.
///
/// An author shaped like a principal ([`is_principal_iri`]) must BE the request's own
/// ([`principal`]); any other author is text and passes. A QUIC client's own name counts in
/// either spelling ([`crate::quic::same_client`]): `urn:iki:gonk:client:<fp>` and
/// `urn:iki:host:gonk:client:<fp>` are one actor, so a client that names itself the old way
/// still writes as itself — and the old spelling of ANY OTHER fingerprint is refused like the
/// new one (ledger [#1183](http://localhost:1060/l/default/item/1183)).
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
/// let anonymous = Capability::scoped(["urn:cap:ledger:write:default"]);
/// let signed_in = anonymous.with_principal(brian).unwrap();
/// assert!(admit::author_refusal(Door::Http, &anonymous, &append(&[("author", brian)])).is_some());
/// assert!(admit::author_refusal(Door::Http, &anonymous, &append(&[("author", "chris")])).is_none());
/// assert!(admit::author_refusal(Door::Http, &signed_in, &append(&[("author", brian)])).is_none());
/// // A stamped ARGUMENT is not the door's word: only the capability's principal is.
/// assert!(admit::author_refusal(Door::Http, &anonymous,
///     &append(&[("author", brian), ("principal", brian)])).is_some());
/// // A QUIC client may name itself, and nobody else.
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let client = anonymous.with_principal(&laptop).unwrap();
/// let authored = |author: &str| append(&[("author", author)]);
/// assert!(admit::author_refusal(Door::Quic, &client, &authored(&laptop)).is_none());
/// assert!(admit::author_refusal(Door::Quic, &client, &authored(brian)).is_some());
/// assert!(admit::author_refusal(Door::Quic, &client, &authored("hermes")).is_none());
/// // Its own name in the pre-2026-10-10 spelling is still itself; another machine's is not.
/// assert!(admit::author_refusal(Door::Quic, &client, &authored("urn:iki:gonk:client:ab12")).is_none());
/// assert!(admit::author_refusal(Door::Quic, &client, &authored("urn:iki:gonk:client:cd34")).is_some());
/// ```
pub fn author_refusal(door: Door, capability: &Capability, request: &Request) -> Option<String> {
    let Some(ArgRef::Inline(bytes)) = request.args.get("author") else {
        return None;
    };
    let author = String::from_utf8_lossy(bytes);
    if !is_principal_iri(&author) {
        return None;
    }
    let own = principal(door, capability);
    if own
        .as_deref()
        .is_some_and(|own| crate::quic::same_client(own, author.trim()))
    {
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

/// `request` with its door's principal filled in as `author`, when that is what the door
/// does: a QUIC write that names no author, whose target `description` declares one for the
/// request's verb. `None` means "leave the request as it is" — any other door, a read, an
/// author already given (the author rule decides that one), no principal in the capability,
/// or a target that takes no `author`.
///
/// ★ R4 option (b), Brian, 2026-10-08: a write through the QUIC door is attributed to the
/// client the door authenticated unless it names an author itself. Only where the target
/// DECLARES `author`, the rule `crate::web`'s form adapter applies on the HTTP door, because
/// an argument a resource does not declare is one nobody reads.
///
/// ```
/// use ikigai_core::{ArgRef, ArgSpec, Capability, Description, Iri, Request, Verb};
/// use ikigai_gonk::{access::Door, admit};
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let client = Capability::scoped(["urn:cap:ledger:write:default"]).with_principal(&laptop).unwrap();
/// let append = |args: &[(&str, &str)]| {
///     args.iter().fold(
///         Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap()),
///         |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
///     )
/// };
/// let takes_author = Description::new("append")
///     .verb(Verb::Sink)
///     .input(ArgSpec::new("author").optional());
/// let filled = admit::fill_author(Door::Quic, &client, &append(&[]), &takes_author)
///     .expect("an unnamed QUIC write is attributed");
/// assert_eq!(filled.args.get("author"), Some(&ArgRef::Inline(laptop.clone().into_bytes())));
/// // Named already, another door, nobody named, or a target with no `author`: left alone.
/// let named = append(&[("author", "hermes")]);
/// assert!(admit::fill_author(Door::Quic, &client, &named, &takes_author).is_none());
/// assert!(admit::fill_author(Door::Socket, &client, &append(&[]), &takes_author).is_none());
/// let nobody = Capability::scoped(["urn:cap:ledger:write:default"]);
/// assert!(admit::fill_author(Door::Quic, &nobody, &append(&[]), &takes_author).is_none());
/// let no_author = Description::new("append").verb(Verb::Sink);
/// assert!(admit::fill_author(Door::Quic, &client, &append(&[]), &no_author).is_none());
/// ```
pub fn fill_author(
    door: Door,
    capability: &Capability,
    request: &Request,
    description: &Description,
) -> Option<Request> {
    if door != Door::Quic || !request.verb.is_mutating() || request.args.contains_key("author") {
        return None;
    }
    let principal = principal(door, capability)?;
    let declares = description
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == request.verb)
        .is_some_and(|spec| {
            spec.inputs
                .iter()
                .any(|input| input.name == "author" && input.source != InputSource::Binding)
        });
    declares.then(|| {
        request
            .clone()
            .with_arg("author", ArgRef::Inline(principal.into_bytes()))
    })
}

/// The description ids of `ikigai-store`'s raw write doors: `urn:iki:store:update` (any
/// SPARQL UPDATE over the whole dataset), `urn:iki:store:graph-update` (any SPARQL UPDATE,
/// confined to one graph) and `urn:iki:store:load` (a bulk load into any graph).
///
/// Matched on the RESOLVED endpoint's id, not on the target's spelling, so the rule follows
/// what actually answers. `tests/raw_store_write_878.rs` pins that the hub describes each of
/// the three IRIs with these ids, so a rename in the store fails there rather than silently
/// switching the rule off.
pub const RAW_WRITE_IDS: [&str; 3] = ["store-update", "store-graph-update", "store-load"];

/// Why `request`, resolved to the endpoint described as `id`, may not write the store raw
/// under `capability`, or `None` when it may.
///
/// ```text
/// not a write, or not a raw store write door            → admitted (the store decides)
/// urn:iki:store:graph-update naming a LEDGER graph G    → refused unless the capability holds
///                                                          grants::cap_raw_write_graph(G)
/// urn:iki:store:graph-update naming any other graph     → admitted (the store decides)
/// urn:iki:store:update, urn:iki:store:load              → refused unless root
/// ```
///
/// ★ **Why a token beside the store's, and not "drop the store token from the ledger grant".**
/// `ikigai-ledger` writes through `urn:iki:store:graph-update` under the CALLER's capability,
/// so a ledger writer must hold the per-graph store write token or `append` itself fails.
/// The same token was therefore two authorities — the ledger's sub-request authority and raw
/// quad authority — and no check on it alone can tell them apart. The raw grant says which:
/// only [`crate::grants::ledger_graph_grants`] mints it, and only an operator naming
/// `--ledger-graph` asks for that.
///
/// ⚠ A ledger graph is named by [`crate::grants::LEDGER_GRAPH_PREFIX`], compared ignoring
/// ASCII case — wider than the store, which matches its own token exactly, so a variant
/// spelling is refused here rather than argued about there. Another graph (the browse graph,
/// whose write token is minted ONLY as raw quad authority, `--browse-graph write`) is not this
/// rule's: the ambiguity it resolves exists only where a ledger grant carries the token.
///
/// `urn:iki:store:update` and `urn:iki:store:load` need the store's broad write token, which
/// [`crate::quic::grant_refusal`] refuses on every grant, so no identity at a network door can
/// reach them anyway. They are refused here too, because each reaches every ledger graph, and
/// a rule about ledger graphs that two of the three raw doors walked around would be stated
/// wrong.
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Iri, Request, Verb};
/// use ikigai_gonk::{admit, grants};
/// let update = |graph: &str| {
///     Request::new(Verb::Sink, Iri::parse("urn:iki:store:graph-update").unwrap())
///         .with_arg("graph", ArgRef::Inline(graph.as_bytes().to_vec()))
/// };
/// let ledger = grants::grants_for("default", grants::Authority::Write).unwrap();
/// let writer = Capability::scoped(ledger.clone());
/// let ledger_graph = "urn:iki:ledger:graph:default";
/// // A ledger writer may not write its ledger's graph raw...
/// assert!(admit::raw_write_refusal(&update(ledger_graph), "store-graph-update", &writer)
///     .is_some());
/// // ...an operator's raw grant may...
/// let raw = ledger.into_iter().chain(grants::ledger_graph_grants("default").unwrap());
/// assert!(admit::raw_write_refusal(&update(ledger_graph), "store-graph-update",
///     &Capability::scoped(raw)).is_none());
/// // ...the browse graph is not this rule's, and the whole-dataset doors need root.
/// let browse = "urn:iki:browse:graph:default";
/// assert!(admit::raw_write_refusal(&update(browse), "store-graph-update", &writer).is_none());
/// let load = Request::new(Verb::Sink, Iri::parse("urn:iki:store:load").unwrap());
/// assert!(admit::raw_write_refusal(&load, "store-load", &writer).is_some());
/// assert!(admit::raw_write_refusal(&load, "store-load", &Capability::root()).is_none());
/// ```
pub fn raw_write_refusal(request: &Request, id: &str, capability: &Capability) -> Option<String> {
    if !request.verb.is_mutating() || !RAW_WRITE_IDS.contains(&id) {
        return None;
    }
    if id != "store-graph-update" {
        return (!capability.is_root()).then(|| {
            format!(
                "`{}` writes the WHOLE dataset, every ledger graph included, and no grant this \
                 server admits may carry its token: it is the owner's, at root over the socket. A \
                 ledger is written through its own endpoints (`urn:iki:ledger:*`)",
                request.target.as_str()
            )
        });
    }
    // Not inline, or not UTF-8: the store refuses it on its own, naming the argument.
    let Some(ArgRef::Inline(bytes)) = request.args.get("graph") else {
        return None;
    };
    let graph = std::str::from_utf8(bytes).ok()?;
    let prefix = crate::grants::LEDGER_GRAPH_PREFIX;
    let is_ledger = graph.len() >= prefix.len()
        && graph.is_char_boundary(prefix.len())
        && graph[..prefix.len()].eq_ignore_ascii_case(prefix);
    if !is_ledger || capability.allows(&crate::grants::cap_raw_write_graph(graph)) {
        return None;
    }
    Some(format!(
        "a raw store write to the ledger graph <{graph}> is refused at this door. A ledger \
         grant carries that graph's store token for the ledger's OWN writes, which it issues \
         itself; it is not authority to write the graph's triples directly — that is how an \
         `author` naming someone else would get in. Write through the ledger's endpoints \
         (`urn:iki:ledger:*`). Raw writes are the owner's, at root over the socket, or an identity's \
         whose grant an operator minted with `--ledger-graph <ledger>` (`{}`)",
        crate::grants::cap_raw_write_graph(graph)
    ))
}

/// ★ **A machine drafts, a person publishes** (ledger [#939](http://localhost:1060/l/default/item/939)):
/// why a request that ARRIVED at the QUIC door may not change a script, or `None`.
///
/// The design's rule for agents — an agent may draft a script, a human publishes it, and the
/// review step IS the publish authority — enforced at the one door whose callers are machines
/// (a client certificate is a machine's identity; a person signs in with a passkey). A grant
/// cannot carry it: `grants.json` is one table for both identity doors, so a grant that may
/// publish on a passkey may on a certificate too.
///
/// ```text
/// Sink urn:script:{name} state=draft     → admitted (a draft is its author's alone)
/// Sink urn:script:{name}, any other state → refused: publishing is a person's
/// Delete urn:script:{name}                → refused: retiring is a person's
/// anything else (a run, a read, eval)     → not this rule's
/// ```
///
/// A person publishes a machine's draft by writing it again under their own grant — they cannot
/// read it first (a draft is its author's), so the text comes to them another way, the review
/// step the rule exists for — or the owner does, from the socket, which sees every draft.
///
/// ```
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::admit::machine_publish_refusal;
/// let sink = |state: Option<&str>| {
///     let request = Request::new(Verb::Sink, Iri::parse("urn:script:open").unwrap())
///         .with_arg("content", ArgRef::Inline(b"SELECT * {}".to_vec()));
///     match state {
///         Some(state) => request.with_arg("state", ArgRef::Inline(state.as_bytes().to_vec())),
///         None => request,
///     }
/// };
/// assert!(machine_publish_refusal("script", &sink(Some("draft"))).is_none());
/// assert!(machine_publish_refusal("script", &sink(None)).is_some());
/// assert!(machine_publish_refusal("script", &sink(Some("published"))).is_some());
/// let retire = Request::new(Verb::Delete, Iri::parse("urn:script:open").unwrap());
/// assert!(machine_publish_refusal("script", &retire).is_some());
/// let run = Request::new(Verb::Sink, Iri::parse("urn:script:open:runs").unwrap());
/// assert!(machine_publish_refusal("script-runs", &run).is_none());
/// ```
pub fn machine_publish_refusal(id: &str, request: &Request) -> Option<String> {
    if id != "script" {
        return None;
    }
    let target = request.target.as_str();
    match request.verb {
        Verb::Delete => Some(format!(
            "a client certificate may not retire `{target}`: a machine drafts, a person \
             publishes and retires (sign in with a passkey, or use the owner's socket)"
        )),
        Verb::Sink => {
            let state = match request.args.get("state") {
                Some(ArgRef::Inline(bytes)) => String::from_utf8_lossy(bytes).trim().to_string(),
                _ => String::new(),
            };
            (state != "draft").then(|| {
                format!(
                    "a client certificate may save `{target}` as a DRAFT (`state=draft`) and may \
                     not publish it: a machine drafts, a person publishes (sign in with a \
                     passkey, or use the owner's socket, which sees every draft)"
                )
            })
        }
        _ => None,
    }
}

/// What a call that ARRIVED on a QUIC connection under `capability` may use, as the client's
/// enrollment and grant read NOW ([`crate::quic::current_authority`]): `Ok(None)` when the
/// connection's authority stands, `Ok(Some(scopes))` when the grant has narrowed since the
/// client connected (answer the call under `capability` attenuated to `scopes`), and `Err` with
/// the reason when the client holds no authority any more.
///
/// A capability naming no QUIC client ([`crate::quic::client_fingerprint`] of its principal)
/// stands: gonk's minter names every client it admits, so such a capability came from a door
/// whose minter is not gonk's, and there is no enrollment to re-read for it.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_gonk::grants::{grants_for, Authority};
/// use ikigai_gonk::{admit::standing_scopes, quic};
/// let home = tempfile::tempdir().unwrap();
/// let layout = quic::Layout::in_config_home(home.path());
/// std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
/// let write = grants_for("default", Authority::Write).unwrap();
/// quic::enrol(&layout, "hermes", "ab12", &write, false).unwrap();
/// let (_, minted) = quic::current_authority(&layout, "ab12").unwrap();
/// // Unchanged: the connection's authority stands.
/// assert_eq!(standing_scopes(&layout, &minted), Ok(None));
/// // Narrowed to read: the call is answered under the read grant.
/// let read = grants_for("default", Authority::Read).unwrap();
/// quic::enrol(&layout, "hermes", "ab12", &read, true).unwrap();
/// let narrowed = standing_scopes(&layout, &minted).unwrap().expect("narrowed");
/// assert!(read.iter().all(|scope| narrowed.contains(scope)));
/// assert!(!narrowed.contains(&"urn:cap:ledger:write:default".to_string()));
/// // Unenrolled: refused.
/// assert!(quic::unenrol(&layout, "ab12").unwrap());
/// assert!(standing_scopes(&layout, &minted).is_err());
/// // A capability naming no client is not this rule's.
/// assert_eq!(standing_scopes(&layout, &Capability::scoped(write)), Ok(None));
/// ```
///
/// # Errors
///
/// The sentence the call is refused with: the client's enrollment or grant is gone, or a file
/// does not parse (fail closed, as the minter does).
pub fn standing_scopes(
    layout: &crate::quic::Layout,
    capability: &Capability,
) -> std::result::Result<Option<Vec<String>>, String> {
    let Some(fingerprint) = capability
        .principal()
        .and_then(crate::quic::client_fingerprint)
    else {
        return Ok(None);
    };
    let (_, current) = crate::quic::current_authority(layout, fingerprint).map_err(|why| {
        format!(
            "this client no longer holds any authority here: {why}. A QUIC client's enrollment \
             (gonk/clients.json) and grant (gonk/grants.json) are read again on every call, and \
             one of them changed after this connection was opened"
        )
    })?;
    let Some(scopes) = current.scopes() else {
        return Ok(None);
    };
    let scopes: Vec<String> = scopes.iter().cloned().collect();
    let narrowed = capability.attenuate(scopes.iter().cloned());
    Ok((narrowed != *capability).then_some(scopes))
}

/// A space that answers a door's refusals before anything it binds runs: an overlay with no
/// identity or structure of its own (it forwards [`Space::id`], [`Space::topology`] and
/// [`Space::entries`], so `urn:kernel:topology` reads the same with it or without it).
pub struct Admitting {
    inner: Arc<dyn Space>,
    door: Door,
    grants: Option<crate::quic::Layout>,
}

impl Admitting {
    /// Guard `inner` for requests that arrive through `door`.
    pub fn new(inner: Arc<dyn Space>, door: Door) -> Admitting {
        Admitting {
            inner,
            door,
            grants: None,
        }
    }

    /// Hold every request that arrives through the QUIC door to the client's authority as
    /// `grants`' files read when it arrives (rule 5, [`standing_scopes`]). `None`, and any
    /// other door, re-reads nothing.
    pub fn rechecking(mut self, grants: Option<crate::quic::Layout>) -> Admitting {
        self.grants = grants;
        self
    }
}

impl Space for Admitting {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        self.inner.resolve(request, scope).map_endpoint(|endpoint| {
            Arc::new(Admitted {
                inner: endpoint,
                door: self.door,
                grants: self.grants.clone(),
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

/// The endpoint [`Admitting`] resolves to: the real one, behind the door's checks — all of
/// them at a network door, only [`raw_write_refusal`] at the socket.
struct Admitted {
    inner: Arc<dyn Endpoint>,
    door: Door,
    grants: Option<crate::quic::Layout>,
}

impl Admitted {
    /// Answer `request` (this invocation's own, or the author-filled one) under the
    /// invocation's capability, or one level down under the narrower one rule 5 found.
    async fn answer(
        &self,
        inv: &Invocation<'_>,
        filled: Option<Request>,
        narrowed: Option<Vec<String>>,
    ) -> Result<Representation> {
        match (filled, narrowed) {
            (None, None) => self.inner.invoke(inv).await,
            (Some(filled), None) => inv.issue(filled).await,
            (filled, Some(scopes)) => {
                let request = filled.unwrap_or_else(|| inv.request.clone());
                inv.issue_attenuated(request, scopes).await
            }
        }
    }
}

#[async_trait]
impl Endpoint for Admitted {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // A sub-request is the work of a request this already admitted, under the same
        // capability; the door's refusals and the author rule are about what arrived at it.
        let arrived = inv.depth() == 0;
        // ★ The socket runs the raw-write rule and NOTHING else (ledger #899): its caller is
        // the owner, no door computes a refusal marker there, and it names no principal, so
        // the author rule would refuse the owner's every principal-shaped author.
        let network = self.door != Door::Socket;
        if arrived && network {
            if let Some(why) = refusal(inv.capability) {
                return Err(Error::Denied(why.to_string()));
            }
        }
        // Rule 5 (ledger #1710): the client's authority as its files read NOW. Refused here
        // when it has none; narrowed below, where the call is answered, after every other
        // rule has read the request as it arrived.
        let narrowed = match &self.grants {
            Some(layout) if arrived && self.door == Door::Quic => {
                standing_scopes(layout, inv.capability).map_err(Error::Denied)?
            }
            _ => None,
        };
        if !inv.request.verb.is_mutating() {
            let answer = self.answer(inv, None, narrowed).await;
            // A derivation refused because the request came from another page reads, from
            // the kernel, as "does not grant `urn:cap:net:*`" — to a person whose grant does.
            // Say which half of the authority went missing, and why.
            return match answer {
                Err(Error::Denied(why))
                    if arrived
                        && inv
                            .capability
                            .scopes()
                            .is_some_and(|held| held.contains(FOREIGN_PAGE)) =>
                {
                    Err(Error::Denied(format!("{why}. {FOREIGN_PAGE_SENTENCE}")))
                }
                other => other,
            };
        }
        // ★ At EVERY depth, unlike the checks around it (ledger #878). Nothing inside a door
        // kernel has a reason to write the store raw — the ledger's own `graph-update`s run
        // inside the hub, out of this overlay's sight — so a raw write that reaches this point
        // below depth 0 came from one of gonk's page adapters, and an adapter that forwarded
        // one would otherwise be a way round the door.
        let description = self.inner.describe();
        if let Some(why) = raw_write_refusal(inv.request, &description.id, inv.capability) {
            return Err(Error::Denied(why));
        }
        if arrived && network {
            if let Some(why) = author_refusal(self.door, inv.capability, inv.request) {
                return Err(Error::Denied(why));
            }
            if self.door == Door::Quic {
                if let Some(why) = machine_publish_refusal(&description.id, inv.request) {
                    return Err(Error::Denied(why));
                }
            }
            // Attributed by the door: the same request with `author` filled in, issued as
            // this request's own sub-request, so it resolves to this same endpoint one level
            // down (where only the raw-write rule runs again, and passes as it did here) under
            // the same capability. The access log is outside and counts it once, as the request
            // that arrived.
            if let Some(filled) = fill_author(self.door, inv.capability, inv.request, &description)
            {
                return self.answer(inv, Some(filled), narrowed).await;
            }
        }
        self.answer(inv, None, narrowed).await
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
