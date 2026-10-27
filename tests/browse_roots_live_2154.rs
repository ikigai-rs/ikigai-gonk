//! Ledger [#2154](http://localhost:1062/l/default/item/2154): a browse root added to or removed
//! from `config.toml` mounts or unmounts while gonk runs, with no restart, and a client that
//! was connected the whole time stays connected.
//!
//! The REAL binary over a scratch config home, as every scratch-gonk test runs it: a QUIC
//! client (a worker's door) and one owner socket connection are opened first and used last, and
//! between them the owner adds a root, browses it, is refused the bad cases, removes it, and an
//! edit of the file by hand mounts another and refuses a third.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

mod common;
mod scratch_gonk;

use ikigai_core::{ArgRef, Error, Iri, Representation, Request, Verb};
use ikigai_resolve::Resolver;
use scratch_gonk::Gonk;

fn request(verb: Verb, iri: &str, content: Option<&str>) -> Request {
    let request = Request::new(verb, Iri::parse(iri).expect("a test IRI"));
    match content {
        Some(content) => request.with_arg("content", ArgRef::Inline(content.as_bytes().to_vec())),
        None => request,
    }
}

fn text(answer: Result<Representation, Error>) -> String {
    String::from_utf8(answer.expect("an answer").bytes).expect("UTF-8")
}

/// A directory holding one file, `name.txt`, whose text is `name`.
fn repo(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.txt")), name).unwrap();
    dir.canonicalize().unwrap()
}

/// The config file the status names on its first line.
fn config_path(status: &str) -> PathBuf {
    let first = status.lines().next().unwrap();
    let path = first
        .strip_prefix("browse roots from ")
        .and_then(|rest| rest.split(" (live").next())
        .unwrap_or_else(|| panic!("not a live status: {first}"));
    PathBuf::from(path)
}

/// Whether `root` is a line in force in `status` (`  {root}\t…`).
fn in_force(status: &str, root: &str) -> bool {
    status
        .lines()
        .any(|line| line.starts_with(&format!("  {root}\t")))
}

