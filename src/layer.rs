//! A named ledger bound to a task LAYER (ledger [#1190](http://localhost:1060/l/default/item/1190),
//! layers arc 12; design: devtools `claude/research/collab-layer-v1-2026-10-10.md`, sections 8
//! and 10).
//!
//! ```toml
//! # ours: offer ledger `bobby`'s items into the task layer `tasks-bobby`
//! gonk.layer = "tasks-bobby offer quic://ikigai-rs.dev:4434 ~/.config/ikigai/gonk/layers ledger=bobby collab=bobby"
//! # theirs: take offers from it into their own ledger
//! gonk.layer = "tasks-bobby take quic://ikigai-rs.dev:4434 ~/.config/ikigai/gonk/layers ledger=ikigai"
//! ```
//!
//! One line binds one layer, in one ROLE, to one ledger of this server. The fields are the
//! layer's name, the role, the layer host's door (`quic://host:port`), the certificate
//! directory this gonk presents there (`client.crt`, `client.key`, and the `server.crt` it
//! pins, laid out as `ikigai --cert-dir` expects), `ledger=<name>`, and optionally
//! `collab=<name>`, the collaboration whose participants graph says what kind of actor a taker
//! is (`urn:collab:{name}:participants`; see [`crate::mirror`] for what it decides).
//!
//! # What a binding mounts
//!
//! gonk mounts `urn:llm:` and nothing else on purpose ([`crate::mount`]). A binding adds the
//! three names a layer CLIENT needs (ledger [#1283](http://localhost:1060/l/default/item/1283)),
//! as override mounts on the configured door, and still nothing else:
//!
//! - `urn:layer:{layer}` and everything under `urn:layer:{layer}:`, for each bound layer
//!   (its definition, shapes, context, feed, tuples and receipts);
//! - `urn:layers:` (the multiplexed feed) and `urn:collab:` (the manifests and their
//!   participants), once, on the one door every binding names.
//!
//! Every answer through them is UNCACHEABLE ([`crate::mount::LazyMount::fresh`]): a task layer's
//! state changes on the layer host's schedule, and threads do not cross a mount. A `gonk.mount`
//! line still refuses every prefix but `urn:llm:`; a layer is reachable only by binding it.
//!
//! # What a binding refuses
//!
//! - **A layer whose model is not `tuplespace`.** Only a task layer is bound to a ledger;
//!   messages never touch one. The model is read from the layer's own definition
//!   (`urn:layer:{layer}`, `ik:layerModel`) when the binding first runs, not at start (a door
//!   that is down must not stop this server), and a binding to any other model is refused for
//!   the life of the process, said once on stderr.
//! - Two lines for one layer; two OFFER lines for one ledger (an item would be offered twice);
//!   bindings that name different doors (one layer host per gonk, so `urn:layers:` and
//!   `urn:collab:` have one place to resolve).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use ikigai_core::{Error, Iri, Representation, Request, Resolution, Scope, Space, SpaceEntry};
use ikigai_ledger::claim::ClaimKind;
use ikigai_ledger::Ledger;
use ikigai_resolve::{MountedRemote, Resolver};

use crate::mount::{LazyMount, Target};

/// The longest layer name ikigai-layers accepts: MCP caps a tool name at 64 characters and the
/// longest a layer projects is `layer-{layer}-latest__source`.
pub const MAX_LAYER_NAME: usize = 24;

/// The principal a mirror's own ledger writes carry: `urn:iki:gonk:layer:{layer}`.
///
/// ★ Minted by this process for its own internal actor, never by a door, and recognized as
/// principal-shaped ([`crate::admit::PRINCIPAL_PREFIXES`]): a caller at any door that wrote it
/// as its `author` would be refused, so a comment signed by a mirror is a mirror's.
pub const MIRROR_PRINCIPAL_PREFIX: &str = "urn:iki:gonk:layer:";

/// Which side of a layer this server is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// This ledger's items are offered into the layer, and taken work is claimed here for its
    /// taker ([`crate::mirror`]).
    Offer,
    /// Offers in the layer are taken into this ledger, and reported on.
    Take,
}

impl Role {
    /// The word a config line spells it with.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Offer => "offer",
            Role::Take => "take",
        }
    }
}

