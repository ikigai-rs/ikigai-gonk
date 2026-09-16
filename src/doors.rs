//! The three doors onto the one kernel, and what each hands a caller.
//!
//! | door | transport | who can reach it | capability |
//! |---|---|---|---|
//! | HTTP | `ikigai-web`, loopback TCP | any local process | [`http_cap`]: the configured ledgers' narrow read+write tokens for an anonymous loopback caller, plus the grant of a signed-in passkey; nothing for a non-loopback peer, a foreign `Host`, or a cross-site write |
//! | socket | `ikigai-ipc`, `0600` Unix socket, peer UID checked | this user only | root — the owner, who can read the dataset's files anyway |
//! | QUIC | `ikigai-quic`, mutual TLS | a certificate this server trusts | the grant that certificate's fingerprint maps to in `clients.json`; refused when it maps to none |
//!
//! # ★ One cache for the whole process
//!
//! `ikigai_ipc::serve` and `ikigai_quic::serve_with` each take a [`Kernel`] **by value**, and
//! a kernel owns its cache and its golden threads. Three kernels over one store would be
//! three caches, and a write through one door cuts threads in that kernel only — so the
//! other two would go on serving the read they cached before it, indefinitely, with nothing
//! wrong in any log.
//!
//! So there is one kernel, the **hub** ([`crate::compose`]), shared as `Arc<Kernel>`. Every
//! door gets a kernel whose space FORWARDS to the hub ([`HubSpace`]) under a cache policy that
//! admits nothing ([`NoCache`]). Every read and every write passes through the hub's cache
//! and the hub's threads, whichever door it came in by.
//!
//! The HTTP door's kernel ([`http_kernel`]) is the same shape with two more spaces around the
//! hub: gonk's own pages in front ([`crate::web`]), and a catch-all behind ([`NotFound`]). Its
//! pages never cache — they are cheap to render and a cached page is exactly the second
//! cache this design exists to avoid — and every ledger read a page makes is a hub read.
//!
//! ⚠ **Why not `ikigai_resolve::RemoteSpace` over the hub**, which is the mount machinery
//! and would have been the obvious reuse: its forwarding endpoint calls the synchronous
//! `Resolver::issue_as`, and `impl Resolver for Kernel` implements that with
//! `futures::executor::block_on`. The socket door already runs each call inside
//! `ikigai_resolve::issue_traced_as`'s own `block_on`, and futures' executor panics when
//! entered twice on one thread. [`HubSpace`] awaits the hub instead.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    Bindings, CachePolicy, Capability, Description, Endpoint, EndpointSpace, EntryFacts, Error,
    Fallback, Invocation, Kernel, Representation, Request, Resolution, Resolved, Result, Scope,
    Space, SpaceEntry, SystemClock, Verb,
};
use ikigai_vocab::TurtleRenderer;
use ikigai_web::{CapFn, EdgeConfig, HttpRequest, Route, RouteTable};

use crate::identity::{self, Passkeys};

/// A space whose every binding is the hub's: it resolves what the hub binds, describes it
/// with the hub's own description, enumerates the hub's catalog, and forwards invocation to
/// the hub under the caller's capability.
pub struct HubSpace {
    hub: Arc<Kernel>,
}

impl HubSpace {
    /// Forward to `hub`.
    pub fn new(hub: Arc<Kernel>) -> Self {
        HubSpace { hub }
    }
}

impl Space for HubSpace {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        // A miss when the hub binds nothing here, so the door reports the kernel's own
        // "no endpoint" rather than a forwarding endpoint that fails on invoke.
        match self.hub.describe(&request.target) {
            Some(description) => Resolution::Hit(Resolved {
                endpoint: Arc::new(Forward {
                    hub: Arc::clone(&self.hub),
                    description,
                }),
                bindings: Bindings::new(),
                // Nothing is rewritten here: the hub canonicalizes, caches and cuts under
                // its own names.
                canonical: None,
            }),
            None => Resolution::Miss,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // The hub's catalog, minus the hub's own `urn:kernel:*` operations: the door kernel
        // answers those itself, and listing both would name every one twice.
        self.hub.entries().map(|entries| {
            entries
                .into_iter()
                .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
                .collect()
        })
    }
}

/// The endpoint a [`HubSpace`] resolves to.
struct Forward {
    hub: Arc<Kernel>,
    description: Description,
}

#[async_trait]
impl Endpoint for Forward {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // The CALLER's capability, unchanged: the door kernel has already checked the
        // declared floor against the same description, and the hub checks it again along
        // with everything the endpoint enforces inside.
        self.hub.issue(inv.request.clone(), inv.capability).await
    }

    fn name(&self) -> &str {
        &self.description.id
    }

    fn describe(&self) -> Description {
        self.description.clone()
    }
}