#[test]
fn a_root_mounts_and_unmounts_with_no_restart_and_no_dropped_connection() {
    let repos = tempfile::tempdir().unwrap();
    let (a, b, d) = (
        repo(repos.path(), "a"),
        repo(repos.path(), "b"),
        repo(repos.path(), "d"),
    );
    let gonk = Gonk::with_quic(&format!("gonk.browse.root = \"a={}\"\n", a.display()));
    let pid = gonk.child.id();

    // Opened FIRST, used throughout and last: a restart would break both.
    let worker = gonk.dial_quic().expect("the worker's QUIC connection");
    let owner = ikigai_ipc::connect(&gonk.socket).expect("the owner's socket");
    let issue = |r: Request| owner.issue(r).map(|(repr, _)| repr);
    let filed = worker
        .issue(request(Verb::Sink, "urn:iki:ledger:append", Some("before")))
        .map(|(repr, _)| repr);
    assert!(filed.is_ok(), "the worker files before: {filed:?}");

    let status = text(issue(request(
        Verb::Source,
        "urn:iki:gonk:browse:roots",
        None,
    )));
    assert!(
        in_force(&status, "a") && !in_force(&status, "b"),
        "{status}"
    );
    let config = config_path(&status);
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:a:file:a.txt", None))),
        "a"
    );
    assert!(
        issue(request(Verb::Source, "urn:repo:b:file:b.txt", None)).is_err(),
        "b is not a root yet"
    );

    // ADD, through the binary's own command, over the running gonk's socket.
    let cli = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--socket")
            .arg(&gonk.socket)
            .arg("--config")
            .arg(&config)
            .output()
            .expect("run ikigai-gonk");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned()
                + &String::from_utf8_lossy(&output.stderr),
        )
    };
    let (ok, out) = cli(&["browse", "add", &format!("b={}", b.display())]);
    assert!(ok && in_force(&out, "b"), "{out}");
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:b:file:b.txt", None))),
        "b",
        "b serves with no restart"
    );
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:b:file:b.txt", None))),
        "b",
        "and serves again (from the cache, under the new watch)"
    );
    // The first root was untouched by the re-arrangement.
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:a:file:a.txt", None))),
        "a"
    );
    let written = std::fs::read_to_string(&config).unwrap();
    let line = format!("gonk.browse.root = \"b={}\"", b.display());
    assert_eq!(written.matches(&line).count(), 1, "{written}");

    // Idempotent: the same name at the same directory writes nothing and cuts nothing.
    let again = text(issue(request(
        Verb::Sink,
        "urn:iki:gonk:browse:root:b",
        Some(&b.display().to_string()),
    )));
    assert!(again.contains("1 re-arrangement(s)"), "{again}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), written);

    // Refused, with a reason, and nothing written: a directory that is not there, and a name
    // already configured for another directory.
    let missing = issue(request(
        Verb::Sink,
        "urn:iki:gonk:browse:root:c",
        Some("/no/such/dir/2154"),
    ))
    .unwrap_err();
    assert!(missing.to_string().contains("not a directory"), "{missing}");
    let conflict = issue(request(
        Verb::Sink,
        "urn:iki:gonk:browse:root:b",
        Some(&a.display().to_string()),
    ))
    .unwrap_err();
    assert!(
        conflict.to_string().contains("already configured"),
        "{conflict}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), written);

    // REMOVE: gone from the family and the status, commented out of the file.
    let (ok, out) = cli(&["browse", "remove", "b"]);
    assert!(ok && !in_force(&out, "b") && in_force(&out, "a"), "{out}");
    assert!(
        issue(request(Verb::Source, "urn:repo:b:file:b.txt", None)).is_err(),
        "b was served from the cache after its removal"
    );
    let removed = std::fs::read_to_string(&config).unwrap();
    assert!(removed.contains(&format!("# {line}")), "{removed}");
    assert!(
        cli(&["browse", "remove", "b"]).0,
        "removing it twice is not an error"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), removed);

    // The same NAME over another directory: an answer cached under the old arrangement must
    // not survive it. (The removal alone cannot show this: an unbound name never reaches the
    // cache. A re-bound one does, and this is the read the coarse cut exists for.)
    let elsewhere = repos.path().join("b-elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("b.txt"), "b, elsewhere").unwrap();
    text(issue(request(
        Verb::Sink,
        "urn:iki:gonk:browse:root:b",
        Some(&elsewhere.display().to_string()),
    )));
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:b:file:b.txt", None))),
        "b, elsewhere",
        "a read cached under the old directory outlived the re-arrangement"
    );
    let (ok, out) = cli(&["browse", "remove", "b"]);
    assert!(ok && !in_force(&out, "b"), "{out}");
    let removed = std::fs::read_to_string(&config).unwrap();

    // A HAND edit of the file: one good line mounts and one bad line is refused, and the root
    // in force goes on serving. Heard by the config watch, so polled.
    std::fs::write(
        &config,
        format!(
            "{removed}gonk.browse.root = \"d={}\"\ngonk.browse.root = \"e=/no/such/dir/2154\"\n",
            d.display()
        ),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        let status = text(issue(request(
            Verb::Source,
            "urn:iki:gonk:browse:roots",
            None,
        )));
        if in_force(&status, "d") || Instant::now() > deadline {
            break status;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        in_force(&status, "d"),
        "the hand edit was not heard: {status}"
    );
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("  refused\te=") && l.contains("not a directory")),
        "{status}"
    );
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:d:file:d.txt", None))),
        "d"
    );
    assert_eq!(
        text(issue(request(Verb::Source, "urn:repo:a:file:a.txt", None))),
        "a"
    );

    // The same process, and the same two connections, all along.
    assert_eq!(gonk.child.id(), pid);
    let filed = worker
        .issue(request(Verb::Sink, "urn:iki:ledger:append", Some("after")))
        .map(|(repr, _)| repr);
    assert!(filed.is_ok(), "the worker's connection survived: {filed:?}");
    assert!(
        issue(request(Verb::Source, "urn:iki:gonk:browse:roots", None)).is_ok(),
        "the owner's connection survived"
    );
}

#[test]
fn the_roots_are_the_owners_alone() {
    let repos = tempfile::tempdir().unwrap();
    let a = repo(repos.path(), "a");
    let gonk = Gonk::with_quic(&format!("gonk.browse.root = \"a={}\"\n", a.display()));
    let worker = gonk.dial_quic().expect("QUIC");
    for r in [
        request(Verb::Source, "urn:iki:gonk:browse:roots", None),
        request(
            Verb::Sink,
            "urn:iki:gonk:browse:root:x",
            Some(&a.display().to_string()),
        ),
        request(Verb::Delete, "urn:iki:gonk:browse:root:a", None),
    ] {
        let target = r.target.to_string();
        assert!(
            worker.issue(r).is_err(),
            "a client certificate reached {target}"
        );
    }
}

