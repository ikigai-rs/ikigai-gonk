//! The one namespace a `gonk.mount` line mounts: `urn:llm:` on a peer that serves it.
//!
//! (A task layer's names are mounted too, by a `gonk.layer` binding and by nothing else; see
//! [`crate::layer`]. They reuse [`LazyMount`], in its [`LazyMount::fresh`] form.)
//!
//! ```toml
//! gonk.mount = "prefer urn:llm:=quic://127.0.0.1:4433 ~/.config/ikigai/gonk/quic/peers/plasma"
//! gonk.mount = "prefer urn:llm:=~/.ikigai/host.sock"
//! ```
//!
//! # Why a mount and not a dependency
//!
//! `urn:repo:{root}:explain` derives through `urn:llm:{provider}:ask`. gonk could have
//! linked `ikigai-llm` and spoken to Ollama itself — and then this binary would contain an
//! outbound HTTP client, reachable by whatever holds a net grant, on a server whose whole
//! argument is that what is not compiled in cannot be reached. A mount moves the model to a
//! peer: gonk dials ONE address it was configured with, presenting ONE certificate, and the
//! peer's own capability ceiling decides what that connection may do. The surface added is a
//! named peer, not a protocol.
//!
//! # `prefer`, and what it can mean here
//!
//! The cli's `prefer` is an override wrapped in a failover over the LOCAL spaces: the peer
//! when it answers, this machine when it does not. **gonk binds nothing under a mounted
//! prefix**, so there is no local half to fall back to and `prefer` and `override` would
//! resolve identically. What the word buys here is the other half of its meaning, which is
//! the half that matters for a ledger server: **an absent peer costs explain and nothing
//! else.** Concretely — [`LazyMount`] never dials at startup, a failed dial is remembered
//! for a backoff that starts at [`FIRST_HOLD`] and doubles to [`RETRY`] rather than retried per
//! request, and a `urn:iki:ledger:*` read does not touch this module at all. The mode word is still required in the line, and still spelled
//! `prefer`, so the line reads as the same setting the cli reads.
//!
//! # ★ The mounted call runs on its own thread, and that is not tidiness
//!
//! `ikigai_quic::connect` returns a resolver holding **its own tokio runtime**, and every
//! call does `Runtime::block_on`. Both of this server's wire doors dispatch from inside a
//! tokio runtime (`ikigai_quic::serve` spawns each connection as a task; the HTTP door runs
//! on the `serve_with_listener` runtime), and tokio panics — "Cannot start a runtime from
//! within a runtime" — when `block_on` is entered from a runtime thread. So a mounted
//! resolve from either door would not fail, it would PANIC the worker.
//!
//! `off_thread` is the fix: every call into the mounted resolver runs on a thread of its
//! own, and the caller blocks on a channel. The cost is one thread per mounted resolve,
//! which is nothing against a wire round trip, let alone a model call. A panic inside comes
//! back as a typed error rather than unwinding the door.
//!
//! ⚠ **The DIAL is one of those calls, and it was the one that got missed.** `connect`
//! builds the runtime and then `block_on`s the handshake on it, so it panics on a runtime
//! thread exactly as a call does — measured 2026-09-16, from gonk's own QUIC door: the first
//! mounted resolve killed the worker, and the second died on the lock the first had
//! poisoned. Hence `off_thread` around `dial`, and a lock that is taken with
//! `into_inner()` on poison: a panic anywhere in here must cost one call, never wedge the
//! mount for the life of the process.
//!
//! # ⚠ Threads do not cross a mount (#92)
//!
//! A mounted result arrives `cacheable()` with an EMPTY dependency set: the peer's golden
//! threads are the peer's, and nothing here can cut them. For `urn:llm:*ask` that is
//! harmless, and it is harmless for a specific reason rather than by luck — **the archive,
//! not the cache, is what makes an explanation cheap the second time.** `ikigai-browse` keys
//! its archive on `(path, content-hash, version-tag)` in the store, and re-derives when the
//! content changes; a kernel cache entry keyed on the ask's prompt would be a second,
//! weaker copy of that. The next thing anyone mounts here will not be harmless: mount a
//! namespace whose answers change on the peer's own schedule and this kernel will serve the
//! first answer it got until the process restarts.

use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, Error, Invocation, Iri, Representation, Request,
    Resolution, Scope, Space, SpaceEntry, Topology, Verb,
};
use ikigai_resolve::{CacheStatus, MountedRemote, Resolver};

/// The only prefix this server mounts.
///
/// A mount is composed IN FRONT of the local spaces and its catalog is surfaced through
/// every door, so a general `gonk.mount` key would let one config line put an arbitrary
/// remote namespace behind the socket, the QUIC door and the HTTP door at once — and the
/// manifest's claim that what is not linked is not reachable would become a claim about a
/// file nobody reads. One prefix, for the one thing this server has a use for.
pub const LLM_PREFIX: &str = "urn:llm:";