/// A cache policy that stores nothing — for the door kernels, whose caches could never be
/// invalidated (the hub is where writes cut threads).
pub struct NoCache;

impl CachePolicy for NoCache {
    fn admit(&self, _candidate: &EntryFacts<'_>) -> bool {
        false
    }

    fn victim(&self, _resident: &[EntryFacts<'_>]) -> Option<usize> {
        None
    }
}

/// A kernel for a transport that takes one by value: a [`HubSpace`] over `hub`, a Meta
/// renderer (a mounting client reads contracts through this kernel's JSON Meta face), and
/// [`NoCache`].
pub fn door_kernel(hub: Arc<Kernel>) -> Kernel {
    Kernel::with_meta_renderer(Arc::new(HubSpace::new(hub)), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
        .with_cache_policy(Arc::new(NoCache))
}

/// The HTTP door's kernel: gonk's pages ([`crate::web::space`]) in front of the hub, and
/// [`NotFound`] behind it — still storing nothing, still one cache in the process.
pub fn http_kernel(hub: Arc<Kernel>, pages: EndpointSpace) -> Kernel {
    let space = Fallback::new(vec![
        Arc::new(pages) as Arc<dyn Space>,
        Arc::new(HubSpace::new(hub)) as Arc<dyn Space>,
        Arc::new(NotFound) as Arc<dyn Space>,
    ]);
    Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
        .with_cache_policy(Arc::new(NoCache))
}

/// The HTTP door's last space: anything nothing else binds is a typed `NotFound`.
///
/// ⚠ **This exists to route around `ikigai-web`'s status mapping**, and it is worth saying
/// where the defect is. A path that resolves to no endpoint surfaces from the kernel as
/// `Error::Unresolved`, and `ikigai-web` maps every error it does not name to `500` — so
/// `GET /favicon.ico` (or a typo) answered `500 no endpoint resolved for urn:favicon.ico`.
/// Here it resolves, to an endpoint that answers `NotFound`, which the library maps to `404`.
/// The body is still `text/plain`: that library writes every error response as plain text,
/// so a 404 cannot be an HTML page from this side of it.
///
/// It enumerates nothing, so it adds no entry to the catalog and cannot be walked or
/// offered as an action.
pub struct NotFound;

impl Space for NotFound {
    fn resolve(&self, _request: &Request, _scope: &Scope) -> Resolution {
        Resolution::Hit(Resolved {
            endpoint: Arc::new(NotFoundEndpoint),
            bindings: Bindings::new(),
            canonical: None,
        })
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Some(Vec::new())
    }
}

struct NotFoundEndpoint;

#[async_trait]
impl Endpoint for NotFoundEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        Err(Error::NotFound(format!(
            "nothing is served at `{}`. The ledger's pages start at / , the SPARQL page is \
             /sparql, and the ledger's own resources are under /iki/ledger/ (for example \
             /iki/ledger/items).",
            inv.request.target
        )))
    }

    fn name(&self) -> &str {
        "gonk-not-found"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-not-found")
            .title("Nothing here")
            .summary("Answers NotFound for every name nothing else binds.")
            .verb(Verb::Source)
            .verb(Verb::Sink)
            .verb(Verb::Delete)
            .verb(Verb::Exists)
    }
}

/// Whether `ip` is loopback, counting an IPv4-mapped IPv6 loopback (`::ffff:127.0.0.1`) —
/// the form a dual-stack listener reports an IPv4 client in.
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// What the HTTP door's capability is computed from.
#[derive(Clone)]
pub struct HttpDoor {
    /// What an ANONYMOUS loopback caller holds — the first arc's grant, unchanged: read and
    /// write on the ledgers in `gonk.http.ledger`.
    pub anonymous: Vec<String>,
    /// The port the door is bound to, which the `Host` and `Origin` checks require.
    pub port: u16,
    /// The passkey sessions, when passkeys are on.
    pub passkeys: Option<Arc<Passkeys>>,
}

