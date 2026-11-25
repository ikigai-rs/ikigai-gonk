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
//!                       "passkeys": { "<credential id>":   { "grants": ["brian"], "label": "brian",
//!                                                             "public_key": "<SPKI, base64url>",
//!                                                             "sign_count": 0 } } }
//! gonk/grants.json    { "brian": ["urn:cap:ledger:read:default", …] }
//! gonk/invites.json   { "<sha256 of an invite code>": { "grant": "brian", "expires": 1757… } }
//! ```
//!
//! ★ **People** (ledger [#4305](http://localhost:1062/l/default/item/4305)): `clients.json`
//! also holds `"people": { "<id>": { "label": "brian", "grants": ["brian"] } }`, and each
//! passkey entry names `"person": "urn:iki:gonk:person:id:<id>"` and `"rp_id": "localhost"`
//! in place of its own grants (its `grants`, and `grant` for a person with one, stay as a
//! copy of its person's that only an older gonk reads, so a downgrade still starts, ledger
//! #4368). People live in `clients.json`, not a file of their own, so a
//! passkey and the person it names are written together, under the one clients lock, in one
//! rename: no reader ever sees a passkey naming a person who is not there yet. An entry from
//! before people is read as a person of its own (`parse_passkeys`) and written so at the
//! next start, or before the next write ([`migrate_people`]). What follows says "a passkey's grants": they are its
//! person's.
//!
//! A passkey holds a LIST of grant names (ledger [#3388](http://localhost:1062/l/default/item/3388);
//! the single-name `"grant": "brian"` earlier servers wrote is still read) and its scopes are
//! their union. A grant is shared, so one person is widened by adding a grant to their passkey
//! ([`Passkeys::add_grant`], an invite redeemed while signed in), never by editing a grant.
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
//! carried it. It may write for 12 h of that at most (see the two bounds below).
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
//! # Two bounds: write lapses, reads run on (ledger [#3815](http://localhost:1062/l/default/item/3815))
//!
//! Every session has two absolute bounds, both counted from its sign-in and never moved by
//! use ([`Passkeys::write_seconds`], [`Passkeys::max_seconds`]):
//!
//! - **The write bound**, `issued + min(SESSION_SECONDS, session_max_hours)`: 12 h at most.
//!   Past it the session's scopes are filtered one by one to those [`read_only`] passes, so it
//!   can still read but no longer write, claim or comment. A fresh sign-in by the same passkey
//!   restores the rest ([`Identity::restore`]).
//! - **The session's end**, `issued + session_max_hours` (up to
//!   [`READ_ONLY_SESSION_SECONDS`], 30 days), or the idle bound, whichever comes first. A
//!   grant with no read scope has nothing left past the write bound, so its session ends there.
//!
//! ⚠ The write bound is absolute, never sliding: an open page polls, so a bound that moved
//! with use would keep write alive in any open tab, or for a stolen cookie that polls. The
//! grant is re-read on every request, so a read-only grant widened to write writes until the
//! write bound of the session it is read in, and only reads after it.
//!
//! `sessions.json` holds no field for the write bound: each sign-in opens a new session, so
//! `issued` is the last sign-in. A gonk from before #3815 still ends a session at
//! `issued + lifetime(scopes)` as it reckons it (12 h for a grant that can write), so a
//! rollback can only end sessions sooner.
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
//! ★ **The server sets the session cookie, `HttpOnly`** ([#1591](http://localhost:1062/l/default/item/1591)):
//! `login` and `add-grant` answer through `ikigai_web::with_headers` with a `Set-Cookie`
//! ([`session_cookie`]), and the HTTP door accepts that header from an endpoint
//! (`EndpointHeader::SetCookie`, `crate::doors::edge_config_with`). No script on any
//! `http://localhost:*` page (cookies are scoped by host, not port) can read the token. The page
//! names no token: `session`, `logout`, `logout-all` and `add-grant` read the passkey from the
//! capability's principal, which the door derives from the cookie.

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

/// The cookie the session token rides in. The SERVER sets it (`HttpOnly`, so no script on
/// any `localhost` page can read it), with [`session_cookie`], and clears it with
/// [`CLEAR_SESSION_COOKIE`].
pub const SESSION_COOKIE: &str = "gonk_session";

/// The `Set-Cookie` value that opens a session: `HttpOnly`, `SameSite=Strict`, for the whole
/// origin, for `seconds`. ⚠ No `Secure`: the HTTP door serves plain `http://localhost`; add it
/// once the door is served over TLS.
pub fn session_cookie(token: &str, seconds: u64) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={seconds}")
}

/// The `Set-Cookie` value that ends the session in this browser.
pub const CLEAR_SESSION_COOKIE: &str =
    "gonk_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0";

/// Who a passkey request says is signed in: a session token a caller holds (the socket, a
/// test, an older page), or what the HTTP door derives from the HttpOnly cookie a page cannot
/// read — the request's own session, by its key, beside the principal.
#[derive(Clone, Copy, Debug)]
pub enum SignedIn<'a> {
    /// A session token.
    Token(&'a str),
    /// The capability's principal ([`passkey_iri`] or [`person_iri`]), if any, and no session
    /// key: the newest live session of any passkey it stands for. A caller that has the
    /// request's session key passes [`SignedIn::Session`] instead.
    Principal(Option<&'a str>),
    /// The HTTP door's request (ledger #4363): the key of the session its cookie names (the
    /// [`session_id`] of the token, which the door puts in the capability) and the principal
    /// the door stamped. It answers THAT session only, never another device's session of the
    /// same person; a key whose session's passkey the principal does not stand for answers
    /// nobody.
    Session {
        /// The session's key in the session table: the SHA-256 of its token, hex.
        key: &'a str,
        /// The capability's principal, if any.
        principal: Option<&'a str>,
    },
}

impl SignedIn<'_> {
    /// The principal this names, if it names one: the door's ([`SignedIn::Principal`],
    /// [`SignedIn::Session`]), never a token's.
    pub fn principal(&self) -> Option<&str> {
        match self {
            SignedIn::Token(_) => None,
            SignedIn::Principal(principal) | SignedIn::Session { principal, .. } => *principal,
        }
    }
}

/// How long a session may WRITE (anything beyond read) at most, however busy it is and
/// whatever `gonk.http.session_max_hours` says; also that setting's default. Past it the
/// session keeps only its read scopes ([`Passkeys::write_seconds`]).
pub const SESSION_SECONDS: u64 = 12 * 60 * 60;

/// The longest `gonk.http.session_max_hours` may make a session, which past
/// [`SESSION_SECONDS`] only READS: 30 days. Never unbounded: a stolen cookie still reads
/// everything its grant covers.
pub const READ_ONLY_SESSION_SECONDS: u64 = 720 * 60 * 60;

/// Whether every scope in `scopes` only reads (`urn:cap:<family>:read:…`). Anything else —
/// write, claim, comment, an alias scheme, a token this rule does not know — can write, and
/// so lapses at [`SESSION_SECONDS`] after the sign-in. An empty list is not read-only: it grants nothing, and
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

/// `scopes` split into those that only read ([`read_only`], one by one) and the rest: what a
/// session keeps past its write bound, and what lapses there.
///
/// ```
/// use ikigai_gonk::identity::split_reads;
/// let scopes = ["urn:cap:ledger:read:acme".to_string(),
///               "urn:cap:ledger:write:acme".to_string()];
/// let (reads, rest) = split_reads(&scopes);
/// assert_eq!(reads, ["urn:cap:ledger:read:acme"]);
/// assert_eq!(rest, ["urn:cap:ledger:write:acme"]);
/// ```
pub fn split_reads(scopes: &[String]) -> (Vec<String>, Vec<String>) {
    scopes
        .iter()
        .cloned()
        .partition(|scope| read_only(std::slice::from_ref(scope)))
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

/// Why a passkey with the credential id `id` cannot be enrolled because no door could name
/// it, or `None` when one can.
///
/// The HTTP door mints [`passkey_iri`] into each signed-in request's capability, and the log
/// writes it as the `principal=` of every line (ledger #383). Core refuses a principal past
/// [`ikigai_core::MAX_PRINCIPAL_LEN`] bytes, and WebAuthn lets an authenticator choose an id of
/// up to 1023 bytes, so through ledger #3414 a long one enrolled and every request it signed
/// was attributed to `anon`. A credential id of up to 368 bytes fits.
pub fn principal_refusal(id: &str) -> Option<String> {
    let iri = passkey_iri(id);
    ikigai_core::Capability::scoped(Vec::<String>::new())
        .with_principal(&iri)
        .err()
        .map(|e| {
            format!(
                "this passkey's credential id ({} characters) is too long to name its holder \
                 ({e}); use another authenticator",
                id.len()
            )
        })
}

/// The prefix of a person's IRI (ledger [#4305](http://localhost:1062/l/default/item/4305)):
/// `urn:iki:gonk:person:id:<id>`. A person is what several passkeys name — a laptop's, a
/// phone's, one registered at another origin — and holds the label and the grant list they
/// share. The `<id>` is short, generated and base64url, so it needs no escaping and leaves the
/// IRI far under core's principal length limit.
pub const PERSON_IRI_PREFIX: &str = "urn:iki:gonk:person:id:";

/// The IRI of the person `id` names.
pub fn person_iri(id: &str) -> String {
    format!("{PERSON_IRI_PREFIX}{id}")
}

/// The id a [`person_iri`] names, or `None` for any other text.
pub fn person_id(iri: &str) -> Option<&str> {
    iri.strip_prefix(PERSON_IRI_PREFIX)
        .filter(|id| !id.is_empty())
}

/// The person id an enrollment from before people existed is migrated to: the first 9 bytes
/// of the SHA-256 of its credential id, base64url (12 characters). Derived, not random, so a
/// file read before its migration is written names the same person the migration writes, and
/// a second migration has nothing to decide.
fn migrated_person_id(credential_id: &str) -> String {
    let digest = Sha256::digest(credential_id.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..9])
}

/// A fresh person id: 9 random bytes, base64url (12 characters).
fn new_person_id() -> Result<String, String> {
    random_b64(9)
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
    /// The person this passkey names ([`person_iri`]). Several passkeys may name one person.
    pub person: String,
    /// The WebAuthn relying-party id this passkey was registered at ([`RP_ID`] for every
    /// passkey so far). A passkey signs in only at that relying party.
    pub rp_id: String,
    /// ★ Its PERSON's grant names (ledger #4305): grants live on the person, so every passkey
    /// of one person holds the same list. The grant names — keys of `grants.json` — this passkey holds, in the order they were
    /// added, never empty. Its scopes are the union of theirs ([`scopes_of_all`]). A grant is
    /// shared by every passkey that holds it, so a passkey is WIDENED by adding a grant to its
    /// own list ([`Passkeys::add_grant`]), never by editing a shared grant (ledger
    /// [#3388](http://localhost:1062/l/default/item/3388)).
    pub grants: Vec<String>,
    /// A human label.
    pub label: String,
    /// base64url of the SPKI DER public key.
    pub public_key: String,
    /// The highest signature counter seen.
    pub sign_count: u32,
}

