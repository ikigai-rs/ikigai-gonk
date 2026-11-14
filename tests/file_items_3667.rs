//! A browse file page lists the open ledger items about that file, findings first by severity
//! (ledger #3667). Before this, nothing on the page queried `ledger:about`, so a finding filed
//! about `urn:repo:file:{root}/{path}` was reachable by `items about=` and invisible where a
//! person reads the file.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

const FILE: &str = "urn:repo:demo:file:src/lib.rs";
const CANONICAL: &str = "urn:repo:file:demo/src/lib.rs";
const FINDING: &str = "https://ikigai-rs.dev/ns/ledger#Finding";

fn git(dir: &Path, args: &[&str]) {
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
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with `src/lib.rs` and `src/other.rs`, one commit.
fn scratch() -> TempDir {
    let tmp = tempfile::tempdir().expect("a scratch dir");
    let dir = tmp.path();
    git(dir, &["init", "-q"]);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
    std::fs::write(dir.join("src/other.rs"), "pub fn two() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "the first commit"]);
    tmp
}

/// The composition `main` builds for one root named `demo` over `dir`, watched.
fn served(dir: &Path) -> (Arc<Kernel>, RootWatch) {
    let roots = vec![("demo".to_string(), dir.to_path_buf())];
    let graph = browse::Graph::chosen();
    let (store, _handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots);
    assert!(refused.is_empty(), "{refused:?}");
    let wired = browse::wire(
        roots,
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

/// File one item in `default` and answer its number.
fn file(hub: &Kernel, title: &str, about: &str, kind: Option<&str>, labels: &str) -> String {
    let mut request = Request::new(Verb::Sink, Iri::parse("urn:iki:ledger:append").unwrap())
        .with_arg("content", ArgRef::Inline(title.as_bytes().to_vec()))
        .with_arg("about", ArgRef::Inline(about.as_bytes().to_vec()));
    if let Some(kind) = kind {
        request = request.with_arg("kind", ArgRef::Inline(kind.as_bytes().to_vec()));
    }
    if !labels.is_empty() {
        request = request.with_arg("labels", ArgRef::Inline(labels.as_bytes().to_vec()));
    }
    let filed = block_on(Kernel::issue(hub, request, &Capability::root())).expect("filed");
    let filed = String::from_utf8(filed.bytes).unwrap();
    filed
        .split_once("urn:iki:ledger:default:item:")
        .expect("the item's IRI")
        .1
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect()
}

fn http(hub: &Arc<Kernel>) -> Kernel {
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
        holder_aliases: Default::default(),
        page_assets: Default::default(),
        toolbox: false,
        faces: Vec::new(),
    });
    doors::http_kernel(Arc::clone(hub), web::space(face))
}

fn read(kernel: &Kernel, iri: &str, cap: &Capability) -> Result<String, Error> {
    let request = Request::new(Verb::Source, Iri::parse(iri).expect("a test IRI"))
        .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
    block_on(Kernel::issue(kernel, request, cap))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
}

/// The browse page `/browse/{start}` as the HTTP door renders it, under `cap`.
fn shell(hub: &Arc<Kernel>, start: &str, cap: &Capability) -> String {
    let iri = format!("urn:iki:gonk:page:browse:{start}");
    read(&http(hub), &iri, cap).unwrap_or_else(|e| panic!("the page {iri}: {e}"))
}

/// The section the page draws, from its opening tag to its close.
fn section(page: &str) -> &str {
    let id = page
        .find(" id='file-items'")
        .unwrap_or_else(|| panic!("a file-items section: {page}"));
    let start = page[..id].rfind("<section").expect("its start");
    let len = page[start..].find("</section>").expect("its end");
    &page[start..start + len]
}

/// Reads the root and the default ledger: what a signed-in reader holds.
fn reader() -> Capability {
    let mut scopes = grants_for("default", Authority::Read).unwrap();
    scopes.push(format!("{}demo", ikigai_browse::CAP_PREFIX));
    Capability::scoped(scopes)
}

/// ★ Findings first, by severity: critical, major, minor, trivial, then the other items; each a
/// link to its item, its severity in words; and nothing about another file.
#[test]
fn a_file_with_findings_lists_them_in_severity_order() {
    let tmp = scratch();
    let (hub, _watch) = served(tmp.path());
    let plain = file(&hub, "A plain item about lib", CANONICAL, None, "");
    let minor = file(
        &hub,
        "A minor finding",
        CANONICAL,
        Some(FINDING),
        "severity:minor",
    );
    let critical = file(
        &hub,
        "A critical finding",
        CANONICAL,
        Some(FINDING),
        "severity:critical",
    );
    let trivial = file(
        &hub,
        "A trivial finding",
        CANONICAL,
        Some(FINDING),
        "severity:trivial",
    );
    let major = file(
        &hub,
        "A major finding",
        CANONICAL,
        Some(FINDING),
        "severity:major",
    );
    file(
        &hub,
        "A finding about another file",
        "urn:repo:file:demo/src/other.rs",
        Some(FINDING),
        "severity:critical",
    );

    let page = shell(&hub, FILE, &reader());
    let html = section(&page);
    assert!(
        html.contains("<h2 id='file-items-heading'>Ledger items about this file</h2>"),
        "{html}"
    );
    let order: Vec<usize> = [&critical, &major, &minor, &trivial, &plain]
        .iter()
        .map(|id| {
            html.find(&format!("href='/l/default/item/{id}'"))
                .unwrap_or_else(|| panic!("item {id} is listed: {html}"))
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}: {html}");
    for words in [
        "critical finding",
        "major finding",
        "minor finding",
        "trivial finding",
    ] {
        assert!(html.contains(words), "{words}: {html}");
    }
    assert!(!html.contains("another file"), "{html}");
    assert!(html.contains("<ul"), "a list: {html}");

    // The same section is what `web/gonk.js` swaps in when the region moves to this file.
    let fragment = read(
        &http(&hub),
        &format!("urn:iki:gonk:fragment:file-items:{FILE}"),
        &reader(),
    )
    .expect("the fragment");
    assert!(
        fragment.contains(&format!("href='/l/default/item/{critical}'")),
        "{fragment}"
    );
}

/// ★ A caller who may read the root but not the ledger sees no item and no hint of one: the
/// same sentence a file with none reads.
#[test]
fn a_caller_without_the_ledgers_read_sees_none() {
    let tmp = scratch();
    let (hub, _watch) = served(tmp.path());
    let id = file(
        &hub,
        "A secret critical finding",
        CANONICAL,
        Some(FINDING),
        "severity:critical",
    );
    let browser = Capability::scoped([format!("{}demo", ikigai_browse::CAP_PREFIX)]);
    let page = shell(&hub, FILE, &browser);
    let html = section(&page);
    assert!(!html.contains("secret"), "{html}");
    assert!(!html.contains(&format!("/item/{id}")), "{html}");
    assert!(!html.contains("critical"), "{html}");
    assert!(
        html.contains("No open items in the ledgers you can read are about this file."),
        "{html}"
    );
}

/// A file with no items says so in a sentence, and a page that is not a file draws no section.
#[test]
fn a_file_with_none_says_so() {
    let tmp = scratch();
    let (hub, _watch) = served(tmp.path());
    file(
        &hub,
        "About the other file",
        "urn:repo:file:demo/src/other.rs",
        Some(FINDING),
        "severity:major",
    );
    let page = shell(&hub, FILE, &reader());
    let html = section(&page);
    assert!(
        html.contains("No open items in the ledgers you can read are about this file."),
        "{html}"
    );
    assert!(!html.contains("<ul"), "{html}");

    let tree = shell(&hub, "urn:repo:demo:tree", &reader());
    assert!(!tree.contains("Ledger items about this file"), "{tree}");
}
