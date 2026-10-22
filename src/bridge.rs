//! How the bridges reach a running gonk, with an identity (ledger
//! [#2075](http://localhost:1062/l/default/item/2075)).
//!
//! `roborev file` ([`crate::roborev`]) and `kata import` ([`crate::kata`]) used to file over the
//! HTTP door, as an anonymous loopback caller. Since ledger #1687 that caller only READS, which
//! is right, and both bridges filed nothing (403). So they file with an identity now, through
//! one of two doors:
//!
//! - **QUIC, as an enrolled client** (`--quic HOST:PORT --cert-dir DIR`): the certificate
//!   directory `ikigai-gonk client add <name> --ledger <ledger>=write` writes (`client.crt`,
//!   `client.key`, the `server.crt` to pin), dialed by the same code a mount dials with
//!   (`crate::mount::connect_quic`). The grant is narrow (one ledger, write), revocable, and
//!   works from any user, sandbox or machine. Every write is authored by the CLIENT: the
//!   bridge names no `author`, and the door fills the client's own name
//!   (`urn:iki:host:gonk:client:<fingerprint>`); who wrote the finding or the kata issue stays
//!   in the text.
//! - **The owner-only socket** (`--socket PATH`, or the `gonk.socket` of `--config`; default
//!   `~/.ikigai/gonk.sock`): the same-user convenience, for a bridge running as gonk's user on
//!   gonk's machine, outside any sandbox. It carries the owner's whole authority — every
//!   ledger — which is why QUIC is the door for a daemon. `findings file`
//!   ([`crate::findings`]), `ledger backfill-keys` ([`crate::keys`]) and `layer take` file
//!   this way.
//!
//! ★ **One rule when both are configured: `--quic` wins.** The socket is not dialed, whatever
//! `--socket` or `--config` say.
//!
//! The HTTP door's anonymous caller stays read-only. Every answer a bridge reads comes through
//! the ledger's JSON face (`as=application/json`, schema 1, [`ikigai_ledger::json`]), never the
//! plain text, which is for people.

use std::path::PathBuf;

use ikigai_core::{ArgRef, Error, Iri, Request, Verb};
use ikigai_ledger::json::{MEDIA_TYPE as JSON, SCHEMA};
use ikigai_resolve::Resolver;
use serde::de::DeserializeOwned;

/// Which door a bridge files through: the QUIC door as an enrolled client, or the owner's
/// socket. `--quic` wins when both are configured (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// gonk's owner-only socket.
    Socket(PathBuf),
    /// gonk's QUIC door at `authority` (`host:port`), as the client whose certificate
    /// directory is `certs`.
    Quic {
        /// `host:port`, resolved when dialed.
        authority: String,
        /// `client.crt`, `client.key` and `server.crt`, as `ikigai-gonk client add` writes them.
        certs: PathBuf,
    },
}

impl std::fmt::Display for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Door::Socket(path) => write!(f, "its socket at {}", path.display()),
            Door::Quic { authority, certs } => {
                write!(f, "quic://{authority} as the client in {}", certs.display())
            }
        }
    }
}

/// `--quic` and `--cert-dir`, as a bridge's flags were given them: both or neither.
pub(crate) fn quic_door(
    command: &str,
    quic: Option<String>,
    certs: Option<PathBuf>,
) -> Result<Option<Door>, String> {
    match (quic, certs) {
        (Some(authority), Some(certs)) => {
            if authority.is_empty() || authority.contains('/') || !authority.contains(':') {
                return Err(format!(
                    "{command}: --quic `{authority}` is not HOST:PORT (gonk's QUIC door, say \
                     127.0.0.1:1060)"
                ));
            }
            Ok(Some(Door::Quic { authority, certs }))
        }
        (None, None) => Ok(None),
        (Some(_), None) => Err(format!(
            "{command}: --quic needs --cert-dir, the directory `ikigai-gonk client add <name> \
             --ledger <ledger>=write` wrote (client.crt, client.key, server.crt)"
        )),
        (None, Some(_)) => Err(format!(
            "{command}: --cert-dir needs --quic HOST:PORT, the QUIC door to present it at"
        )),
    }
}

enum Client {
    Socket(ikigai_ipc::IpcResolver),
    Quic(ikigai_quic::QuicResolver),
}

/// A running gonk, connected through a [`Door`].
pub(crate) struct Gonk {
    client: Client,
    door: Door,
}

impl Gonk {
    /// Connect through `door`.
    pub(crate) fn connect(door: &Door) -> Result<Gonk, String> {
        let client = match door {
            Door::Socket(path) => ikigai_ipc::connect(path).map(Client::Socket).map_err(|e| {
                format!(
                    "cannot reach gonk's socket at {}: {e} (is gonk running? `--socket` names \
                     another, or `--config` the config.toml that does; `--quic` files as an \
                     enrolled client instead)",
                    path.display()
                )
            }),
            Door::Quic { authority, certs } => crate::mount::connect_quic(authority, certs)
                .map(Client::Quic)
                .map_err(|e| {
                    format!(
                        "cannot reach gonk's QUIC door as the client in {}: {e} (is gonk \
                         running, and was this client enrolled with `ikigai-gonk client add \
                         <name> --ledger <ledger>=write` before gonk started?)",
                        certs.display()
                    )
                }),
        }?;
        Ok(Gonk {
            client,
            door: door.clone(),
        })
    }

