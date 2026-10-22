//! **Onboarding a node by itself** (ledger [#2009](http://localhost:1062/l/default/item/2009),
//! phase 1, arc B): the controller's tools, beside the controller key, and gonk's own half of
//! applying a signed entitlement.
//!
//! ```toml
//! gonk.onboard = "quic://ikigai-rs.dev:4434 ~/.config/ikigai/gonk/onboard-door"
//! ```
//!
//! One line names the layer host's door and the certificate directory this gonk presents there AS
//! THE CONTROLLER (`client.crt`, `client.key` and the door's `server.crt`, laid out as `ikigai
//! --cert-dir` expects). That identity must be the layer host's `onboard.controller`: only it may
//! say on the grants layer, and it reads the lobby. Optional words: `lobby=<layer>` (default
//! `enroll`), `grants=<layer>` (default `grants`), `key=<keystore name>` (default
//! [`DEFAULT_KEY`]), `controller-key=<path>` (default `<config home>/gonk/onboard/controller.pub.pem`)
//! and `kinds=<path>` (default `<config home>/gonk/onboard/kinds.ttl`).
//!
//! # The map, as built
//!
//! ```text
//! urn:iki:onboard:kind:{kind}       Source  ATOM (data): one kind's Turtle, from the kinds file (read per call)
//! urn:iki:onboard:invite            Sink    code: mint an invite (name, kind) and its pairing code, shown ONCE
//! urn:iki:onboard:invite:{id}       Source  ATOM (gonk's store, graph urn:iki:onboard:graph:invites): no code
//! urn:iki:onboard:check:{entry}     Source  composition: the lobby entry (door) x the invite (store) x the code
//! urn:iki:onboard:pending           Source  composition: lobby entries with no entitlement, each checked
//! urn:iki:onboard:approve           Sink    code: build, SIGN, say the entitlement; mark the invite used; apply
//! urn:iki:onboard:revoke            Sink    code: a signed entitlement with no grant, superseding the current one
//! urn:iki:onboard:apply             Sink    code: enroll each verified entitlement's gonk grant (client add --cert)
//! urn:iki:onboard:status            Source  what is in force here, what was refused and why, restart pending
//! /onboard (HTTP page)              view    pending, in force, refused; the approve COMMAND, never a button
//! ```
//!
//! **Who may call what.** Every write ([`INVITE`], [`APPROVE`], [`REVOKE`], [`APPLY`]) requires
//! [`CAP_ADMIN`], which no grant may hold ([`crate::grants::CAP_GONK_ADMIN`]): only the owner's
//! socket (root) satisfies it, so an approval never comes from the passkey door or a QUIC client
//! (Brian, 2026-10-22: the controller key lives on plasma only). The reads ([`PENDING`],
//! [`STATUS`], `check`, `invite:{id}`, `kind:{kind}`) require [`CAP_READ`], which a passkey's grant
//! may carry for the page.
//!
//! **The code is the pairing code's ONLY copy, and it is not in the store.** The brief asked for
//! the HASH of the code in the invite, and that is there (`onb:codeHash`); but the proof a node
//! posts is `HMAC-SHA256(key = code, message = fingerprint)` (arc C, merged), and a hash of the key
//! cannot check an HMAC. So the code itself is kept in the KEYSTORE beside the controller key
//! (`ikigai-gonk.onboard.invite.{id}`), read for the check, and deleted when the invite is used.
//!
//! **Signing** is ikigai-sign's `urn:sign:sign` over [`ikigai_layers::onboard::signing_bytes_of`]
//! (the bytes the layer host verifies), in a kernel of its own that no door binds; the private
//! key is read from the keystore for that one call. Gonk VERIFIES every entitlement it applies
//! against the configured public key, in the same way the layer host does.
//!
//! **Applying gonk's half.** For the current entitlement of each fingerprint (the newest verified
//! one, which must `ik:supersede` the one before it, as the layer host rules), its
//! `onb:gonkGrant`s (`{ledger}={read|audit|worker|write}`) become one grant, enrolled exactly as
//! `ikigai-gonk client add onb-{name} --cert <the request's certificate> --ledger …` would, under
//! the bundle and grant name `onb-{name}` (a prefix of its own, so an entitlement can never
//! replace a client an operator enrolled by hand). Idempotent by fingerprint. A revoked or expired
//! one is unenrolled and its bundle removed. ⚠ **gonk reads its TRUSTED certificates at start**
//! ([`crate::quic`]), so a newly enrolled certificate needs a restart, which [`STATUS`] says; a
//! revocation takes effect at the next call without one (`clients.json` is read per call).
//!
//! **Several kinds, one entitlement.** The grants layer's shape allows one `onb:kind` and the host
//! keeps one current entitlement per fingerprint, so an invite for `worker,task-member` is ONE
//! entitlement carrying the union of the kinds' grants, with no `onb:kind` (an invite of one kind
//! names it). Every entitlement this grants also carries `read` on the grants layer, so a node
//! whose kinds grant no layer still reads its own entitlement (the node polls for it there).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact,
    Fallback, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Space, Time,
    UriTemplate, Verb,
};
use ikigai_layers::onboard as onb;
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use serde_json::{json, Value};

use crate::layer::Door;
use crate::mount::{LazyMount, Target};

/// The config key: one line, or none (no onboarding here).
pub const KEY: &str = "gonk.onboard";
/// What every onboarding WRITE requires: invite, approve, revoke, apply. Refused as a grant
/// ([`crate::grants::CAP_GONK_ADMIN`]), so only the owner's socket holds it.
pub const CAP_ADMIN: &str = "urn:cap:gonk:onboard:admin";
/// What the onboarding READS require: pending, status, a check, an invite, a kind. A passkey's
/// grant may carry it, for the `/onboard` page.
pub const CAP_READ: &str = "urn:cap:gonk:onboard:read";

/// `urn:iki:onboard:kind:{kind}`.
pub const KIND_PREFIX: &str = "urn:iki:onboard:kind:";
/// Mint an invite (Sink).
pub const INVITE: &str = "urn:iki:onboard:invite";
/// `urn:iki:onboard:invite:{id}`.
pub const INVITE_PREFIX: &str = "urn:iki:onboard:invite:";
/// `urn:iki:onboard:check:{entry}`, `{entry}` the lobby entry's id.
pub const CHECK_PREFIX: &str = "urn:iki:onboard:check:";
/// The lobby entries waiting for the controller.
pub const PENDING: &str = "urn:iki:onboard:pending";
/// Approve one (Sink).
pub const APPROVE: &str = "urn:iki:onboard:approve";
/// Revoke one (Sink).
pub const REVOKE: &str = "urn:iki:onboard:revoke";
/// Apply the grants layer to gonk's own enrollment (Sink).
pub const APPLY: &str = "urn:iki:onboard:apply";
/// What is in force, refused, and whether a restart is pending.
pub const STATUS: &str = "urn:iki:onboard:status";
/// The store graph the invites live in.
pub const INVITES_GRAPH: &str = "urn:iki:onboard:graph:invites";

/// The lobby layer's default name.
pub const DEFAULT_LOBBY: &str = "enroll";
/// The grants layer's default name.
pub const DEFAULT_GRANTS: &str = "grants";
/// The keystore name of the controller's private key (its public half is `{name}.pub`).
pub const DEFAULT_KEY: &str = "ikigai-gonk.onboard.controller";
/// Where an invite's pairing code is kept until it is used: `{prefix}{invite id}`.
pub const INVITE_SECRET_PREFIX: &str = "ikigai-gonk.onboard.invite.";
/// How long an invite lasts.
pub const INVITE_HOURS: u64 = 24;
/// The pairing code's length.
pub const CODE_LEN: usize = 10;
/// The pairing code's alphabet: no 0/O, 1/I/L or U, so a code read aloud or retyped survives.
/// 30 symbols, so ten carry about 49 bits, for a one-use code that lives 24 hours.
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";
/// The prefix of the client bundle and grant an entitlement enrolls in gonk: `onb-{name}`.
pub const BUNDLE_PREFIX: &str = "onb-";
/// How often the serving process applies the grants layer by itself.
pub const APPLY_EVERY: Duration = Duration::from_secs(60);
/// The page.
pub const PAGE_IRI: &str = "urn:iki:gonk:page:onboard";
/// The page's path.
pub const PAGE_PATH: &str = "/onboard";

/// The most kinds one invite names.
pub const MAX_KINDS: usize = 8;
/// The most lobby or grants entries one pass reads (ten pages of the host's largest).
pub const MAX_ENTRIES: usize = 10_000;

const PAGE_LIMIT: usize = 1000;
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const PROV_ATTRIBUTED: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
const DCT_CREATED: &str = "http://purl.org/dc/terms/created";
const ONB_INVITE_CLASS: &str = "https://ikigai-rs.dev/ns/onboard#Invite";
const ONB_NAME: &str = "https://ikigai-rs.dev/ns/onboard#name";
const ONB_CODE_HASH: &str = "https://ikigai-rs.dev/ns/onboard#codeHash";
const ONB_USED_BY: &str = "https://ikigai-rs.dev/ns/onboard#usedBy";
const GRAPH_CONSTRUCT: &str = "urn:iki:store:graph-construct";
const GRAPH_UPDATE: &str = "urn:iki:store:graph-update";
const N_TRIPLES: &str = "application/n-triples";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain;charset=utf-8";
const CONTROLLER_PRIVATE: &str = "urn:iki:onboard:signer:private";
const CONTROLLER_PUBLIC: &str = "urn:iki:onboard:signer:public";

/// The wall clock, in milliseconds since the epoch: what an invite expires by.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The system clock.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// One `gonk.onboard` line, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The layer host's door, and the controller's certificates there.
    pub target: Target,
    /// The lobby layer.
    pub lobby: String,
    /// The grants layer.
    pub grants: String,
    /// The keystore name of the controller's private key.
    pub key: String,
    /// The controller's public key, SPKI PEM: what gonk verifies entitlements against.
    pub controller_key: PathBuf,
    /// The kinds file.
    pub kinds: PathBuf,
}

