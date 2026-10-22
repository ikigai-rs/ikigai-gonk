//! `ikigai-gonk kata import`, end to end: the real binary, a `kata export` file written by
//! hand from kata's export types (`tests/fixtures/kata-export.jsonl`), and gonk's owner-only
//! socket served in-process over a scratch in-memory store (ledger #2075) — the harness
//! `tests/roborev.rs` uses.
//!
//! - [`an_export_becomes_items_comments_closes_and_links_once`] — the mapping, and a re-run
//!   that changes nothing at all (every item's `updated` stamp is compared).
//! - [`a_later_export_adds_what_is_new_to_items_already_filed`] — the run converges: comments
//!   and links that arrive in a later export land on the items filed the first time.
//!
//! The real binary's own doors — a bridge filing over its socket, and its HTTP door still
//! refusing the anonymous write — are `tests/bridge_identity_2075.rs`.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

mod ready;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors};
use ikigai_store::DurableStore;

const FIXTURE: &str = "tests/fixtures/kata-export.jsonl";

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

/// `ikigai-gonk kata import <file> --socket … --ledger …`.
fn import(gonk: &Served, ledger: &str, file: &Path) -> Output {
    gonk.command()
        .args(["kata", "import"])
        .arg(file)
        .args(gonk.flags())
        .args(["--ledger", ledger])
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

fn source(hub: &Kernel, iri: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri.to_string()).unwrap()),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = block_on(Kernel::issue(hub, request, &Capability::root())).unwrap();
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// The item about one kata issue: its number, from the ledger's own `about` filter.
fn item_about(hub: &Kernel, ledger: &str, uid: &str) -> String {
    let listing = source(
        hub,
        &format!("urn:iki:ledger:{ledger}:items"),
        &[
            ("status", "all"),
            ("about", &format!("urn:kata:issue:{uid}")),
        ],
    );
    let lines: Vec<&str> = listing
        .lines()
        .filter(|l| l.trim_start().starts_with(&format!("{ledger}#")))
        .collect();
    assert_eq!(lines.len(), 1, "one item about {uid}: {listing}");
    let number = lines[0]
        .split_whitespace()
        .next()
        .unwrap()
        .rsplit_once('#')
        .unwrap()
        .1
        .to_string();
    source(hub, &format!("urn:iki:ledger:{ledger}:item:{number}"), &[])
}

/// Every item's detail, in number order: what a re-run must not change.
fn every_item(hub: &Kernel, ledger: &str) -> String {
    let listing = source(
        hub,
        &format!("urn:iki:ledger:{ledger}:items"),
        &[("status", "all")],
    );
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|t| t.rsplit_once('#').map(|(_, n)| n.to_string()))
        .map(|n| source(hub, &format!("urn:iki:ledger:{ledger}:item:{n}"), &[]))
        .collect()
}

const ISSUE_1: &str = "01K6Z000000000000000000001";
const ISSUE_2: &str = "01K6Z000000000000000000002";
const ISSUE_3: &str = "01K6Z000000000000000000003";

#[test]
fn an_export_becomes_items_comments_closes_and_links_once() {
    let (hub, gonk) = serve("kata");
    let first = stdout(&import(&gonk, "kata", Path::new(FIXTURE)));
    assert!(first.contains("3 filed, 0 already there"), "{first}");
    assert!(
        first.contains("2 comment(s), 1 close(s), 3 link(s) added"),
        "{first}"
    );
    assert!(
        first.contains("1 deleted issue(s) and 1 link(s) skipped"),
        "{first}"
    );
    assert!(
        first.contains("not read: event 1, sqlite_sequence 1"),
        "{first}"
    );

    let one = item_about(&hub, "kata", ISSUE_1);
    let two = item_about(&hub, "kata", ISSUE_2);
    let three = item_about(&hub, "kata", ISSUE_3);
    let iri = |detail: &str| {
        detail
            .lines()
            .find_map(|l| l.trim().strip_prefix("iri:"))
            .unwrap()
            .trim()
            .to_string()
    };

    // The issue: title, body, provenance, priority, labels, author, the key.
    assert!(
        one.lines()
            .next()
            .unwrap()
            .contains("open    p1  Stop the loop on an empty queue"),
        "{one}"
    );
    assert!(
        one.contains("[bug area:loop]") || one.contains("[area:loop bug]"),
        "{one}"
    );
    assert!(one.contains("by chris"), "{one}");
    assert!(
        one.contains(&format!("about:    urn:kata:issue:{ISSUE_1}")),
        "{one}"
    );
    assert!(one.contains("See the flight log."), "{one}");
    assert!(
        one.contains(&format!(
            "Imported from kata flight#0001 ({ISSUE_1}), filed 2026-09-02T09:15:00.000Z by chris; owner chris."
        )),
        "{one}"
    );
    // Its comments, attributed, each with its marker.
    assert!(one.contains("2 comment(s):"), "{one}");
    assert!(one.contains("Seen twice this week."), "{one}");
    assert!(
        one.contains(
            "(kata comment 01K6Z0000000000000000000C2, 2026-09-03T10:00:00.000Z, for agent-7)"
        ),
        "{one}"
    );
    assert!(one.contains(&format!("related:  {}", iri(&two))), "{one}");

    // Closed with kata's reason, and kata's close time as the note.
    assert!(two.lines().next().unwrap().contains("closed  p3"), "{two}");
    assert!(two.contains("(wontfix)"), "{two}");
    assert!(
        two.contains("Closed in kata at 2026-09-04T08:00:00.000Z (wontfix)."),
        "{two}"
    );

    // No priority in kata is no priority here; the links keep their direction.
    assert!(
        three.lines().next().unwrap().contains("open    p-"),
        "{three}"
    );
    assert!(
        three.contains(&format!("parent:   {}", iri(&one))),
        "{three}"
    );
    assert!(
        three.contains(&format!("blocks:   {}", iri(&one))),
        "{three}"
    );
    assert!(three.contains("by agent-7"), "{three}");

    // The deleted issue is not there at all.
    let all = source(&hub, "urn:iki:ledger:kata:items", &[("status", "all")]);
    assert!(!all.contains("A mistake"), "{all}");

    // ★ A re-run changes NOTHING: not one item's `updated` stamp, comment or link.
    let before = every_item(&hub, "kata");
    let again = stdout(&import(&gonk, "kata", Path::new(FIXTURE)));
    assert!(again.contains("0 filed, 3 already there"), "{again}");
    assert!(
        again.contains("0 comment(s), 0 close(s), 0 link(s) added"),
        "{again}"
    );
    assert_eq!(every_item(&hub, "kata"), before, "a re-run touched an item");
}

