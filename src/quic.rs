//! The QUIC door: who is trusted, and what each trusted certificate may do.
//!
//! Two questions, answered by two different things, exactly as `ikigai serve quic://…`
//! answers them:
//!
//! 1. **Is this certificate trusted at all?** Mutual TLS with pinned certificates, no CA:
//!    the server presents `quic/server.crt`, and accepts a client only if it presents one of
//!    the certificates under `quic/clients/<name>/client.crt`. Read at startup, so adding a
//!    client takes a restart.
//! 2. **What may it do?** The certificate's SHA-256 fingerprint maps to a grant name in
//!    `clients.json`, and the grant name maps to capability scopes in `grants.json` — the
//!    cli's two files in the cli's two shapes. Re-read on EVERY connection, so editing either
//!    file changes a client's authority on its next connection, and deleting its entry
//!    revokes it.
//!
//! **Fail closed at every step.** A trusted certificate with no grant is refused. A grant
//! that names no scopes (a typo, or one deleted from `grants.json`) is refused, not treated
//! as unrestricted. A grant naming a broad store token, an offering wildcard or the backup
//! family is refused ([`grant_refusal`]) — at startup, and again per connection in case the
//! file was edited after. An enrolment file
//! that does not parse stops the server rather than degrading it.
//!
//! Everything lives under `<config home>/gonk/`:
//!
//! ```text
//! gonk/clients.json              { "clients": { "<fingerprint>": { "grant": "laptop" } } }
//! gonk/grants.json               { "laptop": ["urn:cap:ledger:read:default", …] }
//! gonk/quic/server.crt, .key     this server's identity, generated on first use
//! gonk/quic/clients/<name>/      client.crt (trusted), server.crt (to pin), client.key (if minted here)
//! ```
//!
//! A `clients/<name>/` directory is laid out as `ikigai --cert-dir` expects, so a client
//! copies it and connects with `ikigai --connect quic://host:1060 --cert-dir <it>`.
//! Provisioning, rotation and trust distribution are not built: the server's certificate is
//! pinned by copying it, and a client admitted today is admitted until its entry is removed.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ikigai_core::Capability;
use ikigai_quic::{Identity, Minter, PeerIdentity, Session};
use serde_json::{json, Map, Value};

use crate::config::QuicBind;
use crate::grants::broad_store_scopes;

/// Where the QUIC door's files live.
#[derive(Debug, Clone)]
pub struct Layout {
    dir: PathBuf,
}

impl Layout {
    /// `<config home>/gonk`.
    pub fn in_config_home(config_home: &Path) -> Layout {
        Layout {
            dir: config_home.join("gonk"),
        }
    }

    /// `gonk/quic`.
    pub fn quic_dir(&self) -> PathBuf {
        self.dir.join("quic")
    }

    /// The server certificate, which every client pins.
    pub fn server_cert(&self) -> PathBuf {
        self.quic_dir().join("server.crt")
    }

    /// The server's private key.
    pub fn server_key(&self) -> PathBuf {
        self.quic_dir().join("server.key")
    }

    /// One directory per trusted client.
    pub fn clients_dir(&self) -> PathBuf {
        self.quic_dir().join("clients")
    }

    /// Fingerprint → grant name.
    pub fn clients_json(&self) -> PathBuf {
        self.dir.join("clients.json")
    }

    /// Grant name → scopes.
    pub fn grants_json(&self) -> PathBuf {
        self.dir.join("grants.json")
    }

    /// Outstanding passkey invites: SHA-256 of the code → grant and expiry.
    pub fn invites_json(&self) -> PathBuf {
        self.dir.join("invites.json")
    }

    /// This deployment's render rules, replacing [`crate::rules::DEFAULT_RULES`] wholesale
    /// when the file exists. Read once, at startup, and served at
    /// `urn:iki:gonk:render-rules` — see [`crate::rules`].
    pub fn render_rules_ttl(&self) -> PathBuf {
        self.dir.join("render-rules.ttl")
    }
}

/// The server identity, generated on first use. The `bool` says whether it was just made.
///
/// A certificate without its key (or the reverse) is refused rather than regenerated: every
/// client pins the certificate, and silently replacing it would break all of them.
pub fn server_identity(layout: &Layout) -> Result<(Identity, bool), String> {
    let (crt, key) = (layout.server_cert(), layout.server_key());
    match (crt.exists(), key.exists()) {
        (true, true) => Ok((
            Identity {
                cert_pem: read(&crt)?,
                key_pem: read(&key)?,
            },
            false,
        )),
        (false, false) => {
            private_dir(&layout.quic_dir())?;
            let identity = ikigai_quic::generate();
            write_private(&key, &identity.key_pem)?;
            write_private(&crt, &identity.cert_pem)?;
            Ok((identity, true))
        }
        _ => Err(format!(
            "{} and {} must exist together — restore the missing half, or remove both to \
             generate a new identity (every client then needs the new server.crt)",
            crt.display(),
            key.display()
        )),
    }
}

/// Every trusted client certificate, as `(name, PEM)`, in name order.
pub fn trusted_client_certs(layout: &Layout) -> Result<Vec<(String, String)>, String> {
    let dir = layout.clients_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut trusted = Vec::new();
    for entry in entries.flatten() {
        let crt = entry.path().join("client.crt");
        if crt.is_file() {
            trusted.push((
                entry.file_name().to_string_lossy().into_owned(),
                read(&crt)?,
            ));
        }
    }
    trusted.sort();
    Ok(trusted)
}