/// One `gonk.layer` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The layer's name: `urn:layer:{layer}`.
    pub layer: String,
    /// Which side this server is on.
    pub role: Role,
    /// The layer host's door, and the certificates presented there.
    pub target: Target,
    /// The ledger of this server the layer is bound to.
    pub ledger: String,
    /// The collaboration whose participants graph names the takers' kinds.
    pub collab: Option<String>,
}

impl Binding {
    /// `urn:layer:{layer}`.
    pub fn layer_iri(&self) -> String {
        format!("urn:layer:{}", self.layer)
    }

    /// The principal this binding's mirror writes to the ledger as.
    pub fn principal(&self) -> String {
        format!("{MIRROR_PRINCIPAL_PREFIX}{}", self.layer)
    }
}

/// Whether `name` is a layer name ikigai-layers would accept: lowercase `[a-z0-9-]`, one to
/// [`MAX_LAYER_NAME`] characters.
///
/// ```
/// use ikigai_gonk::layer::is_layer_name;
/// assert!(is_layer_name("tasks-bobby"));
/// assert!(!is_layer_name("Tasks"));
/// assert!(!is_layer_name("tasks:bobby"));
/// assert!(!is_layer_name(""));
/// assert!(!is_layer_name("a-name-that-is-far-too-long"));
/// ```
pub fn is_layer_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LAYER_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parse one `gonk.layer` line: `<layer> <offer|take> <quic url> <cert dir> ledger=<name>
/// [collab=<name>]`. `home` is `$HOME`, for `~/` in the certificate directory.
///
/// ```
/// use ikigai_gonk::layer::{parse, Role};
/// let certs = tempfile::tempdir().unwrap();
/// let line = format!(
///     "tasks-bobby offer quic://127.0.0.1:4434 {} ledger=bobby collab=bobby",
///     certs.path().display()
/// );
/// let binding = parse(&line, std::path::Path::new("/home/u")).unwrap();
/// assert_eq!(binding.role, Role::Offer);
/// assert_eq!(binding.ledger, "bobby");
/// assert_eq!(binding.collab.as_deref(), Some("bobby"));
/// assert_eq!(binding.principal(), "urn:iki:gonk:layer:tasks-bobby");
/// ```
///
/// # Errors
///
/// A field missing, misspelled or out of shape, a door that is not `quic://host:port`, a
/// certificate directory that is not one, or a ledger name the ledger refuses.
pub fn parse(line: &str, home: &Path) -> Result<Binding, String> {
    let shape = "expected `<layer> <offer|take> <quic://host:port> <cert dir> ledger=<name> \
                 [collab=<name>]`";
    let mut words = line.split_whitespace();
    let mut next = |what: &str| {
        words
            .next()
            .ok_or_else(|| format!("gonk.layer `{line}`: no {what}; {shape}"))
    };
    let layer = next("layer name")?.to_string();
    if !is_layer_name(&layer) {
        return Err(format!(
            "gonk.layer `{line}`: `{layer}` is not a layer name (lowercase letters, digits and \
             `-`, at most {MAX_LAYER_NAME} characters)"
        ));
    }
    let role = match next("role")? {
        "offer" => Role::Offer,
        "take" => Role::Take,
        other => {
            return Err(format!(
                "gonk.layer `{line}`: the role is `offer` or `take`, not `{other}`"
            ))
        }
    };
    let url = next("door")?;
    let Some(authority) = url.strip_prefix("quic://").filter(|a| a.contains(':')) else {
        return Err(format!(
            "gonk.layer `{line}`: the door is `quic://host:port`, not `{url}`: a layer host is \
             reached over QUIC with this server's own client certificate"
        ));
    };
    let certs = crate::mount::expand_home(next("certificate directory")?, home);
    if !certs.is_dir() {
        return Err(format!(
            "gonk.layer `{line}`: {} is not a directory (the client.crt, client.key and \
             server.crt this gonk presents at the layer door)",
            certs.display()
        ));
    }
    let (mut ledger, mut collab) = (None, None);
    for word in words {
        match word.split_once('=') {
            Some(("ledger", name)) if ledger.is_none() => ledger = Some(name.to_string()),
            Some(("collab", name)) if collab.is_none() => {
                if !is_layer_name(name) {
                    return Err(format!(
                        "gonk.layer `{line}`: `{name}` is not a collaboration name"
                    ));
                }
                collab = Some(name.to_string())
            }
            _ => return Err(format!("gonk.layer `{line}`: unexpected `{word}`; {shape}")),
        }
    }
    let ledger =
        ledger.ok_or_else(|| format!("gonk.layer `{line}`: no `ledger=<name>`; {shape}"))?;
    Ledger::parse(&ledger).map_err(|e| format!("gonk.layer `{line}`: ledger={ledger}: {e}"))?;
    Ok(Binding {
        layer,
        role,
        target: Target::Quic {
            authority: authority.to_string(),
            certs,
        },
        ledger,
        collab,
    })
}

