//! "On GitHub" on the browse pull-request pages (ledger #2117): a pull request's page links to
//! `pull/{n}` in the root's repository, a file within one to its anchor in the Files tab, and
//! the browse page draws the link for a pull request's page as it does for a tree or a file
//! (ledger #1907, `tests/github_links_1907.rs`, whose scratch repository this file repeats).
//!
//! Every repository here is a HERMETIC scratch repository built in the test; no test reaches
//! GitHub, and none needs `gh`: the link is composed from the root's repository and the number
//! browse names, never from a pull request's own data.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::github;
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

// ---------------------------------------------------------------- the scratch repository

const WHEN: &str = "2026-10-22T12:00:00Z";
const ORIGIN: &str = "git@github.com:ikigai-rs/demo.git";
const REPO: &str = "https://github.com/ikigai-rs/demo";

/// `git -C dir args…` with a fixed identity and nothing from the machine's own config.
fn git(dir: &Path, args: &[&str]) -> String {
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
        .env("GIT_AUTHOR_DATE", WHEN)
        .env("GIT_COMMITTER_DATE", WHEN)
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

/// A file name no URL carries raw: a space, `#`, `%`, `+`, `&`, `=` and a non-ASCII letter.
const ODD: &str = "sp ace/a b#c%d+e&f=g é.rs";
/// The same name as `ikigai-browse` spells it in a `urn:repo:` IRI (it keeps `+&=` literal).
const ODD_IN_BROWSE: &str = "sp%20ace/a%20b%23c%25d+e&f=g%20%C3%A9.rs";

/// The Files tab anchors, each the SHA-256 of the raw path (`printf '%s' path | sha256sum`).
const LIB_ANCHOR: &str = "diff-b1a35a68f14e696205874893c07fd24fdb88882b47c23cc0e0c80a30c7d53759";
const ODD_ANCHOR: &str = "diff-1c20a573adf5621318709e07a00a9decffe25fe15bee153a694b68787dade001";

/// A repository with `src/lib.rs` and [`ODD`], one commit that origin has, and `origin` set
/// to `origin` (none for `None`).
fn scratch(origin: Option<&str>) -> TempDir {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    git(dir, &["init", "-q"]);
    write(dir, "src/lib.rs", "pub fn one() {}\n");
    write(dir, ODD, "// odd\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "the first commit"]);
    if let Some(origin) = origin {
        git(dir, &["remote", "add", "origin", origin]);
        git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    }
    tmp
}

// ---------------------------------------------------------------- the composition

/// The composition `main` builds for one root named `demo` over `dir`, watched.
fn served(dir: &Path) -> (Arc<Kernel>, RootWatch) {
    let roots = vec![("demo".to_string(), dir.to_path_buf())];
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots.clone(),
        store.shared_writer().expect("the store's writer"),
        Some(&watch),
        None,
        &graph,
        None,
    );
    let space = wired
        .space
        .with_github(Arc::new(github::space(&roots, &[])));
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(space)),
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

/// The `href` a link's text face names, or `None` when it answers `none …`.
fn href(kernel: &Kernel, iri: &str) -> Option<String> {
    github::fields(&text(kernel, iri, &[])).and_then(|f| f.get("href").cloned())
}

/// The browse page `/browse/{start}` as the HTTP door renders it, under `cap`.
fn shell(hub: &Arc<Kernel>, start: &str, cap: &Capability) -> String {
    let config = tempfile::tempdir().expect("a config home");
    let layout = quic::Layout::in_config_home(config.path());
    let face = Arc::new(web::Web {
        hub: Arc::clone(hub),
        ledgers: vec!["default".to_string()],
        browse_roots: vec!["demo".to_string()].into(),
        passkeys: Arc::new(Passkeys::new(layout, 1062)),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: ikigai_gonk::config::QueuePolicy::default(),
        epochs: None,
        toolbox: false,
    });
    let http = doors::http_kernel(Arc::clone(hub), web::space(face));
    read(
        &http,
        &format!("urn:iki:gonk:page:browse:{start}"),
        &[("as", "text/html")],
        cap,
    )
    .unwrap_or_else(|e| panic!("the browse page for {start}: {e}"))
}

// ---------------------------------------------------------------- the tests

/// ★ A pull request's page on GitHub: `pull/{n}` in the root's repository, naming no ref, so
/// no note; and a number that is not one names nothing.
#[test]
fn a_pull_request_links_to_its_page_in_the_roots_repository() {
    let tmp = scratch(Some(ORIGIN));
    let (hub, _watch) = served(tmp.path());

    let read = github::fields(&text(&hub, "urn:iki:gonk:github:demo:pull:7", &[])).expect("a link");
    assert_eq!(read["href"], format!("{REPO}/pull/7"));
    assert_eq!(read["kind"], "pull");
    assert_eq!(read["number"], "7");
    assert!(!read.contains_key("note"), "{read:?}");

    let html = text(
        &hub,
        "urn:iki:gonk:github:demo:pull:7",
        &[("as", "text/html")],
    );
    assert!(html.contains(&format!("href='{REPO}/pull/7'")), "{html}");
    assert!(
        html.contains(">On GitHub<span class='sr-only'> (opens in a new tab)</span></a>"),
        "{html}"
    );
    assert!(
        !html.contains("data-lines"),
        "a selected line is a file view's, not a pull request's: {html}"
    );
    assert!(!html.contains("github-note"), "{html}");

    for bad in [
        "urn:iki:gonk:github:demo:pull:x",
        "urn:iki:gonk:github:demo:pull:007",
        "urn:iki:gonk:github:demo:pull:7:other",
        "urn:iki:gonk:github:demo:pull:7%2F..%2F..%2Fsettings",
    ] {
        assert_eq!(href(&hub, bad), None, "{bad}");
    }

    // No repository on GitHub: no link, as for a tree or a file.
    let bare = scratch(None);
    let (hub, _watch) = served(bare.path());
    let answer = text(&hub, "urn:iki:gonk:github:demo:pull:7", &[]);
    assert!(answer.starts_with("none "), "{answer}");
}

/// ★ A file within a pull request: its anchor in the Files tab, the SHA-256 of its path from
/// the top of the repository, so an odd name and a root below the top hash what GitHub does.
#[test]
fn a_file_in_a_pull_request_links_to_its_anchor_in_the_files_tab() {
    let tmp = scratch(Some(ORIGIN));
    let (hub, _watch) = served(tmp.path());
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:pull:7:file:src/lib.rs").as_deref(),
        Some(format!("{REPO}/pull/7/files#{LIB_ANCHOR}").as_str())
    );
    let read = github::fields(&text(
        &hub,
        "urn:iki:gonk:github:demo:pull:7:file:src/lib.rs",
        &[],
    ))
    .expect("a link");
    assert_eq!(read["kind"], "pull-file");
    assert_eq!(
        href(
            &hub,
            &format!("urn:iki:gonk:github:demo:pull:7:file:{ODD_IN_BROWSE}")
        )
        .as_deref(),
        Some(format!("{REPO}/pull/7/files#{ODD_ANCHOR}").as_str())
    );
    for bad in [
        "urn:iki:gonk:github:demo:pull:7:file:..%2F..%2Fetc/passwd",
        "urn:iki:gonk:github:demo:pull:7:file:%2Fetc/passwd",
        "urn:iki:gonk:github:demo:pull:x:file:src/lib.rs",
    ] {
        assert_eq!(href(&hub, bad), None, "{bad}");
    }

    // A root below its repository's top hashes the path from the top.
    let (hub, _watch) = served(&tmp.path().join("src"));
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:pull:7:file:lib.rs").as_deref(),
        Some(format!("{REPO}/pull/7/files#{LIB_ANCHOR}").as_str())
    );
}

