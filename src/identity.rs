//! Passkey identity on the HTTP door — the same thing a client certificate is on the QUIC
//! door, arriving by a different route.
//!
//! # ★ One grant table for both doors
//!
//! A QUIC certificate fingerprint and a passkey credential are both **identities**, and
//! both map to a grant NAME in `gonk/clients.json`; the grant name maps to scopes in
//! `gonk/grants.json`, which is the only place a scope list lives:
//!
//! ```text
//! gonk/clients.json   { "clients":  { "<cert sha256>":     { "grant": "brian" } },
//!                       "passkeys": { "<credential id>":   { "grant": "brian", "label": "brian",
//!                                                             "public_key": "<SPKI, base64url>",
//!                                                             "sign_count": 0 } } }
//! gonk/grants.json    { "brian": ["urn:cap:ledger:read:default", …] }
//! gonk/invites.json   { "<sha256 of an invite code>": { "grant": "brian", "expires": 1757… } }
//! ```
//!
//! So a person with a laptop certificate and a passkey who are enrolled under one grant
//! cannot hold read over QUIC and write over HTTP by accident: there is one list to be
//! wrong in. Both files are re-read on every use — every QUIC connection, every HTTP request
//! carrying a session — so editing a grant or deleting an enrollment takes effect on the next
//! request, with no restart.
//!
//! # Registration: the INVITE is the trust anchor
//!
//! `ikigai-gonk passkey invite <name> --ledger …` writes the grant and a single-use,
//! expiring invite, and prints `http://localhost:<port>/#invite=<code>`. The browser that
//! opens it creates a credential and posts its public key with the code. Nothing here parses
//! an attestation — `ikigai-passkey` is an assertion verifier and holds no CBOR decoder —
//! because attestation answers "what kind of authenticator is this", and the question this
//! server needs answered is "was this person invited", which the code answers. Whoever holds
//! the invite IS the enrollee; that is what makes an invite a secret worth expiring.
//!
//! # Login: `ikigai_passkey::verify`, then a session
//!
//! A login answers a single-use challenge; the assertion is verified against the stored
//! key for the exact origin `http://localhost:<port>` and RP ID `localhost`, with user
//! verification REQUIRED (an identity that grants authority is a biometric or a PIN, not a
//! tap). Success mints a 256-bit session token, kept only as its SHA-256, for the session's
//! absolute lifetime at most (`gonk.http.session_max_hours`, see [`Passkeys::lifetime`]), and for
//! [`DEFAULT_IDLE_SECONDS`] (`gonk.http.session_idle_minutes`) after the last request that
//! carried it.
//!
//! # Sessions survive a restart (ledger [#3229](http://localhost:1062/l/default/item/3229))
//!
//! Every live session is written to `gonk/sessions.json` beside the enrolled passkeys: the
//! SHA-256 of its token (never the token), its principal, its grant, when it was issued, when
//! it was last used and when it ends, and how far this server has read `gonk/passkeys.log`.
//! The file is written whole, to a temporary file renamed over it, readable by this user only.
//! A server starting over the same directory loads the sessions that have not expired and
//! drops the rest, and reads the log from where the last one stopped, so a `passkey logout
//! --all` or `passkey revoke` run while it was down still ends their sessions. An ending
//! (sign-out, sign out everywhere, a revoke, an expired lifetime) is written before the
//! request that caused it answers, so nothing ended comes back after a restart.
//!
//! A session whose grant can only READ may run up to [`READ_ONLY_SESSION_SECONDS`] (30 days)
//! when the setting says so; one whose grant can do anything else ends after
//! [`SESSION_SECONDS`] (12 h) whatever the setting says. The grant is re-read on every
//! request, so a read-only grant widened to write ends its long sessions 12 h after their
//! sign-in.
//!
//! # Ending a session (ledger [#1824](http://localhost:1062/l/default/item/1824))
//!
//! A session caches AUTHENTICATION only: every request re-reads `clients.json` and
//! `grants.json`, so a narrowed grant takes effect at the next request. A REVOKED credential
//! goes further: its sessions are DELETED, never just refused, so nothing done to the files
//! afterwards (enrolling the same credential again, restoring `clients.json` from a backup)
//! brings one back. A fresh sign-in is the only way to a new session.
//!
//! `passkey revoke` runs in another process and cannot reach this table, so the cross-process
//! channel is the file it already writes: `gonk/passkeys.log`. The server tails it
//! ([`Passkeys::identity`] and [`Passkeys::login`] read what was appended since they last
//! looked), and a `gonk:PasskeyRevoked` or `gonk:PasskeySessionsEnded` line ends every session
//! of the credential it names. `passkey logout --all` writes the second; `/auth/logout-all`
//! does the same for the signed-in caller's own credential. And a request whose credential is
//! no longer enrolled deletes its session there and then, so a revoke whose log line could not
//! be written still ends it at the next request.
//!
//! Every sign-in, sign-out and session ending writes one entry into the log segment the door
//! lines go to ([`crate::access::Record`], ledger #383): [`LOGIN_CLASS`] or [`LOGOUT_CLASS`],
//! the passkey resource, then `door`, `outcome`, `principal`, `credential` (`log:passkey`),
//! `passkey` (the credential id), `sessions` and (on a logout) `why`. Never a token.
//!
//! ★ **What each of these records is, and why there is one of each** (ledger #383):
//!
//! - **The segment's sign-in entries** are HISTORY: who held a session, from when. They were
//!   stderr lines until ledger #383, which made them segment entries, hash-chained with the
//!   door lines.
//! - **`gonk/passkeys.log` stays**, because it is a COMMAND CHANNEL, not history: `passkey
//!   revoke` and `passkey logout --all` run in another process and tell this server, through it,
//!   to end sessions, and `sessions.json` holds how far it was read. The server's entry for
//!   acting on one (`door=cli`, `why=revoked`) records what the SERVER did, sessions ended, and
//!   the file line records what the operator asked, so neither repeats the other. Moving the
//!   channel onto the segment would put a second process's writes into this process's chain,
//!   which `ikigai-log` keeps to one writer per segment.
//! - **`gonk/sessions.json` stays**: it is STATE, the live session table a restart reloads,
//!   rewritten whole; a session that ended is gone from it, and its ending is in the segment.
//!
//! ⚠ **The cookie is set by the page's script, not by the server, so it is not HttpOnly**
//! ([#1591](http://localhost:1062/l/default/item/1591)). The `ikigai-web` release this server
//! builds on (0.1.63) builds every response from a kernel representation and has no way to
//! add a `Set-Cookie` header; `ikigai_web::with_headers` (ikigai-rs/ikigai-cli PR 516) is the
//! seam, and it is not in a published release yet. The mitigations are the ones that remain:
//! `SameSite=Strict`, a strict same-origin CSP with no inline script, and a token whose
//! lifetime is bounded (12 h for any grant that can write).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use ikigai_passkey::{Assertion, Policy, RegisteredCredential};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::quic::{self, Layout};

/// The relying-party ID. ⚠ An IP address is not a valid RP ID, so the page must be opened
/// as `http://localhost:<port>` — never `http://127.0.0.1:<port>` — or the browser refuses
/// to create or use a credential at all.
pub const RP_ID: &str = "localhost";

/// The cookie the page stores its session token in.
pub const SESSION_COOKIE: &str = "gonk_session";

/// How long a session whose grant can WRITE (anything beyond read) lasts at most, however
/// busy it is and whatever `gonk.http.session_max_hours` says; also that setting's default.
pub const SESSION_SECONDS: u64 = 12 * 60 * 60;

/// The longest `gonk.http.session_max_hours` may make a session, which only a READ-ONLY grant
/// gets: 30 days. Never unbounded: a stolen cookie still reads everything its grant covers.
pub const READ_ONLY_SESSION_SECONDS: u64 = 720 * 60 * 60;

/// Whether every scope in `scopes` only reads (`urn:cap:<family>:read:…`). Anything else —
/// write, claim, comment, an alias scheme, a token this rule does not know — can write, and
/// so is held to [`SESSION_SECONDS`]. An empty list is not read-only: it grants nothing, and
/// gains nothing from a longer session.
///
/// ```
/// use ikigai_gonk::identity::read_only;
/// let read = ["urn:cap:ledger:read:acme".to_string(),
///             "urn:cap:store:read:graph:urn:iki:ledger:graph:acme".to_string()];
/// assert!(read_only(&read));
/// let mut write = read.to_vec();
/// write.push("urn:cap:ledger:comment:acme".to_string());
/// assert!(!read_only(&write));
/// assert!(!read_only(&[]));
/// ```
pub fn read_only(scopes: &[String]) -> bool {
    !scopes.is_empty()
        && scopes.iter().all(|s| {
            let mut parts = s.split(':');
            parts.next() == Some("urn")
                && parts.next() == Some("cap")
                && parts.next().is_some()
                && parts.next() == Some("read")
        })
}

/// How long a session lasts with no request carrying it (the idle bound), when no
/// `gonk.http.session_idle_minutes` line says otherwise: one hour.
///
/// ⚠ An open gonk page polls (the Queue badge, the Layers count), and a poll carries the
/// cookie, so a tab left open keeps its session alive to the absolute bound. The idle bound
/// ends the session of a closed tab or a sleeping machine.
pub const DEFAULT_IDLE_SECONDS: u64 = 60 * 60;