/// Parse a `gonk.onboard` line. `home` expands `~/`; `gonk_dir` is `<config home>/gonk`, where
/// the defaults live.
///
/// ```
/// use ikigai_gonk::onboard::{parse, DEFAULT_KEY};
/// let certs = tempfile::tempdir().unwrap();
/// let line = format!("quic://door.example:4434 {} grants=entitlements", certs.path().display());
/// let config = parse(&line, std::path::Path::new("/home/u"), std::path::Path::new("/c/gonk")).unwrap();
/// assert_eq!(config.lobby, "enroll");
/// assert_eq!(config.grants, "entitlements");
/// assert_eq!(config.key, DEFAULT_KEY);
/// assert_eq!(config.kinds, std::path::Path::new("/c/gonk/onboard/kinds.ttl"));
/// assert!(parse("http://x:1 /tmp", std::path::Path::new("/"), std::path::Path::new("/")).is_err());
/// ```
///
/// # Errors
///
/// A door that is not `quic://host:port`, a certificate directory that is not one, a word that
/// is not one of the optional ones, a layer name ikigai-layers would refuse.
pub fn parse(line: &str, home: &Path, gonk_dir: &Path) -> std::result::Result<Config, String> {
    let shape = "expected `quic://<host>:<port> <cert dir> [lobby=<layer>] [grants=<layer>] \
                 [key=<keystore name>] [controller-key=<path>] [kinds=<path>]`";
    let mut words = line.split_whitespace();
    let url = words
        .next()
        .ok_or_else(|| format!("{KEY} `{line}`: no door; {shape}"))?;
    let Some(authority) = url.strip_prefix("quic://").filter(|a| a.contains(':')) else {
        return Err(format!(
            "{KEY} `{line}`: the door is `quic://host:port`, not `{url}`: the layer host is \
             reached over QUIC as the controller"
        ));
    };
    let certs = crate::mount::expand_home(
        words
            .next()
            .ok_or_else(|| format!("{KEY} `{line}`: no certificate directory; {shape}"))?,
        home,
    );
    if !certs.is_dir() {
        return Err(format!(
            "{KEY} `{line}`: {} is not a directory (the client.crt, client.key and server.crt \
             the controller presents at the layer door)",
            certs.display()
        ));
    }
    let mut config = Config {
        target: Target::Quic {
            authority: authority.to_string(),
            certs,
        },
        lobby: DEFAULT_LOBBY.to_string(),
        grants: DEFAULT_GRANTS.to_string(),
        key: DEFAULT_KEY.to_string(),
        controller_key: gonk_dir.join("onboard").join("controller.pub.pem"),
        kinds: gonk_dir.join("onboard").join("kinds.ttl"),
    };
    for word in words {
        match word.split_once('=') {
            Some(("lobby", v)) if crate::layer::is_layer_name(v) => config.lobby = v.to_string(),
            Some(("grants", v)) if crate::layer::is_layer_name(v) => config.grants = v.to_string(),
            Some(("key", v)) if !v.is_empty() && !v.starts_with('-') => config.key = v.to_string(),
            Some(("controller-key", v)) => {
                config.controller_key = crate::mount::expand_home(v, home)
            }
            Some(("kinds", v)) => config.kinds = crate::mount::expand_home(v, home),
            _ => return Err(format!("{KEY} `{line}`: unexpected `{word}`; {shape}")),
        }
    }
    if config.lobby == config.grants {
        return Err(format!(
            "{KEY} `{line}`: the lobby and the grants layer are two layers, both `{}`",
            config.lobby
        ));
    }
    Ok(config)
}

/// What [`Onboard::new`] is built from: the door and the files, already read.
pub struct Parts {
    /// The layer host's door, as the controller.
    pub door: Arc<dyn Door>,
    /// The door's address, `host:port`, as an invite prints it for the node.
    pub door_authority: String,
    /// The door's server certificate, PEM, which an invite hands the node to pin.
    pub door_server_cert: String,
    /// The lobby layer.
    pub lobby: String,
    /// The grants layer.
    pub grants: String,
    /// The keystore: the controller's private key and the open invites' codes.
    pub keystore: Arc<dyn ikigai_secret::Backend>,
    /// The controller key's keystore name.
    pub key: String,
    /// The controller's public key, SPKI PEM.
    pub controller_key: String,
    /// The kinds file, read at every use.
    pub kinds: PathBuf,
    /// gonk's own QUIC enrollment, which apply writes.
    pub layout: crate::quic::Layout,
    /// The wall clock.
    pub clock: Clock,
}

/// The onboarding controller and gonk's apply.
pub struct Onboard {
    parts: Parts,
    /// The hub, set by [`Onboard::attach`]: the invites' store graph is read and written through it.
    hub: OnceLock<Weak<Kernel>>,
    /// Signs and verifies, in a kernel no door binds.
    signer: Kernel,
    /// Serializes approve, revoke and apply, and caches what never changes.
    book: Mutex<Book>,
    status: Mutex<Status>,
}

#[derive(Default)]
struct Book {
    /// Each grants entry's verification, by IRI: an entry never changes.
    verified: HashMap<String, std::result::Result<Verified, String>>,
}

/// One entitlement whose signature verified, with its fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The entry, `urn:layer:{grants}:entry:{id}`.
    pub iri: String,
    /// Its feed position.
    pub seq: u64,
    /// `onb:fingerprint`.
    pub fingerprint: String,
    /// `onb:principal`, `urn:collab:participant:{name}`.
    pub principal: String,
    /// `onb:kind`, when there is one.
    pub kind: Option<String>,
    /// `onb:request`, the lobby entry.
    pub request: Option<String>,
    /// `onb:layerGrant`s.
    pub layer_grants: Vec<String>,
    /// `onb:gonkGrant`s.
    pub gonk_grants: Vec<String>,
    /// `ik:supersedes`.
    pub supersedes: Option<String>,
    /// `onb:expires`.
    pub expires: Option<String>,
}

impl Verified {
    /// The participant's short name.
    pub fn name(&self) -> &str {
        self.principal
            .strip_prefix("urn:collab:participant:")
            .unwrap_or(&self.principal)
    }

    fn grants_nothing(&self) -> bool {
        self.layer_grants.is_empty() && self.gonk_grants.is_empty()
    }
}

/// What gonk decided about one grants entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The current entitlement of its fingerprint.
    InForce,
    /// A later one superseded it.
    Superseded {
        /// The one that did.
        by: String,
    },
    /// It revokes (no grant) the one before it.
    Revoked,
    /// Its `onb:expires` has passed.
    Expired,
    /// Not applied, and why.
    Refused {
        /// Why.
        reason: String,
    },
}

impl Verdict {
    fn word(&self) -> &'static str {
        match self {
            Verdict::InForce => "in force",
            Verdict::Superseded { .. } => "superseded",
            Verdict::Revoked => "revoked",
            Verdict::Expired => "expired",
            Verdict::Refused { .. } => "refused",
        }
    }
}

/// One grants entry, and the verdict on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The entry.
    pub entry: String,
    /// Its feed position.
    pub seq: u64,
    /// The fingerprint, when it verified.
    pub fingerprint: Option<String>,
    /// The participant, when it verified.
    pub principal: Option<String>,
    /// The verdict.
    pub verdict: Verdict,
}

/// What the last apply found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Every grants entry, in feed order.
    pub records: Vec<Record>,
    /// What gonk enrolled (or found enrolled) for the entitlements in force: `(bundle, fingerprint, line)`.
    pub enrolled: Vec<(String, String, String)>,
    /// What gonk unenrolled this pass.
    pub removed: Vec<String>,
    /// Whether a certificate was newly trusted since this process started, so gonk must restart.
    pub restart_pending: bool,
    /// Why the last pass could not run, when it could not.
    pub error: Option<String>,
}

impl Status {
    /// The status as JSON.
    pub fn json(&self) -> Value {
        json!({
            "records": self.records.iter().map(|r| {
                let mut v = json!({
                    "entry": r.entry,
                    "seq": r.seq,
                    "verdict": r.verdict.word(),
                    "fingerprint": r.fingerprint,
                    "principal": r.principal,
                });
                match &r.verdict {
                    Verdict::Refused { reason } => v["reason"] = json!(reason),
                    Verdict::Superseded { by } => v["by"] = json!(by),
                    _ => {}
                }
                v
            }).collect::<Vec<_>>(),
            "enrolled": self.enrolled.iter().map(|(b, f, l)| json!({"bundle": b, "fingerprint": f, "state": l})).collect::<Vec<_>>(),
            "removed": self.removed,
            "restart_pending": self.restart_pending,
            "error": self.error,
        })
    }

    /// The status as lines of text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(e) = &self.error {
            out.push_str(&format!("error: {e}\n"));
        }
        for r in &self.records {
            out.push_str(&format!(
                "{:<11} {}  {}",
                r.verdict.word(),
                r.entry,
                r.principal.as_deref().unwrap_or("-")
            ));
            match &r.verdict {
                Verdict::Refused { reason } => out.push_str(&format!("  ({reason})")),
                Verdict::Superseded { by } => out.push_str(&format!("  (by {by})")),
                _ => {}
            }
            out.push('\n');
        }
        for (bundle, fp, state) in &self.enrolled {
            out.push_str(&format!("gonk        {bundle} {}  {state}\n", short(fp)));
        }
        for line in &self.removed {
            out.push_str(&format!("gonk        {line}\n"));
        }
        if self.restart_pending {
            out.push_str(
                "restart ikigai-gonk: a certificate was newly trusted, and trusted certificates \
                 are read at startup\n",
            );
        }
        if out.is_empty() {
            out.push_str("no entitlements on the grants layer\n");
        }
        out
    }
}

/// An invite, as the store holds it (never the code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// The id, `{name}-{six random}`.
    pub id: String,
    /// The node's name.
    pub name: String,
    /// The kinds, in the order given.
    pub kinds: Vec<String>,
    /// When it stops admitting a request, `xsd:dateTime`.
    pub expires: String,
    /// The lobby entry that used it, once one was approved.
    pub used_by: Option<String>,
}

/// A minted invite: what [`INVITE`] answers, the code included, ONCE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    /// The invite.
    pub invite: Invite,
    /// The pairing code.
    pub code: String,
    /// The layer door's address.
    pub door: String,
    /// The SHA-256 of the door's server certificate, which the node pins.
    pub pin: String,
    /// The door's server certificate, PEM.
    pub server_cert: String,
}

/// One lobby entry, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The entry's id.
    pub id: String,
    /// The entry, `urn:layer:{lobby}:entry:{id}`.
    pub entry: String,
    /// `onb:fingerprint` as stated.
    pub fingerprint: Option<String>,
    /// `onb:invite`'s id.
    pub invite: Option<String>,
    /// The invite's node name, when the invite was found.
    pub name: Option<String>,
    /// The invite's kinds, when it was found.
    pub kinds: Vec<String>,
    /// `Ok(())` when it may be approved, else why not.
    pub verdict: std::result::Result<(), String>,
}

impl Checked {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "entry": self.entry,
            "fingerprint": self.fingerprint,
            "invite": self.invite,
            "name": self.name,
            "kinds": self.kinds,
            "verdict": if self.verdict.is_ok() { "pending" } else { "refused" },
            "reason": self.verdict.as_ref().err(),
            "approve": format!("ikigai-gonk onboard approve {}", self.id),
        })
    }

    fn line(&self) -> String {
        match &self.verdict {
            Ok(()) => format!(
                "pending  {}  {} ({})  {}\n         approve: ikigai-gonk onboard approve {}\n",
                self.id,
                self.name.as_deref().unwrap_or("?"),
                self.kinds.join(","),
                self.fingerprint.as_deref().map(short).unwrap_or("-"),
                self.id
            ),
            Err(why) => format!("refused  {}  {why}\n", self.id),
        }
    }
}

fn short(fp: &str) -> &str {
    &fp[..fp.len().min(crate::quic::FINGERPRINT_SHOWN)]
}

/// One subject's triples, from a Turtle document.
#[derive(Debug, Clone, Default)]
struct Node {
    triples: Vec<Triple>,
}

impl Node {
    fn literals(&self, predicate: &str) -> Vec<&str> {
        self.triples
            .iter()
            .filter(|t| t.predicate.as_str() == predicate)
            .filter_map(|t| match &t.object {
                Term::Literal(l) => Some(l.value()),
                _ => None,
            })
            .collect()
    }

    fn iris(&self, predicate: &str) -> Vec<&str> {
        self.triples
            .iter()
            .filter(|t| t.predicate.as_str() == predicate)
            .filter_map(|t| match &t.object {
                Term::NamedNode(n) => Some(n.as_str()),
                _ => None,
            })
            .collect()
    }

    fn one_literal(&self, predicate: &str) -> Option<&str> {
        match self.literals(predicate).as_slice() {
            [one] if self.count(predicate) == 1 => Some(one),
            _ => None,
        }
    }

    fn one_iri(&self, predicate: &str) -> Option<&str> {
        match self.iris(predicate).as_slice() {
            [one] if self.count(predicate) == 1 => Some(one),
            _ => None,
        }
    }