/// The LONGEST a failed dial is remembered before another is attempted: the cap of the
/// backoff [`FIRST_HOLD`] starts ([`hold_after`]).
///
/// The point is the catalog path: `MountedRemote::entries` asks the peer, and the catalog is
/// read by every `urn:kernel:catalog`, every manifold query and every door's enumeration. A
/// peer that is down would otherwise cost a connect attempt — five seconds, on QUIC — per
/// enumeration, which is how "explain is unavailable" turns into "the ledger pages hang".
pub const RETRY: Duration = Duration::from_secs(30);

/// How long the FIRST failed dial in a row is remembered; each further failure doubles it, up
/// to [`RETRY`].
///
/// ★ Ledger [#1300](http://localhost:1060/l/default/item/1300): through gonk 0.x every failure
/// held the mount off for the whole of [`RETRY`], including the one that follows a single lost
/// call on a healthy peer, so one hiccup made explain unavailable for half a minute. A peer that
/// is really down still settles at one dial per [`RETRY`] after five failures (1, 2, 4, 8, 16,
/// then 30 s): bounded, never a hot loop, and never a dial per request.
pub const FIRST_HOLD: Duration = Duration::from_secs(1);

/// The hold after `failures` failed dials in a row: [`FIRST_HOLD`] doubled per failure after
/// the first, capped at [`RETRY`]. No failures, no hold.
///
/// ```
/// use std::time::Duration;
/// use ikigai_gonk::mount::{hold_after, RETRY};
/// assert_eq!(hold_after(0), Duration::ZERO);
/// assert_eq!(hold_after(1), Duration::from_secs(1));
/// assert_eq!(hold_after(3), Duration::from_secs(4));
/// assert_eq!(hold_after(6), RETRY);
/// assert_eq!(hold_after(u32::MAX), RETRY);
/// ```
pub fn hold_after(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    // 2^(failures - 1), saturating well before the cap could overflow anything.
    let doublings = (failures - 1).min(16);
    FIRST_HOLD.saturating_mul(1 << doublings).min(RETRY)
}

/// The deadline a mounted peer gets to DESCRIBE ITSELF (an enumeration, a `Meta`): two thirds
/// of the deadline gonk's own callers give gonk, which is the transports' default (30 s on both
/// `ikigai-ipc` and `ikigai-quic`), so 20 s.
///
/// ★ Ledger [#1288](http://localhost:1060/l/default/item/1288), cli PR 461's rule, by ROLE: a
/// process that serves a door gives its mounts LESS than its callers give it, so when a peer
/// hangs, gonk answers first with the peer's name and a third of the caller's bound to spare.
/// At equal deadlines (gonk 0.x took `ikigai_quic::connect`'s default, 30 s) the two raced and
/// the caller's usually won, blaming gonk, which was healthy. ⚠ Per hop, as the bound always
/// was; the real fix is a budget carried on the wire.
///
/// ```
/// use ikigai_gonk::mount::DESCRIBE_TIMEOUT;
/// assert_eq!(DESCRIBE_TIMEOUT, std::time::Duration::from_secs(20));
/// assert!(DESCRIBE_TIMEOUT < ikigai_ipc::DEFAULT_DESCRIBE_TIMEOUT);
/// assert!(DESCRIBE_TIMEOUT < ikigai_quic::DEFAULT_DESCRIBE_TIMEOUT);
/// ```
pub const DESCRIBE_TIMEOUT: Duration =
    Duration::from_millis(ikigai_quic::DEFAULT_DESCRIBE_TIMEOUT.as_millis() as u64 * 2 / 3);

/// The deadline a mounted call that is NOT a self-description runs under, from its first byte
/// to its last, unless the mount sets its own ([`LazyMount::with_call_timeout`]): the
/// transports' own standing bound, 300 s on both (`ikigai_quic::DEFAULT_IDLE_TIMEOUT`,
/// `ikigai_ipc::DEFAULT_TIMEOUT`).
///
/// ★ Ledger [#1335](http://localhost:1060/l/default/item/1335): through cli 0.1.52 a mounted call
/// had no deadline of its own, only the transport's silence bound, so a peer that accepted a
/// call and never answered it held the caller for twice the idle timeout (a read is replayed
/// once on a fresh connection), and for ever if it kept the connection talking. The standing
/// bound is the default ON PURPOSE for `urn:llm:`: a model call is long, and against an honest
/// peer the silence before its reply already was the whole call, so no call that used to finish
/// is cut. A task layer's door sets a short one ([`crate::layer::CALL_TIMEOUT`]).
///
/// ```
/// use ikigai_gonk::mount::CALL_TIMEOUT;
/// assert_eq!(CALL_TIMEOUT, ikigai_quic::DEFAULT_IDLE_TIMEOUT);
/// assert_eq!(CALL_TIMEOUT, ikigai_ipc::DEFAULT_TIMEOUT);
/// ```
pub const CALL_TIMEOUT: Duration = ikigai_quic::DEFAULT_IDLE_TIMEOUT;