#[test]
fn a_gonk_with_no_root_says_a_first_one_takes_a_restart() {
    // No root in the file: the family is not composed, and the status says a restart is
    // what a first root takes, while the write still lands in the file for that restart.
    let repos = tempfile::tempdir().unwrap();
    let a = repo(repos.path(), "a");
    let gonk = Gonk::start("");
    let owner = ikigai_ipc::connect(&gonk.socket).expect("socket");
    let issue = |r: Request| owner.issue(r).map(|(repr, _)| repr);
    let status = text(issue(request(
        Verb::Source,
        "urn:iki:gonk:browse:roots",
        None,
    )));
    assert!(status.contains("not composed"), "{status}");
    let added = issue(request(
        Verb::Sink,
        "urn:iki:gonk:browse:root:a",
        Some(&a.display().to_string()),
    ))
    .unwrap_err();
    assert!(added.to_string().contains("restart"), "{added}");
}

/// ★ The COARSE cut, pinned where it can be seen: in the hub's own cache. Over the doors a
/// stale entry is hard to observe (a name that stops resolving never reaches the cache), so
/// this composes the hub in process, caches a read, re-arranges the roots so the SAME name
/// is now another directory, and asks the cache directly.
#[test]
fn a_re_arrangement_cuts_every_answer_the_old_one_served() {
    use std::sync::Arc;

    use ikigai_core::Capability;
    use ikigai_gonk::browse;
    use ikigai_gonk::watch::RootWatch;
    use ikigai_store::DurableStore;

    let repos = tempfile::tempdir().unwrap();
    let (first, second) = (repo(repos.path(), "a"), repos.path().join("a2"));
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("a.txt"), "a, elsewhere").unwrap();
    let graph = browse::Graph::chosen();
    let (store, _handle) =
        DurableStore::in_memory_shared_declaring(graph.sharer_writes()).expect("a store");
    let writer = store.shared_writer().expect("a writer");
    let roots = |dir: &Path| vec![("a".to_string(), dir.to_path_buf())];
    let (watch, _) = RootWatch::start(&roots(&first));
    let wired = browse::wire(
        roots(&first),
        writer.clone(),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let family = Arc::new(wired.space);
    let hub = ikigai_gonk::compose_with(
        store.clone(),
        Some(Arc::clone(&family)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    );
    let root = Capability::root();
    let read = || request(Verb::Source, "urn:repo:a:file:a.txt", None);
    let answer = futures::executor::block_on(hub.issue(read(), &root)).expect("read");
    assert_eq!(answer.bytes, b"a");
    assert!(
        hub.is_cached(&read(), &root),
        "a watched file read is cached"
    );

    let (next_watch, _) = RootWatch::start_counting(&roots(&second), family.epochs());
    let next = browse::wire(
        roots(&second),
        writer,
        Some(&next_watch),
        None,
        &graph,
        None,
    );
    family.replace(&hub, &next.space, &[]);
    assert!(
        !hub.is_cached(&read(), &root),
        "an answer served under the old arrangement survived the swap"
    );
    let answer = futures::executor::block_on(hub.issue(read(), &root)).expect("read");
    assert_eq!(answer.bytes, b"a, elsewhere");
    drop((watch, next_watch));
}

/// #2392: configuration order and an appended live root must not dictate listing order.
#[test]
fn root_listings_sort_by_name_before_and_after_a_live_addition() {
    let repos = tempfile::tempdir().unwrap();
    let dir = repo(repos.path(), "files");
    let gonk = Gonk::start(&format!(
        "gonk.browse.root = \"zulu={}\"\ngonk.browse.root = \"alpha={}\"\ngonk.browse.root = \"ALPHA={}\"\n",
        dir.display(), dir.display(), dir.display()
    ));
    let log = gonk.log();
    let banner = log
        .lines()
        .find(|line| line.starts_with("  browse "))
        .unwrap();
    let positions: Vec<_> = ["ALPHA", "alpha", "zulu"]
        .iter()
        .map(|name| banner.find(&format!("{name} (watched)")).unwrap())
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{banner}"
    );
    let owner = ikigai_ipc::connect(&gonk.socket).expect("socket");
    let status = text(
        owner
            .issue(request(Verb::Source, "urn:iki:gonk:browse:roots", None))
            .map(|(r, _)| r),
    );
    let config = config_path(&status);
    let cli = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_ikigai-gonk"))
            .args(args)
            .arg("--socket")
            .arg(&gonk.socket)
            .arg("--config")
            .arg(&config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let token = sign_in_to_browse(&gonk);
    let mut failures = Vec::new();
    for (stage, expected) in [
        ("configured", vec!["ALPHA", "alpha", "zulu"]),
        ("live-added", vec!["ALPHA", "alpha", "Beta", "zulu"]),
    ] {
        if stage == "live-added" {
            cli(&["browse", "add", &format!("Beta={}", dir.display())]);
        }
        let listed = cli(&["browse", "list"]);
        let actual: Vec<_> = listed
            .lines()
            .filter_map(|line| {
                line.strip_prefix("  ")
                    .and_then(|line| line.split_once('\t'))
                    .map(|(name, _)| name)
            })
            .filter(|name| *name != "refused")
            .collect();
        if actual != expected {
            failures.push(format!(
                "{stage} browse list: {actual:?}, expected {expected:?}"
            ));
        }
        let page = http(&gonk, "GET", "/browse", "", Some(&token)).replace('\'', "\"");
        let positions: Vec<_> = expected
            .iter()
            .map(|name| {
                page.find(&format!("href=\"/browse/urn:repo:{name}:tree\""))
                    .unwrap_or_else(|| panic!("missing {name}: {page}"))
            })
            .collect();
        if !positions.windows(2).all(|pair| pair[0] < pair[1]) {
            failures.push(format!(
                "{stage} browse index: positions {positions:?}, expected {expected:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn root_names_have_the_same_order_regardless_of_input_order() {
    use ikigai_gonk::roots::RootNames;

    let expected = ["ALPHA", "alpha", "Beta", "zulu", "Äther", "äther"];
    for input in [
        vec!["zulu", "alpha", "äther", "ALPHA", "Äther", "Beta"],
        vec!["Beta", "Äther", "ALPHA", "äther", "alpha", "zulu"],
    ] {
        let names: RootNames = input.into_iter().map(str::to_string).collect();
        assert_eq!(names.names(), expected);
    }
}

/// Drive the real binary's HTTP door, including its passkey ceremony.
fn http(gonk: &Gonk, method: &str, path: &str, body: &str, token: Option<&str>) -> String {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = TcpStream::connect(gonk.http).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let cookie = token
        .map(|token| {
            format!(
                "Cookie: {}={token}\r\n",
                ikigai_gonk::identity::SESSION_COOKIE
            )
        })
        .unwrap_or_default();
    write!(stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nSec-Fetch-Site: same-origin\r\nAccept: {accept}\r\nContent-Type: application/json\r\n{cookie}Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        port = gonk.http.port(), len = body.len(),
        accept = if method == "GET" { "text/html" } else { "application/json" },
    ).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    response.split_once("\r\n\r\n").unwrap().1.to_string()
}

fn sign_in_to_browse(gonk: &Gonk) -> String {
    use ikigai_gonk::identity;

    let auth = common::Authenticator::new();
    let invite = identity::invite(
        &gonk.layout,
        "browser",
        &[ikigai_browse::CAP_WILDCARD.to_string()],
        false,
        30,
        identity::now_seconds(),
    )
    .unwrap();
    let origin = format!("http://localhost:{}", gonk.http.port());
    let challenge = |path| {
        let options: serde_json::Value =
            serde_json::from_str(&http(gonk, "POST", path, "{}", None)).unwrap();
        options["challenge"].as_str().unwrap().to_string()
    };
    http(
        gonk,
        "POST",
        "/auth/register",
        &auth.register_body(&challenge("/auth/register-options"), &invite, &origin),
        None,
    );
    let session: serde_json::Value = serde_json::from_str(&http(
        gonk,
        "POST",
        "/auth/login",
        &auth.login_body(&challenge("/auth/login-options"), &origin, 1),
        None,
    ))
    .unwrap();
    session["session"].as_str().unwrap().to_string()
}
