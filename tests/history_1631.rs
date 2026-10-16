//! A browse root's git history as weekly series (ledger #1631): what is counted, what is
//! cached and what cuts it, who may read it, and the bound.
//!
//! Every repository here is a HERMETIC scratch repository built in the test, with its
//! commit dates stated: CI checks out shallow, so a test that read this repository's own
//! history would be testing the checkout.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::config::ExplainTiers;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, history};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use tempfile::TempDir;

// ---------------------------------------------------------------- the scratch repository

/// `git -C dir args…` with a fixed identity and both dates set to `when` (an ISO instant),
/// and nothing from the machine's own config.
fn git(dir: &Path, when: &str, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Scratch",
            "-c",
            "user.email=scratch@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", when)
        .env("GIT_COMMITTER_DATE", when)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    std::fs::write(path, text).expect("the file");
}

fn commit(dir: &Path, when: &str, message: &str) -> String {
    git(dir, when, &["add", "-A"]);
    git(dir, when, &["commit", "-q", "-m", message]);
    git(dir, when, &["rev-parse", "HEAD"])
}

const LIB_THREE: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {}\n    #[test]\n    fn b() {}\n    #[tokio::test(flavor = \"multi_thread\")]\n    async fn c() {}\n}\n";
const LIB_TWO: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {}\n    #[tokio::test(flavor = \"multi_thread\")]\n    async fn c() {}\n}\n";
const LIB_ONE: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {}\n}\n";

/// The history the expectations below are written against, oldest first:
///
/// | when (UTC) | week of | commit | counts |
/// |---|---|---|---|
/// | 2026-09-01 | 08-31 | `a.rs` with one `#[test]` | outside a 4-week window |
/// | 2026-09-29 | 09-28 | `src/lib.rs` with two `#[test]` and a `#[tokio::test(…)]`, `t.py` with `def test_a` | rust +3, python +1 |
/// | 2026-10-06 | 10-05 | one `#[test]` removed, `x.test.ts` with two `Deno.test(` | rust −1, deno +2 |
/// | 2026-10-07 | 10-05 | the README | a commit, no tests |
/// | 2026-10-13 | 10-12 | on a side branch: `async def test_b` | python +1 |
/// | 2026-10-14 | 10-12 | the side branch merged `--no-ff` | nothing: a merge |
/// | 2026-10-20 | 10-19 | the `#[tokio::test]` moved into `src/other.rs` | rust +1 −1 |
fn scratch_history() -> (TempDir, String) {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    git(dir, "2026-09-01T12:00:00Z", &["init", "-q"]);
    write(dir, "a.rs", "#[test]\nfn old() {}\n");
    commit(dir, "2026-09-01T12:00:00Z", "before the window");
    write(dir, "src/lib.rs", LIB_THREE);
    write(dir, "t.py", "def test_a():\n    pass\n");
    commit(
        dir,
        "2026-09-29T12:00:00Z",
        "three Rust tests and a Python one",
    );
    write(dir, "src/lib.rs", LIB_TWO);
    write(
        dir,
        "x.test.ts",
        "Deno.test(\"one\", () => {});\nDeno.test(\"two\", () => {});\n",
    );
    commit(
        dir,
        "2026-10-06T12:00:00Z",
        "one Rust test out, two Deno tests in",
    );
    write(dir, "README.md", "# scratch\n");
    commit(dir, "2026-10-07T12:00:00Z", "no tests");
    git(
        dir,
        "2026-10-13T12:00:00Z",
        &["checkout", "-q", "-b", "side"],
    );
    write(dir, "u.py", "async def test_b():\n    pass\n");
    commit(dir, "2026-10-13T12:00:00Z", "a Python test on a branch");
    git(dir, "2026-10-14T12:00:00Z", &["checkout", "-q", "main"]);
    git(
        dir,
        "2026-10-14T12:00:00Z",
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    write(dir, "src/lib.rs", LIB_ONE);
    write(
        dir,
        "src/other.rs",
        "#[tokio::test(flavor = \"multi_thread\")]\nasync fn c() {}\n",
    );
    let head = commit(dir, "2026-10-20T12:00:00Z", "a test moved");
    (tmp, head)
}

const HEADER: &str = "week,commits,tests_added,tests_removed,rust_added,rust_removed,python_added,python_removed,deno_added,deno_removed";

fn expected_four_weeks() -> String {
    [
        HEADER,
        "2026-09-28,1,4,0,3,0,1,0,0,0",
        "2026-10-05,2,2,1,0,1,0,0,2,0",
        "2026-10-12,1,1,0,0,0,1,0,0,0",
        "2026-10-19,1,1,1,1,1,0,0,0,0",
    ]
    .join("\n")
        + "\n"
}

// ---------------------------------------------------------------- the composition

fn roots(dir: &Path) -> Vec<(String, PathBuf)> {
    vec![("demo".to_string(), dir.to_path_buf())]
}

/// The composition `main` builds for a browse root, watched.
fn served(dir: &Path) -> (Arc<Kernel>, RootWatch) {
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots(dir));
    assert!(refused.is_empty(), "{refused:?}");
    let _tiers = ExplainTiers::default();
    let wired = browse::wire(
        roots(dir),
        store.shared_writer().expect("the store's writer"),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
    ));
    (hub, watch)
}