/// How long a challenge may wait for its answer.
pub const CHALLENGE_SECONDS: u64 = 5 * 60;

/// How long an invite is valid by default.
pub const INVITE_MINUTES: u64 = 30;

/// The most sessions held at once. ★ The bound REFUSES past it rather than evicting: an
/// evicted session would sign someone out with an error that names nothing. Only a passkey
/// can open a session, so no anonymous caller can fill it.
///
/// ★ Challenges are NOT bounded by it, because they are not held (ledger
/// [#2804](http://localhost:1062/l/default/item/2804)). Any process on this machine may mint
/// one, and through 44c1d03 each was kept until it expired and the 257th was refused: 256
/// header-less posts locked sign-in out for five minutes, renewably. Evicting instead would
/// only move the lockout (a flood evicts the real ceremony before the person touches the
/// key). So a challenge is SEALED ([`Passkeys::challenge`]): what it is for and when it
/// expires, under an HMAC keyed by this server. Only an ANSWERED challenge is remembered —
/// after a passkey signed it or an invite enrolled it — so only what a credential holder does
/// can grow memory.
pub const MAX_PENDING: usize = 256;

/// The IRI prefix a signed-in passkey is NAMED by when the door stamps a write — the
/// `principal` the HTTP transport attaches (see [`crate::doors::http_principal`]), and the
/// `author` the form adapter forwards to the ledger.
///
/// ★ The tail is the CREDENTIAL ID, not the label. A label is a display name an operator
/// may change (`"Touch ID"`), and two passkeys can share one; the credential id is what the
/// authenticator minted and what `clients.json` is keyed by, so a relabeled passkey keeps
/// its history and two passkeys named alike stay distinct. The IRI is what the store holds;
/// only the face translates it back to a label ([`Passkeys::labels`]).
pub const PASSKEY_IRI_PREFIX: &str = "urn:iki:gonk:passkey:";

/// The stable IRI of the passkey enrolled under `credential_id` (base64url, so the tail
/// needs no escaping).
pub fn passkey_iri(credential_id: &str) -> String {
    format!("{PASSKEY_IRI_PREFIX}{credential_id}")
}

/// The credential id a [`passkey_iri`] names, or `None` for any other text — a plain-name
/// author written by hand, or by the cli.
pub fn passkey_credential(author: &str) -> Option<&str> {
    author.strip_prefix(PASSKEY_IRI_PREFIX)
}

/// One enrolled passkey, as `clients.json` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// base64url of the credential id.
    pub credential_id: String,
    /// The grant name — a key of `grants.json`.
    pub grant: String,
    /// A human label.
    pub label: String,
    /// base64url of the SPKI DER public key.
    pub public_key: String,
    /// The highest signature counter seen.
    pub sign_count: u32,
}

/// Every enrolled passkey, by credential id. Absent file or key: none.
pub fn read_passkeys(layout: &Layout) -> Result<BTreeMap<String, Enrolled>, String> {
    let doc = quic::read_object(&layout.clients_json())?;
    parse_passkeys(&doc)
}

pub(crate) fn parse_passkeys(
    doc: &Map<String, Value>,
) -> Result<BTreeMap<String, Enrolled>, String> {
    let mut out = BTreeMap::new();
    let Some(map) = doc.get("passkeys") else {
        return Ok(out);
    };
    let map = map
        .as_object()
        .ok_or("`passkeys` in clients.json must be an object of credential id → entry")?;
    for (id, entry) in map {
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("passkey `{id}` has no `{name}`"))
        };
        out.insert(
            id.clone(),
            Enrolled {
                credential_id: id.clone(),
                grant: field("grant")?,
                label: field("label").unwrap_or_else(|_| field("grant").unwrap_or_default()),
                public_key: field("public_key")?,
                sign_count: entry
                    .get("sign_count")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
            },
        );
    }
    Ok(out)
}

/// Write one passkey entry into `clients.json`, keeping every other key (the QUIC clients,
/// the default) exactly as it was. The caller holds `_lock` from the read it decided on to
/// here, so no other change to the file lands in between and is undone (ledger #2776).
fn write_passkey(layout: &Layout, _lock: &quic::FileLock, entry: &Enrolled) -> Result<(), String> {
    let mut doc = quic::read_object(&layout.clients_json())?;
    let mut passkeys = match doc.remove("passkeys") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`passkeys` in clients.json must be an object".to_string()),
    };
    passkeys.insert(
        entry.credential_id.clone(),
        json!({
            "grant": entry.grant,
            "label": entry.label,
            "public_key": entry.public_key,
            "sign_count": entry.sign_count,
        }),
    );
    doc.insert("passkeys".to_string(), Value::Object(passkeys));
    let text = quic::pretty(&Value::Object(doc))?;
    // What is written must still be what the QUIC door reads.
    quic::parse_enrolment(&text)?;
    quic::write_private(&layout.clients_json(), &text)
}

/// Which passkey `passkey revoke` names: its label (a display name, so possibly shared) or its
/// credential id (exact).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which<'a> {
    /// Every passkey whose label is this; refused unless exactly one.
    Label(&'a str),
    /// The passkey with this credential id (base64url, as `passkey list` prints it).
    Credential(&'a str),
}

/// The file `passkey revoke` appends one line to per revocation: `gonk/passkeys.log`.
pub fn audit_log(layout: &Layout) -> std::path::PathBuf {
    layout.clients_json().with_file_name("passkeys.log")
}

/// Every enrolled passkey, checked the way a change to the file is checked: `clients.json`
/// must parse as the QUIC door reads it AND every passkey entry must be whole. A file that is
/// not is reported, never repaired: `Err` names what is wrong.
pub fn checked_passkeys(layout: &Layout) -> Result<BTreeMap<String, Enrolled>, String> {
    let path = layout.clients_json();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            quic::parse_enrolment(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    }
    read_passkeys(layout).map_err(|e| format!("{}: {e}", path.display()))
}

/// Remove ONE passkey from `clients.json` and append a `gonk:PasskeyRevoked` line to
/// [`audit_log`] — what `ikigai-gonk passkey revoke` runs (ledger
/// [#313](http://localhost:1062/l/default/item/313)). Returns the passkey removed.
///
/// ★ **Checked before and after, or nothing is written.** The file as it is must be whole
/// ([`checked_passkeys`]); a label must name exactly one passkey (two can share one, and
/// guessing between them would remove the wrong person); and the file as it WILL be must still
/// parse as both doors read it. A malformed file is refused rather than rewritten, so this
/// command can never be the edit that leaves the doors reading garbage.
///
/// Takes effect at the passkey's next request: the HTTP door re-reads `clients.json` for every
/// request that carries a session ([`Passkeys::identity`]), and the server, reading this
/// command's [`REVOKED`] line, DELETES every session of the passkey, so enrolling the same
/// credential again or restoring the file cannot revive one (ledger #1824).
/// `grants.json` is left alone, as `client remove` leaves it: a grant name may be shared.
///
/// The audit line is written AFTER the change, so it never claims a revocation that did not
/// happen; if it cannot be written the change stands and the error says so.
pub fn revoke(layout: &Layout, which: Which<'_>, now_ms: u64) -> Result<Enrolled, String> {
    let lock = layout.lock_clients()?;
    let target = pick(layout, which)?;
    let mut doc = quic::read_object(&layout.clients_json())?;
    let Some(Value::Object(mut passkeys)) = doc.remove("passkeys") else {
        return Err("`passkeys` in clients.json must be an object".to_string());
    };
    passkeys.remove(&target.credential_id);
    doc.insert("passkeys".to_string(), Value::Object(passkeys));
    let text = quic::pretty(&Value::Object(doc))?;
    quic::parse_enrolment(&text)?;
    quic::write_private(&layout.clients_json(), &text)?;
    drop(lock);
    record(layout, REVOKED, &target, now_ms).map_err(|e| {
        format!(
            "passkey `{}` WAS revoked, but its audit line could not be written to {}: {e}",
            target.label,
            audit_log(layout).display()
        )
    })?;
    Ok(target)
}

/// The class of a sign-in's entry in the log segment ([`Passkeys::auditing`]).
pub const LOGIN_CLASS: &str = "urn:iki:gonk:class:Login";

/// The class of a sign-out's, or a session ending's, entry in the log segment.
pub const LOGOUT_CLASS: &str = "urn:iki:gonk:class:Logout";

/// The class of the [`audit_log`] line [`revoke`] writes.
pub const REVOKED: &str = "gonk:PasskeyRevoked";

/// The class of the [`audit_log`] line [`end_sessions`] writes.
pub const SESSIONS_ENDED: &str = "gonk:PasskeySessionsEnded";

/// End every session of ONE passkey without revoking it — what `ikigai-gonk passkey logout
/// --all` runs (ledger [#1824](http://localhost:1062/l/default/item/1824)). Returns the passkey.
///
/// The sessions live in the server's memory and this runs in another process, so it appends a
/// [`SESSIONS_ENDED`] line to [`audit_log`] and the server ends them when it next reads the
/// log: at its next request that carries any session, or its next sign-in, whichever comes
/// first (see the module docs). `clients.json` is not written, so the passkey can sign in again
/// at once. A passkey is named exactly as [`revoke`] names one, and refused the same way.
pub fn end_sessions(layout: &Layout, which: Which<'_>, now_ms: u64) -> Result<Enrolled, String> {
    let target = pick(layout, which)?;
    record(layout, SESSIONS_ENDED, &target, now_ms).map_err(|e| {
        format!(
            "nothing was ended: the line for passkey `{}` could not be written to {}: {e}",
            target.label,
            audit_log(layout).display()
        )
    })?;
    Ok(target)
}

/// Append one `class` line naming `target` to [`audit_log`].
fn record(layout: &Layout, class: &str, target: &Enrolled, now_ms: u64) -> std::io::Result<()> {
    let line = format!(
        "{} {class} {} label={} grant={}\n",
        crate::backup::stamp_iso_ms(now_ms),
        passkey_iri(&target.credential_id),
        encoded(&target.label),
        encoded(&target.grant),
    );
    append_line(&audit_log(layout), &line)
}

/// The ONE enrolled passkey `which` names, from a file that is whole ([`checked_passkeys`]).
fn pick(layout: &Layout, which: Which<'_>) -> Result<Enrolled, String> {
    let enrolled = checked_passkeys(layout)?;
    let matches: Vec<&Enrolled> = enrolled
        .values()
        .filter(|entry| match which {
            Which::Label(label) => entry.label == label,
            Which::Credential(id) => entry.credential_id == id,
        })
        .collect();
    let target = match (matches.as_slice(), which) {
        ([one], _) => (*one).clone(),
        ([], Which::Label(label)) => {
            return Err(format!(
                "no passkey is labeled `{label}` — `ikigai-gonk passkey list` shows what there is"
            ))
        }
        ([], Which::Credential(id)) => {
            return Err(format!(
                "no passkey has credential id `{id}` — `ikigai-gonk passkey list` shows what \
                 there is"
            ))
        }
        (many, _) => {
            let ids: Vec<&str> = many.iter().map(|e| e.credential_id.as_str()).collect();
            return Err(format!(
                "{} passkeys are labeled that way ({}): name one with --credential <id>, and \
                 nothing was written",
                many.len(),
                ids.join(", ")
            ));
        }
    };
    Ok(target)
}

/// A value as one audit-line token: everything but unreserved IRI characters percent-encoded,
/// so a label with a space (`Touch ID`) cannot split the line.
///
/// ⚠ **Deliberately NOT `ikigai_fn::html::url_segment`**: like `access.rs`'s `q` column this
/// is a LOG-LINE rule, not a URL rule. It keeps `:` and `@` so a grant IRI or a principal
/// reads plainly in the audit line, and spells an empty value `-`, the log's own "none". The
/// shared rule would encode both characters and leave an empty field.
fn encoded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() {
        "-".to_string()
    } else {
        out
    }
}