    fn count(&self, predicate: &str) -> usize {
        self.triples
            .iter()
            .filter(|t| t.predicate.as_str() == predicate)
            .count()
    }

    fn is(&self, class: &str) -> bool {
        self.iris(RDF_TYPE).contains(&class)
    }

    fn seq(&self) -> u64 {
        self.literals(&format!("{}seq", ikigai_vocab_ik()))
            .first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    /// The node as N-Triples, which is Turtle: what [`onb::signing_bytes_of`] reads.
    fn n_triples(&self) -> String {
        self.triples.iter().map(|t| format!("{t} .\n")).collect()
    }
}

fn ikigai_vocab_ik() -> &'static str {
    "https://ikigai-rs.dev/ns#"
}

/// Parse a Turtle document into its subjects (named ones; a blank node is not a subject here).
fn subjects(
    turtle: &[u8],
    format: RdfFormat,
) -> std::result::Result<BTreeMap<String, Node>, String> {
    let mut out: BTreeMap<String, Node> = BTreeMap::new();
    for quad in RdfParser::from_format(format).for_slice(turtle) {
        let quad = quad.map_err(|e| format!("not RDF: {e}"))?;
        if let NamedOrBlankNode::NamedNode(s) = &quad.subject {
            out.entry(s.as_str().to_string())
                .or_default()
                .triples
                .push(Triple::new(
                    quad.subject.clone(),
                    quad.predicate,
                    quad.object,
                ));
        }
    }
    Ok(out)
}

fn lit(value: &str) -> String {
    Literal::new_simple_literal(value).to_string()
}

fn node(iri: &str) -> std::result::Result<String, String> {
    NamedNode::new(iri)
        .map(|n| n.to_string())
        .map_err(|e| format!("`{iri}` is not an IRI: {e}"))
}

fn iri(name: &str) -> Iri {
    Iri::parse(name).unwrap_or_else(|_| panic!("a constant IRI: {name}"))
}

fn conflict(why: impl Into<String>) -> Error {
    Error::Conflict(why.into())
}

