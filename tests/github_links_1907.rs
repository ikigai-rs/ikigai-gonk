//! "On GitHub" from the browse pages (ledger #1907): where the repository comes from, which ref
//! a link names and when, the link on a directory and on a file, the page that draws it, and
//! the escaping of a path a URL cannot carry raw.
//!
//! Every repository here is a HERMETIC scratch repository built in the test. "Origin has the
//! commit" is simulated the way a fetch leaves it: a `refs/remotes/origin/…` ref, written with
//! `update-ref`, because no test reaches GitHub.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::github::{self, repository};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

// ---------------------------------------------------------------- the scratch repository

const WHEN: &str = "2026-10-18T12:00:00Z";
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

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A file name no URL carries raw: a space, `#`, `%`, `+`, `&`, `=` and a non-ASCII letter.
const ODD: &str = "sp ace/a b#c%d+e&f=g é.rs";
/// The same name as `ikigai-browse` spells it in a `urn:repo:` IRI (it keeps `+&=` literal).
const ODD_IN_BROWSE: &str = "sp%20ace/a%20b%23c%25d+e&f=g%20%C3%A9.rs";
/// …and as a GitHub path, every segment through `url_segment`.
const ODD_ON_GITHUB: &str = "sp%20ace/a%20b%23c%25d%2Be%26f%3Dg%20%C3%A9.rs";

/// A repository with `src/lib.rs` and [`ODD`], one commit, and `origin` set to `origin` (none
/// for `None`). Answers the scratch dir and the HEAD.
fn scratch(origin: Option<&str>) -> (TempDir, String) {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    git(dir, &["init", "-q"]);
    write(dir, "src/lib.rs", "pub fn one() {}\n");
    write(dir, ODD, "// odd\n");
    let head = commit(dir, "the first commit");
    if let Some(origin) = origin {
        git(dir, &["remote", "add", "origin", origin]);
    }
    (tmp, head)
}

/// What a fetch leaves behind: origin's `main` at the HEAD, and `origin/HEAD` naming it.
fn origin_has_head(dir: &Path) {
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
}

// ---------------------------------------------------------------- the composition

