//! Audit round 4's reproductions (ledger [#864](http://localhost:1060/l/default/item/864)),
//! ported from `ikigai-devtools/claude/research/audit-ikigai-gonk-2026-10-07/claude/repro/`.
//!
//! Each test asserts the behavior the code's own documentation promises, and each FAILED on
//! `c3443a6` because of the defect it names — run there before its fix, which is what makes
//! it a regression test rather than a description. R1–R6 are the auditor's tests with their
//! bodies unchanged; R7 was a shell probe in `RUN.sh` and is the same probe in Rust here,
//! against the built binary with a scratch `HOME` and `XDG_CONFIG_HOME`.
//!
//! Nothing here touches a live gonk: every server binds `127.0.0.1:0` over a tempdir config
//! home, and every CLI run has its own homes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity;
use ikigai_gonk::{compose_with, doors, quic};
use ikigai_store::DurableStore;

fn request(verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    )
}

// ------------------------------------------------------------------------------------------
// R1. The backup family's tokens are refused at startup but ADMITTED per use.
//
// `grants::CAP_GONK_ADMIN`'s doc: "Neither can be attached to a certificate or a passkey, so
// neither is reachable from the QUIC door or the HTTP door at all". `trigger.rs`'s module doc:
// gonk refuses "the backup family's tokens on every certificate and every passkey it admits
// (check_grants), at startup and again per use". The per-use check is
// `quic::scopes_for_grant` (QUIC minter per connection, `identity::scopes_of` per passkey
// request), and it checks broad + wildcard tokens but NOT the backup family. Trigger: an
// operator edits grants.json while gonk runs (the files are re-read per connection precisely
// so an edit takes effect), adding `urn:cap:gonk:restore` to a grant.
// ------------------------------------------------------------------------------------------
#[test]
fn r1_a_grant_naming_the_restore_token_is_refused_per_use_as_it_is_at_startup() {
    const FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";
    let config = tempfile::tempdir().unwrap();
    let layout = quic::Layout::in_config_home(config.path());
    std::fs::create_dir_all(config.path().join("gonk")).unwrap();
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push(backup::CAP_RESTORE.to_string());
    std::fs::write(
        layout.grants_json(),
        serde_json::json!({ "ops": scopes }).to_string(),
    )
    .unwrap();
    std::fs::write(
        layout.clients_json(),
        format!(r#"{{"clients": {{"{FP}": {{"grant": "ops"}}}}}}"#),
    )
    .unwrap();

    let grants: BTreeMap<String, Vec<String>> = quic::read_grants(&layout.grants_json()).unwrap();
    let at_startup = quic::check_grants(&grants);
    assert!(at_startup.is_err(), "startup refuses the restore token");

    // Per use: the QUIC door's per-connection decision, and the HTTP door's per-request one.
    let enrolment = quic::read_enrolment(&layout.clients_json())
        .unwrap()
        .unwrap();
    let per_connection = quic::authority(&enrolment, &grants, FP);
    let per_passkey = identity::scopes_of(&layout, "ops");

    // And what that capability reaches through the QUIC door's kernel: a restore that
    // writes a RocksDB store at a path the REMOTE caller chose.
    let backups_dir = tempfile::tempdir().unwrap();
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().unwrap(),
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: backups_dir.path().to_path_buf(),
                keep: 5,
                every: None,
                store_path: PathBuf::from("/nonexistent/live/store"),
            }),
            jobs: None,
        }),
    ));
    let door = doors::door_kernel(Arc::clone(&hub));
    let into = backups_dir.path().join("chosen-by-the-remote-caller");
    let reached = per_connection.as_ref().ok().map(|(_, cap)| {
        block_on(Kernel::issue(
            &door,
            request(
                Verb::Sink,
                backup::RESTORE,
                &[
                    ("into", into.to_str().unwrap().as_bytes()),
                    ("content", b"<urn:a> <urn:b> <urn:c> <urn:g> .\n"),
                ],
            ),
            cap,
        ))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
    });

    assert!(
        per_connection.is_err() && per_passkey.is_err(),
        "startup refuses grant `ops` ({}) but per use admits it — quic per-connection: {:?}; \
         passkey per-request: {:?}; Sink {} through the QUIC door kernel under that capability: \
         {:?}; directory written: {}",
        at_startup.unwrap_err().lines().next().unwrap_or(""),
        per_connection.as_ref().map(|(g, _)| g),
        per_passkey,
        backup::RESTORE,
        reached,
        into.is_dir()
    );
}

