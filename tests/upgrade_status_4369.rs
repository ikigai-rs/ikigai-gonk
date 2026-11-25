//! What an upgraded install still needs, said at startup and served as
//! `urn:iki:gonk:upgrade:status` (ledger #4369, `ikigai_gonk::upgrade`).
//!
//! Each claim is reproduced on a fixture written in the shape an older release left it:
//!
//! - [`an_old_worker_grant_lacks_the_session_tokens`]: a worker grant as `ikigai-ledger` 0.21
//!   minted it, with no session graph, is refused a session-summary write, and the plan names
//!   it with `client refresh --write`.
//! - [`a_script_published_before_the_exec_split_is_named`]: a head granted
//!   `urn:cap:exec:git` is named, with the republish.
//! - [`an_old_form_finding_is_missing_from_the_file_list_and_named`]: a finding about
//!   `urn:repo:{root}:file:{path}` is not found on the canonical IRI the file page joins on, and
//!   the plan names its ledger with `canonical-file-iris --apply`.
//! - [`a_current_install_says_nothing`]: the same three, in the current shape, print no line.
//! - [`the_status_is_an_operator_read`]: the resource answers the same lines to a holder of
//!   the System read, and refuses a caller without it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::quic::Layout;
use ikigai_gonk::upgrade::{self, Commands, Status, Upgrade};
use ikigai_script::{Backend, Head, MemoryBackend, State};
use ikigai_store::DurableStore;

const OLD_FILE: &str = "urn:repo:demo:file:src/retry.rs";
const NEW_FILE: &str = "urn:repo:file:demo/src/retry.rs";
const KEY: &str = "urn:roborev:finding:00112233445566778899aabbccddeeff";

/// A scratch install: its config home (for `grants.json`), its script atoms, its hub.
struct Install {
    home: tempfile::TempDir,
    scripts: Arc<MemoryBackend>,
    hub: Arc<Kernel>,
    upgrade: Arc<Upgrade>,
}

impl Install {
    fn new() -> Install {
        let home = tempfile::tempdir().unwrap();
        let scripts = Arc::new(MemoryBackend::new());
        let upgrade = Upgrade::new(
            Layout::in_config_home(home.path()),
            Arc::clone(&scripts) as Arc<dyn Backend>,
            commands(home.path().to_path_buf()),
        );
        let hub = Arc::new(ikigai_gonk::compose_with(
            DurableStore::in_memory().expect("an in-memory store"),
            None,
            Vec::new(),
            vec![Arc::new(upgrade.space())],
            None,
            None,
            None,
        ));
        upgrade.attach(&hub);
        Install {
            home,
            scripts,
            hub,
            upgrade,
        }
    }

    fn grants(&self, grants: serde_json::Value) {
        let file = Layout::in_config_home(self.home.path()).grants_json();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, serde_json::to_string_pretty(&grants).unwrap()).unwrap();
    }

    fn script(&self, name: &str, granted: &[&str]) {
        let head = Head {
            schema: ikigai_script::model::SCHEMA,
            name: name.to_string(),
            version: "0".repeat(64),
            state: State::Published,
            public: false,
            granted: granted
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>(),
            exclusions: BTreeSet::new(),
            granted_budget: None,
            publisher: "urn:iki:principal:owner".to_string(),
            updated: None,
            history: Vec::new(),
        };
        self.scripts.swap_head(name, None, &head).unwrap();
    }

    fn finding(&self, file: &str) {
        let request = Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:reviews:append").unwrap(),
        )
        .with_arg("content", inline("A finding about a file"))
        .with_arg("key", inline(KEY))
        .with_arg("about", inline(&format!("{file} {KEY}")))
        .with_arg("author", inline("roborev"));
        block_on(Kernel::issue(&self.hub, request, &Capability::root())).unwrap();
    }

    fn check(&self) -> Status {
        block_on(self.upgrade.check())
    }

    fn lines(&self) -> Vec<String> {
        let lines = self.check().lines(self.upgrade.commands());
        println!("{}", lines.join("\n"));
        lines
    }
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

fn commands(config_home: PathBuf) -> Commands {
    Commands {
        config_home,
        socket: PathBuf::from("/run/gonk.sock"),
    }
}

/// A worker grant on `ledger` as `ikigai-ledger` 0.21 minted it: no session graph at all.
fn old_worker(ledger: &str) -> Vec<String> {
    grants_for(ledger, Authority::Worker)
        .unwrap()
        .into_iter()
        .filter(|t| !t.contains(":session"))
        .collect()
}