/// ★ The browse page draws the link for a pull request's page, its explanation and its
/// review, and none for the listing or for a caller who may not read the root.
#[test]
fn the_browse_page_draws_the_link_for_a_pull_request() {
    let tmp = scratch(Some(ORIGIN));
    let (hub, _watch) = served(tmp.path());
    let reader = Capability::scoped([format!("{}demo", ikigai_browse::CAP_PREFIX)]);

    for start in [
        "urn:repo:demo:pr:7",
        "urn:repo:demo:pr:7:explain",
        "urn:repo:demo:pr:7:review",
    ] {
        let page = shell(&hub, start, &reader);
        assert!(
            page.contains(&format!("href='{REPO}/pull/7'")),
            "{start}: {page}"
        );
        assert!(
            page.contains(">On GitHub<span class='sr-only'> (opens in a new tab)</span></a>"),
            "{start}: {page}"
        );
    }

    // The listing is many pull requests, not one.
    let page = shell(&hub, "urn:repo:demo:prs", &reader);
    assert!(!page.contains("On GitHub"), "{page}");

    let stranger = Capability::scoped([format!("{}other", ikigai_browse::CAP_PREFIX)]);
    let page = shell(&hub, "urn:repo:demo:pr:7", &stranger);
    assert!(!page.contains("On GitHub"), "{page}");
    assert!(
        read(&hub, "urn:iki:gonk:github:demo:pull:7", &[], &stranger).is_err(),
        "naming a root's pull request is reading the root"
    );
}

/// ★ The script maps a pull request's pages to the same name the shell does, so the link
/// follows the region onto one. Wiring only (the repository runs no browser in CI).
#[test]
fn the_link_follows_the_region_onto_a_pull_request() {
    let js = include_str!("../web/gonk.js");
    let wiring = &js[js
        .find("\"On GitHub\" follows the region")
        .expect("the link's wiring")..];
    let wiring = &wiring[..wiring.find("sign-in notices").expect("the next section")];
    for needle in [
        r"pr:([1-9]\d*)(?::explain|:review)?",
        "(m[3] ? \"pull:\" + m[3] : m[2])",
        "const link = githubOf(view);",
    ] {
        assert!(wiring.contains(needle), "the link wiring lost `{needle}`");
    }
    // And the Rust side agrees on every name the script maps.
    for (view, want) in [
        (
            "urn:repo:demo:pr:7",
            Some("urn:iki:gonk:github:demo:pull:7"),
        ),
        (
            "urn:repo:demo:pr:7:explain",
            Some("urn:iki:gonk:github:demo:pull:7"),
        ),
        (
            "urn:repo:demo:pr:7:review",
            Some("urn:iki:gonk:github:demo:pull:7"),
        ),
        ("urn:repo:demo:prs", None),
        ("urn:repo:demo:pr:07", None),
    ] {
        assert_eq!(github::link_iri(view).as_deref(), want, "{view}");
    }
}