/// The most a request's own `wait=<seconds>` (a long poll: a layer's feed waits up to a minute
/// for an event) adds to the call deadline ([`deadline_for`]). A caller's `wait` is the caller's
/// to choose, so without a ceiling it would choose the deadline too.
pub const MAX_WAIT: Duration = Duration::from_secs(120);

/// The deadline `request` runs under on a mount whose call deadline is `call`: `call`, plus the
/// request's own `wait=<seconds>` (at most [`MAX_WAIT`]) when it asks the peer to WAIT. A long
/// poll is not a peer that went silent, so a short call deadline must not cut it; a peer that
/// says nothing for the wait AND the deadline is.
///
/// ```
/// use std::time::Duration;
/// use ikigai_core::{ArgRef, Iri, Request, Verb};
/// use ikigai_gonk::mount::{deadline_for, MAX_WAIT};
/// let call = Duration::from_secs(15);
/// let feed = |wait: &str| {
///     Request::new(Verb::Source, Iri::parse("urn:layer:tasks:feed").unwrap())
///         .with_arg("wait", ArgRef::Inline(wait.as_bytes().to_vec()))
/// };
/// assert_eq!(deadline_for(&feed("50"), call), Duration::from_secs(65));
/// assert_eq!(deadline_for(&feed("100000"), call), call + MAX_WAIT);
/// assert_eq!(deadline_for(&feed("soon"), call), call);
/// let rd = Request::new(Verb::Source, Iri::parse("urn:layer:tasks:tuples").unwrap());
/// assert_eq!(deadline_for(&rd, call), call);
/// ```
pub fn deadline_for(request: &Request, call: Duration) -> Duration {
    let wait = match request.args.get("wait") {
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
            .map(Duration::from_secs),
        _ => None,
    };
    call + wait.unwrap_or(Duration::ZERO).min(MAX_WAIT)
}

/// Where a mounted peer lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `quic://host:port` — an authenticated peer, possibly on another machine. The
    /// directory holds `client.crt`, `client.key` and the `server.crt` to pin, laid out as
    /// `ikigai --cert-dir` expects (which is what `ikigai-gonk client add` writes, from the
    /// other side of the same relationship).
    Quic {
        /// The authority as written, resolved at DIAL time — a name whose DNS (or mDNS)
        /// is not up when gonk starts must not stop gonk from starting.
        authority: String,
        /// The client identity and the pinned server certificate.
        certs: PathBuf,
    },
    /// A Unix socket — an ikigai host on THIS machine, owner-only by file permission.
    Socket(PathBuf),
}

impl Target {
    /// The host a `urn:cap:net:` grant names to reach this peer — the authority's HOST for a
    /// `quic://` target, and `localhost` for a socket.
    ///
    /// ⚠ **The authority's host, not the whole authority and not `localhost`.**
    /// `Capability::allows` is exact string containment, so `urn:cap:net:localhost` and
    /// `urn:cap:net:127.0.0.1` are different hosts as far as a capability is concerned,
    /// however the same they are to a resolver. A socket reaches a peer on this machine and
    /// has no authority to take a host from, so it takes the name a person would write.
    ///
    /// ```
    /// use ikigai_gonk::mount;
    /// let home = std::path::Path::new("/home/nobody");
    /// let socket = mount::parse("prefer urn:llm:=/tmp/llm.sock", home).unwrap();
    /// assert_eq!(socket.target.net_host(), "localhost");
    /// ```
    pub fn net_host(&self) -> String {
        match self {
            Target::Quic { authority, .. } => authority
                .rsplit_once(':')
                .map(|(host, _)| host.to_string())
                .unwrap_or_else(|| authority.clone()),
            Target::Socket(_) => "localhost".to_string(),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Quic { authority, .. } => write!(f, "quic://{authority}"),
            Target::Socket(path) => write!(f, "{}", path.display()),
        }
    }
}

/// One `gonk.mount` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The IRI prefix — always [`LLM_PREFIX`] today.
    pub prefix: String,
    /// Where it resolves.
    pub target: Target,
}