fn request(iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(Verb::Source, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn read(
    kernel: &Kernel,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> Result<String, Error> {
    block_on(Kernel::issue(kernel, request(iri, args), cap))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
}

fn text(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> String {
    read(kernel, iri, args, &Capability::root()).unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"))
}

/// The IRIs the kernel holds cached, from `urn:kernel:cache`.
fn cached(kernel: &Kernel) -> Vec<String> {
    text(kernel, "urn:kernel:cache", &[])
        .lines()
        .skip(3)
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------- the tests

/// ★ What is counted: commits by committer week, tests added and removed per language,
/// merges counted for neither, and nothing outside the window.
#[test]
fn the_series_counts_commits_and_tests_per_week_and_language() {
    let (tmp, head) = scratch_history();
    let (hub, _watch) = served(tmp.path());
    let csv = text(
        &hub,
        &history::weekly_at_iri("demo", &head),
        &[("weeks", "4"), ("as", "text/csv")],
    );
    assert_eq!(csv, expected_four_weeks());
    // The composite answers the same at the HEAD.
    let now = text(
        &hub,
        &history::weekly_iri("demo"),
        &[("weeks", "4"), ("as", "text/csv")],
    );
    assert_eq!(now, csv);
    // A wider window reaches the commit before it: a year holds 53 rows here (the header
    // and 52 weeks), and the oldest test is in the week of 2026-08-31.
    let year = text(&hub, &history::weekly_iri("demo"), &[("as", "text/csv")]);
    assert_eq!(year.lines().count(), 53, "{year}");
    assert!(
        year.contains("\n2026-08-31,1,1,0,1,0,0,0,0,0\n"),
        "the commit before the 4-week window is in the year: {year}"
    );
}

/// The text face states the method; the Turtle face parses and carries one observation per
/// week with every measure.
#[test]
fn the_text_face_states_the_method_and_the_turtle_face_is_a_data_cube() {
    let (tmp, head) = scratch_history();
    let (hub, _watch) = served(tmp.path());
    let iri = history::weekly_at_iri("demo", &head);
    let plain = text(&hub, &iri, &[("weeks", "4")]);
    assert!(plain.contains(history::METHOD), "{plain}");
    assert!(plain.contains("2026-10-19"), "{plain}");
    let turtle = text(&hub, &iri, &[("weeks", "4"), ("as", "text/turtle")]);
    let quads: Vec<_> = oxigraph::io::RdfParser::from_format(oxigraph::io::RdfFormat::Turtle)
        .for_slice(turtle.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("the Turtle face parses: {e}\n{turtle}"));
    let observations = quads
        .iter()
        .filter(|q| {
            q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                && q.object.to_string() == "<http://purl.org/linked-data/cube#Observation>"
        })
        .count();
    assert_eq!(observations, 4, "{turtle}");
    let rust_added = format!("{}rust-added", history::MEASURE_PREFIX);
    let week = format!("{iri}:week:2026-09-28");
    assert!(
        quads
            .iter()
            .any(|q| q.subject.to_string() == format!("<{week}>")
                && q.predicate.as_str() == rust_added
                && q.object.to_string() == "\"3\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
        "the week of 09-28 added three Rust tests: {turtle}"
    );
    assert!(
        !turtle.contains("_:"),
        "no blank nodes in an emitted graph: {turtle}"
    );
}

/// ★ Cacheable per HEAD: the series as at a commit stays cached, and a NEW commit is a new
/// name, reached once the root watch cuts the `state` the composite read.
#[test]
fn a_new_commit_is_a_new_series_and_the_old_one_stays_cached() {
    let (tmp, head) = scratch_history();
    let (hub, watch) = served(tmp.path());
    let args = [("weeks", "4"), ("as", "text/csv")];
    let before = text(&hub, &history::weekly_iri("demo"), &args);
    assert_eq!(before, expected_four_weeks());
    let pinned = history::weekly_at_iri("demo", &head);
    assert!(
        cached(&hub).contains(&pinned),
        "the series as at HEAD is cached: {:?}",
        cached(&hub)
    );
    assert!(
        cached(&hub).contains(&history::weekly_iri("demo")),
        "and so is the composite, under the state it read: {:?}",
        cached(&hub)
    );

    // A commit lands in the HEAD week: one more Rust test.
    write(tmp.path(), "src/more.rs", "#[test]\nfn d() {}\n");
    let next = commit(tmp.path(), "2026-10-21T12:00:00Z", "one more");
    assert_ne!(next, head);

    let mut after = text(&hub, &history::weekly_iri("demo"), &args);
    let deadline = Instant::now() + Duration::from_secs(60);
    while after == before && Instant::now() < deadline {
        // Drive the watch's own notification path; each cut is applied as it arrives.
        let _ = watch.apply_next(&hub, Duration::from_millis(500));
        after = text(&hub, &history::weekly_iri("demo"), &args);
    }
    assert!(
        after.ends_with("2026-10-19,2,2,1,2,1,0,0,0,0\n"),
        "the new commit's week counts it once the watch has cut: {after}"
    );
    assert!(
        cached(&hub).contains(&history::weekly_at_iri("demo", &next)),
        "the new HEAD's series is cached under its own name: {:?}",
        cached(&hub)
    );
    // The old name still means what it meant, and is still a cache hit.
    assert!(cached(&hub).contains(&pinned), "{:?}", cached(&hub));
    assert_eq!(text(&hub, &pinned, &args), expected_four_weeks());
}

/// Reading a root's history is reading the root: browse's grant for THAT root, or none.
#[test]
fn the_series_needs_the_roots_browse_grant() {
    let (tmp, head) = scratch_history();
    let (hub, _watch) = served(tmp.path());
    let other = Capability::scoped(["urn:cap:browse:read:other"]);
    for iri in [
        history::weekly_iri("demo"),
        history::weekly_at_iri("demo", &head),
    ] {
        let denied = read(&hub, &iri, &[], &other);
        assert!(matches!(denied, Err(Error::Denied(_))), "{iri}: {denied:?}");
    }
    let mine = Capability::scoped(["urn:cap:browse:read:demo"]);
    let csv = read(
        &hub,
        &history::weekly_iri("demo"),
        &[("weeks", "4"), ("as", "text/csv")],
        &mine,
    )
    .expect("the root's own grant reads it, state and all");
    assert_eq!(csv, expected_four_weeks());
}

/// Bad arguments are refused by name, and a commit that is not there is absent.
#[test]
fn bad_arguments_are_refused_and_an_unknown_commit_is_not_found() {
    let (tmp, head) = scratch_history();
    let (hub, _watch) = served(tmp.path());
    let pinned = history::weekly_at_iri("demo", &head);
    for weeks in ["0", "521", "a year", "-1"] {
        for iri in [&pinned, &history::weekly_iri("demo")] {
            let refused = read(&hub, iri, &[("weeks", weeks)], &Capability::root());
            assert!(
                matches!(&refused, Err(Error::InvalidArgument { name, .. }) if name == "weeks"),
                "{iri} weeks={weeks}: {refused:?}"
            );
        }
    }
    let refused = read(&hub, &pinned, &[("as", "text/html")], &Capability::root());
    assert!(
        matches!(&refused, Err(Error::InvalidArgument { name, .. }) if name == "as"),
        "{refused:?}"
    );
    let short = history::weekly_at_iri("demo", &head[..12]);
    let refused = read(&hub, &short, &[], &Capability::root());
    assert!(
        matches!(&refused, Err(Error::InvalidArgument { name, .. }) if name == "sha"),
        "an abbreviation is not a constant name: {refused:?}"
    );
    let absent = history::weekly_at_iri("demo", &"0".repeat(40));
    let refused = read(&hub, &absent, &[], &Capability::root());
    assert!(matches!(refused, Err(Error::NotFound(_))), "{refused:?}");
}

/// A root at a repository's top counts every commit, an empty one included; a root that is a
/// subdirectory counts only the commits and tests under it.
#[test]
fn a_subdirectory_root_counts_only_itself_and_a_top_root_counts_everything() {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    let when = "2026-10-06T12:00:00Z";
    git(dir, when, &["init", "-q"]);
    write(dir, "sub/a.rs", "#[test]\nfn a() {}\n");
    commit(dir, when, "a test under sub");
    write(dir, "top.rs", "#[test]\nfn t() {}\n");
    commit(dir, when, "a test at the top");
    git(
        dir,
        "2026-10-07T12:00:00Z",
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    );
    let head = git(dir, when, &["rev-parse", "HEAD"]);
    let roots = vec![
        ("top".to_string(), dir.to_path_buf()),
        ("sub".to_string(), dir.join("sub")),
    ];
    let kernel =
        Kernel::with_meta_renderer(Arc::new(history::space(&roots)), Arc::new(TurtleRenderer));
    let csv = |root: &str| {
        text(
            &kernel,
            &history::weekly_at_iri(root, &head),
            &[("weeks", "1"), ("as", "text/csv")],
        )
    };
    assert_eq!(
        csv("top"),
        format!("{HEADER}\n2026-10-05,3,2,0,2,0,0,0,0,0\n")
    );
    assert_eq!(
        csv("sub"),
        format!("{HEADER}\n2026-10-05,1,1,0,1,0,0,0,0,0\n")
    );
}

/// ★ The bound refuses rather than truncating: past the budget there is no series at all,
/// and the refusal is a typed timeout.
#[test]
fn a_count_past_its_budget_is_refused() {
    let (tmp, head) = scratch_history();
    let space = history::space_with_budget(&roots(tmp.path()), Duration::ZERO);
    let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer));
    let refused = read(
        &kernel,
        &history::weekly_at_iri("demo", &head),
        &[],
        &Capability::root(),
    );
    assert!(matches!(refused, Err(Error::Timeout(_))), "{refused:?}");
}

/// ★ One count per commit and window: the plain and Turtle faces are views composed over the
/// CSV face by name, so reading one face caches the count every face reads. Shown by taking
/// the repository away after one face has been read: the CSV still answers, from the cache.
#[test]
fn every_face_reads_one_count() {
    let (tmp, head) = scratch_history();
    let (hub, _watch) = served(tmp.path());
    let pinned = history::weekly_at_iri("demo", &head);
    let plain = text(&hub, &pinned, &[("weeks", "4")]);
    assert!(plain.contains("2026-10-19"), "{plain}");
    std::fs::rename(tmp.path().join(".git"), tmp.path().join("gone")).expect("hide the repo");
    assert_eq!(
        text(&hub, &pinned, &[("weeks", "4"), ("as", "text/csv")]),
        expected_four_weeks(),
        "the CSV the plain face was composed over is a cache hit"
    );
    let turtle = text(&hub, &pinned, &[("weeks", "4"), ("as", "text/turtle")]);
    assert!(turtle.contains("qb:Observation"), "{turtle}");
}