/// Whether `name` is a node name the grants layer's shape admits as a participant: lowercase
/// `[a-z0-9-]`, starting with a letter or digit, at most 48 characters.
///
/// ```
/// use ikigai_gonk::onboard::is_node_name;
/// assert!(is_node_name("mini"));
/// assert!(is_node_name("bobby-gonk"));
/// assert!(!is_node_name("-mini"));
/// assert!(!is_node_name("Mini"));
/// assert!(!is_node_name(""));
/// ```
pub fn is_node_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The proof a node posts: lowercase hex HMAC-SHA256 with the code's UTF-8 bytes as the key and
/// the fingerprint's ASCII as the message (the contract arc C's `worker.py onboard` computes).
///
/// ```
/// // RFC 4231 test case 2: key "Jefe", message "what do ya want for nothing?".
/// assert_eq!(
///     ikigai_gonk::onboard::proof("Jefe", "what do ya want for nothing?"),
///     "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
/// );
/// ```
pub fn proof(code: &str, fingerprint: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes())
        .expect("HMAC takes a key of any length");
    mac.update(fingerprint.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether `stated` (lowercase hex) is the proof of `fingerprint` under `code`, compared in
/// constant time.
fn proof_holds(code: &str, fingerprint: &str, stated: &str) -> bool {
    let Some(bytes) = hex(stated) else {
        return false;
    };
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes())
        .expect("HMAC takes a key of any length");
    mac.update(fingerprint.as_bytes());
    mac.verify_slice(&bytes).is_ok()
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `count` symbols drawn uniformly from `alphabet` (rejection sampling), from the OS source.
fn random_from(alphabet: &[u8], count: usize) -> std::result::Result<String, String> {
    let limit = 256 - (256 % alphabet.len());
    let mut out = String::with_capacity(count);
    while out.len() < count {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).map_err(|e| format!("the OS random source failed: {e}"))?;
        for b in buf {
            if (b as usize) < limit && out.len() < count {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    Ok(out)
}

/// Whether `expires` (an `xsd:dateTime` with a timezone) is at or before `now`.
fn expired(expires: &str, now: u64) -> std::result::Result<bool, String> {
    let at = oxsdatatypes::DateTime::from_str(expires)
        .map_err(|e| format!("`{expires}` is not an xsd:dateTime: {e}"))?;
    let now = oxsdatatypes::DateTime::from_str(&date_time(now)).expect("the clock's form");
    match at.partial_cmp(&now) {
        Some(order) => Ok(order != std::cmp::Ordering::Greater),
        None => Err(format!("`{expires}` carries no timezone")),
    }
}

fn date_time(millis: u64) -> String {
    ikigai_layers::time::xsd_date_time(Time::from_millis(millis))
}

/// One kind's grants, from the kinds file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Kind {
    layer_grants: Vec<String>,
    gonk_grants: Vec<String>,
    routers: Vec<String>,
    gonk_door: Option<String>,
    gonk_server_cert: Option<String>,
}

/// The fold over the grants layer: every record, and the current entitlement per fingerprint.
struct Fold {
    records: Vec<Record>,
    /// fingerprint → the current verified entitlement (in force, revoking or expired).
    current: BTreeMap<String, Verified>,
}

impl Fold {
    /// The entitlement in force for `fingerprint`: current, granting something, not expired.
    fn in_force(&self, fingerprint: &str) -> Option<&Verified> {
        let entry = &self.current.get(fingerprint)?.iri;
        self.records
            .iter()
            .any(|r| &r.entry == entry && r.verdict == Verdict::InForce)
            .then(|| &self.current[fingerprint])
    }
}

impl Onboard {
    /// Build the controller from its parts.
    pub fn new(parts: Parts) -> Arc<Onboard> {
        let signer = Signer {
            keystore: Arc::clone(&parts.keystore),
            key: parts.key.clone(),
        };
        let public = Public(parts.controller_key.clone().into_bytes());
        let keys = EndpointSpace::new()
            .bind(Exact::new(CONTROLLER_PRIVATE), signer)
            .bind(Exact::new(CONTROLLER_PUBLIC), public);
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sign::space()), Arc::new(keys)];
        Arc::new(Onboard {
            parts,
            hub: OnceLock::new(),
            signer: Kernel::new(Arc::new(Fallback::new(spaces))),
            book: Mutex::new(Book::default()),
            status: Mutex::new(Status::default()),
        })
    }

    /// The production controller for `config`: the door dialled lazily, the platform keystore,
    /// the public key and the door's server certificate read from their files now.
    ///
    /// # Errors
    ///
    /// A file that does not read.
    pub fn from_config(
        config: &Config,
        layout: crate::quic::Layout,
    ) -> std::result::Result<Arc<Onboard>, String> {
        let read = |path: &Path, what: &str| {
            std::fs::read_to_string(path).map_err(|e| {
                format!(
                    "{KEY}: {what} {} does not read: {e}{}",
                    path.display(),
                    if what == "the controller's public key" {
                        " (`ikigai-gonk onboard init` writes it)"
                    } else {
                        ""
                    }
                )
            })
        };
        let Target::Quic { authority, certs } = &config.target else {
            return Err(format!("{KEY}: the door is a QUIC door"));
        };
        let door_server_cert = read(&certs.join("server.crt"), "the layer door's server.crt")?;
        let controller_key = read(&config.controller_key, "the controller's public key")?;
        Ok(Onboard::new(Parts {
            door: Arc::new(
                LazyMount::fresh(config.target.clone())
                    .with_call_timeout(crate::layer::CALL_TIMEOUT),
            ),
            door_authority: authority.clone(),
            door_server_cert,
            lobby: config.lobby.clone(),
            grants: config.grants.clone(),
            keystore: ikigai_secret::default_backend(),
            key: config.key.clone(),
            controller_key,
            kinds: config.kinds.clone(),
            layout,
            clock: system_clock(),
        }))
    }

    /// Give the controller the hub, which holds the invites' store graph.
    pub fn attach(&self, hub: &Arc<Kernel>) {
        let _ = self.hub.set(Arc::downgrade(hub));
    }

    /// Apply the grants layer every [`APPLY_EVERY`], on a thread of its own: what `serve` starts.
    pub fn start(self: &Arc<Self>) {
        let onboard = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("gonk-onboard".to_string())
            .spawn(move || loop {
                let status = onboard.apply();
                if let Some(e) = &status.error {
                    eprintln!("ikigai-gonk: onboard apply: {e}");
                }
                std::thread::sleep(APPLY_EVERY);
            });
    }

    /// The lobby layer.
    pub fn lobby(&self) -> &str {
        &self.parts.lobby
    }

    /// The grants layer.
    pub fn grants(&self) -> &str {
        &self.parts.grants
    }

    /// The last status.
    pub fn status(&self) -> Status {
        self.status.lock().expect("status lock").clone()
    }

    // ---- the store: invites ----

    fn hub(&self) -> std::result::Result<Arc<Kernel>, String> {
        self.hub
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "onboard: no hub attached".to_string())
    }

    fn store_cap() -> Capability {
        Capability::scoped([
            ikigai_store::cap_read_graph(INVITES_GRAPH),
            ikigai_store::cap_write_graph(INVITES_GRAPH),
        ])
    }

    fn invites(&self) -> std::result::Result<BTreeMap<String, Invite>, String> {
        let query =
            format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{INVITES_GRAPH}> {{ ?s ?p ?o }} }}");
        let read = Request::new(Verb::Source, iri(GRAPH_CONSTRUCT))
            .with_arg("graph", ArgRef::Inline(INVITES_GRAPH.as_bytes().to_vec()))
            .with_arg("query", ArgRef::Inline(query.into_bytes()))
            .with_arg("as", ArgRef::Inline(N_TRIPLES.as_bytes().to_vec()));
        let answer = ikigai_resolve::Resolver::issue_as(&*self.hub()?, read, &Self::store_cap())
            .map(|(r, _)| r)
            .map_err(|e| format!("the invites graph does not read: {e}"))?;
        let mut out = BTreeMap::new();
        for (subject, node) in subjects(&answer.bytes, RdfFormat::NTriples)? {
            let Some(id) = subject.strip_prefix(INVITE_PREFIX) else {
                continue;
            };
            if !node.is(ONB_INVITE_CLASS) {
                continue;
            }
            out.insert(
                id.to_string(),
                Invite {
                    id: id.to_string(),
                    name: node.one_literal(ONB_NAME).unwrap_or_default().to_string(),
                    kinds: node
                        .iris(onb::KIND)
                        .iter()
                        .filter_map(|k| k.strip_prefix(KIND_PREFIX))
                        .map(str::to_string)
                        .collect(),
                    expires: node
                        .one_literal(onb::EXPIRES)
                        .unwrap_or_default()
                        .to_string(),
                    used_by: node.iris(ONB_USED_BY).first().map(|s| s.to_string()),
                },
            );
        }
        Ok(out)
    }

    fn store_update(&self, update: String) -> std::result::Result<(), String> {
        let write = Request::new(Verb::Sink, iri(GRAPH_UPDATE))
            .with_arg("graph", ArgRef::Inline(INVITES_GRAPH.as_bytes().to_vec()))
            .with_arg("content", ArgRef::Inline(update.into_bytes()));
        ikigai_resolve::Resolver::issue_as(&*self.hub()?, write, &Self::store_cap())
            .map(|_| ())
            .map_err(|e| format!("the invites graph does not write: {e}"))
    }

    // ---- kinds ----

    fn kinds(&self) -> std::result::Result<BTreeMap<String, Kind>, String> {
        let text = std::fs::read(&self.parts.kinds).map_err(|e| {
            format!(
                "the kinds file {} does not read: {e}",
                self.parts.kinds.display()
            )
        })?;
        let mut out = BTreeMap::new();
        for (subject, node) in subjects(&text, RdfFormat::Turtle)
            .map_err(|e| format!("{}: {e}", self.parts.kinds.display()))?
        {
            let Some(name) = subject.strip_prefix(KIND_PREFIX) else {
                continue;
            };
            out.insert(
                name.to_string(),
                Kind {
                    layer_grants: node
                        .iris(onb::LAYER_GRANT)
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    gonk_grants: node
                        .literals(onb::GONK_GRANT)
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    routers: node
                        .literals(onb::ROUTER)
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    gonk_door: node.one_literal(onb::GONK_DOOR).map(str::to_string),
                    gonk_server_cert: node.one_literal(onb::GONK_SERVER_CERT).map(str::to_string),
                },
            );
        }
        Ok(out)
    }

    fn kind_turtle(&self, name: &str) -> Result<String> {
        let text = std::fs::read(&self.parts.kinds).map_err(|e| {
            Error::Endpoint(format!(
                "the kinds file {} does not read: {e}",
                self.parts.kinds.display()
            ))
        })?;
        let all = subjects(&text, RdfFormat::Turtle).map_err(Error::Endpoint)?;
        let subject = format!("{KIND_PREFIX}{name}");
        all.get(&subject).map(Node::n_triples).ok_or_else(|| {
            Error::NotFound(format!(
                "no kind `{name}` in {}",
                self.parts.kinds.display()
            ))
        })
    }

    // ---- invite ----

    /// Mint an invite for the node `name` of `kinds`: its id, its code (returned here and
    /// nowhere else), the door and the door's certificate to pin.
    ///
    /// # Errors
    ///
    /// A name or kind the contract refuses, a keystore or store that does not write.
    pub fn invite(&self, name: &str, kinds: &[String]) -> std::result::Result<Minted, Error> {
        let bad = |name: &str, detail: String| Error::InvalidArgument {
            name: name.to_string(),
            detail,
        };
        if !is_node_name(name) {
            return Err(bad(
                "name",
                format!(
                    "`{name}` is not a node name (lowercase letters, digits and `-`, at most 48)"
                ),
            ));
        }
        if kinds.is_empty() || kinds.len() > MAX_KINDS {
            return Err(bad(
                "kind",
                format!("one to {MAX_KINDS} kinds, comma-separated"),
            ));
        }
        let known = self.kinds().map_err(Error::Endpoint)?;
        for kind in kinds {
            if !known.contains_key(kind) {
                return Err(bad(
                    "kind",
                    format!(
                        "no kind `{kind}` in {} (it has: {})",
                        self.parts.kinds.display(),
                        known.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
        let id = format!(
            "{name}-{}",
            random_from(b"abcdefghijkmnpqrstvwxyz23456789", 6).map_err(Error::Endpoint)?
        );
        let code = random_from(CODE_ALPHABET, CODE_LEN).map_err(Error::Endpoint)?;
        let now = (self.parts.clock)();
        let expires = date_time(now + INVITE_HOURS * 60 * 60 * 1000);
        self.parts
            .keystore
            .set(&format!("{INVITE_SECRET_PREFIX}{id}"), code.as_bytes())?;
        let subject = node(&format!("{INVITE_PREFIX}{id}")).map_err(Error::Endpoint)?;
        let mut triples = vec![
            format!("{subject} <{RDF_TYPE}> <{ONB_INVITE_CLASS}> ."),
            format!("{subject} <{ONB_NAME}> {} .", lit(name)),
            format!(
                "{subject} <{}> \"{expires}\"^^<{XSD_DATE_TIME}> .",
                onb::EXPIRES
            ),
            format!(
                "{subject} <{DCT_CREATED}> \"{}\"^^<{XSD_DATE_TIME}> .",
                date_time(now)
            ),
            format!(
                "{subject} <{ONB_CODE_HASH}> {} .",
                lit(&sha256_hex(code.as_bytes()))
            ),
        ];
        for kind in kinds {
            triples.push(format!(
                "{subject} <{}> {} .",
                onb::KIND,
                node(&format!("{KIND_PREFIX}{kind}")).map_err(Error::Endpoint)?
            ));
        }
        self.store_update(format!(
            "INSERT DATA {{ GRAPH <{INVITES_GRAPH}> {{\n{}\n}} }}",
            triples.join("\n")
        ))
        .map_err(Error::Endpoint)?;
        let pin = ikigai_quic::fingerprint_of_pem(&self.parts.door_server_cert)
            .map_err(|e| Error::Endpoint(format!("the layer door's server.crt: {e}")))?;
        Ok(Minted {
            invite: Invite {
                id,
                name: name.to_string(),
                kinds: kinds.to_vec(),
                expires,
                used_by: None,
            },
            code,
            door: self.parts.door_authority.clone(),
            pin,
            server_cert: self.parts.door_server_cert.clone(),
        })
    }

    // ---- the layers, through the door ----

    fn call(&self, request: Request) -> std::result::Result<Vec<u8>, String> {
        let target = request.target.to_string();
        self.parts
            .door
            .call(request)
            .map(|r| r.bytes)
            .map_err(|e| format!("{target}: {e}"))
    }

    /// Every entry of `layer` of `class`, in feed order: `(iri, node)`.
    fn entries(
        &self,
        layer: &str,
        class: &str,
    ) -> std::result::Result<Vec<(String, Node)>, String> {
        let mut out: Vec<(String, Node)> = Vec::new();
        let mut after = 0u64;
        loop {
            let request =
                crate::layer::request(Verb::Source, &format!("urn:layer:{layer}:entries"))
                    .with_arg("type", ArgRef::Inline(class.as_bytes().to_vec()))
                    .with_arg("after", ArgRef::Inline(after.to_string().into_bytes()))
                    .with_arg("limit", ArgRef::Inline(PAGE_LIMIT.to_string().into_bytes()));
            let bytes = self.call(request)?;
            let mut page: Vec<(String, Node)> = subjects(&bytes, RdfFormat::Turtle)?
                .into_iter()
                .filter(|(s, _)| s.starts_with(&format!("urn:layer:{layer}:entry:")))
                .collect();
            page.sort_by_key(|(_, n)| n.seq());
            let n = page.len();
            if let Some((_, last)) = page.last() {
                after = last.seq();
            }
            out.extend(page);
            if n < PAGE_LIMIT {
                break;
            }
            if out.len() >= MAX_ENTRIES {
                return Err(format!(
                    "layer `{layer}` holds more than {MAX_ENTRIES} entries of {class}; refusing \
                     to read further in one pass"
                ));
            }
        }
        Ok(out)
    }

    fn lobby_entry(&self, id: &str) -> std::result::Result<(String, Node), String> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(format!("`{id}` is not a lobby entry id"));
        }
        let entry = format!("urn:layer:{}:entry:{id}", self.parts.lobby);
        let bytes = self.call(crate::layer::request(Verb::Source, &entry))?;
        let node = subjects(&bytes, RdfFormat::Turtle)?
            .remove(&entry)
            .ok_or_else(|| format!("{entry} is not in its own answer"))?;
        Ok((entry, node))
    }

    // ---- check ----

    fn check_node(
        &self,
        entry: &str,
        node: &Node,
        invites: &BTreeMap<String, Invite>,
        now: u64,
    ) -> Checked {
        let id = entry.rsplit(':').next().unwrap_or_default().to_string();
        let fingerprint = node.one_literal(onb::FINGERPRINT).map(str::to_string);
        let invite_id = node
            .one_iri(onb::INVITE)
            .and_then(|i| i.strip_prefix(INVITE_PREFIX))
            .map(str::to_string);
        let found = invite_id.as_ref().and_then(|i| invites.get(i));
        let verdict = self.verdict(entry, node, invite_id.as_deref(), found, now);
        Checked {
            id,
            entry: entry.to_string(),
            fingerprint,
            invite: invite_id,
            name: found.map(|i| i.name.clone()),
            kinds: found.map(|i| i.kinds.clone()).unwrap_or_default(),
            verdict,
        }
    }

    /// The check, in the contract's order: the invite exists, is unexpired and unused; the HMAC
    /// matches the invite's code; the fingerprint is the certificate's, recomputed here.
    fn verdict(
        &self,
        entry: &str,
        node: &Node,
        invite_id: Option<&str>,
        invite: Option<&Invite>,
        now: u64,
    ) -> std::result::Result<(), String> {
        if !node.is(onb::ENROLLMENT_REQUEST) {
            return Err("not an onb:EnrollmentRequest".into());
        }
        let invite_id =
            invite_id.ok_or("no single onb:invite naming urn:iki:onboard:invite:{id}")?;
        let invite =
            invite.ok_or_else(|| format!("no invite `{invite_id}` (never minted here)"))?;
        if let Some(by) = &invite.used_by {
            if by != entry {
                return Err(format!("invite `{invite_id}` was already used, by {by}"));
            }
        }
        if expired(&invite.expires, now)? {
            return Err(format!(
                "invite `{invite_id}` expired at {}",
                invite.expires
            ));
        }
        let fingerprint = node
            .one_literal(onb::FINGERPRINT)
            .ok_or("no single onb:fingerprint")?;
        let stated = node.one_literal(onb::PROOF).ok_or("no single onb:proof")?;
        let code = self
            .parts
            .keystore
            .get(&format!("{INVITE_SECRET_PREFIX}{invite_id}"))
            .map_err(|e| format!("the keystore: {e}"))?
            .ok_or_else(|| {
                format!("invite `{invite_id}`'s code is no longer kept (it was used or withdrawn)")
            })?;
        let code = String::from_utf8(code).map_err(|_| "the kept code is not UTF-8")?;
        if !proof_holds(&code, fingerprint, stated) {
            return Err(format!(
                "the proof is not HMAC-SHA256 of the fingerprint under invite `{invite_id}`'s \
                 pairing code"
            ));
        }
        let pem = node
            .one_literal(onb::CERTIFICATE)
            .ok_or("no single onb:certificate")?;
        let actual = ikigai_quic::fingerprint_of_pem(pem)
            .map_err(|e| format!("onb:certificate is not a PEM certificate: {e}"))?;
        if actual != fingerprint {
            return Err(format!(
                "onb:certificate hashes to {actual}, not to its onb:fingerprint {fingerprint}"
            ));
        }
        if let Some(author) = node.one_iri(PROV_ATTRIBUTED) {
            if let Some((_, fp)) = author.rsplit_once(":client:") {
                if fp != fingerprint {
                    return Err(format!(
                        "said by {author}, which is not the certificate it enrolls"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Check one lobby entry, by its id.
    ///
    /// # Errors
    ///
    /// The entry or the invites do not read.
    pub fn check(&self, id: &str) -> std::result::Result<Checked, String> {
        let (entry, node) = self.lobby_entry(id)?;
        Ok(self.check_node(&entry, &node, &self.invites()?, (self.parts.clock)()))
    }

    /// The lobby entries whose fingerprint no entitlement names yet, each checked.
    ///
    /// # Errors
    ///
    /// A layer or the store does not read.
    pub fn pending(&self) -> std::result::Result<Vec<Checked>, String> {
        let fold = self.fold()?;
        let entitled: BTreeSet<String> = fold
            .records
            .iter()
            .filter_map(|r| r.fingerprint.clone())
            .collect();
        let invites = self.invites()?;
        let now = (self.parts.clock)();
        Ok(self
            .entries(&self.parts.lobby, onb::ENROLLMENT_REQUEST)?
            .iter()
            .filter(|(_, n)| {
                n.one_literal(onb::FINGERPRINT)
                    .is_none_or(|fp| !entitled.contains(fp))
            })
            .map(|(entry, n)| self.check_node(entry, n, &invites, now))
            .collect())
    }

    // ---- the grants layer, verified and folded ----

    fn verify(&self, bytes: Vec<u8>, sig: &str) -> std::result::Result<(), String> {
        let request = Request::new(Verb::Source, iri("urn:sign:verify"))
            .with_arg("in", ArgRef::Inline(bytes))
            .with_arg("sig", ArgRef::Inline(sig.as_bytes().to_vec()))
            .with_arg("key", ArgRef::Inline(CONTROLLER_PUBLIC.as_bytes().to_vec()));
        let answer = ikigai_resolve::Resolver::issue_as(
            &self.signer,
            request,
            &Capability::scoped(Vec::<String>::new()),
        )
        .map(|(r, _)| r)
        .map_err(|e| format!("the signature does not verify ({e})"))?;
        let text = String::from_utf8_lossy(&answer.bytes);
        if text.starts_with("valid:") {
            Ok(())
        } else {
            Err(format!(
                "the signature does not verify against the controller key ({})",
                text.trim()
            ))
        }
    }

    fn sign(&self, bytes: Vec<u8>) -> std::result::Result<String, String> {
        let request = Request::new(Verb::Source, iri("urn:sign:sign"))
            .with_arg("in", ArgRef::Inline(bytes))
            .with_arg(
                "key",
                ArgRef::Inline(CONTROLLER_PRIVATE.as_bytes().to_vec()),
            );
        let graph = ikigai_resolve::Resolver::issue_as(
            &self.signer,
            request,
            &Capability::scoped([ikigai_sign::CAP_SIGN]),
        )
        .map(|(r, _)| r)
        .map_err(|e| format!("signing: {e}"))?;
        String::from_utf8(graph.bytes).map_err(|_| "the signature graph is not UTF-8".to_string())
    }

    fn check_entitlement(&self, iri: &str, node: &Node) -> std::result::Result<Verified, String> {
        if !node.is(onb::ENTITLEMENT) {
            return Err("not an onb:Entitlement".into());
        }
        let signature = node
            .one_literal(onb::SIGNATURE)
            .ok_or("no single onb:signature: an entitlement is signed by the controller")?;
        let bytes = onb::signing_bytes_of(&node.n_triples(), iri)?;
        self.verify(bytes, signature)?;
        let fingerprint = node
            .one_literal(onb::FINGERPRINT)
            .and_then(ikigai_layers::name::fold_fingerprint)
            .ok_or("no single onb:fingerprint of 64 hex digits")?;
        let principal = node
            .one_iri(onb::PRINCIPAL)
            .filter(|p| {
                p.strip_prefix("urn:collab:participant:")
                    .is_some_and(is_node_name)
            })
            .ok_or("no single onb:principal urn:collab:participant:{name}")?
            .to_string();
        if node.count(onb::EXPIRES) > 1 {
            return Err("more than one onb:expires".into());
        }
        Ok(Verified {
            iri: iri.to_string(),
            seq: node.seq(),
            fingerprint,
            principal,
            kind: node.one_iri(onb::KIND).map(str::to_string),
            request: node.one_iri(onb::REQUEST).map(str::to_string),
            layer_grants: node
                .iris(onb::LAYER_GRANT)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            gonk_grants: node
                .literals(onb::GONK_GRANT)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            supersedes: node
                .one_iri(&format!("{}supersedes", ikigai_vocab_ik()))
                .map(str::to_string),
            expires: node.one_literal(onb::EXPIRES).map(str::to_string),
        })
    }

    /// Read, verify and fold the grants layer, as the layer host rules: per fingerprint the
    /// newest verified entitlement is current, and it must supersede the one before it.
    fn fold(&self) -> std::result::Result<Fold, String> {
        let entries = self.entries(&self.parts.grants, onb::ENTITLEMENT)?;
        let mut book = self.book.lock().expect("book lock");
        for (iri, node) in &entries {
            if !book.verified.contains_key(iri) {
                let v = self.check_entitlement(iri, node);
                book.verified.insert(iri.clone(), v);
            }
        }
        let now = (self.parts.clock)();
        let mut records: Vec<Record> = Vec::new();
        let mut current: BTreeMap<String, Verified> = BTreeMap::new();
        for (iri, node) in &entries {
            let seq = node.seq();
            let v = match &book.verified[iri] {
                Ok(v) => v.clone(),
                Err(reason) => {
                    records.push(Record {
                        entry: iri.clone(),
                        seq,
                        fingerprint: None,
                        principal: None,
                        verdict: Verdict::Refused {
                            reason: reason.clone(),
                        },
                    });
                    continue;
                }
            };
            let record = |verdict| Record {
                entry: iri.clone(),
                seq,
                fingerprint: Some(v.fingerprint.clone()),
                principal: Some(v.principal.clone()),
                verdict,
            };
            let refusal = match (current.get(&v.fingerprint), &v.supersedes) {
                (Some(cur), sup) if sup.as_deref() != Some(cur.iri.as_str()) => Some(format!(
                    "fingerprint {} is already entitled by {}, which this does not supersede (a \
                     replay, or a stale controller)",
                    short(&v.fingerprint),
                    cur.iri
                )),
                (None, Some(sup)) => Some(format!(
                    "supersedes {sup}, which is not the current entitlement of fingerprint {}",
                    short(&v.fingerprint)
                )),
                (None, None) if v.grants_nothing() => {
                    Some("an entitlement with no grant revokes, and this supersedes nothing".into())
                }
                _ => None,
            };
            if let Some(reason) = refusal {
                records.push(record(Verdict::Refused { reason }));
                continue;
            }
            if let Some(sup) = &v.supersedes {
                for r in records.iter_mut().filter(|r| &r.entry == sup) {
                    if !matches!(r.verdict, Verdict::Refused { .. }) {
                        r.verdict = Verdict::Superseded { by: iri.clone() };
                    }
                }
            }
            let verdict = if v.grants_nothing() {
                Verdict::Revoked
            } else {
                match v.expires.as_deref().map(|e| expired(e, now)) {
                    None | Some(Ok(false)) => Verdict::InForce,
                    Some(Ok(true)) => Verdict::Expired,
                    Some(Err(reason)) => Verdict::Refused { reason },
                }
            };
            records.push(record(verdict));
            current.insert(v.fingerprint.clone(), v);
        }
        Ok(Fold { records, current })
    }

    // ---- approve and revoke ----

    /// Approve the lobby entry `id`: check it, build the entitlement from its invite's kinds,
    /// sign it under the controller key, say it on the grants layer (superseding the current one
    /// for its fingerprint), mark the invite used, and apply. Answers the entitlement's entry.
    ///
    /// # Errors
    ///
    /// A check that does not pass (a [`Error::Conflict`] naming why), a key that is missing, a
    /// layer that refuses.
    pub fn approve(&self, id: &str) -> std::result::Result<(String, Status), Error> {
        let (entry, request) = self.lobby_entry(id).map_err(Error::Endpoint)?;
        let invites = self.invites().map_err(Error::Endpoint)?;
        let checked = self.check_node(&entry, &request, &invites, (self.parts.clock)());
        checked
            .verdict
            .clone()
            .map_err(|why| conflict(format!("{entry} is not approved: {why}")))?;
        let invite = invites[checked.invite.as_deref().unwrap_or_default()].clone();
        if invite.used_by.is_some() {
            return Err(conflict(format!("{entry} was approved already")));
        }
        let fingerprint = checked.fingerprint.clone().unwrap_or_default();
        let kinds = self.kinds().map_err(Error::Endpoint)?;
        let mut layer_grants: BTreeSet<String> = BTreeSet::new();
        let mut gonk_grants: BTreeSet<String> = BTreeSet::new();
        let mut routers: BTreeSet<String> = BTreeSet::new();
        let (mut gonk_door, mut gonk_cert) = (None, None);
        for name in &invite.kinds {
            let kind = kinds.get(name).ok_or_else(|| {
                conflict(format!(
                    "invite `{}` names kind `{name}`, which {} no longer defines",
                    invite.id,
                    self.parts.kinds.display()
                ))
            })?;
            layer_grants.extend(kind.layer_grants.iter().cloned());
            gonk_grants.extend(kind.gonk_grants.iter().cloned());
            routers.extend(kind.routers.iter().cloned());
            gonk_door = gonk_door.or(kind.gonk_door.clone());
            gonk_cert = gonk_cert.or(kind.gonk_server_cert.clone());
        }
        // The node reads its own entitlement on the grants layer, whatever its kinds grant.
        layer_grants.insert(ikigai_layers::name::layer_cap("read", &self.parts.grants));
        for grant in &gonk_grants {
            gonk_scopes(grant)
                .map_err(|e| conflict(format!("kind's onb:gonkGrant `{grant}`: {e}")))?;
        }
        if !gonk_grants.is_empty() && gonk_cert.is_none() {
            gonk_cert = std::fs::read_to_string(self.parts.layout.server_cert()).ok();
        }
        let fold = self.fold().map_err(Error::Endpoint)?;
        let mut body = vec![
            format!("<{RDF_TYPE}> <{}>", onb::ENTITLEMENT),
            format!("<{}> {}", onb::FINGERPRINT, lit(&fingerprint)),
            format!(
                "<{}> {}",
                onb::PRINCIPAL,
                node(&format!("urn:collab:participant:{}", invite.name))
                    .map_err(Error::Endpoint)?
            ),
            format!(
                "<{}> {}",
                onb::REQUEST,
                node(&entry).map_err(Error::Endpoint)?
            ),
        ];
        if let [one] = invite.kinds.as_slice() {
            body.push(format!(
                "<{}> {}",
                onb::KIND,
                node(&format!("{KIND_PREFIX}{one}")).map_err(Error::Endpoint)?
            ));
        }
        for g in &layer_grants {
            body.push(format!(
                "<{}> {}",
                onb::LAYER_GRANT,
                node(g).map_err(Error::Endpoint)?
            ));
        }
        for g in &gonk_grants {
            body.push(format!("<{}> {}", onb::GONK_GRANT, lit(g)));
        }
        for r in &routers {
            body.push(format!("<{}> {}", onb::ROUTER, lit(r)));
        }
        if let Some(d) = &gonk_door {
            body.push(format!("<{}> {}", onb::GONK_DOOR, lit(d)));
        }
        if let Some(c) = &gonk_cert {
            body.push(format!("<{}> {}", onb::GONK_SERVER_CERT, lit(c)));
        }
        if let Some(cur) = fold.current.get(&fingerprint) {
            body.push(format!(
                "<{}supersedes> {}",
                ikigai_vocab_ik(),
                node(&cur.iri).map_err(Error::Endpoint)?
            ));
        }
        let said = self.say_signed(&body).map_err(Error::Endpoint)?;
        // The invite is spent: marked used in the store, its code forgotten.
        self.store_update(format!(
            "INSERT DATA {{ GRAPH <{INVITES_GRAPH}> {{ {} <{ONB_USED_BY}> {} . }} }}",
            node(&format!("{INVITE_PREFIX}{}", invite.id)).map_err(Error::Endpoint)?,
            node(&entry).map_err(Error::Endpoint)?
        ))
        .map_err(Error::Endpoint)?;
        self.parts
            .keystore
            .delete(&format!("{INVITE_SECRET_PREFIX}{}", invite.id))?;
        Ok((said, self.apply()))
    }

    /// Revoke the entitlement in force for `who` (a fingerprint, or a node name): a signed
    /// entitlement with no grant, superseding it.
    ///
    /// # Errors
    ///
    /// Nothing in force for `who`, a missing key, a layer that refuses.
    pub fn revoke(&self, who: &str) -> std::result::Result<(String, Status), Error> {
        let fold = self.fold().map_err(Error::Endpoint)?;
        let folded = ikigai_layers::name::fold_fingerprint(who);
        let current: Vec<&Verified> = fold
            .current
            .values()
            .filter(|v| match &folded {
                Some(fp) => &v.fingerprint == fp,
                None => v.name() == who,
            })
            .filter(|v| !v.grants_nothing())
            .collect();
        let v = match current.as_slice() {
            [one] => (*one).clone(),
            [] => {
                return Err(Error::NotFound(format!(
                    "no entitlement in force names `{who}` (a fingerprint or a node name)"
                )))
            }
            many => {
                return Err(conflict(format!(
                    "`{who}` names {} entitlements in force; revoke one by fingerprint: {}",
                    many.len(),
                    many.iter()
                        .map(|v| v.fingerprint.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        };
        let mut body = vec![
            format!("<{RDF_TYPE}> <{}>", onb::ENTITLEMENT),
            format!("<{}> {}", onb::FINGERPRINT, lit(&v.fingerprint)),
            format!(
                "<{}> {}",
                onb::PRINCIPAL,
                node(&v.principal).map_err(Error::Endpoint)?
            ),
            format!(
                "<{}supersedes> {}",
                ikigai_vocab_ik(),
                node(&v.iri).map_err(Error::Endpoint)?
            ),
        ];
        if let Some(kind) = &v.kind {
            body.push(format!(
                "<{}> {}",
                onb::KIND,
                node(kind).map_err(Error::Endpoint)?
            ));
        }
        let said = self.say_signed(&body).map_err(Error::Endpoint)?;
        Ok((said, self.apply()))
    }

    /// Sign `body` (predicate-object pairs about `<urn:layer:new>`) and say it on the grants
    /// layer. Answers what the host named the entry.
    fn say_signed(&self, body: &[String]) -> std::result::Result<String, String> {
        let unsigned: String = body
            .iter()
            .map(|po| format!("<urn:layer:new> {po} .\n"))
            .collect();
        let bytes = onb::signing_bytes_of(&unsigned, "urn:layer:new")?;
        let signature = self.sign(bytes.clone())?;
        // Checked against the CONFIGURED public key before anything is said: a private key in
        // the keystore that is not the configured key's would make an entitlement every door
        // refuses.
        self.verify(bytes, &signature).map_err(|e| {
            format!(
                "{e}: the keystore's `{}` is not the private half of the configured controller \
                 key",
                self.parts.key
            )
        })?;
        let signed = format!(
            "{unsigned}<urn:layer:new> <{}> {} .\n",
            onb::SIGNATURE,
            lit(&signature)
        );
        let request =
            crate::layer::request(Verb::Sink, &format!("urn:layer:{}:say", self.parts.grants))
                .with_arg("content", ArgRef::Inline(signed.into_bytes()));
        let answer = self.call(request)?;
        let text = String::from_utf8_lossy(&answer).into_owned();
        Ok(first_entry(&text, &self.parts.grants).unwrap_or(text.trim().to_string()))
    }

    // ---- apply: gonk's half ----

    /// Apply the grants layer to gonk's own QUIC enrollment: enroll each entitlement in force
    /// that names a gonk grant, unenroll each one revoked or expired. Idempotent by fingerprint.
    /// Never fails: what went wrong is in the status.
    pub fn apply(&self) -> Status {
        let status = match self.fold() {
            Ok(fold) => self.apply_fold(fold),
            Err(e) => Status {
                error: Some(e),
                ..self.status()
            },
        };
        let mut kept = self.status.lock().expect("status lock");
        let restart = kept.restart_pending || status.restart_pending;
        *kept = Status {
            restart_pending: restart,
            ..status
        };
        kept.clone()
    }

    fn apply_fold(&self, fold: Fold) -> Status {
        let layout = &self.parts.layout;
        let mut status = Status {
            records: fold.records.clone(),
            ..Status::default()
        };
        let enrolment = crate::quic::read_enrolment(&layout.clients_json())
            .ok()
            .flatten()
            .unwrap_or_default();
        for (fp, v) in &fold.current {
            let bundle = format!("{BUNDLE_PREFIX}{}", v.name());
            let wanted = fold.in_force(fp).filter(|v| !v.gonk_grants.is_empty());
            match wanted {
                Some(v) => match self.enroll(v, &bundle) {
                    Ok((line, newly)) => {
                        status.restart_pending |= newly;
                        status.enrolled.push((bundle, fp.clone(), line));
                    }
                    Err(reason) => {
                        for r in status.records.iter_mut().filter(|r| r.entry == v.iri) {
                            r.verdict = Verdict::Refused {
                                reason: format!("gonk's half: {reason}"),
                            };
                        }
                    }
                },
                None => {
                    let ours = enrolment
                        .grant_for(fp)
                        .is_some_and(|g| g.starts_with(BUNDLE_PREFIX));
                    if ours {
                        match self.unenroll(fp, &bundle) {
                            Ok(line) => status.removed.push(line),
                            Err(e) => status.error = Some(e),
                        }
                    }
                }
            }
        }
        status
    }

    /// Enroll `v`'s certificate under `bundle` with its gonk grants. `Ok((state, newly))`, where
    /// `newly` says the certificate was not trusted before (so a restart is needed).
    fn enroll(&self, v: &Verified, bundle: &str) -> std::result::Result<(String, bool), String> {
        let layout = &self.parts.layout;
        let mut scopes: Vec<String> = Vec::new();
        for grant in &v.gonk_grants {
            for s in gonk_scopes(grant)? {
                if !scopes.contains(&s) {
                    scopes.push(s);
                }
            }
        }
        let crt = layout.clients_dir().join(bundle).join("client.crt");
        let existing = std::fs::read_to_string(&crt)
            .ok()
            .and_then(|pem| ikigai_quic::fingerprint_of_pem(&pem).ok());
        let enrolment = crate::quic::read_enrolment(&layout.clients_json())?.unwrap_or_default();
        let grants = crate::quic::read_grants(&layout.grants_json()).unwrap_or_default();
        if existing.as_deref() == Some(v.fingerprint.as_str())
            && enrolment.grant_for(&v.fingerprint) == Some(bundle)
            && grants.get(bundle) == Some(&scopes)
        {
            return Ok(("in force, unchanged".to_string(), false));
        }
        crate::quic::enrol_refusal(layout, bundle, Some(&v.fingerprint), &scopes, true)?;
        let mut newly = false;
        if existing.as_deref() != Some(v.fingerprint.as_str()) {
            let pem = self.certificate_of(v)?;
            let tmp = layout
                .quic_dir()
                .join(format!(".onboard-{}.crt", short(&v.fingerprint)));
            std::fs::create_dir_all(layout.quic_dir())
                .map_err(|e| format!("{}: {e}", layout.quic_dir().display()))?;
            std::fs::write(&tmp, &pem).map_err(|e| format!("{}: {e}", tmp.display()))?;
            let added = crate::quic::add_client(layout, bundle, Some(&tmp), existing.is_some());
            let _ = std::fs::remove_file(&tmp);
            let added = added?;
            if let Some(old) = &added.replaced {
                crate::quic::unenrol(layout, old)?;
            }
            newly = true;
        }
        crate::quic::enrol(layout, bundle, &v.fingerprint, &scopes, true)?;
        Ok((
            format!(
                "enrolled under grant `{bundle}` ({} scopes){}",
                scopes.len(),
                if newly {
                    "; restart ikigai-gonk to trust the certificate"
                } else {
                    ""
                }
            ),
            newly,
        ))
    }

    fn unenroll(&self, fp: &str, bundle: &str) -> std::result::Result<String, String> {
        let layout = &self.parts.layout;
        crate::quic::unenrol(layout, fp)?;
        let crt = layout.clients_dir().join(bundle).join("client.crt");
        let holds = std::fs::read_to_string(&crt)
            .ok()
            .and_then(|pem| ikigai_quic::fingerprint_of_pem(&pem).ok())
            .is_some_and(|f| f == fp);
        if holds {
            std::fs::remove_dir_all(layout.clients_dir().join(bundle))
                .map_err(|e| format!("removing bundle {bundle}: {e}"))?;
        }
        Ok(format!(
            "unenrolled {}{}",
            short(fp),
            if holds {
                format!(" and removed bundle {bundle}")
            } else {
                String::new()
            }
        ))
    }

    /// The certificate an entitlement enrolls, from the lobby request it names, checked again
    /// against its fingerprint.
    fn certificate_of(&self, v: &Verified) -> std::result::Result<String, String> {
        let request = v
            .request
            .as_deref()
            .ok_or("no onb:request: an entitlement that grants names the request it grants")?;
        let id = request
            .strip_prefix(&format!("urn:layer:{}:entry:", self.parts.lobby))
            .ok_or_else(|| format!("onb:request {request} is not an entry of this lobby"))?;
        let (_, node) = self.lobby_entry(id)?;
        let pem = node
            .one_literal(onb::CERTIFICATE)
            .ok_or_else(|| format!("onb:request {request} carries no certificate"))?;
        let fp = ikigai_quic::fingerprint_of_pem(pem)
            .map_err(|e| format!("onb:request {request}: {e}"))?;
        if fp != v.fingerprint {
            return Err(format!(
                "onb:request {request}'s certificate hashes to {fp}, not {}",
                v.fingerprint
            ));
        }
        Ok(pem.to_string())
    }

    // ---- the resources ----

    /// The onboarding resources, for the hub.
    pub fn space(self: &Arc<Self>) -> EndpointSpace {
        let at = |op: Op| Resource {
            onboard: Arc::clone(self),
            op,
        };
        let template = |t: &str| UriTemplate::parse(t).expect("a constant template");
        EndpointSpace::new()
            .bind(Exact::new(INVITE), at(Op::Invite))
            .bind(Exact::new(PENDING), at(Op::Pending))
            .bind(Exact::new(APPROVE), at(Op::Approve))
            .bind(Exact::new(REVOKE), at(Op::Revoke))
            .bind(Exact::new(APPLY), at(Op::Apply))
            .bind(Exact::new(STATUS), at(Op::Status))
            .bind(template("urn:iki:onboard:kind:{kind}"), at(Op::Kind))
            .bind(template("urn:iki:onboard:invite:{id}"), at(Op::InviteOf))
            .bind(template("urn:iki:onboard:check:{entry}"), at(Op::Check))
            .named(crate::spaces::iri(crate::spaces::ONBOARD))
    }
}

/// A `{ledger}={role}` gonk grant as scopes. Delete and purge are refused: an entitlement
/// enrolls a node, never an operator.
///
/// ```
/// use ikigai_gonk::onboard::gonk_scopes;
/// assert!(gonk_scopes("default=worker").unwrap().contains(&"urn:cap:ledger:claim:default".to_string()));
/// assert!(gonk_scopes("default=purge").is_err());
/// assert!(gonk_scopes("default").is_err());
/// ```
///
/// # Errors
///
/// Not `{ledger}={role}`, a ledger name the ledger refuses, a role that is not read, audit,
/// worker or write.
pub fn gonk_scopes(grant: &str) -> std::result::Result<Vec<String>, String> {
    use crate::grants::Authority;
    let (ledger, role) = grant
        .split_once('=')
        .ok_or_else(|| format!("`{grant}` is not {{ledger}}={{role}}"))?;
    let authority: Authority = role.parse()?;
    if matches!(authority, Authority::Delete | Authority::Purge) {
        return Err(format!(
            "`{role}` is not a role an entitlement grants (read, audit, worker or write)"
        ));
    }
    crate::grants::grants_for(ledger, authority)
}

/// The first `urn:layer:{layer}:entry:{id}` named in a say's answer.
fn first_entry(text: &str, layer: &str) -> Option<String> {
    let prefix = format!("urn:layer:{layer}:entry:");
    let at = text.find(&prefix)?;
    let rest = &text[at..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == ':' || c == '-'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// The controller's private key, served to `urn:sign:sign` in the signer's own kernel, read
/// from the keystore at each signing.
struct Signer {
    keystore: Arc<dyn ikigai_secret::Backend>,
    key: String,
}

#[async_trait]
impl Endpoint for Signer {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        let pem = self.keystore.get(&self.key)?.ok_or_else(|| {
            Error::NotFound(format!(
                "no controller key in the keystore under `{}`: run `ikigai-gonk onboard init`",
                self.key
            ))
        })?;
        Ok(Representation::new(
            ReprType::new("application/x-pem-file"),
            pem,
        ))
    }

    fn name(&self) -> &str {
        "gonk-onboard-signer"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-onboard-signer")
            .summary("The controller's private key, for the signer's kernel only; bound in no door")
            .verb(Verb::Source)
    }
}

/// The controller's public key, served to `urn:sign:verify` in the signer's own kernel.
struct Public(Vec<u8>);

#[async_trait]
impl Endpoint for Public {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/x-pem-file"),
            self.0.clone(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-onboard-public"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-onboard-public")
            .summary("The controller's public key; bound in no door")
            .verb(Verb::Source)
    }
}

#[derive(Clone, Copy)]
enum Op {
    Invite,
    Pending,
    Approve,
    Revoke,
    Apply,
    Status,
    Kind,
    InviteOf,
    Check,
}

struct Resource {
    onboard: Arc<Onboard>,
    op: Op,
}

async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("gonk-onboard-call".to_string())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| Error::Endpoint(format!("no thread for onboarding: {e}")))?;
    rx.await
        .map_err(|_| Error::Endpoint("an onboarding call panicked".to_string()))
}

fn wants_json(inv: &Invocation<'_>) -> bool {
    inv.inline_str("as").ok() == Some(JSON)
}

fn answer(inv: &Invocation<'_>, json: Value, text: String) -> Representation {
    if wants_json(inv) {
        Representation::new(
            ReprType::new(JSON),
            serde_json::to_vec_pretty(&json).expect("JSON serializes"),
        )
    } else {
        Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            text.into_bytes(),
        )
    }
}

fn as_spec() -> ArgSpec {
    ArgSpec::new("as")
        .summary("the face: text (default) or application/json")
        .class(XSD_STRING)
        .one_of([TEXT, JSON])
        .default_value(TEXT)
        .optional()
}

fn minted_json(m: &Minted) -> Value {
    json!({
        "invite": m.invite.id,
        "name": m.invite.name,
        "kinds": m.invite.kinds,
        "expires": m.invite.expires,
        "code": m.code,
        "door": m.door,
        "pin": m.pin,
        "server_cert": m.server_cert,
    })
}

#[async_trait]
impl Endpoint for Resource {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let onboard = Arc::clone(&self.onboard);
        match self.op {
            Op::Invite => {
                let name = inv.inline_str("name")?.to_string();
                let kinds: Vec<String> = inv
                    .inline_str("kind")?
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect();
                let minted = off_thread(move || onboard.invite(&name, &kinds)).await??;
                let text = format!(
                    "invite  {}\ncode    {}   (shown once; it is the node's only way in)\nkinds   {}\nexpires {}\ndoor    {}\npin     {}\n",
                    minted.invite.id,
                    minted.code,
                    minted.invite.kinds.join(","),
                    minted.invite.expires,
                    minted.door,
                    minted.pin
                );
                Ok(answer(inv, minted_json(&minted), text))
            }
            Op::Pending => {
                let pending = off_thread(move || onboard.pending())
                    .await?
                    .map_err(Error::Endpoint)?;
                let text: String = if pending.is_empty() {
                    "no enrollment requests wait\n".to_string()
                } else {
                    pending.iter().map(Checked::line).collect()
                };
                Ok(answer(
                    inv,
                    json!({ "pending": pending.iter().map(Checked::json).collect::<Vec<_>>() }),
                    text,
                ))
            }
            Op::Check => {
                let id = inv.bindings.get("entry").unwrap_or_default().to_string();
                let checked = off_thread(move || onboard.check(&id))
                    .await?
                    .map_err(Error::Endpoint)?;
                Ok(answer(inv, checked.json(), checked.line()))
            }
            Op::Approve => {
                let id = inv.inline_str("entry")?.to_string();
                let (said, status) = off_thread(move || onboard.approve(&id)).await??;
                Ok(answer(
                    inv,
                    json!({ "entitlement": said, "status": status.json() }),
                    format!("approved: {said}\n{}", status.text()),
                ))
            }
            Op::Revoke => {
                let who = inv.inline_str("who")?.to_string();
                let (said, status) = off_thread(move || onboard.revoke(&who)).await??;
                Ok(answer(
                    inv,
                    json!({ "revocation": said, "status": status.json() }),
                    format!("revoked: {said}\n{}", status.text()),
                ))
            }
            Op::Apply => {
                let status = off_thread(move || onboard.apply()).await?;
                Ok(answer(inv, status.json(), status.text()))
            }
            Op::Status => {
                let status = onboard.status();
                Ok(answer(inv, status.json(), status.text()))
            }
            Op::Kind => {
                let kind = inv.bindings.get("kind").unwrap_or_default().to_string();
                let turtle = off_thread(move || onboard.kind_turtle(&kind)).await??;
                Ok(Representation::new(
                    ReprType::new(N_TRIPLES),
                    turtle.into_bytes(),
                ))
            }
            Op::InviteOf => {
                let id = inv.bindings.get("id").unwrap_or_default().to_string();
                let invites = off_thread(move || onboard.invites())
                    .await?
                    .map_err(Error::Endpoint)?;
                let invite = invites
                    .get(&id)
                    .ok_or_else(|| Error::NotFound(format!("no invite `{id}`")))?;
                Ok(answer(
                    inv,
                    json!({
                        "invite": invite.id,
                        "name": invite.name,
                        "kinds": invite.kinds,
                        "expires": invite.expires,
                        "used_by": invite.used_by,
                    }),
                    format!(
                        "invite  {}\nname    {}\nkinds   {}\nexpires {}\nused    {}\n",
                        invite.id,
                        invite.name,
                        invite.kinds.join(","),
                        invite.expires,
                        invite.used_by.as_deref().unwrap_or("no")
                    ),
                ))
            }
        }
    }

    fn name(&self) -> &str {
        match self.op {
            Op::Invite => "gonk-onboard-invite",
            Op::Pending => "gonk-onboard-pending",
            Op::Approve => "gonk-onboard-approve",
            Op::Revoke => "gonk-onboard-revoke",
            Op::Apply => "gonk-onboard-apply",
            Op::Status => "gonk-onboard-status",
            Op::Kind => "gonk-onboard-kind",
            Op::InviteOf => "gonk-onboard-invite-of",
            Op::Check => "gonk-onboard-check",
        }
    }

    fn describe(&self) -> Description {
        let base = Description::new(self.name());
        let sink = |summary: &str| {
            ActionSpec::new(Verb::Sink)
                .summary(summary)
                .requires(CAP_ADMIN)
                .output(TEXT)
                .output(JSON)
        };
        let source = |summary: &str| {
            ActionSpec::new(Verb::Source)
                .summary(summary)
                .requires(CAP_READ)
                .input(as_spec())
                .output(TEXT)
                .output(JSON)
        };
        let string =
            |name: &str, summary: &str| ArgSpec::new(name).summary(summary).class(XSD_STRING);
        match self.op {
            Op::Invite => base
                .title("Invite a node")
                .summary(
                    "Mint a one-use, 24-hour invite for a node of one or more kinds: its id and \
                     pairing code (shown once), and the layer door and its certificate to pin",
                )
                .action(
                    sink("mint an invite")
                        .input(string("name", "the node's name (lowercase, digits, -)"))
                        .input(string("kind", "the kinds, comma-separated (e.g. worker,task-member)"))
                        .input(as_spec()),
                ),
            Op::Pending => base
                .title("Pending enrollment requests")
                .summary(
                    "The lobby's requests that no entitlement names yet, each checked: invite, \
                     expiry, use, proof and certificate",
                )
                .action(source("the pending requests")),
            Op::Check => base
                .title("Check one enrollment request")
                .summary("One lobby entry, checked against its invite and its proof")
                .action(
                    source("the verdict").input(
                        ArgSpec::new("entry")
                            .summary("the lobby entry's id")
                            .class(XSD_STRING)
                            .binding(),
                    ),
                ),
            Op::Approve => base
                .title("Approve an enrollment request")
                .summary(
                    "Sign an entitlement for one checked lobby request under the controller key, \
                     say it on the grants layer, spend the invite, and apply gonk's half",
                )
                .action(
                    sink("approve")
                        .input(string("entry", "the lobby entry's id"))
                        .input(as_spec()),
                ),
            Op::Revoke => base
                .title("Revoke an entitlement")
                .summary("A signed entitlement with no grant, superseding the one in force")
                .action(
                    sink("revoke")
                        .input(string("who", "the node's fingerprint, or its name"))
                        .input(as_spec()),
                ),
            Op::Apply => base
                .title("Apply the grants layer")
                .summary(
                    "Enroll each verified entitlement's gonk grant in this gonk (client add \
                     --cert), unenroll each one revoked or expired",
                )
                .action(sink("apply now").input(as_spec())),
            Op::Status => base
                .title("Onboarding status")
                .summary("Every entitlement and its verdict, what gonk enrolled, and whether a restart is pending")
                .action(source("the status")),
            Op::Kind => base
                .title("A node kind")
                .summary("One kind's grants, from the kinds file")
                .action(
                    ActionSpec::new(Verb::Source)
                        .requires(CAP_READ)
                        .input(
                            ArgSpec::new("kind")
                                .summary("the kind's name")
                                .class(XSD_STRING)
                                .binding(),
                        )
                        .output(N_TRIPLES),
                ),
            Op::InviteOf => base
                .title("An invite")
                .summary("An invite as the store holds it: never its code")
                .action(
                    source("the invite").input(
                        ArgSpec::new("id")
                            .summary("the invite's id")
                            .class(XSD_STRING)
                            .binding(),
                    ),
                ),
        }
        .verb(Verb::Meta)
    }
}

// ---- the command line ----

/// `ikigai-gonk onboard …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOp {
    /// Mint the controller key into the keystore and write its public half.
    Init,
    /// `invite <name> --kind <k,k>`.
    Invite {
        /// The node's name.
        name: String,
        /// Its kinds, comma-separated.
        kinds: String,
        /// Where the door's server certificate is written.
        out: PathBuf,
    },
    /// `pending`.
    Pending,
    /// `check <entry>`.
    Check(String),
    /// `approve <entry>`.
    Approve(String),
    /// `revoke <fingerprint|name>`.
    Revoke(String),
    /// `apply`.
    Apply,
    /// `status`.
    Status,
}

/// A parsed `ikigai-gonk onboard` command line.
#[derive(Debug)]
pub struct Args {
    /// What to do.
    pub op: CliOp,
    /// `--socket`, `--config`, `--config-home`, `--data-home`.
    pub flags: crate::config::Flags,
}

/// The `onboard` usage.
pub const USAGE: &str = "ikigai-gonk onboard init                         mint the controller key (Keychain) and write its public half
ikigai-gonk onboard invite <name> --kind <k[,k]> [--out DIR]
                                                 a one-use, 24 h invite: the code once, the door's server.crt into DIR
ikigai-gonk onboard pending                      lobby requests waiting, each checked
ikigai-gonk onboard check <entry>                one request's verdict
ikigai-gonk onboard approve <entry>              sign and say its entitlement, then apply gonk's half
ikigai-gonk onboard revoke <fingerprint|name>    a signed revocation
ikigai-gonk onboard apply | status               apply the grants layer now | what is in force
  (every one but init talks to the running gonk over its owner-only socket; --socket, --config,
   --config-home and --data-home as for serve)";

/// Parse the words after `onboard`.
///
/// ```
/// use ikigai_gonk::onboard::{parse_args, CliOp};
/// let words = ["invite", "mini", "--kind", "worker,task-member"].map(String::from);
/// let args = parse_args(words.into_iter()).unwrap();
/// assert!(matches!(args.op, CliOp::Invite { ref name, ref kinds, .. } if name == "mini" && kinds == "worker,task-member"));
/// assert!(parse_args(["approve"].map(String::from).into_iter()).is_err());
/// ```
///
/// # Errors
///
/// An unknown subcommand or flag, or a missing operand.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> std::result::Result<Args, String> {
    let sub = args
        .next()
        .ok_or_else(|| format!("onboard: expected a subcommand\n\n{USAGE}"))?;
    let mut flags = crate::config::Flags::default();
    let (mut operand, mut kinds, mut out) = (None, None, None);
    while let Some(word) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("onboard {sub}: {word} needs a value"))
        };
        match word.as_str() {
            "--socket" => flags.socket = Some(value()?),
            "--config" => flags.config = Some(value()?.into()),
            "--config-home" => flags.config_home = Some(value()?.into()),
            "--data-home" => flags.data_home = Some(value()?.into()),
            "--kind" if sub == "invite" => kinds = Some(value()?),
            "--out" if sub == "invite" => out = Some(PathBuf::from(value()?)),
            w if !w.starts_with('-') && operand.is_none() => operand = Some(w.to_string()),
            other => return Err(format!("onboard {sub}: unexpected `{other}`\n\n{USAGE}")),
        }
    }
    let need = |what: &str| {
        operand
            .clone()
            .ok_or_else(|| format!("onboard {sub}: expected {what}\n\n{USAGE}"))
    };
    let none = |op: CliOp| match &operand {
        Some(extra) => Err(format!("onboard {sub}: unexpected `{extra}`")),
        None => Ok(op),
    };
    let op = match sub.as_str() {
        "init" => none(CliOp::Init)?,
        "invite" => CliOp::Invite {
            name: need("<name>")?,
            kinds: kinds.ok_or("onboard invite: expected --kind <kind[,kind]>")?,
            out: out.unwrap_or_else(|| PathBuf::from(".")),
        },
        "pending" => none(CliOp::Pending)?,
        "check" => CliOp::Check(need("<entry>")?),
        "approve" => CliOp::Approve(need("<entry>")?),
        "revoke" => CliOp::Revoke(need("<fingerprint|name>")?),
        "apply" => none(CliOp::Apply)?,
        "status" => none(CliOp::Status)?,
        other => return Err(format!("onboard: `{other}` is not a subcommand\n\n{USAGE}")),
    };
    Ok(Args { op, flags })
}

/// The request a socket subcommand issues: `(verb, target, args)`, every answer as text but
/// the invite's, which the caller reads as JSON.
pub fn cli_request(op: &CliOp) -> Option<Request> {
    let with = |r: Request, k: &str, v: &str| r.with_arg(k, ArgRef::Inline(v.as_bytes().to_vec()));
    Some(match op {
        CliOp::Init => return None,
        CliOp::Invite { name, kinds, .. } => with(
            with(
                with(Request::new(Verb::Sink, iri(INVITE)), "name", name),
                "kind",
                kinds,
            ),
            "as",
            JSON,
        ),
        CliOp::Pending => Request::new(Verb::Source, iri(PENDING)),
        CliOp::Check(entry) => Request::new(
            Verb::Source,
            Iri::parse(format!("{CHECK_PREFIX}{entry}")).ok()?,
        ),
        CliOp::Approve(entry) => with(Request::new(Verb::Sink, iri(APPROVE)), "entry", entry),
        CliOp::Revoke(who) => with(Request::new(Verb::Sink, iri(REVOKE)), "who", who),
        CliOp::Apply => Request::new(Verb::Sink, iri(APPLY)),
        CliOp::Status => Request::new(Verb::Source, iri(STATUS)),
    })
}

/// What `onboard invite` prints from the invite's JSON, having written the door's server
/// certificate to `cert_path`: the code once, and the node's exact command.
pub fn invite_lines(minted: &Value, cert_path: &Path) -> String {
    let s = |k: &str| minted[k].as_str().unwrap_or_default().to_string();
    format!(
        "invite   {id}\ncode     {code}   (shown once: give it to the node out of band)\nkinds    {kinds}\nexpires  {expires}\ndoor     {door}\npin      {pin}\nserver   {cert}\n\non the node, in a checkout of ikigai-gonk-starter, with {cert} copied beside it:\n  python3 tools/worker.py onboard --door {door} --pin {pin} --server-cert {file} --code {code} --invite {id} --name {name}\n",
        id = s("invite"),
        code = s("code"),
        kinds = minted["kinds"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")).unwrap_or_default(),
        expires = s("expires"),
        door = s("door"),
        pin = s("pin"),
        cert = cert_path.display(),
        file = cert_path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        name = s("name"),
    )
}

/// Mint the controller key: `urn:secret:generate type=ed25519 into=<key>` over `keystore`
/// (Touch ID on a Mac, through ikigai-secret's own gate), answering the public half's PEM.
///
/// # Errors
///
/// A key already stored under the name (generate never replaces one), a refused gate.
pub fn generate_key(
    keystore: Arc<dyn ikigai_secret::Backend>,
    gate: Option<Arc<dyn ikigai_secret::PresenceGate>>,
    key: &str,
) -> std::result::Result<String, String> {
    let space = match gate {
        Some(gate) => ikigai_secret::space_with_gate(keystore, gate),
        None => ikigai_secret::space(keystore),
    };
    let kernel = Kernel::new(Arc::new(space));
    let request = Request::new(Verb::Source, iri(ikigai_secret::GENERATE_IRI))
        .with_arg("into", ArgRef::Inline(key.as_bytes().to_vec()))
        .with_arg("type", ArgRef::Inline(b"ed25519".to_vec()));
    let cap = Capability::scoped([
        "urn:cap:secret:generate".to_string(),
        format!("urn:cap:secret:write:{key}"),
        format!("urn:cap:secret:write:{key}.pub"),
    ]);
    ikigai_resolve::Resolver::issue_as(&kernel, request, &cap)
        .map(|(r, _)| String::from_utf8_lossy(&r.bytes).into_owned())
        .map_err(|e| format!("minting the controller key `{key}`: {e}"))
}

// ---- the page ----

/// Bind the `/onboard` page in the HTTP door's page space.
pub fn bind_page(space: EndpointSpace, web: &Arc<crate::web::Web>) -> EndpointSpace {
    space.bind(
        Exact::new(PAGE_IRI),
        Page {
            web: Arc::clone(web),
        },
    )
}

/// Whether the header offers the page: this server onboards, and the caller may read it.
pub(crate) fn offers_page(web: &crate::web::Web, capability: &Capability) -> bool {
    capability.allows(CAP_READ) && web.hub.describe(&iri(PENDING)).is_some()
}

struct Page {
    web: Arc<crate::web::Web>,
}

#[async_trait]
impl Endpoint for Page {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        use crate::render::{element, envelope};
        let ledgers = crate::web::readable_ledgers(&self.web, inv.capability);
        let mut children = crate::web::nav(&self.web, inv.capability, &ledgers, None);
        let mut attributes = vec![
            ("view", "onboard".to_string()),
            ("full", "true".to_string()),
            ("title", "Onboard".to_string()),
        ];
        if self.web.hub.describe(&iri(PENDING)).is_none() {
            attributes.push((
                "note",
                format!("This server does not onboard nodes: no {KEY} line."),
            ));
        } else if !inv.capability.allows(CAP_READ) {
            attributes.push((
                "note",
                format!("Reading this page needs {CAP_READ} in your passkey's grant."),
            ));
        } else {
            let read = |target: &str| {
                Request::new(Verb::Source, iri(target))
                    .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()))
            };
            match inv.issue(read(PENDING)).await {
                Ok(r) => {
                    let v: Value = serde_json::from_slice(&r.bytes).unwrap_or(Value::Null);
                    for p in v["pending"].as_array().into_iter().flatten() {
                        let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
                        let kinds = p["kinds"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        children.push_str(&element(
                            "onboard-request",
                            &[
                                ("id", &s("id")),
                                ("name", &s("name")),
                                ("kinds", &kinds),
                                ("fingerprint", &s("fingerprint")),
                                ("verdict", &s("verdict")),
                                ("reason", &s("reason")),
                                ("approve", &s("approve")),
                            ],
                            "",
                        ));
                    }
                }
                Err(e) => attributes.push(("error", format!("The lobby does not read: {e}"))),
            }
            if let Ok(r) = inv.issue(read(STATUS)).await {
                let v: Value = serde_json::from_slice(&r.bytes).unwrap_or(Value::Null);
                for rec in v["records"].as_array().into_iter().flatten() {
                    let s = |k: &str| rec[k].as_str().unwrap_or_default().to_string();
                    children.push_str(&element(
                        "onboard-entitlement",
                        &[
                            ("entry", &s("entry")),
                            ("principal", &s("principal")),
                            ("fingerprint", &s("fingerprint")),
                            ("verdict", &s("verdict")),
                            ("reason", &s("reason")),
                        ],
                        "",
                    ));
                }
                if v["restart_pending"].as_bool() == Some(true) {
                    attributes.push((
                        "restart",
                        "Restart ikigai-gonk: a certificate was newly trusted, and trusted \
                         certificates are read at startup."
                            .to_string(),
                    ));
                }
            }
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        Ok(crate::web::html(
            crate::render::render(&doc, true).map_err(crate::web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-onboard-page"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-onboard-page")
            .title("Onboard")
            .summary(
                "Pending enrollment requests with the command that approves each (approval is \
                 the owner's socket only), and every entitlement with its verdict",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(crate::web::as_html_arg())
            .output("text/html")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_ten_symbols_of_the_alphabet() {
        let code = random_from(CODE_ALPHABET, CODE_LEN).unwrap();
        assert_eq!(code.len(), CODE_LEN);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
    }

    #[test]
    fn a_proof_checks_in_constant_time_and_refuses_junk() {
        let p = proof("ABCDEFGHJK", "ab");
        assert!(proof_holds("ABCDEFGHJK", "ab", &p));
        assert!(!proof_holds("ABCDEFGHJL", "ab", &p));
        assert!(!proof_holds("ABCDEFGHJK", "ab", "zz"));
        assert!(!proof_holds("ABCDEFGHJK", "ab", "abc"));
    }

    #[test]
    fn the_first_entry_of_a_say_is_found() {
        assert_eq!(
            first_entry("said <urn:layer:grants:entry:abc123> at 4", "grants").as_deref(),
            Some("urn:layer:grants:entry:abc123")
        );
        assert_eq!(first_entry("nothing", "grants"), None);
    }
}
