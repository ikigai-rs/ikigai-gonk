//! The one namespace this server mounts: `urn:llm:` on a peer that serves it.
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
//! for [`RETRY`] rather than retried per request, and a `urn:iki:ledger:*` read does not
//! touch this module at all. The mode word is still required in the line, and still spelled
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

use ikigai_core::{Capability, Error, Representation, Request, Space, SpaceEntry};
use ikigai_resolve::{CacheStatus, MountedRemote, Resolver};

/// The only prefix this server mounts.
///
/// A mount is composed IN FRONT of the local spaces and its catalog is surfaced through
/// every door, so a general `gonk.mount` key would let one config line put an arbitrary
/// remote namespace behind the socket, the QUIC door and the HTTP door at once — and the
/// manifest's claim that what is not linked is not reachable would become a claim about a
/// file nobody reads. One prefix, for the one thing this server has a use for.
pub const LLM_PREFIX: &str = "urn:llm:";

/// How long a failed dial is remembered before another is attempted.
///
/// The point is the catalog path: `MountedRemote::entries` asks the peer, and the catalog is
/// read by every `urn:kernel:catalog`, every manifold query and every door's enumeration. A
/// peer that is down would otherwise cost a connect attempt — five seconds, on QUIC — per
/// enumeration, which is how "explain is unavailable" turns into "the ledger pages hang".
pub const RETRY: Duration = Duration::from_secs(30);

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
fn expand_home(spelled: &str, home: &Path) -> PathBuf {
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
pub fn space(mount: &Mount) -> Arc<dyn Space> {
    let origin = mount.target.to_string();
    Arc::new(MountedRemote::overriding(
        Arc::new(LazyMount::new(mount.target.clone())) as Arc<dyn Resolver>,
        mount.prefix.clone(),
        origin,
    ))
}

/// A resolver that dials on first use, remembers a failure for [`RETRY`], and runs every
/// call `off_thread`.
pub struct LazyMount {
    target: Target,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    connected: Option<Arc<dyn Resolver>>,
    /// When the next dial may be attempted, after a failure.
    hold_until: Option<Instant>,
}

impl LazyMount {
    /// A mount on `target`, not yet dialled.
    pub fn new(target: Target) -> LazyMount {
        LazyMount {
            target,
            state: Mutex::new(State::default()),
        }
    }

    /// The connected resolver, dialling if this is the first use or the hold has expired.
    ///
    /// `Err` is [`Error::Unavailable`] — the transient the mount means it to be: a caller
    /// (or a reliability overlay above it) can tell "the peer is down" from "the peer said
    /// no", which a blanket endpoint error could not.
    fn resolver(&self) -> Result<Arc<dyn Resolver>, Error> {
        let mut state = self.state();
        if let Some(resolver) = &state.connected {
            return Ok(Arc::clone(resolver));
        }
        if let Some(until) = state.hold_until {
            if Instant::now() < until {
                return Err(Error::Unavailable(format!(
                    "the mounted peer at {} is not reachable (last dial failed; the next is \
                     held off for up to {}s)",
                    self.target,
                    RETRY.as_secs()
                )));
            }
        }
        // The lock is deliberately held across the dial: a burst of first calls should cost
        // one handshake, not one each.
        let target = self.target.clone();
        let dialled = off_thread(move || dial(&target))
            .unwrap_or_else(|| Err("the dial panicked".to_string()));
        match dialled {
            Ok(resolver) => {
                state.hold_until = None;
                state.connected = Some(Arc::clone(&resolver));
                Ok(resolver)
            }
            Err(e) => {
                state.hold_until = Some(Instant::now() + RETRY);
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

    /// Forget the connection and hold off for [`RETRY`] before dialling again.
    ///
    /// Both wire resolvers redial internally on a dead connection, so reaching here means
    /// one has already tried and given up — the peer is down rather than the socket stale.
    /// Holding the resolver would then fail every call with the same error forever; dialling
    /// on the very next call would put a connect attempt in front of every request while it
    /// stays down. The hold is the same one a first failed dial takes, and the cost of it is
    /// that a single hiccup makes explain unavailable for half a minute.
    fn drop_connection(&self) {
        let mut state = self.state();
        state.connected = None;
        state.hold_until = Some(Instant::now() + RETRY);
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

/// Dial `target`. Neither transport blocks longer than its own connect timeout.
fn dial(target: &Target) -> Result<Arc<dyn Resolver>, String> {
    match target {
        Target::Socket(path) => ikigai_ipc::connect(path)
            .map(|resolver| Arc::new(resolver) as Arc<dyn Resolver>)
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
                .map(|resolver| Arc::new(resolver) as Arc<dyn Resolver>)
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
        let resolver = self.resolver()?;
        let capability = capability.clone();
        let target = self.target.to_string();
        let answer =
            off_thread(move || resolver.issue_as(request, &capability)).ok_or_else(|| {
                Error::Endpoint(format!("the mount to {target} panicked serving this call"))
            })?;
        if matches!(answer, Err(Error::Unavailable(_))) {
            self.drop_connection();
        }
        answer
    }

    fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        // The peer knows; if it cannot be asked, the answer is no.
        let Ok(resolver) = self.resolver() else {
            return false;
        };
        let (request, capability) = (request.clone(), capability.clone());
        off_thread(move || resolver.is_cached(&request, &capability)).unwrap_or(false)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        let resolver = self.resolver().ok()?;
        off_thread(move || resolver.entries()).flatten()
    }

    /// The tracer is forwarded only when the peer is ALREADY connected: `set_tracer` is
    /// called on the resolve path, and dialling from it would turn `trace` into a connect
    /// attempt. Untraced (or first-call) mounted work shows as one node rather than the
    /// peer's subtree, which is a loss of detail and not of correctness.
    fn set_tracer(&self, tracer: Arc<dyn ikigai_core::Tracer>) {
        if let Some(resolver) = self.state().connected.as_ref() {
            resolver.set_tracer(tracer);
        }
    }

    fn clear_tracer(&self) {
        if let Some(resolver) = self.state().connected.as_ref() {
            resolver.clear_tracer();
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
