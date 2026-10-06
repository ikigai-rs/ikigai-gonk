//! `ikigai-gonk kata import`, end to end: the real binary, a `kata export` file written by
//! hand from kata's export types (`tests/fixtures/kata-export.jsonl`), and gonk's HTTP door
//! served in-process over a scratch in-memory store — the harness `tests/roborev.rs` uses.
//!
//! - [`an_export_becomes_items_comments_closes_and_links_once`] — the mapping, and a re-run
//!   that changes nothing at all (every item's `updated` stamp is compared).
//! - [`a_later_export_adds_what_is_new_to_items_already_filed`] — the run converges: comments
//!   and links that arrive in a later export land on the items filed the first time.
//! - [`a_ledger_the_door_does_not_grant_is_refused_plainly`] — the refusal names the grant.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Output;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::{compose, doors, quic};
use ikigai_store::DurableStore;

const FIXTURE: &str = "tests/fixtures/kata-export.jsonl";

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

/// `ikigai-gonk kata import <file> --gonk … --ledger …`.
fn import(addr: SocketAddr, ledger: &str, file: &Path) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
        .args(["kata", "import"])
        .arg(file)
        .arg("--gonk")
        .arg(format!("http://127.0.0.1:{}", addr.port()))
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
    let (hub, addr, _config) = serve("kata");
    let first = stdout(&import(addr, "kata", Path::new(FIXTURE)));
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
    let again = stdout(&import(addr, "kata", Path::new(FIXTURE)));
    assert!(again.contains("0 filed, 3 already there"), "{again}");
    assert!(
        again.contains("0 comment(s), 0 close(s), 0 link(s) added"),
        "{again}"
    );
    assert_eq!(every_item(&hub, "kata"), before, "a re-run touched an item");
}

#[test]
fn a_later_export_adds_what_is_new_to_items_already_filed() {
    let (hub, addr, _config) = serve("kata");
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
    let first = stdout(&import(addr, "kata", &earlier_path));
    assert!(first.contains("3 filed"), "{first}");
    assert!(item_about(&hub, "kata", ISSUE_2)
        .lines()
        .next()
        .unwrap()
        .contains("open"));

    let later = stdout(&import(addr, "kata", Path::new(FIXTURE)));
    assert!(later.contains("0 filed, 3 already there"), "{later}");
    assert!(
        later.contains("2 comment(s), 1 close(s), 3 link(s) added"),
        "{later}"
    );
    let one = item_about(&hub, "kata", ISSUE_1);
    assert!(one.contains("2 comment(s):"), "{one}");
    assert!(item_about(&hub, "kata", ISSUE_2).contains("(wontfix)"));
}

#[test]
fn a_ledger_the_door_does_not_grant_is_refused_plainly() {
    let (hub, addr, _config) = serve("kata");
    let refused = import(addr, "elsewhere", Path::new(FIXTURE));
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("403"), "{stderr}");
    assert!(
        stderr.contains("ikigai-gonk grants"),
        "names the grant: {stderr}"
    );
    assert!(
        stderr.contains("gonk.http.ledger"),
        "names the setting: {stderr}"
    );
    let all = source(&hub, "urn:iki:ledger:kata:items", &[("status", "all")]);
    assert!(
        !all.contains("Stop the loop"),
        "nothing filed anywhere: {all}"
    );
}