/// A client bundle on disk.
#[derive(Debug)]
pub struct Bundle {
    /// `gonk/quic/clients/<name>`.
    pub dir: PathBuf,
    /// Lowercase hex SHA-256 of the client certificate — the `clients.json` key.
    pub fingerprint: String,
    /// Whether this call generated a private key into the bundle.
    pub key_minted: bool,
}

/// Trust a client: mint an identity into `clients/<name>/`, or import a certificate the
/// client generated itself (`import`), so its private key never leaves its machine.
///
/// An existing bundle is reused as it is — so `client add` is also how an existing client is
/// enrolled — unless `force` asks for a new identity. Importing over an existing bundle is
/// refused: it would leave a private key beside a certificate it does not match.
pub fn add_client(
    layout: &Layout,
    name: &str,
    import: Option<&Path>,
    force: bool,
) -> Result<Bundle, String> {
    valid_name(name)?;
    let (server, _) = server_identity(layout)?;
    let dir = layout.clients_dir().join(name);
    let exists = dir.join("client.crt").is_file();
    if exists && import.is_some() {
        return Err(format!(
            "client `{name}` already exists at {} — import under a new name",
            dir.display()
        ));
    }
    private_dir(&layout.clients_dir())?;
    private_dir(&dir)?;
    let (cert_pem, key_minted) = match import {
        Some(path) => (read(path)?, false),
        None if exists && !force => (read(&dir.join("client.crt"))?, false),
        None => {
            let identity = ikigai_quic::generate();
            write_private(&dir.join("client.key"), &identity.key_pem)?;
            (identity.cert_pem, true)
        }
    };
    let fingerprint = ikigai_quic::fingerprint_of_pem(&cert_pem)
        .map_err(|e| format!("client `{name}`: not a PEM certificate: {e}"))?;
    write_private(&dir.join("client.crt"), &cert_pem)?;
    write_private(&dir.join("server.crt"), &server.cert_pem)?;
    Ok(Bundle {
        dir,
        fingerprint,
        key_minted,
    })
}

/// The fingerprint [`add_client`] WOULD enrol, worked out without writing anything — so
/// every refusal can run before a key pair is minted ([`enrol_refusal`]).
///
/// `None` when the call would mint a new identity, whose fingerprint does not exist yet and
/// so cannot already be enrolled under anything.
///
/// # Errors
///
/// The refusals [`add_client`] makes up front: a name that cannot be a bundle, an import
/// over an existing bundle, and an unreadable certificate.
pub fn planned_fingerprint(
    layout: &Layout,
    name: &str,
    import: Option<&Path>,
    force: bool,
) -> Result<Option<String>, String> {
    valid_name(name)?;
    let existing = layout.clients_dir().join(name).join("client.crt");
    let pem = match import {
        Some(_) if existing.is_file() => {
            return Err(format!(
                "client `{name}` already exists at {} — import under a new name",
                layout.clients_dir().join(name).display()
            ))
        }
        Some(path) => read(path)?,
        None if existing.is_file() && !force => read(&existing)?,
        None => return Ok(None),
    };
    ikigai_quic::fingerprint_of_pem(&pem)
        .map(Some)
        .map_err(|e| format!("client `{name}`: not a PEM certificate: {e}"))
}

/// The parsed `clients.json`: fingerprint → grant name, plus an explicit shared default.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Enrolment {
    clients: BTreeMap<String, String>,
    default_grant: Option<String>,
}

impl Enrolment {
    /// How many certificates are enrolled.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Whether this file admits any client certificate at all: one enrolled fingerprint, or
    /// an explicit shared default. Passkeys do not count — they are the HTTP door's.
    pub fn admits_certificates(&self) -> bool {
        !self.clients.is_empty() || self.default_grant.is_some()
    }

    /// The explicitly configured shared default, if any. Absence is never a default.
    pub fn default_grant(&self) -> Option<&str> {
        self.default_grant.as_deref()
    }

    /// The grant for a fingerprint: its own entry, else the explicit default.
    pub fn grant_for(&self, fingerprint: &str) -> Option<&str> {
        self.clients
            .get(&normalize(fingerprint))
            .map(String::as_str)
            .or(self.default_grant.as_deref())
    }
}

/// Parse `clients.json` — the cli's shape: `{"clients": {fp: "grant" | {"grant": …}},
/// "default": "grant"}`. Fingerprints are normalized, so `openssl`'s `AB:CD:…` pastes in.
pub fn parse_enrolment(text: &str) -> Result<Enrolment, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let mut clients = BTreeMap::new();
    if let Some(map) = v.get("clients") {
        let map = map
            .as_object()
            .ok_or("`clients` must be an object of fingerprint → grant")?;
        for (fingerprint, entry) in map {
            let grant = match entry {
                Value::String(name) => name.clone(),
                Value::Object(_) => entry
                    .get("grant")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("client `{fingerprint}` has no `grant`"))?
                    .to_string(),
                _ => {
                    return Err(format!(
                        "client `{fingerprint}` must be a grant name or an object with one"
                    ))
                }
            };
            clients.insert(normalize(fingerprint), grant);
        }
    }
    let default_grant = match v.get("default") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return Err("`default` must be a grant name".to_string()),
    };
    Ok(Enrolment {
        clients,
        default_grant,
    })
}