/// Parse one `gonk.mount` line: `prefer <prefix>=<target> [cert-dir]`, the cli's spelling.
///
/// `home` is `$HOME`, for `~/` in the target and the certificate directory.
///
/// # Errors
///
/// When the mode is not `prefer`, the prefix is not [`LLM_PREFIX`], a `quic://` target
/// carries no certificate directory (or one that is not a directory), or a socket target
/// carries one.
pub fn parse(line: &str, home: &Path) -> Result<Mount, String> {
    let mut parts = line.split_whitespace();
    let mode = parts
        .next()
        .ok_or_else(|| format!("gonk.mount `{line}`: expected `prefer <prefix>=<target>`"))?;
    if mode != "prefer" {
        return Err(format!(
            "gonk.mount `{line}`: the mode must be `prefer` (not `{mode}`). gonk binds \
             nothing under a mounted prefix, so an override would differ only in failing \
             hard when the peer is down — which would make a ledger read depend on a model"
        ));
    }
    let spec = parts
        .next()
        .ok_or_else(|| format!("gonk.mount `{line}`: expected <prefix>=<target>"))?;
    let (prefix, target) = spec
        .split_once('=')
        .ok_or_else(|| format!("gonk.mount `{line}`: expected <prefix>=<target>, got `{spec}`"))?;
    if prefix != LLM_PREFIX {
        return Err(format!(
            "gonk.mount `{line}`: this server mounts `{LLM_PREFIX}` only (not `{prefix}`). A \
             mount is composed in front of the local spaces and its catalog is served \
             through every door, so the namespaces this binary can serve stay a property of \
             its manifest"
        ));
    }
    let certs = parts.next().map(|dir| expand_home(dir, home));
    if let Some(extra) = parts.next() {
        return Err(format!(
            "gonk.mount `{line}`: unexpected `{}`",
            extra.trim_matches('"')
        ));
    }
    let target = match target.strip_prefix("quic://") {
        Some(authority) => {
            let certs = certs.ok_or_else(|| {
                format!(
                    "gonk.mount `{line}`: a quic:// target needs a certificate directory as \
                     the third word — the client.crt/client.key this gonk presents and the \
                     server.crt it pins, laid out as `ikigai --cert-dir` expects"
                )
            })?;
            if !certs.is_dir() {
                return Err(format!(
                    "gonk.mount `{line}`: {} is not a directory",
                    certs.display()
                ));
            }
            if authority.is_empty() {
                return Err(format!("gonk.mount `{line}`: quic:// needs a host:port"));
            }
            Target::Quic {
                authority: authority.to_string(),
                certs,
            }
        }
        None => {
            if certs.is_some() {
                return Err(format!(
                    "gonk.mount `{line}`: a socket target takes no certificate directory — a \
                     Unix socket is authenticated by its file permissions"
                ));
            }
            Target::Socket(expand_home(target, home))
        }
    };
    Ok(Mount {
        prefix: prefix.to_string(),
        target,
    })
}

/// `~/x` → `$HOME/x`. A config file is hand-written, and `~` is what a person types.
pub(crate) fn expand_home(spelled: &str, home: &Path) -> PathBuf {
    match spelled.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(spelled),
    }
}

/// The space to compose in FRONT of the local ones: an override-mode [`MountedRemote`] over
/// a [`LazyMount`].
///
/// Override mode forwards the IRI unchanged — the peer serves `urn:llm:*` under that very
/// name — and `MountedRemote` misses outside its prefix, so the mount claims exactly what it
/// was given and nothing else. (The cli wraps its `prefer` in a `PrefixGuard` because its
/// failover pair would otherwise answer for every local IRI; there is no pair here.)
///
/// ★ **Behind [`Declaring`], so the manifold offers what the mount serves** (ledger
/// [#1204](http://localhost:1062/l/default/item/1204)). The mount enforces `urn:cap:net:{host}`
/// on every call it forwards ([`net_grant`]), but each mounted resource describes itself with
/// the PEER's declaration: `urn:llm:config` declares nothing, so an anonymous caller was offered
/// it and then denied, and `urn:llm:ask` declares the offering wildcard `urn:cap:net:*`, which a
/// grant naming another host satisfies. Every resource under the prefix now also declares the
/// exact token the mount checks.
pub fn space(mount: &Mount) -> Arc<dyn Space> {
    let origin = mount.target.to_string();
    let remote = Arc::new(MountedRemote::overriding(
        Arc::new(LazyMount::new(mount.target.clone())) as Arc<dyn Resolver>,
        mount.prefix.clone(),
        origin,
    ));
    Arc::new(Declaring {
        inner: remote,
        prefix: mount.prefix.clone(),
        scope: format!("urn:cap:net:{}", mount.target.net_host()),
    })
}

/// An overlay with no identity or structure of its own (it forwards [`Space::id`],
/// [`Space::topology`] and [`Space::entries`]) that adds the mount's net requirement to the
/// description of every resource it resolves under the mount's prefix ([`space`]).
///
/// The mount's own status resource lies OUTSIDE the prefix and is answered here, never by the
/// peer, so it reaches no host and declares nothing new.
struct Declaring {
    inner: Arc<dyn Space>,
    prefix: String,
    /// `urn:cap:net:{host}`, exactly what [`net_grant`] checks.
    scope: String,
}