/// The composition `main` builds for one root named `demo` over `dir`, watched, with the
/// given `gonk.browse.github` overrides.
fn served(dir: &Path, overrides: &[(String, String)]) -> (Arc<Kernel>, RootWatch) {
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
        .with_github(Arc::new(github::space(&roots, overrides)));
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
        browse_roots: vec!["demo".to_string()],
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

/// ★ Every origin form GitHub hands out normalizes to the one https form, userinfo is never
/// carried, and anything that is not a repository on GitHub gives no link.
#[test]
fn every_github_origin_normalizes_and_nothing_else_does() {
    let want = Some(REPO.to_string());
    for origin in [
        "git@github.com:ikigai-rs/demo.git",
        "git@github.com:ikigai-rs/demo",
        "ssh://git@github.com/ikigai-rs/demo",
        "ssh://git@github.com/ikigai-rs/demo.git",
        "ssh://git@github.com:22/ikigai-rs/demo.git",
        "https://github.com/ikigai-rs/demo",
        "https://github.com/ikigai-rs/demo.git",
        "https://github.com/ikigai-rs/demo/",
        "https://user@github.com/ikigai-rs/demo",
        "https://user:ghp_s3cret@github.com/ikigai-rs/demo.git",
        "HTTPS://GitHub.com/ikigai-rs/demo",
        "  https://github.com/ikigai-rs/demo.git\n",
    ] {
        assert_eq!(repository(origin), want, "{origin:?}");
        assert!(
            !repository(origin).unwrap_or_default().contains('@'),
            "no userinfo in an href: {origin:?}"
        );
    }
    for origin in [
        "https://gitlab.com/ikigai-rs/demo.git",
        "git@gitlab.com:ikigai-rs/demo.git",
        "https://github.com.evil.example/ikigai-rs/demo",
        "https://evil.example/github.com/ikigai-rs/demo",
        "http://github.com/ikigai-rs/demo",
        "git://github.com/ikigai-rs/demo.git",
        "file:///srv/git/demo.git",
        "/srv/git/demo.git",
        "../demo.git",
        "https://github.com/ikigai-rs",
        "https://github.com/ikigai-rs/demo/tree/main",
        "https://github.com/ikigai-rs/..",
        "https://github.com/ikigai-rs/demo?tab=readme",
        "https://github.com/ikigai-rs/demo#readme",
        "git@github.com:ikigai-rs/de mo.git",
        "",
    ] {
        assert_eq!(repository(origin), None, "{origin:?}");
    }
}

/// ★ The directory link and the file link, at a permalink: `tree/{sha}/…` for a directory,
/// `blob/{sha}/…` for a file, and the file link marked as one a selected line carries onto.
#[test]
fn a_directory_and_a_file_link_to_the_same_path_at_the_head_commit() {
    let (tmp, head) = scratch(Some(ORIGIN));
    origin_has_head(tmp.path());
    let (hub, _watch) = served(tmp.path(), &[]);

    let root = github::fields(&text(&hub, &github::root_iri("demo"), &[])).expect("a repo");
    assert_eq!(root["repository"], REPO);
    assert_eq!(root["ref"], head);
    assert_eq!(root["permalink"], "true");
    // Cached under the `state` it read, so the `git` runs are paid once per change.
    let cache = text(&hub, "urn:kernel:cache", &[]);
    assert!(
        cache.contains(&github::root_iri("demo")),
        "the root's answer is cached: {cache}"
    );

    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:tree").as_deref(),
        Some(format!("{REPO}/tree/{head}").as_str()),
        "the root's own tree"
    );
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:tree:src").as_deref(),
        Some(format!("{REPO}/tree/{head}/src").as_str())
    );
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:file:src/lib.rs").as_deref(),
        Some(format!("{REPO}/blob/{head}/src/lib.rs").as_str())
    );

    // The html face (the renderer writes single-quoted attributes): the link, and on a file
    // only, the mark `web/gonk.js` carries `#L10` or
    // `#L10-L20` onto from `data-base`. No note: this is a permalink.
    let file = text(
        &hub,
        "urn:iki:gonk:github:demo:file:src/lib.rs",
        &[("as", "text/html")],
    );
    let want = format!("{REPO}/blob/{head}/src/lib.rs");
    assert!(file.contains(&format!("href='{want}'")), "{file}");
    assert!(file.contains(&format!("data-base='{want}'")), "{file}");
    assert!(file.contains("data-lines='true'"), "{file}");
    assert!(file.contains(">On GitHub</a>"), "{file}");
    assert!(!file.contains("github-note"), "{file}");
    assert!(
        !file.contains("target="),
        "it opens like every other link here, in the same tab: {file}"
    );
    let dir = text(
        &hub,
        "urn:iki:gonk:github:demo:tree:src",
        &[("as", "text/html")],
    );
    assert!(
        !dir.contains("data-lines"),
        "a directory has no lines: {dir}"
    );
}

