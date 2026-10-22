//! `ikigai-gonk roborev file`, end to end: the real binary, a roborev review rendered the
//! way roborev's `Document.Markdown` renders it (`tests/fixtures/roborev-review-completed.md`),
//! and gonk's owner-only socket served in-process over a scratch in-memory store (ledger #2075).
//!
//! - [`a_review_files_one_item_per_finding_once`] — the mapping (about, labels, priority,
//!   revision), and the same payload filed twice yields each item once.
//! - [`a_gonk_that_is_not_running_is_said_plainly`] — the error names the socket.
//!
//! The real binary's own doors — a bridge filing over its socket, and its HTTP door still
//! refusing the anonymous write — are `tests/bridge_identity_2075.rs`.

use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;

mod ready;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors};
use ikigai_store::DurableStore;

const FIXTURE: &str = include_str!("fixtures/roborev-review-completed.md");

/// The socket door as `main` builds it (`doors::door_kernel`, the owner's), served in-process
/// over a scratch in-memory store: the door both bridges file through since ledger #2075.
/// Returns the hub (to read back through) and where the command finds it.
fn serve(_ledger: &str) -> (Arc<Kernel>, Served) {
    let hub = Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ));
    // A Unix socket path must fit `sun_path` (104 bytes on macOS); a scratch dir may not.
    let short = tempfile::Builder::new()
        .prefix("gk")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = short.path().join("s");
    let door = doors::door_kernel(Arc::clone(&hub));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    ready::socket(&socket);
    let homes = tempfile::tempdir().unwrap();
    std::fs::write(homes.path().join("config.toml"), "").unwrap();
    (
        hub,
        Served {
            socket,
            homes,
            _short: short,
        },
    )
}

/// Where a served socket is, and scratch homes, so nothing of this machine's gonk is read.
struct Served {
    socket: PathBuf,
    homes: tempfile::TempDir,
    _short: tempfile::TempDir,
}

impl Served {
    /// The binary, told this socket and these homes.
    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"));
        command.env("HOME", self.homes.path());
        command
    }

    /// The arguments naming this socket and these homes.
    fn flags(&self) -> Vec<std::ffi::OsString> {
        let homes = self.homes.path();
        vec![
            "--socket".into(),
            self.socket.clone().into(),
            "--config".into(),
            homes.join("config.toml").into(),
            "--config-home".into(),
            homes.into(),
            "--data-home".into(),
            homes.into(),
        ]
    }
}

/// The command a roborev hook line runs, with roborev's values filled in.
fn file(gonk: &Served, ledger: &str, extra: &[&str]) -> Output {
    gonk.command()
        .args(["roborev", "file"])
        .args(gonk.flags())
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
    let (hub, gonk) = serve("reviews");

    let first = stdout(&file(&gonk, "reviews", &[]));
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
    let again = stdout(&file(&gonk, "reviews", &[]));
    assert_eq!(again.matches("already ").count(), 3, "{again}");
    assert_eq!(again.matches("filed ").count(), 0, "{again}");
    assert_eq!(item_lines(&items(&hub, "reviews", &[])).len(), 3);

    // Asking for the low ones files exactly the one that was skipped.
    let low = stdout(&file(&gonk, "reviews", &["--min-severity", "low"]));
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
    let (hub, gonk) = serve("reviews");
    for round in 0..ROUNDS {
        let markdown = format!(
            "## Summary\n\nOne.\n\n## Findings\n\n### 1. High\n\n**Location:** src/race.rs:{round}\
             \n\n**Problem:** Round {round} races.\n\n**Fix:** Key the append.\n"
        );
        let barrier = Arc::new(std::sync::Barrier::new(HOOKS));
        let hooks: Vec<_> = (0..HOOKS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let mut command = gonk.command();
                command
                    .args(["roborev", "file"])
                    .args(gonk.flags())
                    .args(["--ledger", "reviews", "--root", "demo"])
                    .args(["--findings", &markdown]);
                std::thread::spawn(move || {
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
            &[("about", "urn:repo:demo:file:src/race.rs")],
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
        assert_eq!(
            filed,
            1,
            "exactly one hook says it filed:\n{}",
            outputs.join("\n")
        );
    }
}

#[test]
fn a_gonk_that_is_not_running_is_said_plainly() {
    let (hub, gonk) = serve("reviews");
    let output = gonk
        .command()
        .args(["roborev", "file", "--socket"])
        .arg(gonk.homes.path().join("nobody.sock"))
        .args(["--ledger", "reviews", "--root", "demo"])
        .args(["--findings", FIXTURE])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot reach gonk's socket"), "{stderr}");
    assert!(stderr.contains("nobody.sock"), "{stderr}");
    assert!(item_lines(&items(&hub, "reviews", &[])).is_empty());
}

#[test]
fn prose_from_a_fix_job_files_nothing_and_succeeds() {
    let (hub, gonk) = serve("reviews");
    let output = gonk
        .command()
        .args(["roborev", "file"])
        .args(gonk.flags())
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
