//! Ledger [#1143](http://localhost:1060/l/default/item/1143): a REMOTE WORKER's grant, through
//! the QUIC door of a real gonk (the binary, in a child process, over a scratch config home,
//! data home and socket).
//!
//! A worker takes work from a ledger and hands it back; it never decides the work is done.
//! `ikigai-ledger` 0.7.0 split `claim`, `comment` and `close` out of the write grant, and
//! `ikigai-gonk client add <name> --worker <ledger>` (the same as `--ledger <ledger>=worker`)
//! mints exactly the published worker grant ([`grants_for`] at [`Authority::Worker`]). Both
//! workers here are enrolled by the binary's own `client add`, so the CLI form is what is under
//! test, not a hand-written grant.
//!
//! What it may do: read, claim, renew, comment (a hand-back included) and release. What it may
//! not: close, reopen, file, label, edit, or write the ledger's graph raw. And since 0.8.0 a claim
//! records the door's principal, so a second worker holding the same grant cannot release the
//! first one's claim, under any holder name.

use std::path::{Path, PathBuf};

mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic;
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

const LEDGER_GRAPH: &str = "urn:iki:ledger:graph:default";

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(answer: &Representation) -> String {
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// `ikigai-gonk <args> --config-home <config>`, with `HOME` in the scratch home: the binary's
/// own command line, as an operator types it.
fn gonk_cli(config: &Path, home: &Path, args: &[&str]) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(args)
        .arg("--config-home")
        .arg(config)
        .env("HOME", home)
        .output()
        .expect("run ikigai-gonk");
    assert!(
        out.status.success(),
        "ikigai-gonk {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A gonk whose QUIC door admits two workers on the default ledger, `w1` enrolled with
/// `--worker default` and `w2` with `--ledger default=worker`, both before it starts.
fn with_two_workers() -> Gonk {
    Gonk::with_quic_prepared("", &|config, home| {
        gonk_cli(
            config,
            home,
            &["client", "add", "w1", "--worker", "default"],
        );
        gonk_cli(
            config,
            home,
            &["client", "add", "w2", "--ledger", "default=worker"],
        );
    })
}

impl Gonk {
    fn bundle(&self, name: &str) -> PathBuf {
        self.layout.quic_dir().join("clients").join(name)
    }

    /// The principal the QUIC door mints for the client enrolled under grant `name`: its
    /// fingerprint, read back from `clients.json`, as `quic::client_iri` spells it.
    fn principal_of(&self, name: &str) -> String {
        let clients: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(self.layout.clients_json()).unwrap())
                .unwrap();
        let fingerprint = clients["clients"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, entry)| entry["grant"].as_str() == Some(name))
            .map(|(fp, _)| fp.clone())
            .unwrap_or_else(|| panic!("no client enrolled under `{name}`: {clients}"));
        quic::client_iri(&fingerprint)
    }

    fn as_worker(&self, name: &str, request: Request) -> Result<Representation, Error> {
        let client = self
            .dial_quic_as(&self.bundle(name))
            .unwrap_or_else(|e| panic!("QUIC connect as {name}: {e}"));
        client.issue(request).map(|(repr, _)| repr)
    }

    fn over_socket(&self, request: Request) -> Result<Representation, Error> {
        let client = ikigai_ipc::connect(&self.socket)
            .map_err(|e| Error::Endpoint(format!("connect: {e}")))?;
        client.issue(request).map(|(repr, _)| repr)
    }

    /// Item `n`'s JSON face, read over the owner's socket.
    fn item(&self, n: usize) -> serde_json::Value {
        let read = self
            .over_socket(request(
                Verb::Source,
                &format!("urn:iki:ledger:item:{n}"),
                &[("as", "application/json")],
            ))
            .unwrap_or_else(|e| panic!("item {n}: {e}"));
        let doc: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
        doc.get("item").cloned().unwrap_or(doc)
    }
}

fn denied(result: Result<Representation, Error>, what: &str) -> String {
    match result {
        Err(Error::Denied(why)) => why,
        other => panic!("{what}: expected Denied, got {other:?}"),
    }
}

/// ★ The CLI form mints exactly the ledger's published worker grant, under both spellings.
#[test]
fn client_add_worker_mints_the_published_worker_grant() {
    let gonk = with_two_workers();
    let grants: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(gonk.layout.grants_json()).unwrap()).unwrap();
    let worker = grants_for("default", Authority::Worker).unwrap();
    for name in ["w1", "w2"] {
        let held: Vec<String> = grants[name]
            .as_array()
            .unwrap_or_else(|| panic!("no grant `{name}` in {grants}"))
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        assert_eq!(held, worker, "{name}");
    }
    // The literal list the ledger's README publishes, so a change in either crate's spelling
    // fails here, where an operator's grants.json would have to change too.
    assert_eq!(
        worker,
        [
            "urn:cap:ledger:read:default",
            "urn:cap:store:read:graph:urn:iki:ledger:graph:default",
            "urn:cap:ledger:claim:default",
            "urn:cap:ledger:comment:default",
            "urn:cap:store:write:graph:urn:iki:ledger:graph:default",
        ]
    );
}

