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
//! Every call through them runs under a SHORT deadline ([`CALL_TIMEOUT`], plus the wait of a
//! feed read that asks for one): a layer host that accepts a call and never answers it fails the
//! pass in seconds, naming the host, rather than holding it for the transport's idle timeout
//! (ledger [#1335](http://localhost:1060/l/default/item/1335)).
//!
//! # What a binding refuses
//!
//! - **A layer whose model is not `tuplespace`.** Only a task layer is bound to a ledger;
//!   messages never touch one. The model is read from the layer's own definition
//!   (`urn:layer:{layer}`, `ik:layerModel`) when the binding first runs, not at start (a door
//!   that is down must not stop this server), and a binding to any other model is refused for
//!   the life of the process, said once on stderr.
//! - An OFFER line naming the `default` ledger, or a ledger not named for its collaboration
//!   unless confirmed (`confirm-ledger=<ledger>`): [`check_offered_ledger`], ledger #1745.
//! - Two lines for one layer; two OFFER lines for one ledger (an item would be offered twice);
//!   bindings that name different doors (one layer host per gonk, so `urn:layers:` and
//!   `urn:collab:` have one place to resolve).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use ikigai_core::{Error, Iri, Representation, Request, Resolution, Scope, Space, SpaceEntry};
use ikigai_ledger::claim::ClaimKind;
use ikigai_ledger::Ledger;
use ikigai_resolve::{MountedRemote, Resolver};

use crate::mount::{LazyMount, Target};

/// The longest layer name ikigai-layers accepts: MCP caps a tool name at 64 characters and the
/// longest a layer projects is `layer-{layer}-latest__source`.
pub const MAX_LAYER_NAME: usize = 24;

/// The deadline every call through a layer binding's door runs under: a feed read gets its own
/// wait on top ([`crate::mount::deadline_for`]), and a self-description keeps
/// [`crate::mount::DESCRIBE_TIMEOUT`].
///
/// ★ Ledger [#1335](http://localhost:1060/l/default/item/1335): every other call a mirror or a
/// taker makes (a definition, an `rd` page, a take, a drop, `urn:layers:me`) is answered by a
/// healthy layer host in well under a second, so fifteen is generous and a host that accepts a
/// call and never answers it costs a pass fifteen seconds, not the transport's 300 (twice, with
/// the replay). Far below the mount default ([`crate::mount::CALL_TIMEOUT`]), which is sized
/// for model calls.
///
/// ```
/// use ikigai_gonk::layer::CALL_TIMEOUT;
/// assert_eq!(CALL_TIMEOUT, std::time::Duration::from_secs(15));
/// assert!(CALL_TIMEOUT < ikigai_gonk::mount::CALL_TIMEOUT);
/// ```
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// The operator's deny file (`gonk.layer.deny`), for an offer binding: an item whose title
    /// or body matches a line of it is not offered ([`crate::egress`]). Set by the config from
    /// its own key, never from the line.
    pub deny: Option<Arc<crate::egress::DenyList>>,
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

/// An item of THIS gonk's ledgers as a task layer's readers see it (ledger
/// [#1856](http://localhost:1062/l/default/item/1856)): its words, `{ledger} #{number}`, and the
/// page that shows it, `/l/{ledger}/item/{number}`.
///
/// The ledger is the one the item's OWN IRI names, never an assumed `default`: numbers are per
/// ledger, so a bare `#12` is ambiguous the moment a second ledger exists (field guide 9i). The
/// ledger's name is percent-encoded into the path. `None` when `iri` is not a ledger item's
/// IRI, since then there is no ledger to name.
///
/// ```
/// use ikigai_gonk::layer::item_link;
/// assert_eq!(
///     item_link("urn:iki:ledger:bobby:item:01abc", 12),
///     Some(("bobby #12".to_string(), "/l/bobby/item/12".to_string()))
/// );
/// // The default ledger is named too: `#12` alone could be any ledger's.
/// assert_eq!(
///     item_link("urn:iki:ledger:item:01abc", 7),
///     Some(("default #7".to_string(), "/l/default/item/7".to_string()))
/// );
/// assert_eq!(item_link("urn:layer:tasks:offer:01abc", 1), None);
/// ```
pub fn item_link(iri: &str, number: i64) -> Option<(String, String)> {
    let (ledger, _) = Ledger::item_iri(iri)?;
    Some(number_link(&ledger, number))
}