/// The HTTP door's capability: a function of the REQUEST, not a constant.
///
/// ```text
/// Host not this server's loopback name?        → nothing  (DNS rebinding)
/// a write from another origin or site?         → nothing  (cross-site request forgery)
/// otherwise  (loopback peer ? anonymous : ∅)  ∪  (a live passkey session ? its grant : ∅)
/// ```
///
/// ★ **An anonymous caller is strictly weaker than any identity**, because an identity's
/// capability is the anonymous one PLUS its grant, and `ikigai-gonk passkey invite` refuses a
/// grant that adds nothing. And the identity half reads the SAME `grants.json` the QUIC door
/// reads, through the same fail-closed [`crate::quic::scopes_for_grant`].
///
/// ⚠ **The two browser checks are new with the HTML face, and they close a hole the first arc
/// had.** A door that grants writes to whatever reaches loopback also grants them to any web
/// page open in a browser on the machine: a page on any site can send a form-encoded `POST`
/// to `http://127.0.0.1:1060/iki/ledger/append` without a preflight. A browser always labels
/// such a write with `Origin` (and `Sec-Fetch-Site`), which a local non-browser process —
/// `curl`, a script — does not send, so refusing a foreign one costs those callers nothing.
/// And a rebinding attack reaches loopback under a foreign `Host`, which is refused on every
/// method, reads included.
pub fn http_cap(door: HttpDoor) -> CapFn {
    Arc::new(move |request: &HttpRequest| {
        Capability::scoped(http_scopes(&door, request, identity::now_seconds()))
    })
}

/// [`http_cap`]'s scope list, with the clock as an argument.
pub fn http_scopes(door: &HttpDoor, request: &HttpRequest, now: u64) -> Vec<String> {
    if !host_is_ours(request.header("host"), door.port) {
        return Vec::new();
    }
    let safe = matches!(request.method.as_str(), "GET" | "HEAD" | "OPTIONS");
    if !safe && !same_origin(request, door.port) {
        return Vec::new();
    }
    let mut scopes: Vec<String> = match request.peer {
        Some(peer) if is_loopback(peer) => door.anonymous.clone(),
        _ => Vec::new(),
    };
    if let (Some(passkeys), Some(token)) = (&door.passkeys, session_token(request)) {
        if let Some(identity) = passkeys.identity(&token, now) {
            for scope in identity.scopes {
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
        }
    }
    scopes
}

/// The loopback names this door answers to, with or without the port.
fn host_is_ours(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    ["localhost", "127.0.0.1", "[::1]"]
        .iter()
        .any(|name| host == *name || host == format!("{name}:{port}"))
}

/// A browser's `Origin` and `Sec-Fetch-Site`, when present, name this server.
fn same_origin(request: &HttpRequest, port: u16) -> bool {
    if let Some(site) = request.header("sec-fetch-site") {
        if !matches!(site.trim(), "same-origin" | "none") {
            return false;
        }
    }
    match request.header("origin") {
        None => true,
        Some(origin) => ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .any(|name| origin.trim() == format!("http://{name}:{port}")),
    }
}

/// The session token in the request's `Cookie` header.
fn session_token(request: &HttpRequest) -> Option<String> {
    request.header("cookie")?.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == identity::SESSION_COOKIE && !value.is_empty()).then(|| value.to_string())
    })
}

/// The HTTP door's edge policy: `ikigai-web`'s strict defaults, and an explicit route table.
///
/// The page routes are rows here, each onto one of gonk's own resources ([`crate::web`]);
/// every other path takes the mechanical mapping (`POST /iki/ledger/append` → `Sink
/// urn:iki:ledger:append`), exactly as before. The default CSP — `default-src 'self'`, no
/// framing, forms to self — needs no loosening: there is no inline script or style anywhere
/// in the face, and htmx is served from this origin.
pub fn edge_config() -> EdgeConfig {
    let route = |pattern: &str, iri_template: &str| Route {
        pattern: pattern.to_string(),
        iri_template: iri_template.to_string(),
        cap: None,
        cors: None,
        csp: None,
    };
    EdgeConfig {
        routes: RouteTable::new(vec![
            route("/", "urn:iki:gonk:page:home"),
            route("/l/{ledger}", "urn:iki:gonk:page:ledger:{ledger}"),
            route("/l/{ledger}/items", "urn:iki:gonk:fragment:items:{ledger}"),
            route(
                "/l/{ledger}/item/{id}",
                "urn:iki:gonk:page:item:{ledger}:{id}",
            ),
            route(
                "/l/{ledger}/item/{id}/card",
                "urn:iki:gonk:fragment:item:{ledger}:{id}",
            ),
            route("/act", "urn:iki:gonk:act"),
            route("/sparql", "urn:iki:gonk:sparql"),
            route("/sparql/results", "urn:iki:gonk:fragment:sparql"),
            route("/render-rules", "urn:iki:gonk:render-rules"),
            route("/auth/{op}", "urn:iki:gonk:passkey:{op}"),
            route("/static/{name}", "urn:iki:gonk:asset:{name}"),
        ]),
        routes_only: false,
        ..EdgeConfig::default()
    }
}