impl Space for Declaring {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.inner.resolve(request, scope);
        if !request.target.as_str().starts_with(&self.prefix) {
            return resolution;
        }
        // `map_endpoint` carries the canonical name and the answering space through, and
        // leaves a limiter alone.
        resolution.map_endpoint(|endpoint| {
            Arc::new(NetDeclared {
                inner: endpoint,
                scope: self.scope.clone(),
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

/// A mounted endpoint, unchanged but for its description: every action also requires the
/// mount's net token ([`crate::nl::declare`]).
struct NetDeclared {
    inner: Arc<dyn Endpoint>,
    scope: String,
}

#[async_trait]
impl Endpoint for NetDeclared {
    async fn invoke(&self, inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        self.inner.invoke(inv).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        crate::nl::declare(self.inner.describe(), &self.scope)
    }

    fn confinement(&self) -> Option<Topology> {
        self.inner.confinement()
    }
}

/// A resolver that dials on first use, remembers a failure for a bounded backoff
/// ([`hold_after`]), and runs every call `off_thread`.
pub struct LazyMount {
    target: Target,
    state: Mutex<State>,
    /// Whether every answer is handed back UNCACHEABLE ([`LazyMount::fresh`]).
    fresh: bool,
    /// The peer's self-description deadline ([`DESCRIBE_TIMEOUT`] unless a test sets it).
    describe_timeout: Duration,
    /// Every other call's deadline ([`CALL_TIMEOUT`] unless the mount sets its own).
    call_timeout: Duration,
}

/// A connected peer, by transport: kept concrete, not as `dyn Resolver`, because a call that asks
/// the peer to wait runs under a deadline of its own (`issue_as_within`), which the trait has
/// no word for.
enum Peer {
    Socket(ikigai_ipc::IpcResolver),
    Quic(ikigai_quic::QuicResolver),
}

impl Peer {
    fn resolver(&self) -> &dyn Resolver {
        match self {
            Peer::Socket(resolver) => resolver,
            Peer::Quic(resolver) => resolver,
        }
    }

    /// `request` under `deadline` when one is given, else under the standing deadlines the
    /// dial set (the call deadline, or the describe deadline for a `Meta`).
    fn issue_as(
        &self,
        request: Request,
        capability: &Capability,
        deadline: Option<Duration>,
    ) -> Result<(Representation, CacheStatus), Error> {
        match (self, deadline) {
            (Peer::Socket(resolver), Some(d)) => resolver.issue_as_within(request, capability, d),
            (Peer::Quic(resolver), Some(d)) => resolver.issue_as_within(request, capability, d),
            (peer, None) => peer.resolver().issue_as(request, capability),
        }
    }
}

#[derive(Default)]
struct State {
    connected: Option<Arc<Peer>>,
    /// When the next dial may be attempted, after a failure.
    hold_until: Option<Instant>,
    /// Failed dials in a row, for [`hold_after`]; a dial that connects resets it.
    failures: u32,
}

impl LazyMount {
    /// A mount on `target`, not yet dialled.
    pub fn new(target: Target) -> LazyMount {
        LazyMount {
            target,
            state: Mutex::new(State::default()),
            fresh: false,
            describe_timeout: DESCRIBE_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
        }
    }

    /// Set the deadline every call that is not a self-description runs under (builder):
    /// [`CALL_TIMEOUT`] by default. Takes effect at the next dial, so set it before the first
    /// use. A request that asks the peer to wait gets its wait on top ([`deadline_for`]).
    pub fn with_call_timeout(mut self, deadline: Duration) -> LazyMount {
        self.call_timeout = deadline;
        self
    }

    /// The call deadline ([`LazyMount::with_call_timeout`]).
    pub fn call_timeout(&self) -> Duration {
        self.call_timeout
    }

    /// Set the peer's self-description deadline (builder): [`DESCRIBE_TIMEOUT`] by default.
    /// Takes effect at the next dial, so set it before the first use.
    pub fn with_describe_timeout(mut self, deadline: Duration) -> LazyMount {
        self.describe_timeout = deadline;
        self
    }

    /// The peer's self-description deadline ([`LazyMount::with_describe_timeout`]).
    pub fn describe_timeout(&self) -> Duration {
        self.describe_timeout
    }

    /// A mount on `target` whose every answer is marked UNCACHEABLE (`Expiry::Always`) on
    /// the way back — for a namespace whose state changes on the PEER's schedule.
    ///
    /// ★ The module docs' warning made concrete (ledger #1190): threads do not cross a mount,
    /// so a cacheable remote answer arrives here with no dependency anyone can cut, and the
    /// hub would serve it until the process restarts. A layer's `rd` is cacheable on its host
    /// (under that host's tuples thread); through a plain mount, a `rd` read here would show
    /// a tuple as pending long after it was taken. A task layer is read for its CURRENT
    /// state, so every answer is fetched, never remembered.
    pub fn fresh(target: Target) -> LazyMount {
        LazyMount {
            fresh: true,
            ..LazyMount::new(target)
        }
    }

    /// The connected resolver, dialling if this is the first use or the hold has expired.
    ///
    /// `Err` is [`Error::Unavailable`] — the transient the mount means it to be: a caller
    /// (or a reliability overlay above it) can tell "the peer is down" from "the peer said
    /// no", which a blanket endpoint error could not.
    fn resolver(&self) -> Result<Arc<Peer>, Error> {
        let mut state = self.state();
        if let Some(resolver) = &state.connected {
            return Ok(Arc::clone(resolver));
        }
        if let Some(until) = state.hold_until {
            if Instant::now() < until {
                return Err(Error::Unavailable(format!(
                    "the mounted peer at {} is not reachable (the last {} dial(s) failed; the \
                     next is held off for {:?} more)",
                    self.target,
                    state.failures,
                    until.saturating_duration_since(Instant::now())
                )));
            }
        }
        // The lock is deliberately held across the dial: a burst of first calls should cost
        // one handshake, not one each.
        let (target, describe, call) = (
            self.target.clone(),
            self.describe_timeout,
            self.call_timeout,
        );
        let dialled = off_thread(move || dial(&target, describe, call))
            .unwrap_or_else(|| Err("the dial panicked".to_string()));
        match dialled {
            Ok(resolver) => {
                state.hold_until = None;
                state.failures = 0;
                state.connected = Some(Arc::clone(&resolver));
                Ok(resolver)
            }
            Err(e) => {
                state.failures = state.failures.saturating_add(1);
                state.hold_until = Some(Instant::now() + hold_after(state.failures));
                // Once per hold window, not once per request: a down peer must not be able
                // to fill a launchd log.
                eprintln!("ikigai-gonk: mount {}: {e}", self.target);
                Err(Error::Unavailable(format!(
                    "the mounted peer at {}: {e}",
                    self.target
                )))
            }
        }
    }

    /// Forget the connection, so the next call DIALS: no hold of its own.
    ///
    /// Holding the resolver would fail every call with the same error forever, so it goes. But
    /// an `Unavailable` answer does not say the peer is down: the peer's own endpoint can
    /// answer it (its model backend restarting, say) over a healthy connection, and a lost
    /// reply can be one dropped packet. Through gonk 0.x this held the mount off for [`RETRY`]
    /// anyway, so a single hiccup made explain unavailable for half a minute (ledger
    /// [#1300](http://localhost:1060/l/default/item/1300)). Now the next call redials, and only
    /// a dial that FAILS starts the backoff ([`hold_after`]): a peer that is really down still
    /// costs one dial per hold window, never one per request.
    fn drop_connection(&self) {
        self.state().connected = None;
    }

    /// A transient from the peer, with the peer's NAME in it ([`Error::Timeout`] and
    /// [`Error::Unavailable`], the same variant): the transports say "the peer did not describe
    /// itself within 20s" and "connection lost", and a caller several hops away cannot tell
    /// WHICH peer from that (ledger #1288). Every other error is the peer's answer, as it was.
    fn naming_the_peer(&self, error: Error) -> Error {
        let named = |message: String| {
            let peer = self.target.to_string();
            if message.contains(&peer) {
                message
            } else {
                format!("the mounted peer at {peer}: {message}")
            }
        };
        match error {
            Error::Timeout(message) => Error::Timeout(named(message)),
            Error::Unavailable(message) => Error::Unavailable(named(message)),
            other => other,
        }
    }

    /// The state, **taking a poisoned lock as state rather than as a fatality**.
    ///
    /// What this guards lives entirely inside this module — a connection handle and a
    /// deadline — and neither can be left half-written by an unwind. A `PoisonError` here
    /// would turn one panicked call into a mount that fails every call forever, which is
    /// precisely what it did before this line existed.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// ★ **The net grant is enforced against the host this mount dials** (ledger
/// [#805](http://localhost:1060/l/default/item/805), part 1).
///
/// `ikigai-browse` declares the OFFERING wildcard `urn:cap:net:*` on every derivation, and the
/// kernel satisfies a trailing-`*` requirement with ANY grant under the prefix — so before
/// this check a grant naming `urn:cap:net:localhost` reached a peer at `127.0.0.1`, and one
/// naming `urn:cap:net:anywhere.example` reached it too. The kernel's answer is right for an
/// offering (it cannot know which host an argument will name); the host is known HERE, at
/// the one place a call leaves for it, so this is where "may reach that host" is decided.
///
/// Root (the owner-only socket, and this process's own startup reads) holds every host. A
/// scoped capability must hold `urn:cap:net:{host}` exactly — [`Target::net_host`], the same
/// spelling `--browse derive` mints and `trigger::check_reviewer` checks, so a grant this
/// server minted always passes and a hand-written one for another host is refused by name.
///
/// ```
/// use ikigai_core::Capability;
/// use ikigai_gonk::mount;
/// let home = std::path::Path::new("/home/nobody");
/// let socket = mount::parse("prefer urn:llm:=/tmp/llm.sock", home).unwrap().target;
/// let named = |host: &str| Capability::scoped([format!("urn:cap:net:{host}")]);
/// assert!(mount::net_grant(&socket, &named("localhost")).is_ok());
/// assert!(mount::net_grant(&socket, &named("127.0.0.1")).is_err());
/// assert!(mount::net_grant(&socket, &Capability::root()).is_ok());
/// ```
///
/// # Errors
///
/// [`Error::Denied`], naming the token the caller lacks and the host the mount dials.
pub fn net_grant(target: &Target, capability: &Capability) -> Result<(), Error> {
    let needed = format!("urn:cap:net:{}", target.net_host());
    if capability.allows(&needed) {
        return Ok(());
    }
    let held: Vec<&str> = capability
        .scopes()
        .map(|scopes| {
            scopes
                .iter()
                .map(String::as_str)
                .filter(|scope| scope.starts_with("urn:cap:net:"))
                .collect()
        })
        .unwrap_or_default();
    Err(Error::Denied(format!(
        "reaching the mounted peer at {target} needs `{needed}` — a net grant names the host \
         the mount dials, exactly{}",
        if held.is_empty() {
            String::new()
        } else {
            format!(", and this capability names {}", held.join(", "))
        }
    )))
}

/// Dial `target`, with `describe` as the peer's self-description deadline
/// ([`DESCRIBE_TIMEOUT`]) and `call` as every other call's ([`CALL_TIMEOUT`]). Neither
/// transport blocks longer than its own connect timeout.
fn dial(target: &Target, describe: Duration, call: Duration) -> Result<Arc<Peer>, String> {
    match target {
        Target::Socket(path) => ikigai_ipc::connect(path)
            .map(|resolver| {
                Arc::new(Peer::Socket(
                    resolver
                        .with_describe_timeout(Some(describe))
                        .with_call_timeout(Some(call)),
                ))
            })
            .map_err(|e| format!("{}: {e}", path.display())),
        Target::Quic { authority, certs } => {
            let identity = ikigai_quic::Identity {
                cert_pem: read(&certs.join("client.crt"))?,
                key_pem: read(&certs.join("client.key"))?,
            };
            let server = read(&certs.join("server.crt"))?;
            // Resolved per dial, never at startup: a peer named rather than numbered may
            // only become resolvable once the network is up.
            let addr = authority
                .to_socket_addrs()
                .map_err(|e| format!("quic://{authority}: {e}"))?
                .next()
                .ok_or_else(|| format!("quic://{authority}: resolved to no address"))?;
            ikigai_quic::connect(addr, &identity, &server)
                .map(|resolver| {
                    Arc::new(Peer::Quic(
                        resolver
                            .with_describe_timeout(Some(describe))
                            .with_call_timeout(Some(call)),
                    ))
                })
                .map_err(|e| format!("quic://{authority} ({addr}): {e}"))
        }
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Run `work` on a thread of its own and wait for it.
///
/// See the module docs: the mounted resolvers own tokio runtimes, and `Runtime::block_on`
/// panics when it is entered from a runtime thread — which both of this server's wire doors
/// are. A thread per call is the price of that, and it is paid on the path where a model
/// call is about to take a second or more.
///
/// A panic in `work` arrives as `None`, so it becomes a typed error at the call site instead
/// of unwinding a door's worker.
fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv().ok()
}

impl Resolver for LazyMount {
    fn issue(&self, request: Request) -> Result<(Representation, CacheStatus), Error> {
        self.issue_as(request, &Capability::root())
    }

    fn issue_as(
        &self,
        request: Request,
        capability: &Capability,
    ) -> Result<(Representation, CacheStatus), Error> {
        // Before the dial: a caller this mount refuses costs no handshake.
        net_grant(&self.target, capability)?;
        let peer = self.resolver()?;
        let capability = capability.clone();
        let target = self.target.to_string();
        // A long poll (`wait=`) runs under the call deadline plus its wait; everything else
        // under the standing deadlines the dial set. A `Meta` keeps the describe deadline.
        let deadline = (request.verb != Verb::Meta && request.args.contains_key("wait"))
            .then(|| deadline_for(&request, self.call_timeout));
        let answer =
            off_thread(move || peer.issue_as(request, &capability, deadline)).ok_or_else(|| {
                Error::Endpoint(format!("the mount to {target} panicked serving this call"))
            })?;
        if matches!(answer, Err(Error::Unavailable(_))) {
            self.drop_connection();
        }
        let answer = answer.map_err(|error| self.naming_the_peer(error));
        if self.fresh {
            return answer.map(|(representation, _)| {
                (
                    representation.with_expiry(ikigai_core::Expiry::Always),
                    CacheStatus::Uncacheable,
                )
            });
        }
        answer
    }

    fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        // The peer knows; if it cannot be asked, the answer is no.
        let Ok(peer) = self.resolver() else {
            return false;
        };
        let (request, capability) = (request.clone(), capability.clone());
        off_thread(move || peer.resolver().is_cached(&request, &capability)).unwrap_or(false)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        let peer = self.resolver().ok()?;
        off_thread(move || peer.resolver().entries()).flatten()
    }

    /// The tracer is forwarded only when the peer is ALREADY connected: `set_tracer` is
    /// called on the resolve path, and dialling from it would turn `trace` into a connect
    /// attempt. Untraced (or first-call) mounted work shows as one node rather than the
    /// peer's subtree, which is a loss of detail and not of correctness.
    fn set_tracer(&self, tracer: Arc<dyn ikigai_core::Tracer>) {
        if let Some(peer) = self.state().connected.as_ref() {
            peer.resolver().set_tracer(tracer);
        }
    }

    fn clear_tracer(&self) {
        if let Some(peer) = self.state().connected.as_ref() {
            peer.resolver().clear_tracer();
        }
    }

    fn transport(&self) -> String {
        format!("mount · {}", self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/u")
    }

    #[test]
    fn a_socket_mount_parses_and_expands_home() {
        let mount = parse("prefer urn:llm:=~/.ikigai/host.sock", &home()).unwrap();
        assert_eq!(mount.prefix, LLM_PREFIX);
        assert_eq!(
            mount.target,
            Target::Socket(PathBuf::from("/home/u/.ikigai/host.sock"))
        );
        assert_eq!(mount.target.to_string(), "/home/u/.ikigai/host.sock");
    }

    #[test]
    fn a_quic_mount_needs_a_certificate_directory_that_exists() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let line = format!(
            "prefer urn:llm:=quic://127.0.0.1:4433 {}",
            dir.path().display()
        );
        let mount = parse(&line, &home()).unwrap();
        assert_eq!(
            mount.target,
            Target::Quic {
                authority: "127.0.0.1:4433".to_string(),
                certs: dir.path().to_path_buf(),
            }
        );
        assert_eq!(mount.target.to_string(), "quic://127.0.0.1:4433");

        let bare = parse("prefer urn:llm:=quic://127.0.0.1:4433", &home()).unwrap_err();
        assert!(bare.contains("certificate directory"), "{bare}");
        let missing = parse(
            "prefer urn:llm:=quic://127.0.0.1:4433 /nope/nowhere",
            &home(),
        )
        .unwrap_err();
        assert!(missing.contains("is not a directory"), "{missing}");
        let socket_with_certs =
            parse("prefer urn:llm:=/tmp/x.sock /tmp/certs", &home()).unwrap_err();
        assert!(
            socket_with_certs.contains("takes no certificate"),
            "{socket_with_certs}"
        );
    }

    /// ★ Both halves of what this key will NOT do: another mode, and another namespace.
    #[test]
    fn only_prefer_and_only_the_llm_prefix() {
        let overridden = parse("override urn:llm:=/tmp/x.sock", &home()).unwrap_err();
        assert!(overridden.contains("must be `prefer`"), "{overridden}");
        let elsewhere = parse("prefer urn:personal:=/tmp/x.sock", &home()).unwrap_err();
        assert!(elsewhere.contains("urn:llm:` only"), "{elsewhere}");
        // A layer is reached by binding it (`gonk.layer`), never by a mount line.
        for prefix in ["urn:layer:", "urn:layers:", "urn:collab:"] {
            let layer = parse(&format!("prefer {prefix}=/tmp/x.sock"), &home()).unwrap_err();
            assert!(layer.contains("urn:llm:` only"), "{layer}");
        }
        assert!(
            parse("prefer urn:llm=/tmp/x.sock", &home()).is_err(),
            "no colon"
        );
        assert!(
            parse("prefer urn:llm:ask=/tmp/x.sock", &home()).is_err(),
            "one resource"
        );
        assert!(parse("prefer", &home()).is_err());
        assert!(parse("", &home()).is_err());
    }

    /// A dial that cannot succeed fails as a TRANSIENT, is held off rather than retried per
    /// call, and never panics the caller — this is what "explain is an addition, never a
    /// dependency" rests on.
    #[test]
    fn an_unreachable_peer_is_unavailable_and_then_held_off() {
        let mount = LazyMount::new(Target::Socket(PathBuf::from(
            "/nonexistent/ikigai-gonk-test.sock",
        )));
        let first = mount.resolver().err().expect("no such socket");
        assert!(
            matches!(first, Error::Unavailable(_)),
            "a peer that is down is transient, not permanent: {first:?}"
        );
        let second = mount.resolver().err().expect("still no such socket");
        assert!(
            format!("{second:?}").contains("held off"),
            "the second attempt is held off: {second:?}"
        );
        assert_eq!(mount.entries(), None, "a down peer enumerates nothing");
    }
}