/// ★ A worker claims, renews, comments, hands back and releases; it cannot close, reopen,
/// file, label, edit, or write the ledger's graph raw; and another worker cannot release its
/// claim.
#[test]
fn a_worker_claims_renews_comments_and_releases_but_cannot_close() {
    let gonk = with_two_workers();
    gonk.over_socket(request(
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "Fix the widget")],
    ))
    .expect("append over the socket");

    // ── Read.
    let items = gonk
        .as_worker("w1", request(Verb::Source, "urn:iki:ledger:items", &[]))
        .expect("a worker reads the ledger");
    assert!(text(&items).contains("Fix the widget"), "{}", text(&items));

    // ── Claim, then renew.
    let claimed = gonk
        .as_worker(
            "w1",
            request(
                Verb::Sink,
                "urn:iki:ledger:claim",
                &[("item", "1"), ("content", "w1"), ("lease", "1h")],
            ),
        )
        .expect("a worker claims");
    assert!(
        text(&claimed).contains("claimed by w1"),
        "{}",
        text(&claimed)
    );
    gonk.as_worker(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:claim",
            &[
                ("item", "1"),
                ("content", "w1"),
                ("lease", "2h"),
                ("renew", "true"),
            ],
        ),
    )
    .expect("a worker renews its own claim");
    let claim = &gonk.item(1)["claim"];
    assert_eq!(claim["holder"].as_str(), Some("w1"), "{claim}");
    // ★ Taken AS the client the door authenticated, which is what makes it w1's own.
    assert_eq!(
        claim["principal"].as_str(),
        Some(gonk.principal_of("w1").as_str()),
        "{claim}"
    );
    assert_ne!(gonk.principal_of("w1"), gonk.principal_of("w2"));

    // ── Comment.
    gonk.as_worker(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[("item", "1"), ("content", "PR is up")],
        ),
    )
    .expect("a worker comments");

    // ── Not the verdict, and not the record: each a typed Denied.
    let why = denied(
        gonk.as_worker(
            "w1",
            request(Verb::Sink, "urn:iki:ledger:close", &[("item", "1")]),
        ),
        "close",
    );
    assert!(why.contains("urn:cap:ledger:close:default"), "{why}");
    for (what, verb, target, args) in [
        (
            "append",
            Verb::Sink,
            "urn:iki:ledger:append",
            vec![("content", "A new item")],
        ),
        (
            "label",
            Verb::Sink,
            "urn:iki:ledger:label",
            vec![("item", "1"), ("content", "urgent")],
        ),
        (
            "edit",
            Verb::Sink,
            "urn:iki:ledger:item:1",
            vec![("title", "Something else")],
        ),
        (
            "defer",
            Verb::Sink,
            "urn:iki:ledger:defer",
            vec![("item", "1"), ("content", "not now")],
        ),
        ("delete", Verb::Delete, "urn:iki:ledger:item:1", vec![]),
    ] {
        denied(gonk.as_worker("w1", request(verb, target, &args)), what);
    }
    // The store token a worker holds is raw quad authority on a bare kernel; gonk's door
    // refuses a raw write to a ledger graph without the `--ledger-graph` grant (ledger #878),
    // so one SPARQL update cannot close the item past the missing close grant.
    let item_iri = gonk.item(1)["iri"].as_str().unwrap().to_string();
    let raw = format!(
        "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <{item_iri}> <urn:x:forged> \"closed\" }} }}"
    );
    denied(
        gonk.as_worker(
            "w1",
            request(
                Verb::Sink,
                "urn:iki:store:graph-update",
                &[("graph", LEDGER_GRAPH), ("content", &raw)],
            ),
        ),
        "a raw write to the ledger graph",
    );

    // ── Another worker, holding the same grant, cannot release w1's claim by spelling its
    // holder name, nor without one, nor take it under that name (ikigai-ledger 0.8.0: the claim
    // records w1's principal, which only the QUIC door could have minted).
    denied(
        gonk.as_worker(
            "w2",
            request(
                Verb::Delete,
                "urn:iki:ledger:claim",
                &[("item", "1"), ("from", "w1")],
            ),
        ),
        "w2 releasing w1's claim by its holder name",
    );
    denied(
        gonk.as_worker(
            "w2",
            request(Verb::Delete, "urn:iki:ledger:claim", &[("item", "1")]),
        ),
        "w2 releasing w1's claim",
    );
    match gonk.as_worker(
        "w2",
        request(
            Verb::Sink,
            "urn:iki:ledger:claim",
            &[("item", "1"), ("content", "w1")],
        ),
    ) {
        Err(Error::Conflict(_)) => {}
        other => panic!("w2 claiming w1's item under w1's name: {other:?}"),
    }
    assert_eq!(gonk.item(1)["claim"]["holder"].as_str(), Some("w1"));

    // ── Hand back, then release: the item waits for its dispatcher, still open.
    gonk.as_worker(
        "w1",
        request(
            Verb::Sink,
            "urn:iki:ledger:comment",
            &[
                ("item", "1"),
                ("content", "done: PR 7"),
                ("handback", "true"),
            ],
        ),
    )
    .expect("a worker hands the item back");
    gonk.as_worker(
        "w1",
        request(
            Verb::Delete,
            "urn:iki:ledger:claim",
            &[("item", "1"), ("from", "w1")],
        ),
    )
    .expect("a worker releases its own claim");
    let item = gonk.item(1);
    assert!(item["claim"].is_null(), "released: {item}");
    assert_eq!(item["awaiting"].as_str(), Some("dispatcher"), "{item}");
    assert_eq!(item["status"].as_str(), Some("open"), "{item}");
    let rendered = item.to_string();
    for said in ["PR is up", "done: PR 7"] {
        assert!(rendered.contains(said), "{said}: {item}");
    }
}