/// Append `line` to `path`, creating it readable by this user only.
fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(line.as_bytes())
}

/// Create an invite for `grant`, writing the grant's scopes into `grants.json` first. Returns
/// the one-time code — the only copy; the file keeps its SHA-256.
///
/// An existing grant with DIFFERENT scopes is refused unless `force`, exactly as
/// `ikigai-gonk client add` refuses — the same grant name may be shared by a certificate and
/// a passkey, and that sharing is the point, so silently replacing it would change the other
/// identity's authority too.
pub fn invite(
    layout: &Layout,
    grant: &str,
    scopes: &[String],
    force: bool,
    minutes: u64,
    now: u64,
) -> Result<String, String> {
    quic::put_grant(layout, grant, scopes, force)?;
    let code = random_b64(24)?;
    // Held from the read to the rename, so a redeem landing in between is not undone (#2777).
    let _lock = layout.lock_invites()?;
    let mut doc = quic::read_object(&layout.invites_json())?;
    // Expired invites are dropped whenever the file is written, so it does not grow.
    doc.retain(|_, entry| entry.get("expires").and_then(Value::as_u64).unwrap_or(0) > now);
    doc.insert(
        sha256_hex(code.as_bytes()),
        json!({ "grant": grant, "expires": now + minutes * 60 }),
    );
    quic::write_private(&layout.invites_json(), &quic::pretty(&Value::Object(doc))?)?;
    Ok(code)
}

/// Consume an invite: its grant, if the code is live. Single use — removed on success, under
/// the invites lock from the read to the rename, so no concurrent [`invite`] writes it back
/// (ledger #2777). [`Passkeys::register`] calls this holding the clients lock, which is the
/// order [`quic::Layout`] takes them in.
fn redeem(layout: &Layout, code: &str, now: u64) -> Result<String, String> {
    let _lock = layout.lock_invites()?;
    let mut doc = quic::read_object(&layout.invites_json())?;
    let key = sha256_hex(code.as_bytes());
    let entry = doc
        .remove(&key)
        .ok_or("this invite is not valid — it was never issued, or it has been used")?;
    let expires = entry.get("expires").and_then(Value::as_u64).unwrap_or(0);
    let grant = entry
        .get("grant")
        .and_then(Value::as_str)
        .ok_or("the invite names no grant")?
        .to_string();
    quic::write_private(&layout.invites_json(), &quic::pretty(&Value::Object(doc))?)?;
    if expires <= now {
        return Err(
            "this invite has expired — ask for a new one (`ikigai-gonk passkey invite`)"
                .to_string(),
        );
    }
    Ok(grant)
}

/// The grant the invite `code` names, read WITHOUT redeeming it: what an enrollment with no
/// label is labeled, checked before the invite is spent. Unknown codes are refused as
/// [`redeem`] refuses them; expiry is left to [`redeem`].
fn invite_grant(layout: &Layout, code: &str) -> Result<String, String> {
    let _lock = layout.lock_invites()?;
    let doc = quic::read_object(&layout.invites_json())?;
    doc.get(&sha256_hex(code.as_bytes()))
        .ok_or("this invite is not valid — it was never issued, or it has been used")?
        .get("grant")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the invite names no grant".to_string())
}

/// Refuse `label` if an enrolled passkey already has it (ledger
/// [#3384](http://localhost:1062/l/default/item/3384)): `passkey revoke <label>` refuses a
/// label two passkeys share, and the system's passkey picker cannot tell them apart, so
/// enrollment must not create the ambiguity. Labels compare exactly, as [`revoke`] compares them.
fn refuse_taken_label(enrolled: &BTreeMap<String, Enrolled>, label: &str) -> Result<(), String> {
    match enrolled.values().find(|e| e.label == label) {
        Some(_) => Err(format!(
            "another passkey is already labeled `{label}` — choose a different label; \
             the invite is still good"
        )),
        None => Ok(()),
    }
}

/// The scopes a grant name holds, fail-closed: unknown, empty, or naming any token
/// [`quic::grant_refusal`] refuses is an error. The same rule the QUIC door applies.
pub fn scopes_of(layout: &Layout, grant: &str) -> Result<Vec<String>, String> {
    let grants = layout.current_grants()?;
    quic::scopes_for_grant(&grants, grant)
}

/// What a challenge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `navigator.credentials.create`.
    Register,
    /// `navigator.credentials.get`.
    Login,
}

impl Purpose {
    /// The byte a sealed challenge carries for it.
    fn tag(self) -> u8 {
        match self {
            Purpose::Register => b'r',
            Purpose::Login => b'l',
        }
    }
}

/// A sealed challenge's random part, in bytes.
const NONCE_BYTES: usize = 16;
/// A sealed challenge's bytes: nonce, purpose tag, expiry (big-endian seconds), HMAC-SHA256.
const SEALED_BYTES: usize = NONCE_BYTES + 1 + 8 + 32;

struct Session {
    credential_id: String,
    /// The grant the passkey was enrolled under at the sign-in (what the file records; the
    /// scopes themselves are re-read on every request).
    grant: String,
    /// When the sign-in opened it.
    issued: u64,
    /// The absolute end: [`Passkeys::lifetime`] after the sign-in.
    expires: u64,
    /// The last request that carried this session, for the idle bound.
    last_seen: u64,
    /// The `last_seen` the file holds: a request writes the file only once this is
    /// [`SEEN_WRITE_SECONDS`] behind, so a polling page does not write it every few seconds.
    stored_seen: u64,
}

/// How stale the file's `last_seen` may be before a request writes it again. A restart can
/// only make the idle bound end a session this much SOONER, never later.
const SEEN_WRITE_SECONDS: u64 = 60;

/// The `gonk/sessions.json` format version this server writes and reads.
const SESSIONS_FORMAT: u64 = 1;

impl Session {
    /// Why this session is over at `now`, or `None` while it is live.
    fn over(&self, now: u64, idle_seconds: u64) -> Option<&'static str> {
        if self.expires <= now {
            Some("expired")
        } else if self.last_seen.saturating_add(idle_seconds) <= now {
            Some("idle")
        } else {
            None
        }
    }
}

#[derive(Default)]
struct State {
    /// The key challenges are sealed under: drawn on the first challenge, never written down,
    /// so a restart voids every challenge outstanding (as the table it replaced did).
    seal_key: Option<[u8; 32]>,
    /// Every challenge already answered, until it expires: what makes one single-use.
    answered: HashMap<String, u64>,
    /// Keyed by the SHA-256 of the token — the token itself is never held.
    sessions: HashMap<String, Session>,
    /// How many bytes of [`audit_log`] this server has read: the lines past it are news.
    log_seen: u64,
    /// The sessions or `log_seen` changed since `gonk/sessions.json` was last written.
    dirty: bool,
}

