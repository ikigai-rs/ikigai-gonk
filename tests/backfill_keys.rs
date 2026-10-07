//! `ikigai-gonk ledger backfill-keys`, end to end on a scratch store: the real binary, over
//! the owner-only socket door exactly as `main` builds it, against items filed the way the
//! bridges filed them before the keyed append (`about` the bridge IRI, no `ledger:key`) —
//! including a duplicate pair, which is what the old check-then-append race made.
//!
//! - [`old_filings_gain_their_keys_once_and_duplicates_are_listed`] — dry run writes nothing;
//!   the real run keys the first filing of each IRI and lists the rest; a keyed append then
//!   answers `existing`; a second run writes nothing.

use std::path::Path;
use std::process::Output;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::{compose, doors};
use ikigai_store::DurableStore;

const RV1: &str = "urn:roborev:finding:00000000000000000000000000000001";
const RV2: &str = "urn:roborev:finding:00000000000000000000000000000002";
const RV3: &str = "urn:roborev:finding:00000000000000000000000000000003";
const KT1: &str = "urn:kata:issue:01K6Z000000000000000000001";

fn issue(hub: &Kernel, request: Request) -> String {
    let answer = block_on(Kernel::issue(hub, request, &Capability::root())).unwrap();
    String::from_utf8_lossy(&answer.bytes).into_owned()
}

/// An append as the bridges made it, `args` and all; the item's number.
fn append(hub: &Kernel, title: &str, args: &[(&str, &str)]) -> String {
    let request = args.iter().fold(
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:reviews:append").unwrap(),
        )
        .with_arg("content", ArgRef::Inline(title.as_bytes().to_vec())),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let answer = issue(hub, request);
    answer.split_whitespace().next().unwrap().to_string()
}

/// The item holding `key`, as `#N`-ish display, or `None`.
fn keyed(hub: &Kernel, key: &str) -> Option<String> {
    let listing = issue(
        hub,
        Request::new(
            Verb::Source,
            Iri::parse("urn:iki:ledger:reviews:items").unwrap(),
        )
        .with_arg("status", ArgRef::Inline(b"all".to_vec()))
        .with_arg("key", ArgRef::Inline(key.as_bytes().to_vec())),
    );
    let found: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| t.starts_with("reviews#"))
        .collect();
    assert!(found.len() <= 1, "two items hold {key}: {listing}");
    found.first().map(|s| s.to_string())
}

fn backfill(socket: &Path, extra: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["ledger", "backfill-keys", "--ledger", "reviews", "--socket"])
        .arg(socket)
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

#[test]
fn old_filings_gain_their_keys_once_and_duplicates_are_listed() {
    let hub = Arc::new(compose(
        DurableStore::in_memory().expect("an in-memory store"),
    ));
    // The old filings: roborev's `about` carries the file and the key, kata's the key alone.
    let first = append(
        &hub,
        "Boom.",
        &[("about", &format!("urn:repo:demo:file:src/a.rs {RV1}"))],
    );
    let raced = append(
        &hub,
        "Boom.",
        &[("about", &format!("urn:repo:demo:file:src/a.rs {RV1}"))],
    );
    let second = append(&hub, "Bang.", &[("about", RV2)]);
    let kata = append(&hub, "Ship it", &[("about", KT1)]);
    let unrelated = append(
        &hub,
        "Hand-filed",
        &[("about", "urn:repo:demo:file:src/b.rs")],
    );
    // And one filed by a keyed bridge already.
    let new = append(&hub, "Hmm.", &[("about", RV3), ("key", RV3)]);
    assert_eq!(
        [&first, &raced, &second, &kata, &unrelated, &new].map(|s| s.as_str()),
        [
            "reviews#1",
            "reviews#2",
            "reviews#3",
            "reviews#4",
            "reviews#5",
            "reviews#6"
        ]
    );

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("g.sock");
    let door = doors::door_kernel(Arc::clone(&hub));
    let path = socket.clone();
    std::thread::spawn(move || ikigai_ipc::serve(door, &path));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "the socket never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A dry run plans and writes nothing.
    let dry = stdout(&backfill(&socket, &["--dry-run"]));
    assert!(
        dry.contains(&format!("would key  reviews#1  {RV1}")),
        "{dry}"
    );
    assert!(
        dry.contains(&format!("would key  reviews#3  {RV2}")),
        "{dry}"
    );
    assert!(
        dry.contains(&format!("would key  reviews#4  {KT1}")),
        "{dry}"
    );
    assert!(dry.contains("duplicate reviews#2 of reviews#1"), "{dry}");
    assert!(
        dry.contains("sink urn:iki:ledger:reviews:close item=2 reason=duplicate"),
        "the line that closes it: {dry}"
    );
    assert!(
        dry.contains(
            "dry run, nothing written: 3 key(s) would be written, 1 item(s) already keyed, \
             1 duplicate(s) listed, 0 skipped"
        ),
        "{dry}"
    );
    for key in [RV1, RV2, KT1] {
        assert_eq!(keyed(&hub, key), None, "a dry run keyed {key}");
    }

    // The real run: the FIRST filing of each IRI takes its key; the race's second is listed.
    let run = stdout(&backfill(&socket, &[]));
    assert!(
        run.contains("3 key(s) written, 1 item(s) already keyed, 1 duplicate(s) listed"),
        "{run}"
    );
    assert_eq!(keyed(&hub, RV1).as_deref(), Some("reviews#1"));
    assert_eq!(keyed(&hub, RV2).as_deref(), Some("reviews#3"));
    assert_eq!(keyed(&hub, KT1).as_deref(), Some("reviews#4"));
    assert_eq!(keyed(&hub, RV3).as_deref(), Some("reviews#6"));

    // ★ What the backfill is for: the keyed append now finds the old filing instead of filing
    // it a third time.
    let replay = issue(
        &hub,
        Request::new(
            Verb::Sink,
            Iri::parse("urn:iki:ledger:reviews:append").unwrap(),
        )
        .with_arg("content", ArgRef::Inline(b"Boom.".to_vec()))
        .with_arg("key", ArgRef::Inline(RV1.as_bytes().to_vec()))
        .with_arg("about", ArgRef::Inline(RV1.as_bytes().to_vec())),
    );
    assert!(
        replay.starts_with("reviews#1 ") && replay.contains(" existing open"),
        "{replay}"
    );

    // Idempotent: nothing left to write, and the duplicate is still listed for a person.
    let again = stdout(&backfill(&socket, &[]));
    assert!(
        again.contains("0 key(s) written, 4 item(s) already keyed, 1 duplicate(s) listed"),
        "{again}"
    );
}

#[test]
fn an_unreachable_socket_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let output = backfill(&dir.path().join("none.sock"), &["--dry-run"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot reach gonk's socket"), "{stderr}");
}
