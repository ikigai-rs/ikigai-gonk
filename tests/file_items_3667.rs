//! A file's browse page lists the open ledger items about it, findings first by severity
//! (ledger #3667), read under the caller's capability.
//!
//! The items are filed about the CANONICAL file IRI (`urn:repo:file:demo/src/lib.rs`), the form
//! every filer writes; the page opens on browse's own (`urn:repo:demo:file:src/lib.rs`).

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::grants::{grants_for, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_resolve::Resolver;
use ikigai_store::DurableStore;
use tempfile::TempDir;

const FILE: &str = "urn:repo:file:demo/src/lib.rs";
const PAGE: &str = "urn:iki:gonk:page:browse:urn:repo:demo:file:src/lib.rs";
const OTHER_PAGE: &str = "urn:iki:gonk:page:browse:urn:repo:demo:file:README.md";
const HEADING: &str = "Ledger items about this file";

fn scratch() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn one() {}\n").expect("lib.rs");
    std::fs::write(dir.path().join("README.md"), "# scratch\n").expect("README.md");
    dir
}

fn served(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let roots = vec![("demo".to_string(), dir.path().to_path_buf())];
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

/// File one item about `about` in `default`; its number.
fn file(hub: &Kernel, title: &str, about: &str, kind: Option<&str>, labels: &str) -> i64 {
    let arg = |s: &str| ArgRef::Inline(s.as_bytes().to_vec());
    let mut request = Request::new(
        Verb::Sink,
        Iri::parse("urn:iki:ledger:default:append").expect("the append's name"),
    )
    .with_arg("content", arg(title))
    .with_arg("about", arg(about))
    .with_arg("as", arg("application/json"));
    if let Some(kind) = kind {
        request = request.with_arg("kind", arg(kind));
    }
    if !labels.is_empty() {
        request = request.with_arg("labels", arg(labels));
    }
    let (answer, _) = Resolver::issue(hub, request).expect("the append");
    let json: serde_json::Value = serde_json::from_slice(&answer.bytes).expect("its JSON");
    json.pointer("/item/number")
        .and_then(serde_json::Value::as_i64)
        .expect("its number")
}

fn close(hub: &Kernel, number: i64) {
    let arg = |s: &str| ArgRef::Inline(s.as_bytes().to_vec());
    let request = Request::new(
        Verb::Sink,
        Iri::parse("urn:iki:ledger:default:close").expect("the close's name"),
    )
    .with_arg("item", arg(&number.to_string()))
    .with_arg("reason", arg("done"));
    Resolver::issue(hub, request).expect("the close");
}

/// The page `iri` as the HTTP door renders it, under `cap`.
fn page(hub: &Arc<Kernel>, iri: &str, cap: &Capability) -> String {
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
    let http = doors::http_kernel(Arc::clone(hub), web::space(face));
    let request = Request::new(Verb::Source, Iri::parse(iri).expect("a page IRI"))
        .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
    block_on(Kernel::issue(&http, request, cap))
        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
        .unwrap_or_else(|e| panic!("the page {iri}: {e}"))
}

fn browse_reader() -> String {
    format!("{}demo", ikigai_browse::CAP_PREFIX)
}

/// A caller who reads the root and the ledger.
fn reader() -> Capability {
    let mut scopes = grants_for("default", Authority::Read).expect("the ledger's read tokens");
    scopes.push(browse_reader());
    Capability::scoped(scopes)
}

fn position(page: &str, needle: &str) -> usize {
    page.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not on the page: {page}"))
}

/// ★ A file with findings lists them, most severe first, then the file's other open items;
/// each links to its item and says its severity in words. A closed item and an item about
/// another file are not listed.
#[test]
fn a_file_with_findings_lists_them_in_severity_order() {
    let dir = scratch();
    let (hub, _watch) = served(&dir);
    let finding = Some(ikigai_gonk::roborev::FINDING_CLASS);
    let plain = file(&hub, "A plain item about lib", FILE, None, "");
    let trivial = file(&hub, "A trivial finding", FILE, finding, "severity:trivial");
    let critical = file(
        &hub,
        "A critical finding",
        FILE,
        finding,
        "severity:critical",
    );
    let minor = file(&hub, "A minor finding", FILE, finding, "severity:minor");
    let major = file(&hub, "A major finding", FILE, finding, "severity:major");
    let closed = file(&hub, "A closed finding", FILE, finding, "severity:critical");
    close(&hub, closed);
    file(
        &hub,
        "About the readme",
        "urn:repo:file:demo/README.md",
        finding,
        "severity:critical",
    );

    let page = page(&hub, PAGE, &reader());
    assert!(
        page.contains(&format!("<h2 id='file-items-heading'>{HEADING}</h2>")),
        "{page}"
    );
    assert!(page.contains("<ul class='file-items-list'>"), "{page}");
    let at = |n: i64| position(&page, &format!("href='/l/default/item/{n}'"));
    assert!(at(critical) < at(major), "{page}");
    assert!(at(major) < at(minor), "{page}");
    assert!(at(minor) < at(trivial), "{page}");
    assert!(at(trivial) < at(plain), "{page}");
    for word in ["critical", "major", "minor", "trivial"] {
        assert!(
            page.contains(&format!("(finding, {word})")),
            "{word}: {page}"
        );
    }
    assert!(page.contains("A critical finding"), "{page}");
    assert!(
        !page.contains(&format!("href='/l/default/item/{closed}'")),
        "a closed item is not listed: {page}"
    );
    assert!(!page.contains("About the readme"), "{page}");
    assert!(!page.contains("No open ledger item"), "{page}");
}

/// ★ A caller who may browse the root but not read the ledger sees no item, and nothing
/// hints that one exists: the page says what it says about a file with none.
#[test]
fn a_caller_without_the_ledgers_read_sees_none() {
    let dir = scratch();
    let (hub, _watch) = served(&dir);
    file(
        &hub,
        "A secret finding",
        FILE,
        Some(ikigai_gonk::roborev::FINDING_CLASS),
        "severity:critical",
    );
    let page = page(&hub, PAGE, &Capability::scoped([browse_reader()]));
    assert!(!page.contains("A secret finding"), "{page}");
    assert!(!page.contains("/l/default/item/"), "{page}");
    assert!(!page.contains("(finding,"), "{page}");
    assert!(
        page.contains("No open ledger item you may read is about this file."),
        "{page}"
    );
}

/// ★ A file with no items says so in a sentence, under the same heading; a page that does not
/// open on a file draws no section at all.
#[test]
fn a_file_with_none_says_so() {
    let dir = scratch();
    let (hub, _watch) = served(&dir);
    file(&hub, "About lib only", FILE, None, "");
    let page_text = page(&hub, OTHER_PAGE, &reader());
    assert!(page_text.contains(HEADING), "{page_text}");
    assert!(
        page_text.contains("No open ledger item you may read is about this file."),
        "{page_text}"
    );
    assert!(!page_text.contains("About lib only"), "{page_text}");

    let tree = page(
        &hub,
        "urn:iki:gonk:page:browse:urn:repo:demo:tree",
        &reader(),
    );
    assert!(!tree.contains(HEADING), "{tree}");
}