/// Parse every `gonk.layer` line, and refuse the combinations one server cannot hold.
///
/// # Errors
///
/// Any line [`parse`] refuses; one layer bound twice; one ledger offered twice; two doors.
pub fn parse_all(lines: &[String], home: &Path) -> Result<Vec<Binding>, String> {
    let mut bindings: Vec<Binding> = Vec::new();
    for line in lines {
        let binding = parse(line, home)?;
        if let Some(seen) = bindings.iter().find(|b| b.layer == binding.layer) {
            return Err(format!(
                "gonk.layer: layer `{}` is bound twice (as {} into ledger `{}`, and as {} into \
                 `{}`): one line per layer",
                binding.layer,
                seen.role.as_str(),
                seen.ledger,
                binding.role.as_str(),
                binding.ledger
            ));
        }
        if binding.role == Role::Offer {
            if let Some(seen) = bindings
                .iter()
                .find(|b| b.role == Role::Offer && b.ledger == binding.ledger)
            {
                return Err(format!(
                    "gonk.layer: ledger `{}` is offered into `{}` and `{}`: an item would be \
                     offered, taken and claimed twice. One offer binding per ledger",
                    binding.ledger, seen.layer, binding.layer
                ));
            }
        }
        if let Some(first) = bindings.first() {
            if first.target != binding.target {
                return Err(format!(
                    "gonk.layer: `{}` is on {} and `{}` on {} (or with other certificates): one \
                     layer host per server, so `urn:layers:` and `urn:collab:` have one place \
                     to resolve",
                    first.layer, first.target, binding.layer, binding.target
                ));
            }
        }
        bindings.push(binding);
    }
    Ok(bindings)
}

/// The door the bindings share, connected lazily, and the mount spaces in front of the hub.
pub struct Mounted {
    /// The one connection to the layer host, shared by the mounts and the bindings' workers.
    pub door: Option<Arc<LazyMount>>,
    /// The spaces to compose in front of the hub (none without a binding).
    pub spaces: Vec<Arc<dyn Space>>,
}

/// Build the mounts every binding needs: `urn:layer:{layer}` per binding, then `urn:layers:` and
/// `urn:collab:`, all on one [`LazyMount::fresh`] that nothing dials until it is first used.
pub fn mount(bindings: &[Binding]) -> Mounted {
    let Some(first) = bindings.first() else {
        return Mounted {
            door: None,
            spaces: Vec::new(),
        };
    };
    let door = Arc::new(LazyMount::fresh(first.target.clone()));
    let origin = first.target.to_string();
    let resolver = || Arc::clone(&door) as Arc<dyn Resolver>;
    let mut spaces: Vec<Arc<dyn Space>> = bindings
        .iter()
        .map(|b| {
            Arc::new(LayerSpace {
                base: b.layer_iri(),
                inner: MountedRemote::overriding(resolver(), b.layer_iri(), origin.clone()),
            }) as Arc<dyn Space>
        })
        .collect();
    for prefix in ["urn:layers:", "urn:collab:"] {
        spaces.push(Arc::new(MountedRemote::overriding(
            resolver(),
            prefix.to_string(),
            origin.clone(),
        )));
    }
    Mounted {
        door: Some(door),
        spaces,
    }
}

/// One layer's names and nothing else: `urn:layer:{layer}` itself and everything under
/// `urn:layer:{layer}:`. A bare prefix would also claim `urn:layer:{layer}-other`, another
/// layer this server did not bind.
struct LayerSpace {
    base: String,
    inner: MountedRemote,
}