impl State {
    /// Delete every session of `credential_id`; how many there were.
    fn end_sessions(&mut self, credential_id: &str) -> usize {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, s| s.credential_id != credential_id);
        let ended = before - self.sessions.len();
        self.dirty |= ended > 0;
        ended
    }

    /// The file's contents: every session, keyed by its token's hash, and the log offset.
    fn stored(&self) -> Value {
        let sessions: Map<String, Value> = self
            .sessions
            .iter()
            .map(|(hash, s)| {
                (
                    hash.clone(),
                    json!({
                        "principal": passkey_iri(&s.credential_id),
                        "grant": s.grant,
                        "issued": s.issued,
                        "last_seen": s.last_seen,
                        "expires": s.expires,
                    }),
                )
            })
            .collect();
        json!({
            "format": SESSIONS_FORMAT,
            "log_seen": self.log_seen,
            "sessions": sessions,
        })
    }
}

/// Read `gonk/sessions.json`: its sessions not yet expired at `now`, and how far the server
/// that wrote it had read the log. `None` when there is no file (a first start) or it cannot
/// be read as one this server wrote — then no session is restored, the safe direction.
fn load_sessions(layout: &Layout, now: u64) -> Option<(HashMap<String, Session>, u64, bool)> {
    let path = layout.sessions_json();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!(
                "ikigai-gonk: reading {}: {e}; no session restored",
                path.display()
            );
            return None;
        }
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!(
                "ikigai-gonk: {} is not JSON ({e}); no session restored",
                path.display()
            );
            return None;
        }
    };
    if doc.get("format").and_then(Value::as_u64) != Some(SESSIONS_FORMAT) {
        eprintln!(
            "ikigai-gonk: {} is not format {SESSIONS_FORMAT}; no session restored",
            path.display()
        );
        return None;
    }
    let log_seen = doc.get("log_seen").and_then(Value::as_u64).unwrap_or(0);
    let mut sessions = HashMap::new();
    let mut dropped = false;
    for (hash, entry) in doc
        .get("sessions")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let field = |name: &str| entry.get(name).and_then(Value::as_u64);
        let credential = entry
            .get("principal")
            .and_then(Value::as_str)
            .and_then(passkey_credential);
        let grant = entry.get("grant").and_then(Value::as_str);
        let well_formed = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        match (
            well_formed,
            credential,
            grant,
            field("issued"),
            field("last_seen"),
            field("expires"),
        ) {
            (true, Some(credential), Some(grant), Some(issued), Some(last_seen), Some(expires))
                if expires > now && expires.saturating_sub(issued) <= READ_ONLY_SESSION_SECONDS =>
            {
                sessions.insert(
                    hash.clone(),
                    Session {
                        credential_id: credential.to_string(),
                        grant: grant.to_string(),
                        issued,
                        expires,
                        last_seen,
                        stored_seen: last_seen,
                    },
                );
            }
            _ => dropped = true,
        }
    }
    Some((sessions, log_seen, dropped))
}

/// The most of [`audit_log`] one read takes, in bytes. A larger backlog is read over the next
/// requests, a whole line at a time, so a huge file costs a bounded read per request.
const LOG_READ_MAX: u64 = 1 << 20;

/// One sign-in or sign-out line's fields ([`Passkeys::audit`]).
struct AuditLine<'a> {
    /// The passkey resource's last segment: `login`, `logout`, `logout-all`, `session`.
    op: &'a str,
    /// `http`, or `cli` for a line read from [`audit_log`].
    door: &'a str,
    /// `ok`, or the refusal's kind.
    outcome: &'a str,
    /// The credential id, as enrolled or as a caller sent it.
    credential: Option<&'a str>,
    /// Whether `credential` was authenticated, so its IRI is the line's principal.
    authenticated: bool,
    /// How many sessions the event opened or ended.
    sessions: usize,
    /// Why a session ended.
    why: Option<&'a str>,
}

/// The HTTP door's passkey relying party and session table.
pub struct Passkeys {
    layout: Layout,
    origin: String,
    idle_seconds: u64,
    /// `gonk.http.session_max_hours`, in seconds: see [`Passkeys::lifetime`].
    max_seconds: u64,
    audit: Option<std::sync::Arc<crate::access::Record>>,
    state: Mutex<State>,
}

/// A signed-in identity, as a page or the capability function sees it.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The enrollment.
    pub enrolled: Enrolled,
    /// The grant's scopes, resolved now.
    pub scopes: Vec<String>,
    /// When the session ends at the latest (the absolute bound), in seconds since the epoch.
    /// It ends sooner if no request carries it for the idle bound.
    pub expires: u64,
}

impl Passkeys {
    /// A relying party for the page served at `http://localhost:{port}`, with the
    /// [`DEFAULT_IDLE_SECONDS`] idle bound, a [`SESSION_SECONDS`] lifetime and no audit lines,
    /// holding the sessions `gonk/sessions.json` keeps that have not expired by the wall clock.
    pub fn new(layout: Layout, port: u16) -> Passkeys {
        Passkeys::new_at(layout, port, now_seconds())
    }

    /// [`Passkeys::new`], with the clock reading `now`.
    ///
    /// With no `gonk/sessions.json`, lines already in [`audit_log`] are history: no session
    /// existed when they were written, so this server reads only what is appended after it
    /// starts. With one, it reads from where the server that wrote it stopped, so a sign-out
    /// written while no server ran still ends what it names. (A log now shorter than that was
    /// replaced, and is read from its start, which can only end sessions.)
    pub fn new_at(layout: Layout, port: u16, now: u64) -> Passkeys {
        let log_len = std::fs::metadata(audit_log(&layout))
            .map(|m| m.len())
            .unwrap_or(0);
        let state = match load_sessions(&layout, now) {
            Some((sessions, log_seen, dropped)) => State {
                sessions,
                log_seen: if log_seen > log_len { 0 } else { log_seen },
                dirty: dropped,
                ..State::default()
            },
            None => State {
                log_seen: log_len,
                ..State::default()
            },
        };
        let passkeys = Passkeys {
            layout,
            origin: format!("http://localhost:{port}"),
            idle_seconds: DEFAULT_IDLE_SECONDS,
            max_seconds: SESSION_SECONDS,
            audit: None,
            state: Mutex::new(state),
        };
        if let Ok(mut state) = passkeys.state.lock() {
            passkeys.read_log(&mut state);
            passkeys.save(&mut state);
        }
        passkeys
    }

    /// The same relying party with `gonk.http.session_max_hours` at `seconds`: the absolute
    /// lifetime of a session whose grant is read-only, capped at [`SESSION_SECONDS`] for any
    /// other. Clamped to [`READ_ONLY_SESSION_SECONDS`]; the setting refuses more at load.
    pub fn with_max_seconds(mut self, seconds: u64) -> Passkeys {
        self.max_seconds = seconds.clamp(1, READ_ONLY_SESSION_SECONDS);
        self
    }

    /// `gonk.http.session_max_hours`, in seconds.
    pub fn max_seconds(&self) -> u64 {
        self.max_seconds
    }

    /// How long a session whose grant holds `scopes` lasts at most: the setting if every scope
    /// only reads ([`read_only`]), and never more than [`SESSION_SECONDS`] otherwise.
    pub fn lifetime(&self, scopes: &[String]) -> u64 {
        if read_only(scopes) {
            self.max_seconds
        } else {
            self.max_seconds.min(SESSION_SECONDS)
        }
    }

    /// Write `gonk/sessions.json` if anything in it changed. A failure is said on stderr and
    /// the session table in memory stands: refusing a sign-out because the disk is full would
    /// keep the session open, the wrong direction.
    fn save(&self, state: &mut State) {
        if !state.dirty {
            return;
        }
        let path = self.layout.sessions_json();
        let written =
            quic::pretty(&state.stored()).and_then(|text| quic::write_private(&path, &text));
        match written {
            Ok(()) => {
                state.dirty = false;
                for session in state.sessions.values_mut() {
                    session.stored_seen = session.last_seen;
                }
            }
            // No `gonk/` directory: nothing is enrolled, so no session can be open to keep.
            Err(_) if state.sessions.is_empty() && !path.parent().is_some_and(|d| d.is_dir()) => {}
            Err(e) => eprintln!("ikigai-gonk: sessions not saved: {e}"),
        }
    }

    /// The same relying party with an idle bound of `seconds` (`gonk.http.session_idle_minutes`).
    pub fn with_idle_seconds(mut self, seconds: u64) -> Passkeys {
        self.idle_seconds = seconds;
        self
    }

    /// The same relying party, writing its sign-in and sign-out entries into `record`'s segment,
    /// beside the door lines: what `main` gives it.
    pub fn auditing(mut self, record: std::sync::Arc<crate::access::Record>) -> Passkeys {
        self.audit = Some(record);
        self
    }

    /// The idle bound, in seconds.
    pub fn idle_seconds(&self) -> u64 {
        self.idle_seconds
    }