#[test]
fn an_old_worker_grant_lacks_the_session_tokens() {
    // The defect: the old grant does not hold the session graph's write, which a worker's
    // session-summary write needs, and nothing at startup said so.
    let session_write = ikigai_store::cap_write_graph(
        &ikigai_ledger::Ledger::parse("acme")
            .unwrap()
            .session_graph(),
    );
    let old = old_worker("acme");
    assert!(!Capability::scoped(old.iter().map(String::as_str)).allows(&session_write));
    assert!(grants_for("acme", Authority::Worker)
        .unwrap()
        .contains(&session_write));

    let install = Install::new();
    install.grants(serde_json::json!({ "worker-1": old }));
    let status = install.check();
    assert_eq!(status.grants.len(), 1, "{status:?}");
    assert_eq!(status.grants[0].grant, "worker-1");
    assert!(status.grants[0].adds.contains(&session_write), "{status:?}");
    let lines = install.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("`worker-1`"), "{lines:?}");
    assert!(
        lines[0].contains(&format!(
            "`ikigai-gonk client refresh --write --config-home {}`",
            install.home.path().display()
        )),
        "{lines:?}"
    );
    // Read-only: the file is as it was.
    let after =
        ikigai_gonk::quic::read_grants(&Layout::in_config_home(install.home.path()).grants_json())
            .unwrap();
    assert_eq!(after["worker-1"], old_worker("acme"));
}

#[test]
fn a_script_published_before_the_exec_split_is_named() {
    let install = Install::new();
    install.script(
        "pr-digest",
        &["urn:cap:exec:gh", "urn:cap:ledger:read:acme"],
    );
    install.script("log-scan", &["urn:cap:exec:git"]);
    install.script("fresh", &["urn:cap:exec:read:git"]);
    let status = install.check();
    let named: Vec<&str> = status.scripts.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(named, ["log-scan", "pr-digest"], "{status:?}");
    let lines = install.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("2 published script(s)"), "{lines:?}");
    assert!(lines[0].contains("`log-scan`, `pr-digest`"), "{lines:?}");
    assert!(
        lines[0].contains("urn:cap:exec:read:gh and urn:cap:exec:read:git"),
        "{lines:?}"
    );
    assert!(lines[0].contains("sink urn:script:"), "{lines:?}");
}

#[test]
fn an_old_form_finding_is_missing_from_the_file_list_and_named() {
    let install = Install::new();
    install.finding(OLD_FILE);
    // The defect: the canonical IRI the file page joins on does not find it.
    let listing = |about: &str| {
        let request = Request::new(
            Verb::Source,
            Iri::parse("urn:iki:ledger:reviews:items").unwrap(),
        )
        .with_arg("about", inline(about));
        let answer = block_on(Kernel::issue(&install.hub, request, &Capability::root())).unwrap();
        String::from_utf8_lossy(&answer.bytes).into_owned()
    };
    assert!(!listing(NEW_FILE).contains("reviews#1"));
    assert!(listing(OLD_FILE).contains("reviews#1"));

    let status = install.check();
    assert_eq!(status.file_iris.len(), 1, "{status:?}");
    assert_eq!(status.file_iris[0].ledger, "reviews");
    assert_eq!(status.file_iris[0].rewrites, 1);
    let lines = install.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(
            "`ikigai-gonk ledger canonical-file-iris --ledger reviews --socket /run/gonk.sock --apply`"
        ),
        "{lines:?}"
    );
    // Read-only: still in the old form.
    assert!(listing(OLD_FILE).contains("reviews#1"));
}

#[test]
fn a_current_install_says_nothing() {
    let install = Install::new();
    // No grants file, no scripts, no ledger: a fresh install.
    assert_eq!(install.check(), Status::default());
    assert!(install.lines().is_empty());

    install.grants(serde_json::json!({
        "worker-1": grants_for("acme", Authority::Worker).unwrap(),
        "writer": grants_for("acme", Authority::Write).unwrap(),
    }));
    install.script(
        "log-scan",
        &["urn:cap:exec:read:git", "urn:cap:exec:read:gh"],
    );
    install.finding(NEW_FILE);
    let status = install.check();
    assert!(status.is_empty(), "{status:?}");
    assert!(install.lines().is_empty());
}

#[test]
fn the_status_is_an_operator_read() {
    let install = Install::new();
    install.grants(serde_json::json!({ "worker-1": old_worker("acme") }));
    let read = |capability: &Capability, json: bool| {
        let mut request = Request::new(Verb::Source, Iri::parse(upgrade::STATUS).unwrap());
        if json {
            request = request.with_arg("as", inline("application/json"));
        }
        block_on(Kernel::issue(&install.hub, request, capability))
            .map(|answer| String::from_utf8_lossy(&answer.bytes).into_owned())
    };
    let operator = Capability::scoped([ikigai_gonk::system::CAP_READ]);
    let text = read(&operator, false).unwrap();
    assert_eq!(
        text.trim_end().lines().collect::<Vec<_>>(),
        install.lines(),
        "the resource says what the startup says"
    );
    let json: serde_json::Value = serde_json::from_str(&read(&operator, true).unwrap()).unwrap();
    assert_eq!(json["needs_nothing"], false);
    assert_eq!(json["grants"][0]["grant"], "worker-1");
    assert!(read(&Capability::scoped(["urn:cap:ledger:read:acme"]), false).is_err());

    // Fixed, it says so.
    install.grants(serde_json::json!({
        "worker-1": grants_for("acme", Authority::Worker).unwrap(),
    }));
    assert!(read(&operator, false).unwrap().contains("nothing to do"));
}