    /// One request, in the ledger's JSON face when `json`; the representation's bytes, or the
    /// kernel's error.
    fn issue(
        &self,
        verb: Verb,
        iri: &str,
        args: &[(&str, String)],
        content: Option<&str>,
        json: bool,
    ) -> Result<Vec<u8>, Error> {
        let target = Iri::parse(iri).map_err(|e| Error::Endpoint(format!("`{iri}`: {e}")))?;
        let quic = matches!(self.client, Client::Quic(_));
        let mut request = args
            .iter()
            // ★ Over QUIC the door names the author: the client itself. A bridge's `author`
            // (roborev, kata's) is for the owner's socket only.
            .filter(|(name, _)| !(quic && *name == "author"))
            .fold(Request::new(verb, target), |request, (name, value)| {
                request.with_arg(*name, inline(value))
            });
        if let Some(content) = content {
            request = request.with_arg("content", inline(content));
        }
        if json {
            request = request.with_arg("as", inline(JSON));
        }
        let answer = match &self.client {
            Client::Socket(client) => client.issue(request),
            Client::Quic(client) => client.issue(request),
        };
        answer.map(|(repr, _)| repr.bytes)
    }

    /// A write (`Sink`) whose answer is not read: a comment, a close, a link.
    pub(crate) fn sink(
        &self,
        iri: &str,
        args: &[(&str, String)],
        content: &str,
    ) -> Result<(), String> {
        self.issue(Verb::Sink, iri, args, Some(content), false)
            .map(|_| ())
            .map_err(|e| self.failed("Sink", iri, &e))
    }

    /// A write (`Sink`) in the ledger's JSON face, read into its type.
    pub(crate) fn sink_json<T: DeserializeOwned>(
        &self,
        iri: &str,
        args: &[(&str, String)],
        content: &str,
    ) -> Result<T, String> {
        let bytes = self
            .issue(Verb::Sink, iri, args, Some(content), true)
            .map_err(|e| self.failed("Sink", iri, &e))?;
        read_json("Sink", iri, &bytes)
    }

    /// A read (`Source`) in the ledger's JSON face, read into its type.
    pub(crate) fn source_json<T: DeserializeOwned>(&self, iri: &str) -> Result<T, String> {
        self.lookup(iri)?
            .ok_or_else(|| format!("gonk answered Source {iri}: there is nothing at that name"))
    }

    /// [`Gonk::source_json`], with "not found" answered as `None`: how a read asks "is
    /// there one?".
    pub(crate) fn lookup<T: DeserializeOwned>(&self, iri: &str) -> Result<Option<T>, String> {
        match self.issue(Verb::Source, iri, &[], None, true) {
            Ok(bytes) => read_json("Source", iri, &bytes).map(Some),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(self.failed("Source", iri, &e)),
        }
    }

    fn failed(&self, verb: &str, iri: &str, error: &Error) -> String {
        let hint = match (&self.client, error) {
            // The QUIC door refuses a certificate it does not trust, or one enrolled under no
            // grant, by closing the connection: all the client sees is a transport error.
            (Client::Quic(_), Error::Unavailable(_)) => {
                " (the QUIC door refuses a certificate it does not trust, and one enrolled \
                 under no grant, by closing the connection: enroll this client with \
                 `ikigai-gonk client add <name> --ledger <ledger>=write` and restart gonk)"
            }
            _ => "",
        };
        format!(
            "gonk refused {verb} {iri} over {}: {error}{hint}",
            self.door
        )
    }
}

/// The ledger's JSON answer, read into its type — the machine contract, checked for its schema.
fn read_json<T: DeserializeOwned>(verb: &str, iri: &str, bytes: &[u8]) -> Result<T, String> {
    let text = String::from_utf8_lossy(bytes);
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        format!("gonk answered {verb} {iri} with something that is not JSON ({e}): {text}")
    })?;
    let schema = value.get("schema").and_then(serde_json::Value::as_u64);
    if schema != Some(u64::from(SCHEMA)) {
        return Err(format!(
            "gonk answered {verb} {iri} in the ledger's JSON face schema {}, and this build \
             reads schema {SCHEMA}: rebuild ikigai-gonk against the ledger that server runs",
            schema.map_or_else(|| "(none)".to_string(), |s| s.to_string())
        ));
    }
    serde_json::from_value(value).map_err(|e| {
        format!(
            "gonk's answer to {verb} {iri} is not the shape schema {SCHEMA} promises ({e}): {text}"
        )
    })
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

/// The refusal for `--gonk`, which named the HTTP door: it files nothing since ledger #1687.
pub(crate) fn no_http_door(command: &str) -> String {
    format!(
        "{command}: --gonk is gone. It named gonk's HTTP door, whose anonymous caller only reads \
         since ledger #1687, so filing through it answered 403. This command files with an \
         identity now: drop --gonk, and pass --quic HOST:PORT --cert-dir DIR to file as a client \
         enrolled with `ikigai-gonk client add <name> --ledger <ledger>=write`, or file as gonk's \
         owner over its socket (--socket PATH, or --config the config.toml naming \
         `gonk.socket`, when it is not ~/.ikigai/gonk.sock)"
    )
}