/// Read `clients.json`. `Ok(None)` when there is no file; `Err` when it exists and is
/// unusable, which must stop a server rather than leave it serving under a guess.
pub fn read_enrolment(path: &Path) -> Result<Option<Enrolment>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_enrolment(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// The QUIC door this run: shut, with the banner's reason, or open with what it serves.
pub enum QuicDoor {
    /// Not opening. The text is the banner's `quic` line, written for a person to act on.
    Off(String),
    /// Opening.
    Open {
        /// The UDP address.
        addr: SocketAddr,
        /// This server's identity (generated now if it did not exist).
        identity: Identity,
        /// Trusted client certificates, PEM.
        trusted: Vec<String>,
        /// How many fingerprints `clients.json` enrols.
        enrolled: usize,
    },
}

/// Decide whether the QUIC door opens, and prepare it if so.
///
/// ★ **The door opens only on a decision to face the network**: an enrolled client
/// certificate (a fingerprint under `clients`, or a `default` grant), or a bind the operator
/// named. `clients.json` EXISTING is not one — a browser enrolling a passkey writes that file
/// too, and through 0.1.0 that made the next start refuse (gonk PENDING §3).
///
/// - A default bind with no certificate enrolled: [`QuicDoor::Off`], and nothing is written.
/// - A named bind with no certificate enrolled: refused, because the operator expects a door.
/// - A certificate enrolled but no `client.crt` trusted: refused, as before.
///
/// The server identity is generated only on the path that returns [`QuicDoor::Open`], so a
/// run that will not serve QUIC leaves no `quic/server.*` behind.
pub fn open_door(layout: &Layout, bind: QuicBind) -> Result<QuicDoor, String> {
    let (addr, named) = match bind {
        QuicBind::Off => return Ok(QuicDoor::Off("off (--no-quic)".to_string())),
        QuicBind::Default(addr) => (addr, false),
        QuicBind::Explicit(addr) => (addr, true),
    };
    let clients_json = layout.clients_json();
    let enrolment = read_enrolment(&clients_json)?;
    if !enrolment
        .as_ref()
        .is_some_and(Enrolment::admits_certificates)
    {
        let why = match enrolment {
            None => format!(
                "no client certificate is enrolled (there is no {})",
                clients_json.display()
            ),
            Some(_) => format!(
                "{} enrols no client certificate (passkeys sign in on the HTTP door only)",
                clients_json.display()
            ),
        };
        let add = "`ikigai-gonk client add <name> --ledger <ledger>=write`";
        return if named {
            Err(format!(
                "the QUIC door was asked for on udp {addr} (--quic-bind or gonk.quic.bind), but \
                 {why} — enrol one with {add}, or remove the bind (or start with --no-quic)"
            ))
        } else {
            Ok(QuicDoor::Off(format!(
                "off — {why}; to open it, run {add} and restart"
            )))
        };
    }
    let trusted: Vec<String> = trusted_client_certs(layout)?
        .into_iter()
        .map(|(_, pem)| pem)
        .collect();
    if trusted.is_empty() {
        return Err(format!(
            "{} enrols a client certificate but none is trusted (no client.crt under {}) — add \
             one with `ikigai-gonk client add <name>`, or start with --no-quic",
            clients_json.display(),
            layout.clients_dir().display()
        ));
    }
    let (identity, _) = server_identity(layout)?;
    Ok(QuicDoor::Open {
        addr,
        identity,
        trusted,
        enrolled: enrolment.map_or(0, |e| e.len()),
    })
}

/// Parse `grants.json` — the cli's shape: grant name → an array of scopes, or an object
/// with a `scopes` array.
pub fn parse_grants(text: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let map = v
        .as_object()
        .ok_or("grants must be an object of grant name → scopes")?;
    let mut grants = BTreeMap::new();
    for (name, entry) in map {
        let array = match entry {
            Value::Array(_) => entry,
            Value::Object(_) => entry
                .get("scopes")
                .ok_or_else(|| format!("grant `{name}` has no `scopes`"))?,
            _ => return Err(format!("grant `{name}` must be an array of scopes")),
        };
        let scopes = array
            .as_array()
            .ok_or_else(|| format!("grant `{name}`: `scopes` must be an array"))?
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("grant `{name}`: every scope must be a string"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        grants.insert(name.clone(), scopes);
    }
    Ok(grants)
}

/// Read `grants.json`; absent is an empty map (and then every enrolled client is refused).
pub fn read_grants(path: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_grants(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Refuse a grants file in which any grant names a token [`grant_refusal`] refuses.
///
/// Run before the store opens and before either identity door reads the file, so a grant
/// that would hand a remote client the whole dataset, the whole shell or the backup family
/// stops the server instead of admitting one connection under it. It is the STARTUP copy of
/// the one rule; [`scopes_for_grant`] is the per-use copy, and both are [`grant_refusal`].
pub fn check_grants(grants: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    for (name, scopes) in grants {
        if let Some(refusal) = grant_refusal(name, scopes) {
            return Err(refusal);
        }
    }
    Ok(())
}

/// ★★ **The one decision about which tokens no identity this server admits may hold** — and
/// every path that turns a grant into scopes goes through it: the startup check
/// ([`check_grants`]), every QUIC connection and every passkey request
/// ([`scopes_for_grant`], through [`authority`] and `identity::scopes_of`), the headless
/// reviewer (`trigger::reviewer_scopes`), and both writers of `grants.json` ([`put_grant`],
/// [`enrol`]).
///
/// Three families, each a token whose SHAPE says "all of them" or "the airlock":
///
/// - the store's broad tokens ([`broad_store_scopes`]): every graph, or `DROP ALL`;
/// - the offering wildcards (`wildcard_refusal`): every program, or every host;
/// - the backup family ([`crate::grants::gonk_admin_scopes`]): every graph in one file, and a
///   store built at a path the caller names. Reachable from the owner-only socket only.
///
/// ⚠ Audit round 4 (ledger [#864](http://localhost:1060/l/default/item/864), R1) found the
/// third family checked at STARTUP and nowhere else: `grants.json` is re-read per connection
/// precisely so an edit takes effect, and an edit after startup put `urn:cap:gonk:restore` on
/// a QUIC client, which then wrote a RocksDB store at a path it chose. The list lived in four
/// places and one of them had all three; this function is the list, once.
pub fn grant_refusal(name: &str, scopes: &[String]) -> Option<String> {
    let broad = broad_store_scopes(scopes);
    if !broad.is_empty() {
        return Some(broad_refusal(name, &broad));
    }
    if let Some(refusal) = wildcard_refusal(name, scopes) {
        return Some(refusal);
    }
    let admin = crate::grants::gonk_admin_scopes(scopes);
    if !admin.is_empty() {
        return Some(format!(
            "grant `{name}` names {} — the backup family's tokens. A backup is every graph in \
             the dataset in one file and a restore builds a store from bytes the caller \
             supplies; neither belongs on a certificate or a passkey. They are reachable from \
             the owner-only socket, whose caller can read the dataset's files anyway",
            admin.join(" and ")
        ));
    }
    None
}

/// The offering wildcards that must never be a grant, and why each is not.
///
/// ★ **A wildcard is an offering form, not a grant form, and the two are the same string.**
/// `ikigai-repo` declares `urn:cap:exec:*` and `ikigai-browse` declares `urn:cap:net:*` to
/// mean "holds some grant under this prefix"; copied into `grants.json` each means the
/// opposite of the narrowing a reader would take it for. Both are refused at startup and
/// again per connection, because the file is re-read per connection and an edit after
/// startup is exactly when this would slip in.
fn wildcard_refusal(name: &str, scopes: &[String]) -> Option<String> {
    let exec = crate::grants::unbounded_exec_scopes(scopes);
    if !exec.is_empty() {
        return Some(format!(
            "grant `{name}` names {} — the OFFERING wildcard `ikigai-repo` declares, which \
             as a grant is every program on this machine. Name the tools instead: \
             `urn:cap:exec:git`, `urn:cap:exec:gh`",
            exec.join(" and ")
        ));
    }
    let net = crate::grants::unbounded_net_scopes(scopes);
    if !net.is_empty() {
        return Some(format!(
            "grant `{name}` names {} — the OFFERING wildcard `ikigai-browse` declares on \
             every derivation, which as a grant is every host this kernel could dial. Name \
             the host instead: `urn:cap:net:localhost` for a mounted peer on this machine",
            net.join(" and ")
        ));
    }
    None
}

fn broad_refusal(grant: &str, broad: &[String]) -> String {
    format!(
        "grant `{grant}` names {} — the store's whole-dataset tokens. This server hands out \
         per-ledger grants only; use `ikigai-gonk grants <ledger> <authority>` for the \
         tokens a ledger caller needs",
        broad.join(" and ")
    )
}

/// The authority one authenticated fingerprint runs under, or the reason it is refused.
pub fn authority(
    enrolment: &Enrolment,
    grants: &BTreeMap<String, Vec<String>>,
    fingerprint: &str,
) -> Result<(String, Capability), String> {
    let grant = enrolment
        .grant_for(fingerprint)
        .ok_or("no grant is configured for this certificate")?;
    let scopes = scopes_for_grant(grants, grant)?;
    Ok((grant.to_string(), Capability::scoped(scopes)))
}

/// A grant name's scopes, **fail closed** — the one rule both identity doors apply: a grant
/// that is unknown, that names no scopes, or that names any token [`grant_refusal`] refuses
/// is refused. The QUIC door calls it per connection; the HTTP door per request carrying a
/// passkey session; the trigger once, for the reviewer.
pub fn scopes_for_grant(
    grants: &BTreeMap<String, Vec<String>>,
    grant: &str,
) -> Result<Vec<String>, String> {
    let scopes = grants
        .get(grant)
        .filter(|scopes| !scopes.is_empty())
        .ok_or_else(|| {
            format!("grant `{grant}` is unknown or grants no scopes (check grants.json)")
        })?;
    if let Some(refusal) = grant_refusal(grant, scopes) {
        return Err(refusal);
    }
    Ok(scopes.clone())
}

/// What writing a scope list under a grant name would do to the grant already there.
///
/// ★ **A silent narrowing of authority is the same class of defect as a silent widening, and
/// arguably worse**, because nothing fails until someone clicks a button (ledger
/// [#435](http://localhost:1060/l/default/item/435)). `passkey invite brian --force` used to
/// rewrite a hand-widened grant back to what the flags could express and say nothing about
/// what it dropped; the feature that needed the dropped scopes went dark with no signal. So
/// every rewrite of an existing grant is described in both directions — a refusal names what
/// it WOULD remove and add, and a forced write prints what it DID.
///
/// Compared as SETS: a reordering is no change, so it is neither refused nor reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantChange {
    /// Whether a grant of that name was already in the file.
    pub existed: bool,
    /// Scopes the existing grant holds that the new list does not — what a write takes away.
    pub removed: Vec<String>,
    /// Scopes the new list holds that the existing grant does not — what a write hands out.
    pub added: Vec<String>,
}

impl GrantChange {
    /// The change from `existing` (a grant's JSON value, if there is one) to `wanted`.
    ///
    /// A malformed existing value — not an array, or with non-string members — is read as
    /// the strings it does hold, so a forced write can still replace it and say what it
    /// replaced.
    pub fn against(existing: Option<&Value>, wanted: &[String]) -> GrantChange {
        let Some(existing) = existing else {
            return GrantChange::default();
        };
        let held: Vec<String> = existing
            .as_array()
            .map(|scopes| {
                scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        GrantChange {
            existed: true,
            removed: held
                .iter()
                .filter(|scope| !wanted.contains(scope))
                .cloned()
                .collect(),
            added: wanted
                .iter()
                .filter(|scope| !held.contains(scope))
                .cloned()
                .collect(),
        }
    }

    /// Whether the write would change an EXISTING grant — what needs `--force`.
    pub fn changes(&self) -> bool {
        self.existed && !(self.removed.is_empty() && self.added.is_empty())
    }

    /// Whether the write would take authority away from an existing grant.
    pub fn narrows(&self) -> bool {
        self.existed && !self.removed.is_empty()
    }

    /// One line per scope, `removes` first: what an operator reads before (or after) a
    /// `--force`.
    ///
    /// ```
    /// use ikigai_gonk::quic::GrantChange;
    /// let existing = serde_json::json!(["urn:cap:net:localhost", "urn:cap:annotate"]);
    /// let change = GrantChange::against(
    ///     Some(&existing),
    ///     &["urn:cap:annotate".to_string(), "urn:cap:net:127.0.0.1".to_string()],
    /// );
    /// assert_eq!(
    ///     change.lines(),
    ///     "    removes  urn:cap:net:localhost\n    adds     urn:cap:net:127.0.0.1"
    /// );
    /// ```
    pub fn lines(&self) -> String {
        self.removed
            .iter()
            .map(|scope| format!("    removes  {scope}"))
            .chain(
                self.added
                    .iter()
                    .map(|scope| format!("    adds     {scope}")),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The refusal for a change written without `--force`, naming every scope it would move.
    pub fn refusal(&self, grant: &str, grants_json: &Path) -> String {
        let how = match (self.removed.is_empty(), self.added.is_empty()) {
            (false, true) => "NARROW it",
            (false, false) => "NARROW it and widen it",
            _ => "widen it",
        };
        format!(
            "grant `{grant}` already exists in {} with different scopes — nothing was \
             written. Replacing it would {how}:\n{}\nUse --force to replace it, which \
             changes every identity enrolled under it",
            grants_json.display(),
            self.lines()
        )
    }
}

/// The change writing `scopes` under `grant` would make to `grants.json` as it is now — read
/// BEFORE anything is written, so a command can refuse (or describe) the rewrite before it
/// mints a bundle or an invite.
pub fn grant_change(
    layout: &Layout,
    grant: &str,
    scopes: &[String],
) -> Result<GrantChange, String> {
    let grants = read_object(&layout.grants_json())?;
    Ok(GrantChange::against(grants.get(grant), scopes))
}

/// Write one grant into `grants.json` alone — for an identity that is not a certificate (a
/// passkey invite). The same refusals as [`enrol`]: [`grant_refusal`] always, and an
/// existing grant with different scopes unless `force`, because a grant name may be shared by
/// a certificate and a passkey and replacing it changes both.
pub fn put_grant(
    layout: &Layout,
    grant: &str,
    scopes: &[String],
    force: bool,
) -> Result<(), String> {
    valid_name(grant)?;
    if let Some(refusal) = grant_refusal(grant, scopes) {
        return Err(refusal);
    }
    private_dir(&layout.dir)?;
    let mut grants = read_object(&layout.grants_json())?;
    let change = GrantChange::against(grants.get(grant), scopes);
    if change.changes() && !force {
        return Err(change.refusal(grant, &layout.grants_json()));
    }
    grants.insert(grant.to_string(), json!(scopes));
    let text = pretty(&Value::Object(grants))?;
    parse_grants(&text)?;
    write_private(&layout.grants_json(), &text)
}

/// The per-connection minter: re-reads both files, and refuses — logging the full
/// fingerprint and the fix — whenever [`authority`] does.
pub fn minter(layout: Layout) -> Minter {
    Arc::new(move |peer: &PeerIdentity| {
        let decided = read_enrolment(&layout.clients_json())
            .and_then(|enrolment| {
                enrolment.ok_or_else(|| {
                    format!("no enrolment file at {}", layout.clients_json().display())
                })
            })
            .and_then(|enrolment| {
                let grants = read_grants(&layout.grants_json())?;
                authority(&enrolment, &grants, &peer.fingerprint)
            });
        match decided {
            Ok((grant, capability)) => {
                eprintln!(
                    "ikigai-gonk: quic client {} → grant \"{grant}\"",
                    &peer.fingerprint[..peer.fingerprint.len().min(16)]
                );
                Some(Session {
                    capability,
                    file_segment: peer.segment_id.clone(),
                })
            }
            Err(why) => {
                eprintln!(
                    "ikigai-gonk: REFUSED a trusted client certificate — {why}\n  \
                     fingerprint: {}\n  \
                     enrol it with `ikigai-gonk client add <its name> --ledger <ledger>=<authority>`",
                    peer.fingerprint
                );
                None
            }
        }
    })
}

/// Everything [`enrol`] would refuse, checked WITHOUT writing — so `client add` can ask
/// before it mints a key pair or writes a bundle (ledger
/// [#805](http://localhost:1060/l/default/item/805), part 3). Returns what the write would do
/// to an existing grant, for the caller to report.
///
/// `fingerprint` is `None` when it is not known yet (a key pair about to be minted is a new
/// identity, so it cannot already be enrolled under anything).
///
/// # Errors
///
/// A name that cannot be a grant, any token [`grant_refusal`] refuses, an existing grant
/// with different scopes without `force`, or a fingerprint already enrolled under a different
/// grant without `force`.
pub fn enrol_refusal(
    layout: &Layout,
    grant: &str,
    fingerprint: Option<&str>,
    scopes: &[String],
    force: bool,
) -> Result<GrantChange, String> {
    valid_name(grant)?;
    if let Some(refusal) = grant_refusal(grant, scopes) {
        return Err(refusal);
    }
    let grants = read_object(&layout.grants_json())?;
    let change = GrantChange::against(grants.get(grant), scopes);
    if change.changes() && !force {
        return Err(change.refusal(grant, &layout.grants_json()));
    }
    if let Some(fingerprint) = fingerprint {
        let mut clients_doc = read_object(&layout.clients_json())?;
        let clients = match clients_doc.remove("clients") {
            None => Map::new(),
            Some(Value::Object(map)) => map,
            Some(_) => return Err("`clients` must be an object".to_string()),
        };
        if let Some(existing) = clients.get(&normalize(fingerprint)) {
            let current = existing
                .as_str()
                .or_else(|| existing.get("grant").and_then(Value::as_str));
            if current != Some(grant) && !force {
                return Err(format!(
                    "this certificate is already enrolled under grant `{}` — nothing was \
                     written (use --force to replace it)",
                    current.unwrap_or("?")
                ));
            }
        }
    }
    Ok(change)
}

/// Enrol `fingerprint` under a grant called `grant` holding `scopes`, writing both files.
///
/// Both files are checked before either is written ([`enrol_refusal`]): any token
/// [`grant_refusal`] refuses always, and an existing grant with different scopes, or a
/// fingerprint already enrolled under a different grant, unless `force`.
pub fn enrol(
    layout: &Layout,
    grant: &str,
    fingerprint: &str,
    scopes: &[String],
    force: bool,
) -> Result<(), String> {
    enrol_refusal(layout, grant, Some(fingerprint), scopes, force)?;
    let mut grants = read_object(&layout.grants_json())?;
    let mut clients_doc = read_object(&layout.clients_json())?;
    let key = normalize(fingerprint);
    let mut clients = match clients_doc.remove("clients") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`clients` must be an object".to_string()),
    };
    grants.insert(grant.to_string(), json!(scopes));
    clients.insert(key, json!({ "grant": grant, "label": grant }));
    clients_doc.insert("clients".to_string(), Value::Object(clients));

    let grants_text = pretty(&Value::Object(grants))?;
    let clients_text = pretty(&Value::Object(clients_doc))?;
    // What is written must be what the server will read.
    parse_grants(&grants_text)?;
    parse_enrolment(&clients_text)?;
    write_private(&layout.grants_json(), &grants_text)?;
    write_private(&layout.clients_json(), &clients_text)
}

/// A JSON object file, or an empty object when the file does not exist.
pub(crate) fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(format!("{}: must be a JSON object", path.display())),
            Err(e) => Err(format!("{}: not valid JSON: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Pretty JSON with a trailing newline — how every authority file here is written.
pub(crate) fn pretty(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|text| text + "\n")
        .map_err(|e| e.to_string())
}

/// A bundle name is a directory name and a grant name: lowercase letters, digits, `-`, `_`.
fn valid_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "client name `{name}`: use lowercase letters, digits, `-` and `_` (at most 64)"
        ))
    }
}

/// Lowercase, no colons or whitespace.
fn normalize(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .flat_map(char::to_lowercase)
        .collect()
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// Write `contents` to `path`, readable by this user only, **atomically**.
///
/// Written to a temporary file beside the target and renamed over it, because
/// `clients.json` and `grants.json` are re-read on every QUIC connection: a plain write
/// truncates first, and a connection arriving in that window would read a partial file and
/// refuse a client that is enrolled. A rename is seen as the old file or the new one. The
/// temporary file is created `0600` rather than restricted afterwards, so a key is never
/// briefly readable at the process umask.
pub(crate) fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.tmp"));
    create_private(&temp, contents).map_err(|e| format!("writing {}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("replacing {}: {e}", path.display()))
}

#[cfg(unix)]
fn create_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` applies only when the file is created; a leftover temporary keeps its old mode.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

#[cfg(not(unix))]
fn create_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    restrict(path, 0o700)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("restricting {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{grants_for, Authority};

    const FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";

    fn grants() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            (
                "rw".to_string(),
                grants_for("default", Authority::Write).unwrap(),
            ),
            ("empty".to_string(), Vec::new()),
            ("broad".to_string(), vec!["urn:cap:store:write".to_string()]),
        ])
    }

    fn enrolled(grant: &str) -> Enrolment {
        parse_enrolment(&format!(
            r#"{{"clients": {{"{FP}": {{"grant": "{grant}"}}}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn an_enrolled_certificate_gets_exactly_its_grant() {
        let (grant, capability) = authority(&enrolled("rw"), &grants(), FP).unwrap();
        assert_eq!(grant, "rw");
        assert!(capability.allows("urn:cap:ledger:write:default"));
        assert!(!capability.allows("urn:cap:ledger:write:acme"));
        assert!(!capability.allows("urn:cap:store:read"));
    }

    /// FAIL CLOSED: a stranger, an empty grant, and an unknown grant are all refused.
    #[test]
    fn everything_undecided_is_refused() {
        let stranger = "00".repeat(32);
        assert!(authority(&enrolled("rw"), &grants(), &stranger).is_err());
        assert!(authority(&enrolled("empty"), &grants(), FP).is_err());
        assert!(authority(&enrolled("typo"), &grants(), FP).is_err());
    }

    #[test]
    fn a_grant_naming_a_broad_store_token_is_refused_at_both_checks() {
        let refused = authority(&enrolled("broad"), &grants(), FP).unwrap_err();
        assert!(refused.contains("urn:cap:store:write"), "{refused}");
        assert!(check_grants(&grants()).is_err());
    }

    /// ★ New with the browse family: `urn:system:exec` is compiled in, so a grants file can
    /// now name the exec wildcard — and a wildcard written as a grant is the opposite of what
    /// it looks like. The per-tool spelling is accepted, because that is what `ikigai-repo`
    /// enforces at dispatch.
    #[test]
    fn a_grant_naming_the_exec_wildcard_is_refused_but_a_tool_is_not() {
        let mut wildcard = BTreeMap::new();
        wildcard.insert(
            "shell".to_string(),
            vec![crate::grants::CAP_EXEC_ANY.to_string()],
        );
        let refused = check_grants(&wildcard).unwrap_err();
        assert!(refused.contains("urn:cap:exec:*"), "{refused}");
        assert!(
            refused.contains("urn:cap:exec:git"),
            "names the fix: {refused}"
        );

        let mut narrow = BTreeMap::new();
        narrow.insert(
            "builder".to_string(),
            vec![
                "urn:cap:exec:git".to_string(),
                "urn:cap:browse:read:core".to_string(),
            ],
        );
        assert!(check_grants(&narrow).is_ok(), "a tool grant is legitimate");
    }

    #[test]
    fn an_openssl_fingerprint_names_the_same_client_and_a_default_must_be_explicit() {
        let colons = "6F:1C:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:AB:CD";
        let e = parse_enrolment(&format!(r#"{{"clients": {{"{colons}": "rw"}}}}"#)).unwrap();
        assert_eq!(e.grant_for(FP), Some("rw"));
        assert_eq!(e.grant_for(&"00".repeat(32)), None);
        let with_default = parse_enrolment(r#"{"default": "rw"}"#).unwrap();
        assert_eq!(with_default.grant_for(FP), Some("rw"));
        assert!(parse_enrolment("{ nope").is_err());
        assert!(parse_grants(r#"{"x": [1]}"#).is_err());
    }

    #[test]
    fn client_add_then_enrol_writes_files_the_server_reads() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let bundle = add_client(&layout, "laptop", None, false).unwrap();
        assert!(bundle.key_minted);
        assert_eq!(bundle.fingerprint.len(), 64);
        for file in ["client.crt", "client.key", "server.crt"] {
            assert!(bundle.dir.join(file).is_file(), "{file}");
        }
        // Re-adding reuses the identity rather than replacing it.
        let again = add_client(&layout, "laptop", None, false).unwrap();
        assert_eq!(again.fingerprint, bundle.fingerprint);
        assert!(!again.key_minted);

        let scopes = grants_for("default", Authority::Write).unwrap();
        enrol(&layout, "laptop", &bundle.fingerprint, &scopes, false).unwrap();
        let enrolment = read_enrolment(&layout.clients_json()).unwrap().unwrap();
        let grants = read_grants(&layout.grants_json()).unwrap();
        let (grant, _) = authority(&enrolment, &grants, &bundle.fingerprint).unwrap();
        assert_eq!(grant, "laptop");
        assert_eq!(trusted_client_certs(&layout).unwrap().len(), 1);

        // A different scope set under the same grant name is refused without --force…
        let read_only = grants_for("default", Authority::Read).unwrap();
        assert!(enrol(&layout, "laptop", &bundle.fingerprint, &read_only, false).is_err());
        // …and the broad token is refused even with it.
        let broad = vec!["urn:cap:store:read".to_string()];
        assert!(enrol(&layout, "laptop", &bundle.fingerprint, &broad, true).is_err());
        assert!(add_client(&layout, "Bad Name", None, false).is_err());
    }

    /// ★ **A rewrite of an existing grant says what it moves, in both directions** (ledger
    /// #435). The live case: a hand-widened passkey grant re-enrolled with flags that cannot
    /// express all of it. Without `--force` it is refused and the refusal NAMES the scopes it
    /// would drop and the ones it would add; with `--force` the write happens and
    /// [`grant_change`], asked first, is what the CLI prints. A reorder is no change.
    #[test]
    fn rewriting_a_grant_names_what_it_removes_and_adds() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let owned = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let hand_widened = owned(&[
            "urn:cap:ledger:read:default",
            "urn:cap:browse:read:*",
            "urn:cap:annotate",
            "urn:cap:net:localhost",
            "urn:cap:exec:gh",
        ]);
        put_grant(&layout, "brian", &hand_widened, false).unwrap();
        // A new grant is no change, and a reordering of the same set is none either.
        let mut reordered = hand_widened.clone();
        reordered.reverse();
        assert!(!grant_change(&layout, "brian", &reordered)
            .unwrap()
            .changes());
        put_grant(&layout, "brian", &reordered, false).expect("the same set, reordered");

        let reenrolled = owned(&[
            "urn:cap:ledger:read:default",
            "urn:cap:browse:read:*",
            "urn:cap:annotate",
            "urn:cap:net:127.0.0.1",
        ]);
        let change = grant_change(&layout, "brian", &reenrolled).unwrap();
        assert!(change.changes() && change.narrows());
        // In the file's order — which the reorder above reversed.
        assert_eq!(change.removed, ["urn:cap:exec:gh", "urn:cap:net:localhost"]);
        assert_eq!(change.added, ["urn:cap:net:127.0.0.1"]);

        let refused = put_grant(&layout, "brian", &reenrolled, false).unwrap_err();
        assert!(refused.contains("NARROW it and widen it"), "{refused}");
        for line in [
            "    removes  urn:cap:net:localhost",
            "    removes  urn:cap:exec:gh",
            "    adds     urn:cap:net:127.0.0.1",
        ] {
            assert!(refused.contains(line), "`{line}` in: {refused}");
        }
        assert_eq!(
            read_grants(&layout.grants_json()).unwrap()["brian"],
            reordered,
            "nothing was written"
        );

        // The certificate path refuses with the same list.
        let bundle = add_client(&layout, "brian", None, false).unwrap();
        let by_cert = enrol(&layout, "brian", &bundle.fingerprint, &reenrolled, false).unwrap_err();
        assert!(
            by_cert.contains("    removes  urn:cap:exec:gh"),
            "{by_cert}"
        );

        // --force writes it.
        put_grant(&layout, "brian", &reenrolled, true).unwrap();
        assert_eq!(
            read_grants(&layout.grants_json()).unwrap()["brian"],
            reenrolled
        );

        // A pure widening is refused too, and says it widens.
        let mut wider = reenrolled.clone();
        wider.push("urn:cap:ledger:write:default".to_string());
        let widening = put_grant(&layout, "brian", &wider, false).unwrap_err();
        assert!(widening.contains("would widen it"), "{widening}");
        assert!(!widening.contains("removes"), "{widening}");
        assert!(widening.contains("    adds     urn:cap:ledger:write:default"));
        let change = grant_change(&layout, "brian", &wider).unwrap();
        assert!(change.changes() && !change.narrows());
    }

    fn at(spelled: &str) -> SocketAddr {
        spelled.parse().unwrap()
    }

    fn off_reason(door: Result<QuicDoor, String>) -> String {
        match door {
            Ok(QuicDoor::Off(why)) => why,
            Ok(QuicDoor::Open { .. }) => panic!("the door opened"),
            Err(e) => panic!("refused: {e}"),
        }
    }

    fn refusal(door: Result<QuicDoor, String>) -> String {
        match door {
            Err(e) => e,
            Ok(QuicDoor::Off(why)) => panic!("stayed off instead of refusing: {why}"),
            Ok(QuicDoor::Open { .. }) => panic!("the door opened"),
        }
    }

    /// ★ gonk PENDING §3: a file holding only passkeys is not a decision to face the network.
    #[test]
    fn a_passkey_only_clients_json_keeps_the_default_door_shut_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let default = QuicBind::Default(at("0.0.0.0:1060"));

        let none = off_reason(open_door(&layout, default));
        assert!(none.contains("no client certificate is enrolled"), "{none}");

        private_dir(&layout.dir).unwrap();
        write_private(
            &layout.clients_json(),
            r#"{"passkeys": {"cred": {"grant": "brian"}}}"#,
        )
        .unwrap();
        let passkeys = off_reason(open_door(&layout, default));
        assert!(
            passkeys.contains("enrols no client certificate"),
            "{passkeys}"
        );
        assert!(passkeys.contains("ikigai-gonk client add"), "{passkeys}");
        assert!(!layout.quic_dir().exists(), "no identity for a shut door");

        // Named, the same file refuses — and still generates nothing.
        let named = refusal(open_door(&layout, QuicBind::Explicit(at("127.0.0.1:0"))));
        assert!(named.contains("asked for"), "{named}");
        assert!(!layout.quic_dir().exists());

        assert_eq!(
            off_reason(open_door(&layout, QuicBind::Off)),
            "off (--no-quic)"
        );
    }

    #[test]
    fn an_enrolled_certificate_opens_the_default_door_beside_passkeys() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let bundle = add_client(&layout, "laptop", None, false).unwrap();
        // Enrolled in clients.json, with no client.crt trusted yet: refused, nothing generated.
        std::fs::remove_file(bundle.dir.join("client.crt")).unwrap();
        let scopes = grants_for("default", Authority::Write).unwrap();
        enrol(&layout, "laptop", &bundle.fingerprint, &scopes, false).unwrap();
        let mut doc = read_object(&layout.clients_json()).unwrap();
        doc.insert("passkeys".into(), json!({"cred": {"grant": "brian"}}));
        write_private(
            &layout.clients_json(),
            &pretty(&Value::Object(doc)).unwrap(),
        )
        .unwrap();
        std::fs::remove_file(layout.server_cert()).unwrap();
        std::fs::remove_file(layout.server_key()).unwrap();
        let untrusted = refusal(open_door(&layout, QuicBind::Default(at("0.0.0.0:1060"))));
        assert!(untrusted.contains("none is trusted"), "{untrusted}");
        assert!(!layout.server_cert().exists());

        let again = add_client(&layout, "laptop2", None, false).unwrap();
        assert!(again.dir.join("client.crt").is_file());
        // `client add` generates the server identity itself; remove it so the assertion below
        // shows `open_door` generating it.
        std::fs::remove_file(layout.server_cert()).unwrap();
        std::fs::remove_file(layout.server_key()).unwrap();
        match open_door(&layout, QuicBind::Default(at("0.0.0.0:1060"))) {
            Ok(QuicDoor::Open {
                addr,
                trusted,
                enrolled,
                ..
            }) => {
                assert_eq!(addr, at("0.0.0.0:1060"));
                assert_eq!((trusted.len(), enrolled), (1, 1));
            }
            Ok(QuicDoor::Off(why)) => panic!("stayed off: {why}"),
            Err(e) => panic!("refused: {e}"),
        }
        assert!(
            layout.server_cert().is_file(),
            "generated on the path that serves"
        );
        // A shared default grant is a certificate decision too.
        assert!(parse_enrolment(r#"{"default": "rw"}"#)
            .unwrap()
            .admits_certificates());
    }
}