impl Enrolled {
    /// The grant names as one line for a person: `brian`, or `brian, reviewer`.
    pub fn grant_names(&self) -> String {
        self.grants.join(", ")
    }
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
    let people = parse_people(doc)?;
    for (id, entry) in map {
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("passkey `{id}` has no `{name}`"))
        };
        let (person, grants, label) = match entry.get("person") {
            None | Some(Value::Null) => {
                // An enrollment from before people (ledger #4305): its own person, carrying
                // its grants, exactly as [`migrate_people`] writes it.
                let grants = entry_grants(id, entry)?;
                let label = field("label").unwrap_or_else(|_| grants[0].clone());
                let pid = migrated_person_id(id);
                // ★ Unless that person already exists (ledger #4368): an old-form entry put
                // back by hand, or rewritten by an older gonk, holds its PERSON's grants, as
                // the migration will make it, never a list of its own beside the person's.
                let grants = people.get(&pid).map_or(grants, |p| p.grants.clone());
                (person_iri(&pid), grants, label)
            }
            Some(Value::String(iri)) => {
                let who = person_id(iri)
                    .and_then(|pid| people.get(pid))
                    .ok_or_else(|| {
                        format!("passkey `{id}` names person `{iri}`, who is not in `people`")
                    })?;
                let label = field("label").unwrap_or_else(|_| who.label.clone());
                (iri.clone(), who.grants.clone(), label)
            }
            Some(_) => return Err(format!("passkey `{id}`'s `person` must be a person IRI")),
        };
        out.insert(
            id.clone(),
            Enrolled {
                credential_id: id.clone(),
                person,
                rp_id: field("rp_id").unwrap_or_else(|_| RP_ID.to_string()),
                label,
                grants,
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

/// One person, as the `people` section of `clients.json` holds it (ledger #4305).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// The person's label: what an author naming the person, or any of its passkeys, reads as.
    pub label: String,
    /// The grant names every passkey of this person holds, never empty.
    pub grants: Vec<String>,
}

/// The `people` section of `clients.json`, by person id. Absent: none.
pub(crate) fn parse_people(doc: &Map<String, Value>) -> Result<BTreeMap<String, Person>, String> {
    let mut out = BTreeMap::new();
    let Some(map) = doc.get("people") else {
        return Ok(out);
    };
    let map = map
        .as_object()
        .ok_or("`people` in clients.json must be an object of person id → entry")?;
    for (id, entry) in map {
        let grants = entry_grants(id, entry).map_err(|e| e.replacen("passkey", "person", 1))?;
        let label = entry
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| grants[0].clone());
        out.insert(id.clone(), Person { label, grants });
    }
    Ok(out)
}

/// The file [`migrate_people`] keeps the pre-migration `clients.json` in.
pub fn people_backup(layout: &Layout) -> std::path::PathBuf {
    layout
        .clients_json()
        .with_file_name("clients.json.before-people")
}

/// What one [`migrate`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Migration {
    /// How many passkey entries from before people now name a person.
    pub migrated: usize,
    /// The file holding `clients.json` as it was before them: [`people_backup`], or a
    /// stamped `clients.json.before-people-<UTC>` beside it when that one already held an
    /// older file (a re-upgrade after a rollback, ledger #4368). `None` when none migrated.
    pub backup: Option<std::path::PathBuf>,
    /// How many migrated entries' compatibility `grants` were rewritten to their person's.
    pub mirrored: usize,
}

/// Give every enrollment from before people its person (ledger #4305), under the clients
/// lock (`migrate_locked`). Returns how many passkeys were migrated.
pub fn migrate_people(layout: &Layout) -> Result<usize, String> {
    migrate(layout).map(|m| m.migrated)
}

/// [`migrate_people`], saying what it did and which backup holds the file before.
pub fn migrate(layout: &Layout) -> Result<Migration, String> {
    match std::fs::metadata(layout.clients_json()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Migration::default()),
        _ => {}
    }
    let lock = layout.lock_clients()?;
    migrate_locked(layout, &lock)
}

/// The sentence a start (or a cli write) prints after a migration that did something.
pub fn migration_line(layout: &Layout, done: &Migration) -> Option<String> {
    let backup = done.backup.as_ref()?;
    Some(format!(
        "gonk: gave {} passkey(s) a person each in {} (the file before is {})",
        done.migrated,
        layout.clients_json().display(),
        backup.display()
    ))
}

/// Set a passkey entry's compatibility grants to its person's (ledger #4368): `grants`, and
/// `grant` too when the person holds exactly one. This server ignores both on an entry that
/// names a person; every gonk from before people reads them (ledger-v0.18.0 through v0.24.x
/// read `grant`, v0.25.0 `grants`, refusing the two when they disagree), so a downgrade
/// starts and signs each passkey in with its person's grants. Returns whether it changed.
fn mirror_grants(object: &mut Map<String, Value>, grants: &[String]) -> bool {
    let before = (object.get("grants").cloned(), object.get("grant").cloned());
    object.insert("grants".to_string(), json!(grants));
    match grants {
        [one] => object.insert("grant".to_string(), Value::String(one.clone())),
        _ => object.remove("grant"),
    };
    before != (object.get("grants").cloned(), object.get("grant").cloned())
}

/// Give every enrollment from before people its own person (ledger #4305): a new `people`
/// entry carrying that passkey's label and grants, and `person` and `rp_id = "localhost"` on
/// the passkey. Nothing is merged: two passkeys become two people, and `passkey attach` is how
/// an operator makes them one. The new file is written whole and renamed into place. A file
/// with nothing to change is not written, so a second run changes nothing.
///
/// ★ **The backup is always a file from before people** (ledger #4368). Every writer of
/// `clients.json` runs this first, under the lock it holds, so no new-form entry is written
/// before the first backup. The first is [`people_backup`], never overwritten; when it is
/// there and differs from the file being migrated now (a rollback restored it, an older gonk
/// changed it, and this is the re-upgrade), the file is kept as a stamped one beside it and
/// that is the one [`Migration::backup`] names.
///
/// ★ **An old-form entry whose person already exists** (put back by hand from a backup, or
/// written by `passkey attach` or an older gonk) is pointed at that person: its own grants are
/// dropped, never copied over the person's, and a difference is reported.
///
/// ★ **Every migrated entry keeps its person's grants as `grants`** ([`mirror_grants`]), so
/// an older gonk still starts on the file. A mirror that differs from its person's list is
/// rewritten here, with a warning.
pub(crate) fn migrate_locked(layout: &Layout, _lock: &quic::FileLock) -> Result<Migration, String> {
    let path = layout.clients_json();
    let before = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Migration::default()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut doc = quic::read_object(&path)?;
    let enrolled = parse_passkeys(&doc)?;
    let Some(Value::Object(mut passkeys)) = doc.remove("passkeys") else {
        return Ok(Migration::default());
    };
    let mut people = match doc.remove("people") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`people` in clients.json must be an object".to_string()),
    };
    let mut done = Migration::default();
    let had_person: std::collections::BTreeSet<String> = passkeys
        .iter()
        .filter(|(_, entry)| entry.get("person").is_some_and(|p| !p.is_null()))
        .map(|(id, _)| id.clone())
        .collect();
    for (id, entry) in passkeys.iter_mut() {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        if object.get("person").is_some_and(|p| !p.is_null()) {
            continue;
        }
        let e = &enrolled[id];
        let pid = person_id(&e.person).unwrap_or_default().to_string();
        if people.contains_key(&pid) {
            // `e.grants` are already the person's (`parse_passkeys`); the entry's own are
            // reported when they differ, then dropped.
            let own = entry_grants(id, &Value::Object(object.clone())).unwrap_or_default();
            if own != e.grants {
                eprintln!(
                    "ikigai-gonk: passkey `{id}` in {} was an entry from before people (put back \
                     by hand, or written by an older gonk) whose person `{}` already exists: it \
                     now names that person and holds its grants ({}); its own ({}) were dropped",
                    path.display(),
                    e.person,
                    e.grant_names(),
                    own.join(", ")
                );
            }
        } else {
            people.insert(pid, json!({ "label": e.label, "grants": e.grants }));
        }
        object.insert("person".to_string(), Value::String(e.person.clone()));
        object.insert("rp_id".to_string(), Value::String(e.rp_id.clone()));
        done.migrated += 1;
    }
    // The mirror on every entry naming a person, the just-migrated ones included.
    let lists = parse_people(&Map::from_iter([(
        "people".to_string(),
        Value::Object(people.clone()),
    )]))?;
    for (id, entry) in passkeys.iter_mut() {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        let Some(person) = object
            .get("person")
            .and_then(Value::as_str)
            .and_then(person_id)
            .and_then(|pid| lists.get(pid))
        else {
            continue;
        };
        let had = object.get("grants").cloned();
        if mirror_grants(object, &person.grants) {
            if had.is_some() && had_person.contains(id) {
                eprintln!(
                    "ikigai-gonk: passkey `{id}`'s `grants` in {} differed from its person's; \
                     rewrote them to {} (only an older gonk reads them)",
                    path.display(),
                    person.grants.join(", ")
                );
            }
            done.mirrored += 1;
        }
    }
    if done.migrated == 0 && done.mirrored == 0 {
        return Ok(done);
    }
    doc.insert("people".to_string(), Value::Object(people));
    doc.insert("passkeys".to_string(), Value::Object(passkeys));
    let text = quic::pretty(&Value::Object(doc))?;
    quic::parse_enrolment(&text)?;
    if done.migrated > 0 {
        let first = people_backup(layout);
        let backup = match std::fs::read_to_string(&first) {
            Ok(kept) if kept == before => first,
            Ok(_) => {
                let stamp = crate::backup::stamp_compact(now_seconds() * 1000);
                let mut fresh = first.with_file_name(format!("clients.json.before-people-{stamp}"));
                let mut n = 1;
                while fresh.exists() {
                    n += 1;
                    fresh = first.with_file_name(format!("clients.json.before-people-{stamp}-{n}"));
                }
                quic::write_private(&fresh, &before)?;
                fresh
            }
            Err(_) => {
                quic::write_private(&first, &before)?;
                first
            }
        };
        done.backup = Some(backup);
    }
    quic::write_private(&path, &text)?;
    Ok(done)
}

