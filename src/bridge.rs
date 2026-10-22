//! How the bridges reach a running gonk: its owner-only socket (ledger
//! [#2075](http://localhost:1062/l/default/item/2075)).
//!
//! `roborev file` ([`crate::roborev`]) and `kata import` ([`crate::kata`]) used to file over the
//! HTTP door, as an anonymous loopback caller. Since ledger #1687 that caller only READS, which
//! is right, and both bridges filed nothing (403). So they file with an identity now, and the
//! one that fits how they run is the socket's: a roborev hook and a `kata import` both run as
//! the user who runs gonk, on the machine gonk runs on (the HTTP door they used answered only
//! loopback callers anyway), and that user already holds the socket — `0600`, peer UID checked,
//! capability root (`crate::doors`). Nothing has to be enrolled, minted or signed in first, the
//! socket's path is read from the same `config.toml` the server reads, and the HTTP door's
//! anonymous caller stays read-only. `findings file` ([`crate::findings`]), `ledger
//! backfill-keys` ([`crate::keys`]) and `layer take` file the same way.
//!
//! ⚠ A QUIC client certificate holding the ledger's write grant was the other identity the
//! item named, and it is the one for a bridge on ANOTHER machine; neither bridge runs there
//! today, so it is not built.
//!
//! Every answer a bridge reads comes through the ledger's JSON face (`as=application/json`,
//! schema 1, [`ikigai_ledger::json`]), never the plain text, which is for people.

use std::path::{Path, PathBuf};

use ikigai_core::{ArgRef, Error, Iri, Request, Verb};
use ikigai_ledger::json::{MEDIA_TYPE as JSON, SCHEMA};
use ikigai_resolve::Resolver;
use serde::de::DeserializeOwned;

/// gonk's owner-only socket, connected.
pub(crate) struct Socket {
    client: ikigai_ipc::IpcResolver,
    path: PathBuf,
}

impl Socket {
    /// Connect to the socket at `path`.
    pub(crate) fn connect(path: &Path) -> Result<Socket, String> {
        let client = ikigai_ipc::connect(path).map_err(|e| {
            format!(
                "cannot reach gonk's socket at {}: {e} (is gonk running? `--socket` names \
                 another, or `--config` the config.toml that does)",
                path.display()
            )
        })?;
        Ok(Socket {
            client,
            path: path.to_path_buf(),
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
        let mut request = args
            .iter()
            .fold(Request::new(verb, target), |request, (name, value)| {
                request.with_arg(*name, inline(value))
            });
        if let Some(content) = content {
            request = request.with_arg("content", inline(content));
        }
        if json {
            request = request.with_arg("as", inline(JSON));
        }
        self.client.issue(request).map(|(repr, _)| repr.bytes)
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

    /// [`Socket::source_json`], with "not found" answered as `None`: how a read asks "is
    /// there one?".
    pub(crate) fn lookup<T: DeserializeOwned>(&self, iri: &str) -> Result<Option<T>, String> {
        match self.issue(Verb::Source, iri, &[], None, true) {
            Ok(bytes) => read_json("Source", iri, &bytes).map(Some),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(self.failed("Source", iri, &e)),
        }
    }

    fn failed(&self, verb: &str, iri: &str, error: &Error) -> String {
        format!(
            "gonk refused {verb} {iri} over its socket at {}: {error}",
            self.path.display()
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
         since ledger #1687, so filing through it answered 403. This command files over the \
         running gonk's owner-only socket now: drop --gonk, and pass --socket PATH (or --config \
         the config.toml naming `gonk.socket`) when the socket is not ~/.ikigai/gonk.sock"
    )
}
