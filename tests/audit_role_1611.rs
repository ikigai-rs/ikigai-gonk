//! Ledger [#1611](http://localhost:1060/l/default/item/1611): the AUDIT role, through every path
//! that mints a grant and through the QUIC door of a real gonk (the binary, in a child process,
//! over a scratch config home, data home and socket).
//!
//! Before it, a ledger's graveyard (`urn:iki:ledger:graph:{name}:deleted`) was readable only
//! with `purge` (or root), so whoever could see what had been deleted could also destroy it.
//! `--ledger <ledger>=audit` is `read` plus the graveyard's READ token and nothing that writes.
//!
//! Every test here fails without the change: `audit` was not an authority, so `client add`,
//! `passkey invite` and `grants` refused it before minting anything. The one that would hold
//! without it, a `read` holder refused the graveyard, is here as the boundary the role must not
//! move.

use std::path::{Path, PathBuf};
use std::process::Output;

mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

const LEDGER_GRAPH: &str = "urn:iki:ledger:graph:default";
const GRAVEYARD: &str = "urn:iki:ledger:graph:default:deleted";

/// The audit grant on `default`, as literals: these go into an operator's `grants.json`.
const AUDIT: [&str; 3] = [
    "urn:cap:ledger:read:default",
    "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
    "urn:cap:store:read:graph:urn:iki:ledger:graph:default:deleted",
];

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(answer: &Representation) -> String {
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// `ikigai-gonk <args> --config-home <config>`, with `HOME` in the scratch home.
fn gonk_cli(config: &Path, home: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .arg("--config-home")
        .arg(config)
        .env("HOME", home)
        .output()
        .expect("run ikigai-gonk")
}

/// `ikigai-gonk <args>` with no config home, for the commands that read none.
fn bare_cli(home: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .expect("run ikigai-gonk")
}

fn succeeded(out: Output, what: &str) -> String {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The scopes `grants.json` holds under `name`.
fn grant(file: &Path, name: &str) -> Vec<String> {
    let grants: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    grants[name]
        .as_array()
        .unwrap_or_else(|| panic!("no grant `{name}` in {grants}"))
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

/// A gonk whose QUIC door admits `auditor` (`--ledger default=audit`) and `reader`
/// (`--ledger default=read`), both enrolled by the binary's own `client add`.
fn with_auditor_and_reader() -> Gonk {
    Gonk::with_quic_prepared("", &|config, home| {
        succeeded(
            gonk_cli(
                config,
                home,
                &["client", "add", "auditor", "--ledger", "default=audit"],
            ),
            "client add auditor",
        );
        succeeded(
            gonk_cli(
                config,
                home,
                &["client", "add", "reader", "--ledger", "default=read"],
            ),
            "client add reader",
        );
    })
}

impl Gonk {
    fn as_client(&self, name: &str, request: Request) -> Result<Representation, Error> {
        let bundle: PathBuf = self.layout.quic_dir().join("clients").join(name);
        let client = self
            .dial_quic_as(&bundle)
            .unwrap_or_else(|e| panic!("QUIC connect as {name}: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }
}

fn denied(result: Result<Representation, Error>, what: &str) -> String {
    match result {
        Err(Error::Denied(why)) => why,
        other => panic!("{what}: expected Denied, got {other:?}"),
    }
}

/// `urn:sparql:select` over one named graph, as CSV.
fn select(graph: &str, query: &str) -> Request {
    request(
        Verb::Source,
        "urn:sparql:select",
        &[("query", query), ("graph", graph), ("as", "text/csv")],
    )
}

/// ★ Every path that mints a grant mints the same three tokens: `client add`, `passkey invite`
/// and `grants`. And `grant_refusal` (the check every grant passes, at startup and per
/// connection) admits them, and still refuses what it refused before.
#[test]
fn every_entry_path_mints_the_audit_grant_and_nothing_more() {
    assert_eq!(grants_for("default", Authority::Audit).unwrap(), AUDIT);

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let config = tmp.path().join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let grants_json = quic::Layout::in_config_home(&config).grants_json();

    // `grants <ledger> audit`: what an operator copies into grants.json by hand. It reads no
    // config home, so it takes no `--config-home`.
    let printed = succeeded(
        bare_cli(&home, &["grants", "default", "audit"]),
        "grants default audit",
    );
    let printed: Vec<String> = serde_json::from_str(&printed).unwrap();
    assert_eq!(printed, AUDIT);

    // `client add` and `passkey invite` (no server: the invite writes the grant and the code).
    succeeded(
        gonk_cli(
            &config,
            &home,
            &["client", "add", "box", "--ledger", "default=audit"],
        ),
        "client add --ledger default=audit",
    );
    assert_eq!(grant(&grants_json, "box"), AUDIT);
    succeeded(
        gonk_cli(
            &config,
            &home,
            &["passkey", "invite", "eve", "--ledger", "default=audit"],
        ),
        "passkey invite --ledger default=audit",
    );
    assert_eq!(grant(&grants_json, "eve"), AUDIT);

    // A hand-written grants.json with the same list is admitted; the refusals stand.
    let audit: Vec<String> = AUDIT.iter().map(|t| t.to_string()).collect();
    assert_eq!(quic::grant_refusal("eve", &audit), None);
    for refused in [
        ikigai_store::CAP_READ,
        ikigai_store::CAP_WRITE,
        "urn:cap:exec:*",
    ] {
        let mut scopes = audit.clone();
        scopes.push(refused.to_string());
        assert!(
            quic::grant_refusal("eve", &scopes).is_some(),
            "{refused} must still be refused beside the audit grant"
        );
    }

    // The browse graph has no graveyard: `audit` there is refused, not read as `read`.
    let out = bare_cli(&home, &["grants", "--browse-graph", "audit"]);
    assert!(!out.status.success(), "--browse-graph audit was accepted");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no graveyard"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// ★ An auditor reads a deleted item: its archived content in the graveyard and its tombstone.
/// It cannot purge, delete, restore, or write anything. A `read` holder still cannot read the
/// graveyard.
#[test]
fn an_auditor_reads_the_graveyard_and_writes_nothing() {
    let gonk = with_auditor_and_reader();
    for title in ["Keep this one", "A secret pasted by mistake"] {
        gonk.over_socket(request(
            Verb::Sink,
            "urn:iki:ledger:append",
            &[("content", title)],
        ))
        .expect("append over the socket");
    }
    gonk.over_socket(request(
        Verb::Delete,
        "urn:iki:ledger:item:2",
        &[("reason", "wrong place")],
    ))
    .expect("the owner deletes item 2");

    // ── The tombstone: the ledger names it, and its record (reason, count, hash) reads back.
    let gone = gonk.as_client(
        "auditor",
        request(Verb::Source, "urn:iki:ledger:item:2", &[]),
    );
    let why = match gone {
        Err(Error::NotFound(why)) => why,
        other => panic!("a deleted item: expected NotFound, got {other:?}"),
    };
    let prefix = "urn:iki:ledger:default:tombstone:";
    let at = why
        .find(prefix)
        .unwrap_or_else(|| panic!("no tombstone named: {why}"));
    let tombstone: String = why[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == ':')
        .collect();
    let record = gonk
        .as_client(
            "auditor",
            select(
                LEDGER_GRAPH,
                &format!("SELECT ?p ?o WHERE {{ <{tombstone}> ?p ?o }}"),
            ),
        )
        .expect("an auditor reads the tombstone");
    assert!(text(&record).contains("wrong place"), "{}", text(&record));

    // ── The graveyard: the deleted item's archived content.
    let archived = gonk
        .as_client(
            "auditor",
            select(GRAVEYARD, "SELECT ?s ?o WHERE { ?s ?p ?o }"),
        )
        .expect("an auditor reads the graveyard");
    assert!(
        text(&archived).contains("A secret pasted by mistake"),
        "{}",
        text(&archived)
    );
    assert!(
        !text(&archived).contains("Keep this one"),
        "{}",
        text(&archived)
    );

    // ── Nothing that writes: purge, delete, file, comment, and a restore or a clear by hand.
    let why = denied(
        gonk.as_client(
            "auditor",
            request(Verb::Delete, "urn:iki:ledger:purge", &[("content", "#2")]),
        ),
        "purge",
    );
    assert!(why.contains("purge"), "{why}");
    for (what, verb, target, args) in [
        ("delete", Verb::Delete, "urn:iki:ledger:item:1", vec![]),
        (
            "append",
            Verb::Sink,
            "urn:iki:ledger:append",
            vec![("content", "A new item")],
        ),
        (
            "comment",
            Verb::Sink,
            "urn:iki:ledger:comment",
            vec![("item", "1"), ("content", "seen")],
        ),
        (
            "close",
            Verb::Sink,
            "urn:iki:ledger:close",
            vec![("item", "1")],
        ),
    ] {
        denied(
            gonk.as_client("auditor", request(verb, target, &args)),
            what,
        );
    }
    let restore = format!(
        "INSERT {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }} WHERE {{ GRAPH <{GRAVEYARD}> {{ ?s ?p ?o }} }}"
    );
    denied(
        gonk.as_client(
            "auditor",
            request(
                Verb::Sink,
                "urn:iki:store:graph-update",
                &[("graph", LEDGER_GRAPH), ("content", &restore)],
            ),
        ),
        "a restore by hand into the ledger graph",
    );
    denied(
        gonk.as_client(
            "auditor",
            request(
                Verb::Sink,
                "urn:iki:store:graph-update",
                &[
                    ("graph", GRAVEYARD),
                    ("content", &format!("CLEAR GRAPH <{GRAVEYARD}>")),
                ],
            ),
        ),
        "clearing the graveyard by hand",
    );
    // Nothing moved: the graveyard still holds the item, and item 1 is still live.
    let after = gonk
        .over_socket(select(GRAVEYARD, "SELECT ?o WHERE { ?s ?p ?o }"))
        .expect("root reads the graveyard");
    assert!(text(&after).contains("A secret pasted by mistake"));
    gonk.over_socket(request(Verb::Source, "urn:iki:ledger:item:1", &[]))
        .expect("item 1 is still live");

    // ── The boundary the role must not move: `read` is still refused the graveyard.
    let why = denied(
        gonk.as_client("reader", select(GRAVEYARD, "SELECT ?s WHERE { ?s ?p ?o }")),
        "a read holder reading the graveyard",
    );
    assert!(why.contains(AUDIT[2]), "{why}");
}