/// [`item_link`] for an item known by its ledger and number.
///
/// ```
/// use ikigai_gonk::layer::number_link;
/// let ledger = ikigai_ledger::Ledger::parse("ikigai").unwrap();
/// assert_eq!(number_link(&ledger, 3).1, "/l/ikigai/item/3");
/// ```
pub fn number_link(ledger: &Ledger, number: i64) -> (String, String) {
    (
        format!("{} #{number}", ledger.name()),
        format!(
            "/l/{}/item/{number}",
            crate::web::percent_encode(ledger.name())
        ),
    )
}

/// An item in a mirror's or a taker's log line: [`item_link`]'s words and page, `bobby #12
/// (/l/bobby/item/12)`, or the ledger's own display form when the item's IRI names no ledger.
pub fn item_words(item: &ikigai_ledger::json::Item) -> String {
    match item_link(&item.iri, item.number) {
        Some((words, path)) => format!("{words} ({path})"),
        None => item.display.clone(),
    }
}

/// The word on an offer line that confirms a ledger whose name is not its collaboration's:
/// `confirm-ledger=<the ledger's name, again>`.
pub const CONFIRM_LEDGER: &str = "confirm-ledger";

/// Whether an OFFER binding may offer `ledger` (ledger
/// [#1745](http://localhost:1062/l/default/item/1745)): every open, unclaimed item of the ledger
/// it names is published to another party within one pass, so one wrong word must not offer the
/// wrong ledger.
///
/// - **The default ledger is never offered**, confirmed or not: it is this server's own work,
///   and its items are not written to be shared.
/// - **Any other ledger is offered when it is named for its collaboration** (`ledger=bobby
///   collab=bobby`, the convention a collaboration's ledger follows), or when the line repeats
///   its name as `confirm-ledger=<name>`, a deliberate second word for an unusual pairing.
///
/// ```
/// use ikigai_gonk::layer::check_offered_ledger;
/// assert!(check_offered_ledger("…", "bobby", Some("bobby"), None).is_ok());
/// assert!(check_offered_ledger("…", "work", Some("bobby"), Some("work")).is_ok());
/// assert!(check_offered_ledger("…", "work", Some("bobby"), None).unwrap_err().contains("confirm-ledger=work"));
/// assert!(check_offered_ledger("…", "work", None, Some("wrok")).is_err());
/// assert!(check_offered_ledger("…", "default", Some("default"), Some("default")).unwrap_err().contains("never offered"));
/// ```
///
/// # Errors
///
/// The default ledger; a ledger neither named for its collaboration nor confirmed.
pub fn check_offered_ledger(
    line: &str,
    ledger: &str,
    collab: Option<&str>,
    confirm: Option<&str>,
) -> Result<(), String> {
    if ledger == crate::config::DEFAULT_LEDGER {
        return Err(format!(
            "gonk.layer `{line}`: the `{ledger}` ledger is never offered into a layer: an offer \
             publishes every open, unclaimed item's title and body to another party, and the \
             default ledger is this server's own work. Offer a ledger kept for the collaboration \
             (`ledger=<collab> collab=<collab>`)"
        ));
    }
    if collab == Some(ledger) || confirm == Some(ledger) {
        return Ok(());
    }
    Err(format!(
        "gonk.layer `{line}`: ledger `{ledger}` is offered {}, and an offer publishes every \
         open, unclaimed item of it to another party. Name the ledger for its collaboration \
         (`ledger={ledger} collab={ledger}`), or confirm this one by repeating its name: \
         `{CONFIRM_LEDGER}={ledger}`",
        match collab {
            Some(c) => format!("to collaboration `{c}`"),
            None => "with no collaboration named".to_string(),
        }
    ))
}