/// A passkey entry's grant names: the list form `"grants": ["a", "b"]` this server writes, or
/// the single-name form `"grant": "a"` every earlier one wrote (still read; written only as the
/// mirror an older gonk reads, [`mirror_grants`]).
/// Refused when it names none, when a name is not a string, or when both forms disagree.
fn entry_grants(id: &str, entry: &Value) -> Result<Vec<String>, String> {
    let single = match entry.get("grant") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return Err(format!("passkey `{id}`'s `grant` must be a name")),
    };
    let listed = match entry.get("grants") {
        None | Some(Value::Null) => None,
        Some(Value::Array(names)) => Some(
            names
                .iter()
                .map(|name| name.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
                .ok_or_else(|| format!("passkey `{id}`'s `grants` must be a list of names"))?,
        ),
        Some(_) => return Err(format!("passkey `{id}`'s `grants` must be a list of names")),
    };
    let grants = match (listed, single) {
        (Some(listed), Some(single)) if listed != [single.clone()] => {
            return Err(format!(
                "passkey `{id}` has both `grant` and `grants`, and they disagree"
            ))
        }
        (Some(listed), _) => listed,
        (None, Some(single)) => vec![single],
        (None, None) => return Err(format!("passkey `{id}` has no `grants`")),
    };
    if grants.is_empty() || grants.iter().any(String::is_empty) {
        return Err(format!("passkey `{id}` names no grant"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if let Some(twice) = grants.iter().find(|name| !seen.insert(name.as_str())) {
        return Err(format!("passkey `{id}` names grant `{twice}` twice"));
    }
    Ok(grants)
}

/// Write one passkey entry into `clients.json`, keeping every other key (the QUIC clients,
/// the default) exactly as it was. The caller holds `_lock` from the read it decided on to
/// here, so no other change to the file lands in between and is undone (ledger #2776).
fn write_passkey(layout: &Layout, lock: &quic::FileLock, entry: &Enrolled) -> Result<(), String> {
    // ★ The migration first, under this lock (ledger #4368), so the backup it keeps is the
    // file from before people, never one this write already changed.
    migrated_by_cli(layout, lock)?;
    let mut doc = quic::read_object(&layout.clients_json())?;
    let mut passkeys = match doc.remove("passkeys") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`passkeys` in clients.json must be an object".to_string()),
    };
    let mut people = match doc.remove("people") {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => return Err("`people` in clients.json must be an object".to_string()),
    };
    let pid = person_id(&entry.person)
        .ok_or_else(|| format!("passkey `{}` names no person", entry.label))?;
    // ★ Grants are the person's (ledger #4305): writing this passkey's list writes its
    // person's, which every other passkey of that person reads. The person's label stays.
    let label = people
        .get(pid)
        .and_then(|p| p.get("label"))
        .and_then(Value::as_str)
        .unwrap_or(&entry.label)
        .to_string();
    people.insert(
        pid.to_string(),
        json!({ "label": label, "grants": entry.grants }),
    );
    passkeys.insert(
        entry.credential_id.clone(),
        json!({
            "person": entry.person,
            "rp_id": entry.rp_id,
            "label": entry.label,
            "public_key": entry.public_key,
            "sign_count": entry.sign_count,
        }),
    );
    // Every passkey of the person keeps its grants as an older gonk reads them.
    for other in passkeys.values_mut() {
        if let Some(object) = other.as_object_mut() {
            if object.get("person").and_then(Value::as_str) == Some(entry.person.as_str()) {
                mirror_grants(object, &entry.grants);
            }
        }
    }
    doc.insert("people".to_string(), Value::Object(people));
    doc.insert("passkeys".to_string(), Value::Object(passkeys));
    let text = quic::pretty(&Value::Object(doc))?;
    // What is written must still be what the QUIC door reads.
    quic::parse_enrolment(&text)?;
    quic::write_private(&layout.clients_json(), &text)
}

/// [`migrate_locked`] from a writer, printing what it did as a start does.
fn migrated_by_cli(layout: &Layout, lock: &quic::FileLock) -> Result<(), String> {
    let done = migrate_locked(layout, lock)?;
    if let Some(line) = migration_line(layout, &done) {
        eprintln!("{line}");
    }
    Ok(())
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
    migrated_by_cli(layout, &lock)?;
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
    record_with(layout, class, target, now_ms, "")
}

/// [`record`], with `extra` (` name=value` fields, each already [`encoded`]) at the end.
/// `grant=` lists every grant the passkey holds, each encoded, joined by `,` (which
/// [`encoded`] never leaves bare, so the list splits back unambiguously). A [`GRANT_ADDED`] or
/// [`GRANT_REMOVED`] line ends with `person=<person IRI>` (ledger #4363): the grant list is
/// the person's, so the change is every passkey of that person's.
fn record_with(
    layout: &Layout,
    class: &str,
    target: &Enrolled,
    now_ms: u64,
    extra: &str,
) -> std::io::Result<()> {
    let grants: Vec<String> = target.grants.iter().map(|g| encoded(g)).collect();
    let person = if matches!(class, GRANT_ADDED | GRANT_REMOVED) {
        format!(" person={}", encoded(&target.person))
    } else {
        String::new()
    };
    let line = format!(
        "{} {class} {} label={} grant={}{extra}{person}\n",
        crate::backup::stamp_iso_ms(now_ms),
        passkey_iri(&target.credential_id),
        encoded(&target.label),
        grants.join(","),
    );
    append_line(&audit_log(layout), &line)
}

/// The class of the [`audit_log`] line [`Passkeys::add_grant`] and [`add_grant`] write: a
/// PERSON was widened by a grant, and with it every passkey of that person. It names the
/// passkey the change was made through (the one that answered, or the one the owner named),
/// then its fields: `label`, `grant` (every grant the person holds now), `added` (the one
/// added), `invite` (`sha256:<hex>` of the code, as `invites.json` keys it — never the code;
/// absent for the owner's), `by` (the owner's) and `person` (the person's IRI, ledger #4363).
pub const GRANT_ADDED: &str = "gonk:PasskeyGrantAdded";

/// The class of the [`audit_log`] line [`remove_grant`] writes: `removed` names the grant,
/// taken off the PERSON `person` names, and so off every passkey of that person.
pub const GRANT_REMOVED: &str = "gonk:PasskeyGrantRemoved";

/// One grant change [`audit_log`] holds for a passkey: what `passkey list` prints under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantChange {
    /// The line's timestamp, as written.
    pub at: String,
    /// The passkey's credential id.
    pub credential_id: String,
    /// `true` for a [`GRANT_ADDED`] line, `false` for a [`GRANT_REMOVED`] one.
    pub added: bool,
    /// The grant added or removed, decoded.
    pub grant: String,
    /// The invite an addition redeemed (`sha256:<hex>`); `None` for a removal, or for an
    /// addition the owner made at the command line ([`add_grant`]).
    pub invite: Option<String>,
    /// Who made the change, when the line says (`by=`): [`BY_OWNER`] for [`add_grant`].
    pub by: Option<String>,
    /// The person whose grants changed (`person=`, ledger #4363); `None` for a line written
    /// before people, which changed the passkey alone.
    pub person: Option<String>,
}

/// Every [`GRANT_ADDED`] and [`GRANT_REMOVED`] line in [`audit_log`], in the order written. No
/// log: none. A line that does not parse is skipped: the log is a record, not a source of
/// authority (that is `clients.json`).
pub fn grant_changes(layout: &Layout) -> Result<Vec<GrantChange>, String> {
    let path = audit_log(layout);
    let text = match std::fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split(' ');
        let (Some(at), Some(class), Some(iri)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (added, key) = match class {
            GRANT_ADDED => (true, "added="),
            GRANT_REMOVED => (false, "removed="),
            _ => continue,
        };
        let Some(credential) = passkey_credential(iri) else {
            continue;
        };
        let rest: Vec<&str> = fields.collect();
        let field = |prefix: &str| rest.iter().find_map(|f| f.strip_prefix(prefix));
        let Some(grant) = field(key).map(decoded) else {
            continue;
        };
        out.push(GrantChange {
            at: at.to_string(),
            credential_id: credential.to_string(),
            added,
            grant,
            invite: field("invite=").map(str::to_string),
            by: field("by=").map(decoded),
            person: field("person=").map(decoded),
        });
    }
    Ok(out)
}

/// Take ONE grant off the PERSON a passkey names in `clients.json` and append a
/// [`GRANT_REMOVED`] line to [`audit_log`] — what `ikigai-gonk passkey remove-grant` runs
/// (ledger [#3388](http://localhost:1062/l/default/item/3388)). The person is named through
/// one of its passkeys. Returns that passkey as it is now.
///
/// ★ Grants are the person's (ledger #4305), so the person's list changes, and with it what
/// EVERY passkey of that person holds. Other people, the grant itself in `grants.json` (which
/// others may hold) and the person's other grants are left exactly as they were. The LAST
/// grant is refused — a person with none could never sign in; `passkey revoke` is the command
/// for a passkey. Checked before and after, as [`revoke`] is. Takes effect at each passkey's
/// next request, which re-reads `clients.json`.
pub fn remove_grant(
    layout: &Layout,
    which: Which<'_>,
    grant: &str,
    now_ms: u64,
) -> Result<Enrolled, String> {
    let lock = layout.lock_clients()?;
    let mut target = pick(layout, which)?;
    if !target.grants.iter().any(|g| g == grant) {
        return Err(format!(
            "passkey `{}` does not hold grant `{grant}` (it holds {}); nothing was written",
            target.label,
            target.grant_names()
        ));
    }
    if target.grants.len() == 1 {
        return Err(format!(
            "`{grant}` is the only grant passkey `{}` holds, and a passkey with none cannot sign \
             in: `ikigai-gonk passkey revoke` removes the passkey. Nothing was written",
            target.label
        ));
    }
    target.grants.retain(|g| g != grant);
    write_passkey(layout, &lock, &target)?;
    drop(lock);
    record_with(
        layout,
        GRANT_REMOVED,
        &target,
        now_ms,
        &format!(" removed={}", encoded(grant)),
    )
    .map_err(|e| {
        format!(
            "grant `{grant}` WAS removed from passkey `{}`, but its audit line could not be \
             written to {}: {e}",
            target.label,
            audit_log(layout).display()
        )
    })?;
    Ok(target)
}

/// The `by=` value of a [`GRANT_ADDED`] line [`add_grant`] writes: the box's owner, at the
/// command line, widened the passkey; no invite was redeemed.
pub const BY_OWNER: &str = "owner";

/// Add ONE grant to the PERSON a passkey names in `clients.json` and append a [`GRANT_ADDED`]
/// line to [`audit_log`] — what `ikigai-gonk passkey grant add` runs (ledger
/// [#3400](http://localhost:1062/l/default/item/3400)). Returns the passkey as it is now.
///
/// The owner's half of [`Passkeys::add_grant`]: the person is named through one of its
/// passkeys, as [`revoke`] and [`remove_grant`] name one, and only the person's list grows —
/// which every passkey of that person reads (ledger #4305). A grant `grants.json` does not
/// define (or that resolves to no scopes, or to a refused token) is refused, and so is one the
/// person already holds; nothing is written then. `grants.json` and every other person are
/// unchanged.
///
/// The line is the one an invite's widening writes, with `by=owner` where that one has
/// `invite=sha256:…`, so a person's grant history stays in one place. As there, it is written
/// under the clients lock with the change, and if it cannot be written the change is undone.
///
/// ★ **No session is ended.** Takes effect at each of the person's passkeys' next request,
/// which re-reads `clients.json` ([`Passkeys::identity`]), so a live session simply holds the
/// wider grant.
/// The invite path ends and re-establishes sessions because the one asking for the wider grant
/// is whoever holds a session cookie and an invite link, so it demands a fresh assertion; here
/// the one deciding is the owner of the box, who could as well have widened the grant itself,
/// and signing the person out would prove nothing more. `passkey logout --all` is there for an
/// owner who wants it.
pub fn add_grant(
    layout: &Layout,
    which: Which<'_>,
    grant: &str,
    now_ms: u64,
) -> Result<Enrolled, String> {
    let lock = layout.lock_clients()?;
    let mut target = pick(layout, which)?;
    if target.grants.iter().any(|g| g == grant) {
        return Err(format!(
            "passkey `{}` already holds grant `{grant}` (it holds {}); nothing was written",
            target.label,
            target.grant_names()
        ));
    }
    let grants = quic::read_grants(&layout.grants_json())?;
    quic::scopes_for_grant(&grants, grant).map_err(|e| {
        format!(
            "{e} — `ikigai-gonk passkey invite {grant} …` defines a grant in {}; nothing was \
             written",
            layout.grants_json().display()
        )
    })?;
    let before = target.clone();
    target.grants.push(grant.to_string());
    write_passkey(layout, &lock, &target)?;
    let extra = format!(" added={} by={BY_OWNER}", encoded(grant));
    if let Err(e) = record_with(layout, GRANT_ADDED, &target, now_ms, &extra) {
        // ★ No widening goes unrecorded: undone, under the same lock.
        write_passkey(layout, &lock, &before)?;
        return Err(format!(
            "grant `{grant}` was NOT added to passkey `{}`: its line could not be written to \
             {}: {e}",
            target.label,
            audit_log(layout).display()
        ));
    }
    drop(lock);
    Ok(target)
}

/// The person `key` names (ledger #4305): a person IRI (`urn:iki:gonk:person:id:…`, as
/// `passkey list` prints it), or the label of exactly one person. Returns the IRI and the
/// person.
pub fn resolve_person(layout: &Layout, key: &str) -> Result<(String, Person), String> {
    let doc = quic::read_object(&layout.clients_json())?;
    // Read through the passkeys too, so a person is found even in a file not yet migrated.
    let mut people: BTreeMap<String, Person> = parse_people(&doc)?
        .into_iter()
        .map(|(id, p)| (person_iri(&id), p))
        .collect();
    for e in parse_passkeys(&doc)?.into_values() {
        people.entry(e.person.clone()).or_insert(Person {
            label: e.label.clone(),
            grants: e.grants.clone(),
        });
    }
    let key = key.trim();
    if person_id(key).is_some() {
        return people.remove_entry(key).ok_or_else(|| {
            format!("no person is `{key}` — `ikigai-gonk passkey list` shows who there is")
        });
    }
    let mut named: Vec<(String, Person)> =
        people.into_iter().filter(|(_, p)| p.label == key).collect();
    match named.len() {
        1 => Ok(named.remove(0)),
        0 => Err(format!(
            "no person is labeled `{key}` — `ikigai-gonk passkey list` shows who there is"
        )),
        n => Err(format!(
            "{n} people are labeled `{key}` ({}): name one by its IRI, and nothing was written",
            named
                .iter()
                .map(|(iri, _)| iri.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The class of the [`audit_log`] line [`attach`] writes.
pub const ATTACHED: &str = "gonk:PasskeyAttached";

/// Attach the enrolled passkey `which` names to the person `person` names ([`resolve_person`]),
/// in place of the person it named — what `ikigai-gonk passkey attach` runs (ledger
/// [#4305](http://localhost:1062/l/default/item/4305)), and how two passkeys enrolled as two
/// people become one. The passkey then holds that person's grants, and its writes from now on
/// are that person's. The person it named before is kept, with its label and grants, even when
/// no passkey names it any more: the record still names it, and nothing can sign in as it.
/// Takes effect at the passkey's next request, as [`add_grant`] does.
pub fn attach(
    layout: &Layout,
    which: Which<'_>,
    person: &str,
    now_ms: u64,
) -> Result<(Enrolled, String), String> {
    let lock = layout.lock_clients()?;
    let mut target = pick(layout, which)?;
    let (iri, to) = resolve_person(layout, person)?;
    if target.person == iri {
        return Err(format!(
            "passkey `{}` already names person `{}` ({iri}); nothing was written",
            target.label, to.label
        ));
    }
    let before = target.clone();
    target.person = iri;
    target.grants = to.grants;
    write_passkey(layout, &lock, &target)?;
    let extra = format!(
        " person={} before={} by={BY_OWNER}",
        encoded(&target.person),
        encoded(&before.person)
    );
    if let Err(e) = record_with(layout, ATTACHED, &target, now_ms, &extra) {
        write_passkey(layout, &lock, &before)?;
        return Err(format!(
            "passkey `{}` was NOT attached: its line could not be written to {}: {e}",
            target.label,
            audit_log(layout).display()
        ));
    }
    drop(lock);
    Ok((target, before.person))
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

/// The inverse of [`encoded`]: `%XX` back to its byte, `-` back to empty.
fn decoded(text: &str) -> String {
    if text == "-" {
        return String::new();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// An invite that enrolls a new passkey of an EXISTING person (`passkey invite --person`,
/// ledger #4305) instead of a person of its own: the enrollment names that person and holds
/// its grants, at once. Returns the code, the person's IRI and the person. No grant is
/// written; the invite records the person's first grant beside it, which is what an unlabeled
/// enrollment is labeled and what the sign-in page shows.
pub fn invite_person(
    layout: &Layout,
    person: &str,
    minutes: u64,
    now: u64,
) -> Result<(String, String, Person), String> {
    let (iri, who) = resolve_person(layout, person)?;
    let code = random_b64(24)?;
    let _lock = layout.lock_invites()?;
    let mut doc = quic::read_object(&layout.invites_json())?;
    doc.retain(|_, entry| entry.get("expires").and_then(Value::as_u64).unwrap_or(0) > now);
    doc.insert(
        sha256_hex(code.as_bytes()),
        json!({ "grant": who.grants[0], "person": iri, "expires": now + minutes * 60 }),
    );
    quic::write_private(&layout.invites_json(), &quic::pretty(&Value::Object(doc))?)?;
    Ok((code, iri, who))
}

/// The person an invite enrolls under ([`invite_person`]), read without redeeming it, or
/// `None` for an invite that makes a person of its own.
fn invite_person_of(layout: &Layout, code: &str) -> Result<Option<String>, String> {
    let _lock = layout.lock_invites()?;
    let doc = quic::read_object(&layout.invites_json())?;
    Ok(doc
        .get(&sha256_hex(code.as_bytes()))
        .and_then(|entry| entry.get("person"))
        .and_then(Value::as_str)
        .map(str::to_string))
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

/// Refuse redeeming a person's invite ([`invite_person`]) to widen a signed-in passkey: it
/// enrolls a new passkey of that person, and widens nobody.
fn refuse_person_invite(layout: &Layout, code: &str) -> Result<(), String> {
    match invite_person_of(layout, code)? {
        Some(iri) => Err(format!(
            "this invite enrolls a new passkey of person {iri}, and adds no grant to one already \
             enrolled — sign out and create the passkey with it; the invite is still good"
        )),
        None => Ok(()),
    }
}

/// Refuse a passkey registered at a relying party other than this server's [`RP_ID`] (ledger
/// #4305): a passkey is bound to the site it was made for, and one made for another origin
/// must not sign in here, whatever its signature says.
fn refuse_other_rp(enrolled: &Enrolled) -> Result<(), String> {
    if enrolled.rp_id != RP_ID {
        return Err(format!(
            "passkey `{}` was registered for the site `{}`, and this page is `{RP_ID}`: a \
             passkey signs in only at the site it was registered for",
            enrolled.label, enrolled.rp_id
        ));
    }
    Ok(())
}

/// Refuse adding `grant` to a passkey that already holds it: nothing would change, and the
/// invite is better kept for someone it would widen.
fn refuse_held(enrolled: &Enrolled, grant: &str) -> Result<(), String> {
    if enrolled.grants.iter().any(|g| g == grant) {
        return Err(format!(
            "the passkey you are signed in with (`{}`) already holds grant `{grant}`; the invite \
             is still good",
            enrolled.label
        ));
    }
    Ok(())
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

/// The scopes a passkey holding `names` holds: the union of each grant's [`scopes_of`], in
/// first-seen order, through the same resolution, read once. Fail-closed as each is: one grant
/// that does not resolve refuses the whole identity, never serves it the rest, so a grant
/// deleted from `grants.json` is noticed rather than silently narrowed around.
pub fn scopes_of_all(layout: &Layout, names: &[String]) -> Result<Vec<String>, String> {
    if names.is_empty() {
        return Err("this passkey holds no grant".to_string());
    }
    let grants = layout.current_grants()?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for name in names {
        for scope in quic::scopes_for_grant(&grants, name)? {
            if seen.insert(scope.clone()) {
                out.push(scope);
            }
        }
    }
    Ok(out)
}

/// What a challenge is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `navigator.credentials.create`.
    Register,
    /// `navigator.credentials.get`.
    Login,
    /// `navigator.credentials.get`, answered to add an invite's grant to the signed-in
    /// passkey ([`Passkeys::add_grant`]): a sign-in challenge cannot be spent on it, nor it on
    /// a sign-in.
    AddGrant,
    /// `navigator.credentials.get`, answered to reopen a session's write with the SAME passkey
    /// ([`Passkeys::reassert`], ledger #3816): no other ceremony's challenge can be spent on it,
    /// nor it on another.
    Reassert,
}

impl Purpose {
    /// The byte a sealed challenge carries for it.
    fn tag(self) -> u8 {
        match self {
            Purpose::Register => b'r',
            Purpose::Login => b'l',
            Purpose::AddGrant => b'g',
            Purpose::Reassert => b'a',
        }
    }
}

/// A sealed challenge's random part, in bytes.
const NONCE_BYTES: usize = 16;
/// A sealed challenge's bytes: nonce, purpose tag, expiry (big-endian seconds), HMAC-SHA256.
const SEALED_BYTES: usize = NONCE_BYTES + 1 + 8 + 32;

struct Session {
    credential_id: String,
    /// The grants the passkey held at the sign-in (what the file records; the passkey's grants
    /// and their scopes are re-read on every request).
    grants: Vec<String>,
    /// When the sign-in opened it.
    issued: u64,
    /// The absolute end: [`Passkeys::lifetime`] after the sign-in. The write bound is not
    /// stored: it is [`Passkeys::write_seconds`] after `issued`.
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
                        "grants": s.grants,
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
        // `grants` as this server writes it; `grant`, one name, as servers before #3388 did.
        let grants = match (entry.get("grants"), entry.get("grant")) {
            (Some(Value::Array(names)), _) => names
                .iter()
                .map(|n| n.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
                .filter(|names| !names.is_empty()),
            (None, Some(Value::String(name))) => Some(vec![name.clone()]),
            _ => None,
        };
        let well_formed = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        match (
            well_formed,
            credential,
            grants,
            field("issued"),
            field("last_seen"),
            field("expires"),
        ) {
            (
                true,
                Some(credential),
                Some(grants),
                Some(issued),
                Some(last_seen),
                Some(expires),
            ) if expires > now && expires.saturating_sub(issued) <= READ_ONLY_SESSION_SECONDS => {
                sessions.insert(
                    hash.clone(),
                    Session {
                        credential_id: credential.to_string(),
                        grants,
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

/// A `navigator.credentials.get` answer that verified ([`Passkeys::verify_assertion`]).
struct Verified {
    /// The passkey that signed, as the file held it when it was checked.
    enrolled: Enrolled,
    /// The authenticator's signature counter.
    count: u32,
    /// The challenge it answered, not yet marked answered.
    challenge: String,
    /// When that challenge expires.
    expires: u64,
}

/// The HTTP door's passkey relying party and session table.
pub struct Passkeys {
    layout: Layout,
    origin: String,
    idle_seconds: u64,
    /// `gonk.http.session_max_hours`, in seconds: see [`Passkeys::lifetime`] and
    /// [`Passkeys::write_seconds`].
    max_seconds: u64,
    audit: Option<std::sync::Arc<crate::access::Record>>,
    state: Mutex<State>,
}

/// A signed-in identity, as a page or the capability function sees it.
#[derive(Debug, Clone)]
pub struct Identity {
    /// The enrollment.
    pub enrolled: Enrolled,
    /// The grant's scopes, resolved now: only those that read once the write bound has passed.
    pub scopes: Vec<String>,
    /// When the session ends at the latest (the absolute bound), in seconds since the epoch.
    /// It ends sooner if no request carries it for the idle bound.
    pub expires: u64,
    /// When the session's write lapses (the write bound), in seconds since the epoch: never
    /// later than `expires`.
    pub write_expires: u64,
    /// The grant's scopes the write bound took from `scopes`, which a fresh sign-in by the same
    /// passkey restores; empty before the write bound, and for a grant that only reads.
    pub restore: Vec<String>,
}

impl Identity {
    /// The seconds of write this session has left at `now`: 0 once the write bound has passed,
    /// and for a session whose scopes only read (a read-only grant, or write lapsed).
    pub fn write_seconds(&self, now: u64) -> u64 {
        if split_reads(&self.scopes).1.is_empty() {
            0
        } else {
            self.write_expires.saturating_sub(now)
        }
    }
}

/// [`Passkeys::labels`], for a caller with a layout and no server: the CURRENT label of every
/// person IRI and passkey IRI this config home names, each as its PERSON's label.
pub fn author_labels(layout: &Layout) -> BTreeMap<String, String> {
    let Ok(doc) = quic::read_object(&layout.clients_json()) else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<String, String> = parse_people(&doc)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, p)| (person_iri(&id), p.label))
        .collect();
    for e in parse_passkeys(&doc).unwrap_or_default().into_values() {
        let label = out.get(&e.person).cloned().unwrap_or(e.label);
        out.entry(e.person.clone()).or_insert_with(|| label.clone());
        out.insert(passkey_iri(&e.credential_id), label);
    }
    out
}

/// Every enrolled passkey's IRI and the person IRI it names now (ledger #4305): what a
/// principal recorded before people ([`passkey_iri`]) is read as since. Read through the old
/// form too, which names the person the migration writes ([`migrate_people`]).
pub fn passkey_people(layout: &Layout) -> Result<BTreeMap<String, String>, String> {
    Ok(read_passkeys(layout)?
        .into_values()
        .map(|e| (passkey_iri(&e.credential_id), e.person))
        .collect())
}

/// The principals `principal` was known by before people (ledger #4305): for a person IRI,
/// the [`passkey_iri`] of each passkey that names it; nothing for any other principal. What a
/// dispatcher cursor kept under a passkey IRI is adopted through
/// ([`crate::dispatch::space_with_former`]).
pub fn former_principals(layout: &Layout, principal: &str) -> Vec<String> {
    if person_id(principal).is_none() {
        return Vec::new();
    }
    layout
        .current_passkeys()
        .map(|enrolled| {
            enrolled
                .values()
                .filter(|e| e.person == principal)
                .map(|e| passkey_iri(&e.credential_id))
                .collect()
        })
        .unwrap_or_default()
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
        // ★ Enrollments from before people get theirs now (ledger #4305). A file this cannot
        // migrate is read as it is (`parse_passkeys` reads the old form) and reported here.
        match migrate(&layout) {
            Ok(done) => {
                if let Some(line) = migration_line(&layout, &done) {
                    eprintln!("{line}");
                }
            }
            Err(e) => eprintln!("gonk: passkeys were not given people: {e}"),
        }
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
    /// lifetime of a session whose grant holds a read scope; its write lapses sooner
    /// ([`Passkeys::write_seconds`]). Clamped to [`READ_ONLY_SESSION_SECONDS`]; the setting
    /// refuses more at load.
    pub fn with_max_seconds(mut self, seconds: u64) -> Passkeys {
        self.max_seconds = seconds.clamp(1, READ_ONLY_SESSION_SECONDS);
        self
    }

    /// `gonk.http.session_max_hours`, in seconds.
    pub fn max_seconds(&self) -> u64 {
        self.max_seconds
    }

    /// How long after its sign-in a session may write: [`SESSION_SECONDS`], or the setting if
    /// that is shorter. Absolute, never moved by use; past it the session keeps only its read
    /// scopes.
    pub fn write_seconds(&self) -> u64 {
        self.max_seconds.min(SESSION_SECONDS)
    }

    /// How long a session whose grant holds `scopes` lasts at most: the setting if any scope
    /// only reads ([`read_only`]), since that scope outlives the write bound; otherwise
    /// [`Passkeys::write_seconds`], where nothing would be left.
    pub fn lifetime(&self, scopes: &[String]) -> u64 {
        if split_reads(scopes).0.is_empty() {
            self.write_seconds()
        } else {
            self.max_seconds
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
        // names the credential it CLAIMED, and nobody. It is built from the whole id, never
        // the cut `passkey` column, so it is the IRI the door lines of the same session carry
        // (ledger #3414); an enrolled id always fits one ([`principal_refusal`]).
        // ★ The PERSON the passkey names (ledger #4305), as the door stamps it; a passkey
        // no longer enrolled is named by its own IRI.
        let principal = match line.credential {
            Some(id) if line.authenticated => self
                .layout
                .current_passkeys()
                .ok()
                .and_then(|enrolled| enrolled.get(id).map(|e| e.person.clone()))
                .unwrap_or_else(|| passkey_iri(id)),
            _ => ikigai_log::ANONYMOUS_PRINCIPAL.to_string(),
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

    /// The grant `invite` would enroll a passkey under, spending nothing — what
    /// `register-options` answers when the page names its invite, so the passkey's WebAuthn
    /// `user.displayName` can carry it (ledger [#3387](http://localhost:1062/l/default/item/3387)).
    /// An invite never issued or already used is refused here, before the authenticator mints
    /// a credential [`Passkeys::register`] would refuse.
    pub fn invite_grant(&self, invite: &str) -> Result<String, String> {
        invite_grant(&self.layout, invite)
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

    /// The CURRENT label of every author IRI this server names, keyed by the IRI: each
    /// [`person_iri`] and each enrolled [`passkey_iri`], both as their PERSON's label (ledger
    /// #4305), so an author written before people existed (a passkey IRI) and one written
    /// since (a person IRI) read alike — what a page renders an author as. Re-read from `clients.json` on every call, like every
    /// other use of that file, so a relabel shows on the next request; an unreadable file
    /// is an empty map here, and a caller falls back to the IRI's tail.
    pub fn labels(&self) -> BTreeMap<String, String> {
        author_labels(&self.layout)
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
        if let Some(why) = principal_refusal(&id) {
            return Err(why);
        }
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
        // ★ An invite made for a person (ledger #4305) binds this passkey to that person, who
        // must still be there, and it holds that person's grants from its first request.
        let person = match invite_person_of(&self.layout, &invite)? {
            Some(iri) => Some(resolve_person(&self.layout, &iri)?),
            None => None,
        };
        let grant = redeem(&self.layout, &invite, now)?;
        scopes_of(&self.layout, &grant)?;
        // Answered only now that an invite was spent on it ([`MAX_PENDING`]).
        self.answer_challenge(&challenge, expires, now)?;
        let (person, grants) = match person {
            Some((iri, who)) => (iri, who.grants),
            None => (person_iri(&new_person_id()?), vec![grant]),
        };
        let entry = Enrolled {
            credential_id: id,
            person,
            rp_id: RP_ID.to_string(),
            grants,
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
        let verified = self.verify_assertion(&v, Purpose::Login, now)?;
        // Answered only now that a passkey signed it ([`MAX_PENDING`]).
        self.answer_challenge(&verified.challenge, verified.expires, now)?;
        // ★ Decided again under the lock, against the file as it is NOW: a `passkey revoke`
        // (or a re-enrolment under another key) may have landed while this was verifying, and
        // writing the entry read before it back would undo it (ledger #2776). So the entry
        // written is the one read now, under the same key: a grant added or removed meanwhile
        // (#3388) stands, and this session opens with it.
        let lock = self.layout.lock_clients()?;
        let mut enrolled = read_passkeys(&self.layout)?
            .remove(&verified.enrolled.credential_id)
            .filter(|now| now.public_key == verified.enrolled.public_key)
            .ok_or("this passkey is not enrolled on this server")?;
        if verified.count > enrolled.sign_count {
            enrolled.sign_count = verified.count;
            write_passkey(&self.layout, &lock, &enrolled)?;
        }
        drop(lock);
        self.open_session(enrolled, now)
    }

    /// Check a `navigator.credentials.get` answer in `v` (`{challenge, id, authenticatorData,
    /// clientDataJSON, signature}`) against the enrolled key: the challenge sealed here for
    /// `purpose`, this origin and RP ID, user verification REQUIRED. Answers nothing and writes
    /// nothing: the caller marks the challenge answered once it acts on it.
    fn verify_assertion(&self, v: &Value, purpose: Purpose, now: u64) -> Result<Verified, String> {
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let challenge = text("challenge")?;
        let expires = self.check_challenge(&challenge, purpose, now)?;
        let id = text("id")?;
        let enrolled = read_passkeys(&self.layout)?
            .remove(&id)
            .ok_or("this passkey is not enrolled on this server")?;
        refuse_other_rp(&enrolled)?;
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
        Ok(Verified {
            enrolled,
            count,
            challenge,
            expires,
        })
    }

    /// Open a session for `enrolled`, as its grants resolve now: the token (the only copy) and
    /// the identity.
    fn open_session(&self, enrolled: Enrolled, now: u64) -> Result<(String, Identity), String> {
        let scopes = scopes_of_all(&self.layout, &enrolled.grants)?;
        let token = random_b64(32)?;
        let expires = now + self.lifetime(&scopes);
        let write_expires = expires.min(now + self.write_seconds());
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
                credential_id: enrolled.credential_id.clone(),
                grants: enrolled.grants.clone(),
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
                write_expires,
                restore: Vec::new(),
            },
        ))
    }

    /// What redeeming `invite` while signed in with the session `token` would add, spending
    /// nothing: the grant, and the credential id of the passkey that session signs in with
    /// (so the page can ask the authenticator for that passkey only). The grant would be added
    /// to that passkey's PERSON. Refused, before any ceremony, when `token` is no live session,
    /// the invite is not valid, or the person already holds the grant.
    pub fn add_grant_preview(
        &self,
        token: &str,
        invite: &str,
        now: u64,
    ) -> Result<(String, String), String> {
        self.add_grant_preview_as(SignedIn::Token(token), invite, now)
    }

    /// [`Passkeys::add_grant_preview`] for whoever [`SignedIn`] names.
    pub fn add_grant_preview_as(
        &self,
        signed_in: SignedIn<'_>,
        invite: &str,
        now: u64,
    ) -> Result<(String, String), String> {
        let who = self.signed_in(signed_in, now).ok_or(
            "you are not signed in — sign in first, or create a new passkey with the invite",
        )?;
        refuse_person_invite(&self.layout, invite)?;
        let grant = invite_grant(&self.layout, invite)?;
        refuse_held(&who.enrolled, &grant)?;
        Ok((grant, who.enrolled.credential_id))
    }

    /// Redeem an invite while signed in by ADDING its grant to the signed-in PERSON (ledger
    /// [#3388](http://localhost:1062/l/default/item/3388), #4305), instead of enrolling a new
    /// passkey. The body: `{session, invite, challenge, id, authenticatorData, clientDataJSON,
    /// signature}`, the last five a FRESH `navigator.credentials.get` answer to an
    /// [`Purpose::AddGrant`] challenge. Returns a new session token (the only copy), the
    /// identity as it is now, and the grant added.
    ///
    /// ★ **A session alone is not enough.** The assertion must be signed, with user
    /// verification, by a passkey of the same PERSON as the one `session` signs in with (that
    /// passkey or another of the person's devices), so a stolen cookie plus an invite link
    /// cannot raise its own access: it would need one of the person's authenticators and their
    /// biometric or PIN too. A passkey of another person is refused.
    ///
    /// ★ **The person widens, and only the person.** The grant is appended to the PERSON's
    /// list in `clients.json`, so every passkey of that person holds it; no grant in
    /// `grants.json` is edited, so every other person keeps exactly what they had, including
    /// one enrolled under a grant this person also holds.
    ///
    /// ★ **The sessions are ended and re-established**, not refreshed: every session of every
    /// passkey of the person (in this browser and any other device) is DELETED, and one new
    /// session is opened from the fresh assertion, for the passkey that answered, so the only
    /// session carrying the wider grant is the one whose holder just proved a passkey of the
    /// person. Its lifetime is decided anew by the wider scopes. They are deleted BEFORE the
    /// wider grant is written (ledger #3413), so none reads it in the meantime; a widening
    /// refused after that point leaves the person signed out everywhere.
    ///
    /// Every widening appends a [`GRANT_ADDED`] line to [`audit_log`] naming the passkey that
    /// answered, the person, the grant and the invite (by its hash), under the clients lock with
    /// the change: if the line cannot be written the change is undone and refused, so no
    /// widening is unrecorded.
    pub fn add_grant(&self, body: &[u8], now: u64) -> Result<(String, Identity, String), String> {
        self.add_grant_as(body, SignedIn::Principal(None), now)
    }

    /// [`Passkeys::add_grant`], where a body with no `session` is signed in as `door` says
    /// (the HTTP door's request: its session key and principal, from the HttpOnly cookie the
    /// page cannot read).
    pub fn add_grant_as(
        &self,
        body: &[u8],
        door: SignedIn<'_>,
        now: u64,
    ) -> Result<(String, Identity, String), String> {
        let answer = self.widen(body, door, now);
        let claimed = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string));
        self.audit(
            LOGIN_CLASS,
            AuditLine {
                op: "add-grant",
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

    fn widen(
        &self,
        body: &[u8],
        door: SignedIn<'_>,
        now: u64,
    ) -> Result<(String, Identity, String), String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let text = |name: &str| {
            v.get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{name}` is missing"))
        };
        let token = text("session").ok();
        let invite = text("invite")?;
        let signed_in = match token.as_deref() {
            Some(token) => SignedIn::Token(token.trim()),
            None => door,
        };
        let who = self.signed_in(signed_in, now).ok_or(
            "you are not signed in — sign in first, or create a new passkey with the invite",
        )?;
        let verified = self.verify_assertion(&v, Purpose::AddGrant, now)?;
        // ★ The same PERSON (ledger #4305): a grant is added to the person, so any of its
        // passkeys may answer for it, whichever of them the session was opened with.
        if verified.enrolled.person != who.enrolled.person {
            return Err(format!(
                "the passkey that answered is not a passkey of the person you are signed in as \
                 (`{}`); the invite is still good",
                who.enrolled.label
            ));
        }
        let lock = self.layout.lock_clients()?;
        let mut current = read_passkeys(&self.layout)?
            .remove(&verified.enrolled.credential_id)
            .filter(|now| now.public_key == verified.enrolled.public_key)
            .ok_or("this passkey is not enrolled on this server")?;
        // Everything that could refuse comes before the invite is spent: a grant already held
        // or one that does not resolve must not burn a single-use code.
        refuse_person_invite(&self.layout, &invite)?;
        let grant = invite_grant(&self.layout, &invite)?;
        refuse_held(&current, &grant)?;
        scopes_of(&self.layout, &grant)?;
        // Answered before the invite is spent, so a replayed answer cannot burn it; only the
        // signed-in passkey's own verified answer gets here ([`MAX_PENDING`]).
        self.answer_challenge(&verified.challenge, verified.expires, now)?;
        let redeemed = redeem(&self.layout, &invite, now)?;
        // ★ Every old session ends BEFORE the wider grant is written: a request re-reads
        // `clients.json` and then finds its session gone, so no session but the fresh one below
        // ever reads the wider grant, even while the line is written or the change undone
        // (ledger #3413).
        // ★ And every session of every OTHER passkey of this person (ledger #4305): the grant
        // is the person's, so each of them would read it.
        let mut credentials = self.credentials_of(&current.person);
        if !credentials.contains(&current.credential_id) {
            credentials.push(current.credential_id.clone());
        }
        for credential in &credentials {
            let ended = {
                let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
                let ended = state.end_sessions(credential);
                self.save(&mut state);
                ended
            };
            self.audit(
                LOGOUT_CLASS,
                AuditLine {
                    op: "session",
                    door: "http",
                    outcome: "ok",
                    credential: Some(credential),
                    authenticated: true,
                    sessions: ended,
                    why: Some("widened"),
                },
            );
        }
        let before = current.clone();
        current.grants.push(redeemed.clone());
        current.sign_count = current.sign_count.max(verified.count);
        write_passkey(&self.layout, &lock, &current)?;
        let now_ms = now.saturating_mul(1000);
        let extra = format!(
            " added={} invite=sha256:{}",
            encoded(&redeemed),
            sha256_hex(invite.as_bytes())
        );
        if let Err(e) = record_with(&self.layout, GRANT_ADDED, &current, now_ms, &extra) {
            // ★ No widening goes unrecorded: undone, under the same lock. The invite is spent.
            write_passkey(&self.layout, &lock, &before)?;
            return Err(format!(
                "grant `{redeemed}` was NOT added: its line could not be written to {}: {e}. \
                 The invite was spent; ask for a new one, and sign in again",
                audit_log(&self.layout).display()
            ));
        }
        drop(lock);
        let (token, identity) = self.open_session(current, now)?;
        Ok((token, identity, redeemed))
    }

    /// The session a re-assertion replaces, as its key in [`State::sessions`] (the SHA-256 of
    /// its token): the token's when `token` is given; else, for the HTTP door (whose page cannot
    /// read its HttpOnly cookie), `session_id` — the non-secret [`session_id`] the sign-in
    /// answered — which must be a session of a passkey the door's principal stands for; with
    /// neither, the request's own session ([`SignedIn::Session`], ledger #4363), or for a
    /// caller with a principal only, that principal's only live session. Never a guess
    /// between two.
    fn reassert_target(
        &self,
        token: Option<&str>,
        session_id: Option<&str>,
        door: SignedIn<'_>,
    ) -> Result<String, String> {
        let not_signed_in = || "you are not signed in — sign in first".to_string();
        if let Some(token) = token {
            return Ok(sha256_hex(token.trim().as_bytes()));
        }
        if let SignedIn::Token(token) = door {
            return Ok(sha256_hex(token.trim().as_bytes()));
        }
        let credentials = door
            .principal()
            .map(|p| self.credentials_of(p))
            .filter(|c| !c.is_empty())
            .ok_or_else(not_signed_in)?;
        let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
        self.read_log(&mut state);
        let mine: Vec<&String> = state
            .sessions
            .iter()
            .filter(|(_, s)| credentials.contains(&s.credential_id))
            .map(|(key, _)| key)
            .collect();
        match (session_id, door) {
            (Some(id), _) => mine
                .into_iter()
                .find(|key| key.as_str() == id.trim())
                .cloned()
                .ok_or_else(|| "that session is not one of your live sessions".into()),
            // ★ The request's own session, whatever other device of the person is signed in.
            (None, SignedIn::Session { key, .. }) => mine
                .into_iter()
                .find(|mine| mine.as_str() == key)
                .cloned()
                .ok_or_else(not_signed_in),
            (None, _) => match mine.as_slice() {
                [only] => Ok((*only).clone()),
                [] => Err(not_signed_in()),
                _ => Err(
                    "you have more than one session; name this one by its `sessionId`, or \
                     sign in again"
                        .to_string(),
                ),
            },
        }
    }

    /// The live session the re-assertion would replace and the credential id of its passkey,
    /// so the page can ask the authenticator for that passkey only (`allowCredentials`), and a
    /// fresh [`Purpose::Reassert`] challenge. Spends nothing.
    pub fn reassert_options(
        &self,
        token: Option<&str>,
        session_id: Option<&str>,
        door: SignedIn<'_>,
        now: u64,
    ) -> Result<(String, String), String> {
        let key = self.reassert_target(token, session_id, door)?;
        let who = self
            .identity_at(key, now)
            .ok_or("you are not signed in — sign in first")?;
        let challenge = self.challenge(Purpose::Reassert, now)?;
        Ok((who.enrolled.credential_id, challenge))
    }

    /// Reopen write with a fresh assertion by the SAME passkey (ledger #3816), once a session's
    /// write has lapsed (#3815). The body: `{session?, sessionId?, challenge, id,
    /// authenticatorData, clientDataJSON, signature}`, the last five a fresh
    /// `navigator.credentials.get` answer to a [`Purpose::Reassert`] challenge. It replaces the
    /// session `session` (a token) names; else the one `sessionId` ([`session_id`]) names, of
    /// a passkey the door's principal stands for; else the request's own session (`door`,
    /// [`SignedIn::Session`]). Returns the new token (the only copy) and the identity.
    ///
    /// ★ **The same passkey, verified as a sign-in is.** The assertion must be by the
    /// passkey the session signs in as, with user verification, and its signature counter is
    /// checked and stored as a sign-in's is: a free choice of passkey would let a re-assertion
    /// change whom later writes are attributed to.
    ///
    /// ★ **Only that session is replaced.** It is ended and a new one opened with fresh
    /// bounds; the passkey's other sessions (another device's) are untouched, unlike a
    /// sign-out or [`Passkeys::add_grant`].
    ///
    /// Writes one `gonk:Login` line with `op=reassert` either way.
    pub fn reassert(
        &self,
        body: &[u8],
        door: SignedIn<'_>,
        now: u64,
    ) -> Result<(String, Identity), String> {
        let answer = self.reassert_session(body, door, now);
        let claimed = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string));
        self.audit(
            LOGIN_CLASS,
            AuditLine {
                op: "reassert",
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

    fn reassert_session(
        &self,
        body: &[u8],
        door: SignedIn<'_>,
        now: u64,
    ) -> Result<(String, Identity), String> {
        let v: Value = serde_json::from_slice(body).map_err(|e| format!("not JSON: {e}"))?;
        let field = |name: &str| v.get(name).and_then(Value::as_str);
        let key = self.reassert_target(field("session"), field("sessionId"), door)?;
        let who = self
            .identity_at(key.clone(), now)
            .ok_or("you are not signed in — sign in first")?;
        let verified = self.verify_assertion(&v, Purpose::Reassert, now)?;
        if verified.enrolled.credential_id != who.enrolled.credential_id {
            return Err(format!(
                "the passkey that answered is not the one you are signed in with (`{}`)",
                who.enrolled.label
            ));
        }
        // Answered only now that the session's own passkey signed it ([`MAX_PENDING`]).
        self.answer_challenge(&verified.challenge, verified.expires, now)?;
        // As [`Passkeys::sign_in`]: the entry is re-read under the lock, so a revocation or a
        // grant change meanwhile stands, and the counter is stored.
        let lock = self.layout.lock_clients()?;
        let mut enrolled = read_passkeys(&self.layout)?
            .remove(&verified.enrolled.credential_id)
            .filter(|now| now.public_key == verified.enrolled.public_key)
            .ok_or("this passkey is not enrolled on this server")?;
        if verified.count > enrolled.sign_count {
            enrolled.sign_count = verified.count;
            write_passkey(&self.layout, &lock, &enrolled)?;
        }
        drop(lock);
        // ★ The one session it replaces, and no other of this passkey.
        let ended = {
            let mut state = self.state.lock().map_err(|_| "passkey state poisoned")?;
            let ended = state.sessions.remove(&key).is_some();
            state.dirty |= ended;
            self.save(&mut state);
            ended
        };
        if !ended {
            return Err("that session ended while the passkey was answering".to_string());
        }
        self.audit(
            LOGOUT_CLASS,
            AuditLine {
                op: "session",
                door: "http",
                outcome: "ok",
                credential: Some(&enrolled.credential_id),
                authenticated: true,
                sessions: 1,
                why: Some("reasserted"),
            },
        );
        self.open_session(enrolled, now)
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

    /// The identity [`SignedIn`] names, re-resolved now. Only the one session it names has
    /// its idle clock restarted.
    pub fn signed_in(&self, who: SignedIn<'_>, now: u64) -> Option<Identity> {
        match who {
            SignedIn::Token(token) => self.identity(token, now),
            SignedIn::Principal(principal) => {
                self.identity_of_any(&self.credentials_of(principal?), now)
            }
            SignedIn::Session { key, principal } => {
                let credentials = principal.map(|p| self.credentials_of(p));
                let credential = self.session_credential(key)?;
                // The door stamped the principal from the same cookie, so they agree; if they
                // ever did not, the request names nobody rather than a session it is not.
                if credentials.is_some_and(|c| !c.contains(&credential)) {
                    return None;
                }
                self.identity_at(key.to_string(), now)
            }
        }
    }

    /// The credential id of the live session stored under `key`, without touching it.
    fn session_credential(&self, key: &str) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        self.read_log(&mut state);
        state.sessions.get(key).map(|s| s.credential_id.clone())
    }

    /// The credential ids a door's principal stands for (ledger #4305): the one passkey a
    /// [`passkey_iri`] names (a session restored from before people), or every enrolled
    /// passkey of the person a [`person_iri`] names, as the door now stamps. Empty for any
    /// other principal, or a person no passkey names any more.
    pub fn credentials_of(&self, principal: &str) -> Vec<String> {
        if let Some(credential) = passkey_credential(principal) {
            return vec![credential.to_string()];
        }
        if person_id(principal).is_none() {
            return Vec::new();
        }
        self.layout
            .current_passkeys()
            .map(|enrolled| {
                enrolled
                    .values()
                    .filter(|e| e.person == principal)
                    .map(|e| e.credential_id.clone())
                    .collect()
            })
            .unwrap_or_default()
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
        self.identity_at(sha256_hex(token.as_bytes()), now)
    }

    /// The identity of the passkey `credential_id` through its newest live session, or `None`
    /// when it has none: [`Passkeys::identity`] for a caller who holds no token, because the
    /// cookie is HttpOnly and the door has already named the passkey from it (the
    /// capability's principal, [`passkey_iri`]).
    pub fn identity_of(&self, credential_id: &str, now: u64) -> Option<Identity> {
        self.identity_of_any(&[credential_id.to_string()], now)
    }

    /// [`Passkeys::identity_of`] over several passkeys — a person's ([`Passkeys::credentials_of`]):
    /// the newest live session of any of them.
    pub fn identity_of_any(&self, credential_ids: &[String], now: u64) -> Option<Identity> {
        let mut keys: Vec<(u64, String)> = {
            let mut state = self.state.lock().ok()?;
            self.read_log(&mut state);
            state
                .sessions
                .iter()
                .filter(|(_, s)| credential_ids.contains(&s.credential_id))
                .map(|(key, s)| (s.issued, key.clone()))
                .collect()
        };
        keys.sort_unstable_by(|a, b| b.cmp(a));
        keys.into_iter()
            .find_map(|(_, key)| self.identity_at(key, now))
    }

    /// End EVERY session of the passkey `credential_id` — what signing out does now that the
    /// page cannot read its own token (the cookie is HttpOnly), so it cannot name the one
    /// session to end. Returns how many ended; writes one `gonk:Logout` line.
    pub fn logout_credential(&self, credential_id: &str, op: &'static str) -> usize {
        let ended = self
            .state
            .lock()
            .map(|mut state| {
                let ended = state.end_sessions(credential_id);
                self.save(&mut state);
                ended
            })
            .unwrap_or(0);
        self.audit(
            LOGOUT_CLASS,
            AuditLine {
                op,
                door: "http",
                outcome: "ok",
                credential: Some(credential_id),
                authenticated: true,
                sessions: ended,
                why: Some(op),
            },
        );
        ended
    }

    fn identity_at(&self, key: String, now: u64) -> Option<Identity> {
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
        let scopes = scopes_of_all(&self.layout, &enrolled.grants).ok()?;
        // ★ The grant is re-read NOW, and so are both bounds: past the write bound only the
        // read scopes are left (ledger #3815), and a grant with none ends there.
        let expires = expires.min(issued.saturating_add(self.lifetime(&scopes)));
        let write_expires = expires.min(issued.saturating_add(self.write_seconds()));
        let (scopes, restore) = if write_expires <= now {
            split_reads(&scopes)
        } else {
            (scopes, Vec::new())
        };
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
            write_expires,
            restore,
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

/// A session's non-secret name: the SHA-256 of its token, the key [`Passkeys`] holds it under.
/// A page that cannot read its HttpOnly cookie keeps this to name its own session to
/// [`Passkeys::reassert`]; it signs in as nobody (the token is its preimage).
pub fn session_id(token: &str) -> String {
    sha256_hex(token.as_bytes())
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

    /// An old `clients.json`: two passkeys, each with its own grants and no person.
    fn old_clients(layout: &Layout) -> String {
        let text = r#"{
  "passkeys": {
    "bGFwdG9w": { "grants": ["brian", "reviewer"], "label": "laptop", "public_key": "AAAA", "sign_count": 3 },
    "cGhvbmU": { "grant": "brian", "label": "phone", "public_key": "BBBB" }
  }
}
"#;
        std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
        std::fs::write(layout.clients_json(), text).unwrap();
        text.to_string()
    }

    /// ★ Ledger #4305: an old file migrates to one person per passkey, carrying its label and
    /// grants, at `rp_id = "localhost"`; the old file is kept beside it; a second run is a no-op.
    #[test]
    fn an_old_file_migrates_to_one_person_per_passkey_once() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        let before = old_clients(&layout);
        let read_before = read_passkeys(&layout).unwrap();
        assert_eq!(migrate_people(&layout).unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(people_backup(&layout)).unwrap(),
            before
        );
        let doc = quic::read_object(&layout.clients_json()).unwrap();
        let people = parse_people(&doc).unwrap();
        assert_eq!(people.len(), 2, "nothing is merged");
        let passkeys = read_passkeys(&layout).unwrap();
        assert_eq!(
            passkeys, read_before,
            "the old form reads as the migrated one"
        );
        let laptop = &passkeys["bGFwdG9w"];
        let phone = &passkeys["cGhvbmU"];
        assert_ne!(laptop.person, phone.person);
        assert_eq!(laptop.rp_id, "localhost");
        assert_eq!(phone.rp_id, "localhost");
        assert_eq!(laptop.sign_count, 3);
        let mine = &people[person_id(&laptop.person).unwrap()];
        assert_eq!(mine.label, "laptop");
        assert_eq!(mine.grants, ["brian", "reviewer"]);
        assert_eq!(people[person_id(&phone.person).unwrap()].grants, ["brian"]);
        assert!(laptop.person.len() < 64, "short: {}", laptop.person);
        assert!(ikigai_core::Capability::scoped(Vec::<String>::new())
            .with_principal(&laptop.person)
            .is_ok());
        let migrated = std::fs::read_to_string(layout.clients_json()).unwrap();
        let doc = quic::read_object(&layout.clients_json()).unwrap();
        assert_eq!(
            doc["passkeys"]["bGFwdG9w"]["grants"],
            json!(["brian", "reviewer"]),
            "grants are the person's, mirrored for an older gonk (ledger #4368)"
        );
        assert!(
            doc["passkeys"]["bGFwdG9w"].get("grant").is_none(),
            "two: no `grant`"
        );
        assert_eq!(
            doc["passkeys"]["cGhvbmU"]["grant"], "brian",
            "one: `grant` too"
        );
        assert_eq!(
            migrate_people(&layout).unwrap(),
            0,
            "a second run changes nothing"
        );
        assert_eq!(
            std::fs::read_to_string(layout.clients_json()).unwrap(),
            migrated
        );
        assert_eq!(
            std::fs::read_to_string(people_backup(&layout)).unwrap(),
            before
        );
    }

    /// ★ Ledger #4363: an old-form entry put back by hand for a credential whose person is
    /// already in `people` leaves that person, and its grants, as they are. Ledger #4368: the
    /// entry is pointed at that person, and holds its grants, not its own.
    #[test]
    fn a_hand_edited_old_entry_never_overwrites_its_person() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        old_clients(&layout);
        migrate_people(&layout).unwrap();
        let mut doc = quic::read_object(&layout.clients_json()).unwrap();
        let laptop = read_passkeys(&layout).unwrap()["bGFwdG9w"].clone();
        let pid = person_id(&laptop.person).unwrap().to_string();
        let passkeys = doc.get_mut("passkeys").unwrap().as_object_mut().unwrap();
        // The phone joins the laptop's person; then the laptop's old entry is put back.
        passkeys["cGhvbmU"]["person"] = Value::String(laptop.person.clone());
        passkeys["bGFwdG9w"] = json!({
            "grant": "root-ish", "label": "laptop", "public_key": "AAAA", "sign_count": 3
        });
        let edited = quic::pretty(&Value::Object(doc)).unwrap();
        std::fs::write(layout.clients_json(), &edited).unwrap();
        assert_eq!(
            read_passkeys(&layout).unwrap()["bGFwdG9w"].grants,
            ["brian", "reviewer"],
            "read before its migration, the entry already holds its person's grants"
        );
        assert_eq!(migrate_people(&layout).unwrap(), 1, "pointed at its person");
        let doc = quic::read_object(&layout.clients_json()).unwrap();
        assert_eq!(
            doc["passkeys"]["bGFwdG9w"]["person"],
            laptop.person.as_str()
        );
        assert_eq!(
            doc["passkeys"]["bGFwdG9w"]["grants"],
            json!(["brian", "reviewer"])
        );
        assert!(doc["passkeys"]["bGFwdG9w"].get("grant").is_none());
        let doc = quic::read_object(&layout.clients_json()).unwrap();
        assert_eq!(
            parse_people(&doc).unwrap()[&pid].grants,
            ["brian", "reviewer"],
            "the person's grants are kept"
        );
        assert_eq!(
            read_passkeys(&layout).unwrap()["cGhvbmU"].grants,
            ["brian", "reviewer"],
            "the person's other passkey keeps its authority"
        );
    }

    /// ★ Ledger #4305: a passkey's grants are its person's, so two passkeys naming one person
    /// hold one list, and an author naming either passkey or the person reads as the person's
    /// label — an author written before the upgrade (a passkey IRI) included.
    #[test]
    fn two_passkeys_of_one_person_share_its_grants_and_label() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        old_clients(&layout);
        migrate_people(&layout).unwrap();
        let mut doc = quic::read_object(&layout.clients_json()).unwrap();
        let laptop = read_passkeys(&layout).unwrap()["bGFwdG9w"].clone();
        let passkeys = doc.get_mut("passkeys").unwrap().as_object_mut().unwrap();
        passkeys["cGhvbmU"]["person"] = Value::String(laptop.person.clone());
        std::fs::write(
            layout.clients_json(),
            quic::pretty(&Value::Object(doc)).unwrap(),
        )
        .unwrap();
        let phone = read_passkeys(&layout).unwrap()["cGhvbmU"].clone();
        assert_eq!(phone.person, laptop.person);
        assert_eq!(phone.grants, ["brian", "reviewer"], "the person's grants");
        let rp = Passkeys::new(layout.clone(), 1060);
        let labels = rp.labels();
        assert_eq!(labels[&laptop.person], "laptop");
        assert_eq!(labels[&passkey_iri("bGFwdG9w")], "laptop");
        assert_eq!(
            labels[&passkey_iri("cGhvbmU")],
            "laptop",
            "a pre-upgrade author"
        );
    }

    /// ★ Ledger #4305: a passkey registered at another relying party is refused at sign-in,
    /// before its signature is looked at, with a sentence that says why.
    #[test]
    fn a_passkey_from_another_relying_party_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_config_home(dir.path());
        std::fs::create_dir_all(layout.clients_json().parent().unwrap()).unwrap();
        let rp = Passkeys::new(layout.clone(), 1060);
        write_passkey(
            &layout,
            &layout.lock_clients().unwrap(),
            &Enrolled {
                credential_id: "ZWxzZXdoZXJl".to_string(),
                person: person_iri("cGVyc29u"),
                rp_id: "gonk.example.org".to_string(),
                grants: vec!["brian".to_string()],
                label: "elsewhere".to_string(),
                public_key: "AAAA".to_string(),
                sign_count: 0,
            },
        )
        .unwrap();
        let challenge = rp.challenge(Purpose::Login, 10).unwrap();
        let body = json!({
            "challenge": challenge,
            "id": "ZWxzZXdoZXJl",
            "authenticatorData": "AAAA",
            "clientDataJSON": "AAAA",
            "signature": "AAAA",
        });
        let refused = rp
            .login(body.to_string().as_bytes(), 11)
            .expect_err("refused");
        assert!(
            refused.contains("registered for the site `gonk.example.org`")
                && refused.contains("only at the site it was registered for"),
            "{refused}"
        );
    }

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
                person: person_iri(&migrated_person_id(id)),
                rp_id: RP_ID.to_string(),
                grants: vec![grant.to_string()],
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
                grants: vec![grant.to_string()],
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
        assert_eq!(entry["grants"], json!(["brian"]));
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
                    person: person_iri(&migrated_person_id(id)),
                    rp_id: RP_ID.to_string(),
                    grants: vec!["brian".to_string()],
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
    /// past 12 h, to 240 h, across a restart, and never had write to lose; a write grant's
    /// session under the same setting only reads past 12 h (ledger #3815); and so does a
    /// read-only grant widened to write.
    #[test]
    fn only_reads_outlive_twelve_hours() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        let reader = session_as(&rp, "cmVhZA", "reader", Authority::Read, 1_000);
        let writer = session_as(&rp, "d3JpdGU", "writer", Authority::Write, 1_000);
        let widened = session_as(&rp, "d2lkZQ", "widened", Authority::Read, 1_000);

        let twelve = 1_000 + SESSION_SECONDS;
        let who = rp.identity(&writer, twelve - 1).unwrap();
        assert!(!read_only(&who.scopes));
        let who = rp
            .identity(&writer, twelve)
            .expect("a write grant still reads");
        assert!(read_only(&who.scopes), "a write grant only reads at 12 h");
        assert!(!who.restore.is_empty());
        let who = rp
            .identity(&reader, twelve + 3_600)
            .expect("read-only past 12 h");
        assert_eq!(who.expires, 1_000 + 240 * 3_600);
        assert!(
            who.restore.is_empty(),
            "nothing to restore: it never could write"
        );
        assert_eq!(who.write_seconds(twelve + 3_600), 0);
        let who = rp.identity(&reader, 1_100).unwrap();
        assert_eq!(
            who.write_seconds(1_100),
            0,
            "a read-only grant has no write"
        );

        quic::put_grant(
            &rp.layout,
            "widened",
            &grants_for("default", Authority::Write).unwrap(),
            true,
        )
        .unwrap();
        let who = rp
            .identity(&widened, twelve + 3_600)
            .expect("a grant that can write now still reads");
        assert!(
            read_only(&who.scopes),
            "and writes no longer, 12 h after the sign-in"
        );
        assert!(!who.restore.is_empty());

        let rp = restart(&rp, twelve + 7_200);
        assert!(rp.identity(&reader, 1_000 + 239 * 3_600).is_some());
        assert!(rp.identity(&reader, 1_000 + 240 * 3_600).is_none());
    }

    /// ★ **Write lapses, reads run on** (ledger #3815): under `gonk.http.session_max_hours =
    /// 240`, a session whose grant holds read and write scopes writes until 12 h after its
    /// sign-in, is refused the write after that, and still reads until 240 h. A request at
    /// 11 h does not move the write bound.
    #[test]
    fn write_lapses_at_twelve_hours_and_reads_run_on() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        let token = session_as(&rp, "bWl4ZWQ", "mixed", Authority::Write, 1_000);
        let all = scopes_of(&rp.layout, "mixed").unwrap();
        let mut reads = grants_for("default", Authority::Read).unwrap();
        reads.push(ikigai_store::cap_read_graph(crate::participants::GRAPH));
        let lapsed: Vec<String> = all.iter().filter(|s| !reads.contains(s)).cloned().collect();
        assert!(lapsed.iter().any(|s| s.contains(":write:")), "{lapsed:?}");
        let write = 1_000 + SESSION_SECONDS;

        // ★ Busy all the way: a request every five minutes does not move the write bound.
        let mut now = 1_000;
        while now < write - 300 {
            now += 300;
            let who = rp.identity(&token, now).expect("writes before 12 h");
            assert_eq!(
                who.scopes, all,
                "every scope before the write bound, at {now}"
            );
            assert!(who.restore.is_empty());
            assert_eq!(who.write_seconds(now), write - now);
            assert_eq!(who.expires, 1_000 + 240 * 3_600);
        }

        let who = rp.identity(&token, write).expect("still signed in at 12 h");
        assert_eq!(
            who.scopes, reads,
            "only the read scopes past the write bound"
        );
        assert!(read_only(&who.scopes));
        assert_eq!(who.restore, lapsed, "what a fresh sign-in restores");
        assert_eq!(who.write_seconds(write), 0);
        assert_eq!(who.expires, 1_000 + 240 * 3_600);

        let rp = restart(&rp, write + 3_600);
        let who = rp
            .identity(&token, 1_000 + 239 * 3_600)
            .expect("reads to 240 h");
        assert_eq!(who.scopes, reads);
        assert!(rp.identity(&token, 1_000 + 240 * 3_600).is_none());
    }

    /// ★ `session` says so (ledger #3815): `writeSeconds` counts down beside `seconds`, then is
    /// 0 with `restore` naming the scopes a fresh sign-in gives back; a read-only grant's
    /// `writeSeconds` is 0 from the start with nothing to restore.
    #[test]
    fn the_session_answer_counts_write_down_and_names_what_a_sign_in_restores() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        let writer = session_as(&rp, "d3JpdGU", "writer", Authority::Write, 1_000);
        let reader = session_as(&rp, "cmVhZA", "reader", Authority::Read, 1_000);
        let answer = |token: &str, now: u64| {
            crate::web::session_answer(&rp.identity(token, now).unwrap(), now)
        };
        let end = 1_000 + 240 * 3_600;
        let write = 1_000 + SESSION_SECONDS;

        let early = answer(&writer, 1_000 + 60);
        assert_eq!(early["seconds"], end - 1_060);
        assert_eq!(early["writeSeconds"], write - 1_060);
        assert_eq!(early["restore"], json!([]));
        let later = answer(&writer, write - 60);
        assert_eq!(later["writeSeconds"], 60, "counting down: {later}");

        let lapsed = answer(&writer, write);
        assert_eq!(lapsed["writeSeconds"], 0);
        assert_eq!(lapsed["seconds"], end - write);
        let restore = lapsed["restore"].as_array().unwrap();
        assert!(!restore.is_empty(), "{lapsed}");
        assert!(
            restore
                .iter()
                .all(|s| !read_only(&[s.as_str().unwrap().to_string()])),
            "{lapsed}"
        );
        assert!(
            restore
                .iter()
                .all(|s| !lapsed["scopes"].as_array().unwrap().contains(s)),
            "{lapsed}"
        );

        let never = answer(&reader, 1_000 + 60);
        assert_eq!(never["writeSeconds"], 0, "{never}");
        assert_eq!(never["restore"], json!([]), "{never}");
        assert_eq!(never["seconds"], end - 1_060);
    }

    /// ★ A grant with no read scope has nothing left past the write bound (ledger #3815): its
    /// session ends there, at 12 h, under `gonk.http.session_max_hours = 240`.
    #[test]
    fn a_write_only_grant_ends_at_the_write_bound() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        let writes = vec!["urn:cap:ledger:comment:default".to_string()];
        quic::put_grant(&rp.layout, "commenter", &writes, false).unwrap();
        let token = session_as(&rp, "Y29tbWVudA", "commenter", Authority::Write, 1_000);
        assert_eq!(rp.lifetime(&writes), SESSION_SECONDS);
        let write = 1_000 + SESSION_SECONDS;
        let who = rp.identity(&token, write - 1).expect("writes before 12 h");
        assert_eq!(who.scopes, writes);
        assert_eq!(who.expires, write);
        assert_eq!(who.write_seconds(write - 1), 1);
        assert!(
            rp.identity(&token, write).is_none(),
            "ends at the write bound"
        );
        assert!(
            rp.identity(&token, write - 1).is_none(),
            "deleted, not just refused"
        );
    }

    /// Under a `gonk.http.session_max_hours` shorter than 12 h the write bound is the session's
    /// end: nothing lapses before it.
    #[test]
    fn a_short_setting_is_also_the_write_bound() {
        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("2")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        assert_eq!(rp.write_seconds(), hours);
        let token = session_for(&rp, "c2hvcnQ", 1_000);
        let who = rp.identity(&token, 1_000 + hours - 1).unwrap();
        assert!(who.restore.is_empty());
        assert_eq!(who.write_seconds(1_000 + hours - 1), 1);
        assert!(rp.identity(&token, 1_000 + hours).is_none());
    }

    /// ★ **The point of the exec split** (ledger #3820): a passkey whose grant is a ledger's
    /// reads plus `urn:cap:exec:read:git` (what a Git object read and ikigai-repo 0.6.0's git
    /// facades require) only reads, so under `gonk.http.session_max_hours = 240` its session
    /// runs to 240 h. The retired spelling `urn:cap:exec:git` never passed [`read_only`], which
    /// held every reader of code links to 12 h.
    #[test]
    fn reads_and_git_reads_get_the_read_only_session() {
        let mut git_reader = grants_for("default", Authority::Read).unwrap();
        git_reader.push(crate::grants::CAP_EXEC_READ_GIT.to_string());
        assert!(read_only(&git_reader), "{git_reader:?}");
        let mut retired = grants_for("default", Authority::Read).unwrap();
        retired.push("urn:cap:exec:git".to_string());
        assert!(!read_only(&retired), "the old spelling was never a read");

        let dir = tempfile::tempdir().unwrap();
        let hours = crate::config::session_max_seconds(Some("240")).unwrap();
        let rp = Passkeys::new_at(Layout::in_config_home(dir.path()), 1060, 1_000)
            .with_idle_seconds(hours)
            .with_max_seconds(hours);
        quic::put_grant(&rp.layout, "code-reader", &git_reader, false).unwrap();
        let token = session_as(&rp, "Y29kZQ", "code-reader", Authority::Read, 1_000);
        assert_eq!(rp.lifetime(&git_reader), hours);
        let who = rp
            .identity(&token, 1_000 + SESSION_SECONDS + 3_600)
            .expect("a git reader's session outlives 12 h");
        assert_eq!(who.expires, 1_000 + 240 * 3_600);
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
            person: person_iri(&migrated_person_id("b2xk")),
            rp_id: RP_ID.to_string(),
            grants: vec!["brian".to_string()],
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