#[test]
fn a_later_export_adds_what_is_new_to_items_already_filed() {
    let (hub, gonk) = serve("kata");
    let full = std::fs::read_to_string(FIXTURE).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    // The earlier export: no comments and no links yet, and issue 0002 still open.
    let earlier: String = full
        .lines()
        .filter(|l| !l.contains(r#""kind":"comment""#) && !l.contains(r#""kind":"link""#))
        .map(|l| {
            if l.contains(ISSUE_2) {
                l.replace(
                    r#""closed_at":"2026-09-04T08:00:00.000Z","closed_reason":"wontfix""#,
                    r#""closed_at":null,"closed_reason":null"#,
                )
                .replace(r#""status":"closed""#, r#""status":"open""#)
            } else {
                l.to_string()
            }
        })
        .map(|l| l + "\n")
        .collect();
    let earlier_path = scratch.path().join("earlier.jsonl");
    std::fs::write(&earlier_path, earlier).unwrap();
    let first = stdout(&import(&gonk, "kata", &earlier_path));
    assert!(first.contains("3 filed"), "{first}");
    assert!(item_about(&hub, "kata", ISSUE_2)
        .lines()
        .next()
        .unwrap()
        .contains("open"));

    let later = stdout(&import(&gonk, "kata", Path::new(FIXTURE)));
    assert!(later.contains("0 filed, 3 already there"), "{later}");
    assert!(
        later.contains("2 comment(s), 1 close(s), 3 link(s) added"),
        "{later}"
    );
    let one = item_about(&hub, "kata", ISSUE_1);
    assert!(one.contains("2 comment(s):"), "{one}");
    assert!(item_about(&hub, "kata", ISSUE_2).contains("(wontfix)"));
}

/// `kata import --dry-run`, run once more with the flag.
fn dry_run(gonk: &Served, ledger: &str, file: &Path) -> Output {
    gonk.command()
        .args(["kata", "import"])
        .arg(file)
        .args(gonk.flags())
        .args(["--ledger", ledger, "--dry-run"])
        .output()
        .expect("run ikigai-gonk")
}

/// ★ Ledger #812: `--dry-run` does the READ half for real. Before, it never asked the ledger,
/// so after a real import it still said `would file` for every issue. Now it reads each issue's
/// key: it says `already`, reports exactly what a real run would add, and writes nothing.
#[test]
fn a_dry_run_reads_the_ledger_and_writes_nothing() {
    let (hub, gonk) = serve("kata");
    let scratch = tempfile::tempdir().unwrap();

    // An empty ledger: everything would be filed, and nothing is.
    let fresh = stdout(&dry_run(&gonk, "kata", Path::new(FIXTURE)));
    assert!(
        fresh.contains("dry run, nothing written: 3 would be filed, 0 already there"),
        "{fresh}"
    );
    assert!(
        fresh.contains("2 comment(s), 1 close(s), 3 link(s) would be added"),
        "{fresh}"
    );
    let all = source(&hub, "urn:iki:ledger:kata:items", &[("status", "all")]);
    assert!(all.contains("no items match"), "a dry run filed: {all}");

    // The earlier export, for real: the issues, no comments or links, 0002 still open.
    let full = std::fs::read_to_string(FIXTURE).unwrap();
    let earlier: String = full
        .lines()
        .filter(|l| !l.contains(r#""kind":"comment""#) && !l.contains(r#""kind":"link""#))
        .map(|l| {
            if l.contains(ISSUE_2) {
                l.replace(
                    r#""closed_at":"2026-09-04T08:00:00.000Z","closed_reason":"wontfix""#,
                    r#""closed_at":null,"closed_reason":null"#,
                )
                .replace(r#""status":"closed""#, r#""status":"open""#)
            } else {
                l.to_string()
            }
        })
        .map(|l| l + "\n")
        .collect();
    let earlier_path = scratch.path().join("earlier.jsonl");
    std::fs::write(&earlier_path, earlier).unwrap();
    stdout(&import(&gonk, "kata", &earlier_path));
    let before = every_item(&hub, "kata");

    // The later export, dry: every issue is ALREADY there, and the convergence a real run
    // would do is reported exactly — and not done.
    let later = stdout(&dry_run(&gonk, "kata", Path::new(FIXTURE)));
    assert!(
        later.contains("0 would be filed, 3 already there"),
        "{later}"
    );
    assert!(
        later.contains("2 comment(s), 1 close(s), 3 link(s) would be added"),
        "{later}"
    );
    assert_eq!(later.matches("already   kata#").count(), 3, "{later}");
    assert!(!later.contains("would file "), "{later}");
    assert_eq!(every_item(&hub, "kata"), before, "a dry run wrote");

    // After the real run, the dry run has nothing left to do.
    stdout(&import(&gonk, "kata", Path::new(FIXTURE)));
    let done = stdout(&dry_run(&gonk, "kata", Path::new(FIXTURE)));
    assert!(
        done.contains("0 would be filed, 3 already there; 0 comment(s), 0 close(s), 0 link(s)"),
        "{done}"
    );
}

/// ★ The race the keyed append exists for (ledger #779, #810), with the importer's own code:
/// several imports of ONE export at the same instant. Through the check-then-append path every
/// import whose `items?about=` check landed before the first append filed the issue again. The
/// keyed append is one store update, so the issue is one item however many imports race.
///
/// The export is issues only, open, with no comments or links: comments, closes and links are
/// converged by reading the item and then writing, which is still check-then-act (the ledger
/// has no keyed comment), so two imports racing on a NEW comment can both add it. The README
/// says so.
#[test]
fn concurrent_imports_of_one_issue_file_it_once() {
    const IMPORTS: usize = 8;
    const ROUNDS: usize = 5;
    let (hub, gonk) = serve("kata");
    let scratch = tempfile::tempdir().unwrap();
    let meta = r#"{"kind":"meta","data":{"key":"export_version","value":"27"}}"#;
    for round in 0..ROUNDS {
        let uid = format!("01K6ZRACE0000000000000000{round}");
        let issue = format!(
            r#"{{"kind":"issue","data":{{"id":1,"uid":"{uid}","project_id":1,"short_id":"r{round}","title":"Race {round}","body":"","status":"open","closed_reason":null,"owner":null,"author":"chris","created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-01T00:00:00.000Z","closed_at":null,"deleted_at":null}}}}"#
        );
        let path = scratch.path().join(format!("race{round}.jsonl"));
        std::fs::write(&path, format!("{meta}\n{issue}\n")).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(IMPORTS));
        let imports: Vec<_> = (0..IMPORTS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let mut command = gonk.command();
                command
                    .args(["kata", "import"])
                    .arg(&path)
                    .args(gonk.flags())
                    .args(["--ledger", "kata"]);
                std::thread::spawn(move || {
                    barrier.wait();
                    command.output().expect("run ikigai-gonk")
                })
            })
            .collect();
        let outputs: Vec<String> = imports
            .into_iter()
            .map(|import| stdout(&import.join().unwrap()))
            .collect();
        let listing = source(
            &hub,
            "urn:iki:ledger:kata:items",
            &[
                ("status", "all"),
                ("about", &format!("urn:kata:issue:{uid}")),
            ],
        );
        let filed = listing
            .lines()
            .filter(|l| l.trim_start().starts_with("kata#"))
            .count();
        assert_eq!(
            filed,
            1,
            "round {round}: {IMPORTS} concurrent imports filed one issue {filed} time(s):\n{listing}\n{}",
            outputs.join("\n")
        );
        assert_eq!(
            outputs.iter().filter(|o| o.contains("1 filed")).count(),
            1,
            "exactly one import says it filed:\n{}",
            outputs.join("\n")
        );
    }
}

/// The DEFAULT ledger's names have no ledger segment (`urn:iki:ledger:item:key:…`), so the key
/// lookup is a different name there: the import, the re-run and the dry run
/// must work on it exactly as on a named ledger.
#[test]
fn the_default_ledger_is_keyed_the_same_way() {
    let (_hub, gonk) = serve("default");
    let first = stdout(&import(&gonk, "default", Path::new(FIXTURE)));
    assert!(first.contains("3 filed, 0 already there"), "{first}");
    let again = stdout(&import(&gonk, "default", Path::new(FIXTURE)));
    assert!(again.contains("0 filed, 3 already there"), "{again}");
    assert!(
        again.contains("0 comment(s), 0 close(s), 0 link(s) added"),
        "{again}"
    );
    let dry = stdout(&dry_run(&gonk, "default", Path::new(FIXTURE)));
    assert!(dry.contains("0 would be filed, 3 already there"), "{dry}");
}
