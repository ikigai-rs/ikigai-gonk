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
//! as unrestricted. A grant naming either of the store's broad tokens is refused — at
//! startup, and again per connection in case the file was edited after. An enrolment file
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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ikigai_core::Capability;
use ikigai_quic::{Identity, Minter, PeerIdentity, Session};
use serde_json::{json, Map, Value};

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

/// Refuse a grants file in which any grant names a broad store token.
pub fn check_grants(grants: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    for (name, scopes) in grants {
        let broad = broad_store_scopes(scopes);
        if !broad.is_empty() {
            return Err(broad_refusal(name, &broad));
        }
    }
    Ok(())
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
/// that is unknown, that names no scopes, or that names a broad store token is refused. The
/// QUIC door calls it per connection; the HTTP door per request carrying a passkey session.
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
    let broad = broad_store_scopes(scopes);
    if !broad.is_empty() {
        return Err(broad_refusal(grant, &broad));
    }
    Ok(scopes.clone())
}

/// Write one grant into `grants.json` alone — for an identity that is not a certificate (a
/// passkey invite). The same refusals as [`enrol`]: a broad token always, and an existing
/// grant with different scopes unless `force`, because a grant name may be shared by a
/// certificate and a passkey and replacing it changes both.
pub fn put_grant(
    layout: &Layout,
    grant: &str,
    scopes: &[String],
    force: bool,
) -> Result<(), String> {
    valid_name(grant)?;
    let broad = broad_store_scopes(scopes);
    if !broad.is_empty() {
        return Err(broad_refusal(grant, &broad));
    }
    private_dir(&layout.dir)?;
    let mut grants = read_object(&layout.grants_json())?;
    let wanted = json!(scopes);
    if let Some(existing) = grants.get(grant) {
        if existing != &wanted && !force {
            return Err(format!(
                "grant `{grant}` already exists in {} with different scopes — nothing was \
                 written (use --force to replace it, which changes every identity enrolled \
                 under it)",
                layout.grants_json().display()
            ));
        }
    }
    grants.insert(grant.to_string(), wanted);
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

/// Enrol `fingerprint` under a grant called `grant` holding `scopes`, writing both files.
///
/// Both files are checked before either is written: an existing grant with different scopes,
/// or a fingerprint already enrolled under a different grant, is refused unless `force`.
pub fn enrol(
    layout: &Layout,
    grant: &str,
    fingerprint: &str,
    scopes: &[String],
    force: bool,
) -> Result<(), String> {
    let broad = broad_store_scopes(scopes);
    if !broad.is_empty() {
        return Err(broad_refusal(grant, &broad));
    }
    let mut grants = read_object(&layout.grants_json())?;
    let wanted = json!(scopes);
    if let Some(existing) = grants.get(grant) {
        if existing != &wanted && !force {
            return Err(format!(
                "grant `{grant}` already exists in {} with different scopes — nothing was \
                 written (use --force to replace it)",
                layout.grants_json().display()
            ));
        }
    }
    let mut clients_doc = read_object(&layout.clients_json())?;
    let key = normalize(fingerprint);
    let mut clients = match clients_doc.remove("clients") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`clients` must be an object".to_string()),
    };
    if let Some(existing) = clients.get(&key) {
        let current = existing
            .as_str()
            .or_else(|| existing.get("grant").and_then(Value::as_str));
        if current != Some(grant) && !force {
            return Err(format!(
                "this certificate is already enrolled under grant `{}` — nothing was written \
                 (use --force to replace it)",
                current.unwrap_or("?")
            ));
        }
    }
    grants.insert(grant.to_string(), wanted);
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
}