impl LayerSpace {
    fn claims(&self, iri: &str) -> bool {
        iri == self.base
            || iri
                .strip_prefix(self.base.as_str())
                .is_some_and(|rest| rest.starts_with(':'))
    }
}

impl Space for LayerSpace {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        if !self.claims(request.target.as_str()) {
            return Resolution::Miss;
        }
        self.inner.resolve(request, scope)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries().map(|entries| {
            entries
                .into_iter()
                .filter(|entry| self.claims(&entry.pattern))
                .collect()
        })
    }
}

/// How a binding's worker reaches the layer host: one request in, the representation out.
///
/// The worker issues as ITSELF, the identity its certificate is enrolled as at the door (the
/// door mints the principal and clamps to that participant's grant), so this trait carries no
/// capability. Production is the [`LazyMount`] the mounts share; a test is a kernel behind a
/// door that mints a participant.
pub trait Door: Send + Sync {
    /// Issue `request` at the layer host.
    ///
    /// # Errors
    ///
    /// Whatever the host or the transport answers.
    fn call(&self, request: Request) -> Result<Representation, Error>;
}

impl Door for LazyMount {
    fn call(&self, request: Request) -> Result<Representation, Error> {
        Resolver::issue(self, request).map(|(representation, _)| representation)
    }
}

/// A request at `iri` (a constant or a name built from a checked layer name).
pub(crate) fn request(verb: ikigai_core::Verb, iri: &str) -> Request {
    Request::new(
        verb,
        Iri::parse(iri).unwrap_or_else(|_| panic!("a layer IRI: {iri}")),
    )
}

/// What kind of actor each participant is, as each mirror last read it from its collaboration's
/// participants graph: `(mirror principal, participant IRI) → kind`.
///
/// ★ Read by the hub's claim stamper ([`crate::admit::claim_kind`]) for a claim a mirror takes:
/// the ledger stamps the kind from the CLAIMANT's invocation, and the claimant here is the
/// mirror, acting for a taker the door named. Process-wide because the stamper is built with
/// the hub, before any binding runs; keyed by the mirror's principal so two bindings never read
/// each other's collaboration.
fn kinds() -> &'static RwLock<BTreeMap<(String, String), ClaimKind>> {
    static KINDS: OnceLock<RwLock<BTreeMap<(String, String), ClaimKind>>> = OnceLock::new();
    KINDS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Record what `mirror`'s collaboration says each participant is, replacing what it said before.
pub fn record_kinds(mirror: &str, participants: &BTreeMap<String, ClaimKind>) {
    let mut kinds = kinds().write().unwrap_or_else(|p| p.into_inner());
    kinds.retain(|(m, _), _| m != mirror);
    for (participant, kind) in participants {
        kinds.insert((mirror.to_string(), participant.clone()), *kind);
    }
}