    /// Write one [`LOGIN_CLASS`] / [`LOGOUT_CLASS`] entry: the passkey resource `op` names,
    /// then `door`, `outcome`, `principal`, `credential` (`log:passkey`, the KIND), `passkey`
    /// (the credential id), `sessions`, and for a logout `why`. `passkey` is base64url when it
    /// came from an enrollment and is encoded and cut when a caller sent it. Never a token.
    fn audit(&self, class: &str, line: AuditLine<'_>) {
        let Some(record) = &self.audit else {
            return;
        };
        let passkey: String = match line.credential {
            Some(id) => encoded(id).chars().take(crate::access::MAX_FIELD).collect(),
            None => "-".to_string(),
        };
        // The principal is named only once the credential is authenticated: a refused sign-in
        // names the credential it CLAIMED, and nobody.
        let principal = if line.authenticated && line.credential.is_some() {
            passkey_iri(&passkey)
        } else {
            ikigai_log::ANONYMOUS_PRINCIPAL.to_string()
        };
        let mut entry = ikigai_log::Entry::new(
            crate::access::now(),
            class,
            format!("urn:iki:gonk:passkey:{}", line.op),
        )
        .with("door", line.door)
        .with("outcome", line.outcome)
        .with(ikigai_log::PRINCIPAL_KEY, principal)
        .with("credential", ikigai_log::PASSKEY_CREDENTIAL)
        .with("passkey", passkey)
        .with("sessions", line.sessions.to_string());
        if let Some(why) = line.why {
            entry = entry.with("why", why);
        }
        record.entry(entry);
    }