/// ★ The fallback: a HEAD origin does not have links to origin's default branch and says so;
/// and the answer moves when the root does, because it hangs from the `state` it read.
#[test]
fn a_tree_ahead_of_origin_links_to_the_default_branch_with_a_note() {
    let (tmp, first) = scratch(Some(ORIGIN));
    origin_has_head(tmp.path());
    let (hub, watch) = served(tmp.path(), &[]);
    let file = "urn:iki:gonk:github:demo:file:src/lib.rs";
    assert_eq!(
        href(&hub, file).as_deref(),
        Some(format!("{REPO}/blob/{first}/src/lib.rs").as_str())
    );

    // A local commit origin does not have: the tree is ahead.
    write(tmp.path(), "src/lib.rs", "pub fn two() {}\n");
    let second = commit(tmp.path(), "a commit origin does not have");
    let want = format!("{REPO}/blob/main/src/lib.rs");
    let mut now = href(&hub, file);
    let deadline = Instant::now() + Duration::from_secs(60);
    while now.as_deref() != Some(want.as_str()) && Instant::now() < deadline {
        let _ = watch.apply_next(&hub, Duration::from_millis(500));
        now = href(&hub, file);
    }
    assert_eq!(
        now.as_deref(),
        Some(want.as_str()),
        "the default branch, once the watch cut"
    );
    let read = github::fields(&text(&hub, file, &[])).expect("a link");
    assert_eq!(read["permalink"], "false");
    assert!(
        read["note"].contains("ahead of origin") && read["note"].contains("main"),
        "{read:?}"
    );
    let html = text(&hub, file, &[("as", "text/html")]);
    assert!(
        html.contains("github-note") && html.contains("ahead of origin"),
        "the note is on the page: {html}"
    );

    // A push catches origin up, and the link is a permalink again.
    git(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let want = format!("{REPO}/blob/{second}/src/lib.rs");
    let mut now = href(&hub, file);
    let deadline = Instant::now() + Duration::from_secs(60);
    while now.as_deref() != Some(want.as_str()) && Instant::now() < deadline {
        let _ = watch.apply_next(&hub, Duration::from_millis(500));
        now = href(&hub, file);
    }
    assert_eq!(now.as_deref(), Some(want.as_str()), "a permalink again");
}

/// With no `origin/HEAD` to name the default branch, the fallback is GitHub's own `HEAD`,
/// which GitHub resolves to the default branch.
#[test]
fn with_no_origin_head_the_fallback_is_githubs_own_head() {
    let (tmp, _) = scratch(Some(ORIGIN));
    let (hub, _watch) = served(tmp.path(), &[]);
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:tree:src").as_deref(),
        Some(format!("{REPO}/tree/HEAD/src").as_str())
    );
    let read =
        github::fields(&text(&hub, "urn:iki:gonk:github:demo:tree:src", &[])).expect("a link");
    assert!(read["note"].contains("the default branch"), "{read:?}");
}

/// ★ No origin and no override: no link, and the page draws none. A non-GitHub origin is the
/// same, and an override names the repository either way.
#[test]
fn no_origin_and_no_override_is_no_link() {
    let (bare, _) = scratch(None);
    let (hub, _watch) = served(bare.path(), &[]);
    let answer = text(&hub, "urn:iki:gonk:github:demo:file:src/lib.rs", &[]);
    assert!(answer.starts_with("none "), "{answer}");
    assert!(github::fields(&answer).is_none());
    let page = shell(&hub, "urn:repo:demo:tree", &Capability::root());
    assert!(!page.contains("On GitHub"), "{page}");
    assert!(
        page.contains("id='github'") && page.contains("hidden"),
        "the empty place the script swaps into is still drawn: {page}"
    );

    let (elsewhere, _) = scratch(Some("https://gitlab.com/ikigai-rs/demo.git"));
    let (hub, _watch) = served(elsewhere.path(), &[]);
    assert_eq!(href(&hub, "urn:iki:gonk:github:demo:tree"), None);

    // The override, for a root whose origin is not on GitHub.
    let overrides = vec![(
        "demo".to_string(),
        github::override_repository("ikigai-rs/demo").expect("owner/repo"),
    )];
    let (hub, _watch) = served(elsewhere.path(), &overrides);
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:tree").as_deref(),
        Some(format!("{REPO}/tree/HEAD").as_str())
    );
}

/// ★ An odd path: browse's IRI spelling is decoded, and every segment of the GitHub path is
/// percent-encoded on its own, so `#`, `%`, `+`, `&`, `=`, a space and non-ASCII stay inside
/// the path they belong to.
#[test]
fn an_odd_path_is_escaped_segment_by_segment() {
    let (tmp, head) = scratch(Some(ORIGIN));
    origin_has_head(tmp.path());
    let (hub, _watch) = served(tmp.path(), &[]);
    // browse names the file as this test expects it to.
    let listing = text(&hub, "urn:repo:demo:tree:sp%20ace", &[]);
    assert!(listing.contains("a b#c%d+e&f=g é.rs"), "{listing}");
    let iri = format!("urn:iki:gonk:github:demo:file:{ODD_IN_BROWSE}");
    assert_eq!(
        href(&hub, &iri).as_deref(),
        Some(format!("{REPO}/blob/{head}/{ODD_ON_GITHUB}").as_str())
    );
    // A path that is not a plain relative path once decoded names nothing.
    for bad in [
        "urn:iki:gonk:github:demo:file:..%2F..%2Fetc/passwd",
        "urn:iki:gonk:github:demo:file:%2Fetc/passwd",
    ] {
        assert_eq!(href(&hub, bad), None, "{bad}");
    }
}