/// The kind of the claim `mirror` takes for `holder`: what its collaboration's participants
/// graph says, and `Machine` (the ledger's own default) when it says nothing.
pub fn kind_for(mirror: &str, holder: &str) -> ClaimKind {
    kinds()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(mirror.to_string(), holder.to_string()))
        .copied()
        .unwrap_or(ClaimKind::Machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> std::path::PathBuf {
        std::path::PathBuf::from("/home/u")
    }

    fn line(rest: &str, certs: &Path) -> String {
        format!(
            "tasks-bobby offer quic://127.0.0.1:4434 {} {rest}",
            certs.display()
        )
    }

    #[test]
    fn a_line_parses_with_and_without_a_collaboration() {
        let certs = tempfile::tempdir().unwrap();
        let bare = parse(&line("ledger=bobby", certs.path()), &home()).unwrap();
        assert_eq!(bare.layer, "tasks-bobby");
        assert_eq!(bare.collab, None);
        assert_eq!(bare.target.to_string(), "quic://127.0.0.1:4434");
        let take = format!(
            "tasks-bobby take quic://127.0.0.1:4434 {} ledger=ikigai",
            certs.path().display()
        );
        assert_eq!(parse(&take, &home()).unwrap().role, Role::Take);
    }

    #[test]
    fn every_field_is_checked() {
        let certs = tempfile::tempdir().unwrap();
        let refused = |l: &str, needle: &str| {
            let e = parse(l, &home()).unwrap_err();
            assert!(e.contains(needle), "{l}: {e}");
        };
        let c = certs.path().display();
        refused(
            &format!("Tasks offer quic://h:1 {c} ledger=b"),
            "not a layer name",
        );
        refused(
            &format!("t give quic://h:1 {c} ledger=b"),
            "`offer` or `take`",
        );
        refused(
            &format!("t offer /tmp/x.sock {c} ledger=b"),
            "quic://host:port",
        );
        refused(
            "t offer quic://h:1 /nope/nowhere ledger=b",
            "not a directory",
        );
        refused(&format!("t offer quic://h:1 {c}"), "no `ledger=<name>`");
        refused(
            &format!("t offer quic://h:1 {c} ledger=b extra"),
            "unexpected",
        );
        refused(
            &format!("t offer quic://h:1 {c} ledger=b ledger=c"),
            "unexpected",
        );
        refused(
            &format!("t offer quic://h:1 {c} ledger=items"),
            "ledger=items",
        );
        refused("t offer", "no door");
    }

    #[test]
    fn one_layer_once_one_ledger_offered_once_one_door() {
        let certs = tempfile::tempdir().unwrap();
        let c = certs.path().display();
        let twice = vec![
            format!("a offer quic://h:1 {c} ledger=x"),
            format!("a take quic://h:1 {c} ledger=y"),
        ];
        assert!(parse_all(&twice, &home())
            .unwrap_err()
            .contains("bound twice"));
        let offered_twice = vec![
            format!("a offer quic://h:1 {c} ledger=x"),
            format!("b offer quic://h:1 {c} ledger=x"),
        ];
        assert!(parse_all(&offered_twice, &home())
            .unwrap_err()
            .contains("offered into"));
        let two_doors = vec![
            format!("a offer quic://h:1 {c} ledger=x"),
            format!("b take quic://h:2 {c} ledger=x"),
        ];
        assert!(parse_all(&two_doors, &home())
            .unwrap_err()
            .contains("one layer host"));
        let fine = vec![
            format!("a offer quic://h:1 {c} ledger=x"),
            format!("b take quic://h:1 {c} ledger=x"),
        ];
        assert_eq!(parse_all(&fine, &home()).unwrap().len(), 2);
    }

    #[test]
    fn a_layer_space_claims_its_own_names_only() {
        let certs = tempfile::tempdir().unwrap();
        let binding = parse(&line("ledger=bobby", certs.path()), &home()).unwrap();
        let mounted = mount(&[binding]);
        assert_eq!(
            mounted.spaces.len(),
            3,
            "the layer, urn:layers:, urn:collab:"
        );
        let space = &mounted.spaces[0];
        let resolves = |iri: &str| {
            matches!(
                space.resolve(&request(ikigai_core::Verb::Source, iri), &Scope::empty()),
                Resolution::Hit(_)
            )
        };
        assert!(resolves("urn:layer:tasks-bobby"));
        assert!(resolves("urn:layer:tasks-bobby:tuples"));
        assert!(!resolves("urn:layer:tasks-bobby-other:tuples"));
        assert!(!resolves("urn:layer:talk:say"));
        assert!(!resolves("urn:llm:ask"));
    }

    #[test]
    fn kinds_are_per_mirror_and_machine_by_default() {
        let mut said = BTreeMap::new();
        said.insert("urn:p:alice".to_string(), ClaimKind::Person);
        record_kinds("urn:iki:gonk:layer:kinds-test", &said);
        assert_eq!(
            kind_for("urn:iki:gonk:layer:kinds-test", "urn:p:alice"),
            ClaimKind::Person
        );
        assert_eq!(
            kind_for("urn:iki:gonk:layer:kinds-other", "urn:p:alice"),
            ClaimKind::Machine
        );
        record_kinds("urn:iki:gonk:layer:kinds-test", &BTreeMap::new());
        assert_eq!(
            kind_for("urn:iki:gonk:layer:kinds-test", "urn:p:alice"),
            ClaimKind::Machine
        );
    }
}