    /// Read what was appended to [`audit_log`] since the last read, and end every session of a
    /// credential a [`REVOKED`] or [`SESSIONS_ENDED`] line names. A file shorter than what was
    /// read was replaced or cut: it is read again from the start, which can only end sessions.
    fn read_log(&self, state: &mut State) {
        use std::io::{Read, Seek, SeekFrom};
        let path = audit_log(&self.layout);
        let Ok(len) = std::fs::metadata(&path).map(|m| m.len()) else {
            state.log_seen = 0;
            return;
        };
        if len < state.log_seen {
            state.log_seen = 0;
            state.dirty = true;
        }
        if len == state.log_seen {
            return;
        }
        let Ok(mut file) = std::fs::File::open(&path) else {
            return;
        };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(state.log_seen)).is_err()
            || file
                .take((len - state.log_seen).min(LOG_READ_MAX))
                .read_to_end(&mut bytes)
                .is_err()
        {
            return;
        }
        // Whole lines only: a line still being written is read next time. A read that is all
        // one unended line is no line this server writes, and is stepped over.
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            if bytes.len() as u64 == LOG_READ_MAX {
                state.log_seen += LOG_READ_MAX;
                state.dirty = true;
            }
            return;
        };
        let whole = &bytes[..=end];
        for line in String::from_utf8_lossy(whole).lines() {
            let mut fields = line.split(' ');
            let (Some(_at), Some(class), Some(iri)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let why = match class {
                REVOKED => "revoked",
                SESSIONS_ENDED => "logout-all",
                _ => continue,
            };
            if let Some(credential) = passkey_credential(iri) {
                let ended = state.end_sessions(credential);
                self.audit(
                    LOGOUT_CLASS,
                    AuditLine {
                        op: "session",
                        door: "cli",
                        outcome: "ok",
                        credential: Some(credential),
                        authenticated: true,
                        sessions: ended,
                        why: Some(why),
                    },
                );
            }
        }
        state.log_seen += whole.len() as u64;
        state.dirty = true;
    }

    /// Refuse `label` if an enrolled passkey already has it — what `register-options` asks
    /// when the page names its label, so a taken one is refused BEFORE the authenticator
    /// mints a credential that could then never be enrolled. [`Passkeys::register`] checks
    /// again under the clients lock; this early answer is only a courtesy.
    pub fn check_label(&self, label: &str) -> Result<(), String> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(());
        }
        refuse_taken_label(&read_passkeys(&self.layout)?, label)
    }

    /// The only origin a ceremony is accepted from.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// How many passkeys are enrolled (for the banner). An unreadable file counts as none
    /// here; every use that matters re-reads and fails loud.
    pub fn enrolled_count(&self) -> usize {
        read_passkeys(&self.layout).map(|m| m.len()).unwrap_or(0)
    }

    /// Whether this server has no passkey at all, so no sign-in can succeed yet: what a page says
    /// beside its sign-in button (ledger [#1847](http://localhost:1062/l/default/item/1847)). Read
    /// from `clients.json` on every call, so the sentence is gone on the page after an enrollment.
    /// ★ Unlike [`Passkeys::enrolled_count`], an unreadable file is NOT "none": a page must not
    /// tell a person with passkeys to go and make one because a read failed.
    pub fn none_enrolled(&self) -> bool {
        read_passkeys(&self.layout).is_ok_and(|enrolled| enrolled.is_empty())
    }

    /// Every enrolled passkey's CURRENT label, by credential id — what a page renders a
    /// [`passkey_iri`] author as. Re-read from `clients.json` on every call, like every
    /// other use of that file, so a relabel shows on the next request; an unreadable file
    /// is an empty map here, and a caller falls back to the IRI's tail.
    pub fn labels(&self) -> BTreeMap<String, String> {
        read_passkeys(&self.layout)
            .map(|m| m.into_iter().map(|(id, e)| (id, e.label)).collect())
            .unwrap_or_default()
    }

    /// A fresh single-use challenge, base64url: a random nonce, `purpose` and the expiry,
    /// sealed by this server ([`MAX_PENDING`] says why it is not held). Minting one costs no
    /// memory, so however many are minted, the next is minted too.
    pub fn challenge(&self, purpose: Purpose, now: u64) -> Result<String, String> {
        let key = {
            let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
            match state.seal_key {
                Some(key) => key,
                None => {
                    let mut key = [0u8; 32];
                    getrandom::fill(&mut key)
                        .map_err(|e| format!("the OS random source failed: {e}"))?;
                    *state.seal_key.insert(key)
                }
            }
        };
        let mut sealed = vec![0u8; NONCE_BYTES];
        getrandom::fill(&mut sealed).map_err(|e| format!("the OS random source failed: {e}"))?;
        sealed.push(purpose.tag());
        sealed.extend_from_slice(&(now + CHALLENGE_SECONDS).to_be_bytes());
        let mac = seal(&key, &sealed).finalize().into_bytes();
        sealed.extend_from_slice(&mac);
        Ok(URL_SAFE_NO_PAD.encode(&sealed))
    }

    /// `challenge` was sealed here for `purpose` and is neither expired nor answered: when it
    /// expires. Checked first, so a ceremony refuses a foreign challenge before any work;
    /// [`Passkeys::answer_challenge`] decides again, atomically, once the ceremony succeeds.
    fn check_challenge(&self, challenge: &str, purpose: Purpose, now: u64) -> Result<u64, String> {
        let not_ours =
            || "that challenge was not issued here, or has already been answered".to_string();
        let sealed = URL_SAFE_NO_PAD.decode(challenge).map_err(|_| not_ours())?;
        if sealed.len() != SEALED_BYTES {
            return Err(not_ours());
        }
        let (body, mac) = sealed.split_at(SEALED_BYTES - 32);
        let state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        let key = state.seal_key.ok_or_else(not_ours)?;
        seal(&key, body).verify_slice(mac).map_err(|_| not_ours())?;
        if state.answered.contains_key(challenge) {
            return Err(not_ours());
        }
        let expires = u64::from_be_bytes(body[NONCE_BYTES + 1..].try_into().expect("8 bytes"));
        if body[NONCE_BYTES] != purpose.tag() || expires <= now {
            return Err(
                "that challenge has expired or was issued for another ceremony".to_string(),
            );
        }
        Ok(expires)
    }

    /// Mark `challenge` (checked, expiring at `expires`) answered, or refuse if it already
    /// is. Called only once a ceremony has SUCCEEDED — a passkey signed it, or an invite
    /// enrolled it — so a caller without either cannot grow [`State::answered`].
    fn answer_challenge(&self, challenge: &str, expires: u64, now: u64) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        state.answered.retain(|_, until| *until > now);
        if state
            .answered
            .insert(challenge.to_string(), expires)
            .is_some()
        {
            return Err(
                "that challenge was not issued here, or has already been answered".to_string(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn take_challenge(&self, challenge: &str, purpose: Purpose, now: u64) -> Result<(), String> {
        let expires = self.check_challenge(challenge, purpose, now)?;
        self.answer_challenge(challenge, expires, now)
    }

    /// Finish a registration. The body is the page's JSON:
    /// `{challenge, invite, id, publicKey, clientDataJSON, label?}` (byte fields base64url).
    pub fn register(&self, body: &[u8], now: u64) -> Result<Enrolled, String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let challenge = text("challenge")?;
        let expires = self.check_challenge(&challenge, Purpose::Register, now)?;
        let client_data = b64(&text("clientDataJSON")?, "clientDataJSON")?;
        self.check_client_data(&client_data, "webauthn.create", &challenge)?;
        let id = text("id")?;
        let key = b64(&text("publicKey")?, "publicKey")?;
        // Parsing is the check that this is an ES256 key this server can verify against.
        RegisteredCredential::from_spki_der(b64(&id, "id")?, &key, 0)
            .map_err(|e| format!("{e} — this server verifies ES256 (P-256) passkeys only"))?;
        let invite = text("invite")?;
        let lock = self.layout.lock_clients()?;
        let enrolled = read_passkeys(&self.layout)?;
        if enrolled.contains_key(&id) {
            return Err("this credential is already enrolled".to_string());
        }
        // No label falls back to the invite's grant, so that is the label checked; reading the
        // invite here spends nothing.
        let label = match v
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            Some(label) => label.to_string(),
            None => invite_grant(&self.layout, &invite)?,
        };
        // Under the clients lock, so two enrollments racing for one label cannot both pass.
        refuse_taken_label(&enrolled, &label)?;
        // The invite is redeemed LAST, after everything that could refuse the credential:
        // a malformed key or a taken label must not burn a single-use code.
        let grant = redeem(&self.layout, &invite, now)?;
        scopes_of(&self.layout, &grant)?;
        // Answered only now that an invite was spent on it ([`MAX_PENDING`]).
        self.answer_challenge(&challenge, expires, now)?;
        let entry = Enrolled {
            credential_id: id,
            grant,
            label,
            public_key: URL_SAFE_NO_PAD.encode(&key),
            sign_count: 0,
        };
        write_passkey(&self.layout, &lock, &entry)?;
        Ok(entry)
    }

    /// Finish a login. The body: `{challenge, id, authenticatorData, clientDataJSON,
    /// signature}`. Returns the session token (the only copy) and the identity.
    ///
    /// Writes one `gonk:Login` line either way: `outcome=ok` naming the passkey, or
    /// `outcome=denied` naming the credential the body claimed and no principal.
    pub fn login(&self, body: &[u8], now: u64) -> Result<(String, Identity), String> {
        let answer = self.sign_in(body, now);
        let claimed = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string));
        self.audit(
            LOGIN_CLASS,
            AuditLine {
                op: "login",
                door: "http",
                outcome: if answer.is_ok() { "ok" } else { "denied" },
                credential: claimed.as_deref(),
                authenticated: answer.is_ok(),
                sessions: usize::from(answer.is_ok()),
                why: None,
            },
        );
        answer
    }

    fn sign_in(&self, body: &[u8], now: u64) -> Result<(String, Identity), String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let challenge = text("challenge")?;
        let expires = self.check_challenge(&challenge, Purpose::Login, now)?;
        let id = text("id")?;
        let mut enrolled = read_passkeys(&self.layout)?
            .remove(&id)
            .ok_or("this passkey is not enrolled on this server")?;
        let credential = RegisteredCredential::from_spki_der(
            b64(&id, "id")?,
            &b64(&enrolled.public_key, "public_key")?,
            enrolled.sign_count,
        )
        .map_err(|e| e.to_string())?;
        let (auth, client, signature) = (
            b64(&text("authenticatorData")?, "authenticatorData")?,
            b64(&text("clientDataJSON")?, "clientDataJSON")?,
            b64(&text("signature")?, "signature")?,
        );
        let count = ikigai_passkey::verify(
            &credential,
            &Assertion {
                credential_id: &credential.id,
                authenticator_data: &auth,
                client_data_json: &client,
                signature: &signature,
            },
            &Policy {
                rp_id: RP_ID,
                origin: &self.origin,
                challenge_b64url: &challenge,
                require_user_verified: true,
            },
        )
        // One refusal on the wire; the precise reason is in the error the caller logs.
        .map_err(|e| format!("the passkey assertion was refused ({e})"))?;
        // Answered only now that a passkey signed it ([`MAX_PENDING`]).
        self.answer_challenge(&challenge, expires, now)?;
        // ★ Decided again under the lock, against the file as it is NOW: a `passkey revoke`
        // (or a re-enrolment under another key) may have landed while this was verifying, and
        // writing the entry read before it back would undo it (ledger #2776).
        let lock = self.layout.lock_clients()?;
        let unchanged = read_passkeys(&self.layout)?.remove(&id).is_some_and(|now| {
            now.public_key == enrolled.public_key && now.grant == enrolled.grant
        });
        if !unchanged {
            return Err("this passkey is not enrolled on this server".to_string());
        }
        if count != enrolled.sign_count {
            enrolled.sign_count = count;
            write_passkey(&self.layout, &lock, &enrolled)?;
        }
        drop(lock);
        let scopes = scopes_of(&self.layout, &enrolled.grant)?;
        let token = random_b64(32)?;
        let expires = now + self.lifetime(&scopes);
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        // ★ Before the new session exists: a revocation already in the log ends the OLD
        // sessions of this credential, never the one this sign-in opens.
        self.read_log(&mut state);
        let idle = self.idle_seconds;
        let before = state.sessions.len();
        state.sessions.retain(|_, s| s.over(now, idle).is_none());
        state.dirty |= state.sessions.len() != before;
        if state.sessions.len() >= MAX_PENDING {
            self.save(&mut state);
            return Err(format!("{MAX_PENDING} sessions are already open"));
        }
        state.sessions.insert(
            sha256_hex(token.as_bytes()),
            Session {
                credential_id: id,
                grant: enrolled.grant.clone(),
                issued: now,
                expires,
                last_seen: now,
                stored_seen: now,
            },
        );
        state.dirty = true;
        self.save(&mut state);
        Ok((
            token,
            Identity {
                enrolled,
                scopes,
                expires,
            },
        ))
    }

    /// End a session. Unknown tokens are not an error: signing out twice is still signed out.
    /// Writes one `gonk:Logout` line, `sessions=0` for a token that named none.
    pub fn logout(&self, token: &str) {
        let ended = self.state.lock().ok().and_then(|mut state| {
            let ended = state.sessions.remove(&sha256_hex(token.as_bytes()));
            state.dirty |= ended.is_some();
            self.save(&mut state);
            ended
        });
        self.audit(
            LOGOUT_CLASS,
            AuditLine {
                op: "logout",
                door: "http",
                outcome: "ok",
                credential: ended.as_ref().map(|s| s.credential_id.as_str()),
                authenticated: true,
                sessions: usize::from(ended.is_some()),
                why: Some("logout"),
            },
        );
    }

    /// End EVERY session of the passkey `token` signs in as, this one included — "sign out
    /// everywhere" (ledger #1824). Returns how many ended; `None` when `token` is no live
    /// session, which ends nothing (a stranger's token names nobody to sign out).
    pub fn logout_all(&self, token: &str, now: u64) -> Option<usize> {
        let who = self.identity(token, now)?;
        let credential = who.enrolled.credential_id;
        let ended = {
            let mut state = self.state.lock().ok()?;
            let ended = state.end_sessions(&credential);
            self.save(&mut state);
            ended
        };
        self.audit(
            LOGOUT_CLASS,
            AuditLine {
                op: "logout-all",
                door: "http",
                outcome: "ok",
                credential: Some(&credential),
                authenticated: true,
                sessions: ended,
                why: Some("logout-all"),
            },
        );
        Some(ended)
    }

    /// The identity behind a session token, re-resolved NOW: a session whose passkey was
    /// removed from `clients.json`, or whose grant no longer resolves, is no identity.
    ///
    /// ★ A session past either bound, or whose credential is not enrolled, is DELETED here,
    /// not just refused, and so is every other session of a credential that is not enrolled:
    /// nothing done to the files afterwards can bring one back. (A grant that no longer
    /// resolves only refuses: a grant is authority, re-read on every request, and a session
    /// holds authentication only.) A live session's idle clock restarts.
    pub fn identity(&self, token: &str, now: u64) -> Option<Identity> {
        let key = sha256_hex(token.as_bytes());
        let (credential_id, issued, expires) = {
            let mut state = self.state.lock().ok()?;
            self.read_log(&mut state);
            let Some(session) = state.sessions.get(&key) else {
                self.save(&mut state);
                return None;
            };
            if let Some(why) = session.over(now, self.idle_seconds) {
                let ended = state.sessions.remove(&key);
                state.dirty = true;
                self.save(&mut state);
                drop(state);
                self.audit(
                    LOGOUT_CLASS,
                    AuditLine {
                        op: "session",
                        door: "http",
                        outcome: "ok",
                        credential: ended.as_ref().map(|s| s.credential_id.as_str()),
                        authenticated: true,
                        sessions: 1,
                        why: Some(why),
                    },
                );
                return None;
            }
            let found = (
                session.credential_id.clone(),
                session.issued,
                session.expires,
            );
            self.save(&mut state);
            found
        };
        let Some(enrolled) = self
            .layout
            .current_passkeys()
            .ok()?
            .get(&credential_id)
            .cloned()
        else {
            let ended = {
                let mut state = self.state.lock().ok()?;
                let ended = state.end_sessions(&credential_id);
                self.save(&mut state);
                ended
            };
            self.audit(
                LOGOUT_CLASS,
                AuditLine {
                    op: "session",
                    door: "http",
                    outcome: "ok",
                    credential: Some(&credential_id),
                    authenticated: true,
                    sessions: ended,
                    why: Some("unenrolled"),
                },
            );
            return None;
        };
        let scopes = scopes_of(&self.layout, &enrolled.grant).ok()?;
        // ★ The grant is re-read NOW: a session opened read-only whose grant has since been
        // widened to write ends where a write session would have, 12 h after its sign-in.
        let expires = expires.min(issued.saturating_add(self.lifetime(&scopes)));
        {
            let mut state = self.state.lock().ok()?;
            if expires <= now {
                let ended = state.sessions.remove(&key);
                state.dirty |= ended.is_some();
                self.save(&mut state);
                drop(state);
                if ended.is_some() {
                    self.audit(
                        LOGOUT_CLASS,
                        AuditLine {
                            op: "session",
                            door: "http",
                            outcome: "ok",
                            credential: Some(&credential_id),
                            authenticated: true,
                            sessions: 1,
                            why: Some("expired"),
                        },
                    );
                }
                return None;
            }
            // Gone if it ended while this request was reading the files: no identity then.
            let session = state.sessions.get_mut(&key)?;
            session.last_seen = session.last_seen.max(now);
            if session.last_seen >= session.stored_seen.saturating_add(SEEN_WRITE_SECONDS) {
                state.dirty = true;
                self.save(&mut state);
            }
        }
        Some(Identity {
            enrolled,
            scopes,
            expires,
        })
    }

    fn check_client_data(&self, bytes: &[u8], kind: &str, challenge: &str) -> Result<(), String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|_| "clientDataJSON is not JSON".to_string())?;
        let field = |name: &str| v.get(name).and_then(Value::as_str).unwrap_or_default();
        if field("type") != kind {
            return Err(format!("clientDataJSON is not a `{kind}` ceremony"));
        }
        if field("origin") != self.origin {
            return Err(format!(
                "the ceremony ran at `{}`, and this server accepts only `{}` — open the page \
                 through `localhost`, not an IP address",
                field("origin"),
                self.origin
            ));
        }
        if field("challenge") != challenge {
            return Err("clientDataJSON answers a different challenge".to_string());
        }
        Ok(())
    }
}