// ------------------------------------------------------------------------------------------
// R1, every path. One function decides (`quic::grant_refusal`), and each path that turns a
// grant into scopes is asked here on its own, so a fifth path added later without it is the
// test that goes red rather than the audit that finds it.
// ------------------------------------------------------------------------------------------
#[test]
fn r1_every_grant_to_scopes_path_refuses_the_backup_family() {
    for token in ikigai_gonk::grants::CAP_GONK_ADMIN {
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let mut scopes = grants_for("default", Authority::Read).unwrap();
        scopes.push(token.to_string());
        let grants: BTreeMap<String, Vec<String>> =
            [("ops".to_string(), scopes.clone())].into_iter().collect();

        assert!(quic::check_grants(&grants).is_err(), "startup: {token}");
        assert!(
            quic::scopes_for_grant(&grants, "ops").is_err(),
            "per use: {token}"
        );
        assert!(
            ikigai_gonk::trigger::reviewer_scopes(&grants, "ops").is_err(),
            "the reviewer grant: {token}"
        );
        // The QUIC door's per-connection decision, and the passkey door's per-request one,
        // read the files as an edit after startup left them.
        std::fs::create_dir_all(config.path().join("gonk")).unwrap();
        std::fs::write(
            layout.grants_json(),
            serde_json::json!({ "ops": scopes }).to_string(),
        )
        .unwrap();
        let enrolment = quic::parse_enrolment(r#"{"clients": {"ab": "ops"}}"#).unwrap();
        assert!(
            quic::authority(&enrolment, &grants, "ab").is_err(),
            "per connection: {token}"
        );
        assert!(
            identity::scopes_of(&layout, "ops").is_err(),
            "per passkey request: {token}"
        );

        // Both writers of grants.json refuse it, and write nothing.
        let fresh = tempfile::tempdir().unwrap();
        let fresh = quic::Layout::in_config_home(fresh.path());
        let refused = quic::put_grant(&fresh, "ops", &scopes, true).unwrap_err();
        assert!(refused.contains(token), "{refused}");
        let refused = quic::enrol(&fresh, "ops", "ab", &scopes, true).unwrap_err();
        assert!(refused.contains(token), "{refused}");
        assert!(!fresh.grants_json().exists() && !fresh.clients_json().exists());
    }
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 2: `enrol` applied only the broad-token refusal, while `put_grant` also
// refused the offering wildcards — so a certificate could be enrolled under a grant a
// passkey could not. Both are `grant_refusal` now.
// ------------------------------------------------------------------------------------------
#[test]
fn enrol_refuses_the_offering_wildcards_as_put_grant_does() {
    for wildcard in [
        ikigai_gonk::grants::CAP_NET_ANY,
        ikigai_gonk::grants::CAP_EXEC_ANY,
    ] {
        let config = tempfile::tempdir().unwrap();
        let layout = quic::Layout::in_config_home(config.path());
        let mut scopes = grants_for("default", Authority::Read).unwrap();
        scopes.push(wildcard.to_string());
        let refused = quic::enrol(&layout, "laptop", "ab", &scopes, true).unwrap_err();
        assert!(refused.contains(wildcard), "{refused}");
        assert!(!layout.grants_json().exists() && !layout.clients_json().exists());
        assert_eq!(
            refused,
            quic::put_grant(&layout, "laptop", &scopes, true).unwrap_err(),
            "the two writers refuse with one sentence"
        );
    }
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 3: `client add` minted (or wrote) the bundle and THEN enrolled it, so a
// refusal from the enrolment left a bundle behind. Importing another client's certificate
// under a new name is refused by the enrolment ("already enrolled under grant `a`"), and on
// `c3443a6` it left `clients/b/` with that certificate in it.
// ------------------------------------------------------------------------------------------
#[test]
fn client_add_refuses_before_it_writes_a_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for dir in ["home", "xdg/ikigai"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .output()
            .expect("run ikigai-gonk")
    };
    let clients = root.join("xdg/ikigai/gonk/quic/clients");
    let first = run(&["client", "add", "a", "--ledger", "default=read"]);
    assert!(first.status.success(), "{first:?}");
    let a_cert = clients.join("a/client.crt");
    let refused = run(&[
        "client",
        "add",
        "b",
        "--cert",
        a_cert.to_str().unwrap(),
        "--ledger",
        "default=read",
    ]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("already enrolled under grant `a`"),
        "{refused:?}"
    );
    assert!(
        !clients.join("b").exists(),
        "a refused enrolment left a bundle behind"
    );
}

// ------------------------------------------------------------------------------------------
// Ledger #805, part 1: the net host in a grant is enforced against the mount it reaches.
// `ikigai-browse` declares the offering wildcard `urn:cap:net:*`, which the kernel satisfies
// with ANY net grant — so a grant naming `127.0.0.1` reached a socket mount (whose host is
// `localhost`). The mount refuses it by name, before it dials: the socket below does not
// exist, so a call that got past the check would come back `Unavailable`, not `Denied`.
// ------------------------------------------------------------------------------------------
#[test]
fn a_net_grant_for_another_host_is_refused_at_the_mount() {
    use ikigai_resolve::Resolver;
    let home = std::path::Path::new("/nonexistent-home");
    let mount = ikigai_gonk::mount::parse("prefer urn:llm:=/nonexistent/llm.sock", home).unwrap();
    let lazy = ikigai_gonk::mount::LazyMount::new(mount.target.clone());
    let ask = || request(Verb::Source, "urn:llm:coder:ask", &[("prompt", b"hi")]);
    let as_host = |host: &str| Capability::scoped([format!("urn:cap:net:{host}")]);

    match lazy.issue_as(ask(), &as_host("127.0.0.1")) {
        Err(ikigai_core::Error::Denied(why)) => {
            assert!(why.contains("urn:cap:net:localhost"), "{why}")
        }
        other => panic!("a grant for another host reached the mount: {other:?}"),
    }
    match lazy.issue_as(ask(), &as_host("localhost")) {
        Err(ikigai_core::Error::Unavailable(_)) => {}
        other => panic!("the mount's own host is admitted, then the dial fails: {other:?}"),
    }
}
