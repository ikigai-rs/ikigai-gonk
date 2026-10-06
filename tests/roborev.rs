//! `ikigai-gonk roborev file`, end to end: the real binary, a roborev review rendered the
//! way roborev's `Document.Markdown` renders it (`tests/fixtures/roborev-review-completed.md`),
//! and gonk's HTTP door served in-process over a scratch in-memory store.
//!
//! - [`a_review_files_one_item_per_finding_once`] — the mapping (about, labels, priority,
//!   revision), and the same payload filed twice yields each item once.
//! - [`a_ledger_the_door_does_not_grant_is_refused_plainly`] — the refusal names the grant.

use std::net::SocketAddr;
use std::process::Output;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_store::DurableStore;

const FIXTURE: &str = include_str!("fixtures/roborev-review-completed.md");

/// The HTTP door exactly as `main` builds it, granting `ledger` read and write to an
/// anonymous loopback caller. Returns the hub (to read back through) and the address.
fn serve(ledger: &str) -> (Arc<Kernel>, SocketAddr, tempfile::TempDir) {
    let grants = grants_for(ledger, Authority::Write).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let config = tempfile::tempdir().unwrap();
    let hub = Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ));
    let passkeys = Arc::new(ikigai_gonk::identity::Passkeys::new(
        quic::Layout::in_config_home(config.path()),
        addr.port(),
    ));
    let face = Arc::new(ikigai_gonk::web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec![ledger.to_string()],
        browse_roots: Vec::new(),
        passkeys: Arc::clone(&passkeys),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
    });
    let kernel = Arc::new(doors::http_kernel(
        Arc::clone(&hub),
        ikigai_gonk::web::space(face),
    ));
    let door = doors::HttpDoor {
        anonymous: grants,
        port: addr.port(),
        passkeys: Some(passkeys),
    };
    let cap = doors::http_cap(door.clone());
    let edge = doors::edge_config(door);
    std::thread::spawn(move || {
        runtime.block_on(ikigai_web::serve_with_listener(kernel, cap, listener, edge))
    });
    (hub, addr, config)
}

/// The command a roborev hook line runs, with roborev's values filled in.
fn file(addr: SocketAddr, ledger: &str, extra: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["roborev", "file", "--gonk"])
        .arg(format!("http://127.0.0.1:{}", addr.port()))
        .args(["--ledger", ledger, "--root", "demo", "--job", "42"])
        .args(["--sha", "0123456789abcdef0123456789abcdef01234567"])
        .args(["--agent", "codex", "--repo-path", "/work/demo"])
        .args(["--findings", FIXTURE])
        .args(extra)
        .output()
        .expect("run ikigai-gonk")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `Source urn:iki:ledger:{ledger}:items`, every status, under root, read from the hub.
fn items(hub: &Kernel, ledger: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(
            Verb::Source,
            Iri::parse(format!("urn:iki:ledger:{ledger}:items")).unwrap(),
        )
        .with_arg("status", ArgRef::Inline(b"all".to_vec())),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(Kernel::issue(hub, request, &Capability::root())).unwrap();
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

fn item_lines(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .filter(|line| line.trim_start().starts_with("reviews#"))
        .collect()
}

#[test]
fn a_review_files_one_item_per_finding_once() {
    let (hub, addr, _config) = serve("reviews");

    let first = stdout(&file(addr, "reviews", &[]));
    assert_eq!(first.matches("filed ").count(), 3, "{first}");
    assert_eq!(
        first.matches("skipped ").count(),
        1,
        "low is skipped by default: {first}"
    );
    let all = items(&hub, "reviews", &[]);
    assert_eq!(item_lines(&all).len(), 3, "{all}");

    // The mapping: severity → priority and label, the file joined by `about`.
    let retry = items(
        &hub,
        "reviews",
        &[("about", "urn:repo:demo:file:src/retry.rs")],
    );
    let lines = item_lines(&retry);
    assert_eq!(
        lines.len(),
        2,
        "the critical and the high are about src/retry.rs: {retry}"
    );
    let critical = lines
        .iter()
        .find(|l| l.contains("[critical roborev]"))
        .unwrap_or_else(|| panic!("{retry}"));
    assert!(critical.contains(" p0 "), "{critical}");
    assert!(
        critical.contains("`write_with_retry` loops while the store"),
        "{critical}"
    );
    let high = lines.iter().find(|l| l.contains("[high roborev]")).unwrap();
    assert!(high.contains(" p1 "), "{high}");
    let medium = items(&hub, "reviews", &[("labels", "medium")]);
    assert!(item_lines(&medium)[0].contains(" p2 "), "{medium}");

    // One item, in full: body, revision, both about IRIs.
    let number = critical.split_whitespace().next().unwrap();
    let id = number.split_once('#').unwrap().1;
    let request = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:iki:ledger:reviews:item:{id}")).unwrap(),
    );
    let one = block_on(Kernel::issue(&hub, request, &Capability::root())).unwrap();
    let one = String::from_utf8_lossy(&one.bytes);
    for expected in [
        "urn:repo:demo:file:src/retry.rs",
        "urn:roborev:finding:",
        "Fix: Bound the loop",
        "- commit: 0123456789abcdef0123456789abcdef01234567",
        "- roborev job: 42 (`roborev show 42`)",
        "- reported by: codex, claude-code",
        "Trigger: a writer that holds the RocksDB lock",
    ] {
        assert!(one.contains(expected), "missing `{expected}` in:\n{one}");
    }

    // The same payload again: nothing new.
    let again = stdout(&file(addr, "reviews", &[]));
    assert_eq!(again.matches("already ").count(), 3, "{again}");
    assert_eq!(again.matches("filed ").count(), 0, "{again}");
    assert_eq!(item_lines(&items(&hub, "reviews", &[])).len(), 3);

    // Asking for the low ones files exactly the one that was skipped.
    let low = stdout(&file(addr, "reviews", &["--min-severity", "low"]));
    assert_eq!(low.matches("filed ").count(), 1, "{low}");
    assert_eq!(low.matches("already ").count(), 3, "{low}");
    let all = items(&hub, "reviews", &[]);
    assert_eq!(item_lines(&all).len(), 4, "{all}");
    assert!(
        all.contains("[low roborev]") && all.contains(" p3 "),
        "{all}"
    );
}