/// Parse one `gonk.layer` line: `<layer> <offer|take> <quic url> <cert dir> ledger=<name>
/// [collab=<name>] [confirm-ledger=<name>]`. `home` is `$HOME`, for `~/` in the certificate
/// directory. An offer line is checked by [`check_offered_ledger`].
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
                 [collab=<name>] [confirm-ledger=<name>]`";
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
    let (mut ledger, mut collab, mut confirm) = (None, None, None);
    for word in words {
        match word.split_once('=') {
            Some(("ledger", name)) if ledger.is_none() => ledger = Some(name.to_string()),
            Some((CONFIRM_LEDGER, name)) if confirm.is_none() => confirm = Some(name.to_string()),
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
    if role == Role::Offer {
        check_offered_ledger(line, &ledger, collab.as_deref(), confirm.as_deref())?;
    } else if confirm.is_some() {
        return Err(format!(
            "gonk.layer `{line}`: `{CONFIRM_LEDGER}=` confirms an OFFER binding's ledger, and \
             this line takes"
        ));
    }
    Ok(Binding {
        layer,
        role,
        target: Target::Quic {
            authority: authority.to_string(),
            certs,
        },
        ledger,
        collab,
        deny: None,
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
    /// The one connection to the layer host, shared by the mounts and the bindings' passes (each
    /// binding's feed long-polls on a door of its own, [`feed_door`]).
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
    let door = Arc::new(LazyMount::fresh(first.target.clone()).with_call_timeout(CALL_TIMEOUT));
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

/// The door a binding's FEED long-polls on: a [`LazyMount::fresh`] of its own on the binding's
/// target, under [`CALL_TIMEOUT`] (a feed read gets its wait on top), that nothing dials until
/// the feed first reads. One per binding, never the door its passes use.
///
/// ★ Ledger [#1743](http://localhost:1060/l/default/item/1743): a QUIC door answers the calls on
/// one connection ONE AT A TIME (ikigai-quic through 0.1.63), and a [`LazyMount`] is one
/// connection. A feed's `wait=50` read on the passes' door held every pass call past its
/// deadline. Two bindings' feeds on one door would hold each other the same way, hence one each.
/// (The Layers page learned this first: its follower has a door of its own,
/// [`crate::layers_view::LayersView::connect`].)
pub fn feed_door(binding: &Binding) -> Arc<dyn Door> {
    Arc::new(LazyMount::fresh(binding.target.clone()).with_call_timeout(CALL_TIMEOUT))
}

/// Start each binding's worker on `mounted`'s door, and say what is bound (ledger #1190): what
/// `ikigai-gonk serve` calls.
///
/// The offer role runs the mirror ([`crate::mirror::start`]), the take role the taker
/// ([`crate::taker::start`]), each with its feed on a door of its own ([`feed_door`]); nothing
/// is dialled here, so a layer door that is down stops
/// nothing (each worker says so once, and keeps trying).
///
/// # Errors
///
/// A binding whose worker cannot be built (its capability, or its role).
pub fn start(
    bindings: &[Binding],
    mounted: &Mounted,
    hub: &Arc<ikigai_core::Kernel>,
) -> Result<String, String> {
    let Some(door) = mounted.door.as_ref() else {
        return Ok("no gonk.layer, so no ledger is bound to a layer".to_string());
    };
    let mut lines = Vec::new();
    for binding in bindings {
        match binding.role {
            Role::Offer => {
                let worker = crate::mirror::Mirror::new(
                    binding.clone(),
                    Arc::clone(door) as Arc<dyn Door>,
                    Arc::clone(hub),
                )?;
                // ★ Ledger #1745: how much one binding would publish, said before its first
                // pass, so a binding to the wrong ledger is seen in the log.
                let eligible = match worker.eligible() {
                    Ok(n) => format!("{n} open, unclaimed item(s) it would offer"),
                    Err(e) => format!("its items could not be counted: {e}"),
                };
                crate::mirror::start(worker, feed_door(binding));
                lines.push(format!(
                    "ledger `{}` offered into layer `{}` on {} ({eligible})",
                    binding.ledger, binding.layer, binding.target
                ));
            }
            Role::Take => {
                let ledger = crate::taker::hub_ledger(binding, Arc::clone(hub))?;
                let worker = crate::taker::Taker::new(
                    binding.clone(),
                    Arc::clone(door) as Arc<dyn Door>,
                    ledger,
                )?;
                crate::taker::start(worker, hub, feed_door(binding));
                lines.push(format!(
                    "layer `{}` on {} taken into ledger `{}` (`ikigai-gonk layer take <offer>`)",
                    binding.layer, binding.target, binding.ledger
                ));
            }
        }
    }
    Ok(lines.join("; "))
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

/// `ik:Machine`: a running host acting under its own identity (itself a `prov:SoftwareAgent`).
const IK_MACHINE: &str = "https://ikigai-rs.dev/ns#Machine";
/// `foaf:Person`.
const FOAF_PERSON: &str = "http://xmlns.com/foaf/0.1/Person";
/// `prov:SoftwareAgent`: an agent acting for a person (a collaborator's Claude).
const PROV_SOFTWARE_AGENT: &str = "http://www.w3.org/ns/prov#SoftwareAgent";

/// The claim kind a participant's `rdf:type`s in a collaboration's participants graph name, read
/// the way the layer host reads them for `urn:layers:me` (ikigai-layers' `kind_of`): `ik:Machine`
/// first (it is also a `prov:SoftwareAgent`), then `foaf:Person`, then `prov:SoftwareAgent` as an
/// AGENT ([`ClaimKind::Agent`], ikigai-ledger 0.11.0, ledger
/// [#1330](http://localhost:1060/l/default/item/1330)), and `Machine`, the ledger's own default,
/// for anything else.
///
/// ```
/// use std::collections::BTreeSet;
/// use ikigai_gonk::layer::kind_of;
/// use ikigai_ledger::claim::ClaimKind;
/// let types = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
/// let agent = "http://www.w3.org/ns/prov#SoftwareAgent";
/// assert_eq!(kind_of(&types(&["http://xmlns.com/foaf/0.1/Person"])), ClaimKind::Person);
/// assert_eq!(kind_of(&types(&[agent])), ClaimKind::Agent);
/// assert_eq!(kind_of(&types(&[agent, "https://ikigai-rs.dev/ns#Machine"])), ClaimKind::Machine);
/// assert_eq!(kind_of(&types(&["urn:example:Robot"])), ClaimKind::Machine);
/// ```
pub fn kind_of(types: &BTreeSet<String>) -> ClaimKind {
    if types.contains(IK_MACHINE) {
        ClaimKind::Machine
    } else if types.contains(FOAF_PERSON) {
        ClaimKind::Person
    } else if types.contains(PROV_SOFTWARE_AGENT) {
        ClaimKind::Agent
    } else {
        ClaimKind::Machine
    }
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
        let bare = parse(
            &line("ledger=bobby confirm-ledger=bobby", certs.path()),
            &home(),
        )
        .unwrap();
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

    /// ★ Ledger #1745: `ledger=default` in the offer role was accepted, and nothing tied
    /// `ledger=` to `collab=`, so one wrong word offered every open, unclaimed item of the main
    /// ledger to another party within one pass.
    #[test]
    fn the_default_ledger_is_never_offered_and_another_is_offered_only_when_paired() {
        let certs = tempfile::tempdir().unwrap();
        let c = certs.path().display();
        let refused = |l: String, needle: &str| {
            let e = parse(&l, &home()).unwrap_err();
            assert!(e.contains(needle), "{l}: {e}");
        };
        refused(
            format!("t offer quic://h:1 {c} ledger=default collab=bobby"),
            "never offered",
        );
        refused(
            format!("t offer quic://h:1 {c} ledger=default collab=default confirm-ledger=default"),
            "never offered",
        );
        refused(
            format!("t offer quic://h:1 {c} ledger=work collab=bobby"),
            "confirm-ledger=work",
        );
        refused(
            format!("t offer quic://h:1 {c} ledger=work"),
            "no collaboration named",
        );
        refused(
            format!("t offer quic://h:1 {c} ledger=work collab=bobby confirm-ledger=wrok"),
            "confirm-ledger=work",
        );
        refused(
            format!("t take quic://h:1 {c} ledger=work confirm-ledger=work"),
            "OFFER binding",
        );
        let paired = parse(
            &format!("t offer quic://h:1 {c} ledger=bobby collab=bobby"),
            &home(),
        )
        .unwrap();
        assert_eq!(paired.ledger, "bobby");
        let confirmed = parse(
            &format!("t offer quic://h:1 {c} ledger=work collab=bobby confirm-ledger=work"),
            &home(),
        )
        .unwrap();
        assert_eq!(confirmed.ledger, "work");
        // The take role is untouched: a collaborator takes into whatever ledger it keeps.
        let take = parse(&format!("t take quic://h:1 {c} ledger=default"), &home()).unwrap();
        assert_eq!(take.role, Role::Take);
    }

    #[test]
    fn one_layer_once_one_ledger_offered_once_one_door() {
        let certs = tempfile::tempdir().unwrap();
        let c = certs.path().display();
        let twice = vec![
            format!("a offer quic://h:1 {c} ledger=x collab=x"),
            format!("a take quic://h:1 {c} ledger=y"),
        ];
        assert!(parse_all(&twice, &home())
            .unwrap_err()
            .contains("bound twice"));
        let offered_twice = vec![
            format!("a offer quic://h:1 {c} ledger=x collab=x"),
            format!("b offer quic://h:1 {c} ledger=x collab=x"),
        ];
        assert!(parse_all(&offered_twice, &home())
            .unwrap_err()
            .contains("offered into"));
        let two_doors = vec![
            format!("a offer quic://h:1 {c} ledger=x collab=x"),
            format!("b take quic://h:2 {c} ledger=x"),
        ];
        assert!(parse_all(&two_doors, &home())
            .unwrap_err()
            .contains("one layer host"));
        let fine = vec![
            format!("a offer quic://h:1 {c} ledger=x collab=x"),
            format!("b take quic://h:1 {c} ledger=x"),
        ];
        assert_eq!(parse_all(&fine, &home()).unwrap().len(), 2);
    }

    #[test]
    fn a_layer_space_claims_its_own_names_only() {
        let certs = tempfile::tempdir().unwrap();
        let binding = parse(&line("ledger=bobby collab=bobby", certs.path()), &home()).unwrap();
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
