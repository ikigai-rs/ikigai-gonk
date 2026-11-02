//! Who may claim an author other than themselves (ledger #2836): this server's participants
//! graph, and `ikigai-gonk ledger enroll`, which writes it.
//!
//! `ikigai-ledger` stamps a write's principal as its committer and reads this graph, under the
//! caller's capability, to admit any other `author=`:
//!
//! - `<principal> prov:actedOnBehalfOf <author>`: a **delegate**. One QUIC principal may run
//!   several workers, so each machine's worker claims its own author on a hand-back.
//! - `<principal> a ledger:Importer`: an **import or sync principal**, which may name any foreign
//!   actor and claim authored dates.
//!
//! ★ **Enrollment is the owner's, over the socket, at root.** A network door refuses a raw write
//! to this graph ([`crate::admit::raw_write_refusal`]) and no grant carries its write token, so
//! nobody enrolls themselves. Every writing authority holds its READ token
//! ([`crate::grants::grants_for`]).

use std::path::{Path, PathBuf};

use ikigai_core::{ArgRef, Iri, Request, Verb};
use ikigai_ledger::vocabulary as v;

/// The participants graph, named to `ikigai_ledger::SpaceConfig::participants`.
pub const GRAPH: &str = "urn:collab:participants";

/// What an enrollment says about its principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// `--acts-for <iri>`: a delegate of that principal.
    ActsFor(String),
    /// `--importer`: an import or sync principal.
    Importer,
}

/// Everything `ikigai-gonk ledger enroll` or `unenroll` was told.
#[derive(Debug)]
pub struct EnrollArgs {
    /// `true` for `unenroll`.
    pub remove: bool,
    /// The principal's IRI, or the name of a client `client add` wrote a bundle for.
    pub principal: String,
    /// What it is enrolled as.
    pub role: Role,
    /// `--socket`: gonk's owner-only socket; unset, the one the config names.
    pub socket: Option<PathBuf>,
    /// `--config`, `--config-home` and `--data-home`, for that socket and the client's bundle.
    pub flags: crate::config::Flags,
}

/// Parse the arguments after `ledger enroll` or `ledger unenroll`.
pub fn parse_args(
    remove: bool,
    mut args: impl Iterator<Item = String>,
) -> Result<EnrollArgs, String> {
    let command = if remove { "unenroll" } else { "enroll" };
    let principal = args
        .next()
        .filter(|word| !word.starts_with('-'))
        .ok_or_else(|| format!("ledger {command}: expected <client name or principal IRI>"))?;
    let (mut acts_for, mut importer, mut socket) = (None, false, None);
    let mut flags = crate::config::Flags::default();
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("ledger {command}: {flag} needs a value"))
        };
        match flag.as_str() {
            "--acts-for" => acts_for = Some(value()?),
            "--importer" => importer = true,
            "--socket" => socket = Some(PathBuf::from(value()?)),
            "--config" => flags.config = Some(PathBuf::from(value()?)),
            "--config-home" => flags.config_home = Some(PathBuf::from(value()?)),
            "--data-home" => flags.data_home = Some(PathBuf::from(value()?)),
            other => return Err(format!("ledger {command}: unknown argument `{other}`")),
        }
    }
    let role = match (acts_for, importer) {
        (Some(author), false) => Role::ActsFor(author),
        (None, true) => Role::Importer,
        _ => {
            return Err(format!(
                "ledger {command}: expected exactly one of --acts-for <iri> and --importer"
            ))
        }
    };
    Ok(EnrollArgs {
        remove,
        principal,
        role,
        socket,
        flags,
    })
}

/// The principal an enrollment names: an IRI as written, or the IRI of the client `name`, read
/// from the bundle `ikigai-gonk client add` wrote.
pub fn principal_iri(word: &str, config_home: &Path) -> Result<String, String> {
    if word.contains(':') {
        return Ok(word.to_string());
    }
    let layout = crate::quic::Layout::in_config_home(config_home);
    crate::quic::client_iri_of(&layout, word)
}

/// The triple an enrollment says, as a statement inside `GRAPH <…> { … }`.
///
/// ```
/// use ikigai_gonk::participants::{statement, Role};
/// assert_eq!(
///     statement("urn:iki:host:gonk:client:ab12", &Role::ActsFor("urn:collab:participant:mini".to_string())).unwrap(),
///     "<urn:iki:host:gonk:client:ab12> <http://www.w3.org/ns/prov#actedOnBehalfOf> <urn:collab:participant:mini> ."
/// );
/// assert!(statement("urn:x> <y", &Role::Importer).is_err());
/// assert!(statement("urn:x", &Role::ActsFor("mini".to_string())).is_err());
/// ```
pub fn statement(principal: &str, role: &Role) -> Result<String, String> {
    // Spliced into an update, so held to the IRI grammar first.
    let iri = |value: &str, what: &str| {
        let spliceable = !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\"{}|\\^`".contains(c));
        match Iri::parse(value) {
            Ok(_) if spliceable && value.contains(':') => Ok(format!("<{value}>")),
            _ => Err(format!("{what}: `{value}` is not an IRI")),
        }
    };
    let subject = iri(principal, "principal")?;
    Ok(match role {
        Role::ActsFor(author) => format!(
            "{subject} <{}> {} .",
            v::ext::ACTED_ON_BEHALF_OF,
            iri(author, "--acts-for")?
        ),
        Role::Importer => format!("{subject} <{}> <{}> .", v::ext::TYPE, v::IMPORTER_CLASS),
    })
}

/// Enroll `principal` as `role`, or remove that enrollment, over the owner's socket.
pub fn run(
    issue: &impl Fn(Request) -> Result<Vec<u8>, String>,
    remove: bool,
    principal: &str,
    role: &Role,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    let triple = statement(principal, role)?;
    let verb = if remove { "DELETE" } else { "INSERT" };
    let update = format!("{verb} DATA {{ GRAPH <{GRAPH}> {{ {triple} }} }}");
    issue(
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:store:graph-update").expect("a constant IRI"),
        )
        .with_arg("content", ArgRef::Inline(update.into_bytes()))
        .with_arg("graph", ArgRef::Inline(GRAPH.as_bytes().to_vec())),
    )
    .map_err(|e| format!("writing <{GRAPH}>: {e}"))?;
    let done = if remove { "unenrolled" } else { "enrolled" };
    writeln!(out, "{done} in <{GRAPH}>: {triple}").map_err(|e| e.to_string())
}