/// Refuse a scope list holding any token no identity may hold — [`quic::grant_refusal`],
/// the one decision, under its old name. Kept for callers of the library; nothing in this
/// crate needs a second spelling of the rule.
pub fn refuse_broad(grant: &str, scopes: &[String]) -> Result<(), String> {
    match quic::grant_refusal(grant, scopes) {
        None => Ok(()),
        Some(refusal) => Err(refusal),
    }
}

fn b64(text: &str, field: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| format!("`{field}` is not base64url"))
}

/// The HMAC a challenge is sealed with, over `body`, keyed by this server's `key`.
fn seal(key: &[u8; 32], body: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(body);
    mac
}

fn random_b64(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("the OS random source failed: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Seconds since the epoch, from the wall clock. The capability function has no kernel to
/// take an injected clock from; everything testable here takes `now` as an argument instead.
pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{grants_for, Authority};

    #[test]
    fn an_invite_is_single_use_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let scopes = grants_for("default", Authority::Delete).unwrap();
        let code = invite(&layout, "brian", &scopes, false, 30, 1_000).unwrap();
        assert!(
            !std::fs::read_to_string(layout.invites_json())
                .unwrap()
                .contains(&code),
            "the file keeps a hash, never the code"
        );
        assert_eq!(redeem(&layout, &code, 1_001).unwrap(), "brian");
        assert!(redeem(&layout, &code, 1_002).is_err(), "single use");
        let late = invite(&layout, "brian", &scopes, false, 1, 1_000).unwrap();
        assert!(redeem(&layout, &late, 1_000 + 61)
            .unwrap_err()
            .contains("expired"));
        // The same grant name with different scopes is refused without --force.
        let read = grants_for("default", Authority::Read).unwrap();
        assert!(invite(&layout, "brian", &read, false, 30, 1_000).is_err());
    }

    #[test]
    fn a_challenge_is_single_use_and_bound_to_its_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp.take_challenge(&c, Purpose::Register, 11).is_err());
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp.take_challenge(&c, Purpose::Login, 11).is_ok());
        assert!(rp.take_challenge(&c, Purpose::Login, 12).is_err());
        let c = rp.challenge(Purpose::Login, 10).unwrap();
        assert!(rp
            .take_challenge(&c, Purpose::Login, 10 + CHALLENGE_SECONDS)
            .is_err());
    }

    /// Enroll a passkey `id` under a fresh `brian` grant and open a session for it at `now`,
    /// straight into the table (no assertion: these tests are about what a session does after).
    fn session_for(rp: &Passkeys, id: &str, now: u64) -> String {
        session_as(rp, id, "brian", Authority::Write, now)
    }

    /// [`session_for`] under the grant `grant` holding `authority` over `default`, written to
    /// `gonk/sessions.json` as a sign-in writes it.
    fn session_as(rp: &Passkeys, id: &str, grant: &str, authority: Authority, now: u64) -> String {
        if quic::read_grants(&rp.layout.grants_json())
            .map(|g| !g.contains_key(grant))
            .unwrap_or(true)
        {
            let scopes = grants_for("default", authority).unwrap();
            quic::put_grant(&rp.layout, grant, &scopes, false).unwrap();
        }
        write_passkey(
            &rp.layout,
            &rp.layout.lock_clients().unwrap(),
            &Enrolled {
                credential_id: id.to_string(),
                grant: grant.to_string(),
                label: id.to_string(),
                public_key: "AAAA".to_string(),
                sign_count: 0,
            },
        )
        .unwrap();
        let scopes = scopes_of(&rp.layout, grant).unwrap();
        let token = random_b64(32).unwrap();
        let mut state = rp.state.lock().unwrap();
        state.sessions.insert(
            sha256_hex(token.as_bytes()),
            Session {
                credential_id: id.to_string(),
                grant: grant.to_string(),
                issued: now,
                expires: now + rp.lifetime(&scopes),
                last_seen: now,
                stored_seen: now,
            },
        );
        state.dirty = true;
        rp.save(&mut state);
        token
    }

    /// A NEW relying party over the same state directory: what a restarted gonk builds.
    fn restart(rp: &Passkeys, now: u64) -> Passkeys {
        Passkeys::new_at(rp.layout.clone(), 1060, now)
            .with_idle_seconds(rp.idle_seconds)
            .with_max_seconds(rp.max_seconds)
    }

    /// ★ A session survives a restart (ledger #3229): a new door over the same directory
    /// accepts it, with its idle clock and its end carried over; one already expired is
    /// dropped at load, and one that idled out while the server was down is refused.
    #[test]
    fn a_session_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000);
        let token = session_for(&rp, "Y3JlZA", 1_000);
        let idle = session_for(&rp, "aWRsZQ", 1_000);
        assert!(rp.identity(&token, 2_000).is_some());
        drop(rp);

        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 3_000);
        let who = rp
            .identity(&token, 3_000)
            .expect("accepted after the restart");
        assert_eq!(who.enrolled.credential_id, "Y3JlZA");
        assert_eq!(
            who.expires,
            1_000 + SESSION_SECONDS,
            "its end is carried over"
        );
        assert!(
            rp.identity(&idle, 4_601).is_none(),
            "the idle bound counts from its last request before the restart"
        );
        drop(rp);

        // Past its end: dropped at load, and gone from the file.
        let rp = Passkeys::new_at(
            Layout::in_config_home(dir.path()),
            1060,
            1_000 + SESSION_SECONDS,
        );
        assert!(rp.state.lock().unwrap().sessions.is_empty());
        let file = std::fs::read_to_string(rp.layout.sessions_json()).unwrap();
        assert!(!file.contains(&sha256_hex(token.as_bytes())), "{file}");
    }

    /// ★ The file holds each token's hash, its principal, grant and times, and NEVER the token;
    /// it is readable by this user only.
    #[test]
    fn the_stored_file_holds_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000);
        let token = session_for(&rp, "Y3JlZA", 1_000);
        let path = rp.layout.sessions_json();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&token), "{text}");
        let doc: Value = serde_json::from_str(&text).unwrap();
        let entry = &doc["sessions"][sha256_hex(token.as_bytes())];
        assert_eq!(entry["principal"], passkey_iri("Y3JlZA"));
        assert_eq!(entry["grant"], "brian");
        assert_eq!(entry["issued"], 1_000);
        assert_eq!(entry["last_seen"], 1_000);
        assert_eq!(entry["expires"], 1_000 + SESSION_SECONDS);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
    }

    /// ★ Every way a session ends holds across a restart: a sign-out, sign out everywhere (from
    /// the page or from `passkey logout --all`, run while the server is up or down), and a
    /// revoked passkey, whose sessions do not come back even if it is enrolled again.
    #[test]
    fn an_ended_session_stays_ended_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let rp = Passkeys::new_at(layout.clone(), 1060, 1_000);
        let out = session_for(&rp, "b3V0", 1_000);
        let page_all = session_for(&rp, "YWxs", 1_000);
        let cli_up = session_for(&rp, "dXA", 1_000);
        let cli_down = session_for(&rp, "ZG93bg", 1_000);
        let revoked_up = session_for(&rp, "cmV2dXA", 1_000);
        let revoked_down = session_for(&rp, "cmV2ZG93bg", 1_000);
        let kept = session_for(&rp, "a2VwdA", 1_000);

        rp.logout(&out);
        assert_eq!(rp.logout_all(&page_all, 1_001), Some(1));
        end_sessions(&layout, Which::Credential("dXA"), 0).unwrap();
        revoke(&layout, Which::Credential("cmV2dXA"), 0).unwrap();
        // The running server reads the log at its next request.
        assert!(rp.identity(&kept, 1_002).is_some());
        drop(rp);

        // While no server runs.
        end_sessions(&layout, Which::Credential("ZG93bg"), 0).unwrap();
        revoke(&layout, Which::Credential("cmV2ZG93bg"), 0).unwrap();

        let rp = Passkeys::new_at(layout.clone(), 1060, 1_003);
        // The revoked passkeys enrolled again under the same id: their old sessions stay ended.
        for id in ["cmV2dXA", "cmV2ZG93bg"] {
            write_passkey(
                &layout,
                &layout.lock_clients().unwrap(),
                &Enrolled {
                    credential_id: id.to_string(),
                    grant: "brian".to_string(),
                    label: id.to_string(),
                    public_key: "AAAA".to_string(),
                    sign_count: 0,
                },
            )
            .unwrap();
        }
        for (what, token) in [
            ("signed out", &out),
            ("signed out everywhere from the page", &page_all),
            ("logout --all while up", &cli_up),
            ("logout --all while down", &cli_down),
            ("revoked while up", &revoked_up),
            ("revoked while down", &revoked_down),
        ] {
            assert!(rp.identity(token, 1_004).is_none(), "{what}");
        }
        assert!(
            rp.identity(&kept, 1_004).is_some(),
            "the others' session is kept"
        );

        // A sign-in after a logout --all is not ended by reading that line again at a restart.
        let fresh = session_for(&rp, "dXA", 1_005);
        drop(rp);
        let rp = Passkeys::new_at(layout.clone(), 1060, 1_006);
        assert!(rp.identity(&fresh, 1_006).is_some());
    }

    /// ★ `gonk.http.session_max_hours = 240` (ledger #3229): a read-only grant's session runs
    /// past 12 h, to 240 h, across a restart; a write grant's session under the same setting
    /// ends at 12 h; and a read-only grant widened to write ends its session at 12 h too.
    #[test]
    fn only_a_read_only_session_outlives_twelve_hours() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        let reader = session_as(&rp, "cmVhZA", "reader", Authority::Read, 1_000);
        let writer = session_as(&rp, "d3JpdGU", "writer", Authority::Write, 1_000);
        let widened = session_as(&rp, "d2lkZQ", "widened", Authority::Read, 1_000);

        let twelve = 1_000 + SESSION_SECONDS;
        assert!(rp.identity(&writer, twelve - 1).is_some());
        assert!(
            rp.identity(&writer, twelve).is_none(),
            "a write grant ends at 12 h"
        );
        let who = rp
            .identity(&reader, twelve + 3_600)
            .expect("read-only past 12 h");
        assert_eq!(who.expires, 1_000 + 240 * 3_600);

        quic::put_grant(
            &rp.layout,
            "widened",
            &grants_for("default", Authority::Write).unwrap(),
            true,
        )
        .unwrap();
        assert!(
            rp.identity(&widened, twelve + 3_600).is_none(),
            "a grant that can write now ends at 12 h after the sign-in"
        );

        let rp = restart(&rp, twelve + 7_200);
        assert!(rp.identity(&reader, 1_000 + 239 * 3_600).is_some());
        assert!(rp.identity(&reader, 1_000 + 240 * 3_600).is_none());
    }

    /// ★ The idle bound ends a session no request has carried for that long, and a request
    /// restarts its clock; the absolute bound ends it however busy it is. Either way the
    /// session is DELETED, so a later request inside the window cannot find it again.
    #[test]
    fn an_idle_session_ends_and_a_busy_one_ends_at_the_absolute_bound() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060).with_idle_seconds(600);
        let token = session_for(&rp, "Y3JlZA", 1_000);
        assert!(
            rp.identity(&token, 1_599).is_some(),
            "inside the idle bound"
        );
        assert!(
            rp.identity(&token, 2_198).is_some(),
            "the last request restarted it"
        );
        assert!(
            rp.identity(&token, 2_799).is_none(),
            "600 s with no request"
        );
        assert!(
            rp.identity(&token, 2_200).is_none(),
            "deleted, not just refused"
        );

        let busy = session_for(&rp, "YnVzeQ", 1_000);
        let mut now = 1_000;
        while now < 1_000 + SESSION_SECONDS - 300 {
            now += 300;
            assert!(rp.identity(&busy, now).is_some(), "busy at {now}");
        }
        assert!(rp.identity(&busy, 1_000 + SESSION_SECONDS).is_none());
    }

    /// ★ A revocation is read from `passkeys.log`, so it reaches this server from another
    /// process; but a line written before this server started is history, and a line that
    /// ends sessions ends only the ones open when it is read, never one a later sign-in opens.
    #[test]
    fn the_log_ends_sessions_open_when_it_is_read_and_no_others() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
        let old = Enrolled {
            credential_id: "b2xk".to_string(),
            grant: "brian".to_string(),
            label: "old".to_string(),
            public_key: "AAAA".to_string(),
            sign_count: 0,
        };
        record(&layout, REVOKED, &old, 0).unwrap();
        let rp = Passkeys::new(layout.clone(), 1060);
        let token = session_for(&rp, "b2xk", 10);
        assert!(
            rp.identity(&token, 11).is_some(),
            "a line from before this server started ends nothing"
        );

        let other = session_for(&rp, "b3RoZXI", 10);
        end_sessions(&layout, Which::Credential("b2xk"), 0).unwrap();
        assert!(rp.identity(&token, 12).is_none());
        assert!(
            rp.identity(&other, 12).is_some(),
            "only the passkey the line names"
        );
        let fresh = session_for(&rp, "b2xk", 13);
        assert!(rp.identity(&fresh, 14).is_some(), "a line is read once");

        // A half-written line is read when it is whole, and a replaced file from its start.
        append_line(
            &audit_log(&layout),
            "2026-10-17T00:00:00.000Z gonk:PasskeySessionsEnded ",
        )
        .unwrap();
        assert!(rp.identity(&fresh, 15).is_some());
        append_line(&audit_log(&layout), &format!("{}\n", passkey_iri("b2xk"))).unwrap();
        assert!(rp.identity(&fresh, 16).is_none());
        let again = session_for(&rp, "b2xk", 17);
        std::fs::write(audit_log(&layout), "").unwrap();
        assert!(rp.identity(&again, 18).is_some());
        record(&layout, REVOKED, &old, 0).unwrap();
        assert!(rp.identity(&again, 19).is_none());
    }

    /// ★ Minting challenges holds nothing (ledger #2804): past [`MAX_PENDING`] a challenge
    /// is still minted, and the FIRST one minted is still answerable — nothing is refused and
    /// nothing is evicted.
    #[test]
    fn past_any_bound_a_challenge_is_minted_and_none_is_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let first = rp.challenge(Purpose::Login, 0).unwrap();
        for _ in 0..4 * MAX_PENDING {
            rp.challenge(Purpose::Login, 0).unwrap();
            rp.challenge(Purpose::Register, 0).unwrap();
        }
        let last = rp.challenge(Purpose::Login, 0).unwrap();
        assert!(rp.state.lock().unwrap().answered.is_empty());
        assert!(rp.take_challenge(&first, Purpose::Login, 1).is_ok());
        assert!(rp.take_challenge(&last, Purpose::Login, 1).is_ok());
    }

    /// A challenge is believed only under THIS server's seal: another server's, one with a
    /// byte changed, and one with its purpose or expiry rewritten are all refused.
    #[test]
    fn a_challenge_sealed_elsewhere_or_altered_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let other = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        rp.challenge(Purpose::Login, 0).unwrap();
        let foreign = other.challenge(Purpose::Login, 0).unwrap();
        assert!(rp.take_challenge(&foreign, Purpose::Login, 1).is_err());
        let ours = URL_SAFE_NO_PAD
            .decode(rp.challenge(Purpose::Register, 0).unwrap())
            .unwrap();
        for at in [0, NONCE_BYTES, NONCE_BYTES + 1, SEALED_BYTES - 1] {
            let mut altered = ours.clone();
            altered[at] ^= if at == NONCE_BYTES { b'r' ^ b'l' } else { 1 };
            let altered = URL_SAFE_NO_PAD.encode(&altered);
            assert!(rp.take_challenge(&altered, Purpose::Login, 1).is_err());
            assert!(rp.take_challenge(&altered, Purpose::Register, 1).is_err());
        }
        for junk in ["", "AAAA", "not base64!"] {
            assert!(rp.take_challenge(junk, Purpose::Login, 1).is_err());
        }
        let ours = URL_SAFE_NO_PAD.encode(&ours);
        assert!(rp.take_challenge(&ours, Purpose::Register, 1).is_ok());
    }

    /// The answered set forgets a challenge once it has expired: it grows only with
    /// ceremonies answered inside one [`CHALLENGE_SECONDS`].
    #[test]
    fn an_answered_challenge_is_forgotten_once_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let rp = Passkeys::new(Layout::in_config_home(dir.path()), 1060);
        let c = rp.challenge(Purpose::Login, 0).unwrap();
        assert!(rp.take_challenge(&c, Purpose::Login, 1).is_ok());
        assert_eq!(rp.state.lock().unwrap().answered.len(), 1);
        let d = rp.challenge(Purpose::Login, CHALLENGE_SECONDS).unwrap();
        assert!(rp
            .take_challenge(&d, Purpose::Login, CHALLENGE_SECONDS + 1)
            .is_ok());
        assert_eq!(rp.state.lock().unwrap().answered.len(), 1);
    }
}