/// A root below its repository's top links to its own place in the repository.
#[test]
fn a_root_inside_its_repository_links_below_the_top() {
    let (tmp, head) = scratch(Some(ORIGIN));
    origin_has_head(tmp.path());
    let sub = tmp.path().join("src");
    let (hub, _watch) = served(&sub, &[]);
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:tree").as_deref(),
        Some(format!("{REPO}/tree/{head}/src").as_str())
    );
    assert_eq!(
        href(&hub, "urn:iki:gonk:github:demo:file:lib.rs").as_deref(),
        Some(format!("{REPO}/blob/{head}/src/lib.rs").as_str())
    );
}

/// ★ The browse page draws the link for the view it opens on: a directory's, a file's, and
/// none for a caller who may not read the root (whose page draws no region either).
#[test]
fn the_browse_page_draws_the_link_for_its_view() {
    let (tmp, head) = scratch(Some(ORIGIN));
    origin_has_head(tmp.path());
    let (hub, _watch) = served(tmp.path(), &[]);
    let reader = Capability::scoped([format!("{}demo", ikigai_browse::CAP_PREFIX)]);

    let page = shell(&hub, "urn:repo:demo:tree:src", &reader);
    assert!(
        page.contains(&format!("href='{REPO}/tree/{head}/src'")),
        "{page}"
    );
    assert!(page.contains(">On GitHub</a>"), "{page}");

    let page = shell(&hub, "urn:repo:demo:file:src/lib.rs", &reader);
    assert!(
        page.contains(&format!("href='{REPO}/blob/{head}/src/lib.rs'")),
        "{page}"
    );
    assert!(page.contains("data-lines='true'"), "{page}");

    // Another view of the root (its pull requests, an explanation) draws no link.
    let page = shell(&hub, "urn:repo:demo:prs", &reader);
    assert!(!page.contains("On GitHub"), "{page}");

    // A grant naming another root: no region, and no link.
    let stranger = Capability::scoped([format!("{}other", ikigai_browse::CAP_PREFIX)]);
    let page = shell(&hub, "urn:repo:demo:tree", &stranger);
    assert!(!page.contains("On GitHub"), "{page}");
    assert!(
        read(&hub, "urn:iki:gonk:github:demo:tree", &[], &stranger).is_err(),
        "naming a root's repository is reading the root"
    );
}

/// ★ The line anchor: a file link's `data-base` plus the selected line, and nothing but a line.
/// Wiring only (the repository runs no browser in CI); the behavior was checked by hand in a
/// browser, and the shape the script accepts is pinned here.
#[test]
fn a_selected_line_carries_onto_a_file_link_and_nothing_else_does() {
    let js = include_str!("../web/gonk.js");
    let wiring = &js[js
        .find("\"On GitHub\" follows the region")
        .expect("the link's wiring")..];
    let wiring = &wiring[..wiring.find("sign-in notices").expect("the next section")];
    for needle in [
        r"const LINES_HASH = /^#L\d+(?:-L\d+)?$/;",
        "#github a.github-link[data-lines]",
        "a.getAttribute(\"data-base\")",
        "+ lineFragment()",
        "a.browse-annotation-line",
        "window.addEventListener(\"hashchange\", carryLines)",
        "\"urn:iki:gonk:github:\" + view.slice(\"urn:repo:\".length)",
        "target: \"#github\"",
    ] {
        assert!(wiring.contains(needle), "the link wiring lost `{needle}`");
    }
    // What the script hands back is the server's own href, or that href and a line; a hash
    // of any other shape adds nothing.
    let fragment = &wiring[wiring.find("function lineFragment").unwrap()..];
    let fragment = &fragment[..fragment.find("function carryLines").unwrap()];
    assert!(
        fragment.contains("return LINES_HASH.test(href) ? href : \"\";"),
        "{fragment}"
    );
}
