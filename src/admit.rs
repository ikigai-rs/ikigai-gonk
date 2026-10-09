//! What a door refuses BEFORE a request reaches anything it names — and who a request is
//! from, as far as the door itself can say.
//!
//! Four decisions live here, made at the outermost endpoint of a door's kernel
//! ([`Admitting`]). The first three are made once per host-issued request and none of them is
//! a capability question; the fourth reads the capability for a token no endpoint requires:
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
//!    ([`crate::doors::http_admit`]), which runs ahead of all of them and answers `403` with
//!    [`refusal`]'s sentence. The capability function still computes the same refusal as a
//!    marker scope ([`REFUSED_FOREIGN_HOST`], [`REFUSED_CROSS_SITE`] — scopes no resource
//!    requires and no grant may name) and [`Admitting`] still answers a marker with `Denied`:
//!    one decision ([`crate::doors::http_refusal`]), two places that act on it, so a kernel
//!    served without the edge hook refuses exactly what the edge refuses.
//!
//! 2. **May this request name that author?** Brian's decision (a) on R4: a door refuses an
//!    `author` that is shaped like a principal this server names ([`is_principal_iri`]) unless
//!    it is the request's OWN principal ([`principal`]). A free-text author (`chris`,
//!    `roborev`) renders as text and stays allowed; an author the item page would render AS A
//!    PERSON can only be the person the door authenticated.
//!
//! 3. **Who wrote this, when the writer did not say?** (R4 option (b), Brian, 2026-10-08.) A
//!    QUIC write with no `author` is attributed to the client the door authenticated: the
//!    stamped [`principal`] is filled in as `author` wherever the target declares one
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
    Request, Resolution, Result, Scope, Space, SpaceEntry, Topology,
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

/// The argument both network transports stamp a request's principal under — the same name
/// `ikigai_quic::PRINCIPAL_ARG` and `ikigai-web`'s `PrincipalFn` use.
pub const PRINCIPAL_ARG: &str = "principal";

/// Who a request through `door` is from, as the DOOR knows it — never a value a caller
/// submitted. Both network doors hand it over the same way: the inline argument
/// [`PRINCIPAL_ARG`], which the TRANSPORT stamps after removing any a caller sent.
///
/// - **HTTP**: on a WRITE, the passkey IRI `doors::http_principal` computed from the session
///   cookie. `ikigai-web` stamps writes only, and drops a submitted `?principal=` on every verb
///   (on reads too since 0.1.41; ledger #864 R6, #879) — so a read names nobody here, and the
///   write-only test below is belt-and-braces over the library rather than the only guard.
/// - **QUIC**: on every verb, the client's NAME the minter set on the connection's session
///   ([`crate::quic::minter`], `urn:iki:gonk:client:<fingerprint>`). It does not travel
///   through the capability, so a client carrying a narrower capability of its own is still
///   named (ledger #879).
/// - **socket**: `None` — the owner, who is no principal this server names. Nothing stamps
///   the argument on that door, so a value there would be the caller's own and is ignored.
pub fn principal(door: Door, request: &Request) -> Option<String> {
    let stamped = || match request.args.get(PRINCIPAL_ARG) {
        Some(ArgRef::Inline(bytes)) => {
            let value = String::from_utf8_lossy(bytes);
            (!value.is_empty() && value.chars().all(|c| c.is_ascii_graphic()))
                .then(|| value.into_owned())
        }
        _ => None,
    };
    match door {
        Door::Quic => stamped(),
        Door::Http if request.verb.is_mutating() => stamped(),
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
/// assert!(admit::author_refusal(Door::Http, &append(&[("author", brian)])).is_some());
/// assert!(admit::author_refusal(Door::Http, &append(&[("author", "chris")])).is_none());
/// assert!(admit::author_refusal(Door::Http, &append(&[("author", brian), ("principal", brian)]))
///     .is_none());
/// // A QUIC client may name itself, and nobody else.
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let stamped = |author: &str| append(&[("author", author), ("principal", &laptop)]);
/// assert!(admit::author_refusal(Door::Quic, &stamped(&laptop)).is_none());
/// assert!(admit::author_refusal(Door::Quic, &stamped(brian)).is_some());
/// assert!(admit::author_refusal(Door::Quic, &stamped("hermes")).is_none());
/// ```
pub fn author_refusal(door: Door, request: &Request) -> Option<String> {
    let Some(ArgRef::Inline(bytes)) = request.args.get("author") else {
        return None;
    };
    let author = String::from_utf8_lossy(bytes);
    if !is_principal_iri(&author) {
        return None;
    }
    let own = principal(door, request);
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

/// `request` with its door's principal filled in as `author`, when that is what the door
/// does: a QUIC write that names no author, whose target `description` declares one for the
/// request's verb. `None` means "leave the request as it is" — any other door, a read, an
/// author already given (the author rule decides that one), no stamped principal, or a target
/// that takes no `author`.
///
/// ★ R4 option (b), Brian, 2026-10-08: a write through the QUIC door is attributed to the
/// client the door authenticated unless it names an author itself. Only where the target
/// DECLARES `author`, the rule `crate::web`'s form adapter applies on the HTTP door, because
/// an argument a resource does not declare is one nobody reads.
///
/// ```
/// use ikigai_core::{ArgRef, ArgSpec, Description, Iri, Request, Verb};
/// use ikigai_gonk::{access::Door, admit};
/// let laptop = ikigai_gonk::quic::client_iri("ab12");
/// let append = |args: &[(&str, &str)]| {
///     args.iter().fold(
///         Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap()),
///         |r, (k, v)| r.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec())),
///     )
/// };
/// let takes_author = Description::new("append")
///     .verb(Verb::Sink)
///     .input(ArgSpec::new("author").optional());
/// let filled = admit::fill_author(Door::Quic, &append(&[("principal", &laptop)]), &takes_author)
///     .expect("an unnamed QUIC write is attributed");
/// assert_eq!(filled.args.get("author"), Some(&ArgRef::Inline(laptop.clone().into_bytes())));
/// // Named already, another door, or a target with no `author`: left alone.
/// let named = append(&[("principal", &laptop), ("author", "hermes")]);
/// assert!(admit::fill_author(Door::Quic, &named, &takes_author).is_none());
/// assert!(admit::fill_author(Door::Socket, &append(&[("principal", &laptop)]), &takes_author)
///     .is_none());
/// let no_author = Description::new("append").verb(Verb::Sink);
/// assert!(admit::fill_author(Door::Quic, &append(&[("principal", &laptop)]), &no_author)
///     .is_none());
/// ```
pub fn fill_author(door: Door, request: &Request, description: &Description) -> Option<Request> {
    if door != Door::Quic || !request.verb.is_mutating() || request.args.contains_key("author") {
        return None;
    }
    let principal = principal(door, request)?;
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

/// The endpoint [`Admitting`] resolves to: the real one, behind the door's checks — all of
/// them at a network door, only [`raw_write_refusal`] at the socket.
struct Admitted {
    inner: Arc<dyn Endpoint>,
    door: Door,
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
        if !inv.request.verb.is_mutating() {
            return self.inner.invoke(inv).await;
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
            if let Some(why) = author_refusal(self.door, inv.request) {
                return Err(Error::Denied(why));
            }
            // Attributed by the door: the same request with `author` filled in, issued as
            // this request's own sub-request, so it resolves to this same endpoint one level
            // down (where only the raw-write rule runs again, and passes as it did here) under
            // the same capability. The access log is outside and counts it once, as the request
            // that arrived.
            if let Some(filled) = fill_author(self.door, inv.request, &description) {
                return inv.issue(filled).await;
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