/// ★ The race the keyed append exists for (ledger #779, #810), with the bridge's own code:
/// several hooks filing ONE finding at the same instant. Through the check-then-append path
/// (`GET items?about=<key>`, then `POST append` when the list was empty) every hook whose check
/// landed before the first append filed it, so one finding became several items. The keyed
/// append is one store update, so exactly one hook files and the rest are told `already`.
#[test]
fn concurrent_hooks_filing_one_finding_file_it_once() {
    const HOOKS: usize = 8;
    const ROUNDS: usize = 5;
    let (hub, addr, _config) = serve("reviews");
    for round in 0..ROUNDS {
        let markdown = format!(
            "## Summary\n\nOne.\n\n## Findings\n\n### 1. High\n\n**Location:** src/race.rs:{round}\
             \n\n**Problem:** Round {round} races.\n\n**Fix:** Key the append.\n"
        );
        let barrier = Arc::new(std::sync::Barrier::new(HOOKS));
        let hooks: Vec<_> = (0..HOOKS)
            .map(|_| {
                let (barrier, markdown) = (Arc::clone(&barrier), markdown.clone());
                std::thread::spawn(move || {
                    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
                    command
                        .args(["roborev", "file", "--gonk"])
                        .arg(format!("http://127.0.0.1:{}", addr.port()))
                        .args(["--ledger", "reviews", "--root", "demo"])
                        .args(["--findings", &markdown]);
                    barrier.wait();
                    command.output().expect("run ikigai-gonk")
                })
            })
            .collect();
        let outputs: Vec<String> = hooks
            .into_iter()
            .map(|hook| stdout(&hook.join().unwrap()))
            .collect();
        let filed = outputs.iter().filter(|o| o.contains("filed ")).count();
        let lines = item_lines(&items(
            &hub,
            "reviews",
            &[("about", &format!("urn:repo:demo:file:src/race.rs"))],
        ))
        .len();
        assert_eq!(
            lines,
            round + 1,
            "round {round}: {HOOKS} concurrent hooks filed one finding {} time(s) (stdout says \
             {filed}):\n{}",
            lines - round,
            outputs.join("\n")
        );
        assert_eq!(filed, 1, "exactly one hook says it filed:\n{}", outputs.join("\n"));
    }
}

#[test]
fn a_ledger_the_door_does_not_grant_is_refused_plainly() {
    let (hub, addr, _config) = serve("reviews");
    let refused = file(addr, "elsewhere", &[]);
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("refused"), "{stderr}");
    assert!(stderr.contains("gonk.http.ledger"), "{stderr}");
    assert!(item_lines(&items(&hub, "reviews", &[])).is_empty());
}

#[test]
fn prose_from_a_fix_job_files_nothing_and_succeeds() {
    let (hub, addr, _config) = serve("reviews");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["roborev", "file", "--gonk"])
        .arg(format!("http://127.0.0.1:{}", addr.port()))
        .args(["--ledger", "reviews", "--root", "demo"])
        .args([
            "--findings",
            "## Plan\n\nRetry less.\n\n## Implementation\n\nDone.",
        ])
        .output()
        .unwrap();
    assert!(stdout(&output).contains("nothing to file"));
    assert!(item_lines(&items(&hub, "reviews", &[])).is_empty());
}
